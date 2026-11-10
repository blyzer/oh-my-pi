//! Checks attached to a change request or a commit.
//!
//! [`required_checks`] is the set the gate looks at. A required check
//! satisfies the gate only when its state is [`CheckState::Success`].
//! The host decides which checks are required; this crate does not.

#![allow(missing_docs, reason = "strum IntoStaticStr emits undocumented into_str")]

use std::{iter::FusedIterator, mem::size_of, sync::Arc};

use omp_core::{IntoStr, Str};
use serde::{Deserialize, Serialize};
use strum::{Display, EnumString, IntoStaticStr};

/// Whether a check passed, failed, or is still running.
#[derive(
	Debug,
	Clone,
	Copy,
	PartialEq,
	Eq,
	Hash,
	Display,
	EnumString,
	IntoStaticStr,
	Serialize,
	Deserialize,
)]
#[strum(serialize_all = "snake_case", ascii_case_insensitive, const_into_str)]
#[serde(rename_all = "snake_case")]
pub enum CheckState {
	/// The check has not finished.
	Pending,
	/// The check passed.
	Success,
	/// The check failed.
	Failure,
	/// The check errored before it could pass or fail.
	Error,
	/// The check does not apply. A required neutral check still blocks.
	Neutral,
}

impl CheckState {
	/// Whether a required check in this state satisfies the gate.
	///
	/// Only [`Self::Success`] does. [`Self::Neutral`] does not: the host
	/// clears [`Check::required`] when a check is not a gate.
	#[inline]
	#[must_use]
	pub const fn satisfies_required(self) -> bool {
		matches!(self, Self::Success)
	}
}

/// One named check.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Check {
	/// Check name, as the forge reports it.
	pub name:     Str,
	/// Current state.
	pub state:    CheckState,
	/// Whether the gate requires this check to succeed.
	pub required: bool,
	/// Browser URL. Absent when the forge has none.
	pub web_url:  Option<Str>,
}

const _: () = assert!(size_of::<Check>() <= 96, "Check must stay compact");

impl Check {
	/// A check with no browser URL.
	#[must_use]
	pub fn new(name: impl IntoStr, state: CheckState, required: bool) -> Self {
		Self { name: name.into_str(), state, required, web_url: None }
	}

	/// Sets the browser URL.
	#[must_use]
	pub fn with_web_url(mut self, url: impl IntoStr) -> Self {
		self.web_url = Some(url.into_str());
		self
	}
}

/// What a check set is attached to.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CheckTarget {
	/// Checks on a change request.
	ChangeRequest {
		/// Change-request number.
		number: u64,
	},
	/// Checks on a commit identity.
	Commit {
		/// Opaque commit identity.
		id: Str,
	},
}

/// The checks a forge reported for one target.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForgeChecks {
	/// Change request or commit the checks belong to.
	pub target: CheckTarget,
	/// Checks in forge order.
	pub checks: Arc<[Check]>,
}

const _: () = assert!(size_of::<ForgeChecks>() <= 64, "ForgeChecks must stay compact");

impl ForgeChecks {
	/// Collects `checks` into one shared slice.
	#[must_use]
	pub fn new(target: CheckTarget, checks: impl IntoIterator<Item = Check>) -> Self {
		Self { target, checks: checks.into_iter().collect() }
	}

	/// Every check, in forge order.
	#[must_use]
	pub fn as_slice(&self) -> &[Check] {
		&self.checks
	}
}

/// Required checks, in forge order.
pub fn required_checks(
	checks: &ForgeChecks,
) -> impl DoubleEndedIterator<Item = &Check> + FusedIterator + Clone + '_ {
	checks.checks.iter().filter(|check| check.required)
}

/// Required checks that are not [`CheckState::Success`], in forge order.
pub fn unsatisfied_required(
	checks: &ForgeChecks,
) -> impl DoubleEndedIterator<Item = &Check> + FusedIterator + Clone + '_ {
	required_checks(checks).filter(|check| !check.state.satisfies_required())
}

/// Whether every required check succeeded.
///
/// An empty required set is satisfied.
#[must_use]
pub fn required_satisfied(checks: &ForgeChecks) -> bool {
	unsatisfied_required(checks).next().is_none()
}

#[cfg(test)]
mod tests {
	use super::*;

	fn checks(items: impl IntoIterator<Item = Check>) -> ForgeChecks {
		ForgeChecks::new(CheckTarget::ChangeRequest { number: 1 }, items)
	}

	#[test]
	fn required_set_keeps_forge_order_and_ignores_optional_checks() {
		let set = checks([
			Check::new("lint", CheckState::Success, true),
			Check::new("extra", CheckState::Failure, false),
			Check::new("test", CheckState::Pending, true),
		]);
		let names: Vec<&str> = required_checks(&set)
			.map(|check| check.name.as_str())
			.collect();
		assert_eq!(names, ["lint", "test"]);
		assert!(!required_satisfied(&set));
		let blocking: Vec<&str> = unsatisfied_required(&set)
			.map(|check| check.name.as_str())
			.collect();
		assert_eq!(blocking, ["test"]);
	}

	#[test]
	fn neutral_required_check_blocks_and_an_empty_gate_passes() {
		let neutral = checks([Check::new("lint", CheckState::Neutral, true)]);
		assert!(!neutral.checks[0].state.satisfies_required());
		assert!(!required_satisfied(&neutral));
		assert!(required_satisfied(&checks([])));
		let optional = checks([Check::new("lint", CheckState::Failure, false)]);
		assert!(required_satisfied(&optional));
	}
}
