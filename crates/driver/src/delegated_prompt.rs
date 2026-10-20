//! Renders a bounded exact [`DelegatedPrompt`] from [`CanonicalPromptSource`].
//!
//! [`build_delegated_prompt`] is the single entry point. It produces the
//! exact prompt OMP would have sent to its own model — the same banded system
//! sections followed by volatile items — with the caller-supplied task text as
//! the user turn, and returns it alongside the caller-supplied [`TurnLimits`].
//!
//! The function adds no paraphrase or rewrite. [`DelegatedPrompt::digest`] is
//! the reference the host uses to detect backend rewriting; a backend that
//! reports a different value in [`TurnAccepted::forwarded`] causes the host to
//! end the session with [`ExecutorError::PromptRewritten`].

use omp_agent::{
	BandHash, CanonicalPromptSource,
	delegated::{DelegatedPrompt, PromptConversionError, TurnLimits},
	prompt::PromptError,
};
use omp_core::{Hash32, Str};
use omp_dom::Dom;
use thiserror::Error;

/// Maximum byte length of the task text in a delegated turn.
///
/// Matches `MAX_SESSION_PROMPT_BYTES` in `omp-envd` so the same guard applies
/// to delegated turns as to initial session prompts.
pub const MAX_TASK_BYTES: usize = 256 * 1024;

/// Why [`build_delegated_prompt`] could not produce a prompt.
#[derive(Debug, Error)]
pub enum BuildError {
	/// The task text is empty. An empty turn has no defined semantics.
	#[error("task text is empty")]
	EmptyTask,
	/// The task text is too long for a delegated turn.
	#[error("task text is {actual} bytes, which exceeds the delegated-turn limit")]
	TaskTooLong {
		/// Observed byte length.
		actual: usize,
	},
	/// Rendering the canonical prompt from the session DOM failed.
	#[error("prompt rendering failed")]
	Render(#[source] PromptError),
	/// A rendered item cannot be expressed as plain text.
	#[error("rendered prompt item cannot be converted for delegation")]
	Conversion(#[source] PromptConversionError),
}

/// Builds a [`DelegatedPrompt`] from the session DOM and a task text.
///
/// The rendered system items (Frozen, Stable, Dynamic bands) and volatile items
/// are exactly what [`CanonicalPromptSource`] would send to OMP's own model.
/// `task` becomes the user turn. `limits` is returned unmodified as the bound.
///
/// # Errors
///
/// [`BuildError::EmptyTask`] when `task` is empty.
/// [`BuildError::TaskTooLong`] when `task` exceeds [`MAX_TASK_BYTES`].
/// [`BuildError::Render`] when the DOM cannot be projected.
/// [`BuildError::Conversion`] when a rendered item is not plain text.
pub fn build_delegated_prompt(
	dom: &Dom,
	task: Str,
	limits: TurnLimits,
) -> Result<(DelegatedPrompt, TurnLimits), BuildError> {
	if task.is_empty() {
		return Err(BuildError::EmptyTask);
	}
	if task.len() > MAX_TASK_BYTES {
		return Err(BuildError::TaskTooLong { actual: task.len() });
	}

	let src = CanonicalPromptSource;
	let (system, band_hashes) = src.banded_render(dom).map_err(BuildError::Render)?;
	let volatile = src.volatile_items(dom).map_err(BuildError::Render)?;
	let bands = band_hashes.map(|b: BandHash| Hash32::new(*b.as_bytes()));

	let prompt = DelegatedPrompt::from_items(&system, &volatile, task, bands)
		.map_err(BuildError::Conversion)?;

	Ok((prompt, limits))
}
