//! Codex `app-server` adapter for the delegated-executor contract (ADR 0043).
//!
//! The adapter lives in `omp-envd`, not `omp-agent`, because it owns a
//! resident child process and reuses [`crate::child_tree`] for isolation and
//! teardown (`omp-envd` depends on `omp-agent`, never the other way round).
//! The backend-neutral contract it implements is `omp_agent::delegated`.
//!
//! - `wire`: typed serde structs for exactly the messages the first slice
//!   needs. Nothing is built with `json!` or walked as an untyped value.
//! - `frame`: the newline-delimited, size-capped line reader.
//! - `policy`: the exact, fail-closed policy mapping.
//! - `session`: JSON-RPC correlation, event normalization, approvals.
//! - `executor`: probe and spawn.

mod executor;
mod frame;
mod policy;
mod session;
mod wire;

#[cfg(test)]
mod tests;

pub use executor::{CODEX_VERSIONS, CodexExecutor, MAX_FRAME_BYTES};
pub use session::CodexSession;
