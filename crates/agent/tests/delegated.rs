//! Contract-level tests of the delegated-executor boundary, run against the
//! deterministic in-memory mock. No real CLI, process or credential is
//! involved. All timing uses paused tokio time, so hangs, timeouts and grace
//! periods are exact and instantaneous.

use std::{
	error::Error as _,
	path::PathBuf,
	sync::{
		Arc,
		atomic::{AtomicU32, Ordering},
	},
	time::Duration,
};

use omp_agent::{
	ApprovalDecision, ApprovalScope, ApprovalSource,
	delegated::{
		ActiveSession, ApprovalPosture, AuthStatus, CallId, Capabilities, CliSessionId, CliVersion,
		DelegatedEvent, DelegatedPolicy, DelegatedPrompt, DenyAll, DenyReason, ExecutionMode,
		ExecutorError, ExecutorId, ExitFact, Feature, FeatureSet, FrameFault, LifecycleEvent,
		NetworkPosture, PermissionAnswer, PermissionKind, PermissionRequest, PermissionResolver,
		PermissionTally, ProbeReport, PromptConversionError, PromptId, PromptRole, PromptSection,
		ProtocolViolation, RemoteFailure, SandboxPosture, SessionRequest, ToolEvent, ToolKind,
		ToolOutcome, TurnEnd, TurnId, TurnInput, TurnLimits, TurnReport, UnknownAuth, VersionRange,
		admit_request, map_decision,
		mock::{
			InterruptScript, MockConfig, MockExecutor, MockSession, ProbeScript, ResumeScript, Step,
		},
		start_session,
	},
};
use omp_ai::Usage;
use omp_core::{Hash32, Str};
use omp_proto::thread::v1::{self as thread, Item, item, part};
use tokio_util::sync::CancellationToken;

type Session = ActiveSession<MockSession>;

const WORKSPACE_WRITE: DelegatedPolicy = DelegatedPolicy::new(
	SandboxPosture::WorkspaceWrite,
	NetworkPosture::Scoped,
	ApprovalPosture::Write,
);

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

fn caps(features: FeatureSet) -> Capabilities {
	Capabilities::new(
		ExecutorId::new_static("mock-cli"),
		VersionRange {
			min_inclusive: CliVersion::new(1, 0, 0),
			max_exclusive: Some(CliVersion::new(2, 0, 0)),
		},
		features,
		[WORKSPACE_WRITE, DelegatedPolicy::STRICTEST],
	)
}

fn executor() -> MockExecutor {
	MockExecutor::new(caps(FeatureSet::FULL))
}

fn workdir() -> PathBuf {
	PathBuf::from(if cfg!(windows) {
		r"C:\work\project"
	} else {
		"/work/project"
	})
}

fn request_with(policy: DelegatedPolicy) -> SessionRequest {
	SessionRequest {
		mode: ExecutionMode::CliManaged,
		workdir: workdir(),
		policy,
		resume: None,
		unknown_auth: UnknownAuth::Refuse,
	}
}

fn request() -> SessionRequest {
	request_with(WORKSPACE_WRITE)
}

fn bands() -> [Hash32; 4] {
	[1_u8, 2, 3, 4].map(|byte| Hash32::new([byte; 32]))
}

/// A prompt whose bytes would not survive a casual rewrite: non-ASCII text,
/// significant trailing whitespace, a blank line, and a volatile section.
fn prompt() -> DelegatedPrompt {
	DelegatedPrompt::from_sections(
		vec![
			PromptSection {
				role: PromptRole::System,
				text: Str::new("Eres el agente de OMP — sistema ✓\n\n  indentado  \n"),
			},
			PromptSection {
				role: PromptRole::System,
				text: Str::new("<rules>\n\tuse tabs\n</rules>"),
			},
			PromptSection { role: PromptRole::User, text: Str::new("status: dirty\r\n") },
			PromptSection { role: PromptRole::User, text: Str::new("fix the bug in `src/lib.rs`") },
		],
		bands(),
	)
}

fn input() -> TurnInput {
	TurnInput { prompt: prompt() }
}

const fn limits() -> TurnLimits {
	TurnLimits {
		timeout:         Duration::from_secs(10),
		idle:            Duration::from_secs(2),
		interrupt_grace: Duration::from_millis(500),
	}
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

fn delta(n: u32, text: &str) -> Step {
	Step::Event(DelegatedEvent::AgentTextDelta { turn: turn(n), text: Str::new(text) })
}

fn permission_request(kind: PermissionKind, session_grant: bool) -> PermissionRequest {
	PermissionRequest {
		prompt: PromptId::new_static("prompt-1"),
		turn: turn(1),
		call: Some(CallId::new_static("call-1")),
		kind,
		subject: Str::new_static("rm -rf build"),
		session_grant,
	}
}

const fn decision(
	approved: bool,
	scope: ApprovalScope,
	source: ApprovalSource,
	audited: bool,
) -> ApprovalDecision {
	ApprovalDecision { approved, scope, source, decided_by: None, reason: None, audited }
}

fn user_yes(scope: ApprovalScope) -> ApprovalDecision {
	decision(true, scope, ApprovalSource::User, false)
}

fn fail<T>(result: Result<T, ExecutorError>) -> ExecutorError {
	match result {
		Ok(_) => panic!("expected an error, got success"),
		Err(error) => error,
	}
}

async fn open(exec: &MockExecutor) -> Session {
	start_session(exec, &request())
		.await
		.expect("session opens")
}

async fn run_with<R: PermissionResolver>(
	session: &mut Session,
	limits: TurnLimits,
	cancel: &CancellationToken,
	resolver: &mut R,
) -> (Result<TurnReport, ExecutorError>, Vec<DelegatedEvent>) {
	let (tx, rx) = flume::unbounded();
	let result = session
		.run_turn(&input(), limits, cancel, resolver, &tx)
		.await;
	drop(tx);
	(result, rx.drain().collect())
}

async fn run(session: &mut Session) -> (Result<TurnReport, ExecutorError>, Vec<DelegatedEvent>) {
	run_with(session, limits(), &CancellationToken::new(), &mut DenyAll).await
}

/// Resolver with a fixed answer and a configurable thinking time.
struct Scripted {
	decision: ApprovalDecision,
	delay:    Duration,
	calls:    Arc<AtomicU32>,
}

impl Scripted {
	fn new(decision: ApprovalDecision) -> (Self, Arc<AtomicU32>) {
		let calls = Arc::new(AtomicU32::new(0));
		(Self { decision, delay: Duration::ZERO, calls: Arc::clone(&calls) }, calls)
	}
}

impl PermissionResolver for Scripted {
	fn resolve<'a>(
		&'a mut self,
		_request: &'a PermissionRequest,
	) -> impl Future<Output = ApprovalDecision> + Send + 'a {
		self.calls.fetch_add(1, Ordering::SeqCst);
		let decision = self.decision.clone();
		let delay = self.delay;
		async move {
			tokio::time::sleep(delay).await;
			decision
		}
	}
}

