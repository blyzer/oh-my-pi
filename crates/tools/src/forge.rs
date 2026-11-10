//! Neutral forge tool (`forge@1`).
//!
//! The model names a [`ForgeOp`]. This tool fills an omitted repository from
//! the checkout, then calls a [`ForgeHost`]. GitHub I/O stays behind the
//! envd adapter. `fill`, reviewers, assignees, labels, search dates, and
//! batched checkouts are not part of this schema.

use std::{fmt::Write as _, sync::Arc};

use async_stream::stream;
use futures::Stream;
use omp_core::{Str, sf};
use omp_forge::{
	ChangeRequest, ChangeRequestDraft, CheckTarget, CheckoutReceipt, CheckoutRequest,
	ComponentError, FileBody, FileQuery, ForgeChecks, ForgeError, ForgeHost, ForgeOp, Issue,
	IssueDraft, PathComponent, PushReceipt, PushRequest, RepoRef, Repository, SearchKind,
	SearchPage, SearchQuery,
};
use omp_tool::{
	Abort, ArgIssue, ArgIssueKind, CommitError, Constraint, Effects, Ev, ExecEffects,
	IncomingParams, ParamError, Part, PromptCaps, RecordedCall, Rev, Tool, ToolSpec, ToolTerminal,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Arguments for one `forge@1` call.
#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Params {
	/// Contract operation.
	pub op:               ForgeOp,
	/// `host/owner/name`. Two segments borrow the host from the current
	/// checkout. Omitted on search and view, and on any other operation that
	/// needs a repository: the tool calls `current` first.
	pub repo:             Option<Str>,
	/// Repository-relative file path.
	pub path:             Option<Str>,
	/// Revision for a file read. A branch, a tag, or a commit.
	pub rev:              Option<Str>,
	/// Change-request or issue number.
	pub number:           Option<u64>,
	/// Search text. Empty lists the scope.
	pub query:            Option<Str>,
	/// Search page size. `0` and omitted let the host choose.
	pub limit:            Option<u32>,
	/// Title for a created change request or issue.
	pub title:            Option<Str>,
	/// Body for a created change request or issue.
	pub body:             Option<Str>,
	/// Base branch for a created change request.
	pub base:             Option<Str>,
	/// Head branch for a created change request, or the local branch to push.
	pub head:             Option<Str>,
	/// Create the change request as a draft.
	#[serde(default)]
	pub draft:            bool,
	/// Reset an existing local change-request branch.
	#[serde(default)]
	pub force:            bool,
	/// Refuse a push when the remote moved.
	#[serde(default)]
	pub force_with_lease: bool,
	/// Commit identity when `op` is `list_checks` and the target is a commit.
	pub commit:           Option<Str>,
}

/// Durable forge result.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Payload {
	/// Operation that completed.
	pub op:      ForgeOp,
	/// Contract value as JSON. File bytes stay in [`Self::output`].
	pub result:  Value,
	/// Human projection.
	pub output:  Str,
	/// Whether the operation produced nothing to act on.
	pub useless: bool,
}

/// Model-facing forge failure.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, thiserror::Error)]
#[error("{message}")]
pub struct Fault {
	/// Stable category.
	pub code:    Str,
	/// Rendered diagnostic.
	pub message: Str,
}

/// `forge@1` does not stream progress.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Update {}

/// Forge tool bound to one host router.
pub struct Forge<H> {
	host:    H,
	current: Arc<dyn Fn() -> Result<RepoRef, ForgeError> + Send + Sync>,
	spec:    ToolSpec,
}

/// Creates `forge@1`.
///
/// `current` fills an omitted repository before search and view, and before
/// any other operation whose contract value needs a [`RepoRef`].
pub fn tool<H: ForgeHost>(
	host: H,
	current: Arc<dyn Fn() -> Result<RepoRef, ForgeError> + Send + Sync>,
) -> Forge<H> {
	Forge {
		host,
		current,
		spec: ToolSpec {
			name:            sf!("forge"),
			rev:             Rev { family: Str::default(), n: 1 },
			description:     sf!(
				"Reads and writes a forge: repository metadata, files, change requests, issues, \
				 checks, and search. `repo` is host/owner/name; owner/name uses the current \
				 checkout's host. Omit `repo` on search and view to use that checkout. Creating, \
				 checking out, and pushing a change request mutate. Issue creation mutates."
			),
			schema:          omp_tool::schema::<Params>(),
			constraint:      Constraint::Schema {
				priority:       100,
				on_unsupported: omp_tool::Fallback::Unspecified,
			},
			effects:         Effects {
				documents: None,
				exec:      Some(ExecEffects { commands: Arc::from([sf!("git")]), network: true }),
				inference: None,
				desktop:   None,
				fetch:     None,
				subagents: 0,
			},
			confinement:     omp_tool::Confinement::Host,
			projection_code: omp_tool::native_projection_code(
				env!("CARGO_PKG_NAME"),
				env!("CARGO_PKG_VERSION"),
				include_bytes!("forge.rs"),
			)
			.into(),
		},
	}
}

