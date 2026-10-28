//! The audit's hot-path workloads (§12.1) as real calls against a [`Target`].
//!
//! Each function performs one repetition and returns what it measured and the
//! facts it verified. Nothing here substitutes a timing stand-in: documents
//! are leased, read and committed through the versioned document API, commands
//! run in a server-owned exec session, tools are invoked on the extension
//! host, and worktrees come from the environment's worktree API.

use std::{
	fmt::Write as _,
	fs,
	path::Path,
	sync::{
		LazyLock,
		atomic::{AtomicU64, Ordering},
	},
	time::{Duration, Instant},
};

use bytes::Bytes;
use futures::future::try_join_all;
use omp_core::{EnvPath, Str};
use omp_env::{
	ClientError, EnvClient, ExecEvent, ExecRun, Invocation, InvocationEvent, TransactionOutcome,
	document_frame as document, frame,
};
use omp_tool::CallOutcome;
use serde_json::{Value, json};
use url::Url;

use super::{
	DaemonState, Transport, Workload,
	census::quiesce,
	topology::{Staging, Target},
};
use crate::{
	Context as _, Result, error,
	support::{InProcessEnvHarness, PythonExtension, read_blob, within},
};

/// Documents read by one W1 repetition (audit §12.1: 1,000 reads of 1 KiB).
pub(super) const W1_READS: u64 = 1_000;
/// Edits committed by one W2 repetition. The audit asks for 200, but the
/// document authority holds every commit that no language server answers for
/// the whole 500 ms diagnostics settle of `enrich_transaction_diagnostics`, so
/// 200 edits cost about 110 s a repetition and a 30-repetition matrix would
/// take hours. The per-edit metrics are comparable at ten.
pub(super) const W2_EDITS: u64 = 10;
/// Commands run by one W3 repetition (audit: 500 runs of `true`).
pub(super) const W3_COMMANDS: u64 = 500;
/// Extension tool calls made by one W6 repetition (audit: 500 calls).
pub(super) const W6_CALLS: u64 = 500;

/// Upper bound on any single request outside a batch's own bound.
pub(super) const STEP_LIMIT: Duration = Duration::from_secs(30);

const READ_FILE: &str = "read-target.txt";
const EDIT_FILE: &str = "edit-target.txt";
const COUNTER_START: u64 = 8;
const COUNTER_END: u64 = 14;

const REPO_DIRECTORIES: usize = 100;
const REPO_FILES_PER_DIRECTORY: usize = 30;
const REPO_LINES: usize = 40;
const REPO_NEEDLE_LINE: usize = 17;
const REPO_NEEDLE_EVERY: usize = 10;
const REPO_NEEDLES: u64 = (REPO_DIRECTORIES * REPO_FILES_PER_DIRECTORY / REPO_NEEDLE_EVERY) as u64;

const EXTENSION_SOURCE: &str = r#"
import os
import time
import omp


@omp.tool("p8_echo", rev=1)
def echo(value: int):
    return {"parts": [], "details": {"value": value + 1, "pid": os.getpid()}}


@omp.tool("p8_hold", rev=1)
def hold(millis: int):
    time.sleep(millis / 1000)
    return {"parts": [], "details": {"held": millis, "pid": os.getpid()}}
"#;

/// The recorder's Python extension: a cheap echo tool for W6 and a tool that
/// holds the host for a given time for W8's local-survival proof.
pub(super) static EXTENSION: PythonExtension =
	PythonExtension::new("p8_recorder_extension", EXTENSION_SOURCE, &["p8_echo", "p8_hold"]);

static EXTENSION_REVISION: LazyLock<String> = LazyLock::new(|| EXTENSION.revision());
static READ_CONTENT: LazyLock<Vec<u8>> = LazyLock::new(|| {
	(0..16_u8)
		.flat_map(|line| {
			let mut row: Vec<u8> = (0..63_u8)
				.map(|column| b'a' + (line + column) % 26)
				.collect();
			row.push(b'\n');
			row
		})
		.collect()
});

