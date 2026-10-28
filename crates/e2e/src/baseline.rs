//! P8 performance recorder for retained TUI frames, the journal-first kernel,
//! and the envd hot-path workload matrix.
//!
//! The synthetic measurements ([`measure`]) cover the retained frame, the
//! kernel loop and journal replay. The matrix recorder ([`record_matrix`])
//! runs the audit's workloads W1 through W9 against real environment
//! authorities, embedded and attached, cold and warm, and summarizes each
//! cell as p50, p95 and median absolute deviation.
//!
//! P8 records host measurements but never gates CI on timing. The one
//! exception is W8, a correctness gate: a recording whose containment or
//! cleanup proof fails is itself a failure.

use std::{
	collections::BTreeMap,
	env, fmt, fs,
	path::{Path, PathBuf},
	process::Command,
	str::FromStr,
	sync::Arc,
	thread,
	time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use omp_agent::{DispatchPolicy, Kernel, RunControl, StaticPrompt, TurnInput};
use omp_ai::{BlockKind, ChatEvent, Completion, ExecutionReceipt, FinishReason, Usage};
use omp_core::Str;
use omp_journal::blob::BlobStore;
use omp_session::{ComponentRegistry, Session};
use omp_tool::Registry;
use omp_tui::{Prop, Renderer, Ui, UiContext, components::TextLeaf};
use serde::{Deserialize, Serialize};

use crate::{
	Context as _, Result, error,
	support::{ScriptedInference, scripted_stream},
};

#[cfg(unix)]
mod census;
#[cfg(unix)]
mod containment;
#[cfg(unix)]
mod recorder;
#[cfg(unix)]
mod topology;
#[cfg(unix)]
mod workloads;

#[cfg(unix)]
pub use recorder::record_matrix;

const SCHEMA_VERSION: u32 = 3;
const TOKEN: &str = "·";

/// Full-loop throughput, in tokens per second, below which a recording is a
/// gross regression.
///
/// The floor is absolute because the loop has to keep up with the stream it
/// receives, not with the scripted stream it is measured against: that raw
/// stream only walks a vector, so even one `write` per commit is tens of times
/// slower and a ratio limit could never hold. The fastest streaming providers
/// sustain a few thousand tokens per second, and this floor keeps several
/// times that in hand. It is also well clear of both sides of the history it
/// has to separate: the dev-profile loop recorded 121 984 tokens/s on the
/// self-hosted M1 Pro once stream deltas were coalesced, and 189 tokens/s
/// while every delta paid its own `F_FULLFSYNC`.
pub const LOOP_THROUGHPUT_FLOOR: f64 = 10_000.0;

/// Replay throughput, in journal entries per second, below which a recording
/// is a gross regression.
///
/// Absolute for the same reason as [`LOOP_THROUGHPUT_FLOOR`]: replay has to
/// reopen real sessions promptly, not beat a synthetic sibling. It sits
/// between the two sides it has to separate, both measured on the 30-rewind,
/// 123-entry workload: the dev-profile replay recorded 3 516 entries/s on
/// the self-hosted M1 Pro (2026-10-24), and the loaded-host pathology it
/// exists to catch replayed the same shape in 1.974 s, 62 entries/s.
pub const REPLAY_THROUGHPUT_FLOOR: f64 = 1_000.0;

/// Recorded frame, agent-loop, and replay measurements for a baseline run.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct BaselineMetrics {
	/// Version of the serialized baseline schema.
	pub schema_version: u32,
	/// Retained-TUI frame measurements.
	pub frame:          FrameMetrics,
	/// Journal-first kernel measurements.
	pub r#loop:         LoopMetrics,
	/// Journal replay measurements.
	pub replay:         ReplayMetrics,
}

/// Replay measurements collected by reopening a rewind-dense journal.
///
/// The workload mirrors the `omp-session` replay law test: rewinds nested
/// under one assistant, each followed by a non-root patch and a stream
/// append, which is the densest rewind shape the law suite pins. Correctness
/// of the replay is asserted by that suite; this records only its cost.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ReplayMetrics {
	/// Nested rewinds in the journal.
	pub rewind_depth:           usize,
	/// Journal entries replayed by `Session::open`.
	pub entry_count:            usize,
	/// Wall time of the replaying `Session::open`, in nanoseconds.
	pub replay_duration_ns:     u128,
	/// Replay throughput in journal entries per second.
	pub entries_per_second:     f64,
	/// Floor the run was classified against
	/// ([`REPLAY_THROUGHPUT_FLOOR`]).
	pub min_entries_per_second: f64,
	/// Whether replay throughput fell below
	/// [`Self::min_entries_per_second`].
	pub gross_regression:       bool,
}