/// Runs one turn that raises a single permission prompt and reports what the
/// CLI was told, how often the resolver was consulted, and the tally.
async fn ask(
	policy: DelegatedPolicy,
	kind: PermissionKind,
	session_grant: bool,
	decision: ApprovalDecision,
) -> (PermissionAnswer, u32, PermissionTally) {
	let exec = executor();
	let handle = exec.handle();
	exec.push_turn([
		started(1),
		Step::Event(DelegatedEvent::Permission(permission_request(kind, session_grant))),
		completed(1),
	]);
	let mut session = start_session(&exec, &request_with(policy))
		.await
		.expect("session opens");
	let (mut resolver, calls) = Scripted::new(decision);
	let (result, _) =
		run_with(&mut session, limits(), &CancellationToken::new(), &mut resolver).await;
	let report = result.expect("turn completes");
	let answers = handle.snapshot().answers;
	assert_eq!(answers.len(), 1, "every prompt gets exactly one answer");
	(answers[0].1, calls.load(Ordering::SeqCst), report.permissions)
}

// ---------------------------------------------------------------------------
// Negotiation: executable, version, auth, mode, policy
// ---------------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn missing_executable_is_reported_and_nothing_is_opened() {
	let exec =
		executor().with_config(MockConfig { probe: ProbeScript::Missing, ..Default::default() });
	let handle = exec.handle();
	let error = fail(start_session(&exec, &request()).await);
	assert!(
		matches!(error, ExecutorError::ExecutableMissing { ref executor } if executor.as_str() == "mock-cli")
	);
	assert!(handle.snapshot().opens.is_empty());
}

#[tokio::test(start_paused = true)]
async fn unsupported_version_is_refused_at_both_bounds() {
	for (version, supported) in [
		(CliVersion::new(0, 9, 9), false),
		(CliVersion::new(1, 0, 0), true),
		(CliVersion::new(1, 99, 99), true),
		(CliVersion::new(2, 0, 0), false),
		(CliVersion::new(3, 1, 0), false),
	] {
		let exec = executor().with_config(MockConfig {
			probe: ProbeScript::Report(ProbeReport { version, auth: AuthStatus::Authenticated }),
			..Default::default()
		});
		let handle = exec.handle();
		let outcome = start_session(&exec, &request()).await;
		if supported {
			assert!(outcome.is_ok(), "{version:?} should be accepted");
		} else {
			let error = fail(outcome);
			assert!(
				matches!(error, ExecutorError::UnsupportedVersion { found, .. } if found == version),
				"{version:?} should be refused, got {error:?}"
			);
			assert!(handle.snapshot().opens.is_empty(), "no process for an unsupported CLI");
		}
	}
}

#[tokio::test(start_paused = true)]
async fn unknown_auth_is_refused_unless_the_request_opts_in() {
	let config = MockConfig {
		probe: ProbeScript::Report(ProbeReport {
			version: CliVersion::new(1, 2, 3),
			auth:    AuthStatus::Unknown,
		}),
		..Default::default()
	};
	let exec = executor().with_config(config.clone());
	let handle = exec.handle();
	let error = fail(start_session(&exec, &request()).await);
	assert!(matches!(error, ExecutorError::AuthUnknown { .. }));
	assert!(handle.snapshot().opens.is_empty());

	let exec = executor().with_config(config);
	let mut permissive = request();
	permissive.unknown_auth = UnknownAuth::Proceed;
	assert!(start_session(&exec, &permissive).await.is_ok());
}

#[tokio::test(start_paused = true)]
async fn auth_required_is_refused_even_with_unknown_auth_consent() {
	let exec = executor().with_config(MockConfig {
		probe: ProbeScript::Report(ProbeReport {
			version: CliVersion::new(1, 2, 3),
			auth:    AuthStatus::Required,
		}),
		..Default::default()
	});
	let handle = exec.handle();
	let mut permissive = request();
	permissive.unknown_auth = UnknownAuth::Proceed;
	let error = fail(start_session(&exec, &permissive).await);
	assert!(matches!(error, ExecutorError::AuthRequired { .. }));
	assert!(handle.snapshot().opens.is_empty());
}

#[tokio::test(start_paused = true)]
async fn auth_required_mid_turn_is_a_typed_failure_that_keeps_the_session() {
	let exec = executor();
	let handle = exec.handle();
	exec.push_turn([
		started(1),
		Step::Event(DelegatedEvent::Failure {
			turn:    turn(1),
			failure: RemoteFailure::AuthRequired,
		}),
	]);
	let mut session = open(&exec).await;
	let (result, _) = run(&mut session).await;
	assert!(matches!(fail(result), ExecutorError::AuthRequired { .. }));
	assert!(session.is_open());
	assert_eq!(handle.snapshot().terminations, 0);
}

#[tokio::test(start_paused = true)]
async fn direct_api_mode_is_rejected_and_never_rerouted() {
	let exec = executor();
	let handle = exec.handle();
	let mut direct = request();
	direct.mode = ExecutionMode::DirectApi;
	let error = fail(start_session(&exec, &direct).await);
	assert!(matches!(error, ExecutorError::ModeUnsupported {
		requested: ExecutionMode::DirectApi,
		offered: ExecutionMode::CliManaged,
		..
	}));
	let record = handle.snapshot();
	assert_eq!(record.probes, 0, "a rejected mode must not touch the CLI at all");
	assert!(record.opens.is_empty());
}

