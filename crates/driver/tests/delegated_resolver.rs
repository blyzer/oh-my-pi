//! Contract tests for [`RoutePermissionResolver`].
//!
//! Every test drives a real [`ApprovalRoute`] (the production channel) and
//! a real [`MockExecutor`] (from `omp_agent::delegated::mock`). No mock of
//! the resolver itself is involved; the route IS the resolver under test.
//!
//! All timing uses paused tokio time so timeouts and grace periods are exact
//! and instantaneous.

use std::{sync::Arc, time::Duration};

use omp_agent::{
	ApprovalBook, ApprovalDecision, ApprovalRoute, ApprovalScope, ApprovalSource,
	delegated::{
		ActiveSession, ApprovalPosture, AuthStatus, Capabilities, CliSessionId, CliVersion,
		DelegatedEvent, DelegatedPolicy, DelegatedPrompt, DenyAll, ExecutionMode, ExecutorError,
		Feature, FeatureSet, LifecycleEvent, NetworkPosture, PermissionAnswer, PermissionKind,
		PermissionRequest, PermissionTally, ProbeReport, PromptId, PromptRole, PromptSection,
		SandboxPosture, SessionRequest, TurnEnd, TurnId, TurnInput, TurnLimits, TurnReport,
		UnknownAuth, VersionRange,
		mock::{MockConfig, MockExecutor, MockSession, Step},
		start_session,
	},
};
use omp_core::{Hash32, Str, sf};
use omp_driver::delegated_resolver::RoutePermissionResolver;
use tokio_util::sync::CancellationToken;

// ---------------------------------------------------------------------------
// Shared fixtures (mirrors delegated.rs)
// ---------------------------------------------------------------------------

type Session = ActiveSession<MockSession>;

const WORKSPACE_WRITE: DelegatedPolicy = DelegatedPolicy::new(
	SandboxPosture::WorkspaceWrite,
	NetworkPosture::Scoped,
	ApprovalPosture::Write,
);

fn caps() -> Capabilities {
	Capabilities::new(
		omp_agent::delegated::ExecutorId::new_static("mock-cli"),
		VersionRange {
			min_inclusive: CliVersion::new(1, 0, 0),
			max_exclusive: Some(CliVersion::new(2, 0, 0)),
		},
		FeatureSet::FULL,
		[WORKSPACE_WRITE, DelegatedPolicy::STRICTEST],
	)
}

fn executor() -> MockExecutor {
	MockExecutor::new(caps())
}

fn workdir() -> std::path::PathBuf {
	std::path::PathBuf::from(if cfg!(windows) {
		r"C:\work\project"
	} else {
		"/work/project"
	})
}

fn request() -> SessionRequest {
	SessionRequest {
		mode:         ExecutionMode::CliManaged,
		workdir:      workdir(),
		policy:       WORKSPACE_WRITE,
		resume:       None,
		unknown_auth: UnknownAuth::Refuse,
	}
}

fn bands() -> [Hash32; 4] {
	[1_u8, 2, 3, 4].map(|byte| Hash32::new([byte; 32]))
}

fn prompt() -> DelegatedPrompt {
	DelegatedPrompt::from_sections(
		vec![
			PromptSection { role: PromptRole::System, text: Str::new_static("system") },
			PromptSection { role: PromptRole::User, text: Str::new_static("do the thing") },
		],
		bands(),
	)
}

fn input() -> TurnInput {
	TurnInput { prompt: prompt() }
}

fn turn(n: u32) -> TurnId {
	TurnId::new(format!("turn-{n}"))
}

fn started(n: u32) -> Step {
	Step::Event(DelegatedEvent::Lifecycle(LifecycleEvent::TurnStarted { turn: turn(n) }))
}

fn completed(n: u32) -> Step {
	Step::Event(DelegatedEvent::Lifecycle(LifecycleEvent::TurnCompleted {
		turn:    turn(n),
		outcome: TurnEnd::Completed,
	}))
}

