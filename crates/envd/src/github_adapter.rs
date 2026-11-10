//! GitHub-backed [`ForgeHost`] and [`CiHost`] adapters.
//!
//! `GithubService` still performs the HTTP and git I/O. These wrappers speak
//! the neutral contracts and are installed behind `Delegated` routers.
//! `view_change_request`, `view_issue`, the unified diff, and the comment
//! list are reads. Numbered `pr://` and `issue://` resources call them.
//! `PermitAll` is intentional: the desk gate is the tool commit, and the
//! routers are not exported, so a second approval would only repeat that gate.
//!
//! Hosts are registered explicitly. `github.com` is always present. The
//! checkout host from `infer_repo` is added when it differs. There is no
//! fallback that sends every host to GitHub.
//!
//! The CI registration advertises `log_stream` only. Status stays allowed
//! because the contract does not flag it. Cancel, rerun, trigger, artifacts,
//! and play-manual are `NotCapable` at the router.

use std::{future::Future, sync::Arc};

use omp_ci::{
	ArtifactPage, Capabilities, CiError, CiHost, CiOp, CiProvider, CiStatus, Job, LogPage, Pipeline,
	RunRef, Trigger,
};
use omp_ci_delegated::{Delegated as CiDelegated, PermitAll as CiPermitAll};
use omp_core::{Str, sf};
use omp_forge::{
	ChangeRequest, ChangeRequestDraft, CheckTarget, CheckoutReceipt, CheckoutRequest, Comment,
	CommitRef, FileBody, FileQuery, ForgeChecks, ForgeError, ForgeHost, ForgeOp, Issue, IssueDraft,
	IssueState, PushReceipt, PushRequest, RepoRef, Repository, RequestState, ResourceKind,
	SearchHit, SearchKind, SearchPage, SearchQuery,
};
use omp_forge_delegated::{Delegated as ForgeDelegated, PermitAll as ForgePermitAll};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::{
	github::GithubService,
	github_params::{self, Operation, Params, Payload, PrSelector},
	github_url,
};

/// GitHub implementation of [`ForgeHost`].
#[derive(Clone)]
pub(crate) struct GithubForgeAdapter {
	service: Arc<GithubService>,
}

/// GitHub implementation of [`CiHost`].
#[derive(Clone)]
pub(crate) struct GithubCiAdapter {
	service: Arc<GithubService>,
}

/// Routes `forge@1` to [`GithubForgeAdapter`].
///
/// See the module docs for `PermitAll` and host registration.
pub(crate) fn forge_router(
	service: Arc<GithubService>,
) -> ForgeDelegated<GithubForgeAdapter, ForgePermitAll> {
	let adapter = GithubForgeAdapter { service: Arc::clone(&service) };
	let mut builder = ForgeDelegated::builder(ForgePermitAll);
	builder
		.register(github_url::GITHUB_HOST, adapter.clone())
		.expect("github.com is a forge host");
	register_checkout_host(&service, |host| {
		let _ = builder.register(host, adapter.clone());
	});
	builder.build()
}

/// Routes `ci@1` to [`GithubCiAdapter`] with log streaming only.
///
/// See the module docs for `PermitAll` and host registration.
pub(crate) fn ci_router(service: Arc<GithubService>) -> CiDelegated<GithubCiAdapter, CiPermitAll> {
	let adapter = GithubCiAdapter { service: Arc::clone(&service) };
	let caps = github_capabilities();
	let mut builder = CiDelegated::builder(CiPermitAll);
	builder
		.register(github_url::GITHUB_HOST, adapter.clone(), caps)
		.expect("github.com is a CI provider");
	register_checkout_host(&service, |host| {
		let _ = builder.register(host, adapter.clone(), caps);
	});
	builder.build()
}

fn github_capabilities() -> Capabilities {
	Capabilities { log_stream: true, ..Capabilities::none() }
}

fn register_checkout_host(service: &GithubService, mut register: impl FnMut(&str)) {
	let Ok(repo) = github_url::infer_repo(service.root()) else {
		return;
	};
	if repo.host().eq_ignore_ascii_case(github_url::GITHUB_HOST) {
		return;
	}
	register(repo.host());
}

struct CancelOnDrop(CancellationToken);

impl Drop for CancelOnDrop {
	fn drop(&mut self) {
		self.0.cancel();
	}
}

async fn execute(service: &GithubService, params: Params) -> Result<Payload, github_params::Fault> {
	let token = CancellationToken::new();
	let _guard = CancelOnDrop(token.clone());
	let (updates, _rx) = flume::bounded(1);
	service.execute(params, token, updates).await
}

fn blank(op: Operation) -> Params {
	Params {
		op,
		repo: None,
		path: None,
		branch: None,
		pr: None,
		query: None,
		since: None,
		until: None,
		date_field: None,
		limit: None,
		title: None,
		body: None,
		base: None,
		head: None,
		run: None,
		draft: false,
		force: false,
		force_with_lease: false,
		fill: false,
		reviewer: Vec::new(),
		assignee: Vec::new(),
		label: Vec::new(),
		tail: None,
	}
}