#[tokio::test(start_paused = true)]
async fn unlisted_policy_is_refused_with_no_closest_match_fallback() {
	for requested in [
		// looser than anything listed
		DelegatedPolicy::new(
			SandboxPosture::Unrestricted,
			NetworkPosture::Open,
			ApprovalPosture::Yolo,
		),
		// stricter than a listed policy on one axis, but not itself listed
		DelegatedPolicy::new(
			SandboxPosture::WorkspaceWrite,
			NetworkPosture::Scoped,
			ApprovalPosture::AlwaysAsk,
		),
	] {
		let exec = executor();
		let handle = exec.handle();
		let error = fail(start_session(&exec, &request_with(requested)).await);
		assert!(
			matches!(error, ExecutorError::PolicyUnmappable { requested: got, .. } if got == requested),
			"{requested:?} must be refused"
		);
		let record = handle.snapshot();
		assert_eq!(record.probes, 0, "policy is negotiated before the CLI is touched");
		assert!(record.opens.is_empty());
	}
}

#[tokio::test(start_paused = true)]
async fn policy_drift_after_open_tears_the_session_down() {
	let loose = DelegatedPolicy::new(
		SandboxPosture::Unrestricted,
		NetworkPosture::Open,
		ApprovalPosture::Yolo,
	);
	let exec =
		executor().with_config(MockConfig { applied_policy: Some(loose), ..Default::default() });
	let handle = exec.handle();
	let error = fail(start_session(&exec, &request()).await);
	assert!(matches!(
		error,
		ExecutorError::PolicyDrift { requested, applied, .. }
			if requested == WORKSPACE_WRITE && applied == loose
	));
	assert_eq!(handle.snapshot().terminations, 1);
}

// ---------------------------------------------------------------------------
// Working directory
// ---------------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn working_directory_is_passed_through_and_reported_back() {
	let exec = executor();
	let handle = exec.handle();
	let session = open(&exec).await;
	assert_eq!(session.info().workdir, workdir());
	assert_eq!(handle.snapshot().opens[0].workdir, workdir());
}

#[tokio::test(start_paused = true)]
async fn relative_working_directory_is_refused_before_anything_starts() {
	let exec = executor();
	let handle = exec.handle();
	let mut relative = request();
	relative.workdir = PathBuf::from("project/../..");
	let error = fail(start_session(&exec, &relative).await);
	assert!(
		matches!(error, ExecutorError::WorkdirNotAbsolute { ref path } if *path == relative.workdir)
	);
	assert_eq!(handle.snapshot().probes, 0);
}

#[tokio::test(start_paused = true)]
async fn reported_working_directory_mismatch_tears_the_session_down() {
	let elsewhere = if cfg!(windows) {
		PathBuf::from(r"C:\elsewhere")
	} else {
		PathBuf::from("/elsewhere")
	};
	let exec = executor()
		.with_config(MockConfig { reported_workdir: Some(elsewhere.clone()), ..Default::default() });
	let handle = exec.handle();
	let error = fail(start_session(&exec, &request()).await);
	assert!(matches!(
		error,
		ExecutorError::WorkdirMismatch { ref requested, ref reported, .. }
			if *requested == workdir() && *reported == elsewhere
	));
	assert_eq!(handle.snapshot().terminations, 1);
}

#[tokio::test(start_paused = true)]
async fn executor_without_working_directory_inheritance_is_refused() {
	let exec =
		MockExecutor::new(caps(FeatureSet::FULL.without(Feature::WorkingDirectoryInheritance)));
	let error = fail(start_session(&exec, &request()).await);
	assert!(matches!(error, ExecutorError::FeatureUnavailable {
		feature: Feature::WorkingDirectoryInheritance,
		..
	}));
}

// ---------------------------------------------------------------------------
// Sessions and resume
// ---------------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn session_resumes_by_cli_identity_and_runs_a_second_turn() {
	let exec = executor();
	let handle = exec.handle();
	exec.push_turn([started(1), completed(1)]);
	exec.push_turn([started(1), completed(1)]);

	let mut first = open(&exec).await;
	assert!(!first.info().resumed);
	let identity = first.info().session.clone();
	assert!(run(&mut first).await.0.is_ok());
	first.terminate().await.expect("terminate");

	let mut resume = request();
	resume.resume = Some(identity.clone());
	let mut second = start_session(&exec, &resume).await.expect("resume");
	assert!(second.info().resumed);
	assert_eq!(second.info().session, identity);
	assert!(run(&mut second).await.0.is_ok());
	assert_eq!(handle.snapshot().opens[1].resume, Some(identity));
}

#[tokio::test(start_paused = true)]
async fn consecutive_turns_on_one_session_get_distinct_turn_ids() {
	let exec = executor();
	exec.push_turn([started(1), completed(1)]);
	exec.push_turn([started(2), completed(2)]);
	let mut session = open(&exec).await;
	let first = run(&mut session).await.0.expect("first turn");
	let second = run(&mut session).await.0.expect("second turn");
	assert_eq!(first.turn, turn(1));
	assert_eq!(second.turn, turn(2));
}

#[tokio::test(start_paused = true)]
async fn resume_of_an_unknown_session_fails_instead_of_starting_fresh() {
	let exec = executor();
	let handle = exec.handle();
	let mut resume = request();
	resume.resume = Some(CliSessionId::new_static("ghost"));
	let error = fail(start_session(&exec, &resume).await);
	assert!(
		matches!(error, ExecutorError::ResumeFailed { ref requested, .. } if requested.as_str() == "ghost")
	);
	assert_eq!(handle.snapshot().terminations, 1);
}

#[tokio::test(start_paused = true)]
async fn resume_answered_with_a_fresh_session_fails() {
	let exec =
		executor().with_config(MockConfig { resume: ResumeScript::Fresh, ..Default::default() });
	let first = open(&exec).await;
	let mut resume = request();
	resume.resume = Some(first.info().session.clone());
	let error = fail(start_session(&exec, &resume).await);
	assert!(matches!(error, ExecutorError::ResumeFailed { .. }));
}

#[tokio::test(start_paused = true)]
async fn resume_claimed_without_a_request_is_a_protocol_violation() {
	let exec = executor()
		.with_config(MockConfig { resume: ResumeScript::ClaimResumed, ..Default::default() });
	let error = fail(start_session(&exec, &request()).await);
	assert!(matches!(error, ExecutorError::Protocol {
		violation: ProtocolViolation::UnrequestedResume,
		..
	}));
}