fn exec_request() -> PermissionRequest {
	PermissionRequest {
		prompt:        PromptId::new_static("p1"),
		turn:          turn(1),
		call:          Some(omp_agent::delegated::CallId::new_static("call-1")),
		kind:          PermissionKind::Exec,
		subject:       Str::new_static("cargo build"),
		session_grant: false,
	}
}

const fn limits() -> TurnLimits {
	TurnLimits {
		timeout:         Duration::from_secs(10),
		idle:            Duration::from_secs(2),
		interrupt_grace: Duration::from_millis(500),
	}
}

async fn open(exec: &MockExecutor) -> Session {
	start_session(exec, &request())
		.await
		.expect("session opens")
}

/// Runs one turn with `resolver`, returns the result and all forwarded events.
async fn run_with<R: omp_agent::delegated::PermissionResolver>(
	session: &mut Session,
	resolver: &mut R,
	cancel: &CancellationToken,
) -> (Result<TurnReport, ExecutorError>, Vec<DelegatedEvent>) {
	let (tx, rx) = flume::unbounded();
	let result = session
		.run_turn(&input(), limits(), cancel, resolver, &tx)
		.await;
	drop(tx);
	(result, rx.drain().collect())
}

/// Builds a route + inbox pair from a fresh book (no session journaling needed
/// in these tests; the route's standalone path is what matters here).
fn route() -> (ApprovalRoute, omp_agent::ApprovalInbox) {
	ApprovalRoute::new(Arc::new(ApprovalBook::new()), None)
}

fn approve_once() -> ApprovalDecision {
	ApprovalDecision {
		approved:   true,
		scope:      ApprovalScope::Once,
		source:     ApprovalSource::User,
		decided_by: None,
		reason:     None,
		audited:    false,
	}
}

fn deny() -> ApprovalDecision {
	ApprovalDecision {
		approved:   false,
		scope:      ApprovalScope::Once,
		source:     ApprovalSource::User,
		decided_by: None,
		reason:     Some(Str::new_static("denied by test")),
		audited:    false,
	}
}

// ---------------------------------------------------------------------------
// Test: approve through the real route
// ---------------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn approve_through_route_allows_once() {
	let exec = executor();
	let handle = exec.handle();
	exec.push_turn([
		started(1),
		Step::Event(DelegatedEvent::Permission(exec_request())),
		completed(1),
	]);
	let mut session = open(&exec).await;

	let (approval_route, inbox) = route();
	let cancel = CancellationToken::new();
	let mut resolver = RoutePermissionResolver::new(approval_route, cancel.clone());

	// Approve from the "host actor" side while the resolver waits.
	let driver = async {
		let request = inbox.recv().await.expect("ticket arrives");
		request.respond(approve_once()).expect("reply sent");
	};
	let (result, _events) = tokio::join!(run_with(&mut session, &mut resolver, &cancel), driver,);
	let (result, _) = result;
	let report = result.expect("turn completes");

	// The CLI received AllowOnce (session_grant is false on exec_request, so
	// even a Session-scoped answer would become AllowOnce; Once maps directly).
	let answers = handle.snapshot().answers;
	assert_eq!(answers.len(), 1);
	assert_eq!(answers[0].1, PermissionAnswer::AllowOnce);

	// Tally: one asked, one allowed.
	assert_eq!(report.permissions, PermissionTally {
		asked:             1,
		allowed:           1,
		denied:            0,
		refused_by_policy: 0,
	});
}

// ---------------------------------------------------------------------------
// Test: deny through the real route
// ---------------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn deny_through_route_denies_the_cli() {
	let exec = executor();
	let handle = exec.handle();
	exec.push_turn([
		started(1),
		Step::Event(DelegatedEvent::Permission(exec_request())),
		completed(1),
	]);
	let mut session = open(&exec).await;

	let (approval_route, inbox) = route();
	let cancel = CancellationToken::new();
	let mut resolver = RoutePermissionResolver::new(approval_route, cancel.clone());

	let driver = async {
		let request = inbox.recv().await.expect("ticket arrives");
		request.respond(deny()).expect("reply sent");
	};
	let (result, _events) = tokio::join!(run_with(&mut session, &mut resolver, &cancel), driver,);
	let (result, _) = result;
	result.expect("turn completes even on denial");

	let answers = handle.snapshot().answers;
	assert_eq!(answers.len(), 1);
	assert_eq!(answers[0].1, PermissionAnswer::Deny);
}

