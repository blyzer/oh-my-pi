//! ACP (Agent Client Protocol) wire transport for `omp-envd`.
//!
//! This module implements the newline-delimited JSON-RPC transport layer for
//! the Agent Client Protocol v1 (<https://agentclientprotocol.com>), spec
//! version 2026-10-19.  In ACP, OMP acts as the **client** (editor side) and
//! the agent is the server.  The data flow is the mirror of the Codex adapter
//! in `codex_app_server`, where OMP drives a CLI agent over its own JSON-RPC
//! dialect.
//!
//! ## Structure
//!
//! - [`wire`]: typed serde structs for every ACP v1 message variant this
//!   transport needs.  No `json!`, no untyped `Value` traversal.
//! - [`session`]: the correlation table, framing (via [`crate::line_reader`]),
//!   and cancel-safe `next_message`.
//!
//! ## Shared framing
//!
//! [`crate::line_reader::LineReader`] is the same size-capped, cancel-safe
//! newline reader used by `codex_app_server`.  It is not copied: both modules
//! import it from `crate::line_reader`.
//!
//! ## Non-goals
//!
//! This slice is transport and wire only.  A `DelegatedExecutor` implementation
//! (spawning the agent, process-group teardown via `child_tree`, approval
//! routing) is the next task.

pub mod session;
pub mod wire;

pub use session::{AcpSession, InboundMessage, MAX_FRAME_BYTES, SessionError};

#[cfg(test)]
mod tests;