fn repo_arg(op: ForgeOp, repo: &RepoRef) -> Result<Str, ForgeError> {
	if repo.owner.contains('/') {
		return Err(ForgeError::Unavailable { op });
	}
	if repo.host.eq_ignore_ascii_case(github_url::GITHUB_HOST) {
		Ok(sf!("{}/{}", repo.owner, repo.name))
	} else {
		Ok(sf!("{}/{}/{}", repo.host, repo.owner, repo.name))
	}
}

fn map_forge(
	op: ForgeOp,
	repo: &RepoRef,
	number: Option<u64>,
	path: Option<&str>,
	fault: github_params::Fault,
) -> ForgeError {
	if fault.status == Some(404) {
		return match op {
			ForgeOp::RepoView => ForgeError::RepoNotFound {
				host:  repo.host.clone(),
				owner: repo.owner.clone(),
				name:  repo.name.clone(),
			},
			ForgeOp::FileRead => ForgeError::FileNotFound {
				host:  repo.host.clone(),
				owner: repo.owner.clone(),
				name:  repo.name.clone(),
				path:  Str::new(path.unwrap_or_default()),
			},
			ForgeOp::ChangeRequestView
			| ForgeOp::ChangeRequestCreate
			| ForgeOp::ChangeRequestCheckout
			| ForgeOp::ChangeRequestPush => ForgeError::ChangeRequestNotFound {
				host:   repo.host.clone(),
				owner:  repo.owner.clone(),
				name:   repo.name.clone(),
				number: number.unwrap_or(0),
			},
			ForgeOp::IssueView | ForgeOp::IssueCreate => ForgeError::IssueNotFound {
				host:   repo.host.clone(),
				owner:  repo.owner.clone(),
				name:   repo.name.clone(),
				number: number.unwrap_or(0),
			},
			_ => ForgeError::Rejected { op, status: 404 },
		};
	}
	match fault.status {
		Some(status) => ForgeError::Rejected { op, status },
		None => ForgeError::Unavailable { op },
	}
}

fn repository_from(repo: &RepoRef, value: &Value) -> Repository {
	Repository {
		repo:           repo.clone(),
		default_branch: json_str(value, "default_branch"),
		clone_url:      json_str(value, "clone_url"),
		web_url:        json_str(value, "html_url"),
		description:    json_str(value, "description"),
	}
}

fn github_repo(op: ForgeOp, repo: &RepoRef) -> Result<github_url::GithubRepo, ForgeError> {
	if repo.owner.contains('/') {
		return Err(ForgeError::Unavailable { op });
	}
	github_url::GithubRepo::new(repo.host.as_str(), repo.owner.as_str(), repo.name.as_str())
		.map_err(|_| ForgeError::Unavailable { op })
}

fn change_request_from(
	repo: &RepoRef,
	value: &Value,
	op: ForgeOp,
) -> Result<ChangeRequest, ForgeError> {
	let number = value
		.get("number")
		.and_then(Value::as_u64)
		.ok_or(ForgeError::Unavailable { op })?;
	let merged = value
		.get("merged")
		.and_then(Value::as_bool)
		.unwrap_or(false);
	let state = if merged {
		RequestState::Merged
	} else if value.get("state").and_then(Value::as_str) == Some("closed") {
		RequestState::Closed
	} else {
		RequestState::Open
	};
	Ok(ChangeRequest {
		repo: repo.clone(),
		number,
		title: json_str(value, "title"),
		body: json_str(value, "body"),
		state,
		draft: value.get("draft").and_then(Value::as_bool).unwrap_or(false),
		base_ref: pointer_str(value, "/base/ref"),
		head_ref: pointer_str(value, "/head/ref"),
		head_commit: pointer_str(value, "/head/sha"),
		base_commit: pointer_str(value, "/base/sha"),
		web_url: json_str(value, "html_url"),
	})
}

fn issue_from(repo: &RepoRef, value: &Value) -> Result<Issue, ForgeError> {
	let number = value
		.get("number")
		.and_then(Value::as_u64)
		.ok_or(ForgeError::Unavailable { op: ForgeOp::IssueView })?;
	let state = if value.get("state").and_then(Value::as_str) == Some("closed") {
		IssueState::Closed
	} else {
		IssueState::Open
	};
	Ok(Issue {
		repo: repo.clone(),
		number,
		title: json_str(value, "title"),
		body: json_str(value, "body"),
		state,
		web_url: json_str(value, "html_url"),
	})
}

fn comment_from(value: &Value) -> Comment {
	Comment {
		author:     value
			.pointer("/user/login")
			.and_then(Value::as_str)
			.map(Str::new)
			.unwrap_or_else(Str::empty),
		created_at: json_str(value, "created_at"),
		body:       json_str(value, "body"),
	}
}

