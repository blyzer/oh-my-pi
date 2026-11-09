//! Approval posture shared by admission and the session composition.
//!
//! The environment still probes the live sandbox and applies the decision.
//! These types are the vocabulary both sides name.

use omp_core::{Str, sf};
use omp_ext::workspace_trust::WorkspaceTrust;
use omp_proto::env::v1::SandboxState as WireSandboxState;
use omp_tool::{Confinement, Effects};
use serde::{Deserialize, Serialize};

/// Default approval posture applied before one invocation reaches interactive
/// admission.
#[derive(
	Clone,
	Copy,
	Debug,
	Default,
	Deserialize,
	Eq,
	PartialEq,
	Serialize,
	strum::Display,
	strum::EnumString,
	strum::IntoStaticStr,
	strum::VariantNames,
)]
#[serde(rename_all = "kebab-case")]
#[strum(serialize_all = "kebab-case")]
pub enum ApprovalMode {
	/// Read-only effects proceed; fetches, writes and execution require
	/// confirmation.
	AlwaysAsk,
	/// Read, fetch and workspace-write effects proceed; execution requires
	/// confirmation.
	Write,
	/// Every declared tier proceeds unless a per-tool policy overrides it.
	#[default]
	Yolo,
}

omp_con::con_enum!(ApprovalMode);

/// Why a requested sandbox could not be constructed.
#[derive(
	Clone,
	Copy,
	Debug,
	Deserialize,
	Eq,
	PartialEq,
	Serialize,
	strum::Display,
	strum::EnumString,
	strum::IntoStaticStr,
)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum SandboxUnavailable {
	/// This operating system has no native command-backed sandbox.
	UnsupportedHost,
	/// The native backend is missing or failed its live probe.
	BackendUnavailable,
	/// The policy was refused rather than the platform; commands cannot start.
	PolicyRejected,
}

/// Whether a native sandbox was actually constructed for this environment.
///
/// The convar alone never decides this: only a sandbox that was compiled and
/// confines the filesystem counts as [`SandboxState::Active`].
#[derive(
	Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, strum::Display, strum::IntoStaticStr,
)]
#[serde(tag = "state", rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum SandboxState {
	/// A sandbox was constructed and confines spawned commands.
	Active,
	/// No filesystem sandbox was requested.
	Off,
	/// A sandbox was requested but could not be constructed.
	Unavailable {
		/// Why construction failed.
		cause: SandboxUnavailable,
	},
}

impl SandboxState {
	/// Whether a constructed sandbox confines this environment.
	#[must_use]
	pub const fn confines(self) -> bool {
		matches!(self, Self::Active)
	}

	/// The state as a `ServerHello` reports it (`sandbox_state`).
	#[must_use]
	pub const fn to_wire(self) -> WireSandboxState {
		match self {
			Self::Active => WireSandboxState::Active,
			Self::Off => WireSandboxState::Off,
			Self::Unavailable { cause: SandboxUnavailable::UnsupportedHost } => {
				WireSandboxState::UnsupportedHost
			},
			Self::Unavailable { cause: SandboxUnavailable::BackendUnavailable } => {
				WireSandboxState::BackendUnavailable
			},
			Self::Unavailable { cause: SandboxUnavailable::PolicyRejected } => {
				WireSandboxState::PolicyRejected
			},
		}
	}

	/// The state a `ServerHello` reports; `None` when it reports none.
	#[must_use]
	pub const fn from_wire(state: WireSandboxState) -> Option<Self> {
		match state {
			WireSandboxState::Unspecified => None,
			WireSandboxState::Active => Some(Self::Active),
			WireSandboxState::Off => Some(Self::Off),
			WireSandboxState::UnsupportedHost => {
				Some(Self::Unavailable { cause: SandboxUnavailable::UnsupportedHost })
			},
			WireSandboxState::BackendUnavailable => {
				Some(Self::Unavailable { cause: SandboxUnavailable::BackendUnavailable })
			},
			WireSandboxState::PolicyRejected => {
				Some(Self::Unavailable { cause: SandboxUnavailable::PolicyRejected })
			},
		}
	}
}

