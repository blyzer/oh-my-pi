//! Closed construction table for the production built-in roster.
//!
//! Driver selects a [`SelectedRoster`] in `compose_kernel`. The environment
//! host iterates that selection and fills the host traits below. This module
//! enumerates every built-in family; the host enumerates none. Capability
//! rows such as `browser` and `computer` are rows of this table only while
//! their `cap-*` feature is on. The `Family` variant and the contract module
//! stay either way.
//!
//! Constructors are the exhaustive `Family` matches in this file, not function
//! pointers on the row. Several host traits return `impl Future` and are not
//! object-safe, so a `linkme` or fn-pointer table cannot name them. The match
//! is closed at compile time: a new family fails to build until every dispatch
//! arm names it. Erasure stays at the registry's single `Arc<dyn ErasedTool>`
//! boundary.
//!
//! A [`SelectedRoster`] may carry one [`RosterExtension`]. Production leaves it
//! empty. A test crate registers a probe through it, which is how adding a
//! test capability edits no `crates/envd` file.

#![allow(missing_docs, reason = "strum IntoStaticStr emits undocumented inherent methods")]

use std::sync::Arc;

use omp_memory::Capabilities;
use omp_tool::{
	Claims, Precedence, Presentation, Registry, RegistryError, Rev, Tool, ToolSpec, ToolsPolicy,
};
use strum::{EnumIter, IntoStaticStr};

use crate::{
	ask::{AskPresenter, AskVocalizer},
	checkpoint::CheckpointControl,
	github::GithubHost,
	goal::GoalControl,
	grep::SearchPolicy,
	read::ReadPolicy,
	shell::ShellPromptSnapshot,
	web_search::SearchBackend,
};

/// Stable identity of one production native tool family.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BuiltinToolIdentity {
	/// Model-facing family name.
	pub name:   &'static str,
	/// Whether the family is omitted from ordinary user-facing lists.
	pub hidden: bool,
}

/// Where a built-in is constructed.
///
/// Composer rows are registered by driver before the session rows. Environment
/// rows are split so resource hosts (SSH, vault, resolvers) are installed
/// between the early devices and the document tools, and so `dyn` is installed
/// between those tools and `bash`.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum InstallPhase {
	/// Session-locus tools constructed with the session host.
	Session,
	/// Driver-owned tools injected as dynamic registrations.
	Composer,
	/// Environment devices constructed before resource resolvers exist.
	EnvironmentEarly,
	/// Environment tools constructed after resource resolvers exist.
	Environment,
	/// `bash`, constructed after the dynamic device host is installed.
	Shell,
}

/// How a row is presented to the model.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PresentationClass {
	/// Slot, unless the session policy is device-only.
	Essential,
	/// Device, unless the session policy is tool-only.
	LongTail,
	/// Always a slot. `bash` stays on the wire roster under every policy.
	Bash,
	/// Invokable, but omitted until a session policy selects it.
	Hidden,
	/// Always a catalog device.
	Device,
	/// Always a wire slot.
	Slot,
}

/// Claim class recorded with a row.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClaimsClass {
	/// Harness core precedence.
	Core,
	/// Core precedence only while the row is a slot; otherwise an enhancement.
	LongTail,
	/// Enhancement precedence. The registry refuses devices at core precedence.
	BuiltinDevice,
}

/// Host bridges a row's constructor reads.
///
/// The bits document the row. Dispatch does not branch on them: the `Family`
/// match is the constructor.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BridgeNeeds(u32);

impl BridgeNeeds {
	/// Ask presenter.
	pub const ASK: Self = Self(1 << 3);
	/// Blob store.
	pub const BLOBS: Self = Self(1 << 4);
	/// Browser daemon.
	pub const BROWSER: Self = Self(1 << 6);
	/// Checkpoint control.
	pub const CHECKPOINT: Self = Self(1 << 11);
	/// Computer session.
	pub const COMPUTER: Self = Self(1 << 13);
	/// Debugger control.
	pub const DEBUG: Self = Self(1 << 16);
	/// Document authority.
	pub const DOCUMENTS: Self = Self(1 << 5);
	/// Eval executor.
	pub const EVAL: Self = Self(1 << 9);
	/// GitHub credentials.
	pub const GITHUB: Self = Self(1 << 1);
	/// Goal authority.
	pub const GOAL: Self = Self(1 << 2);
	/// Language-server control.
	pub const LSP: Self = Self(1 << 15);
	/// Memory runtime.
	pub const MEMORY: Self = Self(1 << 7);
	/// The row reads no composition bridge.
	pub const NONE: Self = Self(0);
	/// Auxiliary search inference.
	pub const SEARCH: Self = Self(1 << 0);
	/// Security-scan service.
	pub const SECURITY: Self = Self(1 << 17);
	/// Shell executor.
	pub const SHELL: Self = Self(1 << 8);
	/// Managed-skill authority.
	pub const SKILLS: Self = Self(1 << 12);
	/// Subagent spawner or hub backend.
	pub const SUBAGENT: Self = Self(1 << 18);
	/// Telemetry upload.
	pub const TELEMETRY: Self = Self(1 << 10);
	/// Workspace search.
	pub const WORKSPACE: Self = Self(1 << 14);

	/// Union of two bridge sets.
	#[must_use]
	pub const fn or(self, other: Self) -> Self {
		Self(self.0 | other.0)
	}

	/// Whether `bit` is entirely present.
	#[must_use]
	pub const fn contains(self, bit: Self) -> bool {
		self.0 & bit.0 == bit.0
	}
}

/// One closed built-in family.
///
/// The snake_case name is the model-facing family name.
#[allow(missing_docs, reason = "strum IntoStaticStr emits into_str without a doc comment")]
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, EnumIter, IntoStaticStr)]
#[strum(serialize_all = "snake_case", const_into_str)]
pub enum Family {
	/// `image_gen`.
	ImageGen,
	/// `tts`.
	Tts,
	/// `report_issue`.
	ReportIssue,
	/// `github`.
	Github,
	/// `web_search`.
	WebSearch,
	/// `todo`.
	Todo,
	/// `ask`.
	Ask,
	/// `think`.
	Think,
	/// `goal`.
	Goal,
	/// `yield`.
	Yield,
	/// `checkpoint`.
	Checkpoint,
	/// `rewind`.
	Rewind,
	/// `task`.
	Task,
	/// `hub`.
	Hub,
	/// `browser`.
	Browser,
	/// `computer`.
	Computer,
	/// `security_scan`.
	SecurityScan,
	/// `retain`.
	Retain,
	/// `recall`.
	Recall,
	/// `reflect`.
	Reflect,
	/// `memory_edit`.
	MemoryEdit,
	/// `manage_skill`.
	ManageSkill,
	/// `learn`.
	Learn,
	/// `read`.
	Read,
	/// `edit`.
	Edit,
	/// `write`.
	Write,
	/// `lsp`.
	Lsp,
	/// `debug`.
	Debug,
	/// `grep`.
	Grep,
	/// `glob`.
	Glob,
	/// `ast_grep`.
	AstGrep,
	/// `ast_edit`.
	AstEdit,
	/// `eval`.
	Eval,
	/// `py_eval`. Unlisted: it shares `eval`'s executor and has no card.
	PyEval,
	/// `bash`.
	Bash,
}

