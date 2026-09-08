//! Phases: the unit of the trace and the only place work happens.

use serde::{Deserialize, Serialize};

use crate::gate::GateReport;

/// Three lanes, one primitive. `Engineer` and `Code` are executed by the
/// caller and reported back; only `Agent` costs tokens.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PhaseKind {
	Engineer,
	Agent,
	Code,
}

/// Explicit revision routing for a coherent negative review.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewRoute {
	pub to:            String,
	pub max_revisions: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PhaseParams {
	pub name:        String,
	pub kind:        PhaseKind,
	/// Agent name for `Agent` phases, subsystem for `Code` (e.g. `git`).
	/// Never a model id — the roster resolves the model.
	pub owner:       String,
	#[serde(default)]
	pub description: String,
	/// Where a rejected attempt sends the run instead of retrying in place.
	///
	/// A `Code` phase has no agent to correct: re-running the same command
	/// after a red test suite is deterministic, so it burns the budget and the
	/// agent that wrote the code never learns it broke. Naming an earlier phase
	/// here rewinds the run to it with the failure as its correction.
	///
	/// The engine only obeys the name; the caller decides what the default
	/// target is, so no phase-selection policy lives in here.
	#[serde(default)]
	pub rewind_to:   Option<String>,
	/// Independent of envelope/gate correction and deterministic code rewind.
	#[serde(default)]
	pub on_reject:   Option<ReviewRoute>,
	/// Phases that must pass before this one runs.
	///
	/// Omitted means the preceding declaration; an explicit empty list means
	/// independent. Workflow construction resolves omissions before sorting.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub depends_on:  Option<Vec<String>>,
	/// Accepted outputs this phase consumes, named by producer phase.
	///
	/// Independent of `depends_on`, which only orders execution: an input is a
	/// *data* edge. The engine resolves each producer's current accepted
	/// version at dispatch, records the selection in the trace, and hands the
	/// envelopes back on the step — so a phase between producer and consumer
	/// can no longer clobber the handoff the consumer was written against.
	/// A phase without inputs keeps the positional last-envelope handoff.
	#[serde(default)]
	pub inputs:      Vec<String>,
}

impl PhaseParams {
	pub fn new(name: impl Into<String>, kind: PhaseKind, owner: impl Into<String>) -> Self {
		Self {
			name: name.into(),
			kind,
			owner: owner.into(),
			description: String::new(),
			rewind_to: None,
			on_reject: None,
			depends_on: None,
			inputs: Vec::new(),
		}
	}

	/// Require `phases` to pass before this one runs.
	pub fn after(mut self, phases: impl IntoIterator<Item = impl Into<String>>) -> Self {
		self.depends_on = Some(phases.into_iter().map(Into::into).collect());
		self
	}

	/// Consume the accepted outputs of `phases`, resolved at dispatch.
	pub fn consuming(mut self, phases: impl IntoIterator<Item = impl Into<String>>) -> Self {
		self.inputs = phases.into_iter().map(Into::into).collect();
		self
	}

	/// Send a rejected attempt back to `phase` instead of retrying in place.
	pub fn rewinding_to(mut self, phase: impl Into<String>) -> Self {
		self.rewind_to = Some(phase.into());
		self
	}

	pub fn on_reject(mut self, target: impl Into<String>, max_revisions: u16) -> Self {
		self.on_reject = Some(ReviewRoute { to: target.into(), max_revisions });
		self
	}

	pub fn describe(mut self, description: impl Into<String>) -> Self {
		self.description = description.into();
		self
	}
}

/// Success must be earned: a phase is `Failed` until an envelope parses AND
/// every gate comes back green.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PhaseStatus {
	Passed,
	Failed,
}

#[derive(Debug, Clone, Serialize)]
pub struct PhaseRecord {
	pub name:        String,
	pub kind:        PhaseKind,
	pub owner:       String,
	pub status:      PhaseStatus,
	/// Historical evidence superseded by a rewind; never current acceptance.
	pub invalidated: bool,
	pub attempts:    u32,
	pub summary:     String,
	/// Gate evidence for the final attempt, green checks included.
	pub gates:       Vec<GateReport>,
	/// Why the phase was rejected: gate violations PLUS non-gate ones such as
	/// a self-reported `fail` or an unparseable envelope. Empty when passed.
	/// A red test suite has no gate to blame, so without this a failed phase
	/// would report no reason at all.
	pub violations:  Vec<String>,
}