/// Whether the user chose a setting or it is the shipped default.
#[derive(
	Clone,
	Copy,
	Debug,
	Default,
	Deserialize,
	Eq,
	PartialEq,
	Serialize,
	strum::Display,
	strum::IntoStaticStr,
)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum Provenance {
	/// The registration default; nobody asked for it.
	#[default]
	Default,
	/// Set by the user: a flag, `config.cfg`, the session, or the agent class.
	Explicit,
}

impl Provenance {
	/// Reads a convar's provenance from the layers that hold it.
	#[must_use]
	pub fn of_convar(ctx: &omp_con::Ctx, name: &str) -> Self {
		if ctx.is_user_set(name) {
			Self::Explicit
		} else {
			Self::Default
		}
	}
}

/// An approval mode together with who asked for it.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct ConfiguredApproval {
	/// The mode configured.
	pub mode:            ApprovalMode,
	/// Whether the user chose it or it is the shipped default.
	pub provenance:      Provenance,
	/// Host-supplied workspace trust; project configuration cannot raise it.
	pub workspace_trust: WorkspaceTrust,
}

/// Resolves configured approval against workspace trust and the sandbox.
///
/// Explicit `Yolo` is clamped to `Write` in an untrusted workspace, even
/// inside a sandbox. Trusted explicit `Yolo` is respected. Default `Yolo`
/// requires an active sandbox; other modes are unchanged.
///
/// This is the session's posture, and the mode in force for the tools the
/// sandbox confines. [`call_approval_mode`] is the mode a call is admitted
/// under.
#[must_use]
pub const fn effective_approval_mode(
	configured: ConfiguredApproval,
	sandbox: SandboxState,
) -> ApprovalMode {
	match (configured.mode, configured.provenance) {
		(ApprovalMode::Yolo, Provenance::Explicit)
			if matches!(configured.workspace_trust, WorkspaceTrust::Untrusted) =>
		{
			ApprovalMode::Write
		},
		(ApprovalMode::Yolo, Provenance::Default) if !sandbox.confines() => ApprovalMode::Write,
		(mode, _) => mode,
	}
}

/// The mode a call to a tool of `confinement` is admitted under.
///
/// [`effective_approval_mode`] sees the sandbox only for a tool that sandbox
/// confines ([`Confinement::ExecSandbox`]). A [`Confinement::Host`] or
/// [`Confinement::DocumentAuthority`] tool sees none, so a defaulted `Yolo`
/// is `Write` for it even while the sandbox is active, and its mode is the
/// same under every sandbox state. Explicit `Yolo` is respected for all
/// variants only in a trusted workspace.
#[must_use]
pub const fn call_approval_mode(
	configured: ConfiguredApproval,
	sandbox: SandboxState,
	confinement: Confinement,
) -> ApprovalMode {
	let sandbox_for_call = if confinement.sandboxed() {
		sandbox
	} else {
		SandboxState::Off
	};
	effective_approval_mode(configured, sandbox_for_call)
}

/// Name of the typed notice reporting an [`ApprovalPosture`].
pub const APPROVAL_POSTURE_NOTICE: &str = "approval-posture";

/// Typed reason for reporting an approval posture.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalPostureReason {
	/// Explicit `yolo` is clamped because the workspace is untrusted.
	UntrustedWorkspace,
	/// Default `yolo` has no active sandbox.
	DefaultWithoutSandbox,
	/// Trusted explicit `yolo` runs without a sandbox.
	ExplicitWithoutSandbox,
}

/// A configured `yolo` posture requiring an operator-visible notice.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ApprovalPosture {
	/// Mode configured.
	pub configured:      ApprovalMode,
	/// Who configured it.
	pub provenance:      Provenance,
	/// Mode actually enforced.
	pub effective:       ApprovalMode,
	/// Sandbox state that decided it.
	pub sandbox:         SandboxState,
	/// Workspace trust that decided it.
	pub workspace_trust: WorkspaceTrust,
	/// Why this posture needs reporting.
	pub reason:          ApprovalPostureReason,
}