/// One row of the production construction table.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BuiltinRosterEntry {
	/// Model-facing family name. Equal to [`Family::into_str`].
	pub name:                 &'static str,
	/// Omitted from ordinary user-facing lists.
	pub hidden:               bool,
	/// Present in [`builtin_tool_identities`]. `py_eval` is constructed but
	/// unlisted.
	pub listed:               bool,
	/// Construction phase.
	pub phase:                InstallPhase,
	/// Reserve the name for an `omp/core` claim.
	pub protect_core:         bool,
	/// Refuse to start if the name is already live.
	pub ensure_absent:        bool,
	/// Presentation used when the executor is registered.
	pub presentation:         PresentationClass,
	/// Presentation used when the environment half is declared remotely.
	pub declare_presentation: PresentationClass,
	/// Claims used when the executor is registered.
	pub claims:               ClaimsClass,
	/// Claims used when the environment half is declared remotely.
	pub declare_claims:       ClaimsClass,
	/// `tool_settings` key. `None` means the row is not gated by that map.
	pub settings_key:         Option<&'static str>,
	/// Bridges the constructor reads.
	pub bridges:              BridgeNeeds,
	/// Closed family tag.
	pub family:               Family,
}

/// Failure to reserve a built-in name that is already live.
#[derive(Debug, thiserror::Error)]
#[error("built-in tool {name} is already registered")]
pub struct DuplicateBuiltin {
	/// The family name that was already live.
	pub name: &'static str,
}

/// Registration invoked after the session-locus rows.
///
/// Production leaves this empty. A test crate supplies a probe here so the
/// capability is composed without editing `omp-envd`.
pub type RosterExtension = fn(&mut Registry) -> Result<(), RegistryError>;

/// The built-ins a composition installs.
///
/// `None` on [`crate`] consumers means [`SelectedRoster::production`]: every
/// production row and no extension. The mask is a bit per table index.
#[derive(Clone, Copy, Debug)]
pub struct SelectedRoster {
	mask:      u64,
	extension: Option<RosterExtension>,
}

const fn roster_row(
	family: Family,
	hidden: bool,
	listed: bool,
	phase: InstallPhase,
	protect_core: bool,
	presentation: PresentationClass,
	declare_presentation: PresentationClass,
	claims: ClaimsClass,
	declare_claims: ClaimsClass,
	settings: bool,
	bridges: BridgeNeeds,
) -> BuiltinRosterEntry {
	BuiltinRosterEntry {
		name: family.into_str(),
		hidden,
		listed,
		phase,
		protect_core,
		ensure_absent: true,
		presentation,
		declare_presentation,
		claims,
		declare_claims,
		settings_key: if settings {
			Some(family.into_str())
		} else {
			None
		},
		bridges,
		family,
	}
}

const fn same(
	family: Family,
	hidden: bool,
	listed: bool,
	phase: InstallPhase,
	protect_core: bool,
	presentation: PresentationClass,
	claims: ClaimsClass,
	settings: bool,
	bridges: BridgeNeeds,
) -> BuiltinRosterEntry {
	roster_row(
		family,
		hidden,
		listed,
		phase,
		protect_core,
		presentation,
		presentation,
		claims,
		claims,
		settings,
		bridges,
	)
}

use ClaimsClass as Claim;
use InstallPhase as Phase;
use PresentationClass as Present;