#[tokio::test(start_paused = true)]
async fn resume_needs_the_resume_feature() {
	let exec = MockExecutor::new(caps(FeatureSet::FULL.without(Feature::Resume)));
	let handle = exec.handle();
	let mut resume = request();
	resume.resume = Some(CliSessionId::new_static("anything"));
	let error = fail(start_session(&exec, &resume).await);
	assert!(matches!(error, ExecutorError::FeatureUnavailable { feature: Feature::Resume, .. }));
	assert_eq!(handle.snapshot().probes, 0);
}

// ---------------------------------------------------------------------------
// Exact prompt delivery
// ---------------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn prompt_reaches_the_executor_byte_for_byte() {
	let exec = executor();
	let handle = exec.handle();
	exec.push_turn([started(1), completed(1)]);
	let mut session = open(&exec).await;
	assert!(run(&mut session).await.0.is_ok());

	let delivered = handle.snapshot().prompts;
	assert_eq!(delivered.len(), 1);
	let expected = prompt();
	assert_eq!(delivered[0], expected.sections());
	// Compare raw bytes too, so equality cannot hide a normalization.
	let wanted: [&[u8]; 4] = [
		"Eres el agente de OMP — sistema ✓\n\n  indentado  \n".as_bytes(),
		b"<rules>\n\tuse tabs\n</rules>",
		b"status: dirty\r\n",
		b"fix the bug in `src/lib.rs`",
	];
	for (section, bytes) in delivered[0].iter().zip(wanted) {
		assert_eq!(section.text.as_str().as_bytes(), bytes);
	}
	assert_eq!(
		[PromptRole::System, PromptRole::System, PromptRole::User, PromptRole::User]
			.map(|role| role.to_string()),
		delivered[0]
			.iter()
			.map(|s| s.role.to_string())
			.collect::<Vec<_>>()
			.as_slice(),
	);
}

#[tokio::test(start_paused = true)]
async fn a_rewritten_prompt_is_detected_and_the_session_torn_down() {
	let exec = executor().with_config(MockConfig { rewrite_prompt: true, ..Default::default() });
	let handle = exec.handle();
	exec.push_turn([started(1), delta(1, "should never be seen"), completed(1)]);
	let mut session = open(&exec).await;
	let (result, events) = run(&mut session).await;
	let error = fail(result);
	assert!(matches!(
		error,
		ExecutorError::PromptRewritten { supplied, forwarded, .. }
			if supplied == prompt().digest() && forwarded != supplied
	));
	assert!(events.is_empty(), "no agent output may be accepted after a rewrite");
	assert!(!session.is_open());
	assert_eq!(handle.snapshot().terminations, 1);
}

#[test]
fn digest_covers_bytes_roles_boundaries_and_bands() {
	let base = prompt();
	assert_eq!(base.digest(), prompt().digest(), "digest is deterministic");

	let section = |role, text: &str| PromptSection { role, text: Str::new(text) };
	let build =
		|sections: Vec<PromptSection>, bands| DelegatedPrompt::from_sections(sections, bands);

	let ab_c = build(vec![section(PromptRole::User, "ab"), section(PromptRole::User, "c")], bands());
	let a_bc = build(vec![section(PromptRole::User, "a"), section(PromptRole::User, "bc")], bands());
	assert_ne!(ab_c.digest(), a_bc.digest(), "section boundaries are part of the prompt");

	let as_system = build(vec![section(PromptRole::System, "x")], bands());
	let as_user = build(vec![section(PromptRole::User, "x")], bands());
	assert_ne!(as_system.digest(), as_user.digest(), "roles are part of the prompt");

	let mut other_bands = bands();
	other_bands[3] = Hash32::new([9; 32]);
	let rebanded = build(vec![section(PromptRole::User, "x")], other_bands);
	assert_ne!(as_user.digest(), rebanded.digest(), "band digests are part of the prompt");

	let trailing = build(vec![section(PromptRole::User, "x ")], bands());
	assert_ne!(as_user.digest(), trailing.digest(), "whitespace is part of the prompt");

	assert_eq!(
		DelegatedPrompt::digest_of(base.sections(), base.bands()),
		base.digest(),
		"backends recompute the same digest over what they forward"
	);
}

fn message(role: thread::Role, parts: Vec<thread::Part>) -> Item {
	Item {
		seq:           0,
		created_at_ms: 0,
		kind:          Some(item::Kind::Message(thread::Message {
			role: role as i32,
			parts,
			..Default::default()
		})),
		props:         None,
	}
}

fn text_part(text: &str) -> thread::Part {
	thread::Part { kind: Some(part::Kind::Text(text.to_owned())) }
}

#[test]
fn rendered_items_become_sections_in_order_with_the_user_turn_last() {
	let system = [
		message(thread::Role::System, vec![text_part("frozen "), text_part("and stable")]),
		message(thread::Role::System, vec![text_part("dynamic")]),
	];
	let volatile = [message(thread::Role::User, vec![text_part("status: clean")])];
	let built = DelegatedPrompt::from_items(&system, &volatile, Str::new("do the thing"), bands())
		.expect("text only");
	let sections = built.sections();
	assert_eq!(sections.len(), 4);
	assert_eq!(sections[0].text.as_str(), "frozen and stable");
	assert_eq!(sections[1].text.as_str(), "dynamic");
	assert_eq!(sections[2].role, PromptRole::User);
	assert_eq!(sections[2].text.as_str(), "status: clean");
	assert_eq!(sections[3].role, PromptRole::User);
	assert_eq!(sections[3].text.as_str(), "do the thing");
	assert_eq!(built.bands(), &bands());
}

#[test]
fn items_that_cannot_be_carried_verbatim_are_refused_not_dropped() {
	let blob_free_but_not_text = thread::Part {
		kind: Some(part::Kind::Fallback(thread::ModelFallback {
			from_model: "a".into(),
			to_model: "b".into(),
			..Default::default()
		})),
	};
	let non_text = [
		message(thread::Role::System, vec![text_part("ok")]),
		message(thread::Role::System, vec![text_part("fine"), blob_free_but_not_text]),
	];
	assert_eq!(
		DelegatedPrompt::from_items(&non_text, &[], Str::new("go"), bands()).unwrap_err(),
		PromptConversionError::NonTextPart { index: 1 }
	);

	let not_a_message =
		[Item { seq: 0, created_at_ms: 0, kind: None, props: None }];
	assert_eq!(
		DelegatedPrompt::from_items(&not_a_message, &[], Str::new("go"), bands()).unwrap_err(),
		PromptConversionError::NotAMessage { index: 0 }
	);

	let assistant = [message(thread::Role::Assistant, vec![text_part("hi")])];
	assert_eq!(
		DelegatedPrompt::from_items(&assistant, &[], Str::new("go"), bands()).unwrap_err(),
		PromptConversionError::UnsupportedRole { index: 0 }
	);
}

