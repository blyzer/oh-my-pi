//! ACP policy: the only mappable policy is [`DelegatedPolicy::STRICTEST`].
//!
//! ACP v1 provides no sandbox parameter: the agent runs under whatever
//! sandboxing it configured itself.  OMP can only observe that `session/new`
//! returns no sandbox description and cannot verify anything beyond the
//! handshake.  The only policy OMP can *prove* is maintained is the most
//! conservative one:
//!
//! - `sandbox: read-only` — declared, not enforced by OMP (the agent enforces
//!   it); the host asks for approvals on everything.
//! - `network: disabled` — likewise declared.
//! - `approval: always-ask` — OMP always routes permissions through the
//!   resolver; no grant is cached.
//!
//! Any looser policy (`workspace-write`, `network-open`, `write`, `yolo`) is
//! returned as [`ExecutorError::PolicyUnmappable`].

use omp_agent::delegated::{ApprovalPosture, DelegatedPolicy, NetworkPosture, SandboxPosture};

/// The one policy ACP can honestly represent.
pub const ACP_POLICY: DelegatedPolicy = DelegatedPolicy::STRICTEST;

/// Every policy the ACP executor can enforce (exactly one).
pub fn supported_policies() -> Box<[DelegatedPolicy]> {
	Box::new([ACP_POLICY])
}

/// Whether `policy` is the one this executor can enforce.
pub fn is_supported(policy: DelegatedPolicy) -> bool {
	policy.sandbox == SandboxPosture::ReadOnly
		&& policy.network == NetworkPosture::Disabled
		&& policy.approval == ApprovalPosture::AlwaysAsk
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn exactly_one_policy_supported() {
		let supported = supported_policies();
		assert_eq!(supported.len(), 1);
		assert_eq!(supported[0], DelegatedPolicy::STRICTEST);
	}

	#[test]
	fn strictest_is_supported() {
		assert!(is_supported(DelegatedPolicy::STRICTEST));
	}

	#[test]
	fn looser_policies_are_not_supported() {
		use omp_agent::delegated::*;
		let workspace = DelegatedPolicy::new(
			SandboxPosture::WorkspaceWrite,
			NetworkPosture::Disabled,
			ApprovalPosture::AlwaysAsk,
		);
		assert!(!is_supported(workspace));

		let network = DelegatedPolicy::new(
			SandboxPosture::ReadOnly,
			NetworkPosture::Open,
			ApprovalPosture::AlwaysAsk,
		);
		assert!(!is_supported(network));

		let yolo = DelegatedPolicy::new(
			SandboxPosture::ReadOnly,
			NetworkPosture::Disabled,
			ApprovalPosture::Yolo,
		);
		assert!(!is_supported(yolo));
	}
}