// ---------------------------------------------------------------------------
// Test: denied request never reaches the agent as allowed
// ---------------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn denied_request_never_reaches_agent() {
	let exec = executor();
	let handle = exec.handle();
	exec.push_turn([
		started(1),
		Step::Event(DelegatedEvent::Permission(exec_request())),
		completed(1),
	]);
	let mut session = open(&exec).await;

	let (approval_route, inbox) = route();
	let cancel = CancellationToken::new();
	let mut resolver = RoutePermissionResolver::new(approval_route, cancel.clone());

	let driver = async {
		let request = inbox.recv().await.expect("ticket arrives");
		request.respond(deny()).ok();
	};
	let (result, _) = tokio::join!(run_with(&mut session, &mut resolver, &cancel), driver,);
	let (result, _) = result;
	result.expect("turn completes");

	let snap = handle.snapshot();
	// The CLI was told Deny, never AllowOnce or AllowForSession.
	assert!(
		snap
			.answers
			.iter()
			.all(|(_, answer)| *answer == PermissionAnswer::Deny),
		"a denied request must only ever reach the CLI as Deny, got: {:?}",
		snap.answers,
	);
}

// ---------------------------------------------------------------------------
// Test: cancel mid-wait denies
// ---------------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn cancel_mid_wait_denies_and_session_survives() {
	let exec = executor();
	let handle = exec.handle();
	exec.push_turn([
		started(1),
		Step::Event(DelegatedEvent::Permission(exec_request())),
		// No completed event: the CLI is interrupted before it can finish.
	]);
	let mut session = open(&exec).await;

	let (approval_route, _inbox) = route();
	// _inbox kept alive so the route can deliver (but nobody answers it).
	let cancel = CancellationToken::new();
	let mut resolver = RoutePermissionResolver::new(approval_route, cancel.clone());

	// Cancel the turn while the resolver is waiting.
	let trigger = cancel.clone();
	let canceller = async move {
		tokio::time::sleep(Duration::from_millis(10)).await;
		trigger.cancel();
	};
	let ((result, _events), ()) =
		tokio::join!(run_with(&mut session, &mut resolver, &cancel), canceller,);
	assert!(
		matches!(result, Err(ExecutorError::Cancelled { .. })),
		"cancellation must produce Cancelled, got: {result:?}",
	);
	// The session survives (cooperative interrupt was answered by the mock).
	assert!(session.is_open());
}

// ---------------------------------------------------------------------------
// Test: route dropped (inbox half dropped) denies
// ---------------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn route_dropped_denies_the_request() {
	let exec = executor();
	let handle = exec.handle();
	exec.push_turn([
		started(1),
		Step::Event(DelegatedEvent::Permission(exec_request())),
		completed(1),
	]);
	let mut session = open(&exec).await;

	let (approval_route, inbox) = route();
	let cancel = CancellationToken::new();
	let mut resolver = RoutePermissionResolver::new(approval_route, cancel.clone());

	// Drop the inbox immediately so no host can answer.
	drop(inbox);

	let (result, _) = run_with(&mut session, &mut resolver, &cancel).await;
	result.expect("turn still completes");

	let answers = handle.snapshot().answers;
	assert_eq!(answers.len(), 1);
	assert_eq!(answers[0].1, PermissionAnswer::Deny, "dropped route must produce Deny",);
}

