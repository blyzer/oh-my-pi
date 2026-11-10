//! Direct GitHub API host.
//!
//! Change-request worktree checkout/push for `forge@1` runs through
//! [`crate::forge_worktree`] (ADR 0049 D2.4). This module still owns GitHub
//! REST, Actions watch, credentialed HTTP, and residual Operations used by
//! the in-core GHE CI / list-URL paths.

use std::{
	fmt::Write as _,
	io,
	path::{Path, PathBuf},
	sync::Arc,
	time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use bytes::BytesMut;
use futures::StreamExt as _;
use http::{
	HeaderMap, HeaderValue,
	header::{ACCEPT, LOCATION, RETRY_AFTER, USER_AGENT},
};
use omp_ai::auth::HeaderPlacement;
use omp_cache::forge_cache::ForgeCache;
use omp_core::{Str, sf};
use omp_journal::blob::BlobRef;
use omp_vcs::{PushOptions, git::GitRepo};
use serde_json::{Value, json};
use strum::EnumMessage as _;
use tokio::{task, time};
use tokio_util::sync::CancellationToken;

use super::{
	blobs::BlobHost,
	github_params::{Artifact, DateField, Fault, Operation, Params, Payload, PrSelector, Update},
	github_url::{self, GithubCredentialBridge, GithubRepo},
};

const MAX_BODY: usize = 16 * 1024 * 1024;
/// Scheme reads keep the historical 8 MiB ceiling for a diff or a comment page
/// set.
const SCHEME_TEXT_LIMIT: usize = 8 * 1024 * 1024;
const SEARCH_LIMIT_DEFAULT: u32 = 10;
const SEARCH_LIMIT_MAX: u32 = 50;
const RUN_WATCH_TAIL_DEFAULT: usize = 15;
const RUN_WATCH_TAIL_MAX: usize = 200;
const RUN_WATCH_FAST_WINDOW: Duration = Duration::from_secs(60);
const RUN_WATCH_NO_RUNS_LIMIT: Duration = Duration::from_secs(90);
const RUN_WATCH_RATE_LIMIT_RETRIES: u8 = 5;

/// Combined-credential GitHub owner.
pub(crate) struct GithubService {
	root:        PathBuf,
	worktrees:   PathBuf,
	credentials: Arc<GithubCredentialBridge>,
	cache:       Arc<ForgeCache>,
	blobs:       BlobHost,
	client:      omp_http::Client,
}

impl GithubService {
	pub(crate) fn new(
		root: PathBuf,
		state_dir: &Path,
		credentials: Arc<GithubCredentialBridge>,
		cache: Arc<ForgeCache>,
		blobs: BlobHost,
	) -> Arc<Self> {
		Arc::new(Self {
			root,
			worktrees: crate::forge_worktree::worktree_root(state_dir),
			credentials,
			cache,
			blobs,
			client: omp_http::no_redirect_client(),
		})
	}

	/// Isolated change-request worktree root (`forge-worktrees/`).
	pub(crate) fn worktree_root(&self) -> &Path {
		&self.worktrees
	}

	#[tracing::instrument(
		name = "github_api_request",
		level = "debug",
		skip_all,
		fields(
			method = ?method,
			path = %api_path(path),
		),
	)]
	async fn request(
		&self,
		host: &str,
		method: Method,
		path: &str,
		body: Option<&Value>,
		cancellation: &CancellationToken,
	) -> Result<ApiResponse, Fault> {
		let mut headers = self.api_headers(host, cancellation).await?;
		if method == Method::GetTextMatch {
			headers.insert(ACCEPT, HeaderValue::from_static("application/vnd.github.text-match+json"));
		}
		if method == Method::GetDiff {
			headers.insert(ACCEPT, HeaderValue::from_static("application/vnd.github.v3.diff"));
		}
		let url = github_url::api_url_for_host(host, path);
		let request = match method {
			Method::Get | Method::GetTextMatch | Method::GetDiff => self.client.get(url),
			Method::Post => self.client.post(url),
		};
		let request = if let Some(body) = body {
			request.json(body)
		} else {
			request
		};
		let response = tokio::select! {
			result = request.headers(headers).send() => result.map_err(http_fault)?,
			() = cancellation.cancelled() => return Err(cancelled_fault()),
		};
		let status = response.status().as_u16();
		let remaining = response
			.headers()
			.get("x-ratelimit-remaining")
			.and_then(|value| value.to_str().ok())
			.and_then(|value| value.parse().ok());
		let reset = response
			.headers()
			.get("x-ratelimit-reset")
			.and_then(|value| value.to_str().ok())
			.and_then(|value| value.parse().ok());
		let retry_after = response
			.headers()
			.get(RETRY_AFTER)
			.and_then(|value| value.to_str().ok())
			.and_then(|value| value.parse().ok());
		let bytes = read_body(response, cancellation).await?;
		if !(200..300).contains(&status) {
			let remote_message = serde_json::from_slice::<Value>(&bytes)
				.ok()
				.and_then(|value| {
					value
						.get("message")
						.and_then(Value::as_str)
						.map(sanitize_remote_message)
				});
			let rate_limited = status == 429
				|| remaining == Some(0)
				|| remote_message.as_deref().is_some_and(|message| {
					message.to_ascii_lowercase().contains("rate limit")
						|| message.to_ascii_lowercase().contains("abuse detection")
				});
			let (code, message) = if rate_limited {
				(
					sf!("github_rate_limited"),
					sf!("GitHub rate limit was reached; retry after the reported reset"),
				)
			} else if matches!(status, 401 | 403) {
				if status == 401 {
					self.credentials.reject_host(host);
				}
				(
					sf!("github_authentication_failed"),
					sf!("GitHub authentication or authorization failed"),
				)
			} else {
				(
					sf!("github_http_error"),
					remote_message
						.map(Str::new)
						.unwrap_or_else(|| sf!("GitHub API request failed")),
				)
			};
			return Err(Fault {
				code,
				message,
				status: Some(status),
				rate_limit_remaining: remaining,
				rate_limit_reset: reset,
				retry_after_seconds: retry_after,
			});
		}
		if method == Method::GetDiff {
			if bytes.len() > SCHEME_TEXT_LIMIT {
				return Err(fault("github_response_too_large", "GitHub response exceeds 8 MiB"));
			}
			let text = std::str::from_utf8(&bytes)
				.map_err(|_| fault("github_invalid_response", "GitHub diff is not UTF-8"))?;
			return Ok(ApiResponse {
				value: Value::String(text.to_owned()),
				remaining,
				reset,
				artifact: None,
			});
		}
		let value = if bytes.is_empty() {
			Value::Null
		} else {
			serde_json::from_slice(&bytes)
				.map_err(|_| fault("github_invalid_response", "GitHub returned malformed JSON"))?
		};
		Ok(ApiResponse { value, remaining, reset, artifact: None })
	}

	/// Builds the authenticated GitHub API header set for one request to
	/// `host`; the credential is host-scoped (see
	/// [`GithubCredentialBridge::lease_for_host`]).
	async fn api_headers(
		&self,
		host: &str,
		cancellation: &CancellationToken,
	) -> Result<HeaderMap, Fault> {
		let mut headers = HeaderMap::new();
		headers.insert(USER_AGENT, HeaderValue::from_static("omp-github-device"));
		headers.insert(ACCEPT, HeaderValue::from_static("application/vnd.github+json"));
		headers.insert("x-github-api-version", HeaderValue::from_static("2022-11-28"));
		if let Some(lease) = tokio::select! {
			result = self.credentials.lease_for_host(host, &self.root) => result,
			() = cancellation.cancelled() => return Err(cancelled_fault()),
		}
		.map_err(|_| fault("github_credentials_failed", "GitHub credential lease failed"))?
		{
			lease
				.apply_header(&HeaderPlacement::bearer(), &mut headers)
				.map_err(|_| {
					fault("github_credentials_failed", "GitHub credential projection failed")
				})?;
		}
		Ok(headers)
	}

	/// Downloads one Actions job log as text.
	///
	/// GitHub answers the logs endpoint with a redirect to short-lived blob
	/// storage; the redirect is followed once without credentials. Missing or
	/// expired logs resolve to `None` rather than failing the watch.
	async fn job_log(
		&self,
		repo: &GithubRepo,
		job_id: u64,
		cancellation: &CancellationToken,
	) -> Result<Option<String>, Fault> {
		let headers = self.api_headers(repo.host(), cancellation).await?;
		let url = github_url::api_url_for_host(
			repo.host(),
			&format!("/repos/{}/actions/jobs/{job_id}/logs", repo.slug()),
		);
		let response = tokio::select! {
			result = self.client.get(url).headers(headers).send() => result.map_err(http_fault)?,
			() = cancellation.cancelled() => return Err(cancelled_fault()),
		};
		let response = if response.status().is_redirection() {
			let Some(location) = response
				.headers()
				.get(LOCATION)
				.and_then(|value| value.to_str().ok())
				.map(str::to_owned)
			else {
				return Ok(None);
			};
			tokio::select! {
				result = self.client.get(location).send() => result.map_err(http_fault)?,
				() = cancellation.cancelled() => return Err(cancelled_fault()),
			}
		} else {
			response
		};
		if !response.status().is_success() {
			return Ok(None);
		}
		let bytes = read_body(response, cancellation).await?;
		Ok(Some(String::from_utf8_lossy(&bytes).into_owned()))
	}

	fn repo(&self, requested: Option<&str>) -> Result<GithubRepo, Fault> {
		requested.map_or_else(
			|| {
				github_url::infer_repo(&self.root).map_err(|_| {
					fault(
						"github_repo_unresolved",
						"Cannot infer GitHub repository; pass [host/]owner/repo",
					)
				})
			},
			|repo| {
				GithubRepo::parse(repo).map_err(|_| {
					fault("github_invalid_repo", "GitHub repository must be [host/]owner/repo")
				})
			},
		)
	}

	fn pr_repo(&self, requested: Option<&str>, prs: &[Str]) -> Result<GithubRepo, Fault> {
		if let Some(requested) = requested {
			return self.repo(Some(requested));
		}
		let mut from_url = None;
		for pr in prs {
			let Some((repo, _)) = parse_pr_url(pr)? else {
				continue;
			};
			if from_url.as_ref().is_some_and(|current: &GithubRepo| {
				!current.identity().eq_ignore_ascii_case(repo.identity())
			}) {
				return Err(fault(
					"github_repo_mismatch",
					"pull request URLs in one batch must belong to one repository",
				));
			}
			from_url = Some(repo);
		}
		from_url.map_or_else(|| self.repo(None), Ok)
	}

	fn invalidate_repo(&self, repo: &GithubRepo) -> Result<(), Fault> {
		self
			.cache
			.invalidate_repo(repo.identity())
			.map(|_| ())
			.map_err(|_| fault("forge_cache_failed", "forge cache invalidation failed after mutation"))
	}

	/// Project root the service infers a checkout from.
	pub(crate) fn root(&self) -> &Path {
		&self.root
	}

	/// Repository inferred from the checkout, when the origin names one.
	pub(crate) fn inferred_repo(&self) -> Result<GithubRepo, Fault> {
		self.repo(None)
	}

	/// One Actions run and its jobs. This is the snapshot `ci@1` status
	/// reads. It does not poll.
	pub(crate) async fn actions_run(
		&self,
		repo: &GithubRepo,
		run_id: u64,
		cancellation: &CancellationToken,
	) -> Result<ApiResponse, Fault> {
		self
			.fetch_watch_snapshot(repo, &WatchTarget::Run(run_id), cancellation)
			.await
	}

	/// One Actions job log, when GitHub still has it.
	pub(crate) async fn actions_job_log(
		&self,
		repo: &GithubRepo,
		job_id: u64,
		cancellation: &CancellationToken,
	) -> Result<Option<String>, Fault> {
		self.job_log(repo, job_id, cancellation).await
	}

	/// JSON body of one pull request. HTTP 404 stays a fault with that status.
	pub(crate) async fn pr_view(
		&self,
		repo: &GithubRepo,
		number: u64,
		cancellation: &CancellationToken,
	) -> Result<ApiResponse, Fault> {
		self
			.request(
				repo.host(),
				Method::Get,
				&format!("/repos/{}/pulls/{number}", repo.slug()),
				None,
				cancellation,
			)
			.await
	}

	/// JSON body of one issue. HTTP 404 stays a fault with that status.
	pub(crate) async fn issue_view(
		&self,
		repo: &GithubRepo,
		number: u64,
		cancellation: &CancellationToken,
	) -> Result<ApiResponse, Fault> {
		self
			.request(
				repo.host(),
				Method::Get,
				&format!("/repos/{}/issues/{number}", repo.slug()),
				None,
				cancellation,
			)
			.await
	}

	/// Unified diff text of one pull request (`application/vnd.github.v3.diff`).
	pub(crate) async fn pr_diff(
		&self,
		repo: &GithubRepo,
		number: u64,
		cancellation: &CancellationToken,
	) -> Result<String, Fault> {
		let response = self
			.request(
				repo.host(),
				Method::GetDiff,
				&format!("/repos/{}/pulls/{number}", repo.slug()),
				None,
				cancellation,
			)
			.await?;
		Ok(response.value.as_str().unwrap_or_default().to_owned())
	}

	/// Conversation comments for an issue or a pull request.
	///
	/// GitHub serves both from the issue-comment list. Pages stop at 100
	/// comments or 100 pages, and the retained JSON stays under 8 MiB.
	pub(crate) async fn list_comments(
		&self,
		repo: &GithubRepo,
		number: u64,
		cancellation: &CancellationToken,
	) -> Result<Vec<Value>, Fault> {
		let mut comments = Vec::new();
		let mut retained = 0usize;
		for page in 1..=100u32 {
			let response = self
				.request(
					repo.host(),
					Method::Get,
					&format!("/repos/{}/issues/{number}/comments?per_page=100&page={page}", repo.slug()),
					None,
					cancellation,
				)
				.await?;
			let page_comments = match response.value {
				Value::Array(rows) => rows,
				_ => {
					return Err(fault("github_invalid_response", "GitHub comments were not a list"));
				},
			};
			let complete = page_comments.len() < 100;
			for comment in &page_comments {
				retained = retained.saturating_add(comment.to_string().len());
			}
			comments.extend(page_comments);
			if retained > SCHEME_TEXT_LIMIT {
				return Err(fault("github_response_too_large", "GitHub comments exceed 8 MiB"));
			}
			if complete {
				return Ok(comments);
			}
		}
		Err(fault("github_response_too_large", "GitHub comments exceed 100 pages"))
	}

	/// Bytes previously spilled for a file-read artifact.
	pub(crate) fn artifact_bytes(&self, artifact: &Artifact) -> Result<bytes::Bytes, Fault> {
		let Some(hex) = artifact.uri.as_str().strip_prefix("artifact://sha256/") else {
			return Err(fault(
				"github_artifact_failed",
				"GitHub artifact URI is not content-addressed",
			));
		};
		let reference = BlobRef::parse_hex(hex, artifact.size).map_err(|_| {
			fault("github_artifact_failed", "GitHub artifact URI is not a blob digest")
		})?;
		self
			.blobs
			.get(reference.into())
			.map_err(|_| fault("github_artifact_failed", "GitHub file artifact could not be read"))
	}
}

