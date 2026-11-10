//! Provider identity and the capability flags a provider implements.
//!
//! [`CiProvider`] is opaque. [`Capabilities`] is the closed flag set from
//! ADR 0049 D2. [`Capabilities::allows`] answers from those flags, so a host
//! cannot advertise a write that the contract calls a read.

use std::mem::size_of;

use omp_core::{IntoStr, Str};
use serde::{Deserialize, Serialize};

use crate::{CiOp, IdError, error::check_id};

/// One CI provider.
///
/// `id` is opaque operator data. [`crate::route`] uses the details-URL host.
/// This type does not name a product.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CiProvider {
	/// Opaque provider key.
	pub id: Str,
}

const _: () = assert!(size_of::<CiProvider>() <= 24, "CiProvider must stay one string");

impl CiProvider {
	/// Builds a provider after checking the id.
	///
	/// # Errors
	///
	/// Returns [`IdError`] when `id` is empty, too long, or outside the
	/// identifier charset.
	pub fn try_new(id: impl IntoStr) -> Result<Self, IdError> {
		let id = id.into_str();
		check_id(&id)?;
		Ok(Self { id })
	}
}

/// Which contract operations a provider implements.
///
/// Status is not a flag. Every provider that can be routed can be observed
/// for status. The other flags gate the operation of the same name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub struct Capabilities {
	/// [`CiOp::RerunFailed`].
	pub rerun_failed:        bool,
	/// [`CiOp::RerunJob`].
	pub rerun_job:           bool,
	/// [`CiOp::Cancel`].
	pub cancel:              bool,
	/// [`CiOp::Trigger`], including a trigger that carries inputs.
	pub trigger_with_inputs: bool,
	/// [`CiOp::Artifacts`].
	pub artifacts:           bool,
	/// [`CiOp::LogStream`].
	pub log_stream:          bool,
	/// [`CiOp::PlayManual`].
	pub play_manual:         bool,
}

const _: () = assert!(size_of::<Capabilities>() <= 8, "Capabilities must stay eight flags");

impl Capabilities {
	/// A provider that can be observed for status and implements nothing else.
	#[must_use]
	pub const fn none() -> Self {
		Self {
			rerun_failed:        false,
			rerun_job:           false,
			cancel:              false,
			trigger_with_inputs: false,
			artifacts:           false,
			log_stream:          false,
			play_manual:         false,
		}
	}

	/// Whether this provider implements `op`.
	///
	/// [`CiOp::Status`] is always allowed. A write is allowed only when its
	/// flag is set, which keeps the capability bit on the same side of
	/// [`CiOp::mutates`] as the operation.
	#[inline]
	#[must_use]
	pub const fn allows(self, op: CiOp) -> bool {
		match op {
			CiOp::Status => true,
			CiOp::Artifacts => self.artifacts,
			CiOp::LogStream => self.log_stream,
			CiOp::Cancel => self.cancel,
			CiOp::RerunFailed => self.rerun_failed,
			CiOp::RerunJob => self.rerun_job,
			CiOp::Trigger => self.trigger_with_inputs,
			CiOp::PlayManual => self.play_manual,
		}
	}
}

#[cfg(test)]
mod tests {
	use strum::VariantArray;

	use super::*;
	use crate::{CiOp, IdError};

	#[test]
	fn none_allows_status_only_and_each_flag_gates_its_operation() {
		let none = Capabilities::none();
		assert_eq!(none, Capabilities::default());
		for op in CiOp::VARIANTS {
			assert_eq!(none.allows(*op), *op == CiOp::Status, "{op}");
			if op.mutates() {
				assert!(!none.allows(*op), "{op}");
			}
		}
		let caps = Capabilities { cancel: true, artifacts: true, ..Capabilities::none() };
		assert!(caps.allows(CiOp::Status));
		assert!(caps.allows(CiOp::Cancel));
		assert!(caps.allows(CiOp::Artifacts));
		assert!(!caps.allows(CiOp::Trigger));
		assert!(!caps.allows(CiOp::LogStream));
		assert!(!caps.allows(CiOp::RerunFailed));
		assert!(!caps.allows(CiOp::RerunJob));
		assert!(!caps.allows(CiOp::PlayManual));
	}

	#[test]
	fn provider_id_rejects_an_empty_string_and_a_separator() {
		assert_eq!(CiProvider::try_new(""), Err(IdError::Empty));
		assert_eq!(CiProvider::try_new("ci.example").unwrap().id.as_str(), "ci.example");
		assert_eq!(CiProvider::try_new("a/b"), Err(IdError::Invalid));
		assert_eq!(CiProvider::try_new(".."), Err(IdError::Invalid));
	}
}
