//! P12: the project daemon relays a sandbox amendment prompt only to the
//! connection that issued the command, and only that connection's answer
//! decides it.
//!
//! Every case runs the production daemon (`omp_e2e_host envd`, the same
//! `omp_app` dispatch as `omp envd`) as a child process attached to a real
//! docserver. The child's `HOME` and user roots live under scratch, so the
//! shipped `workspace-write` sandbox applies and no in-process approval route
//! exists: a prompt reaches a human only through the issuing connection. A
//! write into the protected `.git` carve-out is denied and files the prompt.
//! The approved rerun writes `$$`, which the in-process shell expands to its
//! host's process id, so the file proves the command ran inside the daemon.
//! The proof needs Seatbelt, so it skips off macOS. On macOS an unavailable
//! Seatbelt fails it instead: the macOS CI job is its only gate, which must
//! never pass without running it.

#![cfg(unix)]

use std::{
	fs,
	path::{Path, PathBuf},
	time::Duration,
};

use bytes::Bytes;
use flume::Receiver;
use omp_core::{EnvPath, Str};
use omp_e2e::{
	Context as _, Result, error,
	support::{
		DEFAULT_TIMEOUT, DocServerTask, EnvHarness, FramedEnvConnection, ProcessEnvHarness,
		RawEnvConnection, Scratch, omp_binary, within,
	},
};
use omp_env::{
	APPROVAL_RELAY_CAPABILITY, ApprovalQueryEvent, ClientError, EnvClient, ExecEvent, ExecRun,
};
use omp_proto::env::v1::{
	ApprovalAnswer, ApprovalDecision, ApprovalQuery, CloseSessionRequest, ExecOutcome, ExecRequest,
	ExecStatusMsg, OpenSessionRequest, Script, client_frame, server_frame,
};
use url::Url;

/// Bound on one relay step. It sits far below the prompt's 120 s timeout and
/// the daemon's backstop behind it, so a step that completes only through a
/// timeout fails the proof instead of passing late.
const RELAY_WAIT: Duration = Duration::from_secs(30);

/// One production daemon child, the docserver it attached, and its scratch
/// roots. Fields drop in order: the daemon process goes before its docserver.
struct Daemon {
	process:   ProcessEnvHarness,
	docserver: DocServerTask,
	project:   PathBuf,
	_scratch:  Scratch,
}

impl Daemon {
	/// Starts the daemon, or returns `None` off macOS. On macOS a failed
	/// Seatbelt probe is an error naming the failure, never a skip.
	async fn start() -> Result<Option<Self>> {
		if !cfg!(target_os = "macos") {
			eprintln!("P12 skipped: it needs the Seatbelt sandbox backend, which only macOS has");
			return Ok(None);
		}
		if let Some(failure) = omp_sandbox::backend_status(omp_sandbox::Backend::Seatbelt).failure() {
			return Err(error(format!(
				"P12 needs the Seatbelt sandbox backend, and its probe failed: {failure} ({failure:?})"
			)));
		}
		let scratch = Scratch::new()?;
		fs::create_dir(scratch.project().join(".git")).context("creating the .git carve-out")?;
		// One canonical path both starts the daemon and names the build its
		// docserver advertises, so the daemon recognizes the docserver as its own.
		let executable = omp_binary().context("resolving the daemon executable")?;
		let docserver = DocServerTask::spawn_for_daemon(
			scratch.project(),
			scratch.socket("attached-docserver.sock"),
			&executable,
		)
		.await?;
		let process = EnvHarness::spawn_attached(&scratch, &executable, docserver.socket()).await?;
		let project = fs::canonicalize(scratch.project()).context("canonical project root")?;
		Ok(Some(Self { process, docserver, project, _scratch: scratch }))
	}

	/// Opens an application connection that relays approvals.
	async fn relay_client(&self, name: &str) -> Result<FramedEnvConnection> {
		self
			.process
			.connect_client_with_capabilities(name, &[APPROVAL_RELAY_CAPABILITY])
			.await
	}

	/// Opens a raw-frame application connection that relays approvals.
	async fn relay_raw(&self, name: &str) -> Result<RawEnvConnection> {
		self
			.process
			.connect_raw(name, &[APPROVAL_RELAY_CAPABILITY])
			.await
	}

	fn pid(&self) -> Result<u32> {
		self.process.pid().context("daemon child has a process id")
	}

