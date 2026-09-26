//! Journal-first subagent composition and session-owned tools.

pub mod autoreply;
pub mod hub;
pub mod revive;
pub mod settings;
pub mod spawn;
pub mod workpool;
mod workpool_runtime;
pub mod workpool_scheduler;
mod yield_assembly;

omp_core::string_id!(
	/// Agent class a composed kernel runs as: [`MAIN_AGENT`] for a top-level
	/// session, the spawned class (`task`, `scout`, ...) for a child. It names
	/// the class cfg (`<agent>.cfg`, ADR 0013) and selects the rules whose
	/// `agents:` frontmatter admits it.
	AgentName
);

/// Agent class of the top-level session.
pub const MAIN_AGENT: &AgentName<str> = AgentName::from_ref("main");
