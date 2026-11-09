//! The OMP policy a delegated session is held to.
//!
//! The three axes mirror OMP's own posture vocabulary
//! (`omp_envd::exec_settings::ExecSandboxMode`,
//! `omp_envd::exec_settings::SandboxNetworkMode`,
//! `omp_envd::admission::ApprovalMode`). They are redefined here because
//! `omp-envd` depends on this crate, not the other way round; the driver
//! converts, after applying `omp_envd::admission::effective_approval_mode`
//! (which clamps an explicit `yolo` in an untrusted workspace).

/// How far the delegated agent may reach into the filesystem.
#[derive(
	Clone, Copy, Debug, Eq, Hash, PartialEq, strum::Display, strum::EnumString, strum::IntoStaticStr,
)]
#[strum(serialize_all = "kebab-case")]
pub enum SandboxPosture {
	/// The agent may not write anywhere (`ExecSandboxMode::ReadOnly`).
	ReadOnly,
	/// The agent may write inside the workspace
	/// (`ExecSandboxMode::WorkspaceWrite`).
	WorkspaceWrite,
	/// No filesystem confinement (`ExecSandboxMode::Off`).
	Unrestricted,
}

/// How far the delegated agent may reach into the network.
#[derive(
	Clone, Copy, Debug, Eq, Hash, PartialEq, strum::Display, strum::EnumString, strum::IntoStaticStr,
)]
#[strum(serialize_all = "kebab-case")]
pub enum NetworkPosture {
	/// No IP networking (`SandboxNetworkMode::Disabled`).
	Disabled,
	/// Egress only through OMP's policy-enforcing broker
	/// (`SandboxNetworkMode::Scoped`).
	Scoped,
	/// Unfiltered egress (`SandboxNetworkMode::Open`).
	Open,
}

/// When the delegated agent must ask before acting.
#[derive(
	Clone, Copy, Debug, Eq, Hash, PartialEq, strum::Display, strum::EnumString, strum::IntoStaticStr,
)]
#[strum(serialize_all = "kebab-case")]
pub enum ApprovalPosture {
	/// Ask before everything but reads (`ApprovalMode::AlwaysAsk`).
	AlwaysAsk,
	/// Workspace writes proceed; execution asks (`ApprovalMode::Write`).
	Write,
	/// Nothing asks (`ApprovalMode::Yolo`).
	Yolo,
}

/// The complete policy one delegated session runs under.
///
/// It is negotiated once, when the session opens, and then verified: the
/// backend reports the policy it actually applies and any difference from the
/// request ends the session.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct DelegatedPolicy {
	/// Filesystem reach.
	pub sandbox:  SandboxPosture,
	/// Network reach.
	pub network:  NetworkPosture,
	/// Approval behavior.
	pub approval: ApprovalPosture,
}

impl DelegatedPolicy {
	/// The most restrictive policy: read-only, offline, ask for everything.
	pub const STRICTEST: Self = Self {
		sandbox:  SandboxPosture::ReadOnly,
		network:  NetworkPosture::Disabled,
		approval: ApprovalPosture::AlwaysAsk,
	};

	/// Creates a policy from its three axes.
	#[must_use]
	pub const fn new(
		sandbox: SandboxPosture,
		network: NetworkPosture,
		approval: ApprovalPosture,
	) -> Self {
		Self { sandbox, network, approval }
	}
}