fn checkout_from(repo: &RepoRef, value: &Value) -> Result<CheckoutReceipt, ForgeError> {
	let row = value
		.pointer("/checkouts/0")
		.ok_or(ForgeError::Unavailable { op: ForgeOp::ChangeRequestCheckout })?;
	let number = row
		.get("pr")
		.and_then(Value::as_u64)
		.ok_or(ForgeError::Unavailable { op: ForgeOp::ChangeRequestCheckout })?;
	let sha = row
		.get("head_sha")
		.and_then(Value::as_str)
		.unwrap_or_default();
	let head = CommitRef::try_new(repo.clone(), sha)?;
	Ok(CheckoutReceipt {
		repo: repo.clone(),
		number,
		head,
		base_ref: json_str(row, "base_ref"),
		head_ref: json_str(row, "remote_branch"),
		worktree: json_str(row, "path"),
	})
}

fn push_from(repo: &RepoRef, value: &Value) -> Result<PushReceipt, ForgeError> {
	let row = value
		.pointer("/pushed/0")
		.ok_or(ForgeError::Unavailable { op: ForgeOp::ChangeRequestPush })?;
	let number = row
		.get("pr")
		.and_then(Value::as_u64)
		.ok_or(ForgeError::Unavailable { op: ForgeOp::ChangeRequestPush })?;
	Ok(PushReceipt { repo: repo.clone(), number, head_commit: json_str(row, "head_sha") })
}

fn search_page(query: &SearchQuery, value: &Value) -> SearchPage {
	let fallback = query.repo.clone();
	let hits = value
		.get("items")
		.and_then(Value::as_array)
		.map(|items| {
			items
				.iter()
				.filter_map(|row| search_hit(query.kind, fallback.as_ref(), row))
				.collect::<Vec<_>>()
		})
		.unwrap_or_default();
	SearchPage::new(query.kind, hits)
}

fn search_hit(kind: SearchKind, fallback: Option<&RepoRef>, row: &Value) -> Option<SearchHit> {
	let repo = hit_repo(kind, fallback?, row);
	let (title, number, commit) = match kind {
		SearchKind::ChangeRequests | SearchKind::Issues => {
			(json_str(row, "title"), row.get("number").and_then(Value::as_u64), None)
		},
		SearchKind::Code => (json_str(row, "path"), None, None),
		SearchKind::Commits => (
			pointer_str(row, "/commit/message")
				.as_str()
				.lines()
				.next()
				.unwrap_or_default()
				.into(),
			None,
			row.get("sha").and_then(Value::as_str).map(Str::new),
		),
		SearchKind::Repositories => (json_str(row, "full_name"), None, None),
	};
	Some(SearchHit { repo, title, web_url: json_str(row, "html_url"), number, commit })
}

fn hit_repo(kind: SearchKind, fallback: &RepoRef, row: &Value) -> RepoRef {
	let full = match kind {
		SearchKind::Code => row.pointer("/repository/full_name").and_then(Value::as_str),
		SearchKind::Repositories => row.get("full_name").and_then(Value::as_str),
		_ => None,
	};
	if let Some((owner, name)) = full.and_then(|full| full.split_once('/'))
		&& let Ok(repo) = RepoRef::try_new(fallback.host.clone(), owner, name)
	{
		return repo;
	}
	fallback.clone()
}

fn json_str(value: &Value, key: &str) -> Str {
	value
		.get(key)
		.and_then(Value::as_str)
		.map(Str::new)
		.unwrap_or_else(Str::empty)
}

fn pointer_str(value: &Value, pointer: &str) -> Str {
	value
		.pointer(pointer)
		.and_then(Value::as_str)
		.map(Str::new)
		.unwrap_or_else(Str::empty)
}

fn search_operation(kind: SearchKind) -> Operation {
	match kind {
		SearchKind::ChangeRequests => Operation::SearchPrs,
		SearchKind::Issues => Operation::SearchIssues,
		SearchKind::Code => Operation::SearchCode,
		SearchKind::Commits => Operation::SearchCommits,
		SearchKind::Repositories => Operation::SearchRepos,
	}
}

impl ForgeHost for GithubForgeAdapter {
	fn view_repo(
		&self,
		repo: &RepoRef,
	) -> impl Future<Output = Result<Repository, ForgeError>> + Send {
		let service = Arc::clone(&self.service);
		let repo = repo.clone();
		async move {
			let mut params = blank(Operation::RepoView);
			params.repo = Some(repo_arg(ForgeOp::RepoView, &repo)?);
			let payload = execute(&service, params)
				.await
				.map_err(|fault| map_forge(ForgeOp::RepoView, &repo, None, None, fault))?;
			Ok(repository_from(&repo, &payload.result))
		}
	}

