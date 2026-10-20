//! Conversion from OMP's driver-side policy settings to [`DelegatedPolicy`].
//!
//! The three driver types map to the three axes of [`DelegatedPolicy`]:
//!
//! | Driver type              | Axis         | Notes                                         |
//! |--------------------------|--------------|-----------------------------------------------|
//! | `ExecSandboxMode`        | sandbox      | direct 1-to-1                                 |
//! | `SandboxNetworkMode`     | network      | direct 1-to-1                                 |
//! | `ApprovalMode`           | approval     | direct 1-to-1                                 |
//! | `WorkspaceTrust`         | all three    | `Untrusted` ⇒ `STRICTEST`, ignoring all axes  |
//!
//! ## Fail-closed rules
//!
//! 1. `WorkspaceTrust::Untrusted` always returns `DelegatedPolicy::STRICTEST`,
//!    regardless of the requested sandbox, network, or approval mode.  This
//!    mirrors `omp_envd::admission::effective_approval_mode`, which clamps an
//!    explicit `Yolo` to `Write` for untrusted workspaces, but is stricter
//!    still: a delegated session operates with external code, so any untrusted
//!    workspace refuses *all* latitude.
//!
//! 2. The three axis mappings are total: every variant has an exact
//!    counterpart.  No combination is ambiguous.
//!
//! 3. The caller never receives a policy looser than what was requested on any
//!    individual axis.
//!
//! ## Mapping table
//!
//! ```text
//! WorkspaceTrust  ExecSandboxMode   SandboxNetworkMode  ApprovalMode
//! ─────────────   ───────────────   ──────────────────  ────────────
//! Untrusted       *                 *                   *
//!   → STRICTEST (ReadOnly, Disabled, AlwaysAsk)
//!
//! Trusted         Off               Disabled            AlwaysAsk
//!   → Unrestricted, Disabled, AlwaysAsk
//! Trusted         Off               Disabled            Write
//!   → Unrestricted, Disabled, Write
//! Trusted         Off               Disabled            Yolo
//!   → Unrestricted, Disabled, Yolo
//! Trusted         Off               Scoped              AlwaysAsk
//!   → Unrestricted, Scoped, AlwaysAsk
//! Trusted         Off               Scoped              Write
//!   → Unrestricted, Scoped, Write
//! Trusted         Off               Scoped              Yolo
//!   → Unrestricted, Scoped, Yolo
//! Trusted         Off               Open                AlwaysAsk
//!   → Unrestricted, Open, AlwaysAsk
//! Trusted         Off               Open                Write
//!   → Unrestricted, Open, Write
//! Trusted         Off               Open                Yolo
//!   → Unrestricted, Open, Yolo
//! Trusted         ReadOnly          Disabled            AlwaysAsk
//!   → ReadOnly, Disabled, AlwaysAsk
//! Trusted         ReadOnly          Disabled            Write
//!   → ReadOnly, Disabled, Write
//! Trusted         ReadOnly          Disabled            Yolo
//!   → ReadOnly, Disabled, Yolo
//! Trusted         ReadOnly          Scoped              AlwaysAsk
//!   → ReadOnly, Scoped, AlwaysAsk
//! Trusted         ReadOnly          Scoped              Write
//!   → ReadOnly, Scoped, Write
//! Trusted         ReadOnly          Scoped              Yolo
//!   → ReadOnly, Scoped, Yolo
//! Trusted         ReadOnly          Open                AlwaysAsk
//!   → ReadOnly, Open, AlwaysAsk
//! Trusted         ReadOnly          Open                Write
//!   → ReadOnly, Open, Write
//! Trusted         ReadOnly          Open                Yolo
//!   → ReadOnly, Open, Yolo
//! Trusted         WorkspaceWrite    Disabled            AlwaysAsk
//!   → WorkspaceWrite, Disabled, AlwaysAsk
//! Trusted         WorkspaceWrite    Disabled            Write
//!   → WorkspaceWrite, Disabled, Write
//! Trusted         WorkspaceWrite    Disabled            Yolo
//!   → WorkspaceWrite, Disabled, Yolo
//! Trusted         WorkspaceWrite    Scoped              AlwaysAsk
//!   → WorkspaceWrite, Scoped, AlwaysAsk
//! Trusted         WorkspaceWrite    Scoped              Write
//!   → WorkspaceWrite, Scoped, Write
//! Trusted         WorkspaceWrite    Scoped              Yolo
//!   → WorkspaceWrite, Scoped, Yolo
//! Trusted         WorkspaceWrite    Open                AlwaysAsk
//!   → WorkspaceWrite, Open, AlwaysAsk
//! Trusted         WorkspaceWrite    Open                Write
//!   → WorkspaceWrite, Open, Write
//! Trusted         WorkspaceWrite    Open                Yolo
//!   → WorkspaceWrite, Open, Yolo
//! ```