// ---------------------------------------------------------------------------
// Event model
// ---------------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn a_full_agent_turn_keeps_text_tools_permissions_and_usage_distinct() {
	let exec = executor();
	let tool_started = DelegatedEvent::Tool(ToolEvent::Started {
		turn:  turn(1),
		call:  CallId::new_static("call-1"),
		kind:  ToolKind::Command,
		label: Str::new_static("cargo test"),
	});
	let tool_finished = DelegatedEvent::Tool(ToolEvent::Finished {
		turn:    turn(1),
		call:    CallId::new_static("call-1"),
		outcome: ToolOutcome::Succeeded,
	});
	let ask_event = DelegatedEvent::Permission(permission_request(PermissionKind::Exec, true));
	let early = Usage { input_tokens: 10, output_tokens: 1, ..Usage::default() };
	let last = Usage { input_tokens: 120, output_tokens: 45, ..Usage::default() };
	let script = vec![
		started(1),
		delta(1, "Looking "),
		delta(1, "at it."),
		Step::Event(ask_event.clone()),
		Step::Event(tool_started.clone()),
		Step::Event(DelegatedEvent::Usage { turn: turn(1), usage: early }),
		Step::Event(tool_finished.clone()),
		Step::Event(DelegatedEvent::AgentMessage { turn: turn(1), text: Str::new_static("Done.") }),
		Step::Event(DelegatedEvent::Usage { turn: turn(1), usage: last }),
		completed(1),
	];
	exec.push_turn(script.clone());
	let mut session = open(&exec).await;
	let (mut resolver, calls) = Scripted::new(user_yes(ApprovalScope::Once));
	let (result, events) =
		run_with(&mut session, limits(), &CancellationToken::new(), &mut resolver).await;
	let report = result.expect("turn completes");

	let expected: Vec<DelegatedEvent> = script
		.into_iter()
		.map(|step| match step {
			Step::Event(event) => event,
			other => panic!("script holds only events, got {other:?}"),
		})
		.collect();
	assert_eq!(events, expected, "every event is forwarded unflattened and in order");
	assert_eq!(report.end, TurnEnd::Completed);
	assert_eq!(report.usage, last, "the last cumulative usage wins");
	assert_eq!(calls.load(Ordering::SeqCst), 1);
	assert_eq!(report.permissions.allowed, 1);
}

#[tokio::test(start_paused = true)]
async fn a_final_text_only_executor_is_representable_but_cannot_fake_streaming() {
	let features = FeatureSet::EMPTY.with(Feature::WorkingDirectoryInheritance);
	let exec = MockExecutor::new(caps(features));
	exec.push_turn([
		started(1),
		Step::Event(DelegatedEvent::AgentMessage { turn: turn(1), text: Str::new_static("final") }),
		completed(1),
	]);
	let mut session = open(&exec).await;
	assert!(run(&mut session).await.0.is_ok());

	exec.push_turn([started(2), delta(2, "streamed anyway")]);
	let (result, _) = run(&mut session).await;
	assert!(matches!(fail(result), ExecutorError::UndeclaredEvent {
		feature: Feature::StreamingText,
		..
	}));
	assert!(!session.is_open());
}

#[tokio::test(start_paused = true)]
async fn every_gated_event_needs_its_feature() {
	let cases = [
		(
			Feature::ToolEvents,
			DelegatedEvent::Tool(ToolEvent::Finished {
				turn:    turn(1),
				call:    CallId::new_static("c"),
				outcome: ToolOutcome::Failed,
			}),
		),
		(
			Feature::Permissions,
			DelegatedEvent::Permission(permission_request(PermissionKind::Exec, false)),
		),
		(Feature::UsageReporting, DelegatedEvent::Usage { turn: turn(1), usage: Usage::default() }),
	];
	for (feature, event) in cases {
		let exec = MockExecutor::new(caps(FeatureSet::FULL.without(feature)));
		exec.push_turn([started(1), Step::Event(event)]);
		let mut session = open(&exec).await;
		let error = fail(run(&mut session).await.0);
		assert!(
			matches!(error, ExecutorError::UndeclaredEvent { feature: got, .. } if got == feature),
			"{feature} must be enforced, got {error:?}"
		);
	}
}

#[tokio::test(start_paused = true)]
async fn an_event_of_another_turn_is_a_protocol_violation() {
	let exec = executor();
	exec.push_turn([started(1), delta(9, "stray")]);
	let mut session = open(&exec).await;
	let error = fail(run(&mut session).await.0);
	assert!(matches!(
		error,
		ExecutorError::Protocol {
			violation: ProtocolViolation::TurnMismatch { ref expected, ref found },
			..
		} if *expected == turn(1) && *found == turn(9)
	));
	assert!(!session.is_open());
}

#[tokio::test(start_paused = true)]
async fn session_closing_mid_turn_is_a_protocol_violation() {
	let exec = executor();
	exec.push_turn([
		started(1),
		Step::Event(DelegatedEvent::Lifecycle(LifecycleEvent::SessionClosed {
			session: CliSessionId::new_static("mock-session-1"),
		})),
	]);
	let mut session = open(&exec).await;
	let error = fail(run(&mut session).await.0);
	assert!(matches!(error, ExecutorError::Protocol {
		violation: ProtocolViolation::SessionClosedMidTurn,
		..
	}));
}

#[tokio::test(start_paused = true)]
async fn remote_failure_is_typed_and_leaves_the_session_usable() {
	let exec = executor();
	let handle = exec.handle();
	exec.push_turn([
		started(1),
		Step::Event(DelegatedEvent::Failure {
			turn:    turn(1),
			failure: RemoteFailure::RateLimited,
		}),
	]);
	exec.push_turn([started(2), completed(2)]);
	let mut session = open(&exec).await;
	let error = fail(run(&mut session).await.0);
	assert!(error.is_turn_local());
	assert!(matches!(error, ExecutorError::Remote { failure: RemoteFailure::RateLimited, .. }));
	assert!(session.is_open());
	assert_eq!(handle.snapshot().terminations, 0);
	assert!(run(&mut session).await.0.is_ok(), "the next turn runs");
}