	fn read_file(
		&self,
		query: &FileQuery,
	) -> impl Future<Output = Result<FileBody, ForgeError>> + Send {
		let service = Arc::clone(&self.service);
		let query = query.clone();
		async move {
			let mut params = blank(Operation::FileRead);
			params.repo = Some(repo_arg(ForgeOp::FileRead, &query.repo)?);
			params.path = Some(query.path.clone());
			params.branch = Some(query.rev.clone());
			let payload = execute(&service, params).await.map_err(|fault| {
				map_forge(ForgeOp::FileRead, &query.repo, None, Some(query.path.as_str()), fault)
			})?;
			let content = if payload.result.get("binary").and_then(Value::as_bool) == Some(true) {
				let artifact = payload
					.artifact
					.as_ref()
					.ok_or(ForgeError::Unavailable { op: ForgeOp::FileRead })?;
				service
					.artifact_bytes(artifact)
					.map_err(|_| ForgeError::Unavailable { op: ForgeOp::FileRead })?
			} else {
				bytes::Bytes::from(payload.output.as_str().as_bytes().to_vec())
			};
			Ok(FileBody { query, content })
		}
	}

	fn view_change_request(
		&self,
		repo: &RepoRef,
		number: u64,
	) -> impl Future<Output = Result<ChangeRequest, ForgeError>> + Send {
		let service = Arc::clone(&self.service);
		let repo = repo.clone();
		async move {
			let github = github_repo(ForgeOp::ChangeRequestView, &repo)?;
			let token = CancellationToken::new();
			let _guard = CancelOnDrop(token.clone());
			let response = service
				.pr_view(&github, number, &token)
				.await
				.map_err(|fault| {
					map_forge(ForgeOp::ChangeRequestView, &repo, Some(number), None, fault)
				})?;
			change_request_from(&repo, &response.value, ForgeOp::ChangeRequestView)
		}
	}

	fn view_issue(
		&self,
		repo: &RepoRef,
		number: u64,
	) -> impl Future<Output = Result<Issue, ForgeError>> + Send {
		let service = Arc::clone(&self.service);
		let repo = repo.clone();
		async move {
			let github = github_repo(ForgeOp::IssueView, &repo)?;
			let token = CancellationToken::new();
			let _guard = CancelOnDrop(token.clone());
			let response = service
				.issue_view(&github, number, &token)
				.await
				.map_err(|fault| map_forge(ForgeOp::IssueView, &repo, Some(number), None, fault))?;
			issue_from(&repo, &response.value)
		}
	}

	fn change_request_diff(
		&self,
		repo: &RepoRef,
		number: u64,
	) -> impl Future<Output = Result<Str, ForgeError>> + Send {
		let service = Arc::clone(&self.service);
		let repo = repo.clone();
		async move {
			let github = github_repo(ForgeOp::ChangeRequestView, &repo)?;
			let token = CancellationToken::new();
			let _guard = CancelOnDrop(token.clone());
			let text = service
				.pr_diff(&github, number, &token)
				.await
				.map_err(|fault| {
					map_forge(ForgeOp::ChangeRequestView, &repo, Some(number), None, fault)
				})?;
			Ok(Str::new(text))
		}
	}

	fn list_comments(
		&self,
		repo: &RepoRef,
		number: u64,
		kind: ResourceKind,
	) -> impl Future<Output = Result<Arc<[Comment]>, ForgeError>> + Send {
		let service = Arc::clone(&self.service);
		let repo = repo.clone();
		async move {
			let op = match kind {
				ResourceKind::ChangeRequest => ForgeOp::ChangeRequestView,
				ResourceKind::Issue => ForgeOp::IssueView,
			};
			let github = github_repo(op, &repo)?;
			let token = CancellationToken::new();
			let _guard = CancelOnDrop(token.clone());
			let rows = service
				.list_comments(&github, number, &token)
				.await
				.map_err(|fault| map_forge(op, &repo, Some(number), None, fault))?;
			Ok(rows.iter().map(comment_from).collect())
		}
	}

	fn list_checks(
		&self,
		_repo: &RepoRef,
		_target: &CheckTarget,
	) -> impl Future<Output = Result<ForgeChecks, ForgeError>> + Send {
		std::future::ready(Err(ForgeError::Unsupported { op: ForgeOp::ListChecks }))
	}

	fn search(
		&self,
		query: SearchQuery,
	) -> impl Future<Output = Result<SearchPage, ForgeError>> + Send {
		let service = Arc::clone(&self.service);
		async move {
			let Some(repo) = query.repo.clone() else {
				return Err(ForgeError::Unscoped { op: query.kind.op() });
			};
			let mut params = blank(search_operation(query.kind));
			params.repo = Some(repo_arg(query.kind.op(), &repo)?);
			params.query = Some(query.text.clone());
			params.limit = (query.limit != 0).then_some(query.limit);
			let payload = execute(&service, params)
				.await
				.map_err(|fault| map_forge(query.kind.op(), &repo, None, None, fault))?;
			Ok(search_page(&query, &payload.result))
		}
	}

