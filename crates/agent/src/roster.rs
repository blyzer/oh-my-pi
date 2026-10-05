//! The wire roster: the tool declarations one session advertises.
//!
//! ADR 0024 rule 2: the roster never changes mid-session, because the tool
//! array sits at the front of every provider's cached prefix and any change
//! to it misses the cache from the first byte. The roster is therefore a
//! function of the session's composition and the route's lowering
//! capabilities only, latched on the first request of a session and shared
//! (`Arc<[ToolDefinition]>`) by every request after it:
//!
//! - every `Slot` declaration, intersected with the `sv_tools` allowlist the
//!   session was composed with (`--tools`, agent cfg; a Director's bind is not
//!   composition),
//! - plus the hidden mounts decided at composition (`think`) or by the first
//!   user-initiated engagement (`goal`, mounted at a turn boundary and never
//!   unmounted),
//! - minus the session tools that withhold their declaration (`task` at the
//!   recursion ceiling), read once.
//!
//! Everything else that used to be expressed by omitting a tool (Plan and
//! Vibe binds, `sv_tools` writes, the `turn_start` hook's `enabled_tools`) is
//! enforced at dispatch through [`omp_tool::ToolRestrictions`].
//!
//! The roster is re-lowered only at an explicit boundary: a route whose
//! lowering capabilities differ (a model switch), or a replaced host tool
//! roster.

use std::{
	path::{Path, PathBuf},
	sync::Arc,
};

use omp_ai::{ToolDefinition, ToolInputConstraint};
use omp_catalog::GrammarBits;
use omp_core::{Hash32, Str};
use omp_tool::{LoweringCaps, Registry, RegistryError, ToolIdentity};

use crate::directors::plan::PLAN_READ_ONLY_TOOLS;

/// The hidden Goal lifecycle tool, mounted by the first goal engagement.
pub(crate) const GOAL: &str = "goal";
/// The hidden scratchpad tool, mounted at composition by
/// `ai_external_thinking`.
pub(crate) const THINK: &str = "think";

/// Hidden tools that join the roster by a mount decision instead of by being
/// declared as a slot.
const MOUNTS: [&str; 2] = [GOAL, THINK];

/// Whether `name` is a hidden tool that joins the roster by a mount decision.
pub(crate) fn is_mount(name: &str) -> bool {
	MOUNTS.contains(&name)
}

/// Everything about the session that decides the wire roster, read once.
pub(crate) struct Composition {
	/// The composition-time `sv_tools` allowlist; `None` advertises every slot.
	pub allowlist: Option<Arc<[Str]>>,
	/// Session tools that withheld their declaration when the roster latched.
	pub withheld:  Arc<[Str]>,
	/// Whether `think` was mounted at composition.
	pub think:     bool,
	/// Whether the session is a subagent of a plan-mode session: its wire
	/// roster is capped at the read-only tools from its first request.
	pub read_only: bool,
}

/// The route capabilities lowering depends on.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct CapsKey {
	strict_schema: bool,
	grammar:       GrammarBits,
	maximum_tools: Option<u16>,
}

impl From<LoweringCaps> for CapsKey {
	fn from(caps: LoweringCaps) -> Self {
		Self {
			strict_schema: caps.strict_schema,
			grammar:       caps.grammar,
			maximum_tools: caps.maximum_tools,
		}
	}
}

/// One lowering of the latched roster for one capabilities key.
#[derive(Clone)]
pub(crate) struct Lowered {
	key:             CapsKey,
	host_generation: u64,
	/// The declarations, shared by every request.
	pub tools:       Arc<[ToolDefinition]>,
	/// Their names, in wire order.
	pub names:       Arc<[Str]>,
	/// The hidden mounts this roster does not carry; a call to one is refused.
	pub unmounted:   Arc<[Str]>,
	identities:      Arc<[ToolIdentity]>,
	/// Digest of the declarations as the codecs receive them.
	pub fingerprint: Hash32,
}

/// The latched wire roster of one kernel/session pair.
#[derive(Default)]
pub(crate) struct WireRoster {
	/// The session the latch belongs to; another session starts a new latch.
	journal:     Option<PathBuf>,
	composition: Option<Composition>,
	/// Whether the first goal engagement has mounted `goal`.
	goal:        bool,
	lowered:     Option<Lowered>,
	/// Fingerprint of the roster the previous request carried.
	announced:   Option<Hash32>,
}

impl WireRoster {
	fn bind(&mut self, journal: &Path) {
		if self.journal.as_deref() != Some(journal) {
			*self = Self { journal: Some(journal.to_path_buf()), ..Self::default() };
		}
	}

	/// Mounts `goal` at a turn boundary once a goal has been engaged in
	/// `journal`'s session. Monotonic: the mount survives the goal completing,
	/// dropping, pausing, or leaving the session, because unmounting would
	/// change the cached prefix again.
	pub(crate) fn observe_boundary(&mut self, journal: &Path, goal_engaged: bool) {
		self.bind(journal);
		if goal_engaged && !self.goal {
			self.goal = true;
			self.lowered = None;
		}
	}