/// Frame-time measurements collected during streaming text updates.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct FrameMetrics {
	/// Number of streamed tokens measured.
	pub token_count:  usize,
	/// Number of individual frame samples.
	pub sample_count: usize,
	/// Ninety-fifth percentile frame duration in nanoseconds.
	pub p95_frame_ns: u128,
}

/// Kernel throughput measurements collected from canonical event streams.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct LoopMetrics {
	/// Number of tokens processed in each sample.
	pub tokens_per_sample:      usize,
	/// Number of independent loop samples.
	pub sample_count:           usize,
	/// Total raw client-stream duration in nanoseconds.
	pub raw_duration_ns:        u128,
	/// Total end-to-end kernel duration in nanoseconds.
	pub full_loop_duration_ns:  u128,
	/// Raw client-stream throughput in tokens per second.
	pub raw_tokens_per_second:  f64,
	/// End-to-end kernel throughput in tokens per second.
	pub full_tokens_per_second: f64,
	/// Ratio of raw throughput to end-to-end throughput, recorded for
	/// comparison between runs; it does not classify the run.
	pub slowdown_ratio:         f64,
	/// Throughput floor, in tokens per second, the run was classified against
	/// ([`LOOP_THROUGHPUT_FLOOR`]).
	pub min_tokens_per_second:  f64,
	/// Whether end-to-end throughput fell below
	/// [`Self::min_tokens_per_second`].
	pub gross_regression:       bool,
}

fn token_events(tokens: usize) -> Vec<ChatEvent> {
	let mut events = Vec::with_capacity(tokens.saturating_add(3));
	events.push(ChatEvent::BlockStarted { index: 0, kind: BlockKind::Text });
	for _ in 0..tokens {
		events.push(ChatEvent::TextDelta { index: 0, text: Str::new_static(TOKEN) });
	}
	events.push(ChatEvent::Completed(Completion {
		reason:  FinishReason::Stop,
		blocks:  1,
		usage:   Usage::default(),
		receipt: ExecutionReceipt::default().into(),
	}));
	events
}

/// Measures retained-frame, end-to-end kernel, and replay performance.
pub async fn measure(
	frame_tokens: usize,
	loop_tokens: usize,
	samples: usize,
	rewind_depth: usize,
) -> Result<BaselineMetrics> {
	if frame_tokens < 100 || loop_tokens < 100 || samples == 0 {
		return Err(error("baseline requires at least 100 frame and loop tokens and one sample"));
	}
	let frame = measure_frames(frame_tokens)?;
	let raw_duration = measure_raw(loop_tokens, samples).await?;
	let scratch = tempfile::tempdir().context("create loop baseline scratch directory")?;
	let scripts = (0..samples).map(|_| token_events(loop_tokens));
	let (inference, _) = ScriptedInference::new(scripts);
	let mut kernel = Kernel::new(
		inference,
		Arc::new(Registry::new()),
		DispatchPolicy::new(
			BlobStore::open(scratch.path().join("blobs")).context("open blob store")?,
		),
		StaticPrompt(Str::new_static("P8 baseline")),
	);
	let mut full_duration = Duration::ZERO;
	for ordinal in 0..samples {
		let path = scratch.path().join(format!("sample-{ordinal}.oms"));
		let mut session =
			Session::create(path, ComponentRegistry::standard()).context("create baseline session")?;
		let started = Instant::now();
		kernel
			.run_turn(
				&mut session,
				TurnInput { text: Str::new_static("measure"), attachments: Vec::new() },
				RunControl::default(),
			)
			.await
			.context("measure full kernel loop")?;
		full_duration = full_duration.saturating_add(started.elapsed());
	}
	let total_tokens = loop_tokens
		.checked_mul(samples)
		.context("loop token count overflow")?;
	let raw_rate = duration_rate(total_tokens, raw_duration)?;
	let full_rate = duration_rate(total_tokens, full_duration)?;
	let slowdown = slowdown_ratio(raw_rate, full_rate)?;
	let replay = measure_replay(rewind_depth)?;
	Ok(BaselineMetrics {
		schema_version: SCHEMA_VERSION,
		frame,
		r#loop: LoopMetrics {
			tokens_per_sample:      loop_tokens,
			sample_count:           samples,
			raw_duration_ns:        raw_duration.as_nanos(),
			full_loop_duration_ns:  full_duration.as_nanos(),
			raw_tokens_per_second:  raw_rate,
			full_tokens_per_second: full_rate,
			slowdown_ratio:         slowdown,
			min_tokens_per_second:  LOOP_THROUGHPUT_FLOOR,
			gross_regression:       is_gross_regression(full_rate),
		},
		replay,
	})
}