impl GithubService {
	/// Dispatches one GitHub operation.
	///
	/// The forge and CI adapters are the callers. The model-facing tools do
	/// not name this vocabulary.
	#[tracing::instrument(name = "github_operation", level = "debug", skip_all, fields(operation = ?params.op))]
	pub(crate) async fn execute(
		&self,
		params: Params,
		cancellation: CancellationToken,
		updates: flume::Sender<Update>,
	) -> Result<Payload, Fault> {
		let response = match params.op {
			Operation::RepoView => {
				let repo = self.repo(params.repo.as_deref())?;
				let mut response = self
					.request(
						repo.host(),
						Method::Get,
						&format!("/repos/{}", repo.slug()),
						None,
						&cancellation,
					)
					.await?;
				if let Some(branch) = params
					.branch
					.as_deref()
					.map(str::trim)
					.filter(|branch| !branch.is_empty())
					&& let Some(object) = response.value.as_object_mut()
				{
					object.insert("requested_branch".to_owned(), Value::String(branch.to_owned()));
				}
				response
			},
			Operation::FileRead => {
				let repo = self.repo(params.repo.as_deref())?;
				let path = required(params.path.as_deref(), "file_read requires `path`")?;
				let branch = params
					.branch
					.as_deref()
					.map(str::trim)
					.filter(|branch| !branch.is_empty());
				let endpoint = file_endpoint(repo.slug(), path, branch)?;
				let mut response = self
					.request(repo.host(), Method::Get, &endpoint, None, &cancellation)
					.await?;
				let (value, artifact) =
					decode_file_response(&response.value, repo.identity(), path, branch, &self.blobs)?;
				response.value = value;
				response.artifact = artifact;
				response
			},
			Operation::PrCreate => self.create_pr(&params, &cancellation).await?,
			Operation::PrCheckout => self.checkout(&params, &cancellation).await?,
			Operation::PrPush => self.push(&params, &cancellation).await?,
			Operation::SearchIssues
			| Operation::SearchPrs
			| Operation::SearchCode
			| Operation::SearchCommits
			| Operation::SearchRepos => self.search(&params, &cancellation).await?,
			Operation::RunWatch => self.watch(&params, &cancellation, &updates).await?,
		};
		let mut result = response.value;
		let output = if params.op == Operation::FileRead
			&& result.get("binary").and_then(Value::as_bool) != Some(true)
		{
			match result.get_mut("content").map(Value::take) {
				Some(Value::String(output)) => output,
				_ => String::new(),
			}
		} else {
			render_output(params.op, &result, response.artifact.as_ref())
		};
		let useless = operation_is_useless(params.op, &result);
		Ok(Payload {
			op: params.op,
			result,
			output: Str::new(output),
			artifact: response.artifact,
			useless,
			rate_limit_remaining: response.remaining,
			rate_limit_reset: response.reset,
		})
	}
}

impl GithubService {
	async fn create_pr(
		&self,
		params: &Params,
		cancellation: &CancellationToken,
	) -> Result<ApiResponse, Fault> {
		let repo = self.repo(params.repo.as_deref())?;
		let inferred_head;
		let head = if let Some(head) = params
			.head
			.as_deref()
			.map(str::trim)
			.filter(|head| !head.is_empty())
		{
			head
		} else {
			let root = self.root.clone();
			let worker = task::spawn_blocking(move || current_git_snapshot(&root));
			let (branch, _) = tokio::select! {
				result = worker => result
					.map_err(|_| fault("github_git_failed", "GitHub Git lookup worker failed"))??,
				() = cancellation.cancelled() => return Err(cancelled_fault()),
			};
			inferred_head = branch;
			&inferred_head
		};
		let default_base;
		let base = if let Some(base) = params
			.base
			.as_deref()
			.map(str::trim)
			.filter(|base| !base.is_empty())
		{
			base
		} else {
			let metadata = self
				.request(
					repo.host(),
					Method::Get,
					&format!("/repos/{}", repo.slug()),
					None,
					cancellation,
				)
				.await?;
			default_base = metadata
				.value
				.get("default_branch")
				.and_then(Value::as_str)
				.ok_or_else(|| {
					fault("github_invalid_response", "GitHub repository has no default branch")
				})?
				.to_owned();
			&default_base
		};
		let pushed = self.ensure_head_pushed(&repo, head, cancellation).await?;
		let title = params
			.title
			.as_deref()
			.map(str::trim)
			.filter(|title| !title.is_empty());
		let (title, body) = if params.fill {
			if title.is_some() || params.body.is_some() {
				return Err(fault(
					"github_invalid_request",
					"fill is mutually exclusive with title and body",
				));
			}
			let compare = self
				.request(
					repo.host(),
					Method::Get,
					&compare_endpoint(repo.slug(), base, head),
					None,
					cancellation,
				)
				.await?;
			let messages = compare
				.value
				.get("commits")
				.and_then(Value::as_array)
				.ok_or_else(|| {
					fault("github_invalid_response", "GitHub compare response has no commit list")
				})?
				.iter()
				.filter_map(|commit| commit.pointer("/commit/message").and_then(Value::as_str))
				.collect::<Vec<_>>();
			fill_from_commits(head, &messages)?
		} else {
			let title = required(title, "title is required unless fill is true")?;
			(title.to_owned(), params.body.as_deref().unwrap_or("").to_owned())
		};
		let request = json!({
			"title": title,
			"body": body,
			"head": head,
			"base": base,
			"draft": params.draft,
		});
		let mut created = self
			.request(
				repo.host(),
				Method::Post,
				&format!("/repos/{}/pulls", repo.slug()),
				Some(&request),
				cancellation,
			)
			.await?;
		self.invalidate_repo(&repo)?;
		let metadata = PrMetadata::from_params(params);
		if metadata.is_empty() {
			attach_push_receipt(&mut created.value, pushed);
			return Ok(created);
		}
		let number = created
			.value
			.get("number")
			.and_then(Value::as_u64)
			.ok_or_else(|| fault("github_invalid_response", "created pull request has no number"))?;
		for (endpoint, body) in metadata.requests(repo.slug(), number) {
			self
				.request(repo.host(), Method::Post, &endpoint, Some(&body), cancellation)
				.await?;
		}
		let refreshed = self
			.request(
				repo.host(),
				Method::Get,
				&format!("/repos/{}/pulls/{number}", repo.slug()),
				None,
				cancellation,
			)
			.await?;
		created.value = refreshed.value;
		created.remaining = refreshed.remaining;
		created.reset = refreshed.reset;
		attach_push_receipt(&mut created.value, pushed);
		Ok(created)
	}

	/// Publishes a local head branch before a pull request is opened from it.
	///
	/// Applies only when `head` is an unqualified branch that exists locally;
	/// `owner:branch` heads and branches that exist only remotely are left as
	/// they are. When the remote branch is missing or points elsewhere, the
	/// local branch is pushed **without force** to the remote that maps to
	/// `repo` (or its HTTPS URL), so a remote that has diverged is rejected by
	/// Git rather than overwritten. Returns the push receipt, if one happened.
	async fn ensure_head_pushed(
		&self,
		repo: &GithubRepo,
		head: &str,
		cancellation: &CancellationToken,
	) -> Result<Option<PushReceipt>, Fault> {
		if head.contains(':') {
			return Ok(None);
		}
		let root = self.root.clone();
		let branch = head.to_owned();
		let worker = task::spawn_blocking(move || local_branch_target(&root, &branch));
		let local = tokio::select! {
			result = worker => result
				.map_err(|_| fault("github_git_failed", "GitHub Git lookup worker failed"))??,
			() = cancellation.cancelled() => return Err(cancelled_fault()),
		};
		let Some((local_sha, remote_name)) = local else {
			return Ok(None);
		};
		let remote_sha = match self
			.request(repo.host(), Method::Get, &branch_endpoint(repo.slug(), head), None, cancellation)
			.await
		{
			Ok(branch) => branch
				.value
				.pointer("/commit/sha")
				.and_then(Value::as_str)
				.map(str::to_owned),
			Err(error) if error.status == Some(404) => None,
			Err(error) => return Err(error),
		};
		if remote_sha.as_deref() == Some(local_sha.as_str()) {
			return Ok(None);
		}
		let metadata = self
			.request(repo.host(), Method::Get, &format!("/repos/{}", repo.slug()), None, cancellation)
			.await?;
		if metadata.value.pointer("/permissions/push") == Some(&Value::Bool(false)) {
			return Err(fault(
				"github_push_forbidden",
				"no push access to the target repository; push the branch to a fork and pass head as \
				 owner:branch",
			));
		}
		let root = self.root.clone();
		let target_repo = repo.clone();
		let (repository, target) = task::spawn_blocking(move || {
			let local = GitRepo::require(&root).map(Arc::new)?;
			let target = push_target(&local, remote_name, &target_repo);
			Ok::<_, omp_vcs::Error>((local, target))
		})
		.await
		.map_err(|_| fault("github_git_failed", "GitHub push worker failed"))?
		.map_err(git_fault)?;
		let lockable = crate::vcs::git::repo::Repository::from_handle(Arc::clone(&repository));
		let _guard = crate::vcs::git::lock::write(&lockable, cancellation)
			.await
			.map_err(|_| fault("github_git_failed", "Git repository mutation lock failed"))?;
		repository
			.push(
				&PushOptions {
					remote:           Some(target.clone()),
					refspec:          Some(format!("refs/heads/{head}:refs/heads/{head}")),
					force_with_lease: None,
				},
				Some(cancellation.clone()),
			)
			.await
			.map_err(|_| {
				fault(
					"github_push_rejected",
					"pushing the head branch failed; the remote branch may have commits the local \
					 branch lacks, or Git credentials for the remote are missing",
				)
			})?;
		Ok(Some(PushReceipt {
			branch:   head.to_owned(),
			remote:   target,
			sha:      local_sha,
			replaced: remote_sha,
		}))
	}

	async fn search(
		&self,
		params: &Params,
		cancellation: &CancellationToken,
	) -> Result<ApiResponse, Fault> {
		let (kind, tag) = match params.op {
			Operation::SearchIssues => ("issues", Some("is:issue")),
			Operation::SearchPrs => ("issues", Some("is:pr")),
			Operation::SearchCode => ("code", None),
			Operation::SearchCommits => ("commits", None),
			Operation::SearchRepos => ("repositories", None),
			_ => unreachable!(),
		};
		if params.op == Operation::SearchCode
			&& (params.since.is_some() || params.until.is_some() || params.date_field.is_some())
		{
			return Err(fault(
				"github_invalid_search",
				"code search does not support date bounds or dateField",
			));
		}
		let mut query = params.query.as_deref().unwrap_or("").trim().to_owned();
		let caller_scoped = has_scope(&query);
		let explicit = params
			.repo
			.as_deref()
			.map(str::trim)
			.filter(|text| !text.is_empty())
			.map(|text| self.repo(Some(text)))
			.transpose()?;
		let repo = if params.op == Operation::SearchRepos {
			None
		} else if let Some(explicit) = explicit.clone() {
			Some(explicit)
		} else if caller_scoped {
			None
		} else {
			Some(self.repo(None)?)
		};
		if let Some(tag) = tag {
			append_query_part(&mut query, tag);
		}
		if params.op != Operation::SearchRepos && !caller_scoped {
			append_query_part(
				&mut query,
				&format!("repo:{}", repo.as_ref().expect("inferred search repository").slug()),
			);
		}
		if let Some(qualifier) = search_date_qualifier(params, SystemTime::now())? {
			append_query_part(&mut query, &qualifier);
		}
		if query.is_empty() {
			return Err(fault("github_invalid_search", "search requires `query` or a date bound"));
		}
		let encoded = encode_query(&query);
		let limit = match params.limit {
			Some(0) => {
				return Err(fault("github_invalid_search", "limit must be a positive number"));
			},
			Some(limit) => limit.min(SEARCH_LIMIT_MAX),
			None => SEARCH_LIMIT_DEFAULT,
		};
		let mut response = self
			.request(
				explicit
					.as_ref()
					.or(repo.as_ref())
					.map_or(github_url::GITHUB_HOST, GithubRepo::host),
				if params.op == Operation::SearchCode {
					Method::GetTextMatch
				} else {
					Method::Get
				},
				&format!("/search/{kind}?q={encoded}&per_page={limit}"),
				None,
				cancellation,
			)
			.await?;
		if let Some(object) = response.value.as_object_mut() {
			object.insert("query".to_owned(), Value::String(query));
			object.insert("limit".to_owned(), Value::from(limit));
			if let Some(repo) = repo {
				object.insert("repository".to_owned(), Value::String(repo.identity().to_owned()));
			}
		}
		Ok(response)
	}

