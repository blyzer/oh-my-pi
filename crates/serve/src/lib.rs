//! gRPC transport projections serving OMP inference, auth, blob, and session
//! control services.

pub mod auth;
pub mod blob;
pub mod inference;
pub mod session;

pub use auth::{AuthRpc, AuthenticatedRevealContext};
pub use blob::BlobRpc;
pub use inference::InferenceRpc;
pub use session::{
	ControlContext, ControlScope, ScopeSet, SessionControlAuthority, SessionControlError,
	SessionControlRpc,
};
