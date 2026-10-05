//! One module per lint (family); [`all`] assembles the rule set the engine
//! runs.

mod arc_struct;
mod error_format;
mod import_alias;
mod inline_path;
mod model_name;
mod mutex_arc;

use crate::lint::AnyLint;

/// Every lint, configured. `max_segments` is the `long-path` threshold.
pub fn all(max_segments: usize) -> Vec<Box<dyn AnyLint>> {
	vec![
		Box::new(import_alias::ImportAlias),
		Box::new(inline_path::LongPath { max_segments }),
		Box::new(inline_path::StdPath),
		Box::new(inline_path::TokioPath),
		Box::new(inline_path::RelativePath),
		Box::new(arc_struct::ArcStruct),
		Box::new(mutex_arc::MutexArc),
		Box::new(model_name::ModelGate),
		Box::new(model_name::ModelTable),
	]
}

/// The rules counted by the error-formatting ratchet ([`crate::ratchet`]).
/// Kept out of [`all`]: hundreds of legacy sites would drown a normal run.
pub fn ratchet() -> Vec<Box<dyn AnyLint>> {
	vec![Box::new(error_format::ErrorStrPayload), Box::new(error_format::ErrorFormat)]
}