static EDIT_SEQUENCE: AtomicU64 = AtomicU64::new(0);
static CALL_SEQUENCE: AtomicU64 = AtomicU64::new(0);
static SLOT_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// What a workload needs in place before its authority starts.
#[derive(Clone, Copy, Debug)]
pub(super) struct Plan {
	pub(super) fixture: Fixture,
	pub(super) python:  bool,
}

/// Files a workload reads, edits or searches.
#[derive(Clone, Copy, Debug)]
pub(super) enum Fixture {
	Empty,
	Documents,
	Output { bytes: u64 },
	Repository,
}

impl Fixture {
	/// Writes the fixture below `project`.
	pub(super) fn write(self, project: &Path) -> Result<()> {
		match self {
			Self::Empty => Ok(()),
			Self::Documents => {
				fs::write(project.join(READ_FILE), &*READ_CONTENT).context("writing read fixture")?;
				let mut seed = b"counter=------\n".to_vec();
				for _ in 0..15 {
					seed.extend_from_slice(&[b'x'; 63]);
					seed.push(b'\n');
				}
				fs::write(project.join(EDIT_FILE), seed).context("writing edit fixture")?;
				Ok(())
			},
			Self::Output { bytes } => {
				fs::write(project.join(output_name(bytes)), output_content(bytes))
					.context("writing output fixture")?;
				Ok(())
			},
			Self::Repository => write_repository(project),
		}
	}
}

fn output_name(bytes: u64) -> String {
	format!("output-{bytes}.txt")
}

fn output_content(bytes: u64) -> Vec<u8> {
	let size = usize::try_from(bytes).unwrap_or(usize::MAX);
	let mut line = vec![b'x'; 79];
	line.push(b'\n');
	let mut content = Vec::with_capacity(size);
	while content.len() < size {
		let take = (size - content.len()).min(line.len());
		content.extend_from_slice(&line[..take]);
	}
	content
}

fn write_repository(project: &Path) -> Result<()> {
	for directory in 0..REPO_DIRECTORIES {
		let path = project.join("src").join(format!("module-{directory:03}"));
		fs::create_dir_all(&path).context("creating repository directory")?;
		for file in 0..REPO_FILES_PER_DIRECTORY {
			let index = directory * REPO_FILES_PER_DIRECTORY + file;
			let mut text = String::with_capacity(REPO_LINES * 80);
			for line in 0..REPO_LINES {
				let _ = if line == REPO_NEEDLE_LINE && index % REPO_NEEDLE_EVERY == 0 {
					writeln!(text, "NEEDLE-{index:05} marker on line {line}")
				} else {
					writeln!(
						text,
						"line {line:03} of file {index:05}: the quick brown fox jumps over the lazy dog"
					)
				};
			}
			fs::write(path.join(format!("file-{file:03}.txt")), text)
				.context("writing repository file")?;
		}
	}
	Ok(())
}

/// One workload parameter: an output size or a slot count.
#[derive(Clone, Debug)]
pub(super) enum Variant {
	None,
	Output { bytes: u64, label: &'static str },
	Slots(usize),
}

impl Variant {
	/// Returns every parameter the workload is recorded at.
	pub(super) fn of(workload: Workload) -> Vec<Self> {
		match workload {
			Workload::W4 => vec![
				Self::Output { bytes: 63 * 1024, label: "63KiB" },
				Self::Output { bytes: 65 * 1024, label: "65KiB" },
				Self::Output { bytes: 8_283_750, label: "7.9MiB" },
				Self::Output { bytes: 9 * 1024 * 1024, label: "9MiB" },
			],
			Workload::W7 => vec![Self::Slots(1), Self::Slots(4), Self::Slots(8)],
			_ => vec![Self::None],
		}
	}