fn measure_frames(tokens: usize) -> Result<FrameMetrics> {
	let root = TextLeaf::new().with(Prop::Id, "stream").text("");
	let mut ui = Ui::from_root(root, 80, UiContext::default());
	let mut renderer = Renderer::new(Vec::<u8>::with_capacity(tokens.saturating_mul(16)));
	ui.present(&mut renderer, 24)
		.context("paint warmup frame")?;
	let mut text = String::with_capacity(tokens.saturating_mul(TOKEN.len()));
	let mut elapsed = Vec::with_capacity(tokens);
	for _ in 0..tokens {
		text.push_str(TOKEN);
		let started = Instant::now();
		if !ui.set_text("stream", text.as_str()) {
			return Err(error("token-storm text component stopped accepting updates"));
		}
		ui.present(&mut renderer, 24)
			.context("paint token-storm frame")?;
		elapsed.push(started.elapsed().as_nanos());
	}
	elapsed.sort_unstable();
	let rank = elapsed
		.len()
		.saturating_mul(95)
		.div_ceil(100)
		.saturating_sub(1);
	Ok(FrameMetrics {
		token_count:  tokens,
		sample_count: elapsed.len(),
		p95_frame_ns: elapsed[rank],
	})
}

async fn measure_raw(tokens: usize, samples: usize) -> Result<Duration> {
	use futures::StreamExt as _;
	let mut total = Duration::ZERO;
	for _ in 0..samples {
		let started = Instant::now();
		let mut stream = scripted_stream(token_events(tokens));
		while let Some(event) = stream.next().await {
			event.context("consume canonical inference event")?;
		}
		total = total.saturating_add(started.elapsed());
	}
	Ok(total)
}

fn measure_replay(rewind_depth: usize) -> Result<ReplayMetrics> {
	use omp_dom::{KnownTag, NodeSpec, Op, PropId, Tag, Txn, Value};
	if rewind_depth == 0 {
		return Err(error("replay baseline requires at least one rewind"));
	}
	let scratch = tempfile::tempdir().context("create replay baseline scratch directory")?;
	let path = scratch.path().join("replay.oms");
	let mut session =
		Session::create(&path, ComponentRegistry::standard()).context("create replay session")?;
	session.begin_turn().context("replay turn starts")?;
	session
		.assistant_start("model", "provider", "route")
		.context("replay assistant starts")?;
	let jobs = session
		.dom()
		.children(session.dom().meta())
		.iter()
		.copied()
		.find(|handle| {
			session
				.dom()
				.get(*handle)
				.is_some_and(|node| node.tag == Tag::Known(KnownTag::Jobs))
		})
		.context("jobs component exists")?;
	let assistant = session
		.dom()
		.children(session.dom().body())
		.last()
		.copied()
		.and_then(|turn| {
			session
				.dom()
				.children(turn)
				.iter()
				.rev()
				.copied()
				.find(|handle| {
					session
						.dom()
						.get(*handle)
						.is_some_and(|node| node.tag == Tag::Known(KnownTag::Assistant))
				})
		})
		.context("replay assistant handle")?;
	for depth in 0..rewind_depth {
		let target = session.head().context("nested rewind target")?;
		session
			.user(format!("abandoned-{depth}"), Vec::new())
			.context("abandoned work")?;
		session.rewind(target).context("nested rewind")?;
		let cause = session.head().context("rewound head")?;
		session
			.patch(Txn {
				cause,
				label: None,
				ops: vec![Op::Ins {
					parent: jobs,
					after:  None,
					node:   NodeSpec::new(KnownTag::Job)
						.with_prop(PropId::Id, Value::Str(Str::new(format!("job-{depth}")))),
				}],
			})
			.context("nested non-root patch")?;
		let prop = omp_dom::PropKey::Custom(Str::new(format!("nested-{depth}")));
		let sid = session
			.stream_open(assistant, prop)
			.context("stream opens")?;
		session.stream_append(sid, "x").context("stream appends")?;
	}
	drop(session);
	let entry_count = omp_journal::Journal::scan(&path)
		.context("scan replay journal")?
		.len();
	let started = Instant::now();
	let restored =
		Session::open(&path, ComponentRegistry::standard()).context("replay journal opens")?;
	let elapsed = started.elapsed();
	drop(restored);
	let rate = duration_rate(entry_count, elapsed)?;
	Ok(ReplayMetrics {
		rewind_depth,
		entry_count,
		replay_duration_ns: elapsed.as_nanos(),
		entries_per_second: rate,
		min_entries_per_second: REPLAY_THROUGHPUT_FLOOR,
		gross_regression: is_replay_gross_regression(rate),
	})
}