/// Production construction order.
///
/// Order is load-bearing: telemetry upload starts when `github` is built,
/// resource resolvers are created between [`InstallPhase::EnvironmentEarly`]
/// and [`InstallPhase::Environment`], and `bash` is last so its sibling list
/// sees the slots registered before it.
pub static ROSTER: &[BuiltinRosterEntry] = &[
	same(
		Family::ImageGen,
		false,
		true,
		Phase::Session,
		false,
		Present::LongTail,
		Claim::BuiltinDevice,
		false,
		BridgeNeeds::SEARCH.or(BridgeNeeds::BLOBS),
	),
	same(
		Family::Tts,
		false,
		true,
		Phase::Session,
		false,
		Present::LongTail,
		Claim::BuiltinDevice,
		false,
		BridgeNeeds::SEARCH
			.or(BridgeNeeds::GITHUB)
			.or(BridgeNeeds::BLOBS),
	),
	same(
		Family::ReportIssue,
		true,
		true,
		Phase::Session,
		false,
		Present::LongTail,
		Claim::BuiltinDevice,
		false,
		BridgeNeeds::TELEMETRY,
	),
	same(
		Family::Github,
		false,
		true,
		Phase::Session,
		false,
		Present::LongTail,
		Claim::BuiltinDevice,
		false,
		BridgeNeeds::GITHUB.or(BridgeNeeds::BLOBS),
	),
	same(
		Family::WebSearch,
		false,
		true,
		Phase::Session,
		false,
		Present::LongTail,
		Claim::LongTail,
		true,
		BridgeNeeds::SEARCH,
	),
	same(
		Family::Todo,
		false,
		true,
		Phase::Session,
		false,
		Present::LongTail,
		Claim::LongTail,
		true,
		BridgeNeeds::NONE,
	),
	same(
		Family::Ask,
		false,
		true,
		Phase::Session,
		false,
		Present::LongTail,
		Claim::LongTail,
		true,
		BridgeNeeds::ASK.or(BridgeNeeds::SEARCH),
	),
	same(
		Family::Think,
		true,
		true,
		Phase::Session,
		false,
		Present::Hidden,
		Claim::Core,
		true,
		BridgeNeeds::NONE,
	),
	same(
		Family::Goal,
		true,
		true,
		Phase::Session,
		false,
		Present::Hidden,
		Claim::Core,
		false,
		BridgeNeeds::GOAL,
	),
	same(
		Family::Yield,
		true,
		true,
		Phase::Session,
		false,
		Present::Hidden,
		Claim::Core,
		true,
		BridgeNeeds::NONE,
	),
	same(
		Family::Checkpoint,
		false,
		true,
		Phase::Session,
		false,
		Present::LongTail,
		Claim::LongTail,
		true,
		BridgeNeeds::CHECKPOINT,
	),
	same(
		Family::Rewind,
		false,
		true,
		Phase::Session,
		false,
		Present::LongTail,
		Claim::LongTail,
		true,
		BridgeNeeds::CHECKPOINT,
	),
	same(
		Family::Task,
		false,
		true,
		Phase::Composer,
		true,
		Present::Slot,
		Claim::Core,
		false,
		BridgeNeeds::SUBAGENT,
	),
	same(
		Family::Hub,
		false,
		true,
		Phase::Composer,
		true,
		Present::Slot,
		Claim::Core,
		false,
		BridgeNeeds::SUBAGENT,
	),
	#[cfg(feature = "cap-browser")]
	same(
		Family::Browser,
		false,
		true,
		Phase::EnvironmentEarly,
		true,
		Present::LongTail,
		Claim::BuiltinDevice,
		true,
		BridgeNeeds::BROWSER.or(BridgeNeeds::BLOBS),
	),
	#[cfg(feature = "cap-computer")]
	same(
		Family::Computer,
		false,
		true,
		Phase::EnvironmentEarly,
		true,
		Present::LongTail,
		Claim::BuiltinDevice,
		true,
		BridgeNeeds::COMPUTER.or(BridgeNeeds::BLOBS),
	),
	same(
		Family::SecurityScan,
		false,
		true,
		Phase::EnvironmentEarly,
		true,
		Present::Device,
		Claim::BuiltinDevice,
		true,
		BridgeNeeds::SECURITY.or(BridgeNeeds::GITHUB),
	),
	same(
		Family::Retain,
		false,
		true,
		Phase::EnvironmentEarly,
		false,
		Present::LongTail,
		Claim::BuiltinDevice,
		false,
		BridgeNeeds::MEMORY,
	),
	same(
		Family::Recall,
		false,
		true,
		Phase::EnvironmentEarly,
		false,
		Present::LongTail,
		Claim::BuiltinDevice,
		false,
		BridgeNeeds::MEMORY,
	),
	same(
		Family::Reflect,
		false,
		true,
		Phase::EnvironmentEarly,
		false,
		Present::LongTail,
		Claim::BuiltinDevice,
		false,
		BridgeNeeds::MEMORY,
	),
	same(
		Family::MemoryEdit,
		false,
		true,
		Phase::EnvironmentEarly,
		true,
		Present::LongTail,
		Claim::BuiltinDevice,
		false,
		BridgeNeeds::MEMORY,
	),
	roster_row(
		Family::ManageSkill,
		true,
		true,
		Phase::EnvironmentEarly,
		true,
		Present::Device,
		Present::LongTail,
		Claim::BuiltinDevice,
		Claim::BuiltinDevice,
		false,
		BridgeNeeds::SKILLS,
	),
	roster_row(
		Family::Learn,
		true,
		true,
		Phase::EnvironmentEarly,
		true,
		Present::Device,
		Present::LongTail,
		Claim::BuiltinDevice,
		Claim::BuiltinDevice,
		false,
		BridgeNeeds::SKILLS.or(BridgeNeeds::MEMORY),
	),
	same(
		Family::Read,
		false,
		true,
		Phase::Environment,
		true,
		Present::Essential,
		Claim::Core,
		true,
		BridgeNeeds::DOCUMENTS,
	),
	same(
		Family::Edit,
		false,
		true,
		Phase::Environment,
		true,
		Present::Essential,
		Claim::Core,
		true,
		BridgeNeeds::DOCUMENTS,
	),
	same(
		Family::Write,
		false,
		true,
		Phase::Environment,
		true,
		Present::LongTail,
		Claim::LongTail,
		true,
		BridgeNeeds::DOCUMENTS,
	),
	same(
		Family::Lsp,
		false,
		true,
		Phase::Environment,
		true,
		Present::LongTail,
		Claim::LongTail,
		true,
		BridgeNeeds::LSP.or(BridgeNeeds::DOCUMENTS),
	),
	same(
		Family::Debug,
		false,
		true,
		Phase::Environment,
		true,
		Present::LongTail,
		Claim::LongTail,
		true,
		BridgeNeeds::DEBUG.or(BridgeNeeds::DOCUMENTS),
	),
	same(
		Family::Grep,
		false,
		true,
		Phase::Environment,
		true,
		Present::Essential,
		Claim::Core,
		true,
		BridgeNeeds::WORKSPACE,
	),
	same(
		Family::Glob,
		false,
		true,
		Phase::Environment,
		true,
		Present::Essential,
		Claim::Core,
		true,
		BridgeNeeds::WORKSPACE,
	),
	same(
		Family::AstGrep,
		false,
		true,
		Phase::Environment,
		true,
		Present::LongTail,
		Claim::LongTail,
		true,
		BridgeNeeds::WORKSPACE,
	),
	same(
		Family::AstEdit,
		false,
		true,
		Phase::Environment,
		true,
		Present::LongTail,
		Claim::LongTail,
		true,
		BridgeNeeds::WORKSPACE,
	),
	same(
		Family::Eval,
		false,
		true,
		Phase::Environment,
		true,
		Present::LongTail,
		Claim::LongTail,
		true,
		BridgeNeeds::EVAL,
	),
	same(
		Family::PyEval,
		false,
		false,
		Phase::Environment,
		false,
		Present::LongTail,
		Claim::LongTail,
		false,
		BridgeNeeds::EVAL,
	),
	same(
		Family::Bash,
		false,
		true,
		Phase::Shell,
		true,
		Present::Bash,
		Claim::Core,
		true,
		BridgeNeeds::SHELL,
	),
];

const ROSTER_LEN: usize = ROSTER.len();
const _: () = assert!(ROSTER_LEN <= 64, "SelectedRoster mask is a u64");
const FULL_MASK: u64 = if ROSTER_LEN == 64 {
	u64::MAX
} else {
	(1_u64 << ROSTER_LEN) - 1
};

const IDENTITY_COUNT: usize = {
	let mut count = 0;
	let mut index = 0;
	while index < ROSTER_LEN {
		if ROSTER[index].listed {
			count += 1;
		}
		index += 1;
	}
	count
};