	/// Returns the artifact label of this parameter.
	pub(super) fn label(&self) -> Option<String> {
		match self {
			Self::None => None,
			Self::Output { label, .. } => Some((*label).to_owned()),
			Self::Slots(count) => Some(format!("slots={count}")),
		}
	}
}

/// Returns what the workload needs staged for its authority.
pub(super) fn plan(workload: Workload, variant: &Variant, transport: Transport) -> Plan {
	match workload {
		Workload::W1 | Workload::W2 => Plan { fixture: Fixture::Documents, python: false },
		Workload::W4 => {
			let bytes = match variant {
				Variant::Output { bytes, .. } => *bytes,
				_ => 0,
			};
			Plan { fixture: Fixture::Output { bytes }, python: false }
		},
		Workload::W5 => Plan { fixture: Fixture::Repository, python: false },
		Workload::W6 => Plan { fixture: Fixture::Empty, python: true },
		Workload::W8 => {
			Plan { fixture: Fixture::Documents, python: transport == Transport::Attached }
		},
		Workload::W3 | Workload::W7 | Workload::W9 => {
			Plan { fixture: Fixture::Empty, python: false }
		},
	}
}

/// Returns the operations one repetition performs.
pub(super) fn operations(workload: Workload, variant: &Variant) -> u64 {
	match workload {
		Workload::W1 => W1_READS,
		Workload::W2 => W2_EDITS,
		Workload::W3 => W3_COMMANDS,
		Workload::W6 => W6_CALLS,
		Workload::W7 => match variant {
			Variant::Slots(count) => *count as u64,
			_ => 1,
		},
		Workload::W4 | Workload::W5 | Workload::W8 | Workload::W9 => 1,
	}
}

/// What one repetition measured and verified.
#[derive(Debug)]
pub(super) struct Sample {
	pub(super) metrics: Vec<(&'static str, f64)>,
	pub(super) facts:   Vec<(&'static str, u64)>,
}

fn nanoseconds(duration: Duration) -> f64 {
	duration.as_nanos() as f64
}

fn batch_sample(
	batch: Duration,
	latencies: &[Duration],
	facts: Vec<(&'static str, u64)>,
) -> Sample {
	let mut sorted: Vec<Duration> = latencies.to_vec();
	sorted.sort_unstable();
	let rank = sorted
		.len()
		.saturating_mul(95)
		.div_ceil(100)
		.saturating_sub(1);
	Sample {
		metrics: vec![
			("batch_ns", nanoseconds(batch)),
			("operation_mean_ns", nanoseconds(batch) / latencies.len().max(1) as f64),
			(
				"operation_p95_ns",
				sorted
					.get(rank)
					.map_or(0.0, |latency| nanoseconds(*latency)),
			),
		],
		facts,
	}
}

/// Runs one repetition of the steady-state workload `workload`.
pub(super) async fn run(workload: Workload, variant: &Variant, target: &Target) -> Result<Sample> {
	match workload {
		Workload::W1 => w1_reads(target).await,
		Workload::W2 => w2_edits(target).await,
		Workload::W3 => w3_commands(target).await,
		Workload::W4 => match variant {
			Variant::Output { bytes, .. } => w4_output(target, *bytes).await,
			_ => Err(error("W4 needs an output size")),
		},
		Workload::W5 => w5_search(target).await,
		Workload::W6 => w6_extension(target).await,
		Workload::W7 => match variant {
			Variant::Slots(count) => w7_slots(target, *count).await,
			_ => Err(error("W7 needs a slot count")),
		},
		Workload::W8 | Workload::W9 => Err(error("W8 and W9 start their own authorities")),
	}
}

pub(super) fn file_path(path: &Path) -> Result<EnvPath> {
	let uri = Url::from_file_path(path)
		.map_err(|()| error(format!("{} is not an absolute file path", path.display())))?;
	EnvPath::new(Str::new(uri.as_str())).context("typed file path")
}

pub(super) fn directory_path(path: &Path) -> Result<EnvPath> {
	let uri = Url::from_directory_path(path)
		.map_err(|()| error(format!("{} is not an absolute directory", path.display())))?;
	EnvPath::new(Str::new(uri.as_str())).context("typed directory path")
}

pub(super) async fn open_session(client: &EnvClient, project: &Path) -> Result<Bytes> {
	let cwd = directory_path(project)?;
	let opened = within(
		"session open",
		STEP_LIMIT,
		client.open_session(&cwd, frame::OpenSessionRequest::default()),
	)
	.await??;
	Ok(opened.session)
}

pub(super) async fn close_session(client: &EnvClient, session: Bytes) -> Result<()> {
	within(
		"session close",
		STEP_LIMIT,
		client.close_session(frame::CloseSessionRequest { session, ..Default::default() }),
	)
	.await??;
	Ok(())
}

pub(super) fn exec_request(session: &Bytes, text: &str) -> frame::ExecRequest {
	frame::ExecRequest {
		session: session.clone(),
		source: Some(frame::Script { text: text.to_owned(), ..frame::Script::default() }),
		..frame::ExecRequest::default()
	}
}

/// An exec request whose output bypasses the live stream's 64 KiB and 64 frame
/// bounds, up to the host ceiling, so the stream carries the complete output.
pub(super) fn exec_request_complete(session: &Bytes, text: &str) -> frame::ExecRequest {
	frame::ExecRequest {
		output_request: frame::OutputRequest::Complete as i32,
		..exec_request(session, text)
	}
}

/// A command stream consumed to its exit.
pub(super) struct Drained {
	pub(super) status:       frame::ExecStatusMsg,
	pub(super) bytes:        u64,
	pub(super) output:       Vec<u8>,
	pub(super) first_output: Option<Duration>,
}

/// Consumes every event of `run` up to its exit. `collect` keeps the output
/// bytes; `started` is the instant the first-output offset is measured from.
pub(super) async fn drain(run: &mut ExecRun, collect: bool, started: Instant) -> Result<Drained> {
	let mut bytes = 0_u64;
	let mut output = Vec::new();
	let mut first_output = None;
	loop {
		match run.next_event().await? {
			Some(ExecEvent::Started(_)) => {},
			Some(ExecEvent::Output(frame)) => {
				first_output.get_or_insert_with(|| started.elapsed());
				bytes += frame.data.len() as u64;
				if collect {
					output.extend_from_slice(&frame.data);
				}
			},
			Some(ExecEvent::Exit(exit)) => {
				let status = exit.status.context("terminal command status")?;
				return Ok(Drained { status, bytes, output, first_output });
			},
			None => return Err(error("command stream ended before its exit")),
		}
	}
}

/// Requires a command that exited normally with status zero.
pub(super) fn ensure_exited(status: &frame::ExecStatusMsg, command: &str) -> Result<()> {
	if status.outcome == frame::ExecOutcome::Exited as i32 && status.exit_code == Some(0) {
		Ok(())
	} else {
		Err(error(format!("{command:?} did not exit cleanly: {status:?}")))
	}
}

/// Runs `script` in `session` to its exit and requires status zero.
pub(super) async fn run_script(
	client: &EnvClient,
	session: &Bytes,
	script: &str,
	collect: bool,
) -> Result<Drained> {
	let started = Instant::now();
	let request = if collect {
		exec_request_complete(session, script)
	} else {
		exec_request(session, script)
	};
	let mut run = within("command start", STEP_LIMIT, client.exec(request)).await??;
	let done = within("command exit", STEP_LIMIT, drain(&mut run, collect, started)).await??;
	ensure_exited(&done.status, script)?;
	Ok(done)
}

fn whole_document() -> document::ReadSelection {
	document::ReadSelection {
		selection: Some(document::read_selection::Selection::Whole(document::WholeDocument {})),
	}
}

/// Opens a lease, reads the whole document, and closes the lease.
pub(super) async fn read_whole(client: &EnvClient, path: &EnvPath) -> Result<Bytes, ClientError> {
	let lease = client.open_document(path, None).await?;
	let read = client
		.read_document(&lease, None, Some(whole_document()))
		.await?;
	let content = read
		.content()
		.cloned()
		.ok_or(ClientError::UnexpectedResponse { expected: "document content" })?;
	lease.close().await?;
	Ok(content)
}

/// Path of the W1 read fixture below `project`.
pub(super) fn read_target(project: &Path) -> Result<EnvPath> {
	file_path(&project.join(READ_FILE))
}

/// Requires `content` to be the W1 fixture.
pub(super) fn ensure_read_fixture(content: &[u8]) -> Result<()> {
	if content == READ_CONTENT.as_slice() {
		Ok(())
	} else {
		Err(error(format!("document read returned {} unexpected bytes", content.len())))
	}
}

async fn w1_reads(target: &Target) -> Result<Sample> {
	let client = target.client();
	let path = read_target(target.project())?;
	let mut latencies = Vec::with_capacity(W1_READS as usize);
	let started = Instant::now();
	within("W1 batch", Duration::from_secs(300), async {
		for _ in 0..W1_READS {
			let operation = Instant::now();
			let content = read_whole(client, &path).await?;
			ensure_read_fixture(&content)?;
			latencies.push(operation.elapsed());
		}
		Ok::<(), crate::Error>(())
	})
	.await??;
	let batch = started.elapsed();
	Ok(batch_sample(batch, &latencies, vec![("bytes_per_read", READ_CONTENT.len() as u64)]))
}

/// A sixteen-byte transaction id unique within this process: the document
/// authority refuses any other width.
fn transaction_id(sequence: u64) -> Bytes {
	let id = (u128::from(std::process::id()) << 64) | u128::from(sequence);
	Bytes::copy_from_slice(&id.to_be_bytes())
}

fn counter_edit(value: u64) -> document::TextMutation {
	document::TextMutation {
		base_revision: None,
		change:        Some(document::text_mutation::Change::Edits(document::ByteEdits {
			edits: vec![document::ByteEdit {
				start:       COUNTER_START,
				end:         COUNTER_END,
				replacement: Bytes::from(format!("{:06}", value % 1_000_000)),
			}],
		})),
		stale_policy:  document::StalePolicy::Fail as i32,
		format_policy: document::FormatPolicy::Disabled as i32,
	}
}

async fn w2_edits(target: &Target) -> Result<Sample> {
	let client = target.client();
	let path = file_path(&target.project().join(EDIT_FILE))?;
	let first = EDIT_SEQUENCE.fetch_add(W2_EDITS, Ordering::Relaxed);
	let mut latencies = Vec::with_capacity(W2_EDITS as usize);
	let started = Instant::now();
	within("W2 batch", Duration::from_secs(300), async {
		for value in first..first + W2_EDITS {
			let operation = Instant::now();
			let mut lease = client.open_document(&path, None).await?;
			let transaction = transaction_id(value);
			match client
				.commit_document(&mut lease, transaction, counter_edit(value))
				.await?
			{
				TransactionOutcome::Committed(_) => {},
				other => return Err(error(format!("edit {value} did not commit: {other:?}"))),
			}
			lease.close().await?;
			latencies.push(operation.elapsed());
		}
		Ok::<(), crate::Error>(())
	})
	.await??;
	let batch = started.elapsed();
	let last = first + W2_EDITS - 1;
	let on_disk = fs::read(target.project().join(EDIT_FILE)).context("reading the edited file")?;
	let expected = format!("{:06}", last % 1_000_000);
	let counter = on_disk
		.get(COUNTER_START as usize..COUNTER_END as usize)
		.context("edited file is shorter than its counter")?;
	if counter != expected.as_bytes() {
		return Err(error(format!(
			"after {W2_EDITS} committed edits the file holds {:?}, expected {expected:?}",
			String::from_utf8_lossy(counter)
		)));
	}
	Ok(batch_sample(batch, &latencies, vec![("final_counter_verified", 1)]))
}

async fn w3_commands(target: &Target) -> Result<Sample> {
	let client = target.client();
	let session = open_session(client, target.project()).await?;
	let request = exec_request(&session, "true");
	let mut latencies = Vec::with_capacity(W3_COMMANDS as usize);
	let started = Instant::now();
	within("W3 batch", Duration::from_secs(600), async {
		for _ in 0..W3_COMMANDS {
			let operation = Instant::now();
			let mut run = client.exec(request.clone()).await?;
			let done = drain(&mut run, false, operation).await?;
			ensure_exited(&done.status, "true")?;
			if done.bytes != 0 {
				return Err(error(format!("true wrote {} bytes", done.bytes)));
			}
			latencies.push(operation.elapsed());
		}
		Ok::<(), crate::Error>(())
	})
	.await??;
	let batch = started.elapsed();
	close_session(client, session).await?;
	Ok(batch_sample(batch, &latencies, vec![("exit_status_zero", 1)]))
}

async fn w4_output(target: &Target, bytes: u64) -> Result<Sample> {
	let client = target.client();
	let session = open_session(client, target.project()).await?;
	let script = format!("cat {}", output_name(bytes));
	let started = Instant::now();
	let mut run =
		within("W4 command start", STEP_LIMIT, client.exec(exec_request(&session, &script)))
			.await??;
	let done =
		within("W4 stream", Duration::from_secs(120), drain(&mut run, false, started)).await??;
	let exec = started.elapsed();
	ensure_exited(&done.status, &script)?;
	let projection = done.status.projection.clone();
	if let Some(projection) = &projection
		&& projection.source_bytes != bytes
	{
		return Err(error(format!(
			"projection reports {} source bytes for a {bytes}-byte output",
			projection.source_bytes
		)));
	}
	let artifact = projection
		.as_ref()
		.and_then(|projection| projection.artifact.clone());
	let fetch_started = Instant::now();
	let mut artifact_bytes = 0_u64;
	if let Some(artifact) = &artifact {
		let blob = read_blob(
			client,
			omp_env::blob_frame::GetRequest { hash: artifact.hash.clone(), offset: 0, length: 0 },
			Duration::from_secs(120),
		)
		.await?;
		artifact_bytes = blob.len() as u64;
		if artifact_bytes != bytes {
			return Err(error(format!(
				"output artifact holds {artifact_bytes} bytes, expected {bytes}"
			)));
		}
	}
	let fetch = fetch_started.elapsed();
	if done.bytes != bytes && artifact_bytes != bytes {
		return Err(error(format!(
			"output is incomplete: {} streamed and {artifact_bytes} stored of {bytes} bytes",
			done.bytes
		)));
	}
	close_session(client, session).await?;
	Ok(Sample {
		metrics: vec![
			("exec_ns", nanoseconds(exec)),
			("first_output_ns", done.first_output.map_or(0.0, nanoseconds)),
			("fetch_ns", nanoseconds(fetch)),
			("total_ns", nanoseconds(exec + fetch)),
		],
		facts:   vec![
			("expected_bytes", bytes),
			("streamed_bytes", done.bytes),
			("source_bytes", projection.as_ref().map_or(done.bytes, |p| p.source_bytes)),
			("inline_bytes", projection.as_ref().map_or(done.bytes, |p| p.inline_bytes)),
			("projection_omitted", u64::from(projection.as_ref().is_some_and(|p| p.omitted))),
			("artifact_bytes", artifact_bytes),
		],
	})
}

async fn w5_search(target: &Target) -> Result<Sample> {
	let client = target.client();
	let session = open_session(client, target.project()).await?;
	let script = "grep -rEn 'NEEDLE-[0-9]{5}' src";
	let started = Instant::now();
	let done = run_script(client, &session, script, true).await?;
	if done
		.status
		.projection
		.as_ref()
		.is_some_and(|projection| projection.omitted)
	{
		return Err(error("the search output exceeded the complete stream's bounds"));
	}
	let search = started.elapsed();
	let text = String::from_utf8_lossy(&done.output);
	let matches = text.lines().filter(|line| line.contains("NEEDLE-")).count() as u64;
	let lines = text.lines().count() as u64;
	if matches != REPO_NEEDLES || lines != REPO_NEEDLES {
		return Err(error(format!(
			"search found {matches} matches in {lines} lines, expected exactly {REPO_NEEDLES}"
		)));
	}
	close_session(client, session).await?;
	Ok(Sample {
		metrics: vec![
			("search_ns", nanoseconds(search)),
			("first_match_ns", done.first_output.map_or(0.0, nanoseconds)),
		],
		facts:   vec![
			("files_searched", (REPO_DIRECTORIES * REPO_FILES_PER_DIRECTORY) as u64),
			("matches", matches),
		],
	})
}

/// Opens an extension tool invocation and commits its arguments, so the call
/// is in flight on the extension host when this returns.
pub(super) async fn start_call(client: &EnvClient, name: &str, args: &Value) -> Result<Invocation> {
	let id = format!("p8-call-{}", CALL_SEQUENCE.fetch_add(1, Ordering::Relaxed));
	let mut invocation = within(
		"open extension invocation",
		STEP_LIMIT,
		client.invoke(frame::InvokeTool {
			invocation_id: id,
			name: name.to_owned(),
			rev: EXTENSION_REVISION.clone(),
			..frame::InvokeTool::default()
		}),
	)
	.await??;
	match within("extension acceptance", STEP_LIMIT, invocation.next_event()).await?? {
		Some(InvocationEvent::Accepted(_)) => {},
		other => return Err(error(format!("expected extension acceptance, got {other:?}"))),
	}
	within(
		"commit extension arguments",
		STEP_LIMIT,
		invocation.commit_args(
			Bytes::from(serde_json::to_vec(args)?),
			Bytes::from_static(b"p8-recorder-token"),
			1000,
			None,
		),
	)
	.await??;
	Ok(invocation)
}

/// Waits for the verdict of an in-flight extension call and returns its
/// successful payload.
pub(super) async fn finish_call(mut invocation: Invocation, limit: Duration) -> Result<Value> {
	within("extension verdict", limit, async {
		loop {
			match invocation.next_event().await? {
				Some(InvocationEvent::Verdict(verdict)) => {
					let outcome: CallOutcome<Value, Value> = serde_json::from_slice(&verdict.json)?;
					return match outcome {
						CallOutcome::Ok(value) => Ok(value),
						other => Err(error(format!("extension tool returned {other:?}"))),
					};
				},
				Some(InvocationEvent::Update(_)) => {},
				Some(other) => return Err(error(format!("unexpected extension event {other:?}"))),
				None => return Err(error("extension invocation closed before its verdict")),
			}
		}
	})
	.await?
}

/// Calls an extension tool to its successful payload.
pub(super) async fn call_tool(client: &EnvClient, name: &str, args: &Value) -> Result<Value> {
	let invocation = start_call(client, name, args).await?;
	finish_call(invocation, STEP_LIMIT).await
}

/// Calls `p8_echo` and requires it to answer `value + 1`.
pub(super) async fn echo(client: &EnvClient, value: u64) -> Result<()> {
	let payload = call_tool(client, "p8_echo", &json!({ "value": value })).await?;
	let details = payload.get("details").unwrap_or(&payload);
	match details.get("value").and_then(Value::as_u64) {
		Some(answer) if answer == value + 1 => Ok(()),
		other => Err(error(format!("p8_echo({value}) answered {other:?} in {payload}"))),
	}
}

async fn w6_extension(target: &Target) -> Result<Sample> {
	let client = target.client();
	let mut latencies = Vec::with_capacity(W6_CALLS as usize);
	let started = Instant::now();
	within("W6 batch", Duration::from_secs(300), async {
		for value in 0..W6_CALLS {
			let operation = Instant::now();
			echo(client, value).await?;
			latencies.push(operation.elapsed());
		}
		Ok::<(), crate::Error>(())
	})
	.await??;
	let batch = started.elapsed();
	Ok(batch_sample(batch, &latencies, vec![("answers_verified", W6_CALLS)]))
}

async fn open_slot(worktree: &frame::WorktreeInfo) -> Result<InProcessEnvHarness> {
	let root = Url::parse(&worktree.root_uri)
		.context("parsing worktree root URI")?
		.to_file_path()
		.map_err(|()| error(format!("worktree root {} is not a file URI", worktree.root_uri)))?;
	let harness = InProcessEnvHarness::spawn_at(&root).await?;
	let canonical = fs::canonicalize(&root).context("canonical worktree root")?;
	let session = open_session(harness.client(), &canonical).await?;
	let done = run_script(harness.client(), &session, "pwd", true).await?;
	let printed = String::from_utf8_lossy(&done.output);
	if Path::new(printed.trim_end()) != canonical {
		return Err(error(format!(
			"slot ran in {:?}, expected its worktree {}",
			printed.trim_end(),
			canonical.display()
		)));
	}
	close_session(harness.client(), session).await?;
	Ok(harness)
}

async fn w7_slots(target: &Target, slots: usize) -> Result<Sample> {
	let client = target.client();
	let sequence = SLOT_SEQUENCE.fetch_add(slots as u64, Ordering::Relaxed);
	let started = Instant::now();
	let worktrees = within(
		"W7 worktree creation",
		STEP_LIMIT,
		try_join_all((0..slots).map(|slot| async move {
			let created = client
				.create_worktree(frame::CreateWorktree {
					name: format!("p8-slot-{}", sequence + slot as u64),
					owner_pid: std::process::id(),
					..frame::CreateWorktree::default()
				})
				.await?;
			created
				.worktree
				.context("environment omitted worktree identity")
		})),
	)
	.await??;
	let created = started.elapsed();
	let ready_started = Instant::now();
	let harnesses =
		within("W7 slot readiness", STEP_LIMIT, try_join_all(worktrees.iter().map(open_slot)))
			.await??;
	let ready = ready_started.elapsed();
	let cleanup_started = Instant::now();
	for harness in harnesses {
		harness.shutdown().await;
	}
	within(
		"W7 worktree cleanup",
		STEP_LIMIT,
		try_join_all(worktrees.iter().map(|worktree| async move {
			client
				.destroy_worktree(frame::DestroyWorktree {
					id: worktree.id.clone(),
					force: true,
					..frame::DestroyWorktree::default()
				})
				.await
		})),
	)
	.await??;
	let cleanup = cleanup_started.elapsed();
	for worktree in &worktrees {
		let root = Url::parse(&worktree.root_uri)
			.context("parsing worktree root URI")?
			.to_file_path()
			.map_err(|()| error("worktree root is not a file URI"))?;
		if root.exists() {
			return Err(error(format!("worktree {} survived its destruction", root.display())));
		}
	}
	Ok(Sample {
		metrics: vec![
			("create_ns", nanoseconds(created)),
			("ready_ns", nanoseconds(ready)),
			("cleanup_ns", nanoseconds(cleanup)),
			("total_ns", nanoseconds(created + ready + cleanup)),
		],
		facts:   vec![("slots", slots as u64), ("worktrees_removed", slots as u64)],
	})
}

/// Records `repetitions` W9 attaches: from nothing to the first usable
/// request (cold), or a new connection to an authority that already served
/// one (warm), then a second request on the same connection.
pub(super) async fn attaches(
	transport: Transport,
	state: DaemonState,
	plan: &Plan,
	repetitions: usize,
) -> Result<Vec<Sample>> {
	let mut samples = Vec::with_capacity(repetitions);
	match state {
		DaemonState::Cold => {
			for _ in 0..repetitions {
				quiesce().await;
				let staging = Staging::new(plan).await?;
				let started = Instant::now();
				let target = within("W9 cold start", STEP_LIMIT, staging.start(transport)).await??;
				let attach = started.elapsed();
				let sample = usable_requests(target.client(), target.project(), attach).await;
				let closed = target.close().await;
				samples.push(sample?);
				closed?;
			}
		},
		DaemonState::Warm => {
			let target = Staging::new(plan).await?.start(transport).await?;
			let result: Result<()> = async {
				usable_requests(target.client(), target.project(), Duration::ZERO).await?;
				for _ in 0..repetitions {
					quiesce().await;
					let started = Instant::now();
					let attachment =
						within("W9 warm attach", STEP_LIMIT, target.attach_again()).await??;
					let attach = started.elapsed();
					samples.push(usable_requests(attachment.client(), target.project(), attach).await?);
				}
				Ok(())
			}
			.await;
			let closed = target.close().await;
			result?;
			closed?;
		},
	}
	Ok(samples)
}

async fn usable_requests(client: &EnvClient, project: &Path, attach: Duration) -> Result<Sample> {
	let started = Instant::now();
	let session = open_session(client, project).await?;
	run_script(client, &session, "true", false).await?;
	let first = started.elapsed();
	let reused_started = Instant::now();
	run_script(client, &session, "true", false).await?;
	let reused = reused_started.elapsed();
	close_session(client, session).await?;
	Ok(Sample {
		metrics: vec![
			("attach_ns", nanoseconds(attach)),
			("first_request_ns", nanoseconds(first)),
			("reused_request_ns", nanoseconds(reused)),
			("total_ns", nanoseconds(attach + first)),
		],
		facts:   vec![("requests_exited_zero", 2)],
	})
}