/// Computes token throughput from a nonzero duration and token count.
pub fn duration_rate(tokens: usize, duration: Duration) -> Result<f64> {
	if tokens == 0 {
		return Err(error("token count must be non-zero"));
	}
	if duration.is_zero() {
		return Err(error("measured duration must be non-zero"));
	}
	let rate = tokens as f64 / duration.as_secs_f64();
	if !rate.is_finite() || rate <= 0.0 {
		return Err(error("token rate is not finite and positive"));
	}
	Ok(rate)
}

/// Computes the end-to-end slowdown relative to raw token throughput.
pub fn slowdown_ratio(raw_rate: f64, full_rate: f64) -> Result<f64> {
	if !raw_rate.is_finite() || raw_rate <= 0.0 || !full_rate.is_finite() || full_rate <= 0.0 {
		return Err(error("token rates must be finite and positive"));
	}
	Ok(raw_rate / full_rate)
}

/// Whether a full-loop throughput is a gross regression: below
/// [`LOOP_THROUGHPUT_FLOOR`]. A rate exactly at the floor is not.
#[must_use]
pub fn is_gross_regression(full_tokens_per_second: f64) -> bool {
	full_tokens_per_second < LOOP_THROUGHPUT_FLOOR
}

/// Whether a replay throughput is a gross regression: below
/// [`REPLAY_THROUGHPUT_FLOOR`]. A rate exactly at the floor is not.
#[must_use]
pub fn is_replay_gross_regression(entries_per_second: f64) -> bool {
	entries_per_second < REPLAY_THROUGHPUT_FLOOR
}

/// Serializes measurements to the requested artifact path.
pub fn write_metrics(path: &Path, metrics: &BaselineMetrics) -> Result<()> {
	if let Some(parent) = path.parent()
		&& !parent.as_os_str().is_empty()
	{
		fs::create_dir_all(parent)
			.with_context(|| format!("create artifact directory {}", parent.display()))?;
	}
	let bytes = serde_json::to_vec_pretty(metrics).context("serialize baseline metrics")?;
	fs::write(path, bytes).with_context(|| format!("write baseline artifact {}", path.display()))?;
	Ok(())
}

/// Version of the serialized [`MatrixArtifact`] schema.
pub const MATRIX_SCHEMA_VERSION: u32 = 1;

/// Repetitions recorded per matrix cell unless a run overrides them: audit
/// §12.3 asks for at least 30 per workload and mode.
pub const DEFAULT_REPETITIONS: usize = 30;

/// Which part of the matrix to record, and how many repetitions per cell.
#[derive(Clone, Debug)]
pub struct MatrixSelection {
	/// Transports to record, in recording order.
	pub transports:  Vec<Transport>,
	/// Daemon states to record, in recording order.
	pub states:      Vec<DaemonState>,
	/// Workloads to record, in recording order.
	pub workloads:   Vec<Workload>,
	/// Repetitions recorded per cell.
	pub repetitions: usize,
}