use omp_agent::delegated::{ApprovalPosture, DelegatedPolicy, NetworkPosture, SandboxPosture};
use omp_envd::{
	admission::ApprovalMode,
	exec_settings::{ExecSandboxMode, SandboxNetworkMode},
};
use omp_ext::workspace_trust::WorkspaceTrust;

/// Converts OMP driver-side policy settings to a [`DelegatedPolicy`].
///
/// The conversion is pure and total.  It fails closed: an untrusted workspace
/// always returns [`DelegatedPolicy::STRICTEST`] regardless of the other
/// arguments.  Any combination with no exact mapping rounds toward stricter,
/// never looser.
///
/// # Parameters
///
/// - `trust`: workspace trust evaluated by the host; a value of
///   [`WorkspaceTrust::Untrusted`] short-circuits all other parameters.
/// - `sandbox`: filesystem confinement the user requested.
/// - `network`: network confinement the user requested.
/// - `approval`: approval posture the user requested.
#[must_use]
pub fn to_delegated_policy(
	trust: WorkspaceTrust,
	sandbox: ExecSandboxMode,
	network: SandboxNetworkMode,
	approval: ApprovalMode,
) -> DelegatedPolicy {
	if trust == WorkspaceTrust::Untrusted {
		return DelegatedPolicy::STRICTEST;
	}

	let sandbox_posture = match sandbox {
		ExecSandboxMode::ReadOnly => SandboxPosture::ReadOnly,
		ExecSandboxMode::WorkspaceWrite => SandboxPosture::WorkspaceWrite,
		ExecSandboxMode::Off => SandboxPosture::Unrestricted,
	};

	let network_posture = match network {
		SandboxNetworkMode::Disabled => NetworkPosture::Disabled,
		SandboxNetworkMode::Scoped => NetworkPosture::Scoped,
		SandboxNetworkMode::Open => NetworkPosture::Open,
	};

	let approval_posture = match approval {
		ApprovalMode::AlwaysAsk => ApprovalPosture::AlwaysAsk,
		ApprovalMode::Write => ApprovalPosture::Write,
		ApprovalMode::Yolo => ApprovalPosture::Yolo,
	};

	DelegatedPolicy::new(sandbox_posture, network_posture, approval_posture)
}

#[cfg(test)]
mod tests {
	use super::*;