	fn protected(&self, name: &str) -> PathBuf {
		self.project.join(".git").join(name)
	}

	async fn shutdown(self) -> Result<()> {
		self.process.shutdown().await?;
		self.docserver.shutdown().await
	}
}

fn project_uri(project: &Path) -> Result<String> {
	Url::from_directory_path(project)
		.map(String::from)
		.map_err(|()| error(format!("{} is not an absolute directory", project.display())))
}

/// A command whose redirection lands in the protected carve-out.
fn protected_write(name: &str) -> String {
	format!("printf '%s' \"$$\" > .git/{name}")
}

async fn open_session(client: &EnvClient, project: &Path) -> Result<Bytes> {
	let cwd = EnvPath::new(Str::from(project_uri(project)?)).context("typed project cwd")?;
	let opened = within(
		"session open",
		DEFAULT_TIMEOUT,
		client.open_session(&cwd, OpenSessionRequest::default()),
	)
	.await??;
	Ok(opened.session)
}

fn exec_request(session: &Bytes, text: &str) -> ExecRequest {
	ExecRequest {
		session: session.clone(),
		source: Some(Script { text: text.to_owned(), ..Script::default() }),
		..ExecRequest::default()
	}
}

async fn exec(client: &EnvClient, session: &Bytes, text: &str) -> Result<ExecRun> {
	Ok(within("command start", DEFAULT_TIMEOUT, client.exec(exec_request(session, text))).await??)
}

/// Reads `run` to its exit and returns the terminal status and output.
async fn exit(run: &mut ExecRun) -> Result<(ExecStatusMsg, Vec<u8>)> {
	within("command exit", RELAY_WAIT, async {
		let mut output = Vec::new();
		loop {
			match run.next_event().await? {
				Some(ExecEvent::Started(_)) => {},
				Some(ExecEvent::Output(frame)) => output.extend_from_slice(&frame.data),
				Some(ExecEvent::Exit(exit)) => {
					return Ok((exit.status.context("terminal command status")?, output));
				},
				None => return Err(error("command stream ended before its exit")),
			}
		}
	})
	.await?
}

/// Waits for the next relayed prompt; a withdrawal fails the proof.
async fn next_query(queries: &Receiver<ApprovalQueryEvent>) -> Result<(u64, ApprovalQuery)> {
	match within("approval query", RELAY_WAIT, queries.recv_async())
		.await?
		.context("approval queue closed")?
	{
		ApprovalQueryEvent::Requested { request_id, query } => Ok((request_id, query)),
		withdrawn => Err(error(format!("expected an approval query, got {withdrawn:?}"))),
	}
}

/// The prompt is the daemon's own sandbox amendment for `command`.
fn assert_amendment(query: &ApprovalQuery, command: &str) {
	assert_eq!(query.invocation_id, None, "a sandbox amendment blocks no invocation");
	let [reason] = query.reasons.as_slice() else {
		panic!("expected one amendment requirement: {query:?}");
	};
	assert_eq!(reason.kind, "sandbox_amendment");
	assert_eq!(reason.scopes, ["once"]);
	assert_eq!(reason.timeout_ms, 120_000);
	assert_eq!(reason.timeout_default, Some(false));
	assert_eq!(reason.unreachable, "fail_closed");
	assert!(reason.require_human);
	assert_eq!(reason.pattern.as_deref(), Some(command));
	assert!(Path::new(&reason.subject).ends_with(".git"), "{}", reason.subject);
}

fn decision(approved: bool) -> ApprovalDecision {
	ApprovalDecision {
		approved,
		scope: "once".to_owned(),
		source: "user".to_owned(),
		..ApprovalDecision::default()
	}
}

fn answer(query_id: u64, approved: bool) -> client_frame::Body {
	client_frame::Body::ApprovalAnswer(ApprovalAnswer {
		query_id,
		decision: Some(decision(approved)),
	})
}

/// Opens a session as one request round trip. Its reply must be the very next
/// frame, so the daemon sent this connection nothing before it: no prompt and
/// no reply to anything it sent earlier.
async fn round_trip(
	connection: &RawEnvConnection,
	request_id: u64,
	project: &Path,
) -> Result<Bytes> {
	connection
		.send(
			request_id,
			client_frame::Body::OpenSession(OpenSessionRequest {
				cwd_uri: project_uri(project)?,
				..OpenSessionRequest::default()
			}),
		)
		.await?;
	let frame = connection.next(RELAY_WAIT).await?;
	match frame.body {
		Some(server_frame::Body::SessionOpened(opened)) if frame.request_id == request_id => {
			Ok(opened.session)
		},
		body => Err(error(format!(
			"expected session {request_id} to open first, got {body:?} on {}",
			frame.request_id
		))),
	}
}