impl MatrixSelection {
	/// Selects every transport, daemon state and workload at the audit's
	/// default repetition count.
	#[must_use]
	pub fn complete() -> Self {
		Self {
			transports:  Transport::ALL.to_vec(),
			states:      DaemonState::ALL.to_vec(),
			workloads:   Workload::ALL.to_vec(),
			repetitions: DEFAULT_REPETITIONS,
		}
	}
}

/// How the client reaches the environment authority.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Transport {
	/// The authority runs in the client process behind `EnvClient::in_process`.
	Embedded,
	/// A production `envd` child process reached through the partition router
	/// and a Unix socket.
	Attached,
}

impl Transport {
	/// Every transport, in recording order.
	pub const ALL: [Self; 2] = [Self::Embedded, Self::Attached];

	/// Returns the stable artifact and command-line name.
	pub const fn as_str(self) -> &'static str {
		match self {
			Self::Embedded => "embedded",
			Self::Attached => "attached",
		}
	}
}

impl fmt::Display for Transport {
	fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
		formatter.write_str(self.as_str())
	}
}

impl FromStr for Transport {
	type Err = crate::Error;

	fn from_str(text: &str) -> Result<Self> {
		Self::ALL
			.into_iter()
			.find(|transport| transport.as_str().eq_ignore_ascii_case(text))
			.ok_or_else(|| error(format!("unknown mode {text:?}; expected embedded or attached")))
	}
}

/// Whether the environment authority was started for the measured repetition
/// or had already served the workload.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum DaemonState {
	/// Every repetition starts a fresh authority and measures its first use.
	Cold,
	/// One authority serves an unmeasured warm-up, then every repetition.
	Warm,
}

impl DaemonState {
	/// Every daemon state, in recording order.
	pub const ALL: [Self; 2] = [Self::Cold, Self::Warm];

	/// Returns the stable artifact and command-line name.
	pub const fn as_str(self) -> &'static str {
		match self {
			Self::Cold => "cold",
			Self::Warm => "warm",
		}
	}
}

impl fmt::Display for DaemonState {
	fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
		formatter.write_str(self.as_str())
	}
}

impl FromStr for DaemonState {
	type Err = crate::Error;

	fn from_str(text: &str) -> Result<Self> {
		Self::ALL
			.into_iter()
			.find(|state| state.as_str().eq_ignore_ascii_case(text))
			.ok_or_else(|| error(format!("unknown daemon state {text:?}; expected cold or warm")))
	}
}

/// One of the audit's envd hot-path workloads (§12.1).
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
pub enum Workload {
	/// Repeated 1 KiB document reads.
	W1,
	/// Single-file document edits through the versioned document API.
	W2,
	/// Repeated `bash` runs of `true`.
	W3,
	/// Command output around the 64 KiB and 8 MiB projection thresholds.
	W4,
	/// Search over a large repository.
	W5,
	/// Python extension tool calls.
	W6,
	/// Worktree-backed subagent slots.
	W7,
	/// Daemon-kill (attached) or cancellation (embedded) containment gate.
	W8,
	/// Cold versus warm daemon attach.
	W9,
}

impl Workload {
	/// Every workload, in recording order.
	pub const ALL: [Self; 9] =
		[Self::W1, Self::W2, Self::W3, Self::W4, Self::W5, Self::W6, Self::W7, Self::W8, Self::W9];

	/// Returns the stable artifact and command-line name.
	pub const fn as_str(self) -> &'static str {
		match self {
			Self::W1 => "W1",
			Self::W2 => "W2",
			Self::W3 => "W3",
			Self::W4 => "W4",
			Self::W5 => "W5",
			Self::W6 => "W6",
			Self::W7 => "W7",
			Self::W8 => "W8",
			Self::W9 => "W9",
		}
	}
}

impl fmt::Display for Workload {
	fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
		formatter.write_str(self.as_str())
	}
}

impl FromStr for Workload {
	type Err = crate::Error;

	fn from_str(text: &str) -> Result<Self> {
		Self::ALL
			.into_iter()
			.find(|workload| workload.as_str().eq_ignore_ascii_case(text))
			.ok_or_else(|| error(format!("unknown workload {text:?}; expected W1 through W9")))
	}
}

