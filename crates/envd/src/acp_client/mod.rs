//! ACP (Agent Client Protocol) wire transport and delegated executor for
//! `omp-envd`.
//!
//! This module implements the ACP v1 client:
//!
//! - [`wire`]: typed serde structs for every ACP v1 message this transport
//!   needs.  No `json!`, no untyped `Value` traversal.
//! - [`session`]: the correlation table, framing (via [`crate::line_reader`]),
//!   and cancel-safe `next_message`.
//! - [`delegated_session`]: [`AcpDelegatedSession`] implementing
//!   [`omp_agent::delegated::DelegatedSession`] over ACP.
//! - [`executor`]: [`AcpExecutor`] implementing
//!   [`omp_agent::delegated::DelegatedExecutor`], spawning the agent over stdio
//!   with [`crate::child_tree`] process-group teardown.
//! - [`policy`]: the single ACP-mappable policy
//!   ([`DelegatedPolicy::STRICTEST`]).
//!
//! ## Shared framing
//!
//! [`crate::line_reader::LineReader`] is the same size-capped, cancel-safe
//! newline reader used by `codex_app_server`.  It is not copied: both modules
//! import it from `crate::line_reader`.

pub mod delegated_session;
pub mod executor;
pub mod policy;
pub mod session;
pub mod wire;

pub use delegated_session::AcpDelegatedSession;
pub use executor::AcpExecutor;
pub use session::{AcpSession, InboundMessage, MAX_FRAME_BYTES, MalformedLine, SessionError};

#[cfg(test)]
mod tests;
