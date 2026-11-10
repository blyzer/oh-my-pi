//! Dyn `forge/*` device bridge (ADR 0049 D2.3).
//!
//! T2 extensions implement [`omp_forge::ForgeHost`] by mounting dyn devices at
//! `forge/<op>` (snake_case [`omp_forge::ForgeOp`]) and answering
//! `omp.devices.call`. This module is the in-core half: a [`DynForgeAdapter`]
//! that dispatches each [`ForgeHost`] method to those paths, plus a
//! [`ForgeBackend`] enum so the production [`super::forge_router`] can hold
//! GitHub and dyn hosts without boxing [`ForgeHost`] futures.
//!
//! Schema source of truth: `docs/adr/0049-forge-dyn-device-schema.md`.

use std::{future::Future, path::PathBuf, pin::Pin, sync::Arc};

use omp_core::Str;
use omp_forge::{
	ChangeRequest, ChangeRequestDraft, CheckTarget, CheckoutPlan, CheckoutReceipt, CheckoutRequest,
	Comment, CommitRef, FileBody, FileQuery, ForgeChecks, ForgeError, ForgeHost, ForgeOp, Issue,
	IssueDraft, PushPrep, PushReceipt, PushRequest, RepoRef, Repository, ResourceKind, SearchPage,
	SearchQuery,
};
use omp_forge_delegated::{DelegatedBuilder, PermitAll, RegisterError};
use parking_lot::RwLock;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use strum::IntoStaticStr as _;
use tokio_util::sync::CancellationToken;

use super::GithubForgeAdapter;
use crate::forge_worktree;

/// Canonical GitLab SaaS host always registered on the dyn forge arm (D2.8).
///
/// Additional self-hosted hosts come from `sv_forge_dyn_hosts` and adapter env
/// `GITLAB_HOST` via [`super::ForgeDynHosts`]. Automatic device-catalog
/// host-claim discovery remains a smaller TODO.
pub const GITLAB_HOST: &str = "gitlab.com";

/// Canonical GitHub SaaS host on the dyn forge arm once the T2 extension can
/// answer (ADR 0049 GitHub T2). Same bound [`LiveForgeDeviceClient`] as
/// [`GITLAB_HOST`]. In-core [`GithubForgeAdapter`] remains for CI, list-URL
/// helpers, and GHE checkout hosts that are not in the Dyn set.
pub const GITHUB_DYN_HOST: &str = "github.com";

/// Device paths under `forge/*`.
///
/// Primary paths match [`ForgeOp`] snake_case. Two [`ForgeHost`] methods share
/// an op for approval but have distinct device paths so an extension can
/// implement them separately.
pub mod paths {
	/// [`ForgeOp::RepoView`] → [`ForgeHost::view_repo`].
	pub const REPO_VIEW: &str = "forge/repo_view";
	/// [`ForgeOp::FileRead`] → [`ForgeHost::read_file`].
	pub const FILE_READ: &str = "forge/file_read";
	/// [`ForgeOp::ChangeRequestView`] → [`ForgeHost::view_change_request`].
	pub const CHANGE_REQUEST_VIEW: &str = "forge/change_request_view";
	/// Same mutation class as [`ForgeOp::ChangeRequestView`] →
	/// [`ForgeHost::change_request_diff`].
	pub const CHANGE_REQUEST_DIFF: &str = "forge/change_request_diff";
	/// [`ForgeOp::IssueView`] → [`ForgeHost::view_issue`].
	pub const ISSUE_VIEW: &str = "forge/issue_view";
	/// Shares [`ForgeOp::ChangeRequestView`] or [`ForgeOp::IssueView`] with
	/// `kind` in the args → [`ForgeHost::list_comments`].
	pub const LIST_COMMENTS: &str = "forge/list_comments";
	/// [`ForgeOp::ListChecks`] → [`ForgeHost::list_checks`].
	pub const LIST_CHECKS: &str = "forge/list_checks";
	/// [`ForgeOp::SearchChangeRequests`] (sibling search ops use
	/// [`super::forge_device_path`]).
	pub const SEARCH_CHANGE_REQUESTS: &str = "forge/search_change_requests";
	/// [`ForgeOp::SearchIssues`].
	pub const SEARCH_ISSUES: &str = "forge/search_issues";
	/// [`ForgeOp::SearchCode`].
	pub const SEARCH_CODE: &str = "forge/search_code";
	/// [`ForgeOp::SearchCommits`].
	pub const SEARCH_COMMITS: &str = "forge/search_commits";
	/// [`ForgeOp::SearchRepos`].
	pub const SEARCH_REPOS: &str = "forge/search_repos";
	/// [`ForgeOp::ChangeRequestCreate`].
	pub const CHANGE_REQUEST_CREATE: &str = "forge/change_request_create";
	/// [`ForgeOp::ChangeRequestCheckout`].
	pub const CHANGE_REQUEST_CHECKOUT: &str = "forge/change_request_checkout";
	/// [`ForgeOp::ChangeRequestPush`].
	pub const CHANGE_REQUEST_PUSH: &str = "forge/change_request_push";
	/// [`ForgeOp::IssueCreate`].
	pub const ISSUE_CREATE: &str = "forge/issue_create";
}

