//! Closed set of pipeline and job states.
//!
//! A host maps provider-specific words onto this set before they cross the
//! contract. [`CiStatus::Manual`] is waiting for [`crate::CiOp::PlayManual`].

#![allow(missing_docs, reason = "strum IntoStaticStr emits undocumented into_str")]

use std::mem::size_of;

use strum::{Display, EnumString, IntoStaticStr, VariantArray};

/// Whether a pipeline or job is waiting, running, or finished.
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
	VariantArray,
	serde::Serialize,
	serde::Deserialize,
)]
#[strum(serialize_all = "snake_case", ascii_case_insensitive, const_into_str)]
#[serde(rename_all = "snake_case")]
pub enum CiStatus {
	/// Accepted and not started.
	Queued,
	/// Started and not finished.
	Running,
	/// Finished successfully.
	Success,
	/// Finished unsuccessfully.
	Failure,
	/// Stopped by cancel.
	Canceled,
	/// Did not run.
	Skipped,
	/// Waiting for a person to play the job.
	Manual,
}

const _: () = assert!(size_of::<CiStatus>() <= 1, "CiStatus must stay a byte");

impl CiStatus {
	/// Whether the pipeline or job will not change without a new operation.
	///
	/// [`Self::Queued`], [`Self::Running`], and [`Self::Manual`] are not
	/// terminal. The other states are.
	#[inline]
	#[must_use]
	pub const fn is_terminal(self) -> bool {
		match self {
			Self::Queued | Self::Running | Self::Manual => false,
			Self::Success | Self::Failure | Self::Canceled | Self::Skipped => true,
		}
	}
}

#[cfg(test)]
mod tests {
	use strum::VariantArray;

	use super::*;

	#[test]
	fn terminal_states_are_finished_and_names_roundtrip() {
		for status in CiStatus::VARIANTS {
			let terminal = match status {
				CiStatus::Queued | CiStatus::Running | CiStatus::Manual => false,
				CiStatus::Success | CiStatus::Failure | CiStatus::Canceled | CiStatus::Skipped => true,
			};
			assert_eq!(status.is_terminal(), terminal, "{status}");
			let name = status.into_str();
			assert_eq!(name.parse::<CiStatus>().unwrap(), *status);
			assert_eq!(name.to_ascii_uppercase().parse::<CiStatus>().unwrap(), *status);
		}
	}
}
