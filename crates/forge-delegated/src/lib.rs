//! In-core forge host router (ADR 0049 D2.2, D4).
//!
//! [`Delegated`] maps a forge host to one [`omp_forge::ForgeHost`] and
//! forwards every method to that adapter. Writes consult an
//! [`ApprovalGate`] first. The operation is the one the method names, and
//! [`omp_forge::ForgeOp::mutates`] comes from the contract.
//!
//! The crate performs no I/O and names no vendor. It is not a stable
//! contract until a second real extension host exists (ADR 0049 D2.8).
//! In-memory hosts prove routing only.
//!
//! Host lookup is ASCII case-insensitive. `A`–`Z` and `a`–`z` fold; no
//! other character does. The registry key is the lowercased host. A port
//! or bracket is part of the key, so `code.example` and `code.example:443`
//! are different hosts. The match does not strip a trailing dot and does
//! not fill in a default port.
//!
//! ```
//! use omp_forge::{ForgeOp, RepoRef};
//! use omp_forge_delegated::{ApprovalGate, DenyAll, GateFn, PermitAll};
//!
//! let repo = RepoRef::try_new("code.example", "acme", "widget")?;
//! assert!(PermitAll.permit(ForgeOp::IssueCreate, &repo));
//! assert!(!DenyAll.permit(ForgeOp::IssueCreate, &repo));
//! let gate = GateFn::new(|op, _repo| op == ForgeOp::IssueCreate);
//! assert!(gate.permit(ForgeOp::IssueCreate, &repo));
//! assert!(!gate.permit(ForgeOp::ChangeRequestPush, &repo));
//! assert!(ForgeOp::ChangeRequestCreate.mutates());
//! assert!(!ForgeOp::RepoView.mutates());
//! # Ok::<(), omp_forge::ComponentError>(())
//! ```

mod error;
mod gate;
mod router;

pub use error::RegisterError;
pub use gate::{ApprovalGate, DenyAll, GateFn, PermitAll};
pub use router::{Delegated, DelegatedBuilder};