/// `forge/<ForgeOp snake_case>` for one operation.
#[inline]
#[must_use]
pub fn forge_device_path(op: ForgeOp) -> Str {
	omp_core::sf!("forge/{}", op.into_str())
}

/// Invokes one mounted `forge/<op>` dyn device.
///
/// Production uses [`LiveForgeDeviceClient`] (catalog / `omp.devices.call`).
/// Tests may inject a fake. The trait returns an unboxed future;
/// [`DynForgeAdapter`] stays generic over the client so [`ForgeHost`] futures
/// stay unboxed.
pub trait ForgeDeviceClient: Clone + Send + Sync + 'static {
	/// Calls the device at `path` with JSON `args`.
	///
	/// Success payloads are the serde form of the matching `omp_forge` result
	/// type. Failures are typed [`ForgeError`] values from the client.
	fn call(
		&self,
		path: &str,
		args: Value,
	) -> impl Future<Output = Result<Value, ForgeError>> + Send;
}

/// Object-safe invoker behind [`LiveForgeDeviceClient`].
///
/// Cold I/O boundary: one allocation per forge device round-trip (extension
/// callback / network). Production installs
/// [`super::super::tools::DeviceControlFactory`]'s catalog path; tests install
/// a recording fake.
pub trait ForgeDeviceInvoker: Send + Sync + 'static {
	/// Invokes the mounted device at `path`.
	fn invoke(
		&self,
		path: &str,
		args: Value,
	) -> Pin<Box<dyn Future<Output = Result<Value, ForgeError>> + Send + '_>>;
}

/// Maps a `forge/<op>` path to a [`ForgeOp`] for error reporting.
#[must_use]
pub fn forge_op_for_path(path: &str) -> ForgeOp {
	path
		.strip_prefix("forge/")
		.and_then(|name| name.parse().ok())
		.unwrap_or(ForgeOp::RepoView)
}

/// Live production [`ForgeDeviceClient`]: dispatches mounted `forge/*` devices.
///
/// Holds a late-bound [`ForgeDeviceInvoker`]. Until [`Self::bind`] installs one
/// (or after [`Self::unbind`]), every call returns
/// [`ForgeError::Unavailable`]. The production router still registers
/// [`GITLAB_HOST`] so Delegated can reach the dyn arm; Unavailable until the
/// GitLab extension mounts devices (or the invoker is bound) is intentional.
///
/// Bind from composition once [`crate::tools::DeviceControlFactory`] exists:
/// `client.bind(devices.forge_invoker())`. That invoker looks up mounted leaves
/// and dispatches `omp.devices.call` — the same destination as
/// `DeviceControlFactory::invoke`.
#[derive(Clone, Default)]
pub struct LiveForgeDeviceClient {
	invoker: Arc<RwLock<Option<Arc<dyn ForgeDeviceInvoker>>>>,
}

impl LiveForgeDeviceClient {
	/// Installs the catalog-backed (or test) invoker.
	pub fn bind(&self, invoker: Arc<dyn ForgeDeviceInvoker>) {
		*self.invoker.write() = Some(invoker);
	}

	/// Clears the invoker (devices unmounted / session teardown).
	pub fn unbind(&self) {
		*self.invoker.write() = None;
	}

	/// Whether an invoker is currently bound.
	#[must_use]
	pub fn is_bound(&self) -> bool {
		self.invoker.read().is_some()
	}
}

impl ForgeDeviceClient for LiveForgeDeviceClient {
	fn call(
		&self,
		path: &str,
		args: Value,
	) -> impl Future<Output = Result<Value, ForgeError>> + Send {
		let invoker = self.invoker.read().clone();
		let path = path.to_owned();
		async move {
			let Some(invoker) = invoker else {
				return Err(ForgeError::Unavailable { op: forge_op_for_path(&path) });
			};
			invoker.invoke(&path, args).await
		}
	}
}

/// Placeholder client that always returns [`ForgeError::Unavailable`].
///
/// Prefer [`LiveForgeDeviceClient`] for production [`ForgeBackend`]. Kept for
/// tests that want a permanently-dead dyn arm without a bind slot.
#[derive(Debug, Clone, Copy, Default)]
pub struct UnavailableForgeClient;

impl ForgeDeviceClient for UnavailableForgeClient {
	fn call(
		&self,
		path: &str,
		_args: Value,
	) -> impl Future<Output = Result<Value, ForgeError>> + Send {
		std::future::ready(Err(ForgeError::Unavailable { op: forge_op_for_path(path) }))
	}
}

