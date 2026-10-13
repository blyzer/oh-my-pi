//! The host driver: written once over the backend traits so every executor
//! gets the same negotiation, time limits and failure handling.
//!
//! [`start_session`] negotiates and verifies; [`ActiveSession::run_turn`]
//! drives one turn. Both fail closed: anything that cannot be proven to match
//! the request ends the session instead of being tolerated.

use std::time::Duration;

use omp_ai::Usage;
use omp_core::Str;
use tokio::time::{Instant, timeout, timeout_at};
use tokio_util::sync::CancellationToken;

use super::{
	CliSessionId, DelegatedEvent, DelegatedExecutor, DelegatedPolicy, DelegatedSession,
	ExecutorError, ExecutorId, Feature, FeatureSet, LifecycleEvent, PermissionAnswer,
	PermissionRequest, ProtocolViolation, RemoteFailure, SessionInfo, SessionRequest, TurnEnd,
	TurnId, TurnInput, TurnLimits, UnknownAuth, admit_request, map_decision,
};
use crate::approvals::{ApprovalDecision, ApprovalScope, ApprovalSource};

/// An upper bound used when a requested limit would overflow the clock.
const FAR_FUTURE: Duration = Duration::from_secs(60 * 60 * 24 * 365 * 30);

/// Answers the permission prompts a delegated agent raises.
///
/// The production implementation files the request on a kernel-bound
/// `ApprovalRoute` and returns the journaled decision, so the host, hooks and
/// session grants decide exactly as they do for OMP's own tools. The time a
/// resolver takes is not counted against the turn timeout.
pub trait PermissionResolver: Send {
	/// Decides one permission request.
	fn resolve<'a>(
		&'a mut self,
		request: &'a PermissionRequest,
	) -> impl Future<Output = ApprovalDecision> + Send + 'a;
}

/// A resolver that denies everything: the answer when no approver exists
/// (print mode, a headless daemon with no relay).
#[derive(Clone, Copy, Debug, Default)]
pub struct DenyAll;

impl PermissionResolver for DenyAll {
	fn resolve<'a>(
		&'a mut self,
		_request: &'a PermissionRequest,
	) -> impl Future<Output = ApprovalDecision> + Send + 'a {
		std::future::ready(ApprovalDecision {
			approved:   false,
			scope:      ApprovalScope::Once,
			source:     ApprovalSource::Unavailable,
			decided_by: None,
			reason:     Some(Str::new_static("no approver is available for delegated permissions")),
			audited:    false,
		})
	}
}

/// What happened to the permission prompts of one turn.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PermissionTally {
	/// Prompts put to the resolver.
	pub asked:              u32,
	/// Prompts the resolver approved and the CLI was told to allow.
	pub allowed:            u32,
	/// Prompts the resolver denied.
	pub denied:             u32,
	/// Prompts the host denied itself, without a usable approval: forbidden by
	/// the session policy, unclassified, or an approval that cannot be mapped.
	pub refused_by_policy:  u32,
}

/// The settled result of one turn.
#[derive(Clone, Debug)]
pub struct TurnReport {
	/// The turn.
	pub turn:        TurnId,
	/// How the CLI ended it.
	pub end:         TurnEnd,
	/// The last cumulative usage the CLI reported; zero when it reported none.
	pub usage:       Usage,
	/// What happened to its permission prompts.
	pub permissions: PermissionTally,
}

/// A negotiated, verified, open delegated session.
///
/// Holds exactly one backend session. After any failure that leaves the
/// process in an unknown state the session is torn down and
/// [`ActiveSession::is_open`] turns false; dropping an open `ActiveSession`
/// drops the backend session, which kills the process tree.
pub struct ActiveSession<S: DelegatedSession> {
	executor: ExecutorId,
	features: FeatureSet,
	policy:   DelegatedPolicy,
	info:     SessionInfo,
	session:  Option<S>,
}