	fn create_change_request(
		&self,
		draft: ChangeRequestDraft,
	) -> impl Future<Output = Result<ChangeRequest, ForgeError>> + Send {
		let service = Arc::clone(&self.service);
		async move {
			let mut params = blank(Operation::PrCreate);
			params.repo = Some(repo_arg(ForgeOp::ChangeRequestCreate, &draft.repo)?);
			params.title = Some(draft.title.clone());
			if !draft.body.is_empty() {
				params.body = Some(draft.body.clone());
			}
			if !draft.base.is_empty() {
				params.base = Some(draft.base.clone());
			}
			if !draft.head.is_empty() {
				params.head = Some(draft.head.clone());
			}
			params.draft = draft.draft;
			let payload = execute(&service, params).await.map_err(|fault| {
				map_forge(ForgeOp::ChangeRequestCreate, &draft.repo, None, None, fault)
			})?;
			change_request_from(&draft.repo, &payload.result, ForgeOp::ChangeRequestCreate)
		}
	}

	fn checkout_change_request(
		&self,
		request: &CheckoutRequest,
	) -> impl Future<Output = Result<CheckoutReceipt, ForgeError>> + Send {
		let service = Arc::clone(&self.service);
		let request = request.clone();
		async move {
			let mut params = blank(Operation::PrCheckout);
			params.repo = Some(repo_arg(ForgeOp::ChangeRequestCheckout, &request.repo)?);
			params.pr = Some(PrSelector::One(sf!("{}", request.number)));
			params.force = request.force;
			let payload = execute(&service, params).await.map_err(|fault| {
				map_forge(
					ForgeOp::ChangeRequestCheckout,
					&request.repo,
					Some(request.number),
					None,
					fault,
				)
			})?;
			checkout_from(&request.repo, &payload.result)
		}
	}

	fn push_change_request(
		&self,
		request: &PushRequest,
	) -> impl Future<Output = Result<PushReceipt, ForgeError>> + Send {
		let service = Arc::clone(&self.service);
		let request = request.clone();
		async move {
			let mut params = blank(Operation::PrPush);
			params.repo = Some(repo_arg(ForgeOp::ChangeRequestPush, &request.repo)?);
			params.pr = Some(PrSelector::One(sf!("{}", request.number)));
			params.force_with_lease = request.force_with_lease;
			if !request.head_ref.is_empty() {
				params.head = Some(request.head_ref.clone());
			}
			let payload = execute(&service, params).await.map_err(|fault| {
				map_forge(ForgeOp::ChangeRequestPush, &request.repo, Some(request.number), None, fault)
			})?;
			push_from(&request.repo, &payload.result)
		}
	}

	fn create_issue(
		&self,
		_draft: IssueDraft,
	) -> impl Future<Output = Result<Issue, ForgeError>> + Send {
		std::future::ready(Err(ForgeError::Unsupported { op: ForgeOp::IssueCreate }))
	}
}

fn map_ci(op: CiOp, run: &RunRef, fault: &github_params::Fault) -> CiError {
	if fault.status == Some(404) {
		return match op {
			CiOp::LogStream => CiError::JobNotFound {
				provider: run.provider.id.clone(),
				pipeline: run.pipeline.clone(),
				job:      run.job.clone().unwrap_or_else(Str::empty),
			},
			_ => CiError::PipelineNotFound {
				provider: run.provider.id.clone(),
				pipeline: run.pipeline.clone(),
			},
		};
	}
	match fault.status {
		Some(status) => CiError::Rejected { op, status },
		None => CiError::Unavailable { op },
	}
}

fn pipeline_id(run: &RunRef) -> Result<u64, CiError> {
	let id = run.pipeline.parse::<u64>().unwrap_or(0);
	if id == 0 {
		return Err(CiError::PipelineNotFound {
			provider: run.provider.id.clone(),
			pipeline: run.pipeline.clone(),
		});
	}
	Ok(id)
}

/// Maps one Actions run snapshot (`{ run, jobs }`) onto a [`Pipeline`].
fn pipeline_from_actions(run: &RunRef, value: &Value) -> Pipeline {
	let body = value.get("run").unwrap_or(value);
	let name = body
		.get("name")
		.and_then(Value::as_str)
		.or_else(|| body.get("display_title").and_then(Value::as_str))
		.unwrap_or_default();
	let status = ci_status(
		body.get("status").and_then(Value::as_str),
		body.get("conclusion").and_then(Value::as_str),
	);
	let jobs = value
		.get("jobs")
		.and_then(Value::as_array)
		.map(|jobs| jobs.iter().filter_map(job_from_actions).collect::<Vec<_>>())
		.unwrap_or_default();
	let mut pipeline = Pipeline::new(run.provider.clone(), run.pipeline.clone(), name, status, jobs);
	if let Some(url) = body.get("html_url").and_then(Value::as_str) {
		pipeline = pipeline.with_web_url(url);
	}
	if let Some(revision) = body.get("head_sha").and_then(Value::as_str) {
		pipeline = pipeline.with_revision(revision);
	}
	pipeline
}

