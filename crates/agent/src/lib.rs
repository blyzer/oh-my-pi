//! Journal-first agent kernel over `omp-session`.

#![feature(type_alias_impl_trait)]

pub mod cancel;
pub mod context;
pub mod directors;
pub mod dispatch;
pub mod env;
pub mod events;
pub mod file_mentions;
pub mod jobs;
pub mod local;
#[path = "loop.rs"]
pub mod loop_;
pub mod model_watch;
pub mod pause;
pub mod prompt;
pub mod registry;
mod roster;
pub mod steering;
mod stream_coalesce;
pub mod submit;
pub mod vars;
/// Image-input policy (`ai_vision`).
pub mod vision;

mod contract_impls;

pub use approvals::*;
pub use cancel::*;
pub use contract_impls::{InferenceErase, UpMailbox, kernel_approval_route, up_same_channel};
pub use director::*;
pub use dispatch::*;
pub use env::*;
pub use events::*;
pub use extensions::*;
pub use file_mentions::*;
pub use hooks::*;
pub use jobs::*;
pub use local::*;
pub use loop_::*;
pub use model_watch::*;
pub use omp_contract::{approvals, delegated, director, extensions, hooks, mailbox::KernelSender};
pub use pause::*;
pub use prompt::*;
pub use registry::*;
pub use roster::roster_fingerprint;
pub use steering::*;
pub use submit::*;
pub use vars::*;