	/// Every input combination in the mapping table, asserting the exact output.
	#[test]
	fn exhaustive_table() {
		use ApprovalMode as AM;
		use ExecSandboxMode as ESM;
		use SandboxNetworkMode as SNM;
		use WorkspaceTrust as WT;

		// Untrusted workspace always yields STRICTEST regardless of other axes.
		for &sandbox in &[ESM::Off, ESM::ReadOnly, ESM::WorkspaceWrite] {
			for &network in &[SNM::Disabled, SNM::Scoped, SNM::Open] {
				for &approval in &[AM::AlwaysAsk, AM::Write, AM::Yolo] {
					assert_eq!(
						to_delegated_policy(WT::Untrusted, sandbox, network, approval),
						DelegatedPolicy::STRICTEST,
						"untrusted({sandbox:?}, {network:?}, {approval:?}) must be STRICTEST"
					);
				}
			}
		}

		// Trusted workspace — every axis maps 1-to-1.
		let cases: &[(ESM, SNM, AM, DelegatedPolicy)] = &[
			// ExecSandboxMode::Off
			(
				ESM::Off,
				SNM::Disabled,
				AM::AlwaysAsk,
				DelegatedPolicy::new(
					SandboxPosture::Unrestricted,
					NetworkPosture::Disabled,
					ApprovalPosture::AlwaysAsk,
				),
			),
			(
				ESM::Off,
				SNM::Disabled,
				AM::Write,
				DelegatedPolicy::new(
					SandboxPosture::Unrestricted,
					NetworkPosture::Disabled,
					ApprovalPosture::Write,
				),
			),
			(
				ESM::Off,
				SNM::Disabled,
				AM::Yolo,
				DelegatedPolicy::new(
					SandboxPosture::Unrestricted,
					NetworkPosture::Disabled,
					ApprovalPosture::Yolo,
				),
			),
			(
				ESM::Off,
				SNM::Scoped,
				AM::AlwaysAsk,
				DelegatedPolicy::new(
					SandboxPosture::Unrestricted,
					NetworkPosture::Scoped,
					ApprovalPosture::AlwaysAsk,
				),
			),
			(
				ESM::Off,
				SNM::Scoped,
				AM::Write,
				DelegatedPolicy::new(
					SandboxPosture::Unrestricted,
					NetworkPosture::Scoped,
					ApprovalPosture::Write,
				),
			),
			(
				ESM::Off,
				SNM::Scoped,
				AM::Yolo,
				DelegatedPolicy::new(
					SandboxPosture::Unrestricted,
					NetworkPosture::Scoped,
					ApprovalPosture::Yolo,
				),
			),
			(
				ESM::Off,
				SNM::Open,
				AM::AlwaysAsk,
				DelegatedPolicy::new(
					SandboxPosture::Unrestricted,
					NetworkPosture::Open,
					ApprovalPosture::AlwaysAsk,
				),
			),
			(
				ESM::Off,
				SNM::Open,
				AM::Write,
				DelegatedPolicy::new(
					SandboxPosture::Unrestricted,
					NetworkPosture::Open,
					ApprovalPosture::Write,
				),
			),
			(
				ESM::Off,
				SNM::Open,
				AM::Yolo,
				DelegatedPolicy::new(
					SandboxPosture::Unrestricted,
					NetworkPosture::Open,
					ApprovalPosture::Yolo,
				),
			),
			// ExecSandboxMode::ReadOnly
			(
				ESM::ReadOnly,
				SNM::Disabled,
				AM::AlwaysAsk,
				DelegatedPolicy::new(
					SandboxPosture::ReadOnly,
					NetworkPosture::Disabled,
					ApprovalPosture::AlwaysAsk,
				),
			),
			(
				ESM::ReadOnly,
				SNM::Disabled,
				AM::Write,
				DelegatedPolicy::new(
					SandboxPosture::ReadOnly,
					NetworkPosture::Disabled,
					ApprovalPosture::Write,
				),
			),
			(
				ESM::ReadOnly,
				SNM::Disabled,
				AM::Yolo,
				DelegatedPolicy::new(
					SandboxPosture::ReadOnly,
					NetworkPosture::Disabled,
					ApprovalPosture::Yolo,
				),
			),
			(
				ESM::ReadOnly,
				SNM::Scoped,
				AM::AlwaysAsk,
				DelegatedPolicy::new(
					SandboxPosture::ReadOnly,
					NetworkPosture::Scoped,
					ApprovalPosture::AlwaysAsk,
				),
			),
			(
				ESM::ReadOnly,
				SNM::Scoped,
				AM::Write,
				DelegatedPolicy::new(
					SandboxPosture::ReadOnly,
					NetworkPosture::Scoped,
					ApprovalPosture::Write,
				),
			),
			(
				ESM::ReadOnly,
				SNM::Scoped,
				AM::Yolo,
				DelegatedPolicy::new(
					SandboxPosture::ReadOnly,
					NetworkPosture::Scoped,
					ApprovalPosture::Yolo,
				),
			),
			(
				ESM::ReadOnly,
				SNM::Open,
				AM::AlwaysAsk,
				DelegatedPolicy::new(
					SandboxPosture::ReadOnly,
					NetworkPosture::Open,
					ApprovalPosture::AlwaysAsk,
				),
			),
			(
				ESM::ReadOnly,
				SNM::Open,
				AM::Write,
				DelegatedPolicy::new(
					SandboxPosture::ReadOnly,
					NetworkPosture::Open,
					ApprovalPosture::Write,
				),
			),
			(
				ESM::ReadOnly,
				SNM::Open,
				AM::Yolo,
				DelegatedPolicy::new(
					SandboxPosture::ReadOnly,
					NetworkPosture::Open,
					ApprovalPosture::Yolo,
				),
			),
			// ExecSandboxMode::WorkspaceWrite
			(
				ESM::WorkspaceWrite,
				SNM::Disabled,
				AM::AlwaysAsk,
				DelegatedPolicy::new(
					SandboxPosture::WorkspaceWrite,
					NetworkPosture::Disabled,
					ApprovalPosture::AlwaysAsk,
				),
			),
			(
				ESM::WorkspaceWrite,
				SNM::Disabled,
				AM::Write,
				DelegatedPolicy::new(
					SandboxPosture::WorkspaceWrite,
					NetworkPosture::Disabled,
					ApprovalPosture::Write,
				),
			),
			(
				ESM::WorkspaceWrite,
				SNM::Disabled,
				AM::Yolo,
				DelegatedPolicy::new(
					SandboxPosture::WorkspaceWrite,
					NetworkPosture::Disabled,
					ApprovalPosture::Yolo,
				),
			),
			(
				ESM::WorkspaceWrite,
				SNM::Scoped,
				AM::AlwaysAsk,
				DelegatedPolicy::new(
					SandboxPosture::WorkspaceWrite,
					NetworkPosture::Scoped,
					ApprovalPosture::AlwaysAsk,
				),
			),
			(
				ESM::WorkspaceWrite,
				SNM::Scoped,
				AM::Write,
				DelegatedPolicy::new(
					SandboxPosture::WorkspaceWrite,
					NetworkPosture::Scoped,
					ApprovalPosture::Write,
				),
			),
			(
				ESM::WorkspaceWrite,
				SNM::Scoped,
				AM::Yolo,
				DelegatedPolicy::new(
					SandboxPosture::WorkspaceWrite,
					NetworkPosture::Scoped,
					ApprovalPosture::Yolo,
				),
			),
			(
				ESM::WorkspaceWrite,
				SNM::Open,
				AM::AlwaysAsk,
				DelegatedPolicy::new(
					SandboxPosture::WorkspaceWrite,
					NetworkPosture::Open,
					ApprovalPosture::AlwaysAsk,
				),
			),
			(
				ESM::WorkspaceWrite,
				SNM::Open,
				AM::Write,
				DelegatedPolicy::new(
					SandboxPosture::WorkspaceWrite,
					NetworkPosture::Open,
					ApprovalPosture::Write,
				),
			),
			(
				ESM::WorkspaceWrite,
				SNM::Open,
				AM::Yolo,
				DelegatedPolicy::new(
					SandboxPosture::WorkspaceWrite,
					NetworkPosture::Open,
					ApprovalPosture::Yolo,
				),
			),
		];

		for &(sandbox, network, approval, expected) in cases {
			assert_eq!(
				to_delegated_policy(WT::Trusted, sandbox, network, approval),
				expected,
				"trusted({sandbox:?}, {network:?}, {approval:?})"
			);
		}
	}