/// [`ForgeHost`] that dispatches each method to a `forge/<op>` dyn device.
///
/// Checkout and push obtain a vendor [`CheckoutPlan`] / [`PushPrep`] from the
/// device, then run forge-neutral git in core ([`forge_worktree`]).
#[derive(Clone)]
pub struct DynForgeAdapter<C> {
	client:        C,
	project_root:  PathBuf,
	worktree_root: PathBuf,
}

impl<C> DynForgeAdapter<C> {
	/// Builds an adapter over `client` with the project and worktree roots
	/// core needs for checkout/push.
	#[must_use]
	pub fn new(client: C, project_root: PathBuf, worktree_root: PathBuf) -> Self {
		Self { client, project_root, worktree_root }
	}
}

impl<C: ForgeDeviceClient> DynForgeAdapter<C> {
	async fn invoke_decode<T: DeserializeOwned>(
		&self,
		op: ForgeOp,
		path: &str,
		args: Value,
	) -> Result<T, ForgeError> {
		let value = self.client.call(path, args).await?;
		serde_json::from_value(value).map_err(|_| ForgeError::Unavailable { op })
	}
}

impl<C: ForgeDeviceClient> ForgeHost for DynForgeAdapter<C> {
	fn view_repo(
		&self,
		repo: &RepoRef,
	) -> impl Future<Output = Result<Repository, ForgeError>> + Send {
		let this = self.clone();
		let repo = repo.clone();
		async move {
			this
				.invoke_decode(ForgeOp::RepoView, paths::REPO_VIEW, json!({ "repo": repo }))
				.await
		}
	}

	fn read_file(
		&self,
		query: &FileQuery,
	) -> impl Future<Output = Result<FileBody, ForgeError>> + Send {
		let this = self.clone();
		let query = query.clone();
		async move {
			this
				.invoke_decode(ForgeOp::FileRead, paths::FILE_READ, json!({ "query": query }))
				.await
		}
	}

	fn view_change_request(
		&self,
		repo: &RepoRef,
		number: u64,
	) -> impl Future<Output = Result<ChangeRequest, ForgeError>> + Send {
		let this = self.clone();
		let repo = repo.clone();
		async move {
			this
				.invoke_decode(
					ForgeOp::ChangeRequestView,
					paths::CHANGE_REQUEST_VIEW,
					json!({ "repo": repo, "number": number }),
				)
				.await
		}
	}

	fn view_issue(
		&self,
		repo: &RepoRef,
		number: u64,
	) -> impl Future<Output = Result<Issue, ForgeError>> + Send {
		let this = self.clone();
		let repo = repo.clone();
		async move {
			this
				.invoke_decode(
					ForgeOp::IssueView,
					paths::ISSUE_VIEW,
					json!({ "repo": repo, "number": number }),
				)
				.await
		}
	}

	fn change_request_diff(
		&self,
		repo: &RepoRef,
		number: u64,
	) -> impl Future<Output = Result<Str, ForgeError>> + Send {
		let this = self.clone();
		let repo = repo.clone();
		async move {
			this
				.invoke_decode(
					ForgeOp::ChangeRequestView,
					paths::CHANGE_REQUEST_DIFF,
					json!({ "repo": repo, "number": number }),
				)
				.await
		}
	}

	fn list_comments(
		&self,
		repo: &RepoRef,
		number: u64,
		kind: ResourceKind,
	) -> impl Future<Output = Result<Arc<[Comment]>, ForgeError>> + Send {
		let this = self.clone();
		let repo = repo.clone();
		async move {
			let op = match kind {
				ResourceKind::ChangeRequest => ForgeOp::ChangeRequestView,
				ResourceKind::Issue => ForgeOp::IssueView,
			};
			let comments: Vec<Comment> = this
				.invoke_decode(
					op,
					paths::LIST_COMMENTS,
					json!({ "repo": repo, "number": number, "kind": kind }),
				)
				.await?;
			Ok(comments.into())
		}
	}

	fn list_checks(
		&self,
		repo: &RepoRef,
		target: &CheckTarget,
	) -> impl Future<Output = Result<ForgeChecks, ForgeError>> + Send {
		let this = self.clone();
		let repo = repo.clone();
		let target = target.clone();
		async move {
			this
				.invoke_decode(
					ForgeOp::ListChecks,
					paths::LIST_CHECKS,
					json!({ "repo": repo, "target": target }),
				)
				.await
		}
	}

	fn search(
		&self,
		query: SearchQuery,
	) -> impl Future<Output = Result<SearchPage, ForgeError>> + Send {
		let this = self.clone();
		async move {
			let op = query.kind.op();
			let path = forge_device_path(op);
			this
				.invoke_decode(op, path.as_str(), json!({ "query": query }))
				.await
		}
	}

	fn create_change_request(
		&self,
		draft: ChangeRequestDraft,
	) -> impl Future<Output = Result<ChangeRequest, ForgeError>> + Send {
		let this = self.clone();
		async move {
			this
				.invoke_decode(
					ForgeOp::ChangeRequestCreate,
					paths::CHANGE_REQUEST_CREATE,
					json!({ "draft": draft }),
				)
				.await
		}
	}

