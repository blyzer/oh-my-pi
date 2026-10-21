//! Driver-level delegated-run host: profile table, executor registry, and
//! `run_delegated_turn`.
//!
//! # Profile table
//!
//! Each supported executor has an operator convar naming the binary to start.
//! It carries only `flags: archive` — **no** `PROJECT` flag — so a project cfg
//! overlay cannot change it. The con engine rejects any attempt with
//! [`omp_con::ConError::ProjectVarDenied`], the same enforcement that protects
//! every `sv_exec_remote_*` variable in `adw/remote.rs`.
//!
//! There is deliberately no operator-supplied argument list: each executor
//! passes exactly the arguments its protocol needs, so an operator cannot add a
//! flag that weakens the agent's own sandbox or approval settings.
//!
//! # Registry
//!
//! [`ExecutorKind`] is a closed enum (one variant per supported executor).
//! [`ExecutorRegistry`] resolves an executor id to a concrete executor
//! without boxing on the hot path. The registry owns a `CodexExecutor` (the
//! only concrete executor today) and builds it once from the profile table.
//!
//! # Host function
//!
//! [`run_delegated_turn`] composes `start_session` + `ActiveSession::run_turn`
//! around a caller-supplied session and cancellation token. Events are
//! journaled into the session as the turn runs (not after it ends), so a long
//! turn is visible while it is in flight and a host crash loses at most the
//! event being written:
//!
//! | Event kind            | Session journal operation          |
//! |-----------------------|------------------------------------|
//! | `TurnStarted`         | `begin_turn`                       |
//! | `AgentMessage`        | `assistant_start` → text stream → `assistant_end` |
//! | `AgentTextDelta`      | streamed into the open assistant message |
//! | `TurnCompleted`       | `assistant_end(stop_reason)`       |
//! | `Usage` (last)        | `receipt(TurnReceipt { … })`       |
//! | `ToolEvent`           | **gap** — no session representation exists; events are forwarded on the channel only |
//! | `Permission`          | **gap** — permission prompts are not journaled; the decision is not recorded in the session |
//!
//! One session serves exactly one turn: the host always terminates the agent's
//! process tree when the turn ends, whatever the outcome. Cancelling interrupts
//! the CLI (killing it if it does not settle in time) and the result is
//! `ExecutorError::Cancelled`. A journal failure is reported only when the turn
//! itself succeeded; a turn that failed, crashed or was cancelled reports its
//! own error.

use std::{ffi::OsString, time::Instant};

use flume::Sender;
use omp_agent::delegated::{
	ActiveSession, Capabilities, DelegatedEvent, DelegatedExecutor, ExecutorError, ExecutorId,
	LifecycleEvent, PermissionResolver, SessionRequest, TurnInput, TurnLimits, TurnReport,
	start_session,
};
use omp_con::Ctx;
use omp_core::Str;
use omp_dom::{Handle, KnownTag, PropId, Sid, Tag};
use omp_envd::codex_app_server::CodexExecutor;
use omp_journal::data::TurnReceipt;
use omp_session::{Session, SessionError};
use thiserror::Error;
use tokio_util::sync::CancellationToken;

// ---------------------------------------------------------------------------
// Profile table
// ---------------------------------------------------------------------------

omp_con::var! {
	/// Program (path or name on `$PATH`) used to start the Codex agent CLI.
	/// Always operator-controlled; a project cfg cannot override it.
	pub static SV_DELEGATED_CODEX_PROGRAM = sv_delegated_codex_program: Str {
		default: Str::new_static("codex"),
		flags: archive,
	};
}

/// The program for one executor profile.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExecutorProfile {
	/// Binary name or absolute path.
	pub program: OsString,
}

/// Reads the Codex executor profile from `ctx`.
#[must_use]
pub fn codex_profile(ctx: &Ctx) -> ExecutorProfile {
	ExecutorProfile { program: OsString::from(SV_DELEGATED_CODEX_PROGRAM.get(ctx).as_str()) }
}

// ---------------------------------------------------------------------------
// Executor registry
// ---------------------------------------------------------------------------

/// A resolved executor, usable without heap allocation on the turn hot path.
///
/// Variants are ordered: `Codex` is first because it is the only executor
/// today; future executors become new variants.
pub enum ExecutorKind {
	/// The Codex CLI adapter.
	Codex(CodexExecutor),
}

