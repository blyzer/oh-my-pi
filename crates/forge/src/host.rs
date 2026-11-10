//! [`ForgeHost`] contract.
//!
//! The trait is the read and write surface for repositories, change
//! requests, and issues. Futures are unboxed. The trait is not object-safe;
//! a composing crate holds a concrete host, the same shape as `ExecHost`.
//!
//! Each method names the [`crate::ForgeOp`] it performs.
//! [`crate::ForgeOp::mutates`] is the approval flag. Checkout and push mutate
//! the repository. The types name the request and the receipt; this crate does
//! not touch a worktree. Creating a change request or an issue mutates the
//! forge record.

use std::{future::Future, sync::Arc};

use omp_core::Str;

use crate::{
	ChangeRequest, ChangeRequestDraft, CheckTarget, CheckoutReceipt, CheckoutRequest, Comment,
	FileBody, FileQuery, ForgeChecks, ForgeError, Issue, IssueDraft, PushReceipt, PushRequest,
	RepoRef, Repository, ResourceKind, SearchPage, SearchQuery,
};

/// A forge a caller can read and write through the contract.
///
/// Implementors perform I/O. The trait definition does not.
pub trait ForgeHost: Clone + Send + Sync + 'static {
	/// Reads repository metadata.
	///
	/// [`crate::ForgeOp::RepoView`] does not mutate.
	fn view_repo(
		&self,
		repo: &RepoRef,
	) -> impl Future<Output = Result<Repository, ForgeError>> + Send;

	/// Reads one file at a revision.
	///
	/// [`crate::ForgeOp::FileRead`] does not mutate.
	fn read_file(
		&self,
		query: &FileQuery,
	) -> impl Future<Output = Result<FileBody, ForgeError>> + Send;

	/// Reads one change request.
	///
	/// [`crate::ForgeOp::ChangeRequestView`] does not mutate.
	fn view_change_request(
		&self,
		repo: &RepoRef,
		number: u64,
	) -> impl Future<Output = Result<ChangeRequest, ForgeError>> + Send;

	/// Reads one issue.
	///
	/// [`crate::ForgeOp::IssueView`] does not mutate.
	fn view_issue(
		&self,
		repo: &RepoRef,
		number: u64,
	) -> impl Future<Output = Result<Issue, ForgeError>> + Send;

	/// Reads the unified diff text of one change request.
	///
	/// [`crate::ForgeOp::ChangeRequestView`] does not mutate. The caller keeps
	/// selectors such as `/diff` and `/diff/<index>` and slices this text.
	fn change_request_diff(
		&self,
		repo: &RepoRef,
		number: u64,
	) -> impl Future<Output = Result<Str, ForgeError>> + Send;

	/// Reads conversation comments on a change request or an issue.
	///
	/// The router classifies the read as [`crate::ForgeOp::ChangeRequestView`]
	/// or [`crate::ForgeOp::IssueView`] from `kind`. Neither mutates.
	fn list_comments(
		&self,
		repo: &RepoRef,
		number: u64,
		kind: ResourceKind,
	) -> impl Future<Output = Result<Arc<[Comment]>, ForgeError>> + Send;

	/// Reads checks on a change request or a commit.
	///
	/// [`crate::ForgeOp::ListChecks`] does not mutate.
	fn list_checks(
		&self,
		repo: &RepoRef,
		target: &CheckTarget,
	) -> impl Future<Output = Result<ForgeChecks, ForgeError>> + Send;

	/// Searches one family.
	///
	/// Every [`crate::SearchKind`] maps to a read [`crate::ForgeOp`].
	fn search(
		&self,
		query: SearchQuery,
	) -> impl Future<Output = Result<SearchPage, ForgeError>> + Send;

	/// Creates a change request.
	///
	/// [`crate::ForgeOp::ChangeRequestCreate`] mutates.
	fn create_change_request(
		&self,
		draft: ChangeRequestDraft,
	) -> impl Future<Output = Result<ChangeRequest, ForgeError>> + Send;

	/// Checks a change request out into an isolated worktree.
	///
	/// [`crate::ForgeOp::ChangeRequestCheckout`] mutates.
	fn checkout_change_request(
		&self,
		request: &CheckoutRequest,
	) -> impl Future<Output = Result<CheckoutReceipt, ForgeError>> + Send;

	/// Pushes a checked-out change-request branch.
	///
	/// [`crate::ForgeOp::ChangeRequestPush`] mutates.
	fn push_change_request(
		&self,
		request: &PushRequest,
	) -> impl Future<Output = Result<PushReceipt, ForgeError>> + Send;

	/// Creates an issue.
	///
	/// [`crate::ForgeOp::IssueCreate`] mutates.
	fn create_issue(
		&self,
		draft: IssueDraft,
	) -> impl Future<Output = Result<Issue, ForgeError>> + Send;
}

