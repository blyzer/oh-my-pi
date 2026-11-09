//! Shared control vocabulary for the agent kernel and the project environment.
//!
//! `omp-agent`, `omp-envd`, and `omp-driver` name these types. The crate sits
//! below them so the environment host does not depend on the kernel, and the
//! driver does not import environment-internal modules for posture or the
//! eval parent bridge.

#![allow(missing_docs, reason = "strum IntoStaticStr emits undocumented inherent methods")]

pub mod approvals;
pub mod delegated;
pub mod director;
pub mod env_event;
pub mod eval_bridge;
pub mod extensions;
pub mod hooks;
pub mod mailbox;
pub mod posture;
pub mod prompt_memory;
pub mod session;
pub mod slots;
pub mod vars;

pub use approvals::*;
pub use director::*;
pub use env_event::*;
pub use extensions::*;
pub use hooks::*;
pub use mailbox::*;
pub use prompt_memory::*;
pub use session::*;
pub use slots::{SlotClass, SlotId};
pub use vars::{AI_MODEL, AI_MODEL_INHERIT};