impl ExecutorKind {
	/// The declared executor id.
	#[must_use]
	pub fn id(&self) -> &ExecutorId {
		match self {
			Self::Codex(e) => &e.capabilities().executor,
		}
	}

	/// The declared capabilities.
	#[must_use]
	pub fn capabilities(&self) -> &Capabilities {
		match self {
			Self::Codex(e) => e.capabilities(),
		}
	}
}

/// Resolves executor ids to concrete executors, built once from the profile
/// table.
///
/// Start with [`ExecutorRegistry::from_ctx`] and pass the registry into
/// [`run_delegated_turn`] by reference.
pub struct ExecutorRegistry {
	executors: Vec<ExecutorKind>,
}

impl ExecutorRegistry {
	/// Builds a registry from `ctx`, constructing each executor from the
	/// operator-controlled profile convars.
	#[must_use]
	pub fn from_ctx(ctx: &Ctx) -> Self {
		let profile = codex_profile(ctx);
		let codex = CodexExecutor::new(profile.program);
		Self { executors: vec![ExecutorKind::Codex(codex)] }
	}

	/// Looks up an executor by its declared id.
	///
	/// Returns `None` when no executor with that id is registered.
	#[must_use]
	pub fn get(&self, id: &ExecutorId) -> Option<&ExecutorKind> {
		self.executors.iter().find(|e| e.id() == id)
	}

	/// The complete list of registered executors.
	#[must_use]
	pub fn executors(&self) -> &[ExecutorKind] {
		&self.executors
	}
}

// ---------------------------------------------------------------------------
// Host function error
// ---------------------------------------------------------------------------