const IDENTITIES: [BuiltinToolIdentity; IDENTITY_COUNT] = {
	let mut identities = [BuiltinToolIdentity { name: "", hidden: false }; IDENTITY_COUNT];
	let mut index = 0;
	let mut out = 0;
	while index < ROSTER_LEN {
		let row = &ROSTER[index];
		if row.listed {
			identities[out] = BuiltinToolIdentity { name: row.name, hidden: row.hidden };
			out += 1;
		}
		index += 1;
	}
	identities
};

/// Returns the stable native builtin and hidden identity set.
///
/// Derived from [`ROSTER`]. `py_eval` is constructed with `eval` and is not
/// listed. `lsp` is listed. `browser` is listed while `cap-browser` is on.
pub const fn builtin_tool_identities() -> &'static [BuiltinToolIdentity] {
	&IDENTITIES
}

impl Default for SelectedRoster {
	fn default() -> Self {
		Self::production()
	}
}

impl SelectedRoster {
	/// Every production row and no extension.
	#[must_use]
	pub const fn production() -> Self {
		Self { mask: FULL_MASK, extension: None }
	}

	/// Registers `extension` after the session-locus rows.
	#[must_use]
	pub const fn with_extension(mut self, extension: RosterExtension) -> Self {
		self.extension = Some(extension);
		self
	}

	/// The composer extension, when one was supplied.
	#[must_use]
	pub const fn extension(self) -> Option<RosterExtension> {
		self.extension
	}

	/// Rows whose mask bit is set, in construction order.
	pub fn iter(&self) -> impl Iterator<Item = &'static BuiltinRosterEntry> + Clone {
		let mask = self.mask;
		ROSTER
			.iter()
			.enumerate()
			.filter(move |(index, _)| mask_contains(mask, *index))
			.map(|(_, row)| row)
	}
}

/// The production row for `family`, ignoring any selection mask.
#[must_use]
pub fn entry(family: Family) -> Option<&'static BuiltinRosterEntry> {
	ROSTER.iter().find(|row| row.family == family)
}

/// Phase recorded for `family` by [`phase_of`].
///
/// The table stores the same phase. Tests reject a row whose stored phase
/// disagrees with this match, so a new family updates both.
#[must_use]
pub const fn phase_of(family: Family) -> InstallPhase {
	match family {
		Family::ImageGen
		| Family::Tts
		| Family::ReportIssue
		| Family::Github
		| Family::WebSearch
		| Family::Todo
		| Family::Ask
		| Family::Think
		| Family::Goal
		| Family::Yield
		| Family::Checkpoint
		| Family::Rewind => InstallPhase::Session,
		Family::Task | Family::Hub => InstallPhase::Composer,
		Family::Browser
		| Family::Computer
		| Family::SecurityScan
		| Family::Retain
		| Family::Recall
		| Family::Reflect
		| Family::MemoryEdit
		| Family::ManageSkill
		| Family::Learn => InstallPhase::EnvironmentEarly,
		Family::Read
		| Family::Edit
		| Family::Write
		| Family::Lsp
		| Family::Debug
		| Family::Grep
		| Family::Glob
		| Family::AstGrep
		| Family::AstEdit
		| Family::Eval
		| Family::PyEval => InstallPhase::Environment,
		Family::Bash => InstallPhase::Shell,
	}
}

const fn mask_contains(mask: u64, index: usize) -> bool {
	mask & (1_u64 << index) != 0
}

/// Presentation of one class under `policy`.
#[must_use]
pub const fn presentation(class: PresentationClass, policy: ToolsPolicy) -> Presentation {
	match class {
		PresentationClass::Essential => {
			if matches!(policy, ToolsPolicy::DeviceOnly) {
				Presentation::Device
			} else {
				Presentation::Slot
			}
		},
		PresentationClass::Bash | PresentationClass::Slot => Presentation::Slot,
		PresentationClass::Hidden => Presentation::Hidden,
		PresentationClass::Device => Presentation::Device,
		PresentationClass::LongTail => {
			if matches!(policy, ToolsPolicy::ToolOnly) {
				Presentation::Slot
			} else {
				Presentation::Device
			}
		},
	}
}

/// Claims of one class under `policy`.
#[must_use]
pub const fn claims(class: ClaimsClass, policy: ToolsPolicy) -> Claims {
	match class {
		ClaimsClass::Core => core_claims(),
		ClaimsClass::BuiltinDevice => builtin_device_claims(),
		ClaimsClass::LongTail => {
			if matches!(policy, ToolsPolicy::ToolOnly) {
				core_claims()
			} else {
				builtin_device_claims()
			}
		},
	}
}

/// Presentation and claims used when an executor is registered.
#[must_use]
pub const fn registration(row: &BuiltinRosterEntry, policy: ToolsPolicy) -> (Presentation, Claims) {
	(presentation(row.presentation, policy), claims(row.claims, policy))
}

/// Presentation and claims used when an environment tool is declared remotely.
///
/// `learn` and `manage_skill` declare a long-tail device and register a
/// permanent device. Every other row uses the same class for both.
#[must_use]
pub const fn declaration(row: &BuiltinRosterEntry, policy: ToolsPolicy) -> (Presentation, Claims) {
	(presentation(row.declare_presentation, policy), claims(row.declare_claims, policy))
}

const fn core_claims() -> Claims {
	Claims { precedence: Precedence::CORE, claimant: omp_core::sf!("omp/core"), replaces: None }
}

const fn builtin_device_claims() -> Claims {
	Claims {
		precedence: Precedence::ENHANCEMENT,
		claimant:   omp_core::sf!("omp/core"),
		replaces:   None,
	}
}

/// Reserves protected names and rejects a live collision on ensured names.
pub fn guard(registry: &mut Registry, roster: &SelectedRoster) -> Result<(), DuplicateBuiltin> {
	registry.protect_core_claims(
		roster
			.iter()
			.filter(|row| row.protect_core)
			.map(|row| row.name),
	);
	for row in roster.iter().filter(|row| row.ensure_absent) {
		if registry.live_identity(row.name).is_some() {
			return Err(DuplicateBuiltin { name: row.name });
		}
	}
	Ok(())
}

fn admit(host: &impl ToolGate, row: &BuiltinRosterEntry) -> bool {
	row.settings_key.is_none_or(|key| host.tool_enabled(key))
}

/// Settings and policy answers shared by every installer.
pub trait ToolGate {
	/// Whether `key` is enabled. Missing keys are enabled.
	fn tool_enabled(&self, key: &str) -> bool;
	/// Session presentation policy.
	fn policy(&self) -> ToolsPolicy;
}