	/// Open change-request number whose head is `branch`.
	///
	/// [`crate::github_adapter::CheckoutForge`] passes this number to the
	/// forge router's `view_change_request`. This method does not fetch the
	/// record.
	pub(crate) async fn open_change_request_number(
		&self,
		repo: &GithubRepo,
		branch: &str,
		cancellation: &CancellationToken,
	) -> Result<u64, Fault> {
		let branch = branch.trim();
		if branch.is_empty() || branch.starts_with('-') {
			return Err(fault(
				"github_invalid_pr",
				"pull request must be a positive number, URL, or branch name",
			));
		}
		let owner = repo.slug().split_once('/').expect("validated repository").0;
		let head = if branch.contains(':') {
			branch.to_owned()
		} else {
			format!("{owner}:{branch}")
		};
		let endpoint = pr_branch_endpoint(repo.slug(), &head);
		let matches = self
			.request(repo.host(), Method::Get, &endpoint, None, cancellation)
			.await?;
		let rows = matches.value.as_array().ok_or_else(|| {
			fault("github_invalid_response", "GitHub pull request lookup was not a list")
		})?;
		let mut numbers = rows
			.iter()
			.filter_map(|row| row.get("number").and_then(Value::as_u64))
			.collect::<Vec<_>>();
		if numbers.is_empty() {
			for page in 1..=100u32 {
				let response = self
					.request(
						repo.host(),
						Method::Get,
						&pr_list_endpoint(repo.slug(), page),
						None,
						cancellation,
					)
					.await?;
				let rows = response.value.as_array().ok_or_else(|| {
					fault("github_invalid_response", "GitHub pull request lookup was not a list")
				})?;
				numbers.extend(rows.iter().filter_map(|row| {
					let matches_branch = row.pointer("/head/ref").and_then(Value::as_str)
						== Some(branch)
						|| row.pointer("/head/label").and_then(Value::as_str) == Some(branch);
					matches_branch
						.then(|| row.get("number").and_then(Value::as_u64))
						.flatten()
				}));
				if rows.len() < 100 {
					break;
				}
			}
		}
		numbers.sort_unstable();
		numbers.dedup();
		let Some(number) = numbers.first().copied() else {
			return Err(fault(
				"github_invalid_pr",
				"no open pull request has the requested head branch",
			));
		};
		if numbers.len() > 1 {
			return Err(fault(
				"github_invalid_pr",
				"multiple open pull requests have the requested head branch",
			));
		}
		Ok(number)
	}

	async fn resolve_pr(
		&self,
		repo: &GithubRepo,
		selector: &str,
		cancellation: &CancellationToken,
	) -> Result<(u64, ApiResponse), Fault> {
		let number = if let Some(number) = parse_pr_number(selector, repo)? {
			number
		} else {
			self
				.open_change_request_number(repo, selector, cancellation)
				.await?
		};
		let response = self
			.request(
				repo.host(),
				Method::Get,
				&format!("/repos/{}/pulls/{number}", repo.slug()),
				None,
				cancellation,
			)
			.await?;
		Ok((number, response))
	}

	/// Vendor plan fields for a pull request (API only; no local git).
	///
	/// Used by [`crate::github_adapter::GithubForgeAdapter`] before core
	/// [`crate::forge_worktree`] runs the checkout (ADR 0049 D2.4).
	pub(crate) async fn change_request_checkout_plan(
		&self,
		repo: &GithubRepo,
		number: u64,
		cancellation: &CancellationToken,
	) -> Result<crate::forge_worktree::CheckoutInput, Fault> {
		let (_, api) = self
			.resolve_pr(repo, &number.to_string(), cancellation)
			.await?;
		let clone_url = api
			.value
			.pointer("/head/repo/clone_url")
			.and_then(Value::as_str)
			.ok_or_else(|| {
				fault("github_invalid_response", "pull request head repository is unavailable")
			})?;
		let head_ref = api
			.value
			.pointer("/head/ref")
			.and_then(Value::as_str)
			.ok_or_else(|| {
				fault("github_invalid_response", "pull request head branch is unavailable")
			})?;
		let head_sha = api
			.value
			.pointer("/head/sha")
			.and_then(Value::as_str)
			.ok_or_else(|| {
				fault("github_invalid_response", "pull request head commit is unavailable")
			})?;
		let base_ref = api
			.value
			.pointer("/base/ref")
			.and_then(Value::as_str)
			.unwrap_or_default();
		Ok(crate::forge_worktree::CheckoutInput {
			repo_identity: repo.identity().to_owned(),
			number,
			clone_url: clone_url.to_owned(),
			base_ref: base_ref.to_owned(),
			head_ref: head_ref.to_owned(),
			head_sha: head_sha.to_owned(),
			force: false,
		})
	}

	/// Vendor push-prep fields for a pull request (API only; no local git).
	pub(crate) async fn change_request_push_prep(
		&self,
		repo: &GithubRepo,
		number: u64,
		cancellation: &CancellationToken,
	) -> Result<crate::forge_worktree::PushInput, Fault> {
		let (_, api) = self
			.resolve_pr(repo, &number.to_string(), cancellation)
			.await?;
		let clone_url = api
			.value
			.pointer("/head/repo/clone_url")
			.and_then(Value::as_str)
			.ok_or_else(|| {
				fault("github_invalid_response", "pull request head repository is unavailable")
			})?;
		let head_ref = api
			.value
			.pointer("/head/ref")
			.and_then(Value::as_str)
			.ok_or_else(|| {
				fault("github_invalid_response", "pull request head branch is unavailable")
			})?;
		Ok(crate::forge_worktree::PushInput {
			repo_identity: repo.identity().to_owned(),
			number,
			clone_url: clone_url.to_owned(),
			head_ref: head_ref.to_owned(),
			force_with_lease: false,
		})
	}

	async fn checkout(
		&self,
		params: &Params,
		cancellation: &CancellationToken,
	) -> Result<ApiResponse, Fault> {
		let prs = params
			.pr
			.as_ref()
			.map(PrSelector::as_slice)
			.ok_or_else(|| fault("github_invalid_pr", "pr_checkout requires `pr`"))?;
		if prs.is_empty() {
			return Err(fault("github_invalid_pr", "pr_checkout requires at least one `pr`"));
		}
		let repo = self.pr_repo(params.repo.as_deref(), prs)?;
		let mut checkouts = Vec::new();
		let mut failures = Vec::new();
		let mut first_failure = None;
		for value in prs {
			match self
				.checkout_one(&repo, value, params.force, cancellation)
				.await
			{
				Ok(checkout) => checkouts.push(checkout),
				Err(error) if error.code == "github_cancelled" => return Err(error),
				Err(error) => {
					failures.push(json!({
						"pr": value,
						"code": error.code.clone(),
						"message": error.message.clone(),
					}));
					first_failure.get_or_insert(error);
				},
			}
		}
		if checkouts.is_empty() {
			return Err(
				first_failure
					.unwrap_or_else(|| fault("github_git_failed", "all pull request checkouts failed")),
			);
		}
		Ok(ApiResponse {
			value:     json!({ "checkouts": checkouts, "failures": failures }),
			remaining: None,
			reset:     None,
			artifact:  None,
		})
	}

	async fn checkout_one(
		&self,
		repo: &GithubRepo,
		value: &str,
		force: bool,
		cancellation: &CancellationToken,
	) -> Result<Value, Fault> {
		let (number, api) = self.resolve_pr(repo, value, cancellation).await?;
		let clone_url = api
			.value
			.pointer("/head/repo/clone_url")
			.and_then(Value::as_str)
			.ok_or_else(|| {
				fault("github_invalid_response", "pull request head repository is unavailable")
			})?;
		let head = api
			.value
			.pointer("/head/ref")
			.and_then(Value::as_str)
			.ok_or_else(|| {
				fault("github_invalid_response", "pull request head branch is unavailable")
			})?;
		let expected_head = api
			.value
			.pointer("/head/sha")
			.and_then(Value::as_str)
			.ok_or_else(|| {
				fault("github_invalid_response", "pull request head commit is unavailable")
			})?;
		let base_ref = api
			.value
			.pointer("/base/ref")
			.and_then(Value::as_str)
			.unwrap_or_default();
		let path = crate::forge_worktree::worktree_path(&self.worktrees, number);
		let reused = path.exists();
		let out = crate::forge_worktree::checkout(
			&self.root,
			&self.worktrees,
			&crate::forge_worktree::CheckoutInput {
				repo_identity: repo.identity().to_owned(),
				number,
				clone_url: clone_url.to_owned(),
				base_ref: base_ref.to_owned(),
				head_ref: head.to_owned(),
				head_sha: expected_head.to_owned(),
				force,
			},
			cancellation,
		)
		.await
		.map_err(forge_worktree_fault)?;
		Ok(json!({
			"pr": number,
			"url": api.value.get("html_url").and_then(Value::as_str),
			"branch": format!("cr-{number}"),
			"path": out.worktree,
			"remote": clone_url,
			"remote_branch": head,
			"head_sha": expected_head,
			"base_ref": base_ref,
			"reused": reused,
		}))
	}

	async fn push(
		&self,
		params: &Params,
		cancellation: &CancellationToken,
	) -> Result<ApiResponse, Fault> {
		let prs = params
			.pr
			.as_ref()
			.map(PrSelector::as_slice)
			.ok_or_else(|| {
				fault("github_invalid_pr", "pr_push requires `pr` from a prior checkout")
			})?;
		if prs.is_empty() {
			return Err(fault("github_invalid_pr", "pr_push requires at least one `pr`"));
		}
		let repo = self.pr_repo(params.repo.as_deref(), prs)?;
		let mut pushed = Vec::new();
		let mut failures = Vec::new();
		let mut first_failure = None;
		for value in prs {
			match self
				.push_one(&repo, value, params.force_with_lease, cancellation)
				.await
			{
				Ok(result) => pushed.push(result),
				Err(error) if error.code == "github_cancelled" => return Err(error),
				Err(error) => {
					failures.push(json!({
						"pr": value,
						"code": error.code.clone(),
						"message": error.message.clone(),
					}));
					first_failure.get_or_insert(error);
				},
			}
		}
		if pushed.is_empty() {
			return Err(
				first_failure
					.unwrap_or_else(|| fault("github_git_failed", "all pull request pushes failed")),
			);
		}
		Ok(ApiResponse {
			value:     json!({ "pushed": pushed, "failures": failures }),
			remaining: None,
			reset:     None,
			artifact:  None,
		})
	}

	async fn push_one(
		&self,
		repo: &GithubRepo,
		value: &str,
		force: bool,
		cancellation: &CancellationToken,
	) -> Result<Value, Fault> {
		let (number, api) = self.resolve_pr(repo, value, cancellation).await?;
		let clone_url = api
			.value
			.pointer("/head/repo/clone_url")
			.and_then(Value::as_str)
			.ok_or_else(|| {
				fault("github_invalid_response", "pull request head repository is unavailable")
			})?;
		let head_ref = api
			.value
			.pointer("/head/ref")
			.and_then(Value::as_str)
			.ok_or_else(|| {
				fault("github_invalid_response", "pull request head branch is unavailable")
			})?;
		let path = crate::forge_worktree::worktree_path(&self.worktrees, number);
		let out = crate::forge_worktree::push(
			&self.worktrees,
			&crate::forge_worktree::PushInput {
				repo_identity: repo.identity().to_owned(),
				number,
				clone_url: clone_url.to_owned(),
				head_ref: head_ref.to_owned(),
				force_with_lease: force,
			},
			cancellation,
		)
		.await
		.map_err(forge_worktree_fault)?;
		self.invalidate_repo(repo)?;
		Ok(json!({
			"pr": number,
			"path": path,
			"remote_branch": head_ref,
			"head_sha": out.head_commit.as_str(),
			"force_with_lease": force,
		}))
	}

	async fn fetch_watch_snapshot(
		&self,
		repo: &GithubRepo,
		target: &WatchTarget,
		cancellation: &CancellationToken,
	) -> Result<ApiResponse, Fault> {
		match target {
			WatchTarget::Run(id) => {
				let mut run = self
					.request(
						repo.host(),
						Method::Get,
						&format!("/repos/{}/actions/runs/{id}", repo.slug()),
						None,
						cancellation,
					)
					.await?;
				let jobs = self.fetch_run_jobs(repo, *id, cancellation).await?;
				run.value = json!({ "run": run.value, "jobs": jobs });
				Ok(run)
			},
			WatchTarget::Commit { branch, head_sha } => {
				let mut runs = self
					.request(
						repo.host(),
						Method::Get,
						&actions_runs_endpoint(repo.slug(), head_sha),
						None,
						cancellation,
					)
					.await?;
				self
					.attach_run_jobs(repo, &mut runs.value, cancellation)
					.await?;
				if let Some(object) = runs.value.as_object_mut() {
					object.insert("branch".to_owned(), Value::String(branch.to_string()));
					object.insert("head_sha".to_owned(), Value::String(head_sha.to_string()));
				}
				Ok(runs)
			},
		}
	}

