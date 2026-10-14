//! Exact, fail-closed mapping between [`DelegatedPolicy`] and the Codex
//! `app-server` sandbox and approval vocabulary.
//!
//! [`wire_policy`] is the only place a policy becomes Codex settings, and
//! [`supported_policies`] is derived from it, so the policies the executor
//! advertises can never drift from the ones it can send.
//!
//! | OMP | Codex |
//! |---|---|
//! | sandbox `read-only`, network `disabled` | `sandbox: read-only` |
//! | sandbox `workspace-write`, network `disabled` | `workspace-write`, `network_access=false` |
//! | sandbox `workspace-write`, network `open` | `workspace-write`, `network_access=true` |
//! | approval `always-ask` | `untrusted` |
//! | approval `yolo` | `never` |
//!
//! Everything else is unmappable: `unrestricted` (never `danger-full-access`),
//! network `scoped` (Codex cannot route through OMP's broker), network `open`
//! under `read-only`, and approval `write` (Codex `on-request` lets the model
//! run commands inside the sandbox without asking, which is broader than
//! OMP's `write`).

use omp_agent::delegated::{ApprovalPosture, DelegatedPolicy, NetworkPosture, SandboxPosture};

use super::wire::{
	ApprovalPolicy, PlainApproval, Reviewer, SandboxMode, SandboxPolicy, ThreadConfig, ThreadOpened,
	WorkspaceWriteConfig,
};

/// A [`DelegatedPolicy`] expressed as Codex settings.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct WirePolicy {
	/// `approvalPolicy`.
	pub(crate) approval:     PlainApproval,
	/// `sandbox` on `thread/start` and `thread/resume`.
	pub(crate) sandbox:      SandboxMode,
	/// Whether commands may reach the network.
	pub(crate) network_open: bool,
}

impl WirePolicy {
	/// The reviewer every request pins.
	pub(crate) const REVIEWER: Reviewer = Reviewer::User;

	/// The `config` override that pins network access under workspace-write.
	pub(crate) const fn config(self) -> Option<ThreadConfig> {
		match self.sandbox {
			SandboxMode::WorkspaceWrite => Some(ThreadConfig {
				sandbox_workspace_write: WorkspaceWriteConfig { network_access: self.network_open },
			}),
			SandboxMode::ReadOnly => None,
		}
	}

	/// The `sandboxPolicy` sent on every `turn/start`.
	pub(crate) const fn turn_sandbox(self) -> SandboxPolicy {
		match self.sandbox {
			SandboxMode::ReadOnly => SandboxPolicy::ReadOnly { network_access: false },
			SandboxMode::WorkspaceWrite => SandboxPolicy::WorkspaceWrite {
				network_access: self.network_open,
				writable_roots: Vec::new(),
			},
		}
	}
}

/// Maps `policy` to Codex settings, or `None` when no exact mapping exists.
pub(crate) const fn wire_policy(policy: DelegatedPolicy) -> Option<WirePolicy> {
	let approval = match policy.approval {
		ApprovalPosture::AlwaysAsk => PlainApproval::Untrusted,
		ApprovalPosture::Yolo => PlainApproval::Never,
		ApprovalPosture::Write => return None,
	};
	let (sandbox, network_open) = match (policy.sandbox, policy.network) {
		(SandboxPosture::ReadOnly, NetworkPosture::Disabled) => (SandboxMode::ReadOnly, false),
		(SandboxPosture::WorkspaceWrite, NetworkPosture::Disabled) => {
			(SandboxMode::WorkspaceWrite, false)
		},
		(SandboxPosture::WorkspaceWrite, NetworkPosture::Open) => (SandboxMode::WorkspaceWrite, true),
		_ => return None,
	};
	Some(WirePolicy { approval, sandbox, network_open })
}

/// Every policy [`wire_policy`] can express.
pub(crate) fn supported_policies() -> Box<[DelegatedPolicy]> {
	const SANDBOXES: [SandboxPosture; 3] =
		[SandboxPosture::ReadOnly, SandboxPosture::WorkspaceWrite, SandboxPosture::Unrestricted];
	const NETWORKS: [NetworkPosture; 3] =
		[NetworkPosture::Disabled, NetworkPosture::Scoped, NetworkPosture::Open];
	const APPROVALS: [ApprovalPosture; 3] =
		[ApprovalPosture::AlwaysAsk, ApprovalPosture::Write, ApprovalPosture::Yolo];
	let mut out = Vec::new();
	for sandbox in SANDBOXES {
		for network in NETWORKS {
			for approval in APPROVALS {
				let policy = DelegatedPolicy::new(sandbox, network, approval);
				if wire_policy(policy).is_some() {
					out.push(policy);
				}
			}
		}
	}
	out.into_boxed_slice()
}

/// The policy a `thread/start` or `thread/resume` response says is in force,
/// or `None` when it cannot be expressed in OMP's vocabulary or is broader
/// than what any request asks for (another reviewer, extra writable roots,
/// granular approvals).
///
/// `Codex on-request` is reported as [`ApprovalPosture::Write`] so a server
/// that downgrades the request is caught as drift; no executor-supported
/// policy contains it.
pub(crate) fn applied_policy(opened: &ThreadOpened, workdir: &str) -> Option<DelegatedPolicy> {
	if opened.approvals_reviewer != Reviewer::User {
		return None;
	}
	let approval = match &opened.approval_policy {
		ApprovalPolicy::Plain(PlainApproval::Untrusted) => ApprovalPosture::AlwaysAsk,
		ApprovalPolicy::Plain(PlainApproval::OnRequest) => ApprovalPosture::Write,
		ApprovalPolicy::Plain(PlainApproval::Never) => ApprovalPosture::Yolo,
		ApprovalPolicy::Granular { .. } | ApprovalPolicy::Other(_) => return None,
	};
	let network = |open: bool| {
		if open {
			NetworkPosture::Open
		} else {
			NetworkPosture::Disabled
		}
	};
	let (sandbox, network) = match &opened.sandbox {
		SandboxPolicy::ReadOnly { network_access } => {
			(SandboxPosture::ReadOnly, network(*network_access))
		},
		SandboxPolicy::WorkspaceWrite { network_access, writable_roots } => {
			if writable_roots.iter().any(|root| root.as_str() != workdir) {
				return None;
			}
			(SandboxPosture::WorkspaceWrite, network(*network_access))
		},
		SandboxPolicy::DangerFullAccess => (SandboxPosture::Unrestricted, NetworkPosture::Open),
		SandboxPolicy::ExternalSandbox | SandboxPolicy::Unknown => return None,
	};
	Some(DelegatedPolicy::new(sandbox, network, approval))
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn exactly_the_documented_policies_are_supported() {
		let supported = supported_policies();
		assert_eq!(supported.len(), 6);
		for policy in &*supported {
			assert_ne!(policy.sandbox, SandboxPosture::Unrestricted);
			assert_ne!(policy.network, NetworkPosture::Scoped);
			assert_ne!(policy.approval, ApprovalPosture::Write);
		}
		assert!(!supported.contains(&DelegatedPolicy::new(
			SandboxPosture::ReadOnly,
			NetworkPosture::Open,
			ApprovalPosture::Yolo
		)));
	}
}