/// Distribution of one metric across a cell's repetitions, in the metric's
/// unit (nanoseconds for names ending `_ns`).
///
/// The audit's noise band is `2 * mad`.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct Summary {
	/// Number of repetitions summarized.
	pub samples: usize,
	/// Smallest sample.
	pub min:     f64,
	/// Largest sample.
	pub max:     f64,
	/// Median.
	pub p50:     f64,
	/// Nearest-rank ninety-fifth percentile.
	pub p95:     f64,
	/// Median absolute deviation from the median, unscaled.
	pub mad:     f64,
}

impl Summary {
	/// Summarizes finite samples.
	pub fn of(samples: &[f64]) -> Result<Self> {
		if samples.is_empty() {
			return Err(error("a summary needs at least one sample"));
		}
		if samples.iter().any(|sample| !sample.is_finite()) {
			return Err(error("samples must be finite"));
		}
		let mut sorted = samples.to_vec();
		sorted.sort_by(f64::total_cmp);
		let p50 = median(&sorted);
		let mut deviations: Vec<f64> = sorted.iter().map(|sample| (sample - p50).abs()).collect();
		deviations.sort_by(f64::total_cmp);
		let rank = sorted
			.len()
			.saturating_mul(95)
			.div_ceil(100)
			.saturating_sub(1);
		Ok(Self {
			samples: sorted.len(),
			min: sorted[0],
			max: sorted[sorted.len() - 1],
			p50,
			p95: sorted[rank],
			mad: median(&deviations),
		})
	}
}

fn median(sorted: &[f64]) -> f64 {
	let middle = sorted.len() / 2;
	if sorted.len() % 2 == 1 {
		sorted[middle]
	} else {
		sorted[middle - 1].midpoint(sorted[middle])
	}
}

/// One named metric of a matrix cell.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct MatrixMetric {
	/// Metric name; a `_ns` suffix marks a nanosecond duration.
	pub name:    String,
	/// Distribution across the cell's repetitions.
	#[serde(flatten)]
	pub summary: Summary,
}

/// Every recorded repetition of one workload, variant, transport and daemon
/// state.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct MatrixCell {
	/// Audit workload.
	pub workload:                  Workload,
	/// Workload parameter such as an output size or slot count, if it has one.
	pub variant:                   Option<String>,
	/// How the client reached the authority.
	pub transport:                 Transport,
	/// Whether each repetition started the authority or reused a warm one.
	pub daemon_state:              DaemonState,
	/// Repetitions recorded.
	pub repetitions:               usize,
	/// Operations one repetition performs.
	pub operations_per_repetition: u64,
	/// Distribution of each metric across the repetitions.
	pub metrics:                   Vec<MatrixMetric>,
	/// Correctness facts every repetition verified, such as bytes received or
	/// processes found after a daemon kill.
	pub facts:                     BTreeMap<String, u64>,
}

/// A matrix cell the recorder could not complete, or whose correctness gate
/// failed.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct MatrixFailure {
	/// Audit workload.
	pub workload:     Workload,
	/// Workload parameter, if it has one.
	pub variant:      Option<String>,
	/// How the client reached the authority.
	pub transport:    Transport,
	/// Whether each repetition started the authority or reused a warm one.
	pub daemon_state: DaemonState,
	/// Why the cell failed.
	pub error:        String,
}

/// What the host was and which commit it measured.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct MatrixMetadata {
	/// Operating system family, as `std::env::consts::OS`.
	pub host_os:             String,
	/// CPU architecture, as `std::env::consts::ARCH`.
	pub host_arch:           String,
	/// Kernel name and release, as `uname -sr` reports them.
	pub host_version:        String,
	/// Logical CPUs available to the recorder.
	pub host_cpus:           usize,
	/// Commit the measured code was built from.
	pub commit_sha:          String,
	/// Whether tracked files differed from that commit.
	pub commit_dirty:        bool,
	/// Cargo profile the recorder ran under.
	pub build_profile:       String,
	/// Wall-clock start of the recording, in milliseconds since the Unix epoch.
	pub recorded_at_unix_ms: u64,
	/// Repetitions requested for every cell.
	pub repetitions:         usize,
	/// Courtesy interval before forced interruption that W8 holds
	/// containment to, in milliseconds.
	pub interrupt_grace_ms:  u64,
}