	async fn watch(
		&self,
		params: &Params,
		cancellation: &CancellationToken,
		updates: &flume::Sender<Update>,
	) -> Result<ApiResponse, Fault> {
		let tail = tail_limit(params.tail)?;
		let run = params.run.as_deref().map(parse_run_reference).transpose()?;
		let repo = match (params.repo.as_deref(), run.as_ref().and_then(|run| run.repo.as_ref())) {
			(Some(requested), Some(from_run)) => {
				let requested = self.repo(Some(requested))?;
				if !requested
					.identity()
					.eq_ignore_ascii_case(from_run.identity())
				{
					return Err(fault(
						"github_repo_mismatch",
						"Actions run URL belongs to a different repository",
					));
				}
				requested
			},
			(Some(requested), None) => self.repo(Some(requested))?,
			(None, Some(from_run)) => from_run.clone(),
			(None, None) => self.repo(None)?,
		};
		let target = if let Some(run) = run {
			WatchTarget::Run(run.id)
		} else if let Some(branch) = params
			.branch
			.as_deref()
			.map(str::trim)
			.filter(|branch| !branch.is_empty())
		{
			let branch_endpoint = branch_endpoint(repo.slug(), branch);
			let response = self
				.request(repo.host(), Method::Get, &branch_endpoint, None, cancellation)
				.await?;
			let head_sha = response
				.value
				.pointer("/commit/sha")
				.and_then(Value::as_str)
				.ok_or_else(|| fault("github_invalid_response", "GitHub branch has no head commit"))?;
			WatchTarget::Commit { branch: Str::new(branch), head_sha: Str::new(head_sha) }
		} else {
			let current_repo = self.repo(None)?;
			if !current_repo
				.identity()
				.eq_ignore_ascii_case(repo.identity())
			{
				return Err(fault(
					"github_repo_mismatch",
					"current checkout does not match `repo`; pass `branch` or `run`",
				));
			}
			let root = self.root.clone();
			let worker = task::spawn_blocking(move || current_git_snapshot(&root));
			let (branch, head_sha) = tokio::select! {
				result = worker => result
					.map_err(|_| fault("github_git_failed", "GitHub Git lookup worker failed"))??,
				() = cancellation.cancelled() => return Err(cancelled_fault()),
			};
			WatchTarget::Commit { branch, head_sha }
		};

		let started = Instant::now();
		let mut poll_count = 0u32;
		let mut rate_limit_failures = 0u8;
		let mut ever_saw_runs = false;
		let mut successful_signature = None;
		loop {
			poll_count = poll_count.saturating_add(1);
			let fetched = self
				.fetch_watch_snapshot(&repo, &target, cancellation)
				.await;
			let mut response = match fetched {
				Ok(response) => {
					rate_limit_failures = 0;
					response
				},
				Err(error)
					if error.is_rate_limited() && rate_limit_failures < RUN_WATCH_RATE_LIMIT_RETRIES =>
				{
					rate_limit_failures += 1;
					let delay = Duration::from_secs(error.retry_after_seconds.unwrap_or(15).min(60));
					poll_sleep(delay, cancellation).await?;
					continue;
				},
				Err(error) => return Err(error),
			};
			let run_count = response
				.value
				.get("workflow_runs")
				.and_then(Value::as_array)
				.map_or(1, Vec::len);
			ever_saw_runs |= run_count != 0;
			let state = actions_state(&response.value);
			if let Some(object) = response.value.as_object_mut() {
				object.insert("poll_count".to_owned(), Value::from(poll_count));
				object
					.insert("outcome".to_owned(), Value::String(<&'static str>::from(state).to_owned()));
			}
			let output = render_output(Operation::RunWatch, &response.value, None);
			let _ = updates.try_send(Update {
				op:     Operation::RunWatch,
				result: response.value.clone(),
				output: Str::new(output),
			});

			if state == ActionsState::Failure {
				poll_sleep(Duration::from_secs(5), cancellation).await?;
				if let Ok(mut refetched) = self
					.fetch_watch_snapshot(&repo, &target, cancellation)
					.await && actions_state(&refetched.value) == ActionsState::Failure
				{
					if let Some(object) = refetched.value.as_object_mut() {
						object.insert("poll_count".to_owned(), Value::from(poll_count));
						object.insert("outcome".to_owned(), Value::String("failure".to_owned()));
					}
					response = refetched;
				}
				let failed = self
					.failed_job_logs(&repo, &response.value, tail, cancellation)
					.await?;
				if let Some(object) = response.value.as_object_mut() {
					object.insert("tail".to_owned(), Value::from(tail));
					object.insert("failed_logs".to_owned(), Value::Array(failed.rows));
				}
				response.artifact = failed.artifact;
				return Ok(response);
			}
			if state == ActionsState::Success {
				if matches!(&target, WatchTarget::Run(_)) {
					return Ok(response);
				}
				let signature = actions_signature(&response.value);
				if successful_signature.as_deref() == Some(signature.as_str()) {
					return Ok(response);
				}
				successful_signature = Some(signature);
			} else {
				successful_signature = None;
			}
			if !ever_saw_runs && started.elapsed() >= RUN_WATCH_NO_RUNS_LIMIT {
				if let Some(object) = response.value.as_object_mut() {
					object.insert("outcome".to_owned(), Value::String("no_runs".to_owned()));
				}
				return Ok(response);
			}
			let delay = if started.elapsed() < RUN_WATCH_FAST_WINDOW {
				Duration::from_secs(3)
			} else {
				Duration::from_secs(15)
			};
			poll_sleep(delay, cancellation).await?;
		}
	}

	/// Fetches bounded tails for failed jobs and retains their complete logs as
	/// one content-addressed artifact.
	async fn failed_job_logs(
		&self,
		repo: &GithubRepo,
		value: &Value,
		tail: usize,
		cancellation: &CancellationToken,
	) -> Result<FailedLogs, Fault> {
		let mut rows = Vec::new();
		let mut complete = None;
		for (run_id, job) in failed_jobs(value) {
			let Some(job_id) = job.get("id").and_then(Value::as_u64) else {
				continue;
			};
			let full = self.job_log(repo, job_id, cancellation).await?;
			let tail_text = full.as_deref().and_then(|log| tail_lines(log, tail));
			if let Some(full) = &full {
				let name = job
					.get("name")
					.and_then(Value::as_str)
					.unwrap_or("unnamed job");
				let stage = match complete.as_mut() {
					Some(stage) => stage,
					None => complete.insert(self.blobs.begin_spill().map_err(|_| {
						fault("github_artifact_failed", "GitHub output artifact could not be opened")
					})?),
				};
				let heading = format!("## {name} (run {run_id}, job {job_id})\n\n");
				io::Write::write_all(stage, heading.as_bytes()).map_err(|_| {
					fault("github_artifact_failed", "GitHub output artifact could not be written")
				})?;
				io::Write::write_all(stage, full.as_bytes()).map_err(|_| {
					fault("github_artifact_failed", "GitHub output artifact could not be written")
				})?;
				io::Write::write_all(stage, b"\n\n").map_err(|_| {
					fault("github_artifact_failed", "GitHub output artifact could not be written")
				})?;
			}
			rows.push(json!({
				"run_id": run_id,
				"job_id": job_id,
				"job_name": job.get("name").and_then(Value::as_str),
				"conclusion": job.get("conclusion").and_then(Value::as_str),
				"available": tail_text.is_some(),
				"tail": tail_text,
			}));
		}
		let artifact = complete
			.map(|stage| {
				stage
					.finish()
					.map_err(|_| {
						fault("github_artifact_failed", "GitHub output artifact could not be committed")
					})
					.map(|reference| Artifact {
						uri:        Str::new(format!(
							"artifact://sha256/{}",
							reference.to_hex().as_str()
						)),
						size:       reference.size,
						media_type: sf!("text/plain"),
					})
			})
			.transpose()?;
		Ok(FailedLogs { rows, artifact })
	}

	async fn fetch_run_jobs(
		&self,
		repo: &GithubRepo,
		run_id: u64,
		cancellation: &CancellationToken,
	) -> Result<Value, Fault> {
		let mut jobs = Vec::new();
		for page in 1..=100u32 {
			let response = self
				.request(
					repo.host(),
					Method::Get,
					&run_jobs_endpoint(repo.slug(), run_id, page),
					None,
					cancellation,
				)
				.await?;
			let page_jobs = response
				.value
				.get("jobs")
				.and_then(Value::as_array)
				.ok_or_else(|| {
					fault("github_invalid_response", "GitHub Actions jobs were not a list")
				})?;
			let complete = page_jobs.len() < 100;
			jobs.extend(page_jobs.iter().cloned());
			if complete {
				return Ok(Value::Array(jobs));
			}
		}
		Err(fault("github_invalid_response", "GitHub Actions jobs exceeded 100 pages"))
	}

	async fn attach_run_jobs(
		&self,
		repo: &GithubRepo,
		value: &mut Value,
		cancellation: &CancellationToken,
	) -> Result<(), Fault> {
		let runs = value
			.get_mut("workflow_runs")
			.and_then(Value::as_array_mut)
			.ok_or_else(|| {
				fault("github_invalid_response", "GitHub Actions workflow runs were not a list")
			})?;
		for run in runs {
			let id = run.get("id").and_then(Value::as_u64).ok_or_else(|| {
				fault("github_invalid_response", "GitHub Actions run has no numeric id")
			})?;
			let jobs = self.fetch_run_jobs(repo, id, cancellation).await?;
			run.as_object_mut()
				.ok_or_else(|| {
					fault("github_invalid_response", "GitHub Actions run was not an object")
				})?
				.insert("jobs".to_owned(), jobs);
		}
		Ok(())
	}
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Method {
	Get,
	GetTextMatch,
	GetDiff,
	Post,
}
fn api_path(path: &str) -> &str {
	path.split_once('?').map_or(path, |(path, _)| path)
}
#[derive(Clone)]
struct RunReference {
	id:   u64,
	repo: Option<GithubRepo>,
}
#[derive(Clone)]
enum WatchTarget {
	Run(u64),
	Commit { branch: Str, head_sha: Str },
}
#[derive(Clone, Copy, Eq, PartialEq, strum::IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
enum ActionsState {
	Pending,
	Success,
	Failure,
}
pub(crate) struct ApiResponse {
	pub(crate) value: Value,
	remaining:        Option<u64>,
	reset:            Option<u64>,
	artifact:         Option<Artifact>,
}
struct FailedLogs {
	rows:     Vec<Value>,
	artifact: Option<Artifact>,
}
fn forge_worktree_fault(error: crate::forge_worktree::Error) -> Fault {
	match error {
		crate::forge_worktree::Error::Cancelled => cancelled_fault(),
		crate::forge_worktree::Error::CheckoutMissing => {
			fault("github_checkout_missing", "pull request checkout metadata is missing")
		},
		crate::forge_worktree::Error::IncompletePlan => {
			fault("github_invalid_response", "pull request checkout plan is incomplete")
		},
		crate::forge_worktree::Error::GitFailed => {
			fault("github_git_failed", "forge worktree git operation failed")
		},
	}
}

/// Evidence that `pr_create` published its head branch first.
#[derive(Debug)]
struct PushReceipt {
	/// Pushed branch name.
	branch:   String,
	/// Remote name or URL that received the push.
	remote:   String,
	/// Commit now at the remote branch head.
	sha:      String,
	/// Remote head the push fast-forwarded from; `None` when it was created.
	replaced: Option<String>,
}

/// Records a head-branch push on the created pull request's result so both
/// the transcript card and the model see that `pr_create` pushed.
fn attach_push_receipt(value: &mut Value, receipt: Option<PushReceipt>) {
	let (Some(receipt), Some(object)) = (receipt, value.as_object_mut()) else {
		return;
	};
	object.insert(
		"omp_pushed_head".to_owned(),
		json!({
			"branch": receipt.branch,
			"remote": receipt.remote,
			"sha": receipt.sha,
			"replaced": receipt.replaced,
		}),
	);
}

/// Resolves a local branch's commit and configured upstream remote name.
///
/// `Ok(None)` when the branch does not exist locally, which leaves `pr_create`
/// to open the pull request from a remote-only branch as before.
fn local_branch_target(
	root: &Path,
	branch: &str,
) -> Result<Option<(String, Option<String>)>, Fault> {
	let repo = GitRepo::require(root).map_err(git_fault)?;
	let Some(sha) = repo
		.resolve_ref(&format!("refs/heads/{branch}"))
		.map_err(git_fault)?
	else {
		return Ok(None);
	};
	let upstream = repo
		.config_get(&format!("branch.{branch}.remote"))
		.map_err(git_fault)?
		.filter(|remote| remote != ".");
	Ok(Some((sha, upstream)))
}

/// Picks where a head branch is pushed: the branch's upstream remote when it
/// points at `repo`, else the first configured remote that does, else the
/// repository's HTTPS URL (authenticated by the user's credential helpers).
fn push_target(local: &GitRepo, upstream: Option<String>, repo: &GithubRepo) -> String {
	let maps_to_repo = |name: &str| {
		local
			.remote_url(name)
			.ok()
			.flatten()
			.and_then(|url| github_url::repo_from_remote(&url).ok())
			.is_some_and(|remote| remote.identity().eq_ignore_ascii_case(repo.identity()))
	};
	upstream
		.filter(|name| maps_to_repo(name))
		.or_else(|| {
			local
				.remote_list()
				.ok()?
				.into_iter()
				.find(|name| maps_to_repo(name))
		})
		.unwrap_or_else(|| format!("https://{}/{}.git", repo.host(), repo.slug()))
}

fn current_git_snapshot(root: &Path) -> Result<(Str, Str), Fault> {
	let repo = GitRepo::require(root).map_err(git_fault)?;
	let branch = repo.current_branch().map_err(git_fault)?.ok_or_else(|| {
		fault("github_git_failed", "current checkout is detached; pass `branch` or `run`")
	})?;
	if branch.is_empty() {
		return Err(fault(
			"github_git_failed",
			"current checkout is detached; pass `branch` or `run`",
		));
	}
	let head_sha = repo
		.head_sha()
		.map_err(git_fault)?
		.ok_or_else(|| git_fault(omp_vcs::Error::RefNotFound { name: "HEAD".to_owned() }))?;
	Ok((Str::new(branch), Str::new(head_sha)))
}
fn has_scope(query: &str) -> bool {
	query.split_whitespace().any(|part| {
		let part = part.strip_prefix('-').unwrap_or(part);
		["repo:", "org:", "user:", "owner:"].iter().any(|prefix| {
			part
				.get(..prefix.len())
				.is_some_and(|value| value.eq_ignore_ascii_case(prefix))
		})
	})
}
fn parse_pr_number(value: &str, repo: &GithubRepo) -> Result<Option<u64>, Fault> {
	let value = value.trim();
	if let Ok(number) = value.parse::<u64>() {
		return if number > 0 {
			Ok(Some(number))
		} else {
			Err(fault("github_invalid_pr", "pull request number must be positive"))
		};
	}
	let Some((url_repo, number)) = parse_pr_url(value)? else {
		return Ok(None);
	};
	if !url_repo.identity().eq_ignore_ascii_case(repo.identity()) {
		return Err(fault("github_invalid_pr", "pull request URL belongs to a different repository"));
	}
	Ok(Some(number))
}

fn parse_pr_url(value: &str) -> Result<Option<(GithubRepo, u64)>, Fault> {
	if !value.contains("://") {
		return Ok(None);
	}
	let parsed = url::Url::parse(value)
		.map_err(|_| fault("github_invalid_pr", "pull request URL is not a valid GitHub URL"))?;
	let host = parsed_url_host(&parsed);
	let parts = parsed
		.path_segments()
		.map(|parts| parts.collect::<Vec<_>>())
		.unwrap_or_default();
	let (repo_host, owner, name, number) = match (host.as_str(), parts.as_slice()) {
		("api.github.com", ["repos", owner, name, "pulls", number, ..]) => {
			("github.com", *owner, *name, *number)
		},
		(host, ["api", "v3", "repos", owner, name, "pulls", number, ..]) => {
			(host, *owner, *name, *number)
		},
		(host, [owner, name, "pull", number, ..]) if !host.is_empty() => {
			(host, *owner, *name, *number)
		},
		_ => {
			return Err(fault(
				"github_invalid_pr",
				"pull request URL must be a GitHub pull request URL",
			));
		},
	};
	let repo = GithubRepo::new(repo_host, owner, name)
		.map_err(|_| fault("github_invalid_pr", "pull request URL has an invalid repository"))?;
	let number = number
		.parse::<u64>()
		.ok()
		.filter(|number| *number > 0)
		.ok_or_else(|| fault("github_invalid_pr", "pull request URL has no positive number"))?;
	Ok(Some((repo, number)))
}
fn parsed_url_host(parsed: &url::Url) -> String {
	let host = parsed.host_str().unwrap_or_default();
	parsed
		.port()
		.map_or_else(|| host.to_owned(), |port| format!("{host}:{port}"))
}

fn parse_run_reference(value: &str) -> Result<RunReference, Fault> {
	let value = value.trim();
	if let Ok(id) = value.parse::<u64>() {
		if id == 0 {
			return Err(fault("github_invalid_run", "Actions run id must be positive"));
		}
		return Ok(RunReference { id, repo: None });
	}
	let parsed = url::Url::parse(value)
		.map_err(|_| fault("github_invalid_run", "Actions run must be an id or GitHub run URL"))?;
	let host = parsed_url_host(&parsed);
	let parts = parsed
		.path_segments()
		.map(|parts| parts.collect::<Vec<_>>())
		.unwrap_or_default();
	let (repo_host, owner, name, id) = match (host.as_str(), parts.as_slice()) {
		("api.github.com", ["repos", owner, name, "actions", "runs", id, ..]) => {
			("github.com", *owner, *name, *id)
		},
		(host, ["api", "v3", "repos", owner, name, "actions", "runs", id, ..]) => {
			(host, *owner, *name, *id)
		},
		(host, [owner, name, "actions", "runs", id, ..]) if !host.is_empty() => {
			(host, *owner, *name, *id)
		},
		_ => {
			return Err(fault(
				"github_invalid_run",
				"Actions run URL must identify a GitHub Actions run",
			));
		},
	};
	let id = id
		.parse::<u64>()
		.ok()
		.filter(|id| *id != 0)
		.ok_or_else(|| fault("github_invalid_run", "Actions run URL has no positive id"))?;
	let repo = GithubRepo::new(repo_host, owner, name)
		.map_err(|_| fault("github_invalid_run", "Actions run URL has an invalid repository"))?;
	Ok(RunReference { id, repo: Some(repo) })
}
fn actions_state(value: &Value) -> ActionsState {
	if let Some(run) = value.get("run") {
		return run_state(run, value.get("jobs").and_then(Value::as_array));
	}
	let Some(runs) = value.get("workflow_runs").and_then(Value::as_array) else {
		return ActionsState::Pending;
	};
	if runs.is_empty() {
		return ActionsState::Pending;
	}
	let mut pending = false;
	for run in runs {
		match run_state(run, run.get("jobs").and_then(Value::as_array)) {
			ActionsState::Failure => return ActionsState::Failure,
			ActionsState::Pending => pending = true,
			ActionsState::Success => {},
		}
	}
	if pending {
		ActionsState::Pending
	} else {
		ActionsState::Success
	}
}
fn run_state(run: &Value, jobs: Option<&Vec<Value>>) -> ActionsState {
	if let Some(jobs) = jobs {
		let mut pending = false;
		for job in jobs {
			if job.get("status").and_then(Value::as_str) != Some("completed") {
				pending = true;
				continue;
			}
			if conclusion_state(job.get("conclusion").and_then(Value::as_str)) == ActionsState::Failure
			{
				return ActionsState::Failure;
			}
		}
		if pending {
			return ActionsState::Pending;
		}
	}
	if run.get("status").and_then(Value::as_str) != Some("completed") {
		return ActionsState::Pending;
	}
	conclusion_state(run.get("conclusion").and_then(Value::as_str))
}
fn conclusion_state(conclusion: Option<&str>) -> ActionsState {
	match conclusion {
		Some("success" | "neutral" | "skipped") => ActionsState::Success,
		Some("failure" | "timed_out" | "cancelled" | "action_required" | "startup_failure") => {
			ActionsState::Failure
		},
		_ => ActionsState::Pending,
	}
}
fn file_endpoint(repo: &str, path: &str, branch: Option<&str>) -> Result<String, Fault> {
	if path.is_empty()
		|| path.starts_with('/')
		|| path
			.split('/')
			.any(|segment| segment.is_empty() || matches!(segment, "." | ".."))
	{
		return Err(fault(
			"github_invalid_path",
			"file path must be a nonempty repository-relative file path",
		));
	}
	let encoded = path
		.split('/')
		.map(encode_path_segment)
		.collect::<Vec<_>>()
		.join("/");
	let mut endpoint = format!("/repos/{repo}/contents/{encoded}");
	if let Some(branch) = branch {
		endpoint.push_str("?ref=");
		endpoint.push_str(&encode_query(branch));
	}
	Ok(endpoint)
}
fn decode_file_response(
	value: &Value,
	repo: &str,
	path: &str,
	branch: Option<&str>,
	blobs: &BlobHost,
) -> Result<(Value, Option<Artifact>), Fault> {
	if value.get("type").and_then(Value::as_str) != Some("file") {
		return Err(fault("github_invalid_content", "GitHub Contents response was not a file"));
	}
	if value.get("encoding").and_then(Value::as_str) != Some("base64") {
		return Err(fault(
			"github_invalid_content",
			"GitHub file content uses an unsupported encoding",
		));
	}
	let encoded = value
		.get("content")
		.and_then(Value::as_str)
		.ok_or_else(|| fault("github_invalid_content", "GitHub file response has no content"))?;
	let compact = encoded
		.bytes()
		.filter(|byte| !byte.is_ascii_whitespace())
		.collect::<Vec<_>>();
	let decoded = omp_core::base64::decode(&compact)
		.into_vec()
		.map_err(|_| fault("github_invalid_content", "GitHub file content was not valid base64"))?;
	let source_url = value.get("html_url").and_then(Value::as_str);
	let prefix = &decoded[..decoded.len().min(omp_tools::read::BINARY_SNIFF_BYTES)];
	let binary = omp_tools::read::is_probably_binary_header(prefix);
	match std::str::from_utf8(&decoded) {
		Ok(content) if !binary => Ok((
			json!({
				"repo": repo,
				"path": path,
				"branch": branch,
				"content": content,
				"size": decoded.len(),
				"source_url": source_url,
				"binary": false,
			}),
			None,
		)),
		_ => {
			let id = blobs.put(&decoded).map_err(|_| {
				fault("github_artifact_failed", "GitHub file artifact could not be retained")
			})?;
			let reference = BlobRef::from(id);
			let image = omp_tools::read::image::sniff_metadata(&decoded);
			let artifact = Artifact {
				uri:        Str::new(format!("artifact://sha256/{}", reference.to_hex().as_str())),
				size:       id.size,
				media_type: image.map_or_else(
					|| sf!("application/octet-stream"),
					|metadata| Str::new_static(metadata.kind.media_type()),
				),
			};
			Ok((
				json!({
					"repo": repo,
					"path": path,
					"branch": branch,
					"content": Value::Null,
					"size": decoded.len(),
					"source_url": source_url,
					"binary": true,
					"artifact": artifact.uri.as_str(),
					"media_type": artifact.media_type.as_str(),
					"width": image.and_then(|metadata| metadata.width),
					"height": image.and_then(|metadata| metadata.height),
				}),
				Some(artifact),
			))
		},
	}
}
fn pr_branch_endpoint(repo: &str, head: &str) -> String {
	format!("/repos/{repo}/pulls?state=open&head={}&per_page=100", encode_query(head),)
}
fn pr_list_endpoint(repo: &str, page: u32) -> String {
	format!("/repos/{repo}/pulls?state=open&per_page=100&page={page}")
}
fn branch_endpoint(repo: &str, branch: &str) -> String {
	format!("/repos/{repo}/branches/{}", encode_path_segment(branch))
}
fn actions_runs_endpoint(repo: &str, head_sha: &str) -> String {
	format!("/repos/{repo}/actions/runs?head_sha={}&per_page=100", encode_query(head_sha),)
}
async fn poll_sleep(delay: Duration, cancellation: &CancellationToken) -> Result<(), Fault> {
	tokio::select! {
		() = time::sleep(delay) => Ok(()),
		() = cancellation.cancelled() => Err(cancelled_fault()),
	}
}
fn run_jobs_endpoint(repo: &str, run_id: u64, page: u32) -> String {
	format!("/repos/{repo}/actions/runs/{run_id}/jobs?per_page=100&page={page}")
}
fn compare_endpoint(repo: &str, base: &str, head: &str) -> String {
	format!("/repos/{repo}/compare/{}...{}", encode_path_segment(base), encode_path_segment(head))
}
/// Reads a response body under the 16 MiB ceiling, observing cancellation
/// between chunks.
async fn read_body(
	response: reqwest::Response,
	cancellation: &CancellationToken,
) -> Result<BytesMut, Fault> {
	let mut bytes = BytesMut::new();
	let mut stream = response.bytes_stream();
	while let Some(chunk) = tokio::select! {
		chunk = stream.next() => chunk,
		() = cancellation.cancelled() => return Err(cancelled_fault()),
	} {
		let chunk = chunk.map_err(http_fault)?;
		if bytes.len().saturating_add(chunk.len()) > MAX_BODY {
			return Err(fault("github_response_too_large", "GitHub response exceeds 16 MiB"));
		}
		bytes.extend_from_slice(&chunk);
	}
	Ok(bytes)
}
/// Derives a pull request title and body from the commits between base and
/// head, matching `gh pr create --fill`: a single commit contributes its
/// subject and body; several commits title the PR after the humanized head
/// branch and list every subject oldest-first.
fn fill_from_commits(head: &str, messages: &[&str]) -> Result<(String, String), Fault> {
	match messages {
		[] => Err(fault(
			"github_invalid_request",
			"fill requires at least one commit between base and head",
		)),
		[message] => {
			let (subject, body) = message.split_once('\n').unwrap_or((message, ""));
			Ok((subject.trim().to_owned(), body.trim().to_owned()))
		},
		_ => {
			let mut title = String::with_capacity(head.len());
			for (index, ch) in head.chars().enumerate() {
				match ch {
					'-' | '_' => title.push(' '),
					_ if index == 0 => title.extend(ch.to_uppercase()),
					_ => title.push(ch),
				}
			}
			let mut body = String::new();
			for message in messages {
				body.push_str("- ");
				body.push_str(message.lines().next().unwrap_or("").trim());
				body.push('\n');
			}
			Ok((title, body))
		},
	}
}
/// Post-creation pull request metadata applied through the issues and
/// reviewers endpoints.
#[derive(Debug, Default, Eq, PartialEq)]
struct PrMetadata {
	reviewers:      Vec<String>,
	team_reviewers: Vec<String>,
	assignees:      Vec<String>,
	labels:         Vec<String>,
}
impl PrMetadata {
	fn from_params(params: &Params) -> Self {
		let mut metadata = Self::default();
		for reviewer in normalized_identifiers(&params.reviewer) {
			match reviewer.rsplit_once('/') {
				Some((_, team)) => metadata.team_reviewers.push(team.to_owned()),
				None => metadata.reviewers.push(reviewer),
			}
		}
		metadata.assignees = normalized_identifiers(&params.assignee);
		metadata.labels = normalized_identifiers(&params.label);
		metadata
	}

	fn is_empty(&self) -> bool {
		self.reviewers.is_empty()
			&& self.team_reviewers.is_empty()
			&& self.assignees.is_empty()
			&& self.labels.is_empty()
	}

	/// Endpoint/body pairs to POST, in application order.
	fn requests(&self, repo: &str, number: u64) -> Vec<(String, Value)> {
		let mut requests = Vec::with_capacity(3);
		if !self.reviewers.is_empty() || !self.team_reviewers.is_empty() {
			requests.push((
				format!("/repos/{repo}/pulls/{number}/requested_reviewers"),
				json!({ "reviewers": self.reviewers, "team_reviewers": self.team_reviewers }),
			));
		}
		if !self.assignees.is_empty() {
			requests.push((
				format!("/repos/{repo}/issues/{number}/assignees"),
				json!({ "assignees": self.assignees }),
			));
		}
		if !self.labels.is_empty() {
			requests.push((
				format!("/repos/{repo}/issues/{number}/labels"),
				json!({ "labels": self.labels }),
			));
		}
		requests
	}
}
/// Trims, drops empty entries, and de-duplicates while preserving order.
fn normalized_identifiers(values: &[Str]) -> Vec<String> {
	let mut output: Vec<String> = Vec::with_capacity(values.len());
	for value in values {
		let trimmed = value.trim();
		if !trimmed.is_empty() && !output.iter().any(|seen| seen == trimmed) {
			output.push(trimmed.to_string());
		}
	}
	output
}
/// Resolves the per-job log tail: 15 lines by default, capped at 200, and
/// never zero.
fn tail_limit(requested: Option<u32>) -> Result<usize, Fault> {
	match requested {
		None => Ok(RUN_WATCH_TAIL_DEFAULT),
		Some(0) => Err(fault("github_invalid_request", "tail must be a positive number")),
		Some(lines) => Ok(usize::try_from(lines)
			.unwrap_or(usize::MAX)
			.min(RUN_WATCH_TAIL_MAX)),
	}
}
/// Returns the last `tail` non-trailing lines of a log, or `None` when the
/// log is blank.
fn tail_lines(log: &str, tail: usize) -> Option<String> {
	let normalized = log.replace("\r\n", "\n");
	let trimmed = normalized.trim();
	if trimmed.is_empty() {
		return None;
	}
	let lines = trimmed.lines().collect::<Vec<_>>();
	let start = lines.len().saturating_sub(tail);
	Some(lines[start..].join("\n").trim_end().to_owned())
}
/// Lists `(run_id, job)` pairs whose completed conclusion is a failure.
fn failed_jobs(value: &Value) -> Vec<(u64, &Value)> {
	fn push_failed<'v>(run: &'v Value, jobs: Option<&'v Value>, out: &mut Vec<(u64, &'v Value)>) {
		let Some(run_id) = run.get("id").and_then(Value::as_u64) else {
			return;
		};
		for job in jobs
			.and_then(Value::as_array)
			.map(Vec::as_slice)
			.unwrap_or_default()
		{
			let completed = job.get("status").and_then(Value::as_str) == Some("completed");
			let failed = matches!(
				job.get("conclusion").and_then(Value::as_str),
				Some("failure" | "timed_out" | "cancelled" | "action_required")
			);
			if completed && failed {
				out.push((run_id, job));
			}
		}
	}
	let mut failed = Vec::new();
	if let Some(run) = value.get("run") {
		push_failed(run, value.get("jobs"), &mut failed);
	} else if let Some(runs) = value.get("workflow_runs").and_then(Value::as_array) {
		for run in runs {
			push_failed(run, run.get("jobs"), &mut failed);
		}
	}
	failed
}
fn encode_path_segment(value: &str) -> String {
	url::form_urlencoded::byte_serialize(value.as_bytes())
		.collect::<String>()
		.replace('+', "%20")
}
fn encode_query(value: &str) -> String {
	url::form_urlencoded::byte_serialize(value.as_bytes()).collect()
}
fn append_query_part(query: &mut String, part: &str) {
	if !query.is_empty() {
		query.push(' ');
	}
	query.push_str(part);
}
fn search_date_qualifier(params: &Params, now: SystemTime) -> Result<Option<String>, Fault> {
	date_qualifier(
		params.op,
		params.date_field,
		params.since.as_deref(),
		params.until.as_deref(),
		now,
	)
}
fn date_qualifier(
	operation: Operation,
	requested: Option<DateField>,
	since: Option<&str>,
	until: Option<&str>,
	now: SystemTime,
) -> Result<Option<String>, Fault> {
	let field = match operation {
		Operation::SearchCommits => "committer-date",
		Operation::SearchRepos if requested == Some(DateField::Updated) => "pushed",
		_ if requested == Some(DateField::Updated) => "updated",
		_ => "created",
	};
	let since = since
		.map(|value| normalize_date_bound(value, now))
		.transpose()?;
	let until = until
		.map(|value| normalize_date_bound(value, now))
		.transpose()?;
	Ok(match (since, until) {
		(Some(since), Some(until)) => Some(format!("{field}:{since}..{until}")),
		(Some(since), None) => Some(format!("{field}:>={since}")),
		(None, Some(until)) => Some(format!("{field}:<={until}")),
		(None, None) => None,
	})
}
fn normalize_date_bound(raw: &str, now: SystemTime) -> Result<String, Fault> {
	let value = raw.trim();
	if value.is_empty() {
		return Err(fault("github_invalid_search", "date bound must not be empty"));
	}
	let digit_count = value
		.bytes()
		.take_while(|byte| byte.is_ascii_digit())
		.count();
	if digit_count > 0 {
		let count = value[..digit_count]
			.parse::<u64>()
			.map_err(|_| fault("github_invalid_search", "relative date bound is too large"))?;
		let unit = value[digit_count..].trim().to_ascii_lowercase();
		if matches!(unit.as_str(), "m" | "h" | "d" | "w") {
			let unit_seconds = match unit.as_str() {
				"m" => 60,
				"h" => 3_600,
				"d" => 86_400,
				"w" => 604_800,
				_ => unreachable!(),
			};
			let delta = count
				.checked_mul(unit_seconds)
				.ok_or_else(|| fault("github_invalid_search", "relative date bound is too large"))?;
			let now = epoch_seconds(now)?;
			let bound = now
				.checked_sub(
					i64::try_from(delta).map_err(|_| {
						fault("github_invalid_search", "relative date bound is too large")
					})?,
				)
				.ok_or_else(|| fault("github_invalid_search", "relative date bound is too large"))?;
			return format_epoch_date(bound);
		}
		if matches!(unit.as_str(), "mo" | "y") {
			let now = epoch_seconds(now)?;
			let (year, month, day) = civil_from_days(now.div_euclid(86_400));
			let count = i64::try_from(count)
				.map_err(|_| fault("github_invalid_search", "relative date bound is too large"))?;
			let target_days = if unit == "mo" {
				let month_index = i64::from(year)
					.checked_mul(12)
					.and_then(|value| value.checked_add(i64::from(month) - 1))
					.and_then(|value| value.checked_sub(count))
					.ok_or_else(|| fault("github_invalid_search", "relative date bound is too large"))?;
				let target_year = month_index.div_euclid(12);
				let target_month =
					u32::try_from(month_index.rem_euclid(12) + 1).expect("month is in range");
				days_from_civil(target_year, target_month, 1)
					.and_then(|value| value.checked_add(i64::from(day) - 1))
			} else {
				let target_year = i64::from(year)
					.checked_sub(count)
					.ok_or_else(|| fault("github_invalid_search", "relative date bound is too large"))?;
				days_from_civil(target_year, month, 1)
					.and_then(|value| value.checked_add(i64::from(day) - 1))
			}
			.ok_or_else(|| fault("github_invalid_search", "relative date bound is too large"))?;
			let (year, month, day) = civil_from_days(target_days);
			return format_date(year, month, day);
		}
	}
	if is_iso_date(value) {
		let (year, month, day) = parse_date(value)?;
		return format_date(year, month, day);
	}
	parse_iso_datetime(value)
}
fn epoch_seconds(time: SystemTime) -> Result<i64, Fault> {
	let seconds = time.duration_since(UNIX_EPOCH).map_err(|_| {
		fault("github_invalid_search", "date normalization time precedes the Unix epoch")
	})?;
	i64::try_from(seconds.as_secs())
		.map_err(|_| fault("github_invalid_search", "date normalization time is out of range"))
}
fn is_iso_date(value: &str) -> bool {
	value.len() == 10
		&& value.as_bytes()[4] == b'-'
		&& value.as_bytes()[7] == b'-'
		&& value
			.bytes()
			.enumerate()
			.all(|(index, byte)| matches!(index, 4 | 7) || byte.is_ascii_digit())
}
fn parse_date(value: &str) -> Result<(i32, u32, u32), Fault> {
	if !is_iso_date(value) {
		return Err(invalid_date_bound());
	}
	let year = value[0..4]
		.parse::<i32>()
		.map_err(|_| invalid_date_bound())?;
	let month = value[5..7]
		.parse::<u32>()
		.map_err(|_| invalid_date_bound())?;
	let day = value[8..10]
		.parse::<u32>()
		.map_err(|_| invalid_date_bound())?;
	if year == 0 || day == 0 || day > days_in_month(year, month).unwrap_or(0) {
		return Err(invalid_date_bound());
	}
	Ok((year, month, day))
}
fn parse_iso_datetime(value: &str) -> Result<String, Fault> {
	if value.len() < 20 || value.as_bytes().get(10).copied() != Some(b'T') {
		return Err(invalid_date_bound());
	}
	let date = value.get(..10).ok_or_else(invalid_date_bound)?;
	let (year, month, day) = parse_date(date)?;
	let remainder = &value[11..];
	let (clock, offset_seconds) = if let Some(clock) = remainder
		.strip_suffix('Z')
		.or_else(|| remainder.strip_suffix('z'))
	{
		(clock, 0i64)
	} else {
		let split = remainder
			.rfind(|character| matches!(character, '+' | '-'))
			.ok_or_else(invalid_date_bound)?;
		let (clock, offset) = remainder.split_at(split);
		if offset.len() != 6 || offset.as_bytes()[3] != b':' {
			return Err(invalid_date_bound());
		}
		if !offset.as_bytes()[1..3]
			.iter()
			.chain(&offset.as_bytes()[4..6])
			.all(|byte| byte.is_ascii_digit())
		{
			return Err(invalid_date_bound());
		}
		let hours = offset[1..3]
			.parse::<i64>()
			.map_err(|_| invalid_date_bound())?;
		let minutes = offset[4..6]
			.parse::<i64>()
			.map_err(|_| invalid_date_bound())?;
		if hours > 23 || minutes > 59 {
			return Err(invalid_date_bound());
		}
		let seconds = hours * 3_600 + minutes * 60;
		(
			clock,
			if offset.starts_with('-') {
				-seconds
			} else {
				seconds
			},
		)
	};
	let mut clock_parts = clock.split(':');
	let hour = clock_parts
		.next()
		.and_then(|value| value.parse::<i64>().ok())
		.ok_or_else(invalid_date_bound)?;
	let minute = clock_parts
		.next()
		.and_then(|value| value.parse::<i64>().ok())
		.ok_or_else(invalid_date_bound)?;
	let seconds = clock_parts.next().ok_or_else(invalid_date_bound)?;
	if clock_parts.next().is_some() {
		return Err(invalid_date_bound());
	}
	let mut second_parts = seconds.split('.');
	let second = second_parts
		.next()
		.and_then(|value| value.parse::<i64>().ok())
		.ok_or_else(invalid_date_bound)?;
	if let Some(fraction) = second_parts.next() {
		if fraction.is_empty()
			|| !fraction.bytes().all(|byte| byte.is_ascii_digit())
			|| second_parts.next().is_some()
		{
			return Err(invalid_date_bound());
		}
	}
	if hour > 23 || minute > 59 || second > 59 {
		return Err(invalid_date_bound());
	}
	let days = days_from_civil(i64::from(year), month, day).ok_or_else(invalid_date_bound)?;
	let timestamp = days
		.checked_mul(86_400)
		.and_then(|value| value.checked_add(hour * 3_600 + minute * 60 + second))
		.and_then(|value| value.checked_sub(offset_seconds))
		.ok_or_else(invalid_date_bound)?;
	format_epoch_datetime(timestamp)
}
fn invalid_date_bound() -> Fault {
	fault(
		"github_invalid_search",
		"invalid date bound; expected a relative duration, ISO date, or ISO datetime",
	)
}
fn days_in_month(year: i32, month: u32) -> Option<u32> {
	Some(match month {
		1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
		4 | 6 | 9 | 11 => 30,
		2 if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) => 29,
		2 => 28,
		_ => return None,
	})
}
fn days_from_civil(year: i64, month: u32, day: u32) -> Option<i64> {
	if !(1..=12).contains(&month) || day == 0 {
		return None;
	}
	let year = year - if month <= 2 { 1 } else { 0 };
	let era = if year >= 0 { year } else { year - 399 } / 400;
	let year_of_era = year - era * 400;
	let month = i64::from(month);
	let day_of_year = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + i64::from(day) - 1;
	let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
	era.checked_mul(146_097)?
		.checked_add(day_of_era)?
		.checked_sub(719_468)
}
fn civil_from_days(days: i64) -> (i32, u32, u32) {
	let days = days + 719_468;
	let era = if days >= 0 { days } else { days - 146_096 } / 146_097;
	let day_of_era = days - era * 146_097;
	let year_of_era =
		(day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
	let mut year = year_of_era + era * 400;
	let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
	let month_prime = (5 * day_of_year + 2) / 153;
	let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
	let month = month_prime + if month_prime < 10 { 3 } else { -9 };
	year += if month <= 2 { 1 } else { 0 };
	(
		i32::try_from(year).unwrap_or(if year < 0 { i32::MIN } else { i32::MAX }),
		u32::try_from(month).expect("month is positive"),
		u32::try_from(day).expect("day is positive"),
	)
}
fn format_date(year: i32, month: u32, day: u32) -> Result<String, Fault> {
	if !(1..=9_999).contains(&year) {
		return Err(invalid_date_bound());
	}
	Ok(format!("{year:04}-{month:02}-{day:02}"))
}
fn format_epoch_date(seconds: i64) -> Result<String, Fault> {
	let (year, month, day) = civil_from_days(seconds.div_euclid(86_400));
	format_date(year, month, day)
}
fn format_epoch_datetime(seconds: i64) -> Result<String, Fault> {
	let days = seconds.div_euclid(86_400);
	let within_day = seconds.rem_euclid(86_400);
	let (year, month, day) = civil_from_days(days);
	if !(1..=9_999).contains(&year) {
		return Err(invalid_date_bound());
	}
	let hour = within_day / 3_600;
	let minute = within_day % 3_600 / 60;
	let second = within_day % 60;
	Ok(format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z"))
}

fn actions_signature(value: &Value) -> String {
	let runs = value
		.get("workflow_runs")
		.and_then(Value::as_array)
		.cloned()
		.unwrap_or_default();
	serde_json::to_string(&runs).unwrap_or_default()
}

fn operation_is_useless(op: Operation, value: &Value) -> bool {
	match op {
		Operation::SearchIssues
		| Operation::SearchPrs
		| Operation::SearchCode
		| Operation::SearchCommits
		| Operation::SearchRepos => value
			.get("items")
			.and_then(Value::as_array)
			.is_none_or(Vec::is_empty),
		Operation::RunWatch => value.get("outcome").and_then(Value::as_str) == Some("no_runs"),
		_ => false,
	}
}

fn render_output(op: Operation, value: &Value, artifact: Option<&Artifact>) -> String {
	let mut output = match op {
		Operation::RepoView => render_repo(value),
		Operation::FileRead => render_file(value),
		Operation::PrCreate => render_created_pr(value),
		Operation::PrCheckout => render_checkout(value),
		Operation::PrPush => render_push(value),
		Operation::SearchIssues
		| Operation::SearchPrs
		| Operation::SearchCode
		| Operation::SearchCommits
		| Operation::SearchRepos => render_search(op, value),
		Operation::RunWatch => render_actions(value),
	};
	if let Some(artifact) = artifact {
		let _ = write!(
			output,
			"\n\nComplete output: {} ({} bytes, {})",
			artifact.uri, artifact.size, artifact.media_type
		);
	}
	output
}

fn render_repo(value: &Value) -> String {
	let name = value
		.get("full_name")
		.and_then(Value::as_str)
		.unwrap_or("GitHub Repository");
	let mut output = format!("# {name}");
	if let Some(description) = value.get("description").and_then(Value::as_str)
		&& !description.trim().is_empty()
	{
		let _ = write!(output, "\n\n{}", description.trim());
	}
	for (label, pointer) in [
		("URL", "/html_url"),
		("Default branch", "/default_branch"),
		("Branch", "/requested_branch"),
		("Visibility", "/visibility"),
		("Language", "/language"),
		("Updated", "/updated_at"),
		("Homepage", "/homepage"),
	] {
		if let Some(text) = value.pointer(pointer).and_then(Value::as_str)
			&& !text.is_empty()
		{
			let _ = write!(output, "\n{label}: {text}");
		}
	}
	for (label, pointer) in [
		("Stars", "/stargazers_count"),
		("Forks", "/forks_count"),
		("Open issues", "/open_issues_count"),
	] {
		if let Some(number) = value.pointer(pointer).and_then(Value::as_u64) {
			let _ = write!(output, "\n{label}: {number}");
		}
	}
	for (label, pointer) in [("Archived", "/archived"), ("Fork", "/fork")] {
		if let Some(flag) = value.pointer(pointer).and_then(Value::as_bool) {
			let _ = write!(output, "\n{label}: {flag}");
		}
	}
	if let Some(topics) = value.get("topics").and_then(Value::as_array) {
		let topics = topics
			.iter()
			.filter_map(Value::as_str)
			.collect::<Vec<_>>()
			.join(", ");
		if !topics.is_empty() {
			let _ = write!(output, "\nTopics: {topics}");
		}
	}
	output
}

fn render_file(value: &Value) -> String {
	let path = value
		.get("path")
		.and_then(Value::as_str)
		.unwrap_or("(unknown path)");
	if value.get("binary").and_then(Value::as_bool) == Some(true) {
		let size = value
			.get("size")
			.and_then(Value::as_u64)
			.unwrap_or_default();
		if let Some(media_type) = value
			.get("media_type")
			.and_then(Value::as_str)
			.filter(|media_type| media_type.starts_with("image/"))
		{
			let mut output = format!("Image file: {path}\nMIME: {media_type}\nSize: {size} bytes");
			if let (Some(width), Some(height)) = (
				value.get("width").and_then(Value::as_u64),
				value.get("height").and_then(Value::as_u64),
			) {
				let _ = write!(output, "\nDimensions: {width}x{height}");
			}
			return output;
		}
		return format!("[Cannot inline binary GitHub file '{path}' ({size} bytes).]");
	}
	value
		.get("content")
		.and_then(Value::as_str)
		.unwrap_or_default()
		.to_owned()
}

fn render_created_pr(value: &Value) -> String {
	let number = value.get("number").and_then(Value::as_u64);
	let title = value
		.get("title")
		.and_then(Value::as_str)
		.unwrap_or("Untitled");
	let mut output = match number {
		Some(number) => format!("# Pull Request #{number}: {title}"),
		None => format!("# Pull Request: {title}"),
	};
	if let Some(url) = value.get("html_url").and_then(Value::as_str) {
		let _ = write!(output, "\nURL: {url}");
	}
	if let (Some(head), Some(base)) = (
		value.pointer("/head/ref").and_then(Value::as_str),
		value.pointer("/base/ref").and_then(Value::as_str),
	) {
		let _ = write!(output, "\nBranch: {head} -> {base}");
	}
	if let Some(pushed) = value.get("omp_pushed_head") {
		let branch = pushed
			.get("branch")
			.and_then(Value::as_str)
			.unwrap_or("head");
		let remote = pushed
			.get("remote")
			.and_then(Value::as_str)
			.unwrap_or("remote");
		let sha = pushed
			.get("sha")
			.and_then(Value::as_str)
			.unwrap_or_default();
		let short = sha.get(..12).unwrap_or(sha);
		let verb = if pushed.get("replaced").is_some_and(|v| !v.is_null()) {
			"Fast-forwarded"
		} else {
			"Published"
		};
		let _ = write!(output, "\n{verb} {branch} at {short} to {remote} before opening");
	}
	output
}

fn render_checkout(value: &Value) -> String {
	let rows = value
		.get("checkouts")
		.and_then(Value::as_array)
		.map(Vec::as_slice)
		.unwrap_or_default();
	let mut output = format!("# {} Pull Request Worktree(s)", rows.len());
	for row in rows {
		let number = row.get("pr").and_then(Value::as_u64).unwrap_or_default();
		let branch = row
			.get("branch")
			.and_then(Value::as_str)
			.unwrap_or("unknown");
		let path = row.get("path").and_then(Value::as_str).unwrap_or("unknown");
		let reused = row.get("reused").and_then(Value::as_bool).unwrap_or(false);
		let _ = write!(
			output,
			"\n- #{number} {branch}{}\n  Path: {path}",
			if reused { " (reused)" } else { "" }
		);
		if let Some(remote) = row.get("remote_branch").and_then(Value::as_str) {
			let _ = write!(output, "\n  Remote branch: {remote}");
		}
	}
	render_batch_failures(&mut output, value);
	output
}

fn render_push(value: &Value) -> String {
	let rows = value
		.get("pushed")
		.and_then(Value::as_array)
		.map(Vec::as_slice)
		.unwrap_or_default();
	let mut output = format!("# {} Pull Request Branch(es) pushed", rows.len());
	for row in rows {
		let number = row.get("pr").and_then(Value::as_u64).unwrap_or_default();
		let branch = row
			.get("remote_branch")
			.and_then(Value::as_str)
			.unwrap_or("unknown");
		let force = row
			.get("force_with_lease")
			.and_then(Value::as_bool)
			.unwrap_or(false);
		let _ = write!(
			output,
			"\n- #{number} -> {branch}{}",
			if force { " (force-with-lease)" } else { "" }
		);
	}
	render_batch_failures(&mut output, value);
	output
}

fn render_batch_failures(output: &mut String, value: &Value) {
	let failures = value
		.get("failures")
		.and_then(Value::as_array)
		.map(Vec::as_slice)
		.unwrap_or_default();
	if failures.is_empty() {
		return;
	}
	output.push_str("\n\n## Failed");
	for failure in failures {
		let reference = failure
			.get("pr")
			.and_then(Value::as_str)
			.unwrap_or("unknown");
		let message = failure
			.get("message")
			.and_then(Value::as_str)
			.unwrap_or("failed");
		let _ = write!(output, "\n- {reference}: {message}");
	}
}

fn render_search(op: Operation, value: &Value) -> String {
	let label = op.get_message().unwrap_or("Search");
	let rows = value
		.get("items")
		.and_then(Value::as_array)
		.map(Vec::as_slice)
		.unwrap_or_default();
	let mut output = format!("# GitHub {label}\n");
	if let Some(query) = value.get("query").and_then(Value::as_str) {
		let _ = write!(output, "\nQuery: {query}");
	}
	if let Some(repo) = value.get("repository").and_then(Value::as_str) {
		let _ = write!(output, "\nRepository: {repo}");
	}
	let _ = write!(output, "\nResults: {}", rows.len());
	for row in rows {
		match op {
			Operation::SearchIssues | Operation::SearchPrs => {
				let number = row
					.get("number")
					.and_then(Value::as_u64)
					.unwrap_or_default();
				let title = row
					.get("title")
					.and_then(Value::as_str)
					.unwrap_or("Untitled");
				let state = row
					.get("state")
					.and_then(Value::as_str)
					.unwrap_or("unknown");
				let _ = write!(output, "\n- [{state}] #{number} {title}");
			},
			Operation::SearchCode => {
				let repo = row
					.pointer("/repository/full_name")
					.and_then(Value::as_str)
					.unwrap_or("unknown repository");
				let path = row
					.get("path")
					.and_then(Value::as_str)
					.unwrap_or("unknown path");
				let _ = write!(output, "\n- {repo}:{path}");
				if let Some(fragment) = row
					.pointer("/text_matches/0/fragment")
					.and_then(Value::as_str)
					.and_then(|text| text.lines().next())
				{
					let _ = write!(output, "\n  Match: {}", fragment.trim());
				}
			},
			Operation::SearchCommits => {
				let sha = row.get("sha").and_then(Value::as_str).unwrap_or("unknown");
				let subject = row
					.pointer("/commit/message")
					.and_then(Value::as_str)
					.and_then(|text| text.lines().next())
					.unwrap_or("(no commit message)");
				let _ = write!(output, "\n- {} {subject}", &sha[..sha.len().min(8)]);
			},
			Operation::SearchRepos => {
				let repo = row
					.get("full_name")
					.and_then(Value::as_str)
					.unwrap_or("unknown repository");
				let stars = row
					.get("stargazers_count")
					.and_then(Value::as_u64)
					.unwrap_or_default();
				let _ = write!(output, "\n- {repo} ({stars} stars)");
			},
			_ => unreachable!(),
		}
		if let Some(url) = row.get("html_url").and_then(Value::as_str) {
			let _ = write!(output, "\n  URL: {url}");
		}
	}
	output
}

fn render_actions(value: &Value) -> String {
	let outcome = value
		.get("outcome")
		.and_then(Value::as_str)
		.unwrap_or("pending");
	let poll = value
		.get("poll_count")
		.and_then(Value::as_u64)
		.unwrap_or_default();
	let mut output = format!("# GitHub Actions\n\nOutcome: {outcome}\nPoll: {poll}");
	let runs = if let Some(run) = value.get("run") {
		std::slice::from_ref(run)
	} else {
		value
			.get("workflow_runs")
			.and_then(Value::as_array)
			.map(Vec::as_slice)
			.unwrap_or_default()
	};
	for run in runs {
		let id = run.get("id").and_then(Value::as_u64).unwrap_or_default();
		let name = run
			.get("name")
			.or_else(|| run.get("display_title"))
			.and_then(Value::as_str)
			.unwrap_or("workflow");
		let state = run
			.get("conclusion")
			.or_else(|| run.get("status"))
			.and_then(Value::as_str)
			.unwrap_or("pending");
		let _ = write!(output, "\n- {name} ({id}): {state}");
		let jobs = run
			.get("jobs")
			.and_then(Value::as_array)
			.or_else(|| value.get("jobs").and_then(Value::as_array));
		for job in jobs.into_iter().flatten() {
			let name = job.get("name").and_then(Value::as_str).unwrap_or("job");
			let state = job
				.get("conclusion")
				.or_else(|| job.get("status"))
				.and_then(Value::as_str)
				.unwrap_or("pending");
			let _ = write!(output, "\n  - {name}: {state}");
		}
	}
	if let Some(logs) = value.get("failed_logs").and_then(Value::as_array) {
		for log in logs {
			let name = log.get("job_name").and_then(Value::as_str).unwrap_or("job");
			let _ = write!(output, "\n\n## {name}");
			if let Some(tail) = log.get("tail").and_then(Value::as_str) {
				let _ = write!(output, "\n{tail}");
			} else {
				output.push_str("\nLog unavailable.");
			}
		}
	}
	output
}

fn required<'a>(value: Option<&'a str>, message: &'static str) -> Result<&'a str, Fault> {
	value
		.map(str::trim)
		.filter(|value| !value.is_empty())
		.ok_or_else(|| fault("github_invalid_request", message))
}
fn fault(code: &'static str, message: &'static str) -> Fault {
	Fault {
		code:                 Str::new_static(code),
		message:              Str::new_static(message),
		status:               None,
		rate_limit_remaining: None,
		rate_limit_reset:     None,
		retry_after_seconds:  None,
	}
}
fn cancelled_fault() -> Fault {
	fault("github_cancelled", "GitHub operation was cancelled")
}
fn http_fault(_: reqwest::Error) -> Fault {
	fault("github_transport_failed", "GitHub transport failed before a response was received")
}
fn git_fault(_: omp_vcs::Error) -> Fault {
	fault("github_git_failed", "Git operation failed")
}
fn sanitize_remote_message(message: &str) -> String {
	message
		.chars()
		.filter(|character| !character.is_control() || matches!(character, '\n' | '\t'))
		.take(1_024)
		.collect()
}
#[cfg(test)]
mod tests {
	use std::time::Duration;

	use serde_json::json;
	use tokio_util::sync::CancellationToken;

	use super::{
		ActionsState, DateField, Operation, Params, PrMetadata, PushReceipt, UNIX_EPOCH,
		actions_runs_endpoint, actions_state, attach_push_receipt, branch_endpoint, compare_endpoint,
		date_qualifier, days_from_civil, decode_file_response, failed_jobs, file_endpoint,
		fill_from_commits, has_scope, local_branch_target, normalize_date_bound, parse_pr_number,
		parse_run_reference, poll_sleep, pr_branch_endpoint, push_target, render_output,
		run_jobs_endpoint, tail_limit, tail_lines,
	};
	use crate::github_url::GithubRepo;

	#[test]
	fn pr_metadata_posts_reviewers_assignees_and_labels_in_order() {
		let params: Params = serde_json::from_value(json!({
			"op": "pr_create",
			"head": "feature/foo",
			"title": "t",
			"reviewer": [" alice ", "org/core-team", "alice", ""],
			"assignee": ["bob"],
			"label": ["bug", "p1"],
		}))
		.expect("params");
		let metadata = PrMetadata::from_params(&params);
		assert_eq!(metadata, PrMetadata {
			reviewers:      vec!["alice".to_owned()],
			team_reviewers: vec!["core-team".to_owned()],
			assignees:      vec!["bob".to_owned()],
			labels:         vec!["bug".to_owned(), "p1".to_owned()],
		});
		let requests = metadata.requests("owner/repo", 7);
		assert_eq!(requests, vec![
			(
				"/repos/owner/repo/pulls/7/requested_reviewers".to_owned(),
				json!({ "reviewers": ["alice"], "team_reviewers": ["core-team"] }),
			),
			("/repos/owner/repo/issues/7/assignees".to_owned(), json!({ "assignees": ["bob"] })),
			("/repos/owner/repo/issues/7/labels".to_owned(), json!({ "labels": ["bug", "p1"] })),
		]);

		let bare: Params =
			serde_json::from_value(json!({ "op": "pr_create", "head": "h", "title": "t" }))
				.expect("params");
		assert!(PrMetadata::from_params(&bare).is_empty());
		assert!(
			PrMetadata::from_params(&bare)
				.requests("owner/repo", 7)
				.is_empty()
		);
	}

	#[test]
	fn fill_derives_title_and_body_like_gh() {
		assert_eq!(
			compare_endpoint("owner/repo", "main", "feature/foo"),
			"/repos/owner/repo/compare/main...feature%2Ffoo",
		);
		assert_eq!(
			fill_from_commits("feature/foo", &["Fix parser\n\nHandles empty input.\n"])
				.expect("single commit"),
			("Fix parser".to_owned(), "Handles empty input.".to_owned()),
		);
		assert_eq!(
			fill_from_commits("add-retry_logic", &["First change\n\ndetails", "Second change"])
				.expect("several commits"),
			("Add retry logic".to_owned(), "- First change\n- Second change\n".to_owned()),
		);
		assert_eq!(
			fill_from_commits("feature/foo", &[])
				.expect_err("no commits")
				.code,
			"github_invalid_request",
		);
	}

	#[test]
	fn tail_defaults_caps_and_rejects_zero() {
		assert_eq!(tail_limit(None).expect("default"), 15);
		assert_eq!(tail_limit(Some(40)).expect("explicit"), 40);
		assert_eq!(tail_limit(Some(5_000)).expect("capped"), 200);
		assert_eq!(tail_limit(Some(0)).expect_err("zero").message, "tail must be a positive number");
		assert_eq!(
			tail_lines("a\r\nb\r\nc\r\nd\n\n", 2).as_deref(),
			Some("c\nd"),
			"tail keeps the last lines after CRLF normalization",
		);
		assert_eq!(tail_lines("   \n", 5), None);
	}

	#[test]
	fn failed_jobs_are_collected_per_run() {
		let run = json!({
			"run": { "id": 9 },
			"jobs": [
				{ "id": 1, "status": "completed", "conclusion": "success" },
				{ "id": 2, "status": "completed", "conclusion": "failure" },
				{ "id": 3, "status": "in_progress", "conclusion": null },
			],
		});
		let failed = failed_jobs(&run);
		assert_eq!(failed.len(), 1);
		assert_eq!(failed[0].0, 9);
		assert_eq!(failed[0].1["id"], 2);

		let commit = json!({
			"workflow_runs": [
				{ "id": 4, "jobs": [{ "id": 40, "status": "completed", "conclusion": "timed_out" }] },
				{ "id": 5, "jobs": [{ "id": 50, "status": "completed", "conclusion": "skipped" }] },
			],
		});
		let failed = failed_jobs(&commit);
		assert_eq!(failed.len(), 1);
		assert_eq!((failed[0].0, failed[0].1["id"].as_u64()), (4, Some(40)));
	}

	fn instant(year: i64, month: u32, day: u32, hour: u64) -> std::time::SystemTime {
		let days = days_from_civil(year, month, day).expect("test date");
		UNIX_EPOCH
			+ Duration::from_secs(
				u64::try_from(days).expect("post-epoch date") * 86_400 + hour * 3_600,
			)
	}

	#[test]
	fn file_request_encodes_each_path_segment_once() {
		assert_eq!(
			file_endpoint("owner/repo", "docs/a#b?100% é.md", Some("feature/encoded"),)
				.expect("valid file path"),
			"/repos/owner/repo/contents/docs/a%23b%3F100%25%20%C3%A9.md?ref=feature%2Fencoded",
		);
		assert!(file_endpoint("owner/repo", "/etc/passwd", None).is_err());
		assert!(file_endpoint("owner/repo", "docs/../secret", None).is_err());
	}

	#[test]
	fn contents_response_must_be_a_base64_file() {
		let dir = tempfile::tempdir().expect("blob tempdir");
		let blobs = super::BlobHost::open(dir.path()).expect("blob store");
		let (decoded, artifact) = decode_file_response(
			&json!({
				"type": "file",
				"encoding": "base64",
				"content": "aGVsbG8=",
				"html_url": "https://github.com/owner/repo/blob/main/hello.txt",
			}),
			"owner/repo",
			"hello.txt",
			None,
			&blobs,
		)
		.expect("file response");
		assert_eq!(decoded["content"], "hello");
		assert!(artifact.is_none());
		assert!(decode_file_response(&json!([]), "owner/repo", "dir", None, &blobs).is_err());
		assert!(
			decode_file_response(
				&json!({ "type": "file", "encoding": "utf-8", "content": "hello" }),
				"owner/repo",
				"hello.txt",
				None,
				&blobs,
			)
			.is_err(),
		);
		let (binary, artifact) = decode_file_response(
			&json!({ "type": "file", "encoding": "base64", "content": "AP8=" }),
			"owner/repo",
			"image.bin",
			None,
			&blobs,
		)
		.expect("binary response");
		assert_eq!(binary["binary"], true);
		assert!(
			artifact
				.expect("binary artifact")
				.uri
				.starts_with("artifact://sha256/")
		);
	}

	#[test]
	fn branch_and_actions_requests_encode_and_commit_scope() {
		assert_eq!(
			pr_branch_endpoint("owner/repo", "owner:feature/foo"),
			"/repos/owner/repo/pulls?state=open&head=owner%3Afeature%2Ffoo&per_page=100",
		);
		assert_eq!(
			branch_endpoint("owner/repo", "feature/foo"),
			"/repos/owner/repo/branches/feature%2Ffoo",
		);
		assert_eq!(
			actions_runs_endpoint("owner/repo", "abc123"),
			"/repos/owner/repo/actions/runs?head_sha=abc123&per_page=100",
		);
		assert!(!actions_runs_endpoint("owner/repo", "abc123").contains("branch="));
		assert_eq!(
			run_jobs_endpoint("owner/repo", 42, 3),
			"/repos/owner/repo/actions/runs/42/jobs?per_page=100&page=3",
		);
	}

	#[test]
	fn pr_references_distinguish_numbers_urls_branches_and_hosts() {
		let default = GithubRepo::parse("owner/repo").expect("default repo");
		assert_eq!(parse_pr_number("17", &default).expect("number"), Some(17));
		assert_eq!(
			parse_pr_number("https://github.com/OWNER/REPO/pull/19/files", &default).expect("URL"),
			Some(19),
		);
		assert_eq!(parse_pr_number("feature/17", &default).expect("branch"), None);
		assert!(parse_pr_number("https://github.com/other/repo/pull/19", &default).is_err(),);

		let enterprise = GithubRepo::parse("ghe.example.com/owner/repo").expect("enterprise repo");
		assert_eq!(
			parse_pr_number("https://ghe.example.com/OWNER/REPO/pull/23", &enterprise)
				.expect("enterprise URL"),
			Some(23),
		);
		let run = parse_run_reference("https://ghe.example.com/OWNER/REPO/actions/runs/987/jobs/1")
			.expect("enterprise run URL");
		assert_eq!(run.id, 987);
		assert_eq!(
			run.repo.expect("run repo").identity().to_ascii_lowercase(),
			"ghe.example.com/owner/repo",
		);
		assert!(
			parse_pr_number("https://github.com/owner/repo/pull/23", &enterprise).is_err(),
			"a PR URL from another host must not drive the enterprise checkout",
		);
	}

	#[test]
	fn search_scope_and_projection_cover_all_search_families() {
		assert!(has_scope("bug -repo:owner/legacy"));
		assert!(has_scope("ORG:example parser"));
		assert!(!has_scope("parser regression"));
		let output = render_output(
			Operation::SearchCode,
			&json!({
				"query": "needle repo:owner/repo",
				"repository": "owner/repo",
				"items": [{
					"path": "src/lib.rs",
					"html_url": "https://github.com/owner/repo/blob/main/src/lib.rs",
					"repository": {"full_name": "owner/repo"},
					"text_matches": [{"fragment": "needle();\ncontext"}],
				}],
			}),
			None,
		);
		assert!(output.contains("owner/repo:src/lib.rs"));
		assert!(output.contains("Match: needle();"));
	}

	#[test]
	fn search_dates_normalize_and_use_operation_fields() {
		let now = instant(2026, 8, 26, 12);
		assert_eq!(normalize_date_bound("3d", now).expect("relative date"), "2026-08-23",);
		assert_eq!(
			date_qualifier(Operation::SearchCommits, None, Some("3d"), None, now)
				.expect("commit qualifier")
				.as_deref(),
			Some("committer-date:>=2026-08-23"),
		);
		assert_eq!(
			date_qualifier(
				Operation::SearchRepos,
				Some(DateField::Updated),
				Some("2026-08-01"),
				Some("2026-08-26"),
				now,
			)
			.expect("repository qualifier")
			.as_deref(),
			Some("pushed:2026-08-01..2026-08-26"),
		);
		assert_eq!(
			normalize_date_bound("2026-08-26T01:02:03.999+02:00", now).expect("ISO datetime"),
			"2026-08-25T23:02:03Z",
		);
		assert!(normalize_date_bound("next Thursday", now).is_err());
	}

	#[test]
	fn actions_outcome_inspects_job_conclusions() {
		let value = json!({
			"workflow_runs": [{
				"id": 1,
				"status": "in_progress",
				"conclusion": null,
				"jobs": [{ "status": "completed", "conclusion": "failure" }],
			}],
		});
		assert!(matches!(actions_state(&value), ActionsState::Failure));
		let pending = json!({ "workflow_runs": [] });
		assert!(matches!(actions_state(&pending), ActionsState::Pending));
	}
	#[tokio::test]
	async fn poll_sleep_stops_immediately_when_cancelled() {
		let cancellation = CancellationToken::new();
		cancellation.cancel();
		tokio::time::timeout(
			Duration::from_millis(50),
			poll_sleep(Duration::from_secs(3_600), &cancellation),
		)
		.await
		.expect("cancelled wait must not sleep")
		.expect_err("cancelled wait must fail");
	}

	fn git(root: &std::path::Path, args: &[&str]) {
		assert!(
			std::process::Command::new("git")
				.args(args)
				.current_dir(root)
				.env("GIT_AUTHOR_NAME", "t")
				.env("GIT_AUTHOR_EMAIL", "t@example.com")
				.env("GIT_COMMITTER_NAME", "t")
				.env("GIT_COMMITTER_EMAIL", "t@example.com")
				.status()
				.unwrap()
				.success(),
			"git {args:?}"
		);
	}

	#[test]
	fn local_head_resolution_reads_sha_and_upstream_remote() {
		let dir = tempfile::tempdir().unwrap();
		let root = dir.path();
		git(root, &["init", "-q", "-b", "main"]);
		git(root, &["commit", "-q", "--allow-empty", "-m", "base"]);
		git(root, &["remote", "add", "fork", "git@github.com:me/repo.git"]);
		git(root, &["remote", "add", "origin", "https://github.com/Owner/Repo.git"]);
		git(root, &["config", "branch.main.remote", "fork"]);

		let (sha, upstream) = local_branch_target(root, "main")
			.unwrap()
			.expect("local branch");
		assert_eq!(sha.len(), 40);
		assert_eq!(upstream.as_deref(), Some("fork"));
		assert!(local_branch_target(root, "missing").unwrap().is_none());

		let local = omp_vcs::git::GitRepo::require(root).unwrap();
		let target = GithubRepo::parse("owner/repo").unwrap();
		assert_eq!(push_target(&local, upstream, &target), "origin", "upstream points at a fork");
		assert_eq!(push_target(&local, Some("origin".to_owned()), &target), "origin");
		let unrelated = GithubRepo::parse("ghe.example.com/team/svc").unwrap();
		assert_eq!(push_target(&local, None, &unrelated), "https://ghe.example.com/team/svc.git");
	}

	#[test]
	fn created_pr_reports_the_head_push() {
		let mut value = json!({
			"number": 9,
			"title": "Ship it",
			"head": { "ref": "feature" },
			"base": { "ref": "main" },
		});
		attach_push_receipt(&mut value, None);
		assert!(value.get("omp_pushed_head").is_none());
		attach_push_receipt(
			&mut value,
			Some(PushReceipt {
				branch:   "feature".to_owned(),
				remote:   "origin".to_owned(),
				sha:      "0123456789abcdef0123456789abcdef01234567".to_owned(),
				replaced: None,
			}),
		);
		let rendered = render_output(Operation::PrCreate, &value, None);
		assert!(
			rendered.ends_with("Published feature at 0123456789ab to origin before opening"),
			"{rendered}"
		);
	}
}