	fn checkout_change_request(
		&self,
		request: &CheckoutRequest,
	) -> impl Future<Output = Result<CheckoutReceipt, ForgeError>> + Send {
		let this = self.clone();
		let request = request.clone();
		async move {
			let op = ForgeOp::ChangeRequestCheckout;
			let plan: CheckoutPlan = this
				.invoke_decode(op, paths::CHANGE_REQUEST_CHECKOUT, json!({ "request": request }))
				.await?;
			let cancel = CancellationToken::new();
			let input = forge_worktree::CheckoutInput {
				repo_identity: format!("{}/{}/{}", plan.repo.host, plan.repo.owner, plan.repo.name),
				number:        plan.number,
				clone_url:     plan.clone_url.to_string(),
				base_ref:      plan.base_ref.to_string(),
				head_ref:      plan.head_ref.to_string(),
				head_sha:      plan.head_sha.to_string(),
				force:         request.force,
			};
			let out =
				forge_worktree::checkout(&this.project_root, &this.worktree_root, &input, &cancel)
					.await
					.map_err(|error| map_worktree(op, error))?;
			let head = CommitRef::try_new(plan.repo.clone(), out.head_sha)?;
			Ok(CheckoutReceipt {
				repo: plan.repo,
				number: out.number,
				head,
				base_ref: Str::new(out.base_ref),
				head_ref: Str::new(out.head_ref),
				worktree: Str::new(out.worktree.to_string_lossy().as_ref()),
			})
		}
	}

	fn push_change_request(
		&self,
		request: &PushRequest,
	) -> impl Future<Output = Result<PushReceipt, ForgeError>> + Send {
		let this = self.clone();
		let request = request.clone();
		async move {
			let op = ForgeOp::ChangeRequestPush;
			let prep: PushPrep = this
				.invoke_decode(op, paths::CHANGE_REQUEST_PUSH, json!({ "request": request }))
				.await?;
			let cancel = CancellationToken::new();
			let head_ref = if !request.head_ref.is_empty() {
				request.head_ref.to_string()
			} else {
				prep.head_ref.to_string()
			};
			let input = forge_worktree::PushInput {
				repo_identity: format!("{}/{}/{}", prep.repo.host, prep.repo.owner, prep.repo.name),
				number: prep.number,
				clone_url: prep.clone_url.to_string(),
				head_ref,
				force_with_lease: request.force_with_lease,
			};
			let out = forge_worktree::push(&this.worktree_root, &input, &cancel)
				.await
				.map_err(|error| map_worktree(op, error))?;
			Ok(PushReceipt {
				repo:        prep.repo,
				number:      out.number,
				head_commit: out.head_commit,
			})
		}
	}

	fn create_issue(
		&self,
		draft: IssueDraft,
	) -> impl Future<Output = Result<Issue, ForgeError>> + Send {
		let this = self.clone();
		async move {
			this
				.invoke_decode(ForgeOp::IssueCreate, paths::ISSUE_CREATE, json!({ "draft": draft }))
				.await
		}
	}
}

/// Concrete [`ForgeHost`] held by the production forge router.
///
/// Enums before `dyn`: GitHub stays a native arm; T2 extensions ride
/// [`DynForgeAdapter`]. Both arms forward with `match`, so [`ForgeHost`]
/// futures stay unboxed. The default client is [`LiveForgeDeviceClient`].
#[derive(Clone)]
pub enum ForgeBackend<C = LiveForgeDeviceClient> {
	/// In-tree GitHub HTTP/git adapter.
	Github(GithubForgeAdapter),
	/// Dyn-device-backed host (`forge/*` over toolhost).
	Dyn(DynForgeAdapter<C>),
}

impl<C: ForgeDeviceClient> ForgeHost for ForgeBackend<C> {
	fn view_repo(
		&self,
		repo: &RepoRef,
	) -> impl Future<Output = Result<Repository, ForgeError>> + Send {
		async move {
			match self {
				Self::Github(host) => host.view_repo(repo).await,
				Self::Dyn(host) => host.view_repo(repo).await,
			}
		}
	}

	fn read_file(
		&self,
		query: &FileQuery,
	) -> impl Future<Output = Result<FileBody, ForgeError>> + Send {
		async move {
			match self {
				Self::Github(host) => host.read_file(query).await,
				Self::Dyn(host) => host.read_file(query).await,
			}
		}
	}

	fn view_change_request(
		&self,
		repo: &RepoRef,
		number: u64,
	) -> impl Future<Output = Result<ChangeRequest, ForgeError>> + Send {
		async move {
			match self {
				Self::Github(host) => host.view_change_request(repo, number).await,
				Self::Dyn(host) => host.view_change_request(repo, number).await,
			}
		}
	}

	fn view_issue(
		&self,
		repo: &RepoRef,
		number: u64,
	) -> impl Future<Output = Result<Issue, ForgeError>> + Send {
		async move {
			match self {
				Self::Github(host) => host.view_issue(repo, number).await,
				Self::Dyn(host) => host.view_issue(repo, number).await,
			}
		}
	}