	/// Whether the composition inputs were already read for `journal`'s session.
	pub(crate) fn is_composed(&mut self, journal: &Path) -> bool {
		self.bind(journal);
		self.composition.is_some()
	}

	/// Latches the composition inputs.
	pub(crate) fn compose(&mut self, composition: Composition) {
		self.composition = Some(composition);
	}

	/// Forgets the lowering (the registry was replaced between turns); the
	/// composition stays latched.
	pub(crate) fn invalidate(&mut self) {
		self.lowered = None;
	}

	/// The latched roster lowered for `caps`, re-lowered only when the
	/// capabilities key or the host tool roster changed.
	pub(crate) fn lowered(
		&mut self,
		registry: &Registry,
		caps: LoweringCaps,
	) -> Result<&Lowered, RegistryError> {
		let key = CapsKey::from(caps);
		let host_generation = registry.host_roster_generation();
		let fresh = self
			.lowered
			.as_ref()
			.is_some_and(|lowered| lowered.key == key && lowered.host_generation == host_generation);
		if !fresh {
			self.lowered = Some(self.lower(registry, caps, key, host_generation)?);
		}
		Ok(self.lowered.as_ref().expect("lowered above"))
	}

	fn lower(
		&self,
		registry: &Registry,
		caps: LoweringCaps,
		key: CapsKey,
		host_generation: u64,
	) -> Result<Lowered, RegistryError> {
		let composition = self
			.composition
			.as_ref()
			.expect("the composition latches before the roster lowers");
		let mut tools = registry.advertise(caps)?;
		// Mounts join by decision only, whatever their presentation.
		tools.retain(|tool| !is_mount(tool.definition.name.as_str()));
		if let Some(allowlist) = &composition.allowlist {
			tools.retain(|tool| allowlist.contains(&tool.definition.name));
		}
		if composition.read_only {
			tools.retain(|tool| PLAN_READ_ONLY_TOOLS.contains(&tool.definition.name.as_str()));
		}
		tools.retain(|tool| !composition.withheld.contains(&tool.definition.name));
		let mut mounts = smallvec::SmallVec::<Str, 2>::new();
		if self.goal {
			mounts.push(Str::new_static(GOAL));
		}
		if composition.think {
			mounts.push(Str::new_static(THINK));
		}
		if !mounts.is_empty() {
			tools.extend(registry.advertise_selected(caps, &mounts)?);
		}
		if let Some(limit) = caps.maximum_tools
			&& tools.len() >= usize::from(limit)
		{
			tracing::debug!(limit, "the route's tool-count budget truncates the latched roster");
		}
		let definitions = tools
			.iter()
			.map(|tool| tool.definition.clone())
			.collect::<Arc<[_]>>();
		let names = definitions
			.iter()
			.map(|tool| tool.name.clone())
			.collect::<Arc<[_]>>();
		Ok(Lowered {
			key,
			host_generation,
			unmounted: MOUNTS
				.into_iter()
				.filter(|mount| !names.iter().any(|name| name == mount))
				.map(Str::new_static)
				.collect(),
			names,
			identities: tools.into_iter().map(|tool| tool.identity).collect(),
			fingerprint: roster_fingerprint(&definitions),
			tools: definitions,
		})
	}

	/// The latched declaration identity of `name`, for a call to a tool the
	/// wire declared but the registry no longer resolves.
	pub(crate) fn identity(&self, name: &str) -> Option<&ToolIdentity> {
		self
			.lowered
			.as_ref()?
			.identities
			.iter()
			.find(|identity| identity.name.as_str() == name)
	}

	/// The fingerprint of the roster last lowered, if any.
	pub(crate) fn fingerprint(&self) -> Option<Hash32> {
		self.lowered.as_ref().map(|lowered| lowered.fingerprint)
	}

	/// Records the fingerprint the request about to leave carries, returning
	/// whether it differs from the previous request's (always true for the
	/// first).
	pub(crate) fn announce(&mut self, fingerprint: Hash32) -> bool {
		self.announced.replace(fingerprint) != Some(fingerprint)
	}
}

/// Digest of a roster's declarations: names, descriptions, and input
/// declarations, in order. Equal digests mean equal tool arrays on the wire.
#[must_use]
pub fn roster_fingerprint(tools: &[ToolDefinition]) -> Hash32 {
	let mut hasher = Hash32::hasher();
	let mut field = |bytes: &[u8]| {
		hasher.update(&(bytes.len() as u64).to_le_bytes());
		hasher.update(bytes);
	};
	for tool in tools {
		field(tool.name.as_bytes());
		field(tool.description.as_deref().unwrap_or_default().as_bytes());
		match &tool.input {
			ToolInputConstraint::JsonSchema { parameters, strict } => {
				field(&[0, u8::from(*strict)]);
				field(parameters.as_value().to_string().as_bytes());
			},
			ToolInputConstraint::Grammar { grammar, fallback } => {
				field(&[1]);
				field(<&'static str>::from(grammar.syntax).as_bytes());
				field(grammar.definition.as_bytes());
				field(fallback.as_value().to_string().as_bytes());
			},
		}
	}
	hasher.finalize()
}
