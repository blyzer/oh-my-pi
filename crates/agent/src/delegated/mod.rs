//! Backend-neutral contract for delegated agent executors (ADR 0043).
//!
//! A *delegated executor* is an official third-party agent CLI that owns its
//! own agent loop, tools, sessions and provider authentication, while OMP owns
//! the process lifecycle, cancellation, timeouts, working-directory policy and
//! the normalized event stream. It is **not** a model transport: it never
//! implements [`crate::Inference`], is never registered in the provider
//! catalog, and its output is never flattened into a raw token stream.
//!
//! The contract has three layers:
//!
//! - **Vocabulary** — [`DelegatedEvent`], [`Capabilities`], [`DelegatedPolicy`],
//!   [`PermissionRequest`], [`ExecutorError`]: plain data, no I/O.
//! - **Backend traits** — [`DelegatedExecutor`] and [`DelegatedSession`], the
//!   only things an adapter implements.
//! - **Host driver** — [`start_session`] and [`ActiveSession::run_turn`],
//!   written once over the traits so every backend gets the same fail-closed
//!   negotiation, timeout, hang, crash and cancellation behavior.
//!
//! Hard rules baked into the types:
//!
//! - Execution mode is explicit ([`ExecutionMode`]); a request for
//!   [`ExecutionMode::DirectApi`] is rejected, never rerouted.
//! - No type here carries a credential, an auth path or an environment map, so
//!   an adapter cannot be handed one through the contract. The CLI owns its
//!   login; OMP only observes [`AuthStatus`].
//! - The executor receives the exact prompt OMP would have used for its own
//!   run ([`DelegatedPrompt`]: rendered system items, volatile band and user
//!   turn, with band digests). It forwards those bytes verbatim and echoes a
//!   digest of what it forwarded; any difference ends the session
//!   ([`ExecutorError::PromptRewritten`]).
//! - A policy that cannot be mapped exactly is refused up front
//!   ([`ExecutorError::PolicyUnmappable`]); a backend that reports a different
//!   effective policy than the one requested is torn down
//!   ([`ExecutorError::PolicyDrift`]); a permission answer can never widen the
//!   negotiated policy ([`admit_request`], [`map_decision`]).
//! - An event the backend never advertised support for is a contract
//!   violation ([`ExecutorError::UndeclaredEvent`]), so a final-text-only CLI
//!   can not be dressed up as a streaming agent.

mod capability;
mod drive;
mod error;
mod event;
mod executor;
mod id;
mod permission;
mod policy;
mod prompt;

pub use capability::*;
pub use drive::*;
pub use error::*;
pub use event::*;
pub use executor::*;
pub use id::*;
pub use permission::*;
pub use policy::*;
pub use prompt::*;