	fn change_request_diff(
		&self,
		repo: &RepoRef,
		number: u64,
	) -> impl Future<Output = Result<Str, ForgeError>> + Send {
		async move {
			match self {
				Self::Github(host) => host.change_request_diff(repo, number).await,
				Self::Dyn(host) => host.change_request_diff(repo, number).await,
			}
		}
	}

	fn list_comments(
		&self,
		repo: &RepoRef,
		number: u64,
		kind: ResourceKind,
	) -> impl Future<Output = Result<Arc<[Comment]>, ForgeError>> + Send {
		async move {
			match self {
				Self::Github(host) => host.list_comments(repo, number, kind).await,
				Self::Dyn(host) => host.list_comments(repo, number, kind).await,
			}
		}
	}

	fn list_checks(
		&self,
		repo: &RepoRef,
		target: &CheckTarget,
	) -> impl Future<Output = Result<ForgeChecks, ForgeError>> + Send {
		async move {
			match self {
				Self::Github(host) => host.list_checks(repo, target).await,
				Self::Dyn(host) => host.list_checks(repo, target).await,
			}
		}
	}

	fn search(
		&self,
		query: SearchQuery,
	) -> impl Future<Output = Result<SearchPage, ForgeError>> + Send {
		async move {
			match self {
				Self::Github(host) => host.search(query).await,
				Self::Dyn(host) => host.search(query).await,
			}
		}
	}

	fn create_change_request(
		&self,
		draft: ChangeRequestDraft,
	) -> impl Future<Output = Result<ChangeRequest, ForgeError>> + Send {
		async move {
			match self {
				Self::Github(host) => host.create_change_request(draft).await,
				Self::Dyn(host) => host.create_change_request(draft).await,
			}
		}
	}

	fn checkout_change_request(
		&self,
		request: &CheckoutRequest,
	) -> impl Future<Output = Result<CheckoutReceipt, ForgeError>> + Send {
		async move {
			match self {
				Self::Github(host) => host.checkout_change_request(request).await,
				Self::Dyn(host) => host.checkout_change_request(request).await,
			}
		}
	}

	fn push_change_request(
		&self,
		request: &PushRequest,
	) -> impl Future<Output = Result<PushReceipt, ForgeError>> + Send {
		async move {
			match self {
				Self::Github(host) => host.push_change_request(request).await,
				Self::Dyn(host) => host.push_change_request(request).await,
			}
		}
	}

	fn create_issue(
		&self,
		draft: IssueDraft,
	) -> impl Future<Output = Result<Issue, ForgeError>> + Send {
		async move {
			match self {
				Self::Github(host) => host.create_issue(draft).await,
				Self::Dyn(host) => host.create_issue(draft).await,
			}
		}
	}
}

/// Registers a dyn-backed forge host on a [`DelegatedBuilder`].
///
/// This is the seam T2 extensions (and tests) use to place a host such as
/// [`GITLAB_HOST`] or [`GITHUB_DYN_HOST`] on the Dyn arm. The production
/// [`super::forge_router`] registers every host in [`super::ForgeDynHosts`]
/// with a [`LiveForgeDeviceClient`]. Catalog mounts are owned by the
/// `omp.forge` extension (host-dispatch to `omp_github` / `omp_gitlab`).
/// Automatic discovery of additional hosts from the extension device catalog /
/// host claims is not wired yet (TODO, ADR 0049 D2.8).
///
/// # Errors
///
/// Returns [`RegisterError`] when `host` is invalid or already registered.
pub fn register_dyn_forge_host<C: ForgeDeviceClient>(
	builder: &mut DelegatedBuilder<ForgeBackend<C>, PermitAll>,
	host: &str,
	client: C,
	project_root: PathBuf,
	worktree_root: PathBuf,
) -> Result<(), RegisterError> {
	builder
		.register(host, ForgeBackend::Dyn(DynForgeAdapter::new(client, project_root, worktree_root)))
}

fn map_worktree(op: ForgeOp, error: forge_worktree::Error) -> ForgeError {
	match error {
		forge_worktree::Error::Cancelled
		| forge_worktree::Error::CheckoutMissing
		| forge_worktree::Error::IncompletePlan
		| forge_worktree::Error::GitFailed => ForgeError::Unavailable { op },
	}
}

#[cfg(test)]
mod tests {
	use std::{
		future::Future,
		sync::{
			Arc,
			atomic::{AtomicUsize, Ordering},
		},
	};

	use omp_cache::forge_cache::ForgeCache;
	use omp_con::Ctx;
	use omp_core::Str;
	use omp_forge::{
		ChangeRequest, CheckoutPlan, CheckoutRequest, ForgeError, ForgeHost as _, ForgeOp, Issue,
		IssueState, RepoRef, RequestState,
	};
	use omp_forge_delegated::{Delegated, PermitAll};
	use omp_vcs::testing::run as git;
	use parking_lot::Mutex;
	use serde_json::{Value, json};

