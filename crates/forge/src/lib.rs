//! Forge-neutral change-request and issue contract (ADR 0049 D2).
//!
//! [`RepoRef`], [`CommitRef`], [`ChangeRequest`], [`Issue`], [`Check`],
//! [`CheckState`], and [`ForgeChecks`] are the data. [`parse_resource_url`]
//! reads `pr://` and `issue://` into a host, an owner, a repository, and a
//! number. [`ForgeOp::mutates`] says which operations write. [`ForgeHost`]
//! is the read and write trait an adapter implements.
//!
//! The crate performs no I/O and names no vendor. Host strings are opaque.
//! Mutation flags live here, so an adapter cannot mark a write as a read.
//!
//! ```
//! use omp_core::Str;
//! use omp_forge::{
//! 	Check, CheckState, CheckTarget, ForgeChecks, ForgeOp, ResourceKind, parse_resource_url,
//! 	required_checks, required_satisfied,
//! };
//!
//! let resource = parse_resource_url("pr://code.example/acme/widget/42/diff")?;
//! assert_eq!(resource.kind, ResourceKind::ChangeRequest);
//! assert_eq!(resource.host.as_deref(), Some("code.example"));
//! assert_eq!(resource.owner.as_str(), "acme");
//! assert_eq!(resource.name.as_str(), "widget");
//! assert_eq!(resource.number, 42);
//! assert!(ForgeOp::ChangeRequestCreate.mutates());
//! assert!(!ForgeOp::ChangeRequestView.mutates());
//!
//! let checks = ForgeChecks::new(CheckTarget::ChangeRequest { number: 42 }, [Check::new(
//! 	Str::new_static("lint"),
//! 	CheckState::Success,
//! 	true,
//! )]);
//! assert!(required_satisfied(&checks));
//! assert_eq!(required_checks(&checks).count(), 1);
//! # Ok::<(), omp_forge::SchemeError>(())
//! ```

mod change;
mod check;
mod component;
mod error;
mod host;
mod op;
mod repo;
mod scheme;

pub use change::{
	ChangeRequest, ChangeRequestDraft, CheckoutReceipt, CheckoutRequest, Issue, IssueDraft,
	IssueState, PushReceipt, PushRequest, RequestState,
};
pub use check::{
	Check, CheckState, CheckTarget, ForgeChecks, required_checks, required_satisfied,
	unsatisfied_required,
};
pub use component::{
	ComponentError, MAX_COMMIT_LEN, MAX_COMPONENT_LEN, MAX_OWNER_LEN, MAX_SEGMENTS, PathComponent,
};
pub use error::ForgeError;
pub use host::ForgeHost;
pub use op::{ForgeOp, MAX_SEARCH_LIMIT, Mutation, SearchHit, SearchKind, SearchPage, SearchQuery};
pub use repo::{CommitRef, FileBody, FileQuery, PathError, RepoRef, Repository};
pub use scheme::{
	DiffSelector, ResourceKind, ResourceSelector, SchemeError, SchemeRef, parse_resource_url,
};