/// What the driver needs from the negotiated session while driving a turn.
struct Ctx<'a> {
	executor: &'a ExecutorId,
	features: FeatureSet,
	policy:   DelegatedPolicy,
}

/// Whether the session survives the error that ended a turn.
enum Stop {
	/// The process is healthy and idle; the next turn may run.
	Keep(ExecutorError),
	/// The process is in an unknown state; tear it down.
	End(ExecutorError),
}

/// What woke the turn loop.
enum Wake {
	Event(Result<DelegatedEvent, ExecutorError>),
	Cancelled,
	Idle,
	Total,
}

/// Negotiates and opens a session.
///
/// In order, and stopping at the first failure:
///
/// 1. the request's [`ExecutionMode`](super::ExecutionMode) matches the executor's (no direct-API reroute);
/// 2. the working directory is absolute and the executor inherits it;
/// 3. a resume is only requested of an executor that can resume;
/// 4. the policy is one the executor lists verbatim;
/// 5. the CLI is installed, within the supported version range, and either
///    logged in or unknown with the request's explicit consent;
/// 6. after opening, the executor's reported working directory, policy and
///    resume state equal the request. Any difference tears the session down.
///
/// # Errors
///
/// The first failing step, as an [`ExecutorError`]. A teardown failure while
/// rejecting an opened session supersedes the rejection, because it can leak
/// a process.
pub async fn start_session<E: DelegatedExecutor>(
	executor: &E,
	request: &SessionRequest,
) -> Result<ActiveSession<E::Session>, ExecutorError> {
	let caps = executor.capabilities();
	let id = &caps.executor;
	if request.mode != caps.mode {
		return Err(ExecutorError::ModeUnsupported {
			executor:  id.clone(),
			requested: request.mode,
			offered:   caps.mode,
		});
	}
	if !request.workdir.is_absolute() {
		return Err(ExecutorError::WorkdirNotAbsolute { path: request.workdir.clone() });
	}
	require(caps.features, id, Feature::WorkingDirectoryInheritance)?;
	if request.resume.is_some() {
		require(caps.features, id, Feature::Resume)?;
	}
	if !caps.supports_policy(request.policy) {
		return Err(ExecutorError::PolicyUnmappable {
			executor:  id.clone(),
			requested: request.policy,
		});
	}

	let probe = executor.probe().await?;
	if !caps.versions.contains(probe.version) {
		return Err(ExecutorError::UnsupportedVersion {
			executor:  id.clone(),
			found:     probe.version,
			supported: caps.versions,
		});
	}
	match probe.auth {
		super::AuthStatus::Authenticated => {},
		super::AuthStatus::Required => {
			return Err(ExecutorError::AuthRequired { executor: id.clone() });
		},
		super::AuthStatus::Unknown if request.unknown_auth == UnknownAuth::Refuse => {
			return Err(ExecutorError::AuthUnknown { executor: id.clone() });
		},
		super::AuthStatus::Unknown => {},
	}

	let (mut session, info) = executor.open(request).await?;
	if let Err(cause) = verify_open(id, request, &info) {
		return Err(reject_opened(&mut session, cause).await);
	}
	Ok(ActiveSession {
		executor: id.clone(),
		features: caps.features,
		policy: request.policy,
		info,
		session: Some(session),
	})
}

fn require(
	features: FeatureSet,
	executor: &ExecutorId,
	feature: Feature,
) -> Result<(), ExecutorError> {
	if features.contains(feature) {
		Ok(())
	} else {
		Err(ExecutorError::FeatureUnavailable { executor: executor.clone(), feature })
	}
}

