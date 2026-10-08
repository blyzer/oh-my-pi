//! Per-invocation admission gate between finalized arguments and authorization.

use std::{
	collections::BTreeMap,
	io::Cursor,
	path::Path,
	sync::Arc,
	time::{Duration, SystemTime, UNIX_EPOCH},
};

use bytes::{Bytes, BytesMut};
use flume::Receiver;
use omp_agent::{ApprovalRoute, ApprovalSource as DecisionSource, ApprovalSpec};
use omp_core::{Str, sf};
use omp_proto::{
	env::v1::{Admission, AdmitInvocation},
	policy::v1::{BashIr, EffectEnvelope, PolicyDenied},
};
use omp_shell::{
	analysis,
	parser::{Parser, ParserOptions},
};
use omp_tool::{Confinement, Effects};
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
use tokio::{time, time::Instant};
use tokio_util::sync::CancellationToken;

use crate::approval_relay::OwnedApprovals;

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

/// Name of the tool whose `command` argument is parsed into [`BashIr`].
const SHELL_TOOL: &str = "bash";

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

	/// Constructs the sandbox `ctx` asks for and reports what applies.
	#[must_use]
	pub fn probe(ctx: &omp_con::Ctx, workspace_root: &Path) -> Self {
		crate::exec_sandbox::probe(
			&crate::exec_settings::SandboxSettings::from_con(ctx),
			workspace_root,
		)
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
	pub mode:       ApprovalMode,
	/// Whether the user chose it or it is the shipped default.
	pub provenance: Provenance,
}

/// The one rule for how a configured approval mode meets the sandbox.
///
/// `Yolo` (never ask) inside a sandbox that confines the environment is
/// honoured. Without one, a defaulted `Yolo` is downgraded to `Write`, while an
/// explicit `Yolo` (a flag, the user's config) is respected and reported as
/// unconfined. Every other mode is returned unchanged.
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
		(ApprovalMode::Yolo, Provenance::Default) if !sandbox.confines() => ApprovalMode::Write,
		(mode, _) => mode,
	}
}

/// The mode a call to a tool of `confinement` is admitted under.
///
/// [`effective_approval_mode`] sees the sandbox only for a tool that sandbox
/// confines ([`Confinement::ExecSandbox`]). A [`Confinement::Host`] tool sees
/// none, so a defaulted `Yolo` is `Write` for it even while the sandbox is
/// active, and its mode is the same under every sandbox state. An explicit
/// `Yolo` is respected for both.
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

/// A `yolo` approval that no sandbox confines: either downgraded to `write`
/// (the default) or respected and running unconfined (the user asked for it).
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ApprovalPosture {
	/// Mode configured.
	pub configured: ApprovalMode,
	/// Who configured it.
	pub provenance: Provenance,
	/// Mode actually enforced.
	pub effective:  ApprovalMode,
	/// Sandbox state that decided it.
	pub sandbox:    SandboxState,
}

impl ApprovalPosture {
	/// Reports the posture [`effective_approval_mode`] yields for a `yolo`
	/// with no confining sandbox; `None` when nothing needs saying.
	#[must_use]
	pub fn resolve(configured: ConfiguredApproval, sandbox: SandboxState) -> Option<Self> {
		(configured.mode == ApprovalMode::Yolo && !sandbox.confines()).then(|| Self {
			configured: configured.mode,
			provenance: configured.provenance,
			effective: effective_approval_mode(configured, sandbox),
			sandbox,
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
	/// Process, network, inference, or subagent authority.
	Exec,
}

impl ApprovalTier {
	/// Resolves the highest approval tier present in `effects`.
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
			|| effects.desktop.as_ref().is_some_and(|effect| effect.input)
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
	/// Tier derived from the live revision's declared effects.
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
/// sandbox, so its exec-tier calls prompt. An explicit `yolo` is respected
/// either way. A sandboxed tool declares no effects the sandbox confines; with
/// no sandbox in force it is process authority and resolves to the `exec` tier.
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

/// Origin of a nested invocation admitted through the environment host.
#[derive(Clone, Copy, Debug, Eq, PartialEq, strum::IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
pub(crate) enum DynamicInvocationSource {
	/// A target selected by the in-process shell's `dyn` builtin.
	ShellDyn,
}

/// Typed refusal from nested dynamic-target admission.
#[derive(Debug, Error)]
pub(crate) enum DynamicAdmissionError {
	/// The resolved per-target policy denied the invocation.
	#[error("dynamic target `{target}` is denied by approval policy")]
	Denied {
		/// Exact resolved dynamic target.
		target: Str,
	},
	/// The target requires a prompt but no host approval route is installed.
	#[error("dynamic target `{target}` requires an unavailable approval route")]
	ApprovalUnavailable {
		/// Exact resolved dynamic target.
		target: Str,
	},
	/// Cancellation won while the target was awaiting approval.
	#[error("dynamic target `{target}` admission was cancelled")]
	Cancelled {
		/// Exact resolved dynamic target.
		target: Str,
	},
}

/// Shared admission authority for targets resolved inside another tool.
///
/// Dynamic and native routes pass the resolved target's own [`Effects`] and
/// [`Confinement`] here; the containing tool's broader declaration and its
/// sandbox never substitute for the target-specific decision.
#[derive(Clone)]
pub(crate) struct DynamicAdmission {
	mode:      ConfiguredApproval,
	sandbox:   SandboxState,
	overrides: Arc<BTreeMap<Str, ApprovalPolicy>>,
	route:     Arc<RwLock<Option<ApprovalRoute>>>,
}

impl DynamicAdmission {
	/// Builds a cloneable nested admission authority from frozen tool policy.
	pub(crate) fn new(
		mode: ConfiguredApproval,
		sandbox: SandboxState,
		overrides: BTreeMap<Str, ApprovalPolicy>,
		route: Option<ApprovalRoute>,
	) -> Self {
		Self { mode, sandbox, overrides: Arc::new(overrides), route: Arc::new(RwLock::new(route)) }
	}

	/// Replaces the host route used by subsequent dynamic approval prompts.
	pub(crate) fn bind_route(&self, route: Option<ApprovalRoute>) {
		*self.route.write() = route;
	}

	/// Resolves and enforces one target's live effect declaration.
	///
	/// A prompt asks `relay`, the connection that issued the containing
	/// command, when there is one, and never falls back to the host route: a
	/// relay whose connection closed decides the prompt as unreachable. Without
	/// a relay the route the host bound answers, if any.
	#[expect(
		clippy::too_many_arguments,
		reason = "one admission joins the target, its declaration, its origin and its approver"
	)]
	pub(crate) async fn admit(
		&self,
		invocation_id: Str,
		target: Str,
		effects: &Effects,
		confinement: Confinement,
		source: DynamicInvocationSource,
		relay: Option<&OwnedApprovals>,
		cancellation: CancellationToken,
	) -> Result<ResolvedApproval, DynamicAdmissionError> {
		if cancellation.is_cancelled() {
			return Err(DynamicAdmissionError::Cancelled { target });
		}
		let resolved = resolve_approval(
			invocation_id.clone(),
			target.clone(),
			effects,
			confinement,
			self.mode,
			self.sandbox,
			self.overrides.get(&target).copied(),
		);
		match resolved.policy {
			ApprovalPolicy::Allow => return Ok(resolved),
			ApprovalPolicy::Deny => {
				return Err(DynamicAdmissionError::Denied { target });
			},
			ApprovalPolicy::Prompt => {},
		}
		// A relay outranks the host route and never falls back to it.
		let route = if relay.is_some() {
			None
		} else {
			self.route.read().clone()
		};
		let tier: &'static str = resolved.tier.into();
		let origin: &'static str = source.into();
		let confinement: &'static str = confinement.into();
		let reasons = vec![ApprovalSpec {
			title:         sf!("Approve dynamic target"),
			body:          sf!("Allow dynamic target `{target}`?"),
			subject:       target.clone(),
			kind:          Str::new_static(tier),
			scopes:        vec![sf!("once")],
			default:       None,
			route:         sf!("user"),
			approver:      None,
			timeout_ms:    0,
			unreachable:   sf!("fail_closed"),
			require_human: false,
			pattern:       None,
			evidence:      vec![sf!("invocation_source={origin}"), sf!("confinement={confinement}")],
		}];
		let ticket = match (relay, route) {
			(Some(relay), _) => {
				// Dropping the relayed prompt withdraws its query.
				tokio::select! {
					biased;
					() = cancellation.cancelled() => {
						return Err(DynamicAdmissionError::Cancelled { target });
					},
					ticket = relay.request(Some(invocation_id), reasons, epoch_millis()) => ticket,
				}
			},
			(None, Some(route)) => {
				route
					.request_cancellable(
						Some(invocation_id),
						reasons,
						epoch_millis(),
						cancellation.clone(),
					)
					.await
			},
			(None, None) => return Err(DynamicAdmissionError::ApprovalUnavailable { target }),
		};
		let Some(decision) = ticket.decision else {
			return Err(DynamicAdmissionError::ApprovalUnavailable { target });
		};
		if decision.approved {
			return Ok(resolved);
		}
		if decision.source == DecisionSource::Unavailable && cancellation.is_cancelled() {
			return Err(DynamicAdmissionError::Cancelled { target });
		}
		Err(DynamicAdmissionError::Denied { target })
	}
}

fn epoch_millis() -> u64 {
	SystemTime::now()
		.duration_since(UNIX_EPOCH)
		.map_or(0, |elapsed| u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX))
}

/// A finalized admission result, with policy transformation applied before the
/// executor can observe arguments.
pub enum AdmissionDecision {
	/// The effective canonical arguments and regenerated shell facts.
	Allowed {
		/// RFC 8785-compatible serde canonical argument bytes for the executor.
		raw:  Bytes,
		/// Shell facts regenerated from the effective command, when applicable.
		bash: Option<BashIr>,
	},
	/// A refusal carrying the one wire vocabulary for policy denial.
	Denied(PolicyDenied),
}

/// A protocol or transformation failure while admitting an invocation.
#[derive(Debug, Error)]
pub enum AdmissionError {
	/// Arguments ended in a value that cannot be transformed as an object.
	#[error("finalized invocation arguments must be a JSON object")]
	ArgumentsNotObject,
	/// A policy transform was not a valid JSON merge patch.
	#[error("admission argument patch is not valid JSON")]
	InvalidPatch,
	/// The admission response belonged to another invocation.
	#[error("admission response invocation id did not match the pending invocation")]
	WrongInvocation,
	/// No admission query has reached the finalized-arguments transition.
	#[error("admission response arrived before finalized arguments")]
	NotPending,
	/// The query's single response has already been accepted.
	#[error("admission response was already supplied")]
	AlreadyAnswered,
}

/// One cache invalidation target derived from a mutating GitHub CLI command.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct GithubMutationTarget {
	/// Explicit `owner/repo`, absent when the command targets the active
	/// repository.
	pub(crate) repo:   Option<Str>,
	/// Resource family (`issue` or `pr`).
	pub(crate) kind:   Str,
	/// Explicit issue or pull-request number when statically known.
	pub(crate) number: Option<u64>,
}

