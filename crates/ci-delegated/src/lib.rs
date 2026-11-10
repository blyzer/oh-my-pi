//! In-core CI host router (ADR 0049 D2.2, D4).
//!
//! [`Delegated`] maps a provider host to one [`omp_ci::CiHost`] and
//! forwards every method to that adapter. A write consults an
//! [`ApprovalGate`] only when the capabilities registered for that host
//! [`omp_ci::Capabilities::allows`] the operation. The operation is the
//! one the method names, and [`omp_ci::CiOp::mutates`] comes from the
//! contract.
//!
//! The crate performs no I/O and names no vendor. It is not a stable
//! contract until a second real extension host exists (ADR 0049 D2.8).
//! In-memory hosts prove routing only.
//!
//! Host lookup is ASCII case-insensitive. `A`–`Z` and `a`–`z` fold; no
//! other character does. The registry key is the lowercased provider id.
//! A details URL is classified with [`omp_ci::route`] through
//! [`Delegated::resolve`]. `route` keeps a named port beside the host, so
//! `https://ci.example:8443/...` and `https://ci.example/...` select the
//! same adapter. A provider id that itself contains `:8443` is a different
//! key, and `route` does not produce it.
//!
//! Capabilities are the value passed to [`DelegatedBuilder::register`].
//! The router does not call [`omp_ci::CiHost::capabilities`] to decide a
//! later method. That call is forwarded when the caller asks for it. The
//! registered flags are the ceiling: an adapter cannot widen them by
//! reporting a broader set.
//!
//! ```
//! use omp_ci::{CiOp, CiProvider};
//! use omp_ci_delegated::{ApprovalGate, DenyAll, GateFn, PermitAll};
//!
//! let provider = CiProvider::try_new("ci.example")?;
//! assert!(PermitAll.permit(CiOp::Cancel, &provider));
//! assert!(!DenyAll.permit(CiOp::Cancel, &provider));
//! let gate = GateFn::new(|op, _provider| op == CiOp::Cancel);
//! assert!(gate.permit(CiOp::Cancel, &provider));
//! assert!(!gate.permit(CiOp::Trigger, &provider));
//! assert!(CiOp::Cancel.mutates());
//! assert!(!CiOp::Status.mutates());
//! # Ok::<(), omp_ci::IdError>(())
//! ```

mod error;
mod gate;
mod router;

pub use error::RegisterError;
pub use gate::{ApprovalGate, DenyAll, GateFn, PermitAll};
pub use router::{Delegated, DelegatedBuilder};
