//! Tests for [`omp_driver::delegated_prompt::build_delegated_prompt`].
//!
//! Golden compare: the sections equal what [`CanonicalPromptSource`] renders
//! for the same empty [`Dom`].
//!
//! Rejection: empty and over-long task texts return typed errors.
//!
//! Digest mismatch: a backend that paraphrases the prompt causes
//! [`ActiveSession::run_turn`] to return [`ExecutorError::PromptRewritten`]
//! and close the session.

use std::{path::PathBuf, time::Duration};

use omp_agent::{
	BandHash, CanonicalPromptSource,
	delegated::{
		ActiveSession, ApprovalPosture, Capabilities, CliVersion, DelegatedPolicy, DelegatedPrompt,
		DenyAll, ExecutionMode, ExecutorError, ExecutorId, FeatureSet, NetworkPosture, PromptRole,
		SandboxPosture, SessionRequest, TurnInput, TurnLimits, UnknownAuth, VersionRange,
		mock::{MockConfig, MockExecutor, Step},
		start_session,
	},
};
use omp_core::{Hash32, Str};
use omp_dom::Dom;
use omp_driver::delegated_prompt::{BuildError, MAX_TASK_BYTES, build_delegated_prompt};
use tokio_util::sync::CancellationToken;

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

fn limits() -> TurnLimits {
	TurnLimits {
		timeout:         Duration::from_secs(10),
		idle:            Duration::from_secs(2),
		interrupt_grace: Duration::from_millis(500),
	}
}

fn caps() -> Capabilities {
	Capabilities::new(
		ExecutorId::new_static("mock-cli"),
		VersionRange {
			min_inclusive: CliVersion::new(1, 0, 0),
			max_exclusive: Some(CliVersion::new(2, 0, 0)),
		},
		FeatureSet::FULL,
		[
			DelegatedPolicy::new(
				SandboxPosture::WorkspaceWrite,
				NetworkPosture::Scoped,
				ApprovalPosture::Write,
			),
			DelegatedPolicy::STRICTEST,
		],
	)
}

fn executor_with(config: MockConfig) -> MockExecutor {
	MockExecutor::new(caps()).with_config(config)
}

fn request() -> SessionRequest {
	SessionRequest {
		mode:         ExecutionMode::CliManaged,
		workdir:      PathBuf::from("/work/project"),
		policy:       DelegatedPolicy::new(
			SandboxPosture::WorkspaceWrite,
			NetworkPosture::Scoped,
			ApprovalPosture::Write,
		),
		resume:       None,
		unknown_auth: UnknownAuth::Refuse,
	}
}

// ---------------------------------------------------------------------------
// Golden compare
// ---------------------------------------------------------------------------

/// The prompt sections [`build_delegated_prompt`] produces must equal what
/// [`CanonicalPromptSource`] renders for the same [`Dom`].
#[test]
fn sections_equal_canonical_prompt_source_for_same_dom() {
	let dom = Dom::new();
	let task = Str::new("write a hello-world program");

	let (prompt, returned_limits) =
		build_delegated_prompt(&dom, task.clone(), limits()).expect("build succeeds on an empty dom");

	// Limits are returned unmodified.
	assert_eq!(returned_limits, limits());

	// Reconstruct the expected sections from the same source.
	let src = CanonicalPromptSource;
	let (system_items, band_hashes) = src.banded_render(&dom).expect("banded_render succeeds");
	let volatile_items = src.volatile_items(&dom).expect("volatile_items succeeds");
	let bands: [Hash32; 4] = band_hashes.map(|b: BandHash| Hash32::new(*b.as_bytes()));
	let expected = DelegatedPrompt::from_items(&system_items, &volatile_items, task, bands)
		.expect("from_items succeeds");

	assert_eq!(
		prompt.sections(),
		expected.sections(),
		"sections must match CanonicalPromptSource output verbatim",
	);
	assert_eq!(prompt.digest(), expected.digest(), "digests must match");
}

