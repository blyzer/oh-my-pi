//! Named prompt-slot catalog shared by the projector and the extension host.

use strum::{Display, EnumString};

/// Semantic stability of a prompt contribution.
#[derive(Clone, Copy, Debug, Display, EnumString, Eq, Ord, PartialEq, PartialOrd)]
#[repr(u8)]
#[strum(serialize_all = "lowercase")]
pub enum SlotClass {
	/// Immutable for the process lifetime.
	Frozen   = 0,
	/// Changes only after an explicit, journaled configuration event.
	Stable   = 1,
	/// Changes at a compaction or reset boundary.
	Dynamic  = 2,
	/// May change on every turn.
	Volatile = 3,
}

/// Fixed prompt-slot catalog.
#[derive(Clone, Copy, Debug, Display, EnumString, Eq, Ord, PartialEq, PartialOrd)]
#[repr(u8)]
#[strum(serialize_all = "lowercase")]
pub enum SlotId {
	/// RFC and harness conventions.
	Conventions = 0,
	/// Agent identity.
	Role        = 1,
	/// Runtime capability announcements.
	Runtime     = 2,
	/// Tool and device inventory.
	Tools       = 3,
	/// Tool-use policy.
	Policy      = 4,
	/// Engineering workflow.
	Workflow    = 5,
	/// Installed skills.
	Skills      = 6,
	/// Standing rules.
	Rules       = 7,
	/// General guidance.
	Guidance    = 8,
	/// Workspace identity and files.
	Workspace   = 9,
	/// Compaction-epoch memory.
	Memory      = 10,
	/// Compaction-epoch standing instructions.
	Standing    = 11,
	/// Per-turn recall.
	Recall      = 12,
	/// Per-turn runtime status.
	Status      = 13,
	/// Delivery contract.
	Delivery    = 14,
}