#[cfg(test)]
mod tests {
	use std::{future::Future, sync::Arc};

	use omp_core::Str;

	use crate::{
		ChangeRequest, ChangeRequestDraft, CheckTarget, CheckoutReceipt, CheckoutRequest, Comment,
		FileBody, FileQuery, ForgeChecks, ForgeError, ForgeHost, ForgeOp, Issue, IssueDraft,
		PushReceipt, PushRequest, RepoRef, Repository, ResourceKind, SearchPage, SearchQuery,
	};

	fn unsupported<T: Send>(op: ForgeOp) -> std::future::Ready<Result<T, ForgeError>> {
		std::future::ready(Err(ForgeError::Unsupported { op }))
	}

	#[derive(Clone)]
	struct Nop;

	impl ForgeHost for Nop {
		fn view_repo(
			&self,
			_repo: &RepoRef,
		) -> impl Future<Output = Result<Repository, ForgeError>> + Send {
			unsupported(ForgeOp::RepoView)
		}

		fn read_file(
			&self,
			_query: &FileQuery,
		) -> impl Future<Output = Result<FileBody, ForgeError>> + Send {
			unsupported(ForgeOp::FileRead)
		}

		fn view_change_request(
			&self,
			_repo: &RepoRef,
			_number: u64,
		) -> impl Future<Output = Result<ChangeRequest, ForgeError>> + Send {
			unsupported(ForgeOp::ChangeRequestView)
		}

		fn view_issue(
			&self,
			_repo: &RepoRef,
			_number: u64,
		) -> impl Future<Output = Result<Issue, ForgeError>> + Send {
			unsupported(ForgeOp::IssueView)
		}

		fn change_request_diff(
			&self,
			_repo: &RepoRef,
			_number: u64,
		) -> impl Future<Output = Result<Str, ForgeError>> + Send {
			unsupported(ForgeOp::ChangeRequestView)
		}

		fn list_comments(
			&self,
			_repo: &RepoRef,
			_number: u64,
			kind: ResourceKind,
		) -> impl Future<Output = Result<Arc<[Comment]>, ForgeError>> + Send {
			let op = match kind {
				ResourceKind::ChangeRequest => ForgeOp::ChangeRequestView,
				ResourceKind::Issue => ForgeOp::IssueView,
			};
			unsupported(op)
		}

		fn list_checks(
			&self,
			_repo: &RepoRef,
			_target: &CheckTarget,
		) -> impl Future<Output = Result<ForgeChecks, ForgeError>> + Send {
			unsupported(ForgeOp::ListChecks)
		}

		fn search(
			&self,
			query: SearchQuery,
		) -> impl Future<Output = Result<SearchPage, ForgeError>> + Send {
			unsupported(query.kind.op())
		}

		fn create_change_request(
			&self,
			_draft: ChangeRequestDraft,
		) -> impl Future<Output = Result<ChangeRequest, ForgeError>> + Send {
			unsupported(ForgeOp::ChangeRequestCreate)
		}

		fn checkout_change_request(
			&self,
			_request: &CheckoutRequest,
		) -> impl Future<Output = Result<CheckoutReceipt, ForgeError>> + Send {
			unsupported(ForgeOp::ChangeRequestCheckout)
		}

		fn push_change_request(
			&self,
			_request: &PushRequest,
		) -> impl Future<Output = Result<PushReceipt, ForgeError>> + Send {
			unsupported(ForgeOp::ChangeRequestPush)
		}

		fn create_issue(
			&self,
			_draft: IssueDraft,
		) -> impl Future<Output = Result<Issue, ForgeError>> + Send {
			unsupported(ForgeOp::IssueCreate)
		}
	}

	#[test]
	fn a_host_can_implement_the_trait_without_boxing() {
		fn assert_host<H: ForgeHost>(host: &H) {
			let _ = host;
		}
		assert_host(&Nop);
	}
}