/// Why a delegated turn could not complete.
#[derive(Debug, Error)]
pub enum DelegatedHostError {
	/// The requested executor id is not registered.
	#[error("executor `{id}` is not registered")]
	UnknownExecutor {
		/// The unrecognized executor id.
		id: ExecutorId,
	},
	/// The executor or session reported an error.
	#[error("delegated executor error")]
	Executor(#[source] ExecutorError),
	/// Writing to the session journal failed.
	#[error("session journal error")]
	Session(#[source] SessionError),
}

impl From<ExecutorError> for DelegatedHostError {
	fn from(e: ExecutorError) -> Self {
		Self::Executor(e)
	}
}

impl From<SessionError> for DelegatedHostError {
	fn from(e: SessionError) -> Self {
		Self::Session(e)
	}
}

// ---------------------------------------------------------------------------
// Journaling state
// ---------------------------------------------------------------------------

/// Mutable journaling state accumulated across events for one turn.
struct JournalState {
	/// Whether `begin_turn` was called.
	turn_started:   bool,
	/// Whether an assistant message is open.
	assistant_open: bool,
	/// Open stream id for the current assistant text, if any.
	stream_id:      Option<Sid>,
	/// Last usage reported by the CLI.
	last_usage:     omp_ai::Usage,
	/// Wall-clock start of the turn.
	started_at:     Option<Instant>,
}

impl JournalState {
	fn new() -> Self {
		Self {
			turn_started:   false,
			assistant_open: false,
			stream_id:      None,
			last_usage:     omp_ai::Usage::default(),
			started_at:     None,
		}
	}

	/// Calls `begin_turn` exactly once.
	fn ensure_turn(&mut self, session: &mut Session) -> Result<(), SessionError> {
		if self.turn_started {
			return Ok(());
		}
		self.turn_started = true;
		self.started_at = Some(Instant::now());
		session.begin_turn()?;
		Ok(())
	}

	/// Opens an assistant message if none is open, then opens a text stream
	/// on the assistant node, returning the stream id.
	fn ensure_stream(
		&mut self,
		session: &mut Session,
		executor_id: &ExecutorId,
	) -> Result<Sid, SessionError> {
		if !self.assistant_open {
			session.assistant_start(executor_id.as_str(), "delegated", executor_id.as_str())?;
			self.assistant_open = true;
		}
		if let Some(sid) = self.stream_id {
			return Ok(sid);
		}
		// Walk the DOM to find the current assistant node, mirroring the
		// approach from `omp_agent::loop::current_assistant`.
		let node = current_assistant_node(session.dom())?;
		let sid = session.stream_open(node, PropId::Text.into())?;
		self.stream_id = Some(sid);
		Ok(sid)
	}

	/// Closes the stream and assistant message.
	fn close_assistant(
		&mut self,
		session: &mut Session,
		stop_reason: &str,
	) -> Result<(), SessionError> {
		if let Some(sid) = self.stream_id.take() {
			session.stream_close(sid)?;
		}
		if self.assistant_open {
			session.assistant_end(stop_reason)?;
			self.assistant_open = false;
		}
		Ok(())
	}

	/// Journals a `TurnReceipt` from the last usage seen.
	fn record_receipt(
		&self,
		session: &mut Session,
		elapsed_ms: Option<u64>,
	) -> Result<(), SessionError> {
		let u = self.last_usage;
		session
			.receipt(TurnReceipt {
				tokens_in: u.input_tokens,
				tokens_out: u.output_tokens,
				cache_read: u.cache_read_tokens,
				cache_write: u.cache_write_tokens,
				duration_ms: elapsed_ms,
				premium_requests_millionths: u.premium_requests_millionths,
				..TurnReceipt::default()
			})
			.map(|_| ())
	}
}

/// Finds the most-recent [`KnownTag::Assistant`] child of the current turn
/// in the DOM, mirroring what `omp_agent::loop::current_assistant` does.
fn current_assistant_node(dom: &omp_dom::Dom) -> Result<Handle, SessionError> {
	let turn = dom
		.children(dom.body())
		.last()
		.copied()
		.ok_or(SessionError::NoActiveTurn)?;
	dom.children(turn)
		.iter()
		.rev()
		.copied()
		.find(|h| {
			dom.get(*h)
				.is_some_and(|n| n.tag == Tag::Known(KnownTag::Assistant))
		})
		.ok_or(SessionError::NoActiveAssistant)
}

// ---------------------------------------------------------------------------
// Core host function
// ---------------------------------------------------------------------------

/// Runs one delegated turn end-to-end.
///
/// 1. Looks up `executor_id` in `registry`.
/// 2. Calls `start_session` using `request`.
/// 3. Delivers `input` via `run_turn`, forwarding events to `events`.
/// 4. Journals each event into `session` (see module-level table for gaps).
/// 5. Records a [`TurnReceipt`] from the final usage.
/// 6. Returns the [`TurnReport`].
///
/// Cancelling `cancel` mid-turn causes the host to interrupt/kill the CLI and
/// return [`DelegatedHostError::Executor`] wrapping
/// `ExecutorError::Cancelled`. The session is not reused after that.
///
/// # Errors
///
/// [`DelegatedHostError::UnknownExecutor`] when `executor_id` is not in the
/// registry. [`DelegatedHostError::Executor`] on executor/session failure.
/// [`DelegatedHostError::Session`] on journal failure.
pub async fn run_delegated_turn<R: PermissionResolver>(
	registry: &ExecutorRegistry,
	executor_id: &ExecutorId,
	request: &SessionRequest,
	input: &TurnInput,
	limits: TurnLimits,
	mut resolver: R,
	cancel: &CancellationToken,
	events: &Sender<DelegatedEvent>,
	session: &mut Session,
) -> Result<TurnReport, DelegatedHostError> {
	match registry.get(executor_id) {
		Some(ExecutorKind::Codex(executor)) => {
			run_turn_on(executor, request, input, limits, &mut resolver, cancel, events, session).await
		},
		None => Err(DelegatedHostError::UnknownExecutor { id: executor_id.clone() }),
	}
}

/// Generic over the concrete executor type; called from [`run_delegated_turn`]
/// after the dispatch.
async fn run_turn_on<E, R>(
	executor: &E,
	request: &SessionRequest,
	input: &TurnInput,
	limits: TurnLimits,
	resolver: &mut R,
	cancel: &CancellationToken,
	events: &Sender<DelegatedEvent>,
	session: &mut Session,
) -> Result<TurnReport, DelegatedHostError>
where
	E: DelegatedExecutor,
	R: PermissionResolver,
{
	let executor_id = executor.capabilities().executor.clone();

	let mut active: ActiveSession<E::Session> = start_session(executor, request).await?;

	// The turn pushes events into `tx` as it runs; this side journals them as
	// they arrive, so a long turn is visible in the session while it is still
	// running and a host crash loses at most the event in flight. A closed
	// downstream `events` never stops the turn.
	let (tx, rx) = flume::unbounded::<DelegatedEvent>();
	let mut state = JournalState::new();
	let mut journal_failure: Option<SessionError> = None;

	let journal = async {
		while let Ok(event) = rx.recv_async().await {
			let _ = events.send(event.clone());
			// After the first journal failure keep forwarding but stop writing:
			// the turn's own outcome must still reach the caller.
			if journal_failure.is_none()
				&& let Err(error) = journal_event(&event, &mut state, &executor_id, session)
			{
				journal_failure = Some(error);
			}
		}
	};
	let turn = async {
		// The turn owns the only sender, so the channel closes when it ends and
		// the journaling side reaches end-of-stream.
		let result = active.run_turn(input, limits, cancel, resolver, &tx).await;
		drop(tx);
		result
	};
	let (turn_result, ()) = tokio::join!(turn, journal);

	let elapsed_ms = state
		.started_at
		.map(|t| u64::try_from(t.elapsed().as_millis()).unwrap_or(u64::MAX));

	// Close any open assistant stream/message with the correct stop reason, and
	// record the receipt, whatever the turn's outcome.
	if journal_failure.is_none() {
		let stop_reason = match &turn_result {
			Ok(report) => report.end.to_string(),
			Err(ExecutorError::Cancelled { .. }) => "interrupted".to_owned(),
			Err(_) => "error".to_owned(),
		};
		journal_failure = state.close_assistant(session, &stop_reason).err();
	}
	if journal_failure.is_none() && state.turn_started {
		journal_failure = state.record_receipt(session, elapsed_ms).err();
	}

	// Orderly end: interrupt, bounded grace, kill, reap.
	let teardown = active.terminate().await;

	// The turn's own outcome wins: a journal or teardown failure must not hide a
	// turn that failed, was cancelled, or succeeded.
	match turn_result {
		Err(error) => Err(DelegatedHostError::Executor(error)),
		Ok(_) if journal_failure.is_some() => {
			Err(DelegatedHostError::Session(journal_failure.expect("checked is_some")))
		},
		Ok(report) => teardown
			.map(|()| report)
			.map_err(DelegatedHostError::Executor),
	}
}

/// Journals one event into `session`.
fn journal_event(
	event: &DelegatedEvent,
	state: &mut JournalState,
	executor_id: &ExecutorId,
	session: &mut Session,
) -> Result<(), SessionError> {
	match event {
		DelegatedEvent::Lifecycle(LifecycleEvent::TurnStarted { .. }) => {
			state.ensure_turn(session)?;
		},

		DelegatedEvent::AgentMessage { text, .. } => {
			state.ensure_turn(session)?;
			let sid = state.ensure_stream(session, executor_id)?;
			session.stream_append(sid, text.as_str())?;
		},

		DelegatedEvent::AgentTextDelta { text, .. } => {
			state.ensure_turn(session)?;
			let sid = state.ensure_stream(session, executor_id)?;
			session.stream_append(sid, text.as_str())?;
		},

		DelegatedEvent::Usage { usage, .. } => {
			state.last_usage = *usage;
		},

		// ToolEvent and Permission have no existing session representation.
		// Gaps listed in the module-level doc table.
		DelegatedEvent::Tool(_) | DelegatedEvent::Permission(_) => {},

		// TurnCompleted and Failure stop-reasons drive close_assistant after
		// the drain.
		DelegatedEvent::Lifecycle(
			LifecycleEvent::TurnCompleted { .. } | LifecycleEvent::SessionClosed { .. },
		)
		| DelegatedEvent::Failure { .. } => {},
	}
	Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
	use std::{path::PathBuf, time::Duration};

	use omp_agent::delegated::{
		ApprovalPosture, Capabilities, CliVersion, DelegatedEvent, DelegatedPolicy, DelegatedPrompt,
		DenyAll, ExecutionMode, ExecutorError, ExecutorId, Feature, FeatureSet, LifecycleEvent,
		NetworkPosture, PromptRole, PromptSection, SandboxPosture, SessionRequest, TurnEnd, TurnId,
		TurnInput, TurnLimits, UnknownAuth, VersionRange,
		mock::{MockExecutor, Step},
	};
	use omp_con::{ConError, Ctx, Source};
	use omp_core::{Hash32, Str};
	use omp_session::{ComponentRegistry, Session};
	use tempfile::tempdir;
	use tokio_util::sync::CancellationToken;

	use super::*;

	// -----------------------------------------------------------------------
	// Helpers
	// -----------------------------------------------------------------------

	fn mock_caps() -> Capabilities {
		let features = FeatureSet::EMPTY
			.with(Feature::WorkingDirectoryInheritance)
			.with(Feature::StreamingText)
			.with(Feature::UsageReporting);
		Capabilities::new(
			ExecutorId::new_static("mock"),
			VersionRange { min_inclusive: CliVersion::new(1, 0, 0), max_exclusive: None },
			features,
			vec![DelegatedPolicy::STRICTEST],
		)
	}

	fn any_limits() -> TurnLimits {
		TurnLimits {
			timeout:         Duration::from_secs(30),
			idle:            Duration::from_secs(10),
			interrupt_grace: Duration::from_secs(2),
		}
	}

	fn simple_input() -> TurnInput {
		let bands = [1_u8, 2, 3, 4].map(|b| Hash32::new([b; 32]));
		TurnInput {
			prompt: DelegatedPrompt::from_sections(
				vec![PromptSection { role: PromptRole::User, text: Str::new_static("do the thing") }],
				bands,
			),
		}
	}

	fn workdir() -> PathBuf {
		std::env::temp_dir()
	}

	fn session_in(dir: &tempfile::TempDir) -> Session {
		let path = dir.path().join("session.oms");
		Session::create(&path, ComponentRegistry::default()).expect("session created")
	}

	fn request(workdir: PathBuf) -> SessionRequest {
		SessionRequest {
			mode: ExecutionMode::CliManaged,
			workdir,
			policy: DelegatedPolicy::STRICTEST,
			resume: None,
			unknown_auth: UnknownAuth::Proceed,
		}
	}

	fn completed_turn(n: u32) -> Vec<Step> {
		let turn_id = TurnId::new(Str::new(format!("turn-{n}")));
		vec![
			Step::Event(DelegatedEvent::Lifecycle(LifecycleEvent::TurnStarted {
				turn: turn_id.clone(),
			})),
			Step::Event(DelegatedEvent::AgentMessage {
				turn: turn_id.clone(),
				text: Str::new_static("hello from delegate"),
			}),
			Step::Event(DelegatedEvent::Lifecycle(LifecycleEvent::TurnCompleted {
				turn:    turn_id,
				outcome: TurnEnd::Completed,
			})),
		]
	}

	// -----------------------------------------------------------------------
	// Profile table: project cfg cannot change executor profiles
	// -----------------------------------------------------------------------

	#[test]
	fn project_overlay_cannot_change_codex_program() {
		let ctx = Ctx::new();

		// Operator sets it fine.
		ctx.exec("sv_delegated_codex_program my-codex", Source::Console)
			.unwrap();
		assert_eq!(SV_DELEGATED_CODEX_PROGRAM.get(&ctx).as_str(), "my-codex");

		// Project overlay rejected.
		let err = ctx
			.exec(
				"sv_delegated_codex_program evil-binary",
				Source::Project(Str::new_static("config.cfg")),
			)
			.unwrap_err();
		assert!(
			matches!(err, ConError::ProjectVarDenied { .. }),
			"expected ProjectVarDenied, got {err:?}"
		);
		// Value must not have changed.
		assert_eq!(SV_DELEGATED_CODEX_PROGRAM.get(&ctx).as_str(), "my-codex");
	}

	// -----------------------------------------------------------------------
	// Registry
	// -----------------------------------------------------------------------

	#[test]
	fn registry_registers_codex() {
		let ctx = Ctx::new();
		let registry = ExecutorRegistry::from_ctx(&ctx);
		let codex_id = CodexExecutor::executor_id();
		assert!(registry.get(&codex_id).is_some(), "codex not found in registry");
	}

	#[test]
	fn registry_unknown_id_returns_none() {
		let ctx = Ctx::new();
		let registry = ExecutorRegistry::from_ctx(&ctx);
		let unknown = ExecutorId::new_static("does-not-exist");
		assert!(registry.get(&unknown).is_none());
	}

	// -----------------------------------------------------------------------
	// Host: mock executor end-to-end
	// -----------------------------------------------------------------------

	#[tokio::test]
	async fn mock_turn_reaches_journal() {
		let executor = MockExecutor::new(mock_caps());
		executor.push_turn(completed_turn(1));

		let dir = tempdir().unwrap();
		let mut session = session_in(&dir);
		let (tx, rx) = flume::unbounded();
		let cancel = CancellationToken::new();
		let mut resolver = DenyAll;

		let report = run_turn_on(
			&executor,
			&request(workdir()),
			&simple_input(),
			any_limits(),
			&mut resolver,
			&cancel,
			&tx,
			&mut session,
		)
		.await
		.expect("turn succeeded");

		assert_eq!(report.end, TurnEnd::Completed);

		// Events forwarded to the caller.
		let events: Vec<_> = rx.try_iter().collect();
		assert!(
			events
				.iter()
				.any(|e| matches!(e, DelegatedEvent::Lifecycle(LifecycleEvent::TurnCompleted { .. }))),
			"TurnCompleted not forwarded; events: {events:?}"
		);

		// Journal has entries (begin_turn, assistant_start, stream, assistant_end,
		// receipt).
		assert!(session.entry_count() > 0, "journal has no entries");
	}

	#[tokio::test]
	async fn events_reach_caller() {
		let executor = MockExecutor::new(mock_caps());
		executor.push_turn(completed_turn(1));

		let dir = tempdir().unwrap();
		let mut session = session_in(&dir);
		let (tx, rx) = flume::unbounded();
		let cancel = CancellationToken::new();
		let mut resolver = DenyAll;

		run_turn_on(
			&executor,
			&request(workdir()),
			&simple_input(),
			any_limits(),
			&mut resolver,
			&cancel,
			&tx,
			&mut session,
		)
		.await
		.unwrap();

		let events: Vec<_> = rx.try_iter().collect();
		assert!(
			events
				.iter()
				.any(|e| matches!(e, DelegatedEvent::AgentMessage { .. })),
			"AgentMessage not forwarded; events: {events:?}"
		);
	}

	#[tokio::test]
	async fn cancel_mid_turn_terminates() {
		use omp_agent::delegated::{
			AuthStatus, ProbeReport,
			mock::{InterruptScript, MockConfig, ProbeScript},
		};

		let config = MockConfig {
			probe: ProbeScript::Report(ProbeReport {
				version: CliVersion::new(1, 0, 0),
				auth:    AuthStatus::Authenticated,
			}),
			interrupt: InterruptScript::Cooperative,
			..MockConfig::default()
		};
		let executor = MockExecutor::new(mock_caps()).with_config(config);

		let turn_id = TurnId::new(Str::new_static("turn-1"));
		executor.push_turn(vec![
			Step::Event(DelegatedEvent::Lifecycle(LifecycleEvent::TurnStarted {
				turn: turn_id.clone(),
			})),
			Step::Sleep(Duration::from_secs(60)),
		]);

		let dir = tempdir().unwrap();
		let mut session = session_in(&dir);
		let (tx, _rx) = flume::unbounded();
		let cancel = CancellationToken::new();
		let mut resolver = DenyAll;

		let cancel_clone = cancel.clone();
		tokio::spawn(async move {
			tokio::time::sleep(Duration::from_millis(50)).await;
			cancel_clone.cancel();
		});

		let result = run_turn_on(
			&executor,
			&request(workdir()),
			&simple_input(),
			any_limits(),
			&mut resolver,
			&cancel,
			&tx,
			&mut session,
		)
		.await;

		assert!(
			matches!(result, Err(DelegatedHostError::Executor(ExecutorError::Cancelled { .. }))),
			"expected Cancelled, got {result:?}"
		);
	}

	#[tokio::test]
	async fn unmappable_policy_refused() {
		// Use a network-enabled policy; mock_caps() only lists STRICTEST.
		let loose_policy = DelegatedPolicy::new(
			SandboxPosture::WorkspaceWrite,
			NetworkPosture::Open,
			ApprovalPosture::AlwaysAsk,
		);

		let executor = MockExecutor::new(mock_caps());
		let dir = tempdir().unwrap();
		let mut session = session_in(&dir);
		let (tx, _rx) = flume::unbounded();
		let cancel = CancellationToken::new();
		let mut resolver = DenyAll;

		let bad_request = SessionRequest {
			mode:         ExecutionMode::CliManaged,
			workdir:      workdir(),
			policy:       loose_policy,
			resume:       None,
			unknown_auth: UnknownAuth::Proceed,
		};

		let result = run_turn_on(
			&executor,
			&bad_request,
			&simple_input(),
			any_limits(),
			&mut resolver,
			&cancel,
			&tx,
			&mut session,
		)
		.await;

		assert!(
			matches!(
				result,
				Err(DelegatedHostError::Executor(ExecutorError::PolicyUnmappable { .. }))
			),
			"expected PolicyUnmappable, got {result:?}"
		);
	}

	// -----------------------------------------------------------------------
	// Journaling behavior
	// -----------------------------------------------------------------------

	fn assistant_text(session: &Session) -> Option<String> {
		let dom = session.dom();
		let turn = dom.children(dom.body()).last().copied()?;
		let node = dom.children(turn).iter().rev().copied().find(|h| {
			dom.get(*h)
				.is_some_and(|n| n.tag == Tag::Known(KnownTag::Assistant))
		})?;
		match dom.get(node)?.prop(&PropId::Text.into())? {
			omp_dom::Value::Str(text) => Some(text.as_str().to_owned()),
			_ => None,
		}
	}

	#[tokio::test]
	async fn streamed_deltas_are_journaled_in_order_as_one_assistant_text() {
		let executor = MockExecutor::new(mock_caps());
		let turn_id = TurnId::new(Str::new_static("turn-1"));
		executor.push_turn(vec![
			Step::Event(DelegatedEvent::Lifecycle(LifecycleEvent::TurnStarted {
				turn: turn_id.clone(),
			})),
			Step::Event(DelegatedEvent::AgentTextDelta {
				turn: turn_id.clone(),
				text: Str::new_static("hel"),
			}),
			Step::Event(DelegatedEvent::AgentTextDelta {
				turn: turn_id.clone(),
				text: Str::new_static("lo "),
			}),
			Step::Event(DelegatedEvent::AgentTextDelta {
				turn: turn_id.clone(),
				text: Str::new_static("world"),
			}),
			Step::Event(DelegatedEvent::Lifecycle(LifecycleEvent::TurnCompleted {
				turn:    turn_id,
				outcome: TurnEnd::Completed,
			})),
		]);
		let dir = tempdir().unwrap();
		let mut session = session_in(&dir);
		let (tx, _rx) = flume::unbounded();
		let cancel = CancellationToken::new();
		let mut resolver = DenyAll;
		run_turn_on(
			&executor,
			&request(workdir()),
			&simple_input(),
			any_limits(),
			&mut resolver,
			&cancel,
			&tx,
			&mut session,
		)
		.await
		.expect("turn");
		assert_eq!(assistant_text(&session).as_deref(), Some("hello world"));
	}

	#[tokio::test]
	async fn a_crashed_turn_still_journals_what_was_streamed() {
		use omp_agent::delegated::ExitFact;
		let executor = MockExecutor::new(mock_caps());
		let turn_id = TurnId::new(Str::new_static("turn-1"));
		executor.push_turn(vec![
			Step::Event(DelegatedEvent::Lifecycle(LifecycleEvent::TurnStarted {
				turn: turn_id.clone(),
			})),
			Step::Event(DelegatedEvent::AgentTextDelta {
				turn: turn_id,
				text: Str::new_static("partial answer"),
			}),
			Step::Crash(ExitFact::Unknown),
		]);
		let dir = tempdir().unwrap();
		let mut session = session_in(&dir);
		let (tx, _rx) = flume::unbounded();
		let cancel = CancellationToken::new();
		let mut resolver = DenyAll;
		let result = run_turn_on(
			&executor,
			&request(workdir()),
			&simple_input(),
			any_limits(),
			&mut resolver,
			&cancel,
			&tx,
			&mut session,
		)
		.await;
		assert!(
			matches!(result, Err(DelegatedHostError::Executor(ExecutorError::Crashed { .. }))),
			"{result:?}"
		);
		assert_eq!(assistant_text(&session).as_deref(), Some("partial answer"));
	}

	#[tokio::test]
	async fn a_cancelled_turn_keeps_the_text_it_streamed_and_forwards_the_delta() {
		use omp_agent::delegated::{
			AuthStatus, ProbeReport,
			mock::{InterruptScript, MockConfig, ProbeScript},
		};
		let config = MockConfig {
			probe: ProbeScript::Report(ProbeReport {
				version: CliVersion::new(1, 0, 0),
				auth:    AuthStatus::Authenticated,
			}),
			interrupt: InterruptScript::Cooperative,
			..MockConfig::default()
		};
		let executor = MockExecutor::new(mock_caps()).with_config(config);
		let turn_id = TurnId::new(Str::new_static("turn-1"));
		executor.push_turn(vec![
			Step::Event(DelegatedEvent::Lifecycle(LifecycleEvent::TurnStarted {
				turn: turn_id.clone(),
			})),
			Step::Event(DelegatedEvent::AgentTextDelta {
				turn: turn_id,
				text: Str::new_static("before cancel"),
			}),
			Step::Sleep(Duration::from_secs(60)),
		]);
		let dir = tempdir().unwrap();
		let mut session = session_in(&dir);
		let (tx, rx) = flume::unbounded();
		let cancel = CancellationToken::new();
		let mut resolver = DenyAll;
		let cancel_clone = cancel.clone();
		tokio::spawn(async move {
			tokio::time::sleep(Duration::from_millis(100)).await;
			cancel_clone.cancel();
		});
		let result = run_turn_on(
			&executor,
			&request(workdir()),
			&simple_input(),
			any_limits(),
			&mut resolver,
			&cancel,
			&tx,
			&mut session,
		)
		.await;
		assert!(
			matches!(result, Err(DelegatedHostError::Executor(ExecutorError::Cancelled { .. }))),
			"{result:?}"
		);
		assert_eq!(assistant_text(&session).as_deref(), Some("before cancel"));
		assert!(
			rx.try_iter()
				.any(|e| matches!(e, DelegatedEvent::AgentTextDelta { .. }))
		);
	}

	#[tokio::test]
	async fn streamed_text_reaches_session_subscribers_while_the_cli_is_still_running() {
		use omp_agent::delegated::{
			AuthStatus, ProbeReport,
			mock::{InterruptScript, MockConfig, ProbeScript},
		};
		let config = MockConfig {
			probe: ProbeScript::Report(ProbeReport {
				version: CliVersion::new(1, 0, 0),
				auth:    AuthStatus::Authenticated,
			}),
			interrupt: InterruptScript::Cooperative,
			..MockConfig::default()
		};
		let executor = MockExecutor::new(mock_caps()).with_config(config);
		let turn_id = TurnId::new(Str::new_static("turn-1"));
		// The CLI streams one delta and then sleeps far longer than the test waits.
		executor.push_turn(vec![
			Step::Event(DelegatedEvent::Lifecycle(LifecycleEvent::TurnStarted {
				turn: turn_id.clone(),
			})),
			Step::Event(DelegatedEvent::AgentTextDelta {
				turn: turn_id,
				text: Str::new_static("already streamed"),
			}),
			Step::Sleep(Duration::from_secs(30)),
		]);
		let dir = tempdir().unwrap();
		let mut session = session_in(&dir);
		let (_snapshot, events) = session.subscribe();
		let (tx, _rx) = flume::unbounded();
		let cancel = CancellationToken::new();
		let mut resolver = DenyAll;

		// A subscriber, like the TUI actor, must see the delta before the turn ends.
		let watcher = {
			let cancel = cancel.clone();
			tokio::spawn(async move {
				let seen = tokio::time::timeout(Duration::from_secs(5), async {
					while let Ok(event) = events.recv_async().await {
						if let omp_dom::Event::Stream { text: Some(text), .. } = event {
							return Some(text.as_str().to_owned());
						}
					}
					None
				})
				.await
				.ok()
				.flatten();
				// Only now end the still-running turn.
				cancel.cancel();
				seen
			})
		};
		let result = run_turn_on(
			&executor,
			&request(workdir()),
			&simple_input(),
			any_limits(),
			&mut resolver,
			&cancel,
			&tx,
			&mut session,
		)
		.await;
		assert!(
			matches!(result, Err(DelegatedHostError::Executor(ExecutorError::Cancelled { .. }))),
			"{result:?}"
		);
		let seen = watcher.await.expect("watcher task");
		assert_eq!(
			seen.as_deref(),
			Some("already streamed"),
			"no delta reached subscribers before the turn ended"
		);
	}
}