impl<H: ForgeHost> Tool for Forge<H> {
	type Fault = Fault;
	type Params = Params;
	type Payload = Payload;
	type Update = Update;

	fn spec(&self) -> &ToolSpec {
		&self.spec
	}

	fn call<'c>(
		&'c self,
		mut incoming: IncomingParams<'c>,
	) -> impl Stream<Item = Ev<Update, Payload, Fault>> + Send + 'c {
		stream! {
			let params = match incoming.whole::<Params>().await {
				Ok(params) => params,
				Err(error) => {
					yield param_event(error);
					return;
				},
			};
			if let Err(error) = incoming.interruptable().committed().await {
				yield commit_event(error);
				return;
			}
			let execution = dispatch(&self.host, &self.current, params);
			tokio::pin!(execution);
			tokio::select! {
				result = &mut execution => {
					let useless = result.as_ref().is_ok_and(|payload| payload.useless);
					yield Ev::Done(ToolTerminal::Done { result, useless });
				},
				interrupt = incoming.next_interrupt() => {
					if let Ok(interrupt) = interrupt {
						yield Ev::Aborted(Abort::Interrupted { reason: interrupt.reason });
					} else {
						yield Ev::Aborted(Abort::InputDropped);
					}
				},
			}
		}
	}

	fn prompt(&self, view: Result<&Payload, &Fault>, _: &PromptCaps) -> Vec<Part> {
		let text = match view {
			Ok(payload) => payload.output.clone(),
			Err(fault) => fault.message.clone(),
		};
		vec![Part::Text { text }]
	}

	fn lift(&self, _: &Rev, _: RecordedCall<'_>) -> Option<omp_tool::LiftedCall> {
		None
	}
}