/// Derives only mutating issue/PR operations from admitted BashIR.
///
/// Dynamic words are ignored rather than guessed, and read-only `gh` actions
/// never invalidate cache entries.
pub(crate) fn github_mutation_targets(bash: &BashIr) -> Vec<GithubMutationTarget> {
	let mut targets = Vec::new();
	for command in &bash.commands {
		if command.name.as_deref() != Some("gh")
			|| command.argv.iter().any(|argument| argument.dynamic)
		{
			continue;
		}
		let argv = command
			.argv
			.iter()
			.map(|argument| argument.text.as_str())
			.collect::<Vec<_>>();
		let Some(kind_index) = argv
			.iter()
			.position(|argument| matches!(*argument, "issue" | "pr"))
		else {
			continue;
		};
		let Some(action) = argv.get(kind_index + 1).copied() else {
			continue;
		};
		if !matches!(
			action,
			"create"
				| "edit" | "close"
				| "reopen"
				| "delete"
				| "comment"
				| "lock" | "unlock"
				| "pin" | "unpin"
				| "transfer"
				| "merge"
				| "ready"
				| "review"
		) {
			continue;
		}
		let repo = option_value(&argv, "-R")
			.or_else(|| option_value(&argv, "--repo"))
			.or_else(|| inline_option(&argv, "--repo="))
			.map(Str::new);
		let number = argv
			.iter()
			.skip(kind_index + 2)
			.find_map(|argument| argument.trim_start_matches('#').parse::<u64>().ok());
		targets.push(GithubMutationTarget { repo, kind: Str::new(argv[kind_index]), number });
	}
	targets
}

fn option_value<'a>(arguments: &[&'a str], option: &str) -> Option<&'a str> {
	arguments
		.windows(2)
		.find_map(|pair| (pair[0] == option).then_some(pair[1]))
}

fn inline_option<'a>(arguments: &[&'a str], prefix: &str) -> Option<&'a str> {
	arguments
		.iter()
		.find_map(|argument| argument.strip_prefix(prefix))
}

/// Env-owned one-shot admission state for one invocation.
pub struct AdmissionGate {
	invocation_id:      Str,
	tool_name:          Str,
	deadline:           Instant,
	fragments:          BytesMut,
	requested:          Option<Value>,
	query_emitted:      bool,
	policy:             ApprovalPolicy,
	defer_until_commit: bool,
	answer_tx:          Option<flume::Sender<Admission>>,
	answer_rx:          Receiver<Admission>,
}

impl AdmissionGate {
	/// Starts an OPEN invocation gate whose deadline is enforced by env.
	#[cfg(test)]
	pub(crate) fn new(invocation_id: Str, tool_name: Str, deadline: Duration) -> Self {
		Self::with_policy(invocation_id, tool_name, deadline, ApprovalPolicy::Prompt)
	}

	/// Starts an OPEN invocation gate with its resolved admission policy.
	pub(crate) fn with_policy(
		invocation_id: Str,
		tool_name: Str,
		deadline: Duration,
		policy: ApprovalPolicy,
	) -> Self {
		Self::with_policy_mode(invocation_id, tool_name, deadline, policy, false)
	}

	/// Starts a caller-composed gate whose query uses only committed arguments.
	pub(crate) fn with_deferred_policy(
		invocation_id: Str,
		tool_name: Str,
		deadline: Duration,
		policy: ApprovalPolicy,
	) -> Self {
		Self::with_policy_mode(invocation_id, tool_name, deadline, policy, true)
	}

	fn with_policy_mode(
		invocation_id: Str,
		tool_name: Str,
		deadline: Duration,
		policy: ApprovalPolicy,
		defer_until_commit: bool,
	) -> Self {
		let (answer_tx, answer_rx) = flume::bounded(1);
		Self {
			invocation_id,
			tool_name,
			deadline: Instant::now() + deadline,
			fragments: BytesMut::new(),
			requested: None,
			query_emitted: false,
			policy,
			defer_until_commit,
			answer_tx: Some(answer_tx),
			answer_rx,
		}
	}