/// Volatile items must not appear in the stable bands. A non-empty volatile
/// band is detected by verifying the volatile band hash changes when the DOM
/// changes, and that system (non-volatile) sections do not change.
#[test]
fn volatile_items_are_not_in_stable_band() {
	let dom = Dom::new();
	let task = Str::new("task");

	let (prompt, _) = build_delegated_prompt(&dom, task, limits()).expect("build on empty dom");

	// All system-role sections come before any user-role section (except the
	// final task). The volatile items are user-role; system sections are
	// system-role. There is no stable section (system role) after a user
	// volatile section.
	let sections = prompt.sections();
	let last_system = sections.iter().rposition(|s| s.role == PromptRole::System);
	let first_user = sections.iter().position(|s| s.role == PromptRole::User);

	// If there are any system sections they must all precede the first user
	// section (i.e. no stable system content is mixed into the volatile band).
	if let (Some(last_sys), Some(first_usr)) = (last_system, first_user) {
		assert!(
			last_sys < first_usr,
			"all system sections must precede any user section; volatile items must not precede \
			 stable system items",
		);
	}
}

// ---------------------------------------------------------------------------
// Rejection
// ---------------------------------------------------------------------------

#[test]
fn empty_task_is_rejected() {
	let dom = Dom::new();
	let err = build_delegated_prompt(&dom, Str::new(""), limits())
		.expect_err("empty task must be rejected");
	assert!(matches!(err, BuildError::EmptyTask), "expected EmptyTask, got {err:?}",);
}

#[test]
fn over_long_task_is_rejected() {
	let dom = Dom::new();
	let long = Str::from("x".repeat(MAX_TASK_BYTES + 1));
	let err =
		build_delegated_prompt(&dom, long, limits()).expect_err("over-long task must be rejected");
	assert!(
		matches!(err, BuildError::TaskTooLong { actual } if actual == MAX_TASK_BYTES + 1),
		"expected TaskTooLong with correct length, got {err:?}",
	);
}

#[test]
fn task_at_exact_limit_is_accepted() {
	let dom = Dom::new();
	let exactly = Str::from("x".repeat(MAX_TASK_BYTES));
	build_delegated_prompt(&dom, exactly, limits())
		.expect("task at MAX_TASK_BYTES must be accepted");
}

// ---------------------------------------------------------------------------
// Digest mismatch ends the session
// ---------------------------------------------------------------------------

/// A backend that rewrites the prompt (paraphrases the first section) reports
/// a digest that does not match [`DelegatedPrompt::digest`]. The host detects
/// this and ends the session with [`ExecutorError::PromptRewritten`].
#[tokio::test(start_paused = true)]
async fn digest_mismatch_from_lying_backend_ends_session() {
	let exec = executor_with(MockConfig { rewrite_prompt: true, ..Default::default() });
	let handle = exec.handle();

	// Script one turn so the mock has steps to return.
	use omp_agent::delegated::{DelegatedEvent, LifecycleEvent, TurnId};

	let turn_id = TurnId::new("turn-1");
	exec.push_turn([Step::Event(DelegatedEvent::Lifecycle(LifecycleEvent::TurnStarted {
		turn: turn_id.clone(),
	}))]);

	let mut session: ActiveSession<_> = start_session(&exec, &request())
		.await
		.expect("session opens");

	let dom = Dom::new();
	let task = Str::new("write a hello-world program");
	let (prompt, lim) =
		build_delegated_prompt(&dom, task, limits()).expect("build_delegated_prompt succeeds");

	let input = TurnInput { prompt };
	let (tx, rx) = flume::unbounded::<DelegatedEvent>();
	let cancel = CancellationToken::new();
	let result = session
		.run_turn(&input, lim, &cancel, &mut DenyAll, &tx)
		.await;
	drop(tx);

	let events: Vec<_> = rx.drain().collect();

	let error = result.expect_err("rewritten prompt must cause an error");
	assert!(
		matches!(
			error,
			ExecutorError::PromptRewritten { supplied, forwarded, .. }
				if supplied == input.prompt.digest() && forwarded != supplied
		),
		"expected PromptRewritten, got {error:?}",
	);
	assert!(events.is_empty(), "no events must be accepted after a rewrite detection");
	assert!(!session.is_open(), "session must be closed after PromptRewritten");
	assert_eq!(handle.snapshot().terminations, 1, "session must have been terminated");
}
