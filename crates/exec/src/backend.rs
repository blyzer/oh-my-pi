//! The backend contract: what a backend offers and how a run is driven.
//!
//! The traits are never used as `dyn`: the composing crate holds one enum over
//! its concrete backends and matches on it.

use std::{sync::Arc, time::Duration};

use omp_adw::Posture;
use omp_core::Str;
use strum::{Display, EnumString, IntoStaticStr};

use crate::{
	error::ExecutionError,
	ids::{BackendId, ExecutionId},
	platform::{NeedSet, Platform},
	request::ExecutionRequest,
	result::ExecutionEvent,
};

/// Whether a backend can answer from an action cache.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Display, EnumString, IntoStaticStr)]
#[strum(serialize_all = "kebab-case")]
pub enum CacheSupport {
	/// No action cache.
	None,
	/// Cached results can be read.
	Read,
	/// Cached results can be read and written.
	ReadWrite,
}

/// How a backend can stop accepted work.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Display, EnumString, IntoStaticStr)]
#[strum(serialize_all = "kebab-case")]
pub enum CancelSupport {
	/// No cancel; the action timeout bounds the work.
	None,
	/// A cancel request whose effect is not confirmed.
	BestEffort,
	/// A cancel the backend confirms.
	Confirmed,
}

/// What a backend offers. `enforced` is what it enforces, never what it
/// intends (ADR 0039 B5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendCapabilities {
	/// Platforms the backend runs.
	pub platforms:      Arc<[Platform]>,
	/// Host capabilities the backend provides.
	pub needs:          NeedSet,
	/// The longest timeout the backend accepts, if bounded.
	pub max_timeout:    Option<Duration>,
	/// Action-cache support.
	pub action_cache:   CacheSupport,
	/// Cancel support.
	pub cancel:         CancelSupport,
	/// The largest blob the backend accepts, if bounded.
	pub max_blob_bytes: Option<u64>,
	/// Whether named caches survive between runs.
	pub warm_caches:    bool,
	/// The posture the backend enforces on a run.
	pub enforced:       Posture,
}

/// Where placement may send requests: the operator's `sv_exec_policy`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Display, EnumString, IntoStaticStr)]
#[strum(serialize_all = "kebab-case", ascii_case_insensitive)]
pub enum FallbackPolicy {
	/// Everything local; `RemoteRequired` is unplaceable.
	#[default]
	LocalOnly,
	/// Local first; `RemoteRequired` goes remote.
	LocalPreferred,
	/// `RemotePreferred` and `RemoteRequired` go remote; the rest stays local.
	RemotePreferred,
	/// Remote only; `LocalRequired` is unplaceable.
	RemoteOnly,
}

/// Action-cache use: the operator's `sv_exec_remote_cache`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Display, EnumString, IntoStaticStr)]
#[strum(serialize_all = "kebab-case", ascii_case_insensitive)]
pub enum CachePolicy {
	/// Never read or write the action cache.
	#[default]
	Off,
	/// Read cached results; never write.
	Read,
	/// Read and write cached results.
	ReadWrite,
}

/// The operator's execution policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct ExecutionPolicy {
	/// Placement and fallback.
	pub fallback: FallbackPolicy,
	/// Action-cache use.
	pub cache:    CachePolicy,
}

/// Enough to resume watching a detached run after a disconnect or restart.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reattach {
	/// The backend that holds the work.
	pub backend:   BackendId,
	/// The backend's operation name.
	pub operation: Str,
	/// Execution identity.
	pub id:        ExecutionId,
}

/// What a cancel achieved.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Display, EnumString, IntoStaticStr)]
#[strum(serialize_all = "kebab-case")]
pub enum CancelOutcome {
	/// The work stopped.
	Cancelled,
	/// The work had already settled.
	AlreadySettled,
	/// A cancel was sent; the backend did not confirm it.
	Requested,
	/// The backend cannot cancel; the action timeout bounds the work.
	Unsupported,
	/// The cancel request failed; the action timeout bounds the work.
	Failed,
}

/// One execution backend: local, remote, or over SSH.
///
/// Cheap to clone (a handle); every async method returns an unboxed future.
pub trait ExecutionBackend: Clone + Send + Sync + 'static {
	/// A run this backend started or reattached to.
	type Run: ExecutionRun;

	/// The operator-given name of this backend.
	fn id(&self) -> &BackendId<str>;

	/// What this backend offers.
	fn capabilities(&self) -> &BackendCapabilities;

	/// Starts `request`.
	fn start(
		&self,
		request: ExecutionRequest,
	) -> impl Future<Output = Result<Self::Run, ExecutionError>> + Send + '_;

	/// Resumes watching a run detached with [`ExecutionRun::detach`].
	fn reattach(
		&self,
		handle: Reattach,
	) -> impl Future<Output = Result<Self::Run, ExecutionError>> + Send + '_;
}

/// One run in progress.
///
/// Dropping a run that was not detached cancels it. [`ExecutionRun::cancel`]
/// is the one explicit cancel path; a caller bridges its cancellation token to
/// it.
pub trait ExecutionRun: Send + 'static {
	/// The next progress event; `None` after [`ExecutionEvent::Settled`].
	fn next_event(&mut self) -> impl Future<Output = Option<ExecutionEvent>> + Send + '_;

	/// Asks the backend to stop the work.
	fn cancel(&self) -> impl Future<Output = CancelOutcome> + Send + '_;

	/// Stops watching without cancelling and returns what
	/// [`ExecutionBackend::reattach`] needs to resume.
	fn detach(self) -> Reattach;
}
