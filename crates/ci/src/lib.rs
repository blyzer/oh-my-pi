//! CI-neutral pipeline and job contract (ADR 0049 D2).
//!
//! [`Pipeline`], [`Job`], [`CiStatus`], [`CiProvider`], and [`Capabilities`]
//! are the data. [`route`] reads an absolute `http` or `https` details URL
//! into a provider host and a path. [`CiOp::mutates`] says which operations
//! write. [`CiHost`] is the read and write trait an adapter implements.
//!
//! The crate performs no I/O and names no product. Host strings are opaque.
//! Mutation flags live here, so an adapter cannot mark a write as a read.
//!
//! ```
//! use omp_ci::{Capabilities, CiOp, CiStatus, route};
//!
//! let routed = route("https://ci.example/pipelines/9/jobs/3")?;
//! assert_eq!(routed.host(), "ci.example");
//! assert_eq!(routed.path.as_str(), "pipelines/9/jobs/3");
//! assert!(CiOp::Cancel.mutates());
//! assert!(CiOp::RerunFailed.mutates());
//! assert!(CiOp::RerunJob.mutates());
//! assert!(CiOp::Trigger.mutates());
//! assert!(CiOp::PlayManual.mutates());
//! assert!(!CiOp::Status.mutates());
//! assert!(!CiOp::Artifacts.mutates());
//! assert!(!CiOp::LogStream.mutates());
//! let caps = Capabilities { cancel: true, ..Capabilities::none() };
//! assert!(caps.allows(CiOp::Cancel));
//! assert!(caps.allows(CiOp::Status));
//! assert!(!caps.allows(CiOp::Trigger));
//! assert!(CiStatus::Failure.is_terminal());
//! assert!(!CiStatus::Manual.is_terminal());
//! # Ok::<(), omp_ci::RouteError>(())
//! ```

mod error;
mod host;
mod op;
mod provider;
mod route;
mod run;
mod status;

pub use error::{CiError, IdError, MAX_ID_LEN};
pub use host::CiHost;
pub use op::{CiOp, Mutation};
pub use provider::{Capabilities, CiProvider};
pub use route::{
	CiRoute, MAX_HOST_LEN, MAX_SEGMENT_LEN, MAX_SEGMENTS, MAX_URL_LEN, RouteError, RouteScheme,
	route,
};
pub use run::{
	Artifact, ArtifactPage, Job, LogPage, MAX_INPUT_VALUE, MAX_INPUTS, Pipeline, RunRef, Trigger,
	jobs_with_status,
};
pub use status::CiStatus;
