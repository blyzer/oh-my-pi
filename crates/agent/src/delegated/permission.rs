//! Permission prompts and the fail-closed mapping onto OMP approvals.
//!
//! Two pure functions carry the whole policy:
//!
//! - [`admit_request`] refuses, *without asking anyone*, a request that the
//!   negotiated [`DelegatedPolicy`] already forbids or that cannot be
//!   classified.
//! - [`map_decision`] turns an OMP [`ApprovalDecision`] into the answer the CLI
//!   understands. It only ever **narrows** a grant (a wider OMP scope than the
//!   CLI can express becomes a single-use grant) and refuses what it cannot
//!   represent faithfully.
//!
//! Neither function can produce an answer broader than the one OMP gave.
//! Path-level confinement is the CLI sandbox's job; the host judges only the
//! class of effect (write, network, …).

use omp_core::Str;

use super::{CallId, DelegatedPolicy, NetworkPosture, PromptId, SandboxPosture, TurnId};
use crate::approvals::{ApprovalDecision, ApprovalScope, ApprovalSource};

/// The class of effect a permission prompt asks for.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, strum::Display, strum::IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
pub enum PermissionKind {
	/// Read outside what the agent already may read.
	Read,
	/// Modify files.
	Write,
	/// Run a command.
	Exec,
	/// Reach the network.
	Network,
	/// The CLI could not say what is being asked. Never grantable.
	Unclassified,
}

/// One permission prompt raised by the delegated CLI.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PermissionRequest {
	/// Prompt identity assigned by the CLI; echoed in the answer.
	pub prompt:        PromptId,
	/// The turn that raised it.
	pub turn:          TurnId,
	/// The tool call it gates, if known.
	pub call:          Option<CallId>,
	/// The class of effect requested.
	pub kind:          PermissionKind,
	/// Exact command, path, or host the agent wants to act on.
	pub subject:       Str,
	/// Whether the CLI can remember an approval for the rest of its session.
	pub session_grant: bool,
}

/// The answer sent back to the CLI.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, strum::Display, strum::IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
pub enum PermissionAnswer {
	/// Do not perform the action.
	Deny,
	/// Perform this action once.
	AllowOnce,
	/// Perform this action and remember the approval for the CLI session.
	AllowForSession,
}

/// Why the host refused a prompt on its own, without a user-facing answer.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, strum::Display, strum::IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
pub enum DenyReason {
	/// The session sandbox is read-only and the prompt asks to write.
	SandboxForbidsWrite,
	/// The session network posture is disabled and the prompt asks for network.
	NetworkDisabled,
	/// The CLI did not classify the request, so OMP cannot judge it.
	UnclassifiedRequest,
	/// OMP approved with a host-defined scope that has no CLI equivalent.
	UnmappableScope,
	/// OMP approved through a timeout or unavailable route without an audit
	/// record; a fail-open result is never forwarded unaudited.
	UnauditedFailOpen,
}

/// Decides, before any user is asked, whether `request` may be asked at all.
///
/// # Errors
///
/// Returns the reason the request is refused outright.
pub const fn admit_request(
	policy: DelegatedPolicy,
	request: &PermissionRequest,
) -> Result<(), DenyReason> {
	match request.kind {
		PermissionKind::Unclassified => Err(DenyReason::UnclassifiedRequest),
		PermissionKind::Write if matches!(policy.sandbox, SandboxPosture::ReadOnly) => {
			Err(DenyReason::SandboxForbidsWrite)
		},
		PermissionKind::Network if matches!(policy.network, NetworkPosture::Disabled) => {
			Err(DenyReason::NetworkDisabled)
		},
		_ => Ok(()),
	}
}

/// Maps an OMP approval decision onto the answer the CLI understands.
///
/// | OMP decision | Answer |
/// |---|---|
/// | not approved | [`PermissionAnswer::Deny`] |
/// | approved, scope `once`/`call`/`turn` | [`PermissionAnswer::AllowOnce`] |
/// | approved, scope `session`/`persist`, CLI can remember | [`PermissionAnswer::AllowForSession`] |
/// | approved, scope `session`/`persist`, CLI cannot remember | [`PermissionAnswer::AllowOnce`] |
/// | approved, host-defined scope | refused: [`DenyReason::UnmappableScope`] |
/// | approved by timeout/unavailable route, unaudited | refused: [`DenyReason::UnauditedFailOpen`] |
///
/// # Errors
///
/// Returns the reason the approval cannot be forwarded; the caller answers
/// [`PermissionAnswer::Deny`].
pub const fn map_decision(
	request: &PermissionRequest,
	decision: &ApprovalDecision,
) -> Result<PermissionAnswer, DenyReason> {
	if !decision.approved {
		return Ok(PermissionAnswer::Deny);
	}
	let fail_open = matches!(decision.source, ApprovalSource::Timeout | ApprovalSource::Unavailable);
	if fail_open && !decision.audited {
		return Err(DenyReason::UnauditedFailOpen);
	}
	match &decision.scope {
		ApprovalScope::Once | ApprovalScope::Call | ApprovalScope::Turn => {
			Ok(PermissionAnswer::AllowOnce)
		},
		ApprovalScope::Session | ApprovalScope::Persist if request.session_grant => {
			Ok(PermissionAnswer::AllowForSession)
		},
		ApprovalScope::Session | ApprovalScope::Persist => Ok(PermissionAnswer::AllowOnce),
		ApprovalScope::Custom(_) => Err(DenyReason::UnmappableScope),
	}
}