async fn dispatch<H: ForgeHost>(
	host: &H,
	current: &Arc<dyn Fn() -> Result<RepoRef, ForgeError> + Send + Sync>,
	params: Params,
) -> Result<Payload, Fault> {
	let op = params.op;
	let repo = resolve_repo(params.repo.as_deref(), current).map_err(host_fault)?;
	match op {
		ForgeOp::RepoView => {
			let repository = host.view_repo(&repo).await.map_err(host_fault)?;
			done(op, &repository, render_repo(&repository), false)
		},
		ForgeOp::FileRead => {
			let path = required(params.path.as_deref(), "file_read requires `path`")?;
			let rev = params
				.rev
				.as_deref()
				.map(str::trim)
				.filter(|rev| !rev.is_empty())
				.unwrap_or("HEAD");
			let query = FileQuery::try_new(repo, path, rev).map_err(|error| Fault {
				code:    sf!("invalid_path"),
				message: Str::new(error.to_string()),
			})?;
			let body = host.read_file(&query).await.map_err(host_fault)?;
			let output = render_file(&body);
			let result = serde_json::json!({
				"path": body.query.path.as_str(),
				"rev": body.query.rev.as_str(),
				"size": body.content.len(),
			});
			Ok(Payload { op, result, output: Str::new(output), useless: false })
		},
		ForgeOp::ChangeRequestView => {
			let number = required_number(params.number, "change_request_view requires `number`")?;
			let value = host
				.view_change_request(&repo, number)
				.await
				.map_err(host_fault)?;
			done(op, &value, render_change(&value), false)
		},
		ForgeOp::IssueView => {
			let number = required_number(params.number, "issue_view requires `number`")?;
			let value = host.view_issue(&repo, number).await.map_err(host_fault)?;
			done(op, &value, render_issue(&value), false)
		},
		ForgeOp::ListChecks => {
			let target = check_target(&params)?;
			let value = host.list_checks(&repo, &target).await.map_err(host_fault)?;
			done(op, &value, render_checks(&value), value.checks.is_empty())
		},
		ForgeOp::SearchChangeRequests
		| ForgeOp::SearchIssues
		| ForgeOp::SearchCode
		| ForgeOp::SearchCommits
		| ForgeOp::SearchRepos => {
			let kind = search_kind(op);
			let query = SearchQuery {
				kind,
				repo: Some(repo),
				text: params.query.clone().unwrap_or_else(Str::empty),
				limit: params.limit.unwrap_or(0),
			};
			let page = host.search(query).await.map_err(host_fault)?;
			let useless = page.hits.is_empty();
			done(op, &page, render_search(&page), useless)
		},
		ForgeOp::ChangeRequestCreate => {
			let title = required(params.title.as_deref(), "change_request_create requires `title`")?;
			let mut draft = ChangeRequestDraft::new(
				repo,
				title,
				params.base.unwrap_or_else(Str::empty),
				params.head.unwrap_or_else(Str::empty),
			);
			if let Some(body) = params.body {
				draft = draft.with_body(body);
			}
			draft = draft.with_draft(params.draft);
			let value = host
				.create_change_request(draft)
				.await
				.map_err(host_fault)?;
			done(op, &value, render_change(&value), false)
		},
		ForgeOp::ChangeRequestCheckout => {
			let number = required_number(params.number, "change_request_checkout requires `number`")?;
			let mut request = CheckoutRequest::new(repo, number);
			request.force = params.force;
			let value = host
				.checkout_change_request(&request)
				.await
				.map_err(host_fault)?;
			done(op, &value, render_checkout(&value), false)
		},
		ForgeOp::ChangeRequestPush => {
			let number = required_number(params.number, "change_request_push requires `number`")?;
			let request = PushRequest {
				repo,
				number,
				head_ref: params.head.clone().unwrap_or_else(Str::empty),
				force_with_lease: params.force_with_lease,
			};
			let value = host
				.push_change_request(&request)
				.await
				.map_err(host_fault)?;
			done(op, &value, render_push(&value), false)
		},
		ForgeOp::IssueCreate => {
			let title = required(params.title.as_deref(), "issue_create requires `title`")?;
			let mut draft = IssueDraft::new(repo, title);
			if let Some(body) = params.body {
				draft = draft.with_body(body);
			}
			let value = host.create_issue(draft).await.map_err(host_fault)?;
			done(op, &value, render_issue(&value), false)
		},
	}
}

fn resolve_repo(
	requested: Option<&str>,
	current: &Arc<dyn Fn() -> Result<RepoRef, ForgeError> + Send + Sync>,
) -> Result<RepoRef, ForgeError> {
	let Some(text) = requested.map(str::trim).filter(|text| !text.is_empty()) else {
		return current();
	};
	let parts = text
		.split('/')
		.filter(|part| !part.is_empty())
		.collect::<Vec<_>>();
	match parts.as_slice() {
		[owner, name] => {
			let host = current()?.host;
			RepoRef::try_new(host, *owner, *name).map_err(ForgeError::from)
		},
		[host, rest @ .., name] => {
			let owner = rest.join("/");
			RepoRef::try_new(*host, owner, *name).map_err(ForgeError::from)
		},
		_ => Err(ForgeError::Component(ComponentError::Invalid { component: PathComponent::Host })),
	}
}

const fn search_kind(op: ForgeOp) -> SearchKind {
	match op {
		ForgeOp::SearchChangeRequests => SearchKind::ChangeRequests,
		ForgeOp::SearchIssues => SearchKind::Issues,
		ForgeOp::SearchCode => SearchKind::Code,
		ForgeOp::SearchCommits => SearchKind::Commits,
		ForgeOp::SearchRepos => SearchKind::Repositories,
		ForgeOp::RepoView
		| ForgeOp::FileRead
		| ForgeOp::ChangeRequestView
		| ForgeOp::IssueView
		| ForgeOp::ListChecks
		| ForgeOp::ChangeRequestCreate
		| ForgeOp::ChangeRequestCheckout
		| ForgeOp::ChangeRequestPush
		| ForgeOp::IssueCreate => SearchKind::Issues,
	}
}