#[tokio::test(start_paused = true)]
async fn a_closed_event_receiver_does_not_stop_the_turn() {
	let exec = executor();
	exec.push_turn([started(1), delta(1, "x"), completed(1)]);
	let mut session = open(&exec).await;
	let (tx, rx) = flume::unbounded();
	drop(rx);
	let result = session
		.run_turn(&input(), limits(), &CancellationToken::new(), &mut DenyAll, &tx)
		.await;
	assert!(result.is_ok());
}

// ---------------------------------------------------------------------------
// Malformed stream, hang, crash, timeout
// ---------------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn malformed_event_stream_ends_the_session_with_a_typed_cause() {
	let exec = executor();
	let handle = exec.handle();
	exec.push_turn([started(1), Step::Malformed(FrameFault::NotJson)]);
	let mut session = open(&exec).await;
	let error = fail(run(&mut session).await.0);
	assert!(matches!(error, ExecutorError::MalformedStream {
		frame: 1,
		fault: FrameFault::NotJson,
		..
	}));
	assert!(error.source().is_some(), "the decode fault rides #[source]");
	assert!(!error.is_turn_local());
	assert!(!session.is_open());
	assert_eq!(handle.snapshot().terminations, 1);
}

#[tokio::test(start_paused = true)]
async fn a_stream_that_just_ends_is_a_crash_not_a_clean_finish() {
	let exec = executor();
	exec.push_turn([started(1)]);
	let mut session = open(&exec).await;
	let error = fail(run(&mut session).await.0);
	assert!(matches!(error, ExecutorError::Crashed { exit: ExitFact::Unknown, .. }));
}

#[tokio::test(start_paused = true)]
async fn crash_is_reported_with_its_exit_fact_and_the_tree_is_reaped() {
	let exec = executor();
	let handle = exec.handle();
	exec.push_turn([started(1), delta(1, "partial"), Step::Crash(ExitFact::Signal(9))]);
	let mut session = open(&exec).await;
	let (result, events) = run(&mut session).await;
	assert!(matches!(fail(result), ExecutorError::Crashed { exit: ExitFact::Signal(9), .. }));
	assert_eq!(events.len(), 2, "what arrived before the crash is not lost");
	assert!(!session.is_open());
	assert_eq!(handle.snapshot().terminations, 1);
	let after = fail(run(&mut session).await.0);
	assert!(matches!(after, ExecutorError::SessionClosed { .. }));
}

#[tokio::test(start_paused = true)]
async fn teardown_failure_supersedes_the_crash_that_caused_it() {
	let exec = executor().with_config(MockConfig { teardown_fails: true, ..Default::default() });
	exec.push_turn([started(1), Step::Crash(ExitFact::Code(1))]);
	let mut session = open(&exec).await;
	let error = fail(run(&mut session).await.0);
	assert!(matches!(error, ExecutorError::TeardownFailed { .. }));
	assert!(error.source().is_some());
}

#[tokio::test(start_paused = true)]
async fn teardown_failure_supersedes_a_policy_rejection_at_open() {
	let loose = DelegatedPolicy::new(
		SandboxPosture::Unrestricted,
		NetworkPosture::Open,
		ApprovalPosture::Yolo,
	);
	let exec = executor().with_config(MockConfig {
		applied_policy: Some(loose),
		teardown_fails: true,
		..Default::default()
	});
	let error = fail(start_session(&exec, &request()).await);
	assert!(matches!(error, ExecutorError::TeardownFailed { .. }));
}

#[tokio::test(start_paused = true)]
async fn hung_process_is_detected_by_the_idle_limit_and_killed() {
	let exec = executor();
	let handle = exec.handle();
	exec.push_turn([started(1), Step::Hang]);
	let mut session = open(&exec).await;
	let before = tokio::time::Instant::now();
	let error = fail(run(&mut session).await.0);
	assert!(matches!(error, ExecutorError::Hung { idle, .. } if idle == Duration::from_secs(2)));
	assert_eq!(before.elapsed(), Duration::from_secs(2));
	assert!(error.is_turn_local(), "a hang is a turn failure by classification");
	assert!(!session.is_open(), "but the wedged process is not reused");
	assert_eq!(handle.snapshot().terminations, 1);
}

#[tokio::test(start_paused = true)]
async fn total_timeout_fires_even_when_events_keep_arriving_within_the_idle_limit() {
	let exec = executor();
	let handle = exec.handle();
	let slow = Duration::from_millis(80);
	exec.push_turn([
		started(1),
		Step::Sleep(slow),
		delta(1, "a"),
		Step::Sleep(slow),
		delta(1, "b"),
		Step::Sleep(slow),
		delta(1, "c"),
		completed(1),
	]);
	let mut session = open(&exec).await;
	let tight = TurnLimits {
		timeout:         Duration::from_millis(150),
		idle:            Duration::from_millis(100),
		interrupt_grace: Duration::from_millis(50),
	};
	let before = tokio::time::Instant::now();
	let (result, events) =
		run_with(&mut session, tight, &CancellationToken::new(), &mut DenyAll).await;
	assert!(matches!(
		fail(result),
		ExecutorError::TimedOut { limit, .. } if limit == Duration::from_millis(150)
	));
	assert_eq!(before.elapsed(), Duration::from_millis(150));
	assert_eq!(events.len(), 2, "started and the first delta arrived in time");
	assert!(!session.is_open());
	assert_eq!(handle.snapshot().terminations, 1);
}

#[tokio::test(start_paused = true)]
async fn time_spent_waiting_for_a_human_does_not_count_against_the_turn() {
	let exec = executor();
	exec.push_turn([
		started(1),
		Step::Event(DelegatedEvent::Permission(permission_request(PermissionKind::Exec, false))),
		completed(1),
	]);
	let mut session = open(&exec).await;
	let (mut resolver, _) = Scripted::new(user_yes(ApprovalScope::Once));
	resolver.delay = Duration::from_secs(3600);
	let tight = TurnLimits {
		timeout:         Duration::from_millis(100),
		idle:            Duration::from_millis(100),
		interrupt_grace: Duration::from_millis(50),
	};
	let (result, _) = run_with(&mut session, tight, &CancellationToken::new(), &mut resolver).await;
	assert!(result.is_ok(), "an hour at the approval desk is not the agent's time");
}