/// Session-locus parts supplied by the environment host.
///
/// The constructors in [`install_session`] call these and register the tool.
/// Image, speech, and issue tools are built by the host because their types
/// live in the environment crate.
pub trait SessionInstall: ToolGate {
	/// Error returned by registration. Registry failures convert into it.
	type Error: From<RegistryError>;
	/// Image device. The host owns the concrete tool type.
	type Image: Tool;
	/// Speech device. The host owns the concrete tool type.
	type Speech: Tool;
	/// Issue recorder. The host owns the concrete tool type.
	type Report: Tool;
	/// Search backend behind `web_search`.
	type Search: SearchBackend;
	/// Goal authority. Absent when the composition did not supply one.
	type Goal: GoalControl;
	/// Checkpoint and rewind share one control value.
	type Checkpoint: CheckpointControl;

	/// Registers one session-locus tool using the row's presentation and claims.
	fn register_session<T: Tool>(
		&mut self,
		tool: T,
		row: &BuiltinRosterEntry,
	) -> Result<(), Self::Error>;

	/// Invokes a composer extension against the live registry.
	fn register_extension(&mut self, extension: RosterExtension) -> Result<(), Self::Error>;

	/// Builds the image device, consuming the session image configuration.
	fn image_tool(&mut self) -> Self::Image;
	/// Builds the speech device, consuming the session speech configuration.
	fn speech_tool(&mut self) -> Self::Speech;
	/// Builds the issue recorder.
	fn report_tool(&mut self) -> Self::Report;
	/// Search backend shared with the media devices.
	fn search_backend(&self) -> Arc<Self::Search>;
	/// GitHub service. Starts telemetry upload the first time it is built.
	fn github_host(&mut self) -> Arc<dyn GithubHost>;
	/// Ask presenter.
	fn ask_presenter(&self) -> Arc<dyn AskPresenter>;
	/// Ask speech vocalizer.
	fn ask_vocalizer(&self) -> Arc<dyn AskVocalizer>;
	/// Goal control, when the composition supplied one.
	fn goal_control(&self) -> Option<Self::Goal>;
	/// Checkpoint control shared by `checkpoint` and `rewind`.
	fn checkpoint_control(&self) -> Self::Checkpoint;
}

/// Driver-owned `task` and `hub` constructors.
pub trait ComposerInstall: ToolGate {
	/// Spawner bound into `task`.
	type TaskSpawner: crate::task::SubagentSpawner;
	/// Backend bound into `hub`.
	type HubBackend: crate::hub::HubBackend;

	/// Builds one task spawner.
	fn task_spawner(&self) -> Self::TaskSpawner;
	/// Builds one hub backend.
	fn hub_backend(&self) -> Self::HubBackend;
	/// Records one composer tool for later registration.
	fn register_composer<T: Tool>(&mut self, tool: T, row: &BuiltinRosterEntry);
}

/// Facts and sink for remote environment declarations.
///
/// One row drives both this declaration and [`EnvironmentInstall`].
pub trait EnvironmentDeclare: ToolGate {
	/// Records one remote declaration using the row's declaration class.
	fn declare(&mut self, spec: ToolSpec, row: &BuiltinRosterEntry);
	/// Browser settings enabled the daemon.
	///
	/// Absent when `cap-browser` is off. `SV_BROWSER_ENABLED` is not compiled
	/// in that build, and a setting cannot reinstall a row the table omitted.
	#[cfg(feature = "cap-browser")]
	fn browser_enabled(&self) -> bool;
	/// Memory capability advertisement.
	fn memory_capabilities(&self) -> Capabilities;
	/// Managed skills are declared. This is the declaration predicate, which
	/// is wider than the construct predicate.
	fn managed_skills(&self) -> bool;
	/// `py_eval` was requested explicitly.
	fn py_eval_enabled(&self) -> bool;
	/// Eval prompt description. `None` skips the `eval` declaration.
	fn eval_description(&self) -> Option<omp_core::Str>;
	/// Shell prompt snapshot. `None` skips the `bash` declaration.
	fn shell_snapshot(&self) -> Option<ShellPromptSnapshot>;
	/// Read policy shared by the declaration and the executor.
	fn read_policy(&self) -> ReadPolicy;
	/// Search policy shared by `grep`, `glob`, and `ast_grep`.
	fn search_policy(&self) -> SearchPolicy;
	/// Edit revision registered last.
	fn selected_edit(&self) -> Rev;
}

/// Environment-locus constructors filled by the environment host.
///
/// The bodies stay on the host because they close over document, process, and
/// resolver authorities that are not object-safe. [`construct_environment`]
/// decides which body runs. `edit` is one family:
/// [`EnvironmentInstall::install_edit`] registers every dialect. `eval` and
/// `py_eval` share one executor.
pub trait EnvironmentInstall: ToolGate {
	/// Error returned by construction. Includes registry and executor failures.
	type Error: From<RegistryError>;

	/// Ensures the security service exists even when the tool is disabled.
	///
	/// Resolvers read the same service the tool uses.
	fn ensure_security_service(&mut self);
	/// Memory capability advertisement.
	fn memory_capabilities(&self) -> Capabilities;
	/// Managed skills may be constructed. Stricter than the declaration
	/// predicate.
	fn managed_skills(&self) -> bool;
	/// `py_eval` was requested explicitly.
	fn py_eval_enabled(&self) -> bool;
	/// Shell execution is enabled in addition to the `bash` tool setting.
	fn shell_enabled(&self) -> bool;

	/// Browser settings enabled the daemon.
	///
	/// Absent when `cap-browser` is off. The host does not compile the browser
	/// daemon in that build.
	#[cfg(feature = "cap-browser")]
	fn browser_enabled(&self) -> bool;