fn job_from_actions(value: &Value) -> Option<Job> {
	let id = value.get("id").and_then(Value::as_u64)?.to_string();
	let name = value
		.get("name")
		.and_then(Value::as_str)
		.unwrap_or_default();
	let status = ci_status(
		value.get("status").and_then(Value::as_str),
		value.get("conclusion").and_then(Value::as_str),
	);
	let mut job = Job::new(id, name, status);
	if let Some(url) = value.get("html_url").and_then(Value::as_str) {
		job = job.with_web_url(url);
	}
	Some(job)
}

/// Maps GitHub Actions words onto [`CiStatus`].
///
/// `action_required` is [`CiStatus::Failure`]. The old watch treated that
/// conclusion as a failure, and mapping it to [`CiStatus::Manual`] would
/// leave `watch: true` polling a state GitHub will not leave on its own.
fn ci_status(status: Option<&str>, conclusion: Option<&str>) -> CiStatus {
	match conclusion {
		Some("success") => CiStatus::Success,
		Some("cancelled") => CiStatus::Canceled,
		Some("skipped" | "neutral") => CiStatus::Skipped,
		Some("failure" | "timed_out" | "action_required" | "startup_failure") => CiStatus::Failure,
		Some(_) => CiStatus::Failure,
		None => match status {
			Some("queued" | "waiting" | "pending" | "requested") => CiStatus::Queued,
			Some("completed") => CiStatus::Success,
			_ => CiStatus::Running,
		},
	}
}

impl CiHost for GithubCiAdapter {
	fn capabilities(
		&self,
		_provider: &CiProvider,
	) -> impl Future<Output = Result<Capabilities, CiError>> + Send {
		std::future::ready(Ok(github_capabilities()))
	}

	fn status(&self, run: &RunRef) -> impl Future<Output = Result<Pipeline, CiError>> + Send {
		let service = Arc::clone(&self.service);
		let run = run.clone();
		async move {
			let id = pipeline_id(&run)?;
			let repo = service
				.inferred_repo()
				.map_err(|_| CiError::Unavailable { op: CiOp::Status })?;
			let token = CancellationToken::new();
			let _guard = CancelOnDrop(token.clone());
			let response = service
				.actions_run(&repo, id, &token)
				.await
				.map_err(|fault| map_ci(CiOp::Status, &run, &fault))?;
			Ok(pipeline_from_actions(&run, &response.value))
		}
	}

	fn artifacts(
		&self,
		_run: &RunRef,
	) -> impl Future<Output = Result<ArtifactPage, CiError>> + Send {
		std::future::ready(Err(CiError::Unsupported { op: CiOp::Artifacts }))
	}

	fn log_stream(&self, run: &RunRef) -> impl Future<Output = Result<LogPage, CiError>> + Send {
		let service = Arc::clone(&self.service);
		let run = run.clone();
		async move { log_page(&service, &run).await }
	}

	fn cancel(&self, _run: &RunRef) -> impl Future<Output = Result<Pipeline, CiError>> + Send {
		std::future::ready(Err(CiError::Unsupported { op: CiOp::Cancel }))
	}

	fn rerun_failed(&self, _run: &RunRef) -> impl Future<Output = Result<Pipeline, CiError>> + Send {
		std::future::ready(Err(CiError::Unsupported { op: CiOp::RerunFailed }))
	}

	fn rerun_job(&self, _run: &RunRef) -> impl Future<Output = Result<Job, CiError>> + Send {
		std::future::ready(Err(CiError::Unsupported { op: CiOp::RerunJob }))
	}

	fn trigger(&self, _request: Trigger) -> impl Future<Output = Result<Pipeline, CiError>> + Send {
		std::future::ready(Err(CiError::Unsupported { op: CiOp::Trigger }))
	}

	fn play_manual(&self, _run: &RunRef) -> impl Future<Output = Result<Job, CiError>> + Send {
		std::future::ready(Err(CiError::Unsupported { op: CiOp::PlayManual }))
	}
}

async fn log_page(service: &GithubService, run: &RunRef) -> Result<LogPage, CiError> {
	let Some(job) = run.job.clone() else {
		let pipeline = status_snapshot(service, run).await?;
		let mut text = String::new();
		for job in pipeline
			.as_slice()
			.iter()
			.filter(|job| job.status == CiStatus::Failure)
		{
			let mut scoped = run.clone();
			scoped.job = Some(job.id.clone());
			let page = one_job_log(service, &scoped).await?;
			if !text.is_empty() && !page.text.is_empty() {
				text.push('\n');
			}
			text.push_str(page.text.as_str());
		}
		return Ok(LogPage::new(run.clone(), text, true));
	};
	let mut scoped = run.clone();
	scoped.job = Some(job);
	one_job_log(service, &scoped).await
}

async fn status_snapshot(service: &GithubService, run: &RunRef) -> Result<Pipeline, CiError> {
	let id = pipeline_id(run)?;
	let repo = service
		.inferred_repo()
		.map_err(|_| CiError::Unavailable { op: CiOp::Status })?;
	let token = CancellationToken::new();
	let _guard = CancelOnDrop(token.clone());
	let response = service
		.actions_run(&repo, id, &token)
		.await
		.map_err(|fault| map_ci(CiOp::Status, run, &fault))?;
	Ok(pipeline_from_actions(run, &response.value))
}