fn check_target(params: &Params) -> Result<CheckTarget, Fault> {
	if let Some(commit) = params
		.commit
		.as_deref()
		.map(str::trim)
		.filter(|commit| !commit.is_empty())
	{
		return Ok(CheckTarget::Commit { id: Str::new(commit) });
	}
	if let Some(number) = params.number {
		return Ok(CheckTarget::ChangeRequest { number });
	}
	Err(static_fault("missing_target", "list_checks requires `number` or `commit`"))
}

fn required(value: Option<&str>, message: &'static str) -> Result<Str, Fault> {
	value
		.map(str::trim)
		.filter(|value| !value.is_empty())
		.map(Str::new)
		.ok_or_else(|| static_fault("invalid_request", message))
}

fn required_number(value: Option<u64>, message: &'static str) -> Result<u64, Fault> {
	value
		.filter(|number| *number != 0)
		.ok_or_else(|| static_fault("invalid_request", message))
}

fn done(
	op: ForgeOp,
	value: &impl Serialize,
	output: String,
	useless: bool,
) -> Result<Payload, Fault> {
	let result = serde_json::to_value(value)
		.map_err(|_| static_fault("invalid_result", "forge result could not be encoded"))?;
	Ok(Payload { op, result, output: Str::new(output), useless })
}

fn render_repo(repository: &Repository) -> String {
	let mut output = format!("# {}", repository.repo);
	if !repository.description.is_empty() {
		let _ = write!(output, "\n\n{}", repository.description);
	}
	if !repository.default_branch.is_empty() {
		let _ = write!(output, "\nDefault branch: {}", repository.default_branch);
	}
	if !repository.web_url.is_empty() {
		let _ = write!(output, "\nURL: {}", repository.web_url);
	}
	output
}

fn render_file(body: &FileBody) -> String {
	match std::str::from_utf8(&body.content) {
		Ok(text) => text.to_owned(),
		Err(_) => format!("binary file {} ({} bytes)", body.query.path, body.content.len()),
	}
}

fn render_change(value: &ChangeRequest) -> String {
	format!(
		"# {} #{}\n{}\n{} -> {}\n{}",
		value.state, value.number, value.title, value.head_ref, value.base_ref, value.web_url
	)
}

fn render_issue(value: &Issue) -> String {
	format!("# {} #{}\n{}\n{}", value.state, value.number, value.title, value.web_url)
}

fn render_checks(value: &ForgeChecks) -> String {
	let mut output = format!("# checks ({})", value.checks.len());
	for check in value.checks.iter() {
		let _ = write!(output, "\n- {} {}", check.state, check.name);
	}
	output
}

fn render_search(page: &SearchPage) -> String {
	let mut output = format!("# {}\nResults: {}", page.kind, page.hits.len());
	for hit in page.as_slice() {
		match hit.number {
			Some(number) => {
				let _ = write!(output, "\n- #{number} {}", hit.title);
			},
			None => {
				let _ = write!(output, "\n- {}", hit.title);
			},
		}
		if !hit.web_url.is_empty() {
			let _ = write!(output, "\n  {}", hit.web_url);
		}
	}
	output
}

fn render_checkout(value: &CheckoutReceipt) -> String {
	format!("#{} {}\nPath: {}", value.number, value.head_ref, value.worktree)
}

fn render_push(value: &PushReceipt) -> String {
	format!("#{} {}", value.number, value.head_commit)
}

fn host_fault(error: ForgeError) -> Fault {
	let code = match &error {
		ForgeError::RepoNotFound { .. } => "repo_not_found",
		ForgeError::ChangeRequestNotFound { .. } => "change_request_not_found",
		ForgeError::IssueNotFound { .. } => "issue_not_found",
		ForgeError::CommitNotFound { .. } => "commit_not_found",
		ForgeError::FileNotFound { .. } => "file_not_found",
		ForgeError::Unsupported { .. } => "unsupported",
		ForgeError::Rejected { .. } => "rejected",
		ForgeError::Unavailable { .. } => "unavailable",
		ForgeError::Unregistered { .. } => "unregistered",
		ForgeError::NotApproved { .. } => "not_approved",
		ForgeError::Unscoped { .. } => "unscoped",
		ForgeError::Scheme(_) => "scheme",
		ForgeError::Component(_) => "component",
	};
	Fault { code: Str::new_static(code), message: Str::new(error.to_string()) }
}