// ---------------------------------------------------------------------------
// Cancellation
// ---------------------------------------------------------------------------

async fn cancel_after_10ms<R: PermissionResolver>(
	session: &mut Session,
	resolver: &mut R,
) -> (Result<TurnReport, ExecutorError>, Vec<DelegatedEvent>) {
	let cancel = CancellationToken::new();
	let trigger = cancel.clone();
	let canceller = async move {
		tokio::time::sleep(Duration::from_millis(10)).await;
		trigger.cancel();
	};
	let (outcome, ()) = tokio::join!(run_with(session, limits(), &cancel, resolver), canceller);
	outcome
}

#[tokio::test(start_paused = true)]
async fn cancellation_interrupts_cooperatively_and_keeps_the_session() {
	let exec = executor();
	let handle = exec.handle();
	exec.push_turn([started(1), Step::Hang]);
	exec.push_turn([started(2), completed(2)]);
	let mut session = open(&exec).await;
	let (result, events) = cancel_after_10ms(&mut session, &mut DenyAll).await;
	assert!(matches!(fail(result), ExecutorError::Cancelled { .. }));
	assert!(
		matches!(
			events.last(),
			Some(DelegatedEvent::Lifecycle(LifecycleEvent::TurnCompleted {
				outcome: TurnEnd::Interrupted,
				..
			}))
		),
		"the CLI's own interrupted end is forwarded: {events:?}"
	);
	assert!(session.is_open());
	let record = handle.snapshot();
	assert_eq!((record.interrupts, record.terminations), (1, 0));
	assert!(run(&mut session).await.0.is_ok(), "the session runs another turn");
}

#[tokio::test(start_paused = true)]
async fn cancellation_kills_a_cli_that_ignores_the_interrupt() {
	let exec = executor()
		.with_config(MockConfig { interrupt: InterruptScript::Ignore, ..Default::default() });
	let handle = exec.handle();
	exec.push_turn([started(1), Step::Hang]);
	let mut session = open(&exec).await;
	let before = tokio::time::Instant::now();
	let (result, _) = cancel_after_10ms(&mut session, &mut DenyAll).await;
	assert!(matches!(fail(result), ExecutorError::Cancelled { .. }));
	assert_eq!(before.elapsed(), Duration::from_millis(510), "10 ms to cancel + 500 ms grace");
	assert!(!session.is_open());
	let record = handle.snapshot();
	assert_eq!((record.interrupts, record.terminations), (1, 1));
}

#[tokio::test(start_paused = true)]
async fn cancellation_without_interrupt_support_goes_straight_to_teardown() {
	let exec = MockExecutor::new(caps(FeatureSet::FULL.without(Feature::Interrupt)));
	let handle = exec.handle();
	exec.push_turn([started(1), Step::Hang]);
	let mut session = open(&exec).await;
	let (result, _) = cancel_after_10ms(&mut session, &mut DenyAll).await;
	assert!(matches!(fail(result), ExecutorError::Cancelled { .. }));
	let record = handle.snapshot();
	assert_eq!((record.interrupts, record.terminations), (0, 1));
}

#[tokio::test(start_paused = true)]
async fn cancellation_while_awaiting_an_approval_stops_waiting() {
	let exec = executor();
	exec.push_turn([
		started(1),
		Step::Event(DelegatedEvent::Permission(permission_request(PermissionKind::Exec, false))),
	]);
	let mut session = open(&exec).await;
	let (mut resolver, calls) = Scripted::new(user_yes(ApprovalScope::Once));
	resolver.delay = Duration::from_secs(3600);
	let (result, _) = cancel_after_10ms(&mut session, &mut resolver).await;
	assert!(matches!(fail(result), ExecutorError::Cancelled { .. }));
	assert_eq!(calls.load(Ordering::SeqCst), 1);
	assert!(session.is_open(), "the CLI settled its own interrupt");
}

#[tokio::test(start_paused = true)]
async fn an_already_cancelled_token_starts_nothing() {
	let exec = executor();
	let handle = exec.handle();
	let mut session = open(&exec).await;
	let cancel = CancellationToken::new();
	cancel.cancel();
	let (result, events) = run_with(&mut session, limits(), &cancel, &mut DenyAll).await;
	assert!(matches!(fail(result), ExecutorError::Cancelled { .. }));
	assert!(events.is_empty());
	assert!(handle.snapshot().prompts.is_empty(), "no prompt may reach a cancelled run");
	assert!(session.is_open());
}

#[tokio::test(start_paused = true)]
async fn dropping_a_live_session_kills_it_and_terminate_is_idempotent() {
	let exec = executor();
	let handle = exec.handle();
	drop(open(&exec).await);
	assert_eq!(handle.snapshot().dropped_live, 1, "Drop is the kill-on-drop path");

	let exec = executor();
	let handle = exec.handle();
	let mut session = open(&exec).await;
	session.terminate().await.expect("first terminate");
	session
		.terminate()
		.await
		.expect("second terminate is a no-op");
	drop(session);
	let record = handle.snapshot();
	assert_eq!((record.terminations, record.dropped_live), (1, 0));
}

// ---------------------------------------------------------------------------
// Permissions: fail closed
// ---------------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn policy_forbidden_requests_are_denied_without_asking_anyone() {
	for (kind, label) in [(PermissionKind::Write, "write"), (PermissionKind::Network, "network")] {
		let (answer, calls, tally) = ask(
			DelegatedPolicy::STRICTEST,
			kind,
			true,
			// Even a resolver that would say yes must never be reached.
			user_yes(ApprovalScope::Session),
		)
		.await;
		assert_eq!(answer, PermissionAnswer::Deny, "{label} under the strictest policy");
		assert_eq!(calls, 0, "{label}: the user is not asked for what policy forbids");
		assert_eq!(tally, PermissionTally { refused_by_policy: 1, ..Default::default() });
	}
}

#[tokio::test(start_paused = true)]
async fn unclassified_requests_are_denied_without_asking_anyone() {
	let (answer, calls, tally) =
		ask(WORKSPACE_WRITE, PermissionKind::Unclassified, true, user_yes(ApprovalScope::Session))
			.await;
	assert_eq!(answer, PermissionAnswer::Deny);
	assert_eq!(calls, 0);
	assert_eq!(tally.refused_by_policy, 1);
}