impl ApprovalPosture {
	/// Reports a trust clamp or a `yolo` without a confining sandbox.
	#[must_use]
	pub fn resolve(configured: ConfiguredApproval, sandbox: SandboxState) -> Option<Self> {
		let untrusted = configured.provenance == Provenance::Explicit
			&& configured.workspace_trust == WorkspaceTrust::Untrusted;
		(configured.mode == ApprovalMode::Yolo && (untrusted || !sandbox.confines())).then(|| Self {
			configured: configured.mode,
			provenance: configured.provenance,
			effective: effective_approval_mode(configured, sandbox),
			sandbox,
			workspace_trust: configured.workspace_trust,
			reason: if untrusted {
				ApprovalPostureReason::UntrustedWorkspace
			} else if configured.provenance == Provenance::Default {
				ApprovalPostureReason::DefaultWithoutSandbox
			} else {
				ApprovalPostureReason::ExplicitWithoutSandbox
			},
		})
	}

	/// Whether `yolo` was downgraded, rather than respected without a sandbox.
	#[must_use]
	pub fn downgraded(&self) -> bool {
		self.effective != self.configured
	}

	/// Fallback prose for projections that do not read the typed fields.
	#[must_use]
	pub fn body(&self) -> Str {
		if self.reason == ApprovalPostureReason::UntrustedWorkspace {
			return sf!(
				"Approval `yolo` was requested explicitly but the workspace is untrusted; `write` is \
				 in force."
			);
		}
		let effective: &'static str = self.effective.into();
		let sandbox: &'static str = self.sandbox.into();
		let cause = match self.sandbox {
			SandboxState::Unavailable { cause } => Some(cause),
			SandboxState::Active | SandboxState::Off => None,
		};
		match (self.downgraded(), cause) {
			(true, Some(cause)) => sf!(
				"Approval `yolo` is the default and needs an active sandbox; `{effective}` is in \
				 force (sandbox {sandbox}: {cause})."
			),
			(true, None) => sf!(
				"Approval `yolo` is the default and needs an active sandbox; `{effective}` is in \
				 force (sandbox {sandbox})."
			),
			(false, Some(cause)) => sf!(
				"Approval `yolo` was requested explicitly: commands run unconfined (sandbox \
				 {sandbox}: {cause})."
			),
			(false, None) => sf!(
				"Approval `yolo` was requested explicitly: commands run unconfined (sandbox \
				 {sandbox})."
			),
		}
	}
}

/// User policy for one named tool.
#[derive(
	Clone,
	Copy,
	Debug,
	Deserialize,
	Eq,
	PartialEq,
	Serialize,
	strum::Display,
	strum::EnumString,
	strum::IntoStaticStr,
)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum ApprovalPolicy {
	/// Proceed without an interactive decision.
	Allow,
	/// Refuse the invocation.
	Deny,
	/// Require an interactive durable decision.
	Prompt,
}

/// Conservative capability tier derived from a tool's declared [`Effects`].
#[derive(
	Clone,
	Copy,
	Debug,
	Deserialize,
	Eq,
	Ord,
	PartialEq,
	PartialOrd,
	Serialize,
	strum::Display,
	strum::IntoStaticStr,
)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum ApprovalTier {
	/// No mutation, process, inference, egress, or subagent effects.
	Read,
	/// Read-only network egress (a fetch) and nothing above it.
	Fetch,
	/// Declared document mutation without execution-class effects.
	Write,
	/// Process, network, inference, subagent, or desktop authority.
	Exec,
}