const fn static_fault(code: &'static str, message: &'static str) -> Fault {
	Fault { code: Str::new_static(code), message: Str::new_static(message) }
}

fn param_event(error: ParamError) -> Ev<Update, Payload, Fault> {
	match error {
		ParamError::Args(issue) => Ev::Args(*issue),
		ParamError::Interrupted(interrupt) => {
			Ev::Aborted(Abort::Interrupted { reason: interrupt.reason })
		},
		ParamError::Protocol { violation: message } => Ev::Args(protocol_issue(message)),
	}
}

fn commit_event(error: CommitError) -> Ev<Update, Payload, Fault> {
	match error {
		CommitError::Aborted => Ev::Aborted(Abort::InputDropped),
		CommitError::Interrupted(interrupt) => {
			Ev::Aborted(Abort::Interrupted { reason: interrupt.reason })
		},
		CommitError::Protocol { violation: message } => Ev::Args(protocol_issue(message)),
	}
}

fn protocol_issue(message: Str) -> ArgIssue {
	ArgIssue {
		path:     Vec::new(),
		expected: sf!("one committed forge argument object"),
		kind:     ArgIssueKind::Protocol,
		example:  None,
		found:    Some(message),
	}
}

#[cfg(test)]
mod tests {
	use std::{future::Future, sync::Arc};

	use omp_core::Str;
	use omp_forge::{
		ChangeRequest, ChangeRequestDraft, CheckTarget, CheckoutReceipt, CheckoutRequest, FileBody,
		FileQuery, ForgeChecks, ForgeError, ForgeHost, ForgeOp, Issue, IssueDraft, PushReceipt,
		PushRequest, RepoRef, Repository, SearchKind, SearchPage, SearchQuery,
	};
	use omp_tool::{Rev, Tool as _};
	use parking_lot::Mutex;

	use super::{Params, dispatch, tool};

	fn repo() -> RepoRef {
		RepoRef::try_new("code.example", "acme", "widget").unwrap()
	}

	#[derive(Clone)]
	struct Recording {
		seen: Arc<Mutex<Option<RepoRef>>>,
	}

	impl ForgeHost for Recording {
		fn view_repo(
			&self,
			repo: &RepoRef,
		) -> impl Future<Output = Result<Repository, ForgeError>> + Send {
			*self.seen.lock() = Some(repo.clone());
			let repo = repo.clone();
			async move {
				Ok(Repository {
					repo,
					default_branch: Str::new_static("main"),
					clone_url: Str::empty(),
					web_url: Str::empty(),
					description: Str::empty(),
				})
			}
		}

		fn read_file(
			&self,
			_: &FileQuery,
		) -> impl Future<Output = Result<FileBody, ForgeError>> + Send {
			async { Err(ForgeError::Unsupported { op: ForgeOp::FileRead }) }
		}

		fn view_change_request(
			&self,
			_: &RepoRef,
			_: u64,
		) -> impl Future<Output = Result<ChangeRequest, ForgeError>> + Send {
			async { Err(ForgeError::Unsupported { op: ForgeOp::ChangeRequestView }) }
		}

		fn view_issue(
			&self,
			_: &RepoRef,
			_: u64,
		) -> impl Future<Output = Result<Issue, ForgeError>> + Send {
			async { Err(ForgeError::Unsupported { op: ForgeOp::IssueView }) }
		}

		fn list_checks(
			&self,
			_: &RepoRef,
			_: &CheckTarget,
		) -> impl Future<Output = Result<ForgeChecks, ForgeError>> + Send {
			async { Err(ForgeError::Unsupported { op: ForgeOp::ListChecks }) }
		}

		fn search(
			&self,
			query: SearchQuery,
		) -> impl Future<Output = Result<SearchPage, ForgeError>> + Send {
			*self.seen.lock() = query.repo.clone();
			let kind = query.kind;
			async move { Ok(SearchPage::new(kind, [])) }
		}

		fn create_change_request(
			&self,
			_: ChangeRequestDraft,
		) -> impl Future<Output = Result<ChangeRequest, ForgeError>> + Send {
			async { Err(ForgeError::Unsupported { op: ForgeOp::ChangeRequestCreate }) }
		}

		fn checkout_change_request(
			&self,
			_: &CheckoutRequest,
		) -> impl Future<Output = Result<CheckoutReceipt, ForgeError>> + Send {
			async { Err(ForgeError::Unsupported { op: ForgeOp::ChangeRequestCheckout }) }
		}