	/// Registers `browser` when the daemon is enabled.
	///
	/// Absent when `cap-browser` is off.
	#[cfg(feature = "cap-browser")]
	fn install_browser(&mut self, row: &BuiltinRosterEntry) -> Result<(), Self::Error>;
	/// Registers `computer`.
	///
	/// Absent when `cap-computer` is off. The host does not compile the
	/// desktop session in that build.
	#[cfg(feature = "cap-computer")]
	fn install_computer(&mut self, row: &BuiltinRosterEntry) -> Result<(), Self::Error>;
	/// Registers `security_scan`.
	fn install_security_scan(&mut self, row: &BuiltinRosterEntry) -> Result<(), Self::Error>;
	/// Registers `retain`.
	fn install_retain(&mut self, row: &BuiltinRosterEntry) -> Result<(), Self::Error>;
	/// Registers `recall`.
	fn install_recall(&mut self, row: &BuiltinRosterEntry) -> Result<(), Self::Error>;
	/// Registers `reflect`.
	fn install_reflect(&mut self, row: &BuiltinRosterEntry) -> Result<(), Self::Error>;
	/// Registers `memory_edit`.
	fn install_memory_edit(&mut self, row: &BuiltinRosterEntry) -> Result<(), Self::Error>;
	/// Registers `manage_skill`.
	fn install_manage_skill(&mut self, row: &BuiltinRosterEntry) -> Result<(), Self::Error>;
	/// Registers `learn`.
	fn install_learn(&mut self, row: &BuiltinRosterEntry) -> Result<(), Self::Error>;
	/// Registers `read`.
	fn install_read(&mut self, row: &BuiltinRosterEntry) -> Result<(), Self::Error>;
	/// Registers every `edit` dialect, selected revision last.
	fn install_edit(&mut self, row: &BuiltinRosterEntry) -> Result<(), Self::Error>;
	/// Registers `write`.
	fn install_write(&mut self, row: &BuiltinRosterEntry) -> Result<(), Self::Error>;
	/// Registers `lsp`.
	fn install_lsp(&mut self, row: &BuiltinRosterEntry) -> Result<(), Self::Error>;
	/// Registers `debug`.
	fn install_debug(&mut self, row: &BuiltinRosterEntry) -> Result<(), Self::Error>;
	/// Registers `grep`.
	fn install_grep(&mut self, row: &BuiltinRosterEntry) -> Result<(), Self::Error>;
	/// Registers `glob`.
	fn install_glob(&mut self, row: &BuiltinRosterEntry) -> Result<(), Self::Error>;
	/// Registers `ast_grep`.
	fn install_ast_grep(&mut self, row: &BuiltinRosterEntry) -> Result<(), Self::Error>;
	/// Registers `ast_edit`.
	fn install_ast_edit(&mut self, row: &BuiltinRosterEntry) -> Result<(), Self::Error>;
	/// Registers `eval`, building the shared executor on first use.
	fn install_eval(&mut self, row: &BuiltinRosterEntry) -> Result<(), Self::Error>;
	/// Registers `py_eval` on that same executor.
	fn install_py_eval(&mut self, row: &BuiltinRosterEntry) -> Result<(), Self::Error>;
	/// Registers `bash` after sibling slots and the dynamic device host exist.
	fn install_bash(&mut self, row: &BuiltinRosterEntry) -> Result<(), Self::Error>;
}

/// Installs the selected session-locus rows, then the roster extension.
pub fn install_session<H: SessionInstall>(
	host: &mut H,
	roster: &SelectedRoster,
) -> Result<(), H::Error> {
	for row in roster.iter() {
		if row.phase != InstallPhase::Session {
			continue;
		}
		if !admit(host, row) {
			continue;
		}
		install_session_row(host, row)?;
	}
	if let Some(extension) = roster.extension() {
		host.register_extension(extension)?;
	}
	Ok(())
}

fn install_session_row<H: SessionInstall>(
	host: &mut H,
	row: &BuiltinRosterEntry,
) -> Result<(), H::Error> {
	match row.family {
		Family::ImageGen => {
			let tool = host.image_tool();
			host.register_session(tool, row)
		},
		Family::Tts => {
			let tool = host.speech_tool();
			host.register_session(tool, row)
		},
		Family::ReportIssue => {
			let tool = host.report_tool();
			host.register_session(tool, row)
		},
		Family::Github => {
			let service = host.github_host();
			host.register_session(crate::github::tool(service), row)
		},
		Family::WebSearch => {
			let backend = host.search_backend();
			host.register_session(crate::web_search::tool(backend), row)
		},
		Family::Todo => host.register_session(crate::todo::tool(), row),
		Family::Ask => {
			let presenter = host.ask_presenter();
			let vocalizer = host.ask_vocalizer();
			host.register_session(crate::ask::tool_with_vocalizer(presenter, vocalizer), row)
		},
		Family::Think => host.register_session(crate::think::tool(), row),
		Family::Goal => {
			if let Some(control) = host.goal_control() {
				host.register_session(crate::goal::tool(control), row)?;
			}
			Ok(())
		},
		Family::Yield => host.register_session(crate::yield_tool::tool(), row),
		Family::Checkpoint => {
			let (checkpoint, _) = crate::checkpoint::tools(host.checkpoint_control());
			host.register_session(checkpoint, row)
		},
		Family::Rewind => {
			let (_, rewind) = crate::checkpoint::tools(host.checkpoint_control());
			host.register_session(rewind, row)
		},
		Family::Task
		| Family::Hub
		| Family::Browser
		| Family::Computer
		| Family::SecurityScan
		| Family::Retain
		| Family::Recall
		| Family::Reflect
		| Family::MemoryEdit
		| Family::ManageSkill
		| Family::Learn
		| Family::Read
		| Family::Edit
		| Family::Write
		| Family::Lsp
		| Family::Debug
		| Family::Grep
		| Family::Glob
		| Family::AstGrep
		| Family::AstEdit
		| Family::Eval
		| Family::PyEval
		| Family::Bash => Ok(()),
	}
}

/// Records composer rows (`task`, `hub`) for the driver to register.
pub fn install_composer<H: ComposerInstall>(host: &mut H, roster: &SelectedRoster) {
	for row in roster.iter() {
		if row.phase != InstallPhase::Composer || !admit(host, row) {
			continue;
		}
		match row.family {
			Family::Task => {
				let tool = crate::task::tool(host.task_spawner());
				host.register_composer(tool, row);
			},
			Family::Hub => {
				let tool = crate::hub::tool(host.hub_backend());
				host.register_composer(tool, row);
			},
			Family::ImageGen
			| Family::Tts
			| Family::ReportIssue
			| Family::Github
			| Family::WebSearch
			| Family::Todo
			| Family::Ask
			| Family::Think
			| Family::Goal
			| Family::Yield
			| Family::Checkpoint
			| Family::Rewind
			| Family::Browser
			| Family::Computer
			| Family::SecurityScan
			| Family::Retain
			| Family::Recall
			| Family::Reflect
			| Family::MemoryEdit
			| Family::ManageSkill
			| Family::Learn
			| Family::Read
			| Family::Edit
			| Family::Write
			| Family::Lsp
			| Family::Debug
			| Family::Grep
			| Family::Glob
			| Family::AstGrep
			| Family::AstEdit
			| Family::Eval
			| Family::PyEval
			| Family::Bash => {},
		}
	}
}