	use super::{
		DynForgeAdapter, ForgeBackend, ForgeDeviceClient, ForgeDeviceInvoker, GITLAB_HOST,
		LiveForgeDeviceClient, forge_device_path, forge_op_for_path, paths, register_dyn_forge_host,
	};
	use crate::{
		blobs::BlobHost, forge_worktree, github::GithubService, github_adapter::GithubForgeAdapter,
		github_url::GithubCredentialBridge, tool_settings::ForgeCacheSettings,
	};

	#[derive(Clone)]
	struct FakeClient {
		calls: Arc<Mutex<Vec<(String, Value)>>>,
		cr:    ChangeRequest,
		issue: Issue,
		plan:  Option<CheckoutPlan>,
	}

	impl ForgeDeviceClient for FakeClient {
		fn call(
			&self,
			path: &str,
			args: Value,
		) -> impl Future<Output = Result<Value, ForgeError>> + Send {
			self.calls.lock().push((path.to_string(), args));
			let result = match path {
				paths::CHANGE_REQUEST_VIEW => Ok(serde_json::to_value(&self.cr).expect("cr")),
				paths::ISSUE_VIEW => Ok(serde_json::to_value(&self.issue).expect("issue")),
				paths::CHANGE_REQUEST_CHECKOUT => match &self.plan {
					Some(plan) => Ok(serde_json::to_value(plan).expect("plan")),
					None => Err(ForgeError::Unsupported { op: ForgeOp::ChangeRequestCheckout }),
				},
				other => {
					let op = other
						.strip_prefix("forge/")
						.and_then(|name| name.parse().ok())
						.unwrap_or(ForgeOp::RepoView);
					Err(ForgeError::Unsupported { op })
				},
			};
			std::future::ready(result)
		}
	}

	fn roots(dir: &std::path::Path) -> (std::path::PathBuf, std::path::PathBuf) {
		(dir.to_path_buf(), forge_worktree::worktree_root(dir))
	}

	fn sample_repo(host: &str) -> RepoRef {
		RepoRef::try_new(host, "acme", "widget").unwrap()
	}

	fn sample_cr(host: &str) -> ChangeRequest {
		ChangeRequest {
			repo:        sample_repo(host),
			number:      7,
			title:       Str::new_static("Fix the widget"),
			body:        Str::new_static("details"),
			state:       RequestState::Open,
			draft:       false,
			base_ref:    Str::new_static("main"),
			head_ref:    Str::new_static("topic"),
			head_commit: Str::new_static("bbb"),
			base_commit: Str::new_static("aaa"),
			web_url:     Str::new_static("https://gitlab.example/acme/widget/-/merge_requests/7"),
		}
	}

	fn sample_issue(host: &str) -> Issue {
		Issue {
			repo:    sample_repo(host),
			number:  3,
			title:   Str::new_static("Bug"),
			body:    Str::new_static("steps"),
			state:   IssueState::Open,
			web_url: Str::new_static("https://gitlab.example/acme/widget/-/issues/3"),
		}
	}

	fn github_service(root: &std::path::Path) -> Arc<GithubService> {
		let blobs = BlobHost::open(root.join("blobs")).expect("blobs");
		let cache = Arc::new(
			ForgeCache::open(
				root.join("forge-cache.sqlite3"),
				ForgeCacheSettings::from_con(&Ctx::new()).policy(),
			)
			.expect("cache"),
		);
		GithubService::new(
			root.to_path_buf(),
			root,
			Arc::new(GithubCredentialBridge::new()),
			cache,
			blobs,
		)
	}

	#[test]
	fn forge_device_paths_match_op_snake_case() {
		assert_eq!(forge_device_path(ForgeOp::RepoView).as_str(), paths::REPO_VIEW);
		assert_eq!(
			forge_device_path(ForgeOp::ChangeRequestView).as_str(),
			paths::CHANGE_REQUEST_VIEW
		);
		assert_eq!(forge_device_path(ForgeOp::IssueView).as_str(), paths::ISSUE_VIEW);
		assert_eq!(
			forge_device_path(ForgeOp::SearchChangeRequests).as_str(),
			paths::SEARCH_CHANGE_REQUESTS
		);
	}

