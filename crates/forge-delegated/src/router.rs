//! Host registry and [`omp_forge::ForgeHost`] forwarding.
//!
//! [`ForgeHost`](omp_forge::ForgeHost) returns a distinct unboxed future
//! from each method, so it is not object-safe. [`Delegated`] stays generic
//! over one adapter type and keeps those futures unboxed. Heterogeneous
//! adapters are erased by the composer into that one type before
//! [`DelegatedBuilder::register`]. Registration stores an [`Arc`]; a call
//! clones the [`Arc`] and does not box.

use std::{mem::size_of, sync::Arc};

use omp_core::{FastHashMap, Str, StrExt};
use omp_forge::{
	ChangeRequest, ChangeRequestDraft, CheckTarget, CheckoutReceipt, CheckoutRequest,
	ComponentError, FileBody, FileQuery, ForgeChecks, ForgeError, ForgeHost, ForgeOp, Issue,
	IssueDraft, PushReceipt, PushRequest, RepoRef, Repository, SearchPage, SearchQuery, check_host,
};

use crate::{ApprovalGate, RegisterError};

/// Routes forge calls by host and approval-gates writes.
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
	hosts: FastHashMap<Str, Arc<H>>,
	gate:  G,
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
}

impl<H, G> Delegated<H, G>
where
	G: ApprovalGate,
{
	/// Resolves `repo`'s host and, for a write, asks the gate.
	///
	/// The operation is the one the caller method selected. This function
	/// does not read a classification from the adapter.
	fn adapter_for(&self, op: ForgeOp, repo: &RepoRef) -> Result<Arc<H>, ForgeError> {
		let key = repo.host.as_str().to_ascii_lowercase_str();
		let Some(adapter) = self.inner.hosts.get(key.as_str()).cloned() else {
			return Err(ForgeError::Unregistered { host: key, op });
		};
		if op.mutates() && !self.inner.gate.permit(op, repo) {
			return Err(ForgeError::NotApproved { host: key, op });
		}
		Ok(adapter)
	}
}

impl<H, G> ForgeHost for Delegated<H, G>
where
	H: ForgeHost,
	G: ApprovalGate,
{
	async fn view_repo(&self, repo: &RepoRef) -> Result<Repository, ForgeError> {
		let host = self.adapter_for(ForgeOp::RepoView, repo)?;
		host.view_repo(repo).await
	}

	async fn read_file(&self, query: &FileQuery) -> Result<FileBody, ForgeError> {
		let host = self.adapter_for(ForgeOp::FileRead, &query.repo)?;
		host.read_file(query).await
	}

	async fn view_change_request(
		&self,
		repo: &RepoRef,
		number: u64,
	) -> Result<ChangeRequest, ForgeError> {
		let host = self.adapter_for(ForgeOp::ChangeRequestView, repo)?;
		host.view_change_request(repo, number).await
	}

	async fn view_issue(&self, repo: &RepoRef, number: u64) -> Result<Issue, ForgeError> {
		let host = self.adapter_for(ForgeOp::IssueView, repo)?;
		host.view_issue(repo, number).await
	}

	async fn list_checks(
		&self,
		repo: &RepoRef,
		target: &CheckTarget,
	) -> Result<ForgeChecks, ForgeError> {
		let host = self.adapter_for(ForgeOp::ListChecks, repo)?;
		host.list_checks(repo, target).await
	}

	async fn search(&self, query: SearchQuery) -> Result<SearchPage, ForgeError> {
		let op = query.kind.op();
		let Some(repo) = query.repo.clone() else {
			return Err(ForgeError::Unscoped { op });
		};
		let host = self.adapter_for(op, &repo)?;
		host.search(query).await
	}

	async fn create_change_request(
		&self,
		draft: ChangeRequestDraft,
	) -> Result<ChangeRequest, ForgeError> {
		let repo = draft.repo.clone();
		let host = self.adapter_for(ForgeOp::ChangeRequestCreate, &repo)?;
		host.create_change_request(draft).await
	}

	async fn checkout_change_request(
		&self,
		request: &CheckoutRequest,
	) -> Result<CheckoutReceipt, ForgeError> {
		let host = self.adapter_for(ForgeOp::ChangeRequestCheckout, &request.repo)?;
		host.checkout_change_request(request).await
	}

	async fn push_change_request(&self, request: &PushRequest) -> Result<PushReceipt, ForgeError> {
		let host = self.adapter_for(ForgeOp::ChangeRequestPush, &request.repo)?;
		host.push_change_request(request).await
	}

	async fn create_issue(&self, draft: IssueDraft) -> Result<Issue, ForgeError> {
		let repo = draft.repo.clone();
		let host = self.adapter_for(ForgeOp::IssueCreate, &repo)?;
		host.create_issue(draft).await
	}
}

/// Builds a [`Delegated`] router.
///
/// Host identity is the ASCII-lowercased form of the string passed to
/// [`Self::register`]. The string must pass [`check_host`]. `Forge-A.Example`
/// and `forge-a.example` are one host. `code.example` and `code.example:443`
/// are two hosts.
#[must_use = "call build to finish the router"]
pub struct DelegatedBuilder<H, G> {
	hosts: FastHashMap<Str, Arc<H>>,
	gate:  G,
}

impl<H, G> DelegatedBuilder<H, G> {
	/// Registers `adapter` for `host`.
	///
	/// # Errors
	///
	/// Returns [`RegisterError::Duplicate`] when the ASCII-lowercased host
	/// is already registered, and [`RegisterError::Component`] when `host`
	/// fails [`check_host`].
	pub fn register(&mut self, host: &str, adapter: H) -> Result<(), RegisterError> {
		let key = canonical_host(host)?;
		if self.hosts.contains_key(key.as_str()) {
			return Err(RegisterError::Duplicate { host: key });
		}
		self.hosts.insert(key, Arc::new(adapter));
		Ok(())
	}

	/// Freezes the registry.
	#[must_use]
	pub fn build(self) -> Delegated<H, G> {
		Delegated { inner: Arc::new(Inner { hosts: self.hosts, gate: self.gate }) }
	}
}

fn canonical_host(host: &str) -> Result<Str, ComponentError> {
	check_host(host)?;
	Ok(host.to_ascii_lowercase_str())
}