/// Declares the selected environment rows, including `bash` when a snapshot
/// exists.
pub fn declare_environment<H: EnvironmentDeclare>(host: &mut H, roster: &SelectedRoster) {
	for row in roster.iter() {
		if !matches!(
			row.phase,
			InstallPhase::EnvironmentEarly | InstallPhase::Environment | InstallPhase::Shell
		) {
			continue;
		}
		if !admit(host, row) {
			continue;
		}
		declare_row(host, row);
	}
}

fn declare_row<H: EnvironmentDeclare>(host: &mut H, row: &BuiltinRosterEntry) {
	match row.family {
		#[cfg(feature = "cap-browser")]
		Family::Browser => {
			if host.browser_enabled() {
				let spec = crate::browser::spec();
				host.declare(spec, row);
			}
		},
		// The variant stays so chat cards keep a stable `Family`. The row is
		// not in `ROSTER` in this build, so declaration never reaches here.
		#[cfg(not(feature = "cap-browser"))]
		Family::Browser => {},
		#[cfg(feature = "cap-computer")]
		Family::Computer => {
			let spec = crate::computer::spec();
			host.declare(spec, row);
		},
		// The variant stays so chat cards keep a stable `Family`. The row is
		// not in `ROSTER` in this build, so declaration never reaches here.
		#[cfg(not(feature = "cap-computer"))]
		Family::Computer => {},
		Family::SecurityScan => {
			let spec = crate::security_scan::spec();
			host.declare(spec, row);
		},
		Family::Retain => {
			if host.memory_capabilities().writable {
				let spec = crate::memory::retain_spec();
				host.declare(spec, row);
			}
		},
		Family::Recall => {
			if host.memory_capabilities().searchable {
				let spec = crate::memory::recall_spec();
				host.declare(spec, row);
			}
		},
		Family::Reflect => {
			if host.memory_capabilities().searchable {
				let spec = crate::memory::reflect_spec();
				host.declare(spec, row);
			}
		},
		Family::MemoryEdit => {
			if host.memory_capabilities().editable {
				let spec = crate::memory_edit::spec();
				host.declare(spec, row);
			}
		},
		Family::ManageSkill => {
			if host.managed_skills() {
				let spec = crate::manage_skill::spec();
				host.declare(spec, row);
			}
		},
		Family::Learn => {
			if host.managed_skills() && host.memory_capabilities().writable {
				let spec = crate::learn::spec();
				host.declare(spec, row);
			}
		},
		Family::Read => {
			let policy = host.read_policy();
			host.declare(crate::read::spec(policy), row);
		},
		Family::Edit => declare_edit(host, row),
		Family::Write => {
			let spec = crate::write::spec();
			host.declare(spec, row);
		},
		Family::Lsp => {
			let spec = crate::lsp::spec();
			host.declare(spec, row);
		},
		Family::Debug => {
			let spec = crate::debug::spec();
			host.declare(spec, row);
		},
		Family::Grep => {
			let policy = host.search_policy();
			host.declare(crate::grep::spec(policy), row);
		},
		Family::Glob => {
			let policy = host.search_policy();
			host.declare(crate::glob::spec(policy), row);
		},
		Family::AstGrep => {
			let policy = host.search_policy();
			host.declare(crate::ast_grep::spec(policy), row);
		},
		Family::AstEdit => {
			let spec = crate::ast_edit::spec();
			host.declare(spec, row);
		},
		Family::Eval => {
			if let Some(description) = host.eval_description() {
				host.declare(crate::eval::spec(description), row);
			}
		},
		Family::PyEval => {
			if host.py_eval_enabled() {
				let spec = crate::eval::py_eval_spec();
				host.declare(spec, row);
			}
		},
		Family::Bash => {
			if let Some(snapshot) = host.shell_snapshot() {
				host.declare(crate::shell::spec(&snapshot), row);
			}
		},
		Family::ImageGen
		| Family::Tts
		| Family::ReportIssue
		| Family::Github
		| Family::WebSearch
		| Family::Todo
		| Family::Ask
		| Family::Think
		| Family::Goal
		| Family::Yield
		| Family::Checkpoint
		| Family::Rewind
		| Family::Task
		| Family::Hub => {},
	}
}

fn declare_edit<H: EnvironmentDeclare>(host: &mut H, row: &BuiltinRosterEntry) {
	let selected = host.selected_edit();
	let mut edits = [
		crate::edit::replace::legacy_replace_spec(),
		crate::edit::apply_patch::legacy_patch_spec(),
		crate::edit::hashline_spec(),
		crate::edit::replace::replace_spec(),
		crate::edit::apply_patch::patch_spec(),
		crate::edit::apply_patch::apply_patch_spec(),
		crate::edit::apply_patch::sloppy_spec(),
	];
	edits.sort_by_key(|spec| spec.rev == selected);
	for spec in edits {
		host.declare(spec, row);
	}
}

/// Constructs one environment phase of `roster`.
///
/// Callers run [`InstallPhase::EnvironmentEarly`], then install resource
/// resolvers, then [`InstallPhase::Environment`], then the dynamic device
/// host, then [`InstallPhase::Shell`].
pub fn construct_environment<H: EnvironmentInstall>(
	host: &mut H,
	roster: &SelectedRoster,
	phase: InstallPhase,
) -> Result<(), H::Error> {
	for row in roster.iter() {
		if row.phase != phase {
			continue;
		}
		construct_row(host, row)?;
	}
	Ok(())
}