/// Reads a raw exec's frames up to its relayed prompt; any other frame fails
/// the proof.
async fn raw_query(connection: &RawEnvConnection, request_id: u64) -> Result<ApprovalQuery> {
	loop {
		let frame = connection.next(RELAY_WAIT).await?;
		if frame.request_id != request_id {
			return Err(error(format!("unexpected frame before {request_id}'s prompt: {frame:?}")));
		}
		match frame.body {
			Some(server_frame::Body::ApprovalQuery(query)) => return Ok(query),
			Some(server_frame::Body::ExecStarted(_) | server_frame::Body::Output(_)) => {},
			body => {
				return Err(error(format!("expected a prompt on {request_id}, got {body:?}")));
			},
		}
	}
}

/// Reads a raw exec's frames to its exit. A prompt, a withdrawal, or another
/// request's frame fails the proof.
async fn raw_exit(
	connection: &RawEnvConnection,
	request_id: u64,
) -> Result<(ExecStatusMsg, Vec<u8>)> {
	let mut output = Vec::new();
	loop {
		let frame = connection.next(RELAY_WAIT).await?;
		if frame.request_id != request_id {
			return Err(error(format!("unexpected frame before {request_id} exited: {frame:?}")));
		}
		match frame.body {
			Some(server_frame::Body::ExecStarted(_)) => {},
			Some(server_frame::Body::Output(chunk)) => output.extend_from_slice(&chunk.data),
			Some(server_frame::Body::Exit(exit)) => {
				return Ok((exit.status.context("terminal command status")?, output));
			},
			body => {
				return Err(error(format!("unexpected frame before {request_id} exited: {body:?}")));
			},
		}
	}
}

/// A daemon command's amendment prompt reaches only the connection that
/// issued it, on the issuing request. Another relay-capable connection that
/// names the same request and query decides nothing; the owner's denial
/// denies, and its approval reruns the command once inside the daemon. The
/// approving owner reads raw frames, so it sees that the daemon never
/// withdraws the prompt it answered.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn p12_amendment_prompts_only_the_issuing_connection() -> Result<()> {
	let Some(daemon) = Daemon::start().await? else {
		return Ok(());
	};
	let daemon_pid = daemon.pid()?;
	let owner = daemon.relay_client("p12-owner").await?;
	let other = daemon.relay_raw("p12-other").await?;
	let queries = owner.client().approval_queries();
	let session = open_session(owner.client(), &daemon.project).await?;

	let forged_command = protected_write("omp-forged");
	let mut forged = exec(owner.client(), &session, &forged_command).await?;
	let (request_id, forged_query) = next_query(&queries).await?;
	assert_eq!(request_id, forged.guard().request_id(), "the prompt rides the issuing request");
	assert_amendment(&forged_query, &forged_command);
	other
		.send(request_id, answer(forged_query.query_id, true))
		.await?;
	// The forged approval drew no reply, and no prompt reached `other`.
	round_trip(&other, 1, &daemon.project).await?;
	owner
		.client()
		.answer_approval(request_id, forged_query.query_id, decision(false))
		.await?;
	let (status, _) = exit(&mut forged).await?;
	assert_eq!(status.outcome, ExecOutcome::Denied as i32, "{status:?}");
	assert!(!daemon.protected("omp-forged").exists(), "the forged approval wrote");

	// The typed client drops a withdrawal of a query it already answered, so
	// the approved command runs from a raw connection that sees every frame:
	// after the answer, its request carries nothing but output and the exit.
	let approver = daemon.relay_raw("p12-approver").await?;
	let approver_session = round_trip(&approver, 1, &daemon.project).await?;
	let approved_command = protected_write("omp-amend");
	approver
		.send(2, client_frame::Body::Exec(exec_request(&approver_session, &approved_command)))
		.await?;
	let approved_query = raw_query(&approver, 2).await?;
	assert_amendment(&approved_query, &approved_command);
	approver
		.send(2, answer(approved_query.query_id, true))
		.await?;
	let (status, _) = raw_exit(&approver, 2).await?;
	assert_eq!(status.outcome, ExecOutcome::Exited as i32, "{status:?}");
	assert_eq!(status.exit_code, Some(0), "{status:?}");
	assert!(
		status.diags.iter().any(|diag| diag
			.text
			.contains("sandbox: rerun with approved scope: write")),
		"{:?}",
		status.diags
	);
	let written = fs::read_to_string(daemon.protected("omp-amend")).context("approved write")?;
	assert_eq!(written, daemon_pid.to_string(), "the approved rerun ran outside the daemon");
	assert_ne!(written, std::process::id().to_string());

	// The approver's prompt reached neither other connection. Each one's
	// round trip is answered after anything the daemon sent it earlier, and
	// the owner's client queues every prompt it receives, whatever request
	// carries it.
	owner
		.client()
		.close_session(CloseSessionRequest { session, ..CloseSessionRequest::default() })
		.await?;
	assert!(
		queries.is_empty(),
		"another connection's prompt reached the owner: {:?}",
		queries.try_recv()
	);
	round_trip(&other, 2, &daemon.project).await?;
	drop(approver);
	drop(other);
	drop(owner);
	daemon.shutdown().await
}