#[tokio::test(start_paused = true)]
async fn host_defined_scopes_cannot_be_mapped_and_are_denied() {
	let (answer, calls, tally) = ask(
		WORKSPACE_WRITE,
		PermissionKind::Exec,
		true,
		user_yes(ApprovalScope::Custom(Str::new_static("workspace-forever"))),
	)
	.await;
	assert_eq!(answer, PermissionAnswer::Deny);
	assert_eq!(calls, 1, "the user was asked, but the approval cannot be forwarded");
	assert_eq!(tally, PermissionTally { asked: 1, refused_by_policy: 1, ..Default::default() });
}

#[tokio::test(start_paused = true)]
async fn unaudited_fail_open_approvals_are_denied() {
	for source in [ApprovalSource::Timeout, ApprovalSource::Unavailable] {
		let unaudited = decision(true, ApprovalScope::Once, source, false);
		let (answer, _, tally) = ask(WORKSPACE_WRITE, PermissionKind::Exec, false, unaudited).await;
		assert_eq!(answer, PermissionAnswer::Deny, "{source} without an audit record");
		assert_eq!(tally.refused_by_policy, 1);
	}
	let audited = decision(true, ApprovalScope::Once, ApprovalSource::Timeout, true);
	let (answer, ..) = ask(WORKSPACE_WRITE, PermissionKind::Exec, false, audited).await;
	assert_eq!(answer, PermissionAnswer::AllowOnce, "an audited fail-open result is forwarded");
}

#[tokio::test(start_paused = true)]
async fn a_denial_and_a_missing_approver_both_deny() {
	let denied = decision(false, ApprovalScope::Session, ApprovalSource::User, false);
	let (answer, _, tally) = ask(WORKSPACE_WRITE, PermissionKind::Exec, true, denied).await;
	assert_eq!(answer, PermissionAnswer::Deny);
	assert_eq!(tally, PermissionTally { asked: 1, denied: 1, ..Default::default() });

	let exec = executor();
	let handle = exec.handle();
	exec.push_turn([
		started(1),
		Step::Event(DelegatedEvent::Permission(permission_request(PermissionKind::Exec, true))),
		completed(1),
	]);
	let mut session = open(&exec).await;
	let (result, _) = run(&mut session).await;
	assert_eq!(result.expect("turn").permissions.denied, 1);
	assert_eq!(handle.snapshot().answers[0].1, PermissionAnswer::Deny, "DenyAll denies");
}

#[tokio::test(start_paused = true)]
async fn grants_are_only_ever_narrowed() {
	let (answer, ..) =
		ask(WORKSPACE_WRITE, PermissionKind::Exec, true, user_yes(ApprovalScope::Turn)).await;
	assert_eq!(answer, PermissionAnswer::AllowOnce, "turn scope is not a session grant");

	let (answer, ..) =
		ask(WORKSPACE_WRITE, PermissionKind::Exec, true, user_yes(ApprovalScope::Session)).await;
	assert_eq!(answer, PermissionAnswer::AllowForSession);

	let (answer, ..) =
		ask(WORKSPACE_WRITE, PermissionKind::Exec, false, user_yes(ApprovalScope::Session)).await;
	assert_eq!(answer, PermissionAnswer::AllowOnce, "a CLI that cannot remember gets a single use");

	let (answer, ..) =
		ask(WORKSPACE_WRITE, PermissionKind::Exec, false, user_yes(ApprovalScope::Persist)).await;
	assert_eq!(answer, PermissionAnswer::AllowOnce);
}

#[test]
fn admission_and_mapping_tables_are_exact() {
	let request = |kind| permission_request(kind, true);
	let every_policy = [
		DelegatedPolicy::STRICTEST,
		WORKSPACE_WRITE,
		DelegatedPolicy::new(
			SandboxPosture::Unrestricted,
			NetworkPosture::Open,
			ApprovalPosture::Yolo,
		),
	];
	for policy in every_policy {
		assert_eq!(
			admit_request(policy, &request(PermissionKind::Unclassified)),
			Err(DenyReason::UnclassifiedRequest),
			"unclassified is never grantable, even when everything is open"
		);
		assert_eq!(admit_request(policy, &request(PermissionKind::Read)), Ok(()));
		assert_eq!(admit_request(policy, &request(PermissionKind::Exec)), Ok(()));
	}
	assert_eq!(
		admit_request(DelegatedPolicy::STRICTEST, &request(PermissionKind::Write)),
		Err(DenyReason::SandboxForbidsWrite)
	);
	assert_eq!(admit_request(WORKSPACE_WRITE, &request(PermissionKind::Write)), Ok(()));
	assert_eq!(
		admit_request(DelegatedPolicy::STRICTEST, &request(PermissionKind::Network)),
		Err(DenyReason::NetworkDisabled)
	);
	assert_eq!(admit_request(WORKSPACE_WRITE, &request(PermissionKind::Network)), Ok(()));

	let asked = request(PermissionKind::Exec);
	for scope in [
		ApprovalScope::Once,
		ApprovalScope::Call,
		ApprovalScope::Turn,
		ApprovalScope::Session,
		ApprovalScope::Persist,
		ApprovalScope::Custom(Str::new_static("x")),
	] {
		assert_eq!(
			map_decision(&asked, &decision(false, scope.clone(), ApprovalSource::User, false)),
			Ok(PermissionAnswer::Deny),
			"a refusal is a refusal at scope {scope}"
		);
	}
}

// ---------------------------------------------------------------------------
// Credentials
// ---------------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn the_contract_hands_the_executor_no_credential_or_environment() {
	// `SessionRequest` is exhaustively destructured: adding a credential, an
	// auth path or an environment field breaks this test and forces a review.
	let SessionRequest { mode: _, workdir: _, policy: _, resume: _, unknown_auth: _ } = request();

	let exec = executor();
	let handle = exec.handle();
	let _session = open(&exec).await;
	let record = handle.snapshot();
	// Everything an executor ever learns about the session fits in this record.
	assert_eq!(record.opens.len(), 1);
	assert_eq!(record.opens[0].mode, ExecutionMode::CliManaged);
}
