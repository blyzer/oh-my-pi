//! Write approval.
//!
//! The router calls [`ApprovalGate::permit`] only when
//! [`omp_forge::ForgeOp::mutates`] is true. The operation argument is the
//! one the router selected for the method. A gate cannot reclassify it.

use omp_forge::{ForgeOp, RepoRef};

/// Decides whether one mutating forge operation may proceed.
///
/// The composer supplies the gate. [`PermitAll`] and [`DenyAll`] cover the
/// two constant policies.
pub trait ApprovalGate: Send + Sync + 'static {
	/// Whether `op` may run against `repo`.
	///
	/// `op` is a write. Returning `false` refuses the call before the
	/// adapter sees it.
	fn permit(&self, op: ForgeOp, repo: &RepoRef) -> bool;
}

/// Approves every write.
///
/// Reads never reach a gate. This policy is for tests and for a composer
/// that already approved the call.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PermitAll;

impl ApprovalGate for PermitAll {
	fn permit(&self, _op: ForgeOp, _repo: &RepoRef) -> bool {
		true
	}
}

/// Refuses every write.
///
/// Reads still run. A refused write does not reach the adapter.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DenyAll;

impl ApprovalGate for DenyAll {
	fn permit(&self, _op: ForgeOp, _repo: &RepoRef) -> bool {
		false
	}
}

/// Approval callback.
///
/// `F` returns whether the write may proceed. The router passes the
/// operation the method selected.
pub struct GateFn<F> {
	permit: F,
}

impl<F> GateFn<F>
where
	F: Fn(ForgeOp, &RepoRef) -> bool + Send + Sync + 'static,
{
	/// Wraps `permit` as a gate.
	#[must_use]
	pub const fn new(permit: F) -> Self {
		Self { permit }
	}
}

impl<F> ApprovalGate for GateFn<F>
where
	F: Fn(ForgeOp, &RepoRef) -> bool + Send + Sync + 'static,
{
	fn permit(&self, op: ForgeOp, repo: &RepoRef) -> bool {
		(self.permit)(op, repo)
	}
}