	/// Appends one raw argument fragment and emits a query once it is one JSON
	/// document. Further fragments remain the caller's protocol violation.
	pub(crate) fn push_fragment(
		&mut self,
		fragment: &str,
		cwd: &Path,
		root: &Path,
	) -> Option<AdmitInvocation> {
		if self.query_emitted {
			return None;
		}
		if self.defer_until_commit {
			return None;
		}
		self.fragments.extend_from_slice(fragment.as_bytes());
		let value = serde_json::from_slice::<Value>(&self.fragments).ok()?;
		if !value.is_object() {
			return None;
		}
		self.finish_query(value, cwd, root)
	}

	/// Finalizes a call that supplied its complete arguments only with
	/// `ArgsCommitted`, replacing any incomplete speculative fragments.
	pub(crate) fn finalize(
		&mut self,
		raw: &[u8],
		cwd: &Path,
		root: &Path,
	) -> Result<Option<AdmitInvocation>, AdmissionError> {
		if self.query_emitted {
			return Ok(None);
		}
		let value =
			serde_json::from_slice::<Value>(raw).map_err(|_| AdmissionError::ArgumentsNotObject)?;
		if !value.is_object() {
			return Err(AdmissionError::ArgumentsNotObject);
		}
		self.fragments.clear();
		self.fragments.extend_from_slice(raw);
		Ok(self.finish_query(value, cwd, root))
	}

	fn finish_query(&mut self, value: Value, cwd: &Path, root: &Path) -> Option<AdmitInvocation> {
		let bash = bash_ir(&self.tool_name, &value, cwd, root);
		self.requested = Some(value);
		self.query_emitted = true;
		let query = AdmitInvocation {
			invocation_id: self.invocation_id.to_string(),
			bash,
			deadline_ms: self
				.deadline
				.saturating_duration_since(Instant::now())
				.as_millis()
				.try_into()
				.unwrap_or(u64::MAX),
			props: Default::default(),
		};
		match self.policy {
			ApprovalPolicy::Prompt => Some(query),
			ApprovalPolicy::Allow => {
				self
					.answer(Admission {
						invocation_id: self.invocation_id.to_string(),
						allow: true,
						..Admission::default()
					})
					.expect("an internally resolved admission is answered exactly once");
				None
			},
			ApprovalPolicy::Deny => {
				self
					.answer(Admission {
						invocation_id: self.invocation_id.to_string(),
						allow: false,
						denied: Some(approval_denial(&self.invocation_id, &self.tool_name)),
						..Admission::default()
					})
					.expect("an internally resolved admission is answered exactly once");
				None
			},
		}
	}

	/// Accepts Core's one answer without allowing it to block the dispatcher.
	pub(crate) fn answer(&mut self, admission: Admission) -> Result<(), AdmissionError> {
		if !self.query_emitted {
			return Err(AdmissionError::NotPending);
		}
		if admission.invocation_id != self.invocation_id {
			return Err(AdmissionError::WrongInvocation);
		}
		let Some(answer_tx) = self.answer_tx.take() else {
			return Err(AdmissionError::AlreadyAnswered);
		};
		answer_tx
			.send(admission)
			.map_err(|_| AdmissionError::AlreadyAnswered)
	}

	/// Reports whether Core's one admission answer has arrived.
	pub(crate) const fn is_answered(&self) -> bool {
		self.query_emitted && self.answer_tx.is_none()
	}

	/// Returns whether the caller must answer before effective arguments commit.
	pub(crate) const fn requires_external_answer(&self) -> bool {
		matches!(self.policy, ApprovalPolicy::Prompt)
	}

	/// Returns the deadline for a query that is waiting on Core.
	pub(crate) fn pending_deadline(&self) -> Option<Instant> {
		(self.query_emitted && self.answer_tx.is_some()).then_some(self.deadline)
	}

	/// Converts an unanswered query whose deadline has elapsed into env's
	/// synthetic denial. The connection loop owns this transition.
	pub(crate) fn expire(&mut self, now: Instant) -> Option<PolicyDenied> {
		(self.query_emitted && self.answer_tx.is_some() && now >= self.deadline).then(|| {
			self.answer_tx.take();
			timeout_denial(&self.invocation_id)
		})
	}

	/// Waits for Core's answer through the env-owned deadline, synthesizing the
	/// structured fail-closed denial when it expires or the relay closes.
	pub(crate) async fn decide(&self, cwd: &Path, root: &Path) -> AdmissionDecision {
		let admission = match self.answer_rx.try_recv() {
			Ok(admission) => admission,
			Err(flume::TryRecvError::Empty) => {
				let answer = time::timeout_at(self.deadline, self.answer_rx.recv_async()).await;
				let Ok(Ok(admission)) = answer else {
					return AdmissionDecision::Denied(timeout_denial(&self.invocation_id));
				};
				admission
			},
			Err(flume::TryRecvError::Disconnected) => {
				return AdmissionDecision::Denied(timeout_denial(&self.invocation_id));
			},
		};
		if !admission.allow {
			return AdmissionDecision::Denied(
				admission
					.denied
					.unwrap_or_else(|| timeout_denial(&self.invocation_id)),
			);
		}
		let Some(requested) = self.requested.as_ref() else {
			return AdmissionDecision::Denied(timeout_denial(&self.invocation_id));
		};
		let Ok((raw, bash)) =
			apply_admission_patch(requested, &admission.args_patch, &self.tool_name, cwd, root)
		else {
			return AdmissionDecision::Denied(invalid_patch_denial(&self.invocation_id));
		};
		AdmissionDecision::Allowed { raw, bash }
	}
}

/// Refuses an envelope that would widen the resolved tool declaration.
pub fn effects_narrow_or_refuse(
	requested: Option<&EffectEnvelope>,
	maximum: &Effects,
) -> Option<Effects> {
	let requested = requested.map(Effects::try_from).transpose().ok()?;
	match requested {
		Some(requested) if !requested.is_empty() => maximum.narrow(requested),
		Some(_) | None => Some(maximum.clone()),
	}
}

fn apply_admission_patch(
	requested: &Value,
	patch: &[u8],
	tool_name: &str,
	cwd: &Path,
	root: &Path,
) -> Result<(Bytes, Option<BashIr>), AdmissionError> {
	let mut effective = requested.clone();
	if !patch.is_empty() {
		let patch = serde_json::from_slice(patch).map_err(|_| AdmissionError::InvalidPatch)?;
		merge_patch(&mut effective, patch);
	}
	if !effective.is_object() {
		return Err(AdmissionError::ArgumentsNotObject);
	}
	let raw = serde_json::to_vec(&effective).map_err(|_| AdmissionError::InvalidPatch)?;
	let bash = bash_ir(tool_name, &effective, cwd, root);
	Ok((Bytes::from(raw), bash))
}

fn merge_patch(target: &mut Value, patch: Value) {
	let Value::Object(patch) = patch else {
		*target = patch;
		return;
	};
	if !target.is_object() {
		*target = Value::Object(Default::default());
	}
	let Value::Object(target) = target else {
		return;
	};
	for (key, value) in patch {
		if value.is_null() {
			target.remove(&key);
		} else {
			merge_patch(target.entry(key).or_insert(Value::Null), value);
		}
	}
}

