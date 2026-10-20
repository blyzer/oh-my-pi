//! Production [`PermissionResolver`] that files each delegated permission
//! request on an OMP [`ApprovalRoute`] and maps the journaled decision back.
//!
//! [`RoutePermissionResolver`] is the production implementation. It wraps
//! exactly one `ApprovalRoute`; the route is typically the same one that
//! services native-tool approval prompts for the enclosing kernel, so the
//! host actor, hooks, and session grants decide delegated prompts by the same
//! rules as every other tool.
//!
//! # Fail-closed guarantee
//!
//! Every path that is not an explicit, answerable approval denies:
//! - Cancellation of the enclosing turn cancels the pending ticket; the
//!   `unreachable` field is set to `"deny"`, so
//!   `ApprovalTicket::unreachable_decision` returns `approved: false`.
//! - A dropped or disconnected route produces the same result through the same
//!   mechanism.
//! - An unattended session (no host actor connected to the inbox) also drops
//!   the reply sender, causing the same denial path.
//! - `ApprovalTicket::timeout_decision` denies for a ticket whose reasons
//!   declare no explicit `true` default, which is the case for every spec this
//!   resolver files (it sets `default: Some(false)`).
//!
//! # Single-use narrowing
//!
//! The Codex adapter always sets `session_grant: false` on the
//! [`PermissionRequest`] it raises, because no answer it sends outlives the
//! action it was asked about. This resolver follows the same convention: it
//! never offers `session` as a scope and clamps the answer to `AllowOnce`,
//! regardless of what the OMP decision grants, by omitting `"session"` from
//! `scopes`. The `map_decision` function in `omp_agent::delegated::permission`
//! then maps any `Session` or `Persist` scope to `AllowOnce` when
//! `request.session_grant` is `false`, which is exactly what the Codex adapter
//! relies on.

use omp_agent::{
	ApprovalDecision, ApprovalRoute, ApprovalScope, ApprovalSource, ApprovalSpec,
	delegated::{PermissionKind, PermissionRequest, PermissionResolver},
};
use omp_core::{Str, sf};
use tokio_util::sync::CancellationToken;

/// Converts a delegated [`PermissionRequest`] to an OMP approval ticket via
/// `route` and maps the decision back.
///
/// The wait time is not accounted for here; `drive.rs` excludes it from the
/// turn timeout by recording the wall time before and after calling
/// `resolver.resolve` and adding the elapsed duration back to the total
/// deadline.
pub struct RoutePermissionResolver {
	route:  ApprovalRoute,
	cancel: CancellationToken,
}

impl RoutePermissionResolver {
	/// Creates a resolver that files prompts on `route`.
	///
	/// `cancel` is cancelled when the enclosing turn is cancelled; the pending
	/// ticket is then withdrawn (via the `PendingGuard` inside
	/// `request_cancellable`) and the decision it returns is an
	/// `unreachable_decision` that denies.
	#[must_use]
	pub fn new(route: ApprovalRoute, cancel: CancellationToken) -> Self {
		Self { route, cancel }
	}
}

impl PermissionResolver for RoutePermissionResolver {
	fn resolve<'a>(
		&'a mut self,
		request: &'a PermissionRequest,
	) -> impl Future<Output = ApprovalDecision> + Send + 'a {
		async move {
			let spec = spec_from_request(request);
			let ticket = self
				.route
				.request_cancellable(
					request.call.as_ref().map(|id| Str::new(id.as_str())),
					vec![spec],
					epoch_millis(),
					self.cancel.clone(),
				)
				.await;
			decision_from_ticket(ticket)
		}
	}
}

/// Builds the single [`ApprovalSpec`] for a delegated permission request.
///
/// Rules:
/// - `scopes`: only `"once"` — session grants are not offered (single-use
///   narrowing; a `Session`/`Persist` decision maps to `AllowOnce` by
///   `map_decision` when `session_grant` is false, but we match what the Codex
///   adapter does and never even advertise session scope).
/// - `default`: `Some(false)` — timeout denies.
/// - `unreachable`: `"deny"` — cancellation, dropped route, and unattended
///   sessions deny.
/// - `require_human`: `true` — no automated extension can approve on behalf of
///   an external agent.
fn spec_from_request(request: &PermissionRequest) -> ApprovalSpec {
	let (title, body, kind_str) = match request.kind {
		PermissionKind::Read => (
			sf!("Allow read: {}", request.subject),
			sf!("Delegated agent requests read access to: {}", request.subject),
			"read",
		),
		PermissionKind::Write => (
			sf!("Allow write: {}", request.subject),
			sf!("Delegated agent requests write access to: {}", request.subject),
			"write",
		),
		PermissionKind::Exec => (
			sf!("Allow command: {}", request.subject),
			sf!("Delegated agent requests to run: {}", request.subject),
			"exec",
		),
		PermissionKind::Network => (
			sf!("Allow network: {}", request.subject),
			sf!("Delegated agent requests network access to: {}", request.subject),
			"network",
		),
		// Unclassified is refused by admit_request before resolve is called.
		PermissionKind::Unclassified => (
			sf!("Unclassified delegated request"),
			sf!("Delegated agent raised an unclassified request for: {}", request.subject),
			"unclassified",
		),
	};
	ApprovalSpec {
		title,
		body,
		subject: request.subject.clone(),
		kind: Str::new_static(kind_str),
		// Single-use only: no session scope advertised.
		scopes: vec![Str::new_static("once")],
		default: Some(false),
		route: Str::new_static("user"),
		approver: None,
		// No explicit timeout: the turn-level limits and route cancellation
		// cover the wait; an explicit timeout_ms would add a second race.
		timeout_ms: 0,
		unreachable: Str::new_static("deny"),
		require_human: true,
		pattern: None,
		evidence: vec![sf!(
			"delegated agent turn {} raised a {} permission request",
			request.turn.as_str(),
			request.kind
		)],
	}
}

/// Extracts the [`ApprovalDecision`] from a settled ticket.
///
/// `request_cancellable` always sets `ticket.decision` before returning (every
/// arm of its select either stores a decision or falls through to the
/// `ticket.decision = Some(decision)` assignment). The `unwrap_or_else` branch
/// is a defensive fallback that can never fire in practice; it denies.
fn decision_from_ticket(ticket: omp_agent::ApprovalTicket) -> ApprovalDecision {
	ticket.decision.unwrap_or_else(|| ApprovalDecision {
		approved:   false,
		scope:      ApprovalScope::Once,
		source:     ApprovalSource::Unavailable,
		decided_by: None,
		reason:     Some(Str::new_static("approval ticket settled without a decision")),
		audited:    false,
	})
}

fn epoch_millis() -> u64 {
	std::time::SystemTime::now()
		.duration_since(std::time::UNIX_EPOCH)
		.map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}
