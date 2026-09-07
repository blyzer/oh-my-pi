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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PhaseParams {
	pub name: String,
	pub kind: PhaseKind,
	/// Agent name for `Agent` phases, subsystem for `Code` (e.g. `git`).
	/// Never a model id — the roster resolves the model.
	pub owner: String,
	#[serde(default)]
	pub description: String,
}

impl PhaseParams {
	pub fn new(name: impl Into<String>, kind: PhaseKind, owner: impl Into<String>) -> Self {
		Self { name: name.into(), kind, owner: owner.into(), description: String::new() }
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
	pub name: String,
	pub kind: PhaseKind,
	pub owner: String,
	pub status: PhaseStatus,
	pub attempts: u32,
	pub summary: String,
	/// Gate evidence for the final attempt, green checks included.
	pub gates: Vec<GateReport>,
	/// Why the phase was rejected: gate violations PLUS non-gate ones such as
	/// a self-reported `fail` or an unparseable envelope. Empty when passed.
	/// A red test suite has no gate to blame, so without this a failed phase
	/// would report no reason at all.
	pub violations: Vec<String>,
}