pub(crate) fn bash_ir(tool_name: &str, args: &Value, cwd: &Path, root: &Path) -> Option<BashIr> {
	if tool_name != SHELL_TOOL {
		return None;
	}
	let command = args.get("command")?.as_str()?;
	let mut parser = Parser::new(Cursor::new(command), &ParserOptions::default());
	Some(match parser.parse_program() {
		Ok(program) => BashIr::from(&analysis::analyze(
			&program,
			cwd.to_string_lossy().as_ref(),
			root.to_string_lossy().as_ref(),
		)),
		Err(error) => BashIr {
			source: command.to_owned(),
			parse_ok: false,
			parse_error: Some(error.to_string()),
			..BashIr::default()
		},
	})
}

fn timeout_denial(invocation_id: &str) -> PolicyDenied {
	PolicyDenied {
		reason:      "admission deadline elapsed".into(),
		code:        "admission_timeout".into(),
		decision_id: invocation_id.to_owned(),
		rules:       Vec::new(),
		props:       Default::default(),
	}
}

fn invalid_patch_denial(invocation_id: &str) -> PolicyDenied {
	PolicyDenied {
		reason:      "admission transformation was invalid".into(),
		code:        "admission_invalid_patch".into(),
		decision_id: invocation_id.to_owned(),
		rules:       Vec::new(),
		props:       Default::default(),
	}
}

fn approval_denial(invocation_id: &str, tool_name: &str) -> PolicyDenied {
	PolicyDenied {
		reason:      format!("tool `{tool_name}` is denied by approval policy"),
		code:        "approval_policy_denied".into(),
		decision_id: invocation_id.to_owned(),
		rules:       vec![format!("tools.approval.{tool_name}")],
		props:       Default::default(),
	}
}

#[cfg(test)]
mod tests {
	use std::{collections::BTreeMap, path::Path, sync::Arc, time::Duration};

	use bytes::Bytes;
	use omp_agent::{
		ApprovalBook, ApprovalDecision, ApprovalRoute, ApprovalScope,
		ApprovalSource as DecisionSource,
	};
	use omp_core::sf;
	use omp_proto::{
		env::v1::Admission,
		policy::v1::{EffectEnvelope, ExecEffects},
	};
	use omp_tool::{
		Confinement, DesktopEffects, DocEffects, Effects, ExecEffects as ToolExecEffects,
		FetchEffects, InferenceEffects, Usd,
	};
	use proptest::prelude::*;
	use tokio::time;
	use tokio_util::sync::CancellationToken;

	use super::{
		AdmissionDecision, AdmissionGate, ApprovalMode, ApprovalPolicy, ApprovalPosture,
		ApprovalSource, ApprovalTier, ConfiguredApproval, DynamicAdmission, DynamicAdmissionError,
		DynamicInvocationSource, Provenance, SandboxState, SandboxUnavailable, apply_admission_patch,
		bash_ir, call_approval_mode, effective_approval_mode, effects_narrow_or_refuse,
		github_mutation_targets, resolve_approval,
	};

	const UNAVAILABLE: SandboxState =
		SandboxState::Unavailable { cause: SandboxUnavailable::BackendUnavailable };

	const fn explicit(mode: ApprovalMode) -> ConfiguredApproval {
		ConfiguredApproval { mode, provenance: Provenance::Explicit }
	}

	const fn defaulted(mode: ApprovalMode) -> ConfiguredApproval {
		ConfiguredApproval { mode, provenance: Provenance::Default }
	}

	/// The unknown ceiling an undeclared host tool registers is the `exec`
	/// tier: it prompts under `write`, `always-ask` and a sandbox-kept default
	/// `yolo` (a `Host` tool), and only an explicit `yolo` admits it. A host
	/// tool that declares a read-only envelope is admitted even under
	/// `always-ask`.
	#[test]
	fn undeclared_host_tools_are_exec_and_declared_reads_are_not() {
		let unknown = Effects::unknown();
		assert_eq!(ApprovalTier::from_effects(&unknown), ApprovalTier::Exec);
		for (configured, policy) in [
			(explicit(ApprovalMode::AlwaysAsk), ApprovalPolicy::Prompt),
			(explicit(ApprovalMode::Write), ApprovalPolicy::Prompt),
			(defaulted(ApprovalMode::Yolo), ApprovalPolicy::Prompt),
			(explicit(ApprovalMode::Yolo), ApprovalPolicy::Allow),
		] {
			let resolved = resolve_approval(
				"call",
				"host_tool",
				&unknown,
				Confinement::Host,
				configured,
				SandboxState::Active,
				None,
			);
			assert_eq!(resolved.policy, policy, "{configured:?}");
		}
		let read = Effects {
			documents: Some(DocEffects { read: true, write_globs: Arc::from([]) }),
			..Effects::empty()
		};
		let resolved = resolve_approval(
			"call",
			"host_tool",
			&read,
			Confinement::Host,
			explicit(ApprovalMode::AlwaysAsk),
			SandboxState::Active,
			None,
		);
		assert_eq!((resolved.tier, resolved.policy), (ApprovalTier::Read, ApprovalPolicy::Allow));
	}

	/// A fetch is its own tier between read and write: `always-ask` prompts
	/// for it, `write`, the default and an explicit `yolo` allow it, and a
	/// fetch beside a write or an exec effect takes the higher tier. A fetch
	/// is never empty and never mutates the environment.
	#[test]
	fn a_fetch_sits_between_read_and_write() {
		let fetch = Effects { fetch: Some(FetchEffects { credentials: true }), ..Effects::empty() };
		assert_eq!(ApprovalTier::from_effects(&fetch), ApprovalTier::Fetch);
		assert!(!fetch.is_empty());
		assert!(!fetch.mutates_environment());
		for (configured, policy) in [
			(explicit(ApprovalMode::AlwaysAsk), ApprovalPolicy::Prompt),
			(explicit(ApprovalMode::Write), ApprovalPolicy::Allow),
			(defaulted(ApprovalMode::Yolo), ApprovalPolicy::Allow),
			(explicit(ApprovalMode::Yolo), ApprovalPolicy::Allow),
		] {
			let decision = resolve_approval(
				"fetch-1",
				"read",
				&fetch,
				Confinement::Host,
				configured,
				SandboxState::Off,
				None,
			);
			assert_eq!(
				(decision.tier, decision.policy),
				(ApprovalTier::Fetch, policy),
				"{configured:?}"
			);
		}
		let fetch_and_write = Effects {
			documents: Some(DocEffects { read: true, write_globs: Arc::from([sf!("**")]) }),
			..fetch.clone()
		};
		assert_eq!(ApprovalTier::from_effects(&fetch_and_write), ApprovalTier::Write);
		let fetch_and_exec = Effects {
			exec: Some(ToolExecEffects { commands: Arc::from([]), network: true }),
			..fetch
		};
		assert_eq!(ApprovalTier::from_effects(&fetch_and_exec), ApprovalTier::Exec);
	}

	/// A requested fetch narrows a declared fetch only when it asks for no
	/// credentials the maximum withholds, and never when the maximum denies
	/// fetching; the envelope survives the policy wire.
	#[test]
	fn fetch_narrowing_and_wire_round_trip() {
		let anonymous =
			Effects { fetch: Some(FetchEffects { credentials: false }), ..Effects::empty() };
		let credentialed =
			Effects { fetch: Some(FetchEffects { credentials: true }), ..Effects::empty() };
		assert!(anonymous.is_subset_of(&credentialed));
		assert!(!credentialed.is_subset_of(&anonymous));
		assert!(!anonymous.is_subset_of(&Effects::empty()));
		assert!(Effects::empty().is_subset_of(&anonymous));
		let wire = EffectEnvelope::from(&credentialed);
		assert_eq!(Effects::try_from(&wire).expect("decodes"), credentialed);
	}