async fn one_job_log(service: &GithubService, run: &RunRef) -> Result<LogPage, CiError> {
	let job = run
		.job
		.as_deref()
		.ok_or(CiError::MissingJob { op: CiOp::LogStream })?;
	let job_id = job.parse::<u64>().unwrap_or(0);
	if job_id == 0 {
		return Err(CiError::JobNotFound {
			provider: run.provider.id.clone(),
			pipeline: run.pipeline.clone(),
			job:      run.job.clone().unwrap_or_else(Str::empty),
		});
	}
	let repo = service
		.inferred_repo()
		.map_err(|_| CiError::Unavailable { op: CiOp::LogStream })?;
	let token = CancellationToken::new();
	let _guard = CancelOnDrop(token.clone());
	let text = service
		.actions_job_log(&repo, job_id, &token)
		.await
		.map_err(|fault| map_ci(CiOp::LogStream, run, &fault))?
		.unwrap_or_default();
	Ok(LogPage::new(run.clone(), text, true))
}

#[cfg(test)]
mod tests {
	use std::sync::Arc;

	use omp_cache::github_cache::GithubCache;
	use omp_ci::{CiError, CiHost as _, CiOp, CiProvider, CiStatus, RunRef, Trigger};
	use omp_con::Ctx;
	use omp_core::Str;
	use omp_forge::{ForgeError, ForgeHost as _, ForgeOp, IssueState, RepoRef, RequestState};
	use serde_json::json;

	use super::{ci_status, forge_router, pipeline_from_actions};
	use crate::{
		blobs::BlobHost, github::GithubService, github_url::GithubCredentialBridge,
		tool_settings::GithubCacheSettings,
	};