		fn push_change_request(
			&self,
			_: &PushRequest,
		) -> impl Future<Output = Result<PushReceipt, ForgeError>> + Send {
			async { Err(ForgeError::Unsupported { op: ForgeOp::ChangeRequestPush }) }
		}

		fn create_issue(
			&self,
			_: IssueDraft,
		) -> impl Future<Output = Result<Issue, ForgeError>> + Send {
			async { Err(ForgeError::Unsupported { op: ForgeOp::IssueCreate }) }
		}
	}

	#[test]
	fn revision_one_schema_names_the_contract_fields() {
		let built = tool(Recording { seen: Arc::new(Mutex::new(None)) }, Arc::new(|| Ok(repo())));
		assert_eq!(built.spec().rev, Rev { family: Str::default(), n: 1 });
		assert_eq!(built.spec().name.as_str(), "forge");
		assert!(built.spec().effects.exec.as_ref().is_some_and(|exec| {
			exec.network
				&& exec
					.commands
					.iter()
					.any(|command| command.as_str() == "git")
		}));
		let schema: serde_json::Value =
			serde_json::from_slice(&built.spec().schema).expect("forge schema is JSON");
		let properties = schema["properties"].as_object().expect("object properties");
		let mut names = properties
			.keys()
			.filter(|name| !matches!(name.as_str(), "i" | "notrunc"))
			.map(String::as_str)
			.collect::<Vec<_>>();
		names.sort_unstable();
		assert_eq!(names, [
			"base",
			"body",
			"commit",
			"draft",
			"force",
			"forceWithLease",
			"head",
			"limit",
			"number",
			"op",
			"path",
			"query",
			"repo",
			"rev",
			"title",
		]);
		let op = schema["properties"]["op"].to_string();
		assert!(op.contains("change_request_view"), "{op}");
		assert!(op.contains("issue_create"), "{op}");
		assert!(op.contains("search_change_requests"), "{op}");
		assert!(!op.contains("run_watch"), "{op}");
		assert!(!op.contains("fill"), "{op}");
	}

	#[tokio::test]
	async fn omitted_repo_is_filled_before_search_and_view() {
		let seen = Arc::new(Mutex::new(None));
		let host = Recording { seen: Arc::clone(&seen) };
		let current: Arc<dyn Fn() -> Result<RepoRef, ForgeError> + Send + Sync> =
			Arc::new(|| Ok(repo()));
		let params = Params {
			op:               ForgeOp::SearchIssues,
			repo:             None,
			path:             None,
			rev:              None,
			number:           None,
			query:            Some(Str::new_static("bug")),
			limit:            None,
			title:            None,
			body:             None,
			base:             None,
			head:             None,
			draft:            false,
			force:            false,
			force_with_lease: false,
			commit:           None,
		};
		let payload = dispatch(&host, &current, params).await.expect("search");
		assert!(payload.useless);
		assert_eq!(seen.lock().as_ref(), Some(&repo()));

		let params = Params {
			op:               ForgeOp::RepoView,
			repo:             None,
			path:             None,
			rev:              None,
			number:           None,
			query:            None,
			limit:            None,
			title:            None,
			body:             None,
			base:             None,
			head:             None,
			draft:            false,
			force:            false,
			force_with_lease: false,
			commit:           None,
		};
		dispatch(&host, &current, params).await.expect("view");
		assert_eq!(seen.lock().as_ref(), Some(&repo()));
	}

	#[test]
	fn two_segment_repo_borrows_the_current_host() {
		let parsed: Params = serde_json::from_value(serde_json::json!({
			"op": "repo_view",
			"repo": "acme/widget",
		}))
		.expect("params");
		let current: Arc<dyn Fn() -> Result<RepoRef, ForgeError> + Send + Sync> =
			Arc::new(|| Ok(repo()));
		let resolved = super::resolve_repo(parsed.repo.as_deref(), &current).unwrap();
		assert_eq!(resolved.host.as_str(), "code.example");
		assert_eq!(resolved.owner.as_str(), "acme");
		assert_eq!(resolved.name.as_str(), "widget");
	}

	#[test]
	fn search_kind_is_unused_for_non_search_ops() {
		assert_eq!(super::search_kind(ForgeOp::SearchCode), SearchKind::Code);
	}
}
