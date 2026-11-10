//! Operation vocabulary.
//!
//! [`CiOp::mutation`] is the approval flag. It lives on the contract so a
//! host implementor cannot report a write as a read.

#![allow(missing_docs, reason = "strum IntoStaticStr emits undocumented into_str")]

use std::mem::size_of;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use strum::{Display, EnumString, IntoStaticStr, VariantArray};

/// Whether an operation reads or writes.
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
pub enum Mutation {
	/// The operation does not change pipeline or job state.
	Read,
	/// The operation changes pipeline or job state and must be approved.
	Write,
}

/// A CI operation.
///
/// Names are the contract vocabulary. They are not product operation names.
/// [`Self::RerunFailed`] and [`Self::RerunJob`] are the two rerun writes.
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
	Serialize,
	Deserialize,
	JsonSchema,
)]
#[strum(serialize_all = "snake_case", ascii_case_insensitive, const_into_str)]
#[serde(rename_all = "snake_case")]
pub enum CiOp {
	/// Read pipeline and job status. Does not mutate.
	Status,
	/// Read artifact metadata. Does not mutate.
	Artifacts,
	/// Read a log page. Does not mutate.
	LogStream,
	/// Stop a pipeline or a job. Mutates.
	Cancel,
	/// Rerun the failed jobs of a pipeline. Mutates.
	RerunFailed,
	/// Rerun one job. Mutates.
	RerunJob,
	/// Start a pipeline, with inputs when the caller passed any. Mutates.
	Trigger,
	/// Play a job that is waiting for a person. Mutates.
	PlayManual,
}

const _: () = assert!(size_of::<CiOp>() <= 1, "CiOp must stay a byte");

impl CiOp {
	/// The approval class of this operation.
	///
	/// Cancel, both reruns, trigger, and play-manual write. Status,
	/// artifacts, and log stream read.
	#[inline]
	#[must_use]
	pub const fn mutation(self) -> Mutation {
		match self {
			Self::Status | Self::Artifacts | Self::LogStream => Mutation::Read,
			Self::Cancel | Self::RerunFailed | Self::RerunJob | Self::Trigger | Self::PlayManual => {
				Mutation::Write
			},
		}
	}

	/// Whether core must approval-gate this operation.
	#[inline]
	#[must_use]
	pub const fn mutates(self) -> bool {
		matches!(self.mutation(), Mutation::Write)
	}
}

#[cfg(test)]
mod tests {
	use strum::VariantArray;

	use super::*;

	#[test]
	fn mutation_flags_cover_every_operation() {
		for op in CiOp::VARIANTS {
			let expected = match op {
				CiOp::Status | CiOp::Artifacts | CiOp::LogStream => false,
				CiOp::Cancel
				| CiOp::RerunFailed
				| CiOp::RerunJob
				| CiOp::Trigger
				| CiOp::PlayManual => true,
			};
			assert_eq!(op.mutates(), expected, "{op}");
			assert_eq!(op.mutates(), op.mutation() == Mutation::Write);
			let name = op.into_str();
			assert_eq!(name.parse::<CiOp>().unwrap(), *op);
			assert_eq!(name.to_ascii_uppercase().parse::<CiOp>().unwrap(), *op);
		}
	}

	#[test]
	fn operation_names_roundtrip_through_json() {
		let json = serde_json::to_string(&CiOp::PlayManual).unwrap();
		assert_eq!(json, "\"play_manual\"");
		let parsed: CiOp = serde_json::from_str(&json).unwrap();
		assert_eq!(parsed, CiOp::PlayManual);
	}
}
