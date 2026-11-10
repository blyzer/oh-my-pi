//! Host registry and [`omp_ci::CiHost`] forwarding.
//!
//! [`CiHost`](omp_ci::CiHost) returns a distinct unboxed future from each
//! method, so it is not object-safe. [`Delegated`] stays generic over one
//! adapter type and keeps those futures unboxed. Heterogeneous adapters
//! are erased by the composer into that one type before
//! [`DelegatedBuilder::register`]. Registration stores an [`Arc`]; a call
//! clones the [`Arc`] and does not box.
//!
//! Capability flags are the [`omp_ci::Capabilities`] value stored at
//! registration. A method checks [`omp_ci::Capabilities::allows`] against
//! that value and does not call
//! [`CiHost::capabilities`](omp_ci::CiHost::capabilities) to decide. The
//! ceiling cannot move after `register`. A write the flags refuse is not
//! submitted to the [`crate::ApprovalGate`].

use std::{mem::size_of, sync::Arc};

use omp_ci::{
	ArtifactPage, Capabilities, CiError, CiHost, CiOp, CiProvider, IdError, Job, LogPage, Pipeline,
	RunRef, Trigger, route,
};
use omp_core::{FastHashMap, Str, StrExt};

use crate::{ApprovalGate, RegisterError};

/// Routes CI calls by provider host, bounds them by registered capabilities,
/// and approval-gates writes.
///
/// `H` is the adapter. Every host in one router uses that same type. `G`
/// is the write gate. Reads do not call it.
///
/// The value is an [`Arc`]. Cloning it shares the registry and the gate.
///
/// [`Clone`] is manual. A derived impl would demand `H: Clone` and
/// `G: Clone` even though only the [`Arc`] is copied.
pub struct Delegated<H, G> {
	inner: Arc<Inner<H, G>>,
}

impl<H, G> Clone for Delegated<H, G> {
	fn clone(&self) -> Self {
		Self { inner: Arc::clone(&self.inner) }
	}
}

struct Inner<H, G> {
	hosts: FastHashMap<Str, HostEntry<H>>,
	gate:  G,
}

struct HostEntry<H> {
	adapter:      Arc<H>,
	capabilities: Capabilities,
}

const _: () = assert!(
	size_of::<Delegated<(), crate::PermitAll>>() == size_of::<usize>(),
	"Delegated must stay one pointer"
);

impl<H, G> Delegated<H, G> {
	/// Starts an empty registry that uses `gate` for writes.
	pub fn builder(gate: G) -> DelegatedBuilder<H, G> {
		DelegatedBuilder { hosts: FastHashMap::default(), gate }
	}

	/// Resolves `details_url` to a registered provider.
	///
	/// The host comes from [`route`]. Lookup uses the same
	/// ASCII-case-insensitive registry as [`CiHost`] methods. [`route`]
	/// keeps a named port beside the host, so the key is the host alone.
	/// A missing host is [`CiError::ProviderNotFound`]. This lookup names
	/// no operation, so the gate is not asked.
	///
	/// # Errors
	///
	/// Returns [`CiError::Route`] when `details_url` cannot be classified,
	/// and [`CiError::ProviderNotFound`] when the host is not registered.
	pub fn resolve(&self, details_url: &str) -> Result<CiProvider, CiError> {
		let routed = route(details_url)?;
		let key = canonical_provider(&routed.provider);
		if !self.inner.hosts.contains_key(key.as_str()) {
			return Err(CiError::ProviderNotFound { provider: key });
		}
		Ok(routed.provider)
	}
}

impl<H, G> Delegated<H, G>
where
	G: ApprovalGate,
{
	/// Resolves `provider`'s host and, when `op` is offered and writes,
	/// asks the gate.
	///
	/// The operation is the one the caller method selected. This function
	/// does not read a classification from the adapter. Capabilities come
	/// from registration, not from [`CiHost::capabilities`].
	fn adapter_for(&self, op: CiOp, provider: &CiProvider) -> Result<Arc<H>, CiError> {
		let key = canonical_provider(provider);
		let Some(entry) = self.inner.hosts.get(key.as_str()) else {
			return Err(CiError::Unregistered { provider: key, op });
		};
		if !entry.capabilities.allows(op) {
			return Err(CiError::NotCapable { op, provider: key });
		}
		if op.mutates() && !self.inner.gate.permit(op, provider) {
			return Err(CiError::NotApproved { op, provider: key });
		}
		Ok(Arc::clone(&entry.adapter))
	}

	/// Resolves `provider` for capability discovery, which is not a [`CiOp`].
	fn adapter_for_capabilities(&self, provider: &CiProvider) -> Result<Arc<H>, CiError> {
		let key = canonical_provider(provider);
		self
			.inner
			.hosts
			.get(key.as_str())
			.map(|entry| Arc::clone(&entry.adapter))
			.ok_or(CiError::ProviderNotFound { provider: key })
	}
}