fn verify_open(
	executor: &ExecutorId,
	request: &SessionRequest,
	info: &SessionInfo,
) -> Result<(), ExecutorError> {
	if info.workdir != request.workdir {
		return Err(ExecutorError::WorkdirMismatch {
			executor:  executor.clone(),
			requested: request.workdir.clone(),
			reported:  info.workdir.clone(),
		});
	}
	if info.policy != request.policy {
		return Err(ExecutorError::PolicyDrift {
			executor:  executor.clone(),
			requested: request.policy,
			applied:   info.policy,
		});
	}
	match (&request.resume, info.resumed) {
		(Some(wanted), true) if info.session == *wanted => Ok(()),
		(Some(wanted), _) => Err(resume_failed(executor, wanted)),
		(None, true) => Err(ExecutorError::Protocol {
			executor:  executor.clone(),
			violation: ProtocolViolation::UnrequestedResume,
		}),
		(None, false) => Ok(()),
	}
}

fn resume_failed(executor: &ExecutorId, wanted: &CliSessionId) -> ExecutorError {
	ExecutorError::ResumeFailed { executor: executor.clone(), requested: wanted.clone() }
}

/// Tears down a session the host refuses to use; a teardown failure wins.
async fn reject_opened<S: DelegatedSession>(
	session: &mut S,
	cause: ExecutorError,
) -> ExecutorError {
	match session.terminate().await {
		Ok(()) => cause,
		Err(fault) => {
			tracing::warn!(error = %cause, "delegated session rejected, then teardown failed");
			fault
		},
	}
}

impl<S: DelegatedSession> ActiveSession<S> {
	/// What the backend reported when it opened the session.
	#[must_use]
	pub const fn info(&self) -> &SessionInfo {
		&self.info
	}

	/// Whether the backend session is still held.
	#[must_use]
	pub const fn is_open(&self) -> bool {
		self.session.is_some()
	}

	/// Runs one turn to its end.
	///
	/// Events are forwarded to `events` as they arrive; a closed receiver does
	/// not stop the turn. Permission prompts go to `resolver`, after the host
	/// has refused those the session policy forbids.
	///
	/// Time limits: [`TurnLimits::idle`] bounds the silence between events and
	/// [`TurnLimits::timeout`] bounds the whole turn, excluding time spent in
	/// `resolver`. A process that exceeds either, or dies, or emits something
	/// undecodable, is torn down.
	///
	/// Cancelling `cancel` interrupts cooperatively when the executor
	/// supports it and the CLI settles within [`TurnLimits::interrupt_grace`];
	/// otherwise the process tree is killed. Either way the result is
	/// [`ExecutorError::Cancelled`].
	///
	/// # Errors
	///
	/// [`ExecutorError`]. Afterwards [`ActiveSession::is_open`] says whether
	/// the session can run another turn.
	pub async fn run_turn<R: PermissionResolver>(
		&mut self,
		input: &TurnInput,
		limits: TurnLimits,
		cancel: &CancellationToken,
		resolver: &mut R,
		events: &flume::Sender<DelegatedEvent>,
	) -> Result<TurnReport, ExecutorError> {
		let Some(session) = self.session.as_mut() else {
			return Err(ExecutorError::SessionClosed { executor: self.executor.clone() });
		};
		let ctx = Ctx { executor: &self.executor, features: self.features, policy: self.policy };
		match drive(&ctx, session, input, limits, cancel, resolver, events).await {
			Ok(report) => Ok(report),
			Err(Stop::Keep(error)) => Err(error),
			Err(Stop::End(error)) => Err(self.end_after(error).await),
		}
	}

	/// Terminates the process tree. Idempotent.
	///
	/// # Errors
	///
	/// [`ExecutorError::TeardownFailed`] when the tree could not be reaped.
	pub async fn terminate(&mut self) -> Result<(), ExecutorError> {
		match self.session.take() {
			Some(mut session) => session.terminate().await,
			None => Ok(()),
		}
	}

	async fn end_after(&mut self, cause: ExecutorError) -> ExecutorError {
		let Some(mut session) = self.session.take() else {
			return cause;
		};
		match session.terminate().await {
			Ok(()) => cause,
			Err(fault) => {
				tracing::warn!(error = %cause, "delegated turn failed, then teardown failed");
				fault
			},
		}
	}
}