impl MatrixMetadata {
	/// Describes the current host, commit and build for a recording of
	/// `repetitions` repetitions per cell.
	#[must_use]
	pub fn capture(repetitions: usize) -> Self {
		let github_sha = env::var("GITHUB_SHA").ok().filter(|sha| !sha.is_empty());
		let (commit_sha, commit_dirty) = match github_sha {
			Some(sha) => (sha, false),
			None => (
				command_line("git", &["rev-parse", "HEAD"]).unwrap_or_else(|| "unknown".to_owned()),
				command_line("git", &["status", "--porcelain", "--untracked-files=no"])
					.is_some_and(|status| !status.is_empty()),
			),
		};
		Self {
			host_os: env::consts::OS.to_owned(),
			host_arch: env::consts::ARCH.to_owned(),
			host_version: command_line("uname", &["-sr"]).unwrap_or_else(|| "unknown".to_owned()),
			host_cpus: thread::available_parallelism().map_or(1, usize::from),
			commit_sha,
			commit_dirty,
			build_profile: if cfg!(debug_assertions) {
				"debug"
			} else {
				"release"
			}
			.to_owned(),
			recorded_at_unix_ms: SystemTime::now()
				.duration_since(UNIX_EPOCH)
				.map_or(0, |elapsed| u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)),
			repetitions,
			interrupt_grace_ms: omp_tool::DEFAULT_INTERRUPT_GRACE
				.to_std()
				.map_or(0, |grace| u64::try_from(grace.as_millis()).unwrap_or(u64::MAX)),
		}
	}
}

fn command_line(program: &str, args: &[&str]) -> Option<String> {
	let output = Command::new(program).args(args).output().ok()?;
	if !output.status.success() {
		return None;
	}
	Some(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

/// The complete P8 matrix recording: the synthetic kernel measurements plus
/// every workload cell.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct MatrixArtifact {
	/// Version of the serialized matrix schema.
	pub schema_version: u32,
	/// Host, commit and build description.
	pub metadata:       MatrixMetadata,
	/// Retained-frame, kernel-loop and replay measurements.
	pub synthetic:      Option<BaselineMetrics>,
	/// Completed cells, in recording order.
	pub cells:          Vec<MatrixCell>,
	/// Cells that failed, which fails the recording.
	pub failures:       Vec<MatrixFailure>,
}

/// Writes the matrix recording to `path`, replacing any earlier recording
/// atomically so an interrupted run still leaves a readable artifact.
pub fn write_matrix(path: &Path, artifact: &MatrixArtifact) -> Result<()> {
	if let Some(parent) = path.parent()
		&& !parent.as_os_str().is_empty()
	{
		fs::create_dir_all(parent)
			.with_context(|| format!("create artifact directory {}", parent.display()))?;
	}
	let bytes = serde_json::to_vec_pretty(artifact).context("serialize matrix artifact")?;
	let mut staged = path.as_os_str().to_owned();
	staged.push(".partial");
	let staged = PathBuf::from(staged);
	fs::write(&staged, bytes)
		.with_context(|| format!("write matrix artifact {}", staged.display()))?;
	fs::rename(&staged, path)
		.with_context(|| format!("publish matrix artifact {}", path.display()))?;
	Ok(())
}

/// Points the user config, data, state and cache roots at a private directory,
/// so authorities the recorder starts in-process run the shipped defaults
/// instead of the developer's MCP servers, hooks and plugins. Daemons the
/// recorder spawns already get the same treatment.
///
/// Call it from `main` before any thread starts: it edits the process
/// environment. Dropping the returned directory removes it.
pub fn isolate_user_roots() -> Result<tempfile::TempDir> {
	let home = tempfile::Builder::new()
		.prefix("omp-p8-home-")
		.tempdir()
		.context("create isolated user roots")?;
	for root in ["config", "data", "state", "cache"] {
		fs::create_dir_all(home.path().join(root))
			.with_context(|| format!("create isolated {root} root"))?;
	}
	// SAFETY: the caller runs this before the async runtime or any other thread
	// exists, so nothing reads the environment concurrently.
	unsafe {
		env::set_var("HOME", home.path());
		env::set_var("OMP_CONFIG_DIR", home.path().join("config"));
		env::set_var("OMP_DATA_DIR", home.path().join("data"));
		env::set_var("OMP_STATE_DIR", home.path().join("state"));
		env::set_var("OMP_CACHE_DIR", home.path().join("cache"));
	}
	Ok(home)
}