	#[test]
	fn yolo_needs_a_sandbox_unless_the_user_asked_for_it() {
		use ApprovalMode::{AlwaysAsk, Write, Yolo};
		for (configured, sandbox, effective) in [
			(defaulted(Yolo), SandboxState::Active, Yolo),
			(defaulted(Yolo), SandboxState::Off, Write),
			(defaulted(Yolo), UNAVAILABLE, Write),
			(explicit(Yolo), SandboxState::Active, Yolo),
			(explicit(Yolo), SandboxState::Off, Yolo),
			(explicit(Yolo), UNAVAILABLE, Yolo),
			(explicit(Write), SandboxState::Active, Write),
			(explicit(Write), SandboxState::Off, Write),
			(explicit(Write), UNAVAILABLE, Write),
			(defaulted(Write), SandboxState::Off, Write),
			(explicit(AlwaysAsk), SandboxState::Active, AlwaysAsk),
			(explicit(AlwaysAsk), SandboxState::Off, AlwaysAsk),
			(explicit(AlwaysAsk), UNAVAILABLE, AlwaysAsk),
			(defaulted(AlwaysAsk), UNAVAILABLE, AlwaysAsk),
		] {
			assert_eq!(
				effective_approval_mode(configured, sandbox),
				effective,
				"{configured:?} under sandbox {sandbox}"
			);
		}
	}

	#[test]
	fn only_a_constructed_sandbox_confines() {
		assert!(SandboxState::Active.confines());
		assert!(!SandboxState::Off.confines());
		assert!(!UNAVAILABLE.confines());
		for cause in [SandboxUnavailable::UnsupportedHost, SandboxUnavailable::PolicyRejected] {
			assert!(!SandboxState::Unavailable { cause }.confines());
		}
	}

	#[test]
	fn the_posture_is_reported_with_typed_facts_and_only_when_it_applies() {
		let downgraded = ApprovalPosture::resolve(defaulted(ApprovalMode::Yolo), UNAVAILABLE)
			.expect("a defaulted yolo without a sandbox is downgraded");
		assert!(downgraded.downgraded());
		assert_eq!(
			serde_json::to_value(downgraded).expect("typed notice payload serializes"),
			serde_json::json!({
				"configured": "yolo",
				"provenance": "default",
				"effective": "write",
				"sandbox": { "state": "unavailable", "cause": "backend_unavailable" },
			})
		);
		assert!(downgraded.body().contains("backend_unavailable"));

		let respected = ApprovalPosture::resolve(explicit(ApprovalMode::Yolo), SandboxState::Off)
			.expect("an explicit yolo without a sandbox is reported");
		assert!(!respected.downgraded());
		assert_eq!(respected.effective, ApprovalMode::Yolo);
		assert_eq!(respected.provenance, Provenance::Explicit);
		assert!(respected.body().contains("unconfined"));

		for (configured, sandbox) in [
			(defaulted(ApprovalMode::Yolo), SandboxState::Active),
			(explicit(ApprovalMode::Yolo), SandboxState::Active),
			(explicit(ApprovalMode::Write), SandboxState::Off),
			(explicit(ApprovalMode::AlwaysAsk), UNAVAILABLE),
		] {
			assert_eq!(
				ApprovalPosture::resolve(configured, sandbox),
				None,
				"{configured:?} under {sandbox}"
			);
		}
	}

	#[test]
	fn exec_sandboxed_tools_are_exec_tier_unless_a_sandbox_confines_them() {
		use ApprovalPolicy::{Allow, Prompt};
		let none = Effects::empty();
		for (sandbox, configured, policy) in [
			(SandboxState::Active, defaulted(ApprovalMode::Yolo), Allow),
			(SandboxState::Active, explicit(ApprovalMode::Write), Allow),
			(SandboxState::Active, explicit(ApprovalMode::AlwaysAsk), Allow),
			(SandboxState::Off, defaulted(ApprovalMode::Yolo), Prompt),
			(UNAVAILABLE, defaulted(ApprovalMode::Yolo), Prompt),
			(SandboxState::Off, explicit(ApprovalMode::Yolo), Allow),
			(UNAVAILABLE, explicit(ApprovalMode::Yolo), Allow),
			(SandboxState::Off, explicit(ApprovalMode::Write), Prompt),
			(SandboxState::Off, explicit(ApprovalMode::AlwaysAsk), Prompt),
		] {
			// The marker decides, not the name: any sandboxed tool escalates.
			for name in ["bash", "hub", "sandboxed_probe"] {
				let decision = resolve_approval(
					"shell",
					name,
					&none,
					Confinement::ExecSandbox,
					configured,
					sandbox,
					None,
				);
				assert_eq!(
					decision.policy, policy,
					"{name} under {configured:?} with sandbox {sandbox}"
				);
				assert_eq!(
					decision.tier,
					if sandbox.confines() {
						ApprovalTier::Read
					} else {
						ApprovalTier::Exec
					}
				);
			}
		}
		// An effect-free `Host` tool stays read tier whatever its name and the
		// sandbox: a tool called `bash` is not escalated by its name.
		for name in ["bash", "think"] {
			for sandbox in [SandboxState::Active, SandboxState::Off, UNAVAILABLE] {
				let decision = resolve_approval(
					"t",
					name,
					&none,
					Confinement::Host,
					defaulted(ApprovalMode::Yolo),
					sandbox,
					None,
				);
				assert_eq!((decision.tier, decision.policy), (ApprovalTier::Read, Allow), "{name}");
			}
		}
		// A per-tool override stays authoritative without a sandbox, both ways.
		for (override_policy, expected) in
			[(ApprovalPolicy::Allow, Allow), (ApprovalPolicy::Deny, ApprovalPolicy::Deny)]
		{
			let decision = resolve_approval(
				"shell",
				"bash",
				&none,
				Confinement::ExecSandbox,
				defaulted(ApprovalMode::Yolo),
				SandboxState::Off,
				Some(override_policy),
			);
			assert_eq!((decision.policy, decision.source), (expected, ApprovalSource::User));
		}
	}