fn deadline(from: Instant, span: Duration) -> Instant {
	from.checked_add(span).unwrap_or_else(|| from + FAR_FUTURE)
}

fn hung(ctx: &Ctx<'_>, limits: TurnLimits) -> Stop {
	Stop::End(ExecutorError::Hung { executor: ctx.executor.clone(), idle: limits.idle })
}

fn remote(ctx: &Ctx<'_>, turn: &TurnId, failure: RemoteFailure) -> Stop {
	Stop::Keep(match failure {
		RemoteFailure::AuthRequired => ExecutorError::AuthRequired { executor: ctx.executor.clone() },
		other => ExecutorError::Remote {
			executor: ctx.executor.clone(),
			turn:     turn.clone(),
			failure:  other,
		},
	})
}

async fn forward(events: &flume::Sender<DelegatedEvent>, event: DelegatedEvent) {
	// A closed receiver means nobody is watching; the turn itself goes on.
	let _ = events.send_async(event).await;
}

fn check(ctx: &Ctx<'_>, turn: &TurnId, event: &DelegatedEvent) -> Result<(), Stop> {
	if let Some(feature) = event.requires()
		&& !ctx.features.contains(feature)
	{
		return Err(Stop::End(ExecutorError::UndeclaredEvent {
			executor: ctx.executor.clone(),
			feature,
		}));
	}
	match event.turn() {
		Some(found) if found != turn => Err(Stop::End(ExecutorError::Protocol {
			executor:  ctx.executor.clone(),
			violation: ProtocolViolation::TurnMismatch {
				expected: turn.clone(),
				found:    found.clone(),
			},
		})),
		None => Err(Stop::End(ExecutorError::Protocol {
			executor:  ctx.executor.clone(),
			violation: ProtocolViolation::SessionClosedMidTurn,
		})),
		Some(_) => Ok(()),
	}
}

async fn next<S: DelegatedSession>(
	session: &mut S,
	cancel: &CancellationToken,
	total: Instant,
	idle: Duration,
) -> Wake {
	let (at, total_first) = match Instant::now().checked_add(idle) {
		Some(at) if at < total => (at, false),
		_ => (total, true),
	};
	tokio::select! {
		biased;
		() = cancel.cancelled() => Wake::Cancelled,
		got = timeout_at(at, session.next_event()) => match got {
			Ok(result) => Wake::Event(result),
			Err(_) if total_first => Wake::Total,
			Err(_) => Wake::Idle,
		},
	}
}