	fn service(root: &std::path::Path) -> Arc<GithubService> {
		let blobs = BlobHost::open(root.join("blobs")).expect("blobs");
		let cache = Arc::new(
			GithubCache::open(
				root.join("github-cache.sqlite3"),
				GithubCacheSettings::from_con(&Ctx::new()).policy(),
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

	fn run(pipeline: &str) -> RunRef {
		RunRef::try_new(CiProvider::try_new("github.com").unwrap(), pipeline, None).unwrap()
	}

	#[tokio::test]
	async fn unregistered_host_is_not_sent_to_github() {
		let dir = tempfile::tempdir().expect("temp");
		let router = forge_router(service(dir.path()));
		let repo = RepoRef::try_new("gitlab.example", "acme", "widget").unwrap();
		let error = router.view_repo(&repo).await.expect_err("unregistered");
		assert!(
			matches!(error, ForgeError::Unregistered { ref host, op: ForgeOp::RepoView } if host == "gitlab.example"),
			"{error}"
		);
	}

	#[tokio::test]
	async fn ci_writes_and_artifacts_are_not_capable() {
		let dir = tempfile::tempdir().expect("temp");
		let router = super::ci_router(service(dir.path()));
		let run = run("42");
		let provider = run.provider.clone();
		for op in [
			CiOp::Cancel,
			CiOp::RerunFailed,
			CiOp::RerunJob,
			CiOp::Trigger,
			CiOp::Artifacts,
			CiOp::PlayManual,
		] {
			let error = match op {
				CiOp::Cancel => router.cancel(&run).await.err(),
				CiOp::RerunFailed => router.rerun_failed(&run).await.err(),
				CiOp::RerunJob => router.rerun_job(&run).await.err(),
				CiOp::Trigger => router
					.trigger(Trigger::try_new(provider.clone(), "ci", None::<(Str, Str)>).unwrap())
					.await
					.err(),
				CiOp::Artifacts => router.artifacts(&run).await.err(),
				CiOp::PlayManual => router.play_manual(&run).await.err(),
				CiOp::Status | CiOp::LogStream => None,
			};
			assert!(
				matches!(error, Some(CiError::NotCapable { op: got, .. }) if got == op),
				"{op} yielded {error:?}"
			);
		}
		let caps = router.capabilities(&provider).await.expect("caps");
		assert!(caps.log_stream);
		assert!(!caps.cancel);
		assert!(!caps.artifacts);
		assert!(caps.allows(CiOp::Status));
	}

	#[test]
	fn actions_snapshot_maps_failure_and_action_required() {
		let run = run("42");
		let pipeline = pipeline_from_actions(
			&run,
			&json!({
				"run": {
					"id": 42,
					"name": "ci",
					"status": "completed",
					"conclusion": "failure",
					"html_url": "https://github.com/acme/widget/actions/runs/42",
					"head_sha": "abc123"
				},
				"jobs": [{
					"id": 7,
					"name": "test",
					"status": "completed",
					"conclusion": "action_required"
				}]
			}),
		);
		assert_eq!(pipeline.status, CiStatus::Failure);
		assert_eq!(pipeline.id.as_str(), "42");
		assert_eq!(pipeline.revision.as_deref(), Some("abc123"));
		assert_eq!(pipeline.jobs.len(), 1);
		assert_eq!(pipeline.jobs[0].status, CiStatus::Failure);
		assert_eq!(ci_status(Some("queued"), None), CiStatus::Queued);
		assert_eq!(ci_status(Some("in_progress"), None), CiStatus::Running);
		assert_eq!(ci_status(Some("completed"), Some("success")), CiStatus::Success);
		assert_eq!(ci_status(Some("completed"), Some("cancelled")), CiStatus::Canceled);
		assert_eq!(ci_status(Some("completed"), Some("skipped")), CiStatus::Skipped);
	}

	fn view_repo() -> RepoRef {
		RepoRef::try_new("github.com", "acme", "widget").unwrap()
	}

	fn http_fault(status: u16) -> crate::github_params::Fault {
		crate::github_params::Fault {
			code:                 Str::new_static("github_http_error"),
			message:              Str::new_static("GitHub API request failed"),
			status:               Some(status),
			rate_limit_remaining: None,
			rate_limit_reset:     None,
			retry_after_seconds:  None,
		}
	}

	#[test]
	fn pull_request_json_maps_onto_a_change_request() {
		let repo = view_repo();
		let open = super::change_request_from(
			&repo,
			&json!({
				"number": 7,
				"title": "Fix the widget",
				"body": "details",
				"state": "open",
				"draft": true,
				"merged": false,
				"base": {"ref": "main", "sha": "aaa"},
				"head": {"ref": "topic", "sha": "bbb"},
				"html_url": "https://github.com/acme/widget/pull/7"
			}),
			ForgeOp::ChangeRequestView,
		)
		.expect("open pull request");
		assert_eq!(open.number, 7);
		assert_eq!(open.title.as_str(), "Fix the widget");
		assert_eq!(open.body.as_str(), "details");
		assert_eq!(open.state, RequestState::Open);
		assert!(open.draft);
		assert_eq!(open.base_ref.as_str(), "main");
		assert_eq!(open.head_commit.as_str(), "bbb");

		let merged = super::change_request_from(
			&repo,
			&json!({"number": 8, "title": "Landed", "state": "closed", "merged": true}),
			ForgeOp::ChangeRequestView,
		)
		.expect("merged pull request");
		assert_eq!(merged.state, RequestState::Merged);
		assert!(merged.body.is_empty());

		let closed = super::change_request_from(
			&repo,
			&json!({"number": 9, "title": "Dropped", "state": "closed", "merged": false}),
			ForgeOp::ChangeRequestView,
		)
		.expect("closed pull request");
		assert_eq!(closed.state, RequestState::Closed);
	}

	#[test]
	fn issue_json_maps_onto_an_issue() {
		let repo = view_repo();
		let opened = super::issue_from(
			&repo,
			&json!({
				"number": 3,
				"title": "Bug",
				"body": "steps",
				"state": "open",
				"html_url": "https://github.com/acme/widget/issues/3"
			}),
		)
		.expect("issue");
		assert_eq!(opened.number, 3);
		assert_eq!(opened.title.as_str(), "Bug");
		assert_eq!(opened.body.as_str(), "steps");
		assert_eq!(opened.state, IssueState::Open);
		assert_eq!(opened.web_url.as_str(), "https://github.com/acme/widget/issues/3");

		let closed =
			super::issue_from(&repo, &json!({"number": 4, "title": "Done", "state": "closed"}))
				.expect("closed issue");
		assert_eq!(closed.state, IssueState::Closed);
		assert!(closed.body.is_empty());
	}

	#[test]
	fn comment_json_keeps_author_time_and_body() {
		let comment = super::comment_from(&json!({
			"user": {"login": "octocat"},
			"created_at": "2026-11-10T00:00:00Z",
			"body": "looks good"
		}));
		assert_eq!(comment.author.as_str(), "octocat");
		assert_eq!(comment.created_at.as_str(), "2026-11-10T00:00:00Z");
		assert_eq!(comment.body.as_str(), "looks good");
		let anonymous = super::comment_from(&json!({}));
		assert!(anonymous.author.is_empty());
		assert!(anonymous.body.is_empty());
	}

	#[test]
	fn missing_views_are_not_found_rather_than_unsupported() {
		let repo = view_repo();
		let missing =
			super::map_forge(ForgeOp::ChangeRequestView, &repo, Some(7), None, http_fault(404));
		assert!(matches!(missing, ForgeError::ChangeRequestNotFound { number: 7, .. }), "{missing}");
		let missing = super::map_forge(ForgeOp::IssueView, &repo, Some(3), None, http_fault(404));
		assert!(matches!(missing, ForgeError::IssueNotFound { number: 3, .. }), "{missing}");
		let rejected = super::map_forge(ForgeOp::IssueView, &repo, Some(3), None, http_fault(500));
		assert!(matches!(rejected, ForgeError::Rejected { status: 500, .. }), "{rejected}");
	}
}