impl ApprovalTier {
	/// Resolves the highest approval tier present in `effects`.
	///
	/// Every desktop authority is `exec`, the reads (capture, accessibility,
	/// clipboard) as well as input: how desktop reads should be approved is an
	/// open owner question, and until it is answered a call that only reads
	/// the desktop asks exactly as every `computer` call did before calls were
	/// judged by their arguments.
	pub fn from_effects(effects: &Effects) -> Self {
		if effects.subagents != 0
			|| effects
				.exec
				.as_ref()
				.is_some_and(|effect| !effect.is_empty())
			|| effects
				.inference
				.as_ref()
				.is_some_and(|effect| !effect.is_empty())
			|| effects
				.desktop
				.as_ref()
				.is_some_and(|effect| !effect.is_empty())
		{
			Self::Exec
		} else if effects
			.documents
			.as_ref()
			.is_some_and(|effect| !effect.write_globs.is_empty())
		{
			Self::Write
		} else if effects.fetch.is_some() {
			Self::Fetch
		} else {
			Self::Read
		}
	}
}

/// Authority that selected one durable approval policy.
#[derive(
	Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, strum::Display, strum::IntoStaticStr,
)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum ApprovalSource {
	/// The active approval mode's tier ceiling.
	Mode,
	/// A named per-tool user override.
	User,
}

/// Stable per-invocation approval outcome suitable for the admission receipt.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ResolvedApproval {
	/// Stable invocation identity.
	pub invocation_id: Str,
	/// Exact live tool name evaluated.
	pub tool_name:     Str,
	/// Where the live revision's effects happen, as its host asserted.
	pub confinement:   Confinement,
	/// Tier derived from the call's effects: its argument-scoped envelope, else
	/// the live revision's declared maximum.
	pub tier:          ApprovalTier,
	/// Approval mode in force for this call.
	pub mode:          ApprovalMode,
	/// Effective policy.
	pub policy:        ApprovalPolicy,
	/// Authority which selected the policy.
	pub source:        ApprovalSource,
	/// User policy key, present only for a per-tool override.
	pub policy_key:    Option<Str>,
}

/// Resolves a durable invocation decision from the declared effect ceiling
/// and where those effects happen.
///
/// Per-tool overrides remain authoritative in every mode. Without one, the
/// call's mode ([`call_approval_mode`]) approves tiers up to `read`, `write`,
/// and `exec`, respectively: a defaulted `yolo` that only an active sandbox
/// keeps alive covers sandboxed tools ([`Confinement::ExecSandbox`]), and a
/// [`Confinement::Host`] tool is admitted exactly as it would be with no
/// sandbox, so its exec-tier calls prompt. Explicit `yolo` is respected only
/// in a trusted workspace. A sandboxed tool declares no effects the sandbox
/// confines; without a sandbox it is process authority and resolves to `exec`.
pub fn resolve_approval(
	invocation_id: impl Into<Str>,
	tool_name: impl Into<Str>,
	effects: &Effects,
	confinement: Confinement,
	configured: ConfiguredApproval,
	sandbox: SandboxState,
	override_policy: Option<ApprovalPolicy>,
) -> ResolvedApproval {
	let invocation_id = invocation_id.into();
	let tool_name = tool_name.into();
	let tier = if confinement.sandboxed() && !sandbox.confines() {
		ApprovalTier::Exec
	} else {
		ApprovalTier::from_effects(effects)
	};
	let mode = call_approval_mode(configured, sandbox, confinement);
	let (policy, source, policy_key) = override_policy.map_or_else(
		|| {
			let allowed = match mode {
				ApprovalMode::AlwaysAsk => tier <= ApprovalTier::Read,
				ApprovalMode::Write => tier <= ApprovalTier::Write,
				ApprovalMode::Yolo => true,
			};
			(
				if allowed {
					ApprovalPolicy::Allow
				} else {
					ApprovalPolicy::Prompt
				},
				ApprovalSource::Mode,
				None,
			)
		},
		|policy| (policy, ApprovalSource::User, Some(tool_name.clone())),
	);
	ResolvedApproval {
		invocation_id,
		tool_name,
		confinement,
		tier,
		mode,
		policy,
		source,
		policy_key,
	}
}