impl<H, G> CiHost for Delegated<H, G>
where
	H: CiHost,
	G: ApprovalGate,
{
	async fn capabilities(&self, provider: &CiProvider) -> Result<Capabilities, CiError> {
		let host = self.adapter_for_capabilities(provider)?;
		host.capabilities(provider).await
	}

	async fn status(&self, run: &RunRef) -> Result<Pipeline, CiError> {
		let host = self.adapter_for(CiOp::Status, &run.provider)?;
		host.status(run).await
	}

	async fn artifacts(&self, run: &RunRef) -> Result<ArtifactPage, CiError> {
		let host = self.adapter_for(CiOp::Artifacts, &run.provider)?;
		host.artifacts(run).await
	}

	async fn log_stream(&self, run: &RunRef) -> Result<LogPage, CiError> {
		let host = self.adapter_for(CiOp::LogStream, &run.provider)?;
		host.log_stream(run).await
	}

	async fn cancel(&self, run: &RunRef) -> Result<Pipeline, CiError> {
		let host = self.adapter_for(CiOp::Cancel, &run.provider)?;
		host.cancel(run).await
	}

	async fn rerun_failed(&self, run: &RunRef) -> Result<Pipeline, CiError> {
		let host = self.adapter_for(CiOp::RerunFailed, &run.provider)?;
		host.rerun_failed(run).await
	}

	async fn rerun_job(&self, run: &RunRef) -> Result<Job, CiError> {
		let host = self.adapter_for(CiOp::RerunJob, &run.provider)?;
		host.rerun_job(run).await
	}

	async fn trigger(&self, request: Trigger) -> Result<Pipeline, CiError> {
		let host = self.adapter_for(CiOp::Trigger, &request.provider)?;
		host.trigger(request).await
	}

	async fn play_manual(&self, run: &RunRef) -> Result<Job, CiError> {
		let host = self.adapter_for(CiOp::PlayManual, &run.provider)?;
		host.play_manual(run).await
	}
}

/// Builds a [`Delegated`] router.
///
/// Host identity is the ASCII-lowercased form of the string passed to
/// [`Self::register`]. The string must pass [`CiProvider::try_new`].
/// `Ci-A.Example` and `ci-a.example` are one host. `ci.example` and
/// `ci.example:8443` are two hosts. [`route`] selects the first of those
/// two for both `https://ci.example/...` and `https://ci.example:8443/...`.
#[must_use = "call build to finish the router"]
pub struct DelegatedBuilder<H, G> {
	hosts: FastHashMap<Str, HostEntry<H>>,
	gate:  G,
}

impl<H, G> DelegatedBuilder<H, G> {
	/// Registers `adapter` for `host` with the capabilities it advertises.
	///
	/// `capabilities` is the ceiling later methods enforce. The router does
	/// not replace it with a later [`CiHost::capabilities`] answer.
	///
	/// # Errors
	///
	/// Returns [`RegisterError::Duplicate`] when the ASCII-lowercased host
	/// is already registered, and [`RegisterError::Id`] when `host` fails
	/// [`CiProvider::try_new`].
	pub fn register(
		&mut self,
		host: &str,
		adapter: H,
		capabilities: Capabilities,
	) -> Result<(), RegisterError> {
		let key = canonical_host(host)?;
		if self.hosts.contains_key(key.as_str()) {
			return Err(RegisterError::Duplicate { host: key });
		}
		self
			.hosts
			.insert(key, HostEntry { adapter: Arc::new(adapter), capabilities });
		Ok(())
	}

	/// Freezes the registry.
	#[must_use]
	pub fn build(self) -> Delegated<H, G> {
		Delegated { inner: Arc::new(Inner { hosts: self.hosts, gate: self.gate }) }
	}
}

fn canonical_provider(provider: &CiProvider) -> Str {
	provider.id.as_str().to_ascii_lowercase_str()
}

fn canonical_host(host: &str) -> Result<Str, IdError> {
	let provider = CiProvider::try_new(host)?;
	Ok(provider.id.as_str().to_ascii_lowercase_str())
}