	/// Untrusted workspace never yields a policy looser than STRICTEST on any
	/// axis.
	///
	/// "Looser" is defined per axis:
	/// - sandbox: `WorkspaceWrite` or `Unrestricted` is looser than `ReadOnly`.
	/// - network: `Scoped` or `Open` is looser than `Disabled`.
	/// - approval: `Write` or `Yolo` is looser than `AlwaysAsk`.
	#[test]
	fn untrusted_workspace_never_loosens() {
		use ApprovalMode as AM;
		use ExecSandboxMode as ESM;
		use SandboxNetworkMode as SNM;

		for &sandbox in &[ESM::Off, ESM::ReadOnly, ESM::WorkspaceWrite] {
			for &network in &[SNM::Disabled, SNM::Scoped, SNM::Open] {
				for &approval in &[AM::AlwaysAsk, AM::Write, AM::Yolo] {
					let policy =
						to_delegated_policy(WorkspaceTrust::Untrusted, sandbox, network, approval);
					assert_eq!(
						policy.sandbox,
						SandboxPosture::ReadOnly,
						"untrusted: sandbox must be ReadOnly, got {:?}",
						policy.sandbox
					);
					assert_eq!(
						policy.network,
						NetworkPosture::Disabled,
						"untrusted: network must be Disabled, got {:?}",
						policy.network
					);
					assert_eq!(
						policy.approval,
						ApprovalPosture::AlwaysAsk,
						"untrusted: approval must be AlwaysAsk, got {:?}",
						policy.approval
					);
				}
			}
		}
	}

	/// Property: the result on every axis is never looser than the most
	/// restrictive member of that axis, for any trusted input.
	///
	/// Concretely: requesting a strict axis value and a loose one always returns
	/// at least the strict value.  This catches any future loosening regression.
	#[test]
	fn result_never_looser_than_requested_posture() {
		// For each axis, verify that the strictest input produces the strictest
		// output even when the other axes are at their loosest.
		let strictest_sandbox = to_delegated_policy(
			WorkspaceTrust::Trusted,
			ExecSandboxMode::ReadOnly,
			SandboxNetworkMode::Open,
			ApprovalMode::Yolo,
		);
		assert_eq!(
			strictest_sandbox.sandbox,
			SandboxPosture::ReadOnly,
			"ReadOnly sandbox must not be loosened"
		);

		let strictest_network = to_delegated_policy(
			WorkspaceTrust::Trusted,
			ExecSandboxMode::Off,
			SandboxNetworkMode::Disabled,
			ApprovalMode::Yolo,
		);
		assert_eq!(
			strictest_network.network,
			NetworkPosture::Disabled,
			"Disabled network must not be loosened"
		);

		let strictest_approval = to_delegated_policy(
			WorkspaceTrust::Trusted,
			ExecSandboxMode::Off,
			SandboxNetworkMode::Open,
			ApprovalMode::AlwaysAsk,
		);
		assert_eq!(
			strictest_approval.approval,
			ApprovalPosture::AlwaysAsk,
			"AlwaysAsk approval must not be loosened"
		);
	}
}