async fn drive<S: DelegatedSession, R: PermissionResolver>(
	ctx: &Ctx<'_>,
	session: &mut S,
	input: &TurnInput,
	limits: TurnLimits,
	cancel: &CancellationToken,
	resolver: &mut R,
	events: &flume::Sender<DelegatedEvent>,
) -> Result<TurnReport, Stop> {
	if cancel.is_cancelled() {
		return Err(Stop::Keep(ExecutorError::Cancelled { executor: ctx.executor.clone() }));
	}
	let mut total = deadline(Instant::now(), limits.timeout);
	let accepted = match timeout(limits.idle, session.start_turn(input)).await {
		Ok(Ok(accepted)) => accepted,
		Ok(Err(error)) => return Err(Stop::End(error)),
		Err(_) => return Err(hung(ctx, limits)),
	};
	if accepted.forwarded != input.prompt.digest() {
		return Err(Stop::End(ExecutorError::PromptRewritten {
			executor:  ctx.executor.clone(),
			supplied:  input.prompt.digest(),
			forwarded: accepted.forwarded,
		}));
	}
	let turn = accepted.turn;
	let mut report = TurnReport {
		turn:        turn.clone(),
		end:         TurnEnd::Completed,
		usage:       Usage::default(),
		permissions: PermissionTally::default(),
	};

	loop {
		let event = match next(session, cancel, total, limits.idle).await {
			Wake::Event(Ok(event)) => event,
			Wake::Event(Err(error)) => return Err(Stop::End(error)),
			Wake::Cancelled => return Err(interrupt_turn(ctx, session, limits, events).await),
			Wake::Idle => return Err(hung(ctx, limits)),
			Wake::Total => {
				return Err(Stop::End(ExecutorError::TimedOut {
					executor: ctx.executor.clone(),
					limit:    limits.timeout,
				}));
			},
		};
		check(ctx, &turn, &event)?;
		forward(events, event.clone()).await;
		match event {
			DelegatedEvent::Permission(request) => {
				let waited = Instant::now();
				let answer = tokio::select! {
					biased;
					() = cancel.cancelled() => {
						return Err(interrupt_turn(ctx, session, limits, events).await);
					},
					answer = decide(ctx.policy, resolver, &request, &mut report.permissions) => answer,
				};
				total = total.checked_add(waited.elapsed()).unwrap_or(total);
				match timeout(limits.idle, session.answer(&request.prompt, answer)).await {
					Ok(Ok(())) => {},
					Ok(Err(error)) => return Err(Stop::End(error)),
					Err(_) => return Err(hung(ctx, limits)),
				}
			},
			DelegatedEvent::Usage { usage, .. } => report.usage = usage,
			DelegatedEvent::Lifecycle(LifecycleEvent::TurnCompleted { outcome, .. }) => {
				report.end = outcome;
				return Ok(report);
			},
			DelegatedEvent::Failure { failure, .. } => return Err(remote(ctx, &turn, failure)),
			DelegatedEvent::Lifecycle(_)
			| DelegatedEvent::AgentTextDelta { .. }
			| DelegatedEvent::AgentMessage { .. }
			| DelegatedEvent::Tool(_) => {},
		}
	}
}

/// Turns a permission request into the answer the CLI gets. Every path that is
/// not an exact, audited, mappable approval is a denial.
async fn decide<R: PermissionResolver>(
	policy: DelegatedPolicy,
	resolver: &mut R,
	request: &PermissionRequest,
	tally: &mut PermissionTally,
) -> PermissionAnswer {
	if let Err(reason) = admit_request(policy, request) {
		tracing::warn!(%reason, kind = %request.kind, "delegated permission refused by policy");
		tally.refused_by_policy += 1;
		return PermissionAnswer::Deny;
	}
	tally.asked += 1;
	let decision = resolver.resolve(request).await;
	match map_decision(request, &decision) {
		Ok(PermissionAnswer::Deny) => {
			tally.denied += 1;
			PermissionAnswer::Deny
		},
		Ok(allow) => {
			tally.allowed += 1;
			allow
		},
		Err(reason) => {
			tracing::warn!(%reason, kind = %request.kind, "delegated approval cannot be forwarded");
			tally.refused_by_policy += 1;
			PermissionAnswer::Deny
		},
	}
}

/// Cooperative interrupt, then forced teardown. Always ends the turn as
/// cancelled; the session survives only if the CLI settled in time.
async fn interrupt_turn<S: DelegatedSession>(
	ctx: &Ctx<'_>,
	session: &mut S,
	limits: TurnLimits,
	events: &flume::Sender<DelegatedEvent>,
) -> Stop {
	let cancelled = ExecutorError::Cancelled { executor: ctx.executor.clone() };
	let settled = ctx.features.contains(Feature::Interrupt)
		&& matches!(timeout(limits.interrupt_grace, settle(session, events)).await, Ok(Ok(())));
	if settled { Stop::Keep(cancelled) } else { Stop::End(cancelled) }
}

async fn settle<S: DelegatedSession>(
	session: &mut S,
	events: &flume::Sender<DelegatedEvent>,
) -> Result<(), ExecutorError> {
	session.interrupt().await?;
	loop {
		let event = session.next_event().await?;
		let done = matches!(
			event,
			DelegatedEvent::Lifecycle(LifecycleEvent::TurnCompleted { .. })
				| DelegatedEvent::Failure { .. }
		);
		forward(events, event).await;
		if done {
			return Ok(());
		}
	}
}