fn construct_row<H: EnvironmentInstall>(
	host: &mut H,
	row: &BuiltinRosterEntry,
) -> Result<(), H::Error> {
	if row.family == Family::SecurityScan {
		host.ensure_security_service();
	}
	if !admit(host, row) {
		return Ok(());
	}
	match row.family {
		#[cfg(feature = "cap-browser")]
		Family::Browser => {
			if host.browser_enabled() {
				host.install_browser(row)?;
			}
			Ok(())
		},
		#[cfg(not(feature = "cap-browser"))]
		Family::Browser => Ok(()),
		#[cfg(feature = "cap-computer")]
		Family::Computer => host.install_computer(row),
		#[cfg(not(feature = "cap-computer"))]
		Family::Computer => Ok(()),
		Family::SecurityScan => host.install_security_scan(row),
		Family::Retain => {
			if host.memory_capabilities().writable {
				host.install_retain(row)?;
			}
			Ok(())
		},
		Family::Recall => {
			if host.memory_capabilities().searchable {
				host.install_recall(row)?;
			}
			Ok(())
		},
		Family::Reflect => {
			if host.memory_capabilities().searchable {
				host.install_reflect(row)?;
			}
			Ok(())
		},
		Family::MemoryEdit => {
			if host.memory_capabilities().editable {
				host.install_memory_edit(row)?;
			}
			Ok(())
		},
		Family::ManageSkill => {
			if host.managed_skills() {
				host.install_manage_skill(row)?;
			}
			Ok(())
		},
		Family::Learn => {
			if host.managed_skills() && host.memory_capabilities().writable {
				host.install_learn(row)?;
			}
			Ok(())
		},
		Family::Read => host.install_read(row),
		Family::Edit => host.install_edit(row),
		Family::Write => host.install_write(row),
		Family::Lsp => host.install_lsp(row),
		Family::Debug => host.install_debug(row),
		Family::Grep => host.install_grep(row),
		Family::Glob => host.install_glob(row),
		Family::AstGrep => host.install_ast_grep(row),
		Family::AstEdit => host.install_ast_edit(row),
		Family::Eval => host.install_eval(row),
		Family::PyEval => {
			if host.py_eval_enabled() {
				host.install_py_eval(row)?;
			}
			Ok(())
		},
		Family::Bash => {
			if host.shell_enabled() {
				host.install_bash(row)?;
			}
			Ok(())
		},
		Family::ImageGen
		| Family::Tts
		| Family::ReportIssue
		| Family::Github
		| Family::WebSearch
		| Family::Todo
		| Family::Ask
		| Family::Think
		| Family::Goal
		| Family::Yield
		| Family::Checkpoint
		| Family::Rewind
		| Family::Task
		| Family::Hub => Ok(()),
	}
}

#[cfg(test)]
mod tests {
	use std::collections::BTreeSet;

	use strum::IntoEnumIterator as _;

	use super::*;

	fn family_in_construction_table(family: Family) -> bool {
		match family {
			#[cfg(not(feature = "cap-browser"))]
			Family::Browser => false,
			#[cfg(not(feature = "cap-computer"))]
			Family::Computer => false,
			_ => true,
		}
	}

	#[test]
	fn production_rows_are_unique_and_cover_every_family() {
		let names = ROSTER.iter().map(|row| row.name).collect::<BTreeSet<_>>();
		assert_eq!(names.len(), ROSTER.len());
		let families = ROSTER.iter().map(|row| row.family).collect::<BTreeSet<_>>();
		assert_eq!(families.len(), ROSTER.len());
		let compiled = Family::iter()
			.filter(|family| family_in_construction_table(*family))
			.count();
		assert_eq!(families.len(), compiled);
		for family in Family::iter().filter(|family| family_in_construction_table(*family)) {
			let row = entry(family).expect("row");
			assert_eq!(row.name, family.into_str());
			assert_eq!(row.phase, phase_of(family));
			assert!(row.ensure_absent);
			if let Some(key) = row.settings_key {
				assert_eq!(key, row.name);
			}
		}
		#[cfg(feature = "cap-browser")]
		assert!(entry(Family::Browser).is_some(), "browser row is in the construction table");
		#[cfg(not(feature = "cap-browser"))]
		assert!(entry(Family::Browser).is_none(), "browser row is compiled out");
		#[cfg(feature = "cap-computer")]
		assert!(entry(Family::Computer).is_some(), "computer row is in the construction table");
		#[cfg(not(feature = "cap-computer"))]
		assert!(entry(Family::Computer).is_none(), "computer row is compiled out");
	}

	#[test]
	fn identities_follow_listed_rows_and_name_browser_and_lsp() {
		let listed = ROSTER
			.iter()
			.filter(|row| row.listed)
			.map(|row| BuiltinToolIdentity { name: row.name, hidden: row.hidden })
			.collect::<Vec<_>>();
		assert_eq!(builtin_tool_identities(), listed.as_slice());
		#[cfg(feature = "cap-browser")]
		assert!(
			builtin_tool_identities()
				.iter()
				.any(|identity| identity.name == "browser")
		);
		#[cfg(not(feature = "cap-browser"))]
		assert!(
			builtin_tool_identities()
				.iter()
				.all(|identity| identity.name != "browser"),
			"browser is compiled out of the construction table"
		);
		assert!(
			builtin_tool_identities()
				.iter()
				.any(|identity| identity.name == "lsp")
		);
		assert!(
			builtin_tool_identities()
				.iter()
				.all(|identity| identity.name != "py_eval")
		);
		assert_eq!(
			builtin_tool_identities()
				.iter()
				.filter(|identity| identity.hidden)
				.count(),
			6
		);
	}

	#[test]
	fn protect_and_ensure_cover_the_registered_paths() {
		let protect = ROSTER
			.iter()
			.filter(|row| row.protect_core)
			.map(|row| row.name)
			.collect::<BTreeSet<_>>();
		for name in [
			"read",
			"write",
			"bash",
			"edit",
			"glob",
			"grep",
			"eval",
			"task",
			"hub",
			#[cfg(feature = "cap-browser")]
			"browser",
			"learn",
			"manage_skill",
			#[cfg(feature = "cap-computer")]
			"computer",
			"lsp",
			"debug",
			"security_scan",
			"ast_grep",
			"ast_edit",
			"memory_edit",
		] {
			assert!(protect.contains(name), "{name} is protected");
		}
		for name in ["think", "goal", "yield", "py_eval"] {
			assert!(!protect.contains(name), "{name} stays unprotected");
		}
		#[cfg(not(feature = "cap-browser"))]
		assert!(!protect.contains("browser"), "browser is compiled out of the construction table");
		#[cfg(not(feature = "cap-computer"))]
		assert!(!protect.contains("computer"), "computer is compiled out of the construction table");
		let phases = ROSTER.iter().map(|row| row.phase).collect::<BTreeSet<_>>();
		assert!(phases.contains(&InstallPhase::Session));
		assert!(phases.contains(&InstallPhase::Composer));
		assert!(phases.contains(&InstallPhase::EnvironmentEarly));
		assert!(phases.contains(&InstallPhase::Environment));
		assert!(phases.contains(&InstallPhase::Shell));
		let composer = ROSTER
			.iter()
			.filter(|row| row.phase == InstallPhase::Composer)
			.map(|row| row.name)
			.collect::<Vec<_>>();
		assert_eq!(composer, ["task", "hub"]);
		assert!(SelectedRoster::production().extension().is_none());
	}
}