	#[tokio::test]
	async fn delegated_routes_dyn_host_to_device_client() {
		let host = "gitlab.example";
		let calls = Arc::new(Mutex::new(Vec::new()));
		let fake = FakeClient {
			calls: Arc::clone(&calls),
			cr:    sample_cr(host),
			issue: sample_issue(host),
			plan:  None,
		};
		let dir = tempfile::tempdir().expect("temp");
		let github = GithubForgeAdapter { service: github_service(dir.path()) };
		let (project, trees) = roots(dir.path());
		let mut builder = Delegated::<ForgeBackend<FakeClient>, _>::builder(PermitAll);
		builder
			.register("github.com", ForgeBackend::Github(github))
			.expect("github");
		register_dyn_forge_host(&mut builder, host, fake, project, trees).expect("gitlab");
		let router = builder.build();

		let repo = sample_repo(host);
		let cr = router
			.view_change_request(&repo, 7)
			.await
			.expect("change request");
		assert_eq!(cr.number, 7);
		assert_eq!(cr.title.as_str(), "Fix the widget");
		assert_eq!(cr.state, RequestState::Open);

		let issue = router.view_issue(&repo, 3).await.expect("issue");
		assert_eq!(issue.number, 3);
		assert_eq!(issue.title.as_str(), "Bug");
		assert_eq!(issue.state, IssueState::Open);

		let recorded = calls.lock().clone();
		assert_eq!(recorded.len(), 2);
		assert_eq!(recorded[0].0, paths::CHANGE_REQUEST_VIEW);
		assert_eq!(recorded[0].1["number"], 7);
		assert_eq!(recorded[1].0, paths::ISSUE_VIEW);
		assert_eq!(recorded[1].1["number"], 3);
	}

	#[tokio::test]
	async fn unregistered_dyn_host_stays_unregistered() {
		let dir = tempfile::tempdir().expect("temp");
		let github = GithubForgeAdapter { service: github_service(dir.path()) };
		let mut builder = Delegated::<ForgeBackend<FakeClient>, _>::builder(PermitAll);
		builder
			.register("github.com", ForgeBackend::Github(github))
			.expect("github");
		let router = builder.build();
		let repo = sample_repo("gitlab.example");
		let error = router.view_issue(&repo, 1).await.expect_err("unregistered");
		assert!(
			matches!(
				error,
				ForgeError::Unregistered { ref host, op: ForgeOp::IssueView }
					if host == "gitlab.example"
			),
			"{error}"
		);
	}

	#[tokio::test]
	async fn dyn_adapter_alone_decodes_change_request() {
		let host = "gitlab.example";
		let fake = FakeClient {
			calls: Arc::new(Mutex::new(Vec::new())),
			cr:    sample_cr(host),
			issue: sample_issue(host),
			plan:  None,
		};
		let dir = tempfile::tempdir().expect("temp");
		let (project, trees) = roots(dir.path());
		let adapter = DynForgeAdapter::new(fake, project, trees);
		let cr = adapter
			.view_change_request(&sample_repo(host), 7)
			.await
			.expect("cr");
		assert_eq!(cr.web_url.as_str(), "https://gitlab.example/acme/widget/-/merge_requests/7");
	}

	#[tokio::test]
	async fn dyn_checkout_plan_yields_worktree_receipt() {
		let host = "gitlab.example";
		let dir = tempfile::tempdir().expect("temp");
		let project = dir.path().join("project");
		let remote = dir.path().join("remote.git");
		let state = dir.path().join("state");
		std::fs::create_dir_all(&project).expect("project");
		git(&project, &["init", "-b", "main"]);
		git(&project, &["config", "user.name", "omp"]);
		git(&project, &["config", "user.email", "omp@example.test"]);
		std::fs::write(project.join("README"), "hello\n").expect("write");
		git(&project, &["add", "README"]);
		git(&project, &["commit", "-m", "init"]);
		git(&project, &["checkout", "-b", "topic"]);
		std::fs::write(project.join("README"), "topic\n").expect("write");
		git(&project, &["add", "README"]);
		git(&project, &["commit", "-m", "topic"]);
		git(&project, &["checkout", "main"]);
		git(dir.path(), &["init", "--bare", remote.to_str().expect("utf8")]);
		git(&project, &["remote", "add", "origin", remote.to_str().expect("utf8")]);
		git(&project, &["push", "-u", "origin", "main"]);
		git(&project, &["push", "origin", "topic"]);
		let sha = git(&project, &["rev-parse", "topic"]).trim().to_owned();

		let plan = CheckoutPlan {
			repo:      sample_repo(host),
			number:    7,
			clone_url: Str::new(remote.to_string_lossy().as_ref()),
			base_ref:  Str::new_static("main"),
			head_ref:  Str::new_static("topic"),
			head_sha:  Str::new(sha.as_str()),
		};
		let fake = FakeClient {
			calls: Arc::new(Mutex::new(Vec::new())),
			cr:    sample_cr(host),
			issue: sample_issue(host),
			plan:  Some(plan),
		};
		let trees = forge_worktree::worktree_root(&state);
		let adapter = DynForgeAdapter::new(fake, project.clone(), trees.clone());
		let receipt = adapter
			.checkout_change_request(&CheckoutRequest::new(sample_repo(host), 7))
			.await
			.expect("checkout");
		assert_eq!(receipt.number, 7);
		assert_eq!(receipt.head_ref.as_str(), "topic");
		assert_eq!(receipt.head.id.as_str(), sha);
		let expected = forge_worktree::worktree_path(&trees, 7);
		assert_eq!(receipt.worktree.as_str(), expected.to_string_lossy().as_ref());
		assert!(expected.join("README").is_file());
	}