	/// One envelope per tier-deciding effect domain.
	fn tier_envelopes() -> [(&'static str, Effects, ApprovalTier); 6] {
		[
			(
				"document read",
				Effects {
					documents: Some(DocEffects { read: true, write_globs: Arc::from([]) }),
					..Effects::empty()
				},
				ApprovalTier::Read,
			),
			(
				"document write",
				Effects {
					documents: Some(DocEffects {
						read:        true,
						write_globs: Arc::from([sf!("**")]),
					}),
					..Effects::empty()
				},
				ApprovalTier::Write,
			),
			(
				"network",
				Effects {
					exec: Some(ToolExecEffects { commands: Arc::from([]), network: true }),
					..Effects::empty()
				},
				ApprovalTier::Exec,
			),
			(
				"commands",
				Effects {
					exec: Some(ToolExecEffects { commands: Arc::from([sf!("*")]), network: false }),
					..Effects::empty()
				},
				ApprovalTier::Exec,
			),
			(
				"inference",
				Effects {
					inference: Some(InferenceEffects {
						max_requests: 1,
						max_usd:      Usd::from_nanos(1),
					}),
					..Effects::empty()
				},
				ApprovalTier::Exec,
			),
			(
				"desktop input",
				Effects {
					desktop: Some(DesktopEffects {
						capture:       false,
						accessibility: false,
						input:         true,
					}),
					..Effects::empty()
				},
				ApprovalTier::Exec,
			),
		]
	}

	/// The decision-4 rule: a defaulted `yolo` that only an active sandbox
	/// keeps alive covers the tools that sandbox confines. A `Host` tool runs
	/// under `write` for that call, so its read and write tiers proceed and its
	/// exec tier prompts; an explicit `yolo` is respected for every tool.
	#[test]
	fn a_defaulted_yolo_covers_only_sandboxed_tools() {
		use ApprovalMode::{AlwaysAsk, Write, Yolo};
		use ApprovalPolicy::{Allow, Prompt};
		let sandboxes = [SandboxState::Active, SandboxState::Off, UNAVAILABLE];
		let modes = [defaulted(Yolo), explicit(Yolo), explicit(Write), explicit(AlwaysAsk)];
		for (domain, effects, declared_tier) in tier_envelopes() {
			for confinement in [Confinement::Host, Confinement::ExecSandbox] {
				for configured in modes {
					for sandbox in sandboxes {
						let decision = resolve_approval(
							"call",
							"tool",
							&effects,
							confinement,
							configured,
							sandbox,
							None,
						);
						let tier = if confinement.sandboxed() && !sandbox.confines() {
							ApprovalTier::Exec
						} else {
							declared_tier
						};
						// A defaulted yolo survives only where a sandbox confines
						// this very tool; every other configuration is unchanged.
						let mode = match (configured.mode, configured.provenance) {
							(Yolo, Provenance::Default)
								if !(confinement.sandboxed() && sandbox.confines()) =>
							{
								Write
							},
							(mode, _) => mode,
						};
						let allowed = match mode {
							AlwaysAsk => tier <= ApprovalTier::Read,
							Write => tier <= ApprovalTier::Write,
							Yolo => true,
						};
						let context = format!("{domain} {confinement} {configured:?} {sandbox}");
						assert_eq!(decision.confinement, confinement, "{context}");
						assert_eq!(decision.tier, tier, "{context}");
						assert_eq!(decision.mode, mode, "{context}");
						// `/security` reports the same per-call mode admission applies.
						assert_eq!(
							call_approval_mode(configured, sandbox, confinement),
							mode,
							"{context}"
						);
						assert_eq!(decision.policy, if allowed { Allow } else { Prompt }, "{context}");
						assert_eq!(decision.source, ApprovalSource::Mode, "{context}");
					}
				}
			}
		}
		// The headline rows, spelled out.
		let [read, write, network, ..] = tier_envelopes().map(|(_, effects, _)| effects);
		let host = |effects: &Effects, configured| {
			resolve_approval(
				"call",
				"web_search",
				effects,
				Confinement::Host,
				configured,
				SandboxState::Active,
				None,
			)
			.policy
		};
		assert_eq!(host(&network, defaulted(Yolo)), Prompt);
		assert_eq!(host(&read, defaulted(Yolo)), Allow);
		assert_eq!(host(&write, defaulted(Yolo)), Allow);
		assert_eq!(host(&network, explicit(Yolo)), Allow);
		let bash = resolve_approval(
			"call",
			"bash",
			&Effects::empty(),
			Confinement::ExecSandbox,
			defaulted(Yolo),
			SandboxState::Active,
			None,
		);
		assert_eq!((bash.mode, bash.policy), (Yolo, Allow));
	}

	fn sandbox_states() -> [SandboxState; 5] {
		[
			SandboxState::Active,
			SandboxState::Off,
			SandboxState::Unavailable { cause: SandboxUnavailable::UnsupportedHost },
			SandboxState::Unavailable { cause: SandboxUnavailable::BackendUnavailable },
			SandboxState::Unavailable { cause: SandboxUnavailable::PolicyRejected },
		]
	}

	fn any_effects() -> impl Strategy<Value = Effects> {
		(
			proptest::option::of((any::<bool>(), any::<bool>())),
			proptest::option::of((any::<bool>(), any::<bool>())),
			proptest::option::of((0_u32..3, 0_u64..3)),
			proptest::option::of((any::<bool>(), any::<bool>(), any::<bool>())),
			0_u32..3,
		)
			.prop_map(|(documents, exec, inference, desktop, subagents)| Effects {
				documents: documents.map(|(read, write)| DocEffects {
					read,
					write_globs: if write {
						Arc::from([sf!("**")])
					} else {
						Arc::from([])
					},
				}),
				exec: exec.map(|(commands, network)| ToolExecEffects {
					commands: if commands {
						Arc::from([sf!("*")])
					} else {
						Arc::from([])
					},
					network,
				}),
				inference: inference.map(|(max_requests, nanos)| InferenceEffects {
					max_requests,
					max_usd: Usd::from_nanos(nanos),
				}),
				desktop: desktop.map(|(capture, accessibility, input)| DesktopEffects {
					capture,
					accessibility,
					input,
				}),
				fetch: None,
				subagents,
			})
	}

	fn any_configured() -> impl Strategy<Value = ConfiguredApproval> {
		(
			prop_oneof![
				Just(ApprovalMode::AlwaysAsk),
				Just(ApprovalMode::Write),
				Just(ApprovalMode::Yolo),
			],
			prop_oneof![Just(Provenance::Default), Just(Provenance::Explicit)],
		)
			.prop_map(|(mode, provenance)| ConfiguredApproval { mode, provenance })
	}

	fn any_override() -> impl Strategy<Value = Option<ApprovalPolicy>> {
		proptest::option::of(prop_oneof![
			Just(ApprovalPolicy::Allow),
			Just(ApprovalPolicy::Deny),
			Just(ApprovalPolicy::Prompt),
		])
	}

	proptest! {
		/// "Not auto-approved merely because the sandbox is active", made
		/// exact: a `Host` tool gets the same decision under every sandbox
		/// state, for every mode, provenance, override and effect envelope.
		#[test]
		fn host_tools_are_admitted_as_if_no_sandbox_existed(
			effects in any_effects(),
			configured in any_configured(),
			override_policy in any_override(),
		) {
			let decide = |sandbox| {
				resolve_approval(
					"call",
					"host_tool",
					&effects,
					Confinement::Host,
					configured,
					sandbox,
					override_policy,
				)
			};
			let unconfined = decide(SandboxState::Off);
			for sandbox in sandbox_states() {
				prop_assert_eq!(&decide(sandbox), &unconfined, "{}", sandbox);
			}
		}
	}

	#[tokio::test]
	async fn deadline_synthesizes_a_structured_denial() {
		let gate = AdmissionGate::new(sf!("call"), sf!("bash"), Duration::ZERO);
		let AdmissionDecision::Denied(denied) =
			gate.decide(Path::new("/work"), Path::new("/work")).await
		else {
			panic!("elapsed admission deadline must deny");
		};
		assert_eq!(denied.code, "admission_timeout");
	}

	#[tokio::test]
	async fn allow_policy_finalizes_without_emitting_a_query() {
		let mut gate = AdmissionGate::with_policy(
			sf!("call"),
			sf!("bash"),
			Duration::ZERO,
			ApprovalPolicy::Allow,
		);
		assert!(
			gate
				.finalize(br#"{"command":"echo allowed"}"#, Path::new("/work"), Path::new("/work"))
				.expect("valid arguments")
				.is_none()
		);
		let AdmissionDecision::Allowed { raw, .. } =
			gate.decide(Path::new("/work"), Path::new("/work")).await
		else {
			panic!("allow policy must admit");
		};
		assert_eq!(raw, Bytes::from_static(br#"{"command":"echo allowed"}"#));
	}

	#[tokio::test]
	async fn deny_policy_finalizes_without_emitting_a_query() {
		let mut gate =
			AdmissionGate::with_policy(sf!("call"), sf!("bash"), Duration::ZERO, ApprovalPolicy::Deny);
		assert!(
			gate
				.finalize(br#"{"command":"echo denied"}"#, Path::new("/work"), Path::new("/work"))
				.expect("valid arguments")
				.is_none()
		);
		let AdmissionDecision::Denied(denied) =
			gate.decide(Path::new("/work"), Path::new("/work")).await
		else {
			panic!("deny policy must refuse");
		};
		assert_eq!(denied.code, "approval_policy_denied");
		assert_eq!(denied.decision_id, "call");
		assert_eq!(denied.rules, ["tools.approval.bash"]);
	}

	#[tokio::test]
	async fn prompt_policy_emits_a_query_and_waits_for_an_answer() {
		let mut gate = AdmissionGate::new(sf!("call"), sf!("bash"), Duration::from_secs(1));
		assert!(
			gate
				.finalize(br#"{"command":"echo prompt"}"#, Path::new("/work"), Path::new("/work"))
				.expect("valid arguments")
				.is_some()
		);
		assert!(
			time::timeout(
				Duration::from_millis(10),
				gate.decide(Path::new("/work"), Path::new("/work")),
			)
			.await
			.is_err(),
			"prompt policy must wait for the client admission answer"
		);
		gate
			.answer(Admission { invocation_id: "call".into(), allow: true, ..Admission::default() })
			.expect("prompt answer");
		assert!(matches!(
			gate.decide(Path::new("/work"), Path::new("/work")).await,
			AdmissionDecision::Allowed { .. }
		));
	}

	#[test]
	fn patch_changes_effective_args_and_regenerates_shell_facts() {
		let requested = serde_json::json!({"command": "echo requested"});
		let (raw, bash) = apply_admission_patch(
			&requested,
			br#"{"command":"echo effective"}"#,
			"bash",
			Path::new("/work"),
			Path::new("/work"),
		)
		.expect("valid merge patch");
		assert_eq!(raw, Bytes::from_static(br#"{"command":"echo effective"}"#));
		assert_eq!(bash.expect("shell facts").source, "echo effective");
	}

	#[test]
	fn approval_modes_resolve_from_declared_effects() {
		let read = Effects {
			documents: Some(DocEffects { read: true, write_globs: Arc::from([]) }),
			..Effects::empty()
		};
		let write = Effects {
			documents: Some(DocEffects { read: true, write_globs: Arc::from([sf!("**")]) }),
			..Effects::empty()
		};
		let exec = Effects {
			inference: Some(InferenceEffects { max_requests: 1, max_usd: Usd::from_nanos(1) }),
			..Effects::empty()
		};

		let desktop_read = Effects {
			desktop: Some(DesktopEffects {
				capture:       true,
				accessibility: true,
				input:         false,
			}),
			..Effects::empty()
		};
		let desktop_input = Effects {
			desktop: Some(DesktopEffects {
				capture:       false,
				accessibility: false,
				input:         true,
			}),
			..Effects::empty()
		};
		let sandbox = SandboxState::Active;
		let read_decision = resolve_approval(
			"read-1",
			"read",
			&read,
			Confinement::Host,
			explicit(ApprovalMode::AlwaysAsk),
			sandbox,
			None,
		);
		assert_eq!(read_decision.tier, ApprovalTier::Read);
		assert_eq!(read_decision.policy, ApprovalPolicy::Allow);
		assert_eq!(read_decision.source, ApprovalSource::Mode);

		let write_prompt = resolve_approval(
			"write-1",
			"write",
			&write,
			Confinement::Host,
			explicit(ApprovalMode::AlwaysAsk),
			sandbox,
			None,
		);
		assert_eq!(write_prompt.tier, ApprovalTier::Write);
		assert_eq!(write_prompt.policy, ApprovalPolicy::Prompt);

		let write_allowed = resolve_approval(
			"write-2",
			"write",
			&write,
			Confinement::Host,
			explicit(ApprovalMode::Write),
			sandbox,
			None,
		);
		assert_eq!(write_allowed.policy, ApprovalPolicy::Allow);

		let exec_prompt = resolve_approval(
			"eval-1",
			"eval",
			&exec,
			Confinement::Host,
			explicit(ApprovalMode::Write),
			sandbox,
			None,
		);
		assert_eq!(exec_prompt.tier, ApprovalTier::Exec);
		assert_eq!(exec_prompt.policy, ApprovalPolicy::Prompt);
		assert_eq!(ApprovalTier::from_effects(&desktop_read), ApprovalTier::Read);
		assert_eq!(ApprovalTier::from_effects(&desktop_input), ApprovalTier::Exec);
	}

	#[test]
	fn per_tool_override_is_authoritative_and_receipted() {
		let effects = Effects {
			exec: Some(ToolExecEffects { commands: Arc::from([sf!("*")]), network: true }),
			..Effects::empty()
		};
		let decision = resolve_approval(
			"shell-7",
			"bash",
			&effects,
			Confinement::ExecSandbox,
			explicit(ApprovalMode::Yolo),
			SandboxState::Active,
			Some(ApprovalPolicy::Deny),
		);
		assert_eq!(decision.policy, ApprovalPolicy::Deny);
		assert_eq!(decision.source, ApprovalSource::User);
		assert_eq!(decision.policy_key.as_deref(), Some("bash"));
		assert_eq!(
			serde_json::to_value(&decision).expect("durable approval receipt serializes"),
			serde_json::json!({
				"invocation_id": "shell-7",
				"tool_name": "bash",
				"confinement": "exec_sandbox",
				"tier": "exec",
				"mode": "yolo",
				"policy": "deny",
				"source": "user",
				"policy_key": "bash"
			})
		);
	}

	#[tokio::test]
	async fn dynamic_admission_uses_target_effects_and_deny_precedence() {
		let network = Effects {
			exec: Some(ToolExecEffects { commands: Arc::from([]), network: true }),
			..Effects::empty()
		};
		for (mode, override_policy, expected) in [
			(ApprovalMode::Yolo, None, "allow"),
			(ApprovalMode::Write, None, "prompt_unavailable"),
			(ApprovalMode::Yolo, Some(ApprovalPolicy::Deny), "deny"),
			(ApprovalMode::AlwaysAsk, Some(ApprovalPolicy::Allow), "allow"),
		] {
			let overrides = override_policy
				.map(|policy| BTreeMap::from([(sf!("github"), policy)]))
				.unwrap_or_default();
			let admission =
				DynamicAdmission::new(explicit(mode), SandboxState::Active, overrides, None);
			let result = admission
				.admit(
					sf!("dyn-1"),
					sf!("github"),
					&network,
					Confinement::Host,
					DynamicInvocationSource::ShellDyn,
					None,
					CancellationToken::new(),
				)
				.await;
			match expected {
				"allow" => {
					let resolved = result.expect("target policy allows");
					assert_eq!(resolved.tier, ApprovalTier::Exec);
				},
				"prompt_unavailable" => {
					assert!(matches!(result, Err(DynamicAdmissionError::ApprovalUnavailable { .. })))
				},
				"deny" => assert!(matches!(result, Err(DynamicAdmissionError::Denied { .. }))),
				_ => unreachable!("table contains only known outcomes"),
			}
		}
	}

	/// A `dyn` target running outside the sandbox gets no help from it: under
	/// a defaulted `yolo` with an active sandbox, a `Host` network target
	/// prompts (once, naming its confinement) or, with no route, is refused;
	/// a sandboxed target is allowed without a ticket.
	#[tokio::test]
	async fn dynamic_admission_prompts_for_host_targets_under_a_defaulted_yolo() {
		let network = Effects {
			exec: Some(ToolExecEffects { commands: Arc::from([]), network: true }),
			..Effects::empty()
		};
		let unrouted = DynamicAdmission::new(
			defaulted(ApprovalMode::Yolo),
			SandboxState::Active,
			BTreeMap::new(),
			None,
		);
		assert!(matches!(
			unrouted
				.admit(
					sf!("dyn-host"),
					sf!("github"),
					&network,
					Confinement::Host,
					DynamicInvocationSource::ShellDyn,
					None,
					CancellationToken::new(),
				)
				.await,
			Err(DynamicAdmissionError::ApprovalUnavailable { .. })
		));
		let sandboxed = unrouted
			.admit(
				sf!("dyn-sandboxed"),
				sf!("probe"),
				&Effects::empty(),
				Confinement::ExecSandbox,
				DynamicInvocationSource::ShellDyn,
				None,
				CancellationToken::new(),
			)
			.await
			.expect("a sandboxed target needs no route");
		assert_eq!((sandboxed.mode, sandboxed.policy), (ApprovalMode::Yolo, ApprovalPolicy::Allow));

		let (route, inbox) = ApprovalRoute::new(Arc::new(ApprovalBook::new()), None);
		unrouted.bind_route(Some(route.clone()));
		let pending_admission = unrouted.clone();
		let pending = tokio::spawn(async move {
			pending_admission
				.admit(
					sf!("dyn-host-routed"),
					sf!("github"),
					&network,
					Confinement::Host,
					DynamicInvocationSource::ShellDyn,
					None,
					CancellationToken::new(),
				)
				.await
		});
		let request = inbox.recv().await.expect("host target prompts");
		assert_eq!(request.ticket.reasons.len(), 1);
		assert_eq!(request.ticket.reasons[0].kind, "exec");
		assert!(
			request.ticket.reasons[0]
				.evidence
				.iter()
				.any(|line| line == "confinement=host"),
			"{:?}",
			request.ticket.reasons[0].evidence
		);
		request
			.respond(ApprovalDecision {
				approved:   true,
				scope:      ApprovalScope::Once,
				source:     DecisionSource::User,
				decided_by: None,
				reason:     None,
				audited:    false,
			})
			.expect("approval response accepted");
		let resolved = pending
			.await
			.expect("dynamic admission task")
			.expect("human approved the host target");
		assert_eq!(
			(resolved.confinement, resolved.mode, resolved.policy),
			(Confinement::Host, ApprovalMode::Write, ApprovalPolicy::Prompt)
		);
		assert!(inbox.try_recv().is_err(), "exactly one ticket was filed");
	}

	#[tokio::test]
	async fn dynamic_admission_prompts_with_source_and_is_cancellable() {
		let network = Effects {
			exec: Some(ToolExecEffects { commands: Arc::from([]), network: true }),
			..Effects::empty()
		};
		let (route, inbox) = ApprovalRoute::new(Arc::new(ApprovalBook::new()), None);
		let admission = DynamicAdmission::new(
			explicit(ApprovalMode::Write),
			SandboxState::Active,
			BTreeMap::new(),
			None,
		);
		admission.bind_route(Some(route.clone()));
		let pending_admission = admission.clone();
		let pending_network = network.clone();
		let pending = tokio::spawn(async move {
			pending_admission
				.admit(
					sf!("dyn-approval"),
					sf!("github"),
					&pending_network,
					Confinement::Host,
					DynamicInvocationSource::ShellDyn,
					None,
					CancellationToken::new(),
				)
				.await
		});
		let request = inbox
			.recv()
			.await
			.expect("dynamic approval prompt dispatched");
		assert_eq!(request.ticket.invocation_id.as_deref(), Some("dyn-approval"));
		assert_eq!(request.ticket.reasons[0].subject, "github");
		assert_eq!(request.ticket.reasons[0].kind, "exec");
		assert_eq!(request.ticket.reasons[0].evidence.as_slice(), [
			"invocation_source=shell_dyn",
			"confinement=host"
		]);
		request
			.respond(ApprovalDecision {
				approved:   true,
				scope:      ApprovalScope::Once,
				source:     DecisionSource::User,
				decided_by: None,
				reason:     None,
				audited:    false,
			})
			.expect("approval response accepted");
		pending
			.await
			.expect("dynamic admission task")
			.expect("human approved target");

		let cancellation = CancellationToken::new();
		let cancel_admission = admission.clone();
		let cancel_network = network.clone();
		let cancel_token = cancellation.clone();
		let cancelled = tokio::spawn(async move {
			cancel_admission
				.admit(
					sf!("dyn-cancelled"),
					sf!("github"),
					&cancel_network,
					Confinement::Host,
					DynamicInvocationSource::ShellDyn,
					None,
					cancel_token,
				)
				.await
		});
		let _request = inbox.recv().await.expect("cancellable prompt dispatched");
		cancellation.cancel();
		assert!(matches!(
			cancelled.await.expect("cancelled admission task"),
			Err(DynamicAdmissionError::Cancelled { .. })
		));
		assert!(route.pending().is_empty(), "cancelled prompt is withdrawn");
	}

	#[test]
	fn derives_only_static_mutating_github_targets() {
		let bash = bash_ir(
			"bash",
			&serde_json::json!({
				"command": "gh issue edit 42 --repo Owner/Repo && gh pr view 7"
			}),
			Path::new("/work"),
			Path::new("/work"),
		)
		.unwrap();
		let targets = github_mutation_targets(&bash);
		assert_eq!(targets.len(), 1);
		assert_eq!(targets[0].repo.as_deref(), Some("Owner/Repo"));
		assert_eq!(targets[0].kind, "issue");
		assert_eq!(targets[0].number, Some(42));
	}

	#[test]
	fn network_effect_narrowing_preserves_denies() {
		for (maximum_network, requested_network, expected_network) in [
			(false, false, Some(false)),
			(false, true, None),
			(true, false, Some(false)),
			(true, true, Some(true)),
		] {
			let maximum = Effects {
				exec: Some(ToolExecEffects {
					commands: Arc::from([sf!("curl")]),
					network:  maximum_network,
				}),
				..Effects::empty()
			};
			let requested = EffectEnvelope {
				exec: Some(ExecEffects {
					commands: vec![String::from("curl")],
					network:  requested_network,
					props:    None,
				}),
				..EffectEnvelope::default()
			};
			let narrowed = effects_narrow_or_refuse(Some(&requested), &maximum);
			assert_eq!(
				narrowed
					.and_then(|effects| effects.exec)
					.map(|effects| effects.network),
				expected_network,
			);
		}
	}

	#[test]
	fn widened_effect_envelope_is_refused() {
		let maximum = Effects {
			exec: Some(ToolExecEffects { commands: [sf!("git")].into(), network: false }),
			..Effects::empty()
		};
		let requested = EffectEnvelope {
			exec: Some(ExecEffects {
				commands: vec!["git".into(), "curl".into()],
				network:  false,
				props:    None,
			}),
			..EffectEnvelope::default()
		};
		assert!(effects_narrow_or_refuse(Some(&requested), &maximum).is_none());
	}

	#[test]
	fn absent_effect_envelope_retains_declared_maximum() {
		let maximum = Effects {
			exec: Some(ToolExecEffects { commands: [sf!("git")].into(), network: false }),
			..Effects::empty()
		};
		assert_eq!(effects_narrow_or_refuse(None, &maximum), Some(maximum));
	}

	#[test]
	fn default_effect_envelope_retains_declared_maximum() {
		let maximum = Effects {
			exec: Some(ToolExecEffects { commands: [sf!("git")].into(), network: false }),
			..Effects::empty()
		};
		assert_eq!(
			effects_narrow_or_refuse(Some(&EffectEnvelope::default()), &maximum),
			Some(maximum),
		);
	}
}
