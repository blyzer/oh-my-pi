//! Immutable memory-slot snapshot the environment hands the prompt projector.

/// One immutable runtime-owned memory slot.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PromptMemorySlotInput {
	/// Slot-local revision.
	pub generation: u64,
	/// Fully framed, bounded contribution.
	pub content:    Option<omp_core::Str>,
}

/// Immutable Memory, Standing, and Recall slot snapshot.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PromptMemoryInput {
	/// Compaction-epoch memory background.
	pub memory:   PromptMemorySlotInput,
	/// Compaction-epoch non-directive guidance.
	pub standing: PromptMemorySlotInput,
	/// Per-turn volatile recall.
	pub recall:   PromptMemorySlotInput,
}