	#[tokio::test]
	async fn github_arm_still_selected_for_github_com() {
		// Enum routing proof: a host registered on ForgeBackend::Github is not
		// sent to the Dyn client. Production forge_router puts github.com on
		// Dyn; this keeps the Github arm selectable for unlisted GHE hosts.

		let hits = Arc::new(AtomicUsize::new(0));
		#[derive(Clone)]
		struct Counting(Arc<AtomicUsize>);
		impl ForgeDeviceClient for Counting {
			fn call(
				&self,
				_path: &str,
				_args: Value,
			) -> impl Future<Output = Result<Value, ForgeError>> + Send {
				self.0.fetch_add(1, Ordering::SeqCst);
				std::future::ready(Err(ForgeError::Unavailable { op: ForgeOp::IssueView }))
			}
		}
		let dir = tempfile::tempdir().expect("temp");
		let github = GithubForgeAdapter { service: github_service(dir.path()) };
		let mut builder = Delegated::<ForgeBackend<Counting>, _>::builder(PermitAll);
		builder
			.register("github.com", ForgeBackend::Github(github))
			.expect("github");
		let (project, trees) = roots(dir.path());
		register_dyn_forge_host(
			&mut builder,
			"gitlab.example",
			Counting(Arc::clone(&hits)),
			project,
			trees,
		)
		.expect("gitlab");
		let router = builder.build();
		let repo = RepoRef::try_new("github.com", "acme", "widget").unwrap();
		let error = router
			.view_issue(&repo, 1)
			.await
			.expect_err("no live github");
		assert_eq!(hits.load(Ordering::SeqCst), 0, "dyn client must not run for github.com");
		assert!(
			!matches!(error, ForgeError::Unregistered { .. }),
			"github.com must stay registered: {error}"
		);
	}

	struct RecordingInvoker {
		calls: Arc<Mutex<Vec<(String, Value)>>>,
		cr:    ChangeRequest,
		issue: Issue,
	}

	impl ForgeDeviceInvoker for RecordingInvoker {
		fn invoke(
			&self,
			path: &str,
			args: Value,
		) -> std::pin::Pin<Box<dyn Future<Output = Result<Value, ForgeError>> + Send + '_>> {
			self.calls.lock().push((path.to_string(), args));
			let result = match path {
				paths::CHANGE_REQUEST_VIEW => Ok(serde_json::to_value(&self.cr).expect("cr")),
				paths::ISSUE_VIEW => Ok(serde_json::to_value(&self.issue).expect("issue")),
				paths::REPO_VIEW | paths::CHANGE_REQUEST_DIFF | paths::LIST_COMMENTS => {
					Err(ForgeError::Unsupported { op: forge_op_for_path(path) })
				},
				other => Err(ForgeError::Unavailable { op: forge_op_for_path(other) }),
			};
			Box::pin(std::future::ready(result))
		}
	}

	#[tokio::test]
	async fn live_client_unbound_is_unavailable() {
		let client = LiveForgeDeviceClient::default();
		assert!(!client.is_bound());
		let error = client
			.call(paths::ISSUE_VIEW, json!({}))
			.await
			.expect_err("unbound");
		assert!(matches!(error, ForgeError::Unavailable { op: ForgeOp::IssueView }));
	}

	#[tokio::test]
	async fn live_client_bound_invoker_serves_gitlab_host() {
		let host = GITLAB_HOST;
		let calls = Arc::new(Mutex::new(Vec::new()));
		let client = LiveForgeDeviceClient::default();
		client.bind(Arc::new(RecordingInvoker {
			calls: Arc::clone(&calls),
			cr:    sample_cr(host),
			issue: sample_issue(host),
		}));
		assert!(client.is_bound());

		let dir = tempfile::tempdir().expect("temp");
		let github = GithubForgeAdapter { service: github_service(dir.path()) };
		let (project, trees) = roots(dir.path());
		let mut builder = Delegated::<ForgeBackend<LiveForgeDeviceClient>, _>::builder(PermitAll);
		builder
			.register("github.com", ForgeBackend::Github(github))
			.expect("github");
		register_dyn_forge_host(&mut builder, GITLAB_HOST, client, project, trees)
			.expect("gitlab.com");
		let router = builder.build();

		let repo = sample_repo(host);
		let cr = router
			.view_change_request(&repo, 7)
			.await
			.expect("change request via live client");
		assert_eq!(cr.number, 7);
		assert_eq!(cr.title.as_str(), "Fix the widget");

		let issue = router
			.view_issue(&repo, 3)
			.await
			.expect("issue via live client");
		assert_eq!(issue.number, 3);

		let recorded = calls.lock().clone();
		assert_eq!(recorded.len(), 2);
		assert_eq!(recorded[0].0, paths::CHANGE_REQUEST_VIEW);
		assert_eq!(recorded[1].0, paths::ISSUE_VIEW);
	}
}