/// Cancelling a command withdraws its open prompt before the command exits.
/// Closing the issuing connection while its prompt is open ends the command
/// without writing, and the session it held runs the next command well
/// before any prompt timeout.
///
/// The close also cancels every command the connection still streams, so
/// this case cannot tell the relay failing the prompt closed from that
/// cancel. The relay's own disconnect, for a command that outlives its
/// connection, is proven by `closing_a_connection_disconnects_its_relay` in
/// `crates/envd/src/server.rs`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn p12_cancel_withdraws_and_a_closed_owner_frees_its_session() -> Result<()> {
	let Some(daemon) = Daemon::start().await? else {
		return Ok(());
	};
	let owner = daemon.relay_client("p12-owner").await?;
	let queries = owner.client().approval_queries();
	let session = open_session(owner.client(), &daemon.project).await?;

	let cancelled_command = protected_write("omp-cancelled");
	let mut cancelled = exec(owner.client(), &session, &cancelled_command).await?;
	let (request_id, query) = next_query(&queries).await?;
	assert_amendment(&query, &cancelled_command);
	cancelled.guard().cancel();
	let (status, _) = exit(&mut cancelled).await?;
	assert_eq!(status.outcome, ExecOutcome::Cancelled as i32, "{status:?}");
	// The client routes frames in arrival order, so a withdrawal sent before
	// the exit is already queued once the exit is read.
	assert_eq!(
		queries.try_recv().ok(),
		Some(ApprovalQueryEvent::Withdrawn { request_id, query_id: query.query_id }),
		"the prompt was not withdrawn before the exit"
	);
	assert!(matches!(
		owner
			.client()
			.answer_approval(request_id, query.query_id, decision(true))
			.await,
		Err(ClientError::ApprovalQueryClosed { .. })
	));
	assert!(!daemon.protected("omp-cancelled").exists(), "the cancelled command wrote");

	let dropped_command = protected_write("omp-dropped");
	let held = exec(owner.client(), &session, &dropped_command).await?;
	let (dropped_request, dropped_query) = next_query(&queries).await?;
	assert_amendment(&dropped_query, &dropped_command);
	// Disarm the run guard so the socket close alone ends the command.
	let _detached = held.relinquish();
	drop(owner);
	within("approval queue close", RELAY_WAIT, async {
		while queries.recv_async().await.is_ok() {}
	})
	.await?;

	let next = daemon.relay_raw("p12-next").await?;
	// Naming the closed connection's prompt from another connection draws no
	// reply.
	next
		.send(dropped_request, answer(dropped_query.query_id, true))
		.await?;
	round_trip(&next, 1, &daemon.project).await?;
	// Sessions belong to the daemon, and one runs its commands in order: this
	// one starts only after the closed connection's command ended.
	next
		.send(2, client_frame::Body::Exec(exec_request(&session, "printf served")))
		.await?;
	let (status, output) = raw_exit(&next, 2).await?;
	assert_eq!(status.outcome, ExecOutcome::Exited as i32, "{status:?}");
	assert_eq!(output, b"served");
	assert!(!daemon.protected("omp-dropped").exists(), "the disconnected command wrote");
	drop(next);
	daemon.shutdown().await
}