// ---------------------------------------------------------------------------
// Test: unattended session — inbox dropped mid-wait denies
// ---------------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn unattended_session_denies_the_request() {
	// "Unattended" means the host inbox is open when the route delivers the
	// ticket but nobody answers it and the inbox is dropped (host disconnects).
	// The reply sender becomes disconnected and request_cancellable returns
	// unreachable_decision, which denies because `unreachable` is "deny".
	let exec = executor();
	let handle = exec.handle();
	exec.push_turn([
		started(1),
		Step::Event(DelegatedEvent::Permission(exec_request())),
		completed(1),
	]);
	let mut session = open(&exec).await;

	let (approval_route, inbox) = route();
	let cancel = CancellationToken::new();
	let mut resolver = RoutePermissionResolver::new(approval_route, cancel.clone());

	// The host receives the ticket but never answers, then disconnects.
	let driver = async move {
		let _ticket = inbox.recv().await.expect("ticket arrives");
		// Drop inbox + _ticket without responding: reply sender disconnects.
	};
	let (result, _) = tokio::join!(run_with(&mut session, &mut resolver, &cancel), driver,);
	let (result, _) = result;
	result.expect("turn completes");

	let snap = handle.snapshot();
	assert_eq!(snap.answers.len(), 1);
	assert_eq!(snap.answers[0].1, PermissionAnswer::Deny, "unattended session must deny",);
}

// ---------------------------------------------------------------------------
// Test: session_grant=false means AllowOnce even for a Session-scope approval
// ---------------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn session_scope_approval_becomes_allow_once_because_no_session_grant() {
	let exec = executor();
	let handle = exec.handle();
	exec.push_turn([
		started(1),
		Step::Event(DelegatedEvent::Permission(exec_request())),
		completed(1),
	]);
	let mut session = open(&exec).await;

	let (approval_route, inbox) = route();
	let cancel = CancellationToken::new();
	let mut resolver = RoutePermissionResolver::new(approval_route, cancel.clone());

	// Approve with a Session scope — `map_decision` must narrow this to AllowOnce
	// because exec_request() has session_grant: false.
	let session_approval = ApprovalDecision {
		approved:   true,
		scope:      ApprovalScope::Session,
		source:     ApprovalSource::User,
		decided_by: None,
		reason:     None,
		audited:    false,
	};
	let driver = async {
		let request = inbox.recv().await.expect("ticket arrives");
		request.respond(session_approval).expect("reply sent");
	};
	let (result, _) = tokio::join!(run_with(&mut session, &mut resolver, &cancel), driver,);
	let (result, _) = result;
	result.expect("turn completes");

	let answers = handle.snapshot().answers;
	assert_eq!(answers.len(), 1);
	assert_eq!(
		answers[0].1,
		PermissionAnswer::AllowOnce,
		"Session-scope approval must be narrowed to AllowOnce when session_grant=false",
	);
}

// ---------------------------------------------------------------------------
// Test: wait time excluded from turn timeout
// ---------------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn wait_time_at_approval_desk_excluded_from_turn_timeout() {
	// Turn timeout is 100 ms but the "host" answers after 3600 s; the
	// paused clock means this test is instantaneous in wall time.
	let exec = executor();
	exec.push_turn([
		started(1),
		Step::Event(DelegatedEvent::Permission(exec_request())),
		completed(1),
	]);
	let mut session = open(&exec).await;

	let (approval_route, inbox) = route();
	let cancel = CancellationToken::new();
	let mut resolver = RoutePermissionResolver::new(approval_route, cancel.clone());

	// Very tight limits; the approval takes 3600 seconds.
	let tight = TurnLimits {
		timeout:         Duration::from_millis(100),
		idle:            Duration::from_millis(100),
		interrupt_grace: Duration::from_millis(50),
	};

	let driver = async {
		// Simulated human takes "3600 s" (paused clock, so instant in real time).
		tokio::time::sleep(Duration::from_secs(3600)).await;
		let request = inbox.recv().await.expect("ticket arrives");
		request.respond(approve_once()).expect("reply sent");
	};
	let (tx, rx) = flume::unbounded();
	let turn_fut = session.run_turn(&input(), tight, &cancel, &mut resolver, &tx);
	let (result, ()) = tokio::join!(turn_fut, driver);
	drop(tx);
	assert!(
		result.is_ok(),
		"an hour at the approval desk must not count against the turn timeout: {result:?}",
	);
}
