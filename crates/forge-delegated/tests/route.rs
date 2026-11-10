//! Two in-memory hosts prove host routing and the write gate.

use std::{future::Future, sync::Arc};

use bytes::Bytes;
use futures::executor::block_on;
use omp_core::Str;
use omp_forge::{
	ChangeRequest, ChangeRequestDraft, CheckTarget, CheckoutReceipt, CheckoutRequest, CommitRef,
	ComponentError, FileBody, FileQuery, ForgeChecks, ForgeError, ForgeHost, ForgeOp, Issue,
	IssueDraft, IssueState, PathComponent, PushReceipt, PushRequest, RepoRef, Repository,
	RequestState, SearchKind, SearchPage, SearchQuery,
};
use omp_forge_delegated::{
	ApprovalGate, Delegated, DelegatedBuilder, DenyAll, PermitAll, RegisterError,
};
use parking_lot::Mutex;

#[derive(Clone, Copy)]
struct Marker;

#[derive(Clone)]
struct Memory {
	label: &'static str,
	ops:   Arc<Mutex<Vec<ForgeOp>>>,
}

impl Memory {
	fn new(label: &'static str) -> Self {
		Self { label, ops: Arc::new(Mutex::new(Vec::new())) }
	}

	fn note(&self, op: ForgeOp) {
		self.ops.lock().push(op);
	}

	fn ops(&self) -> Vec<ForgeOp> {
		self.ops.lock().clone()
	}
}

struct Seen {
	op:   ForgeOp,
	host: Str,
}

struct RecordGate {
	seen:  Arc<Mutex<Vec<Seen>>>,
	allow: bool,
}

impl RecordGate {
	fn new(allow: bool) -> (Self, Arc<Mutex<Vec<Seen>>>) {
		let seen = Arc::new(Mutex::new(Vec::new()));
		(Self { seen: Arc::clone(&seen), allow }, seen)
	}
}

impl ApprovalGate for RecordGate {
	fn permit(&self, op: ForgeOp, repo: &RepoRef) -> bool {
		self.seen.lock().push(Seen { op, host: repo.host.clone() });
		self.allow
	}
}

fn repo(host: &str) -> RepoRef {
	RepoRef::try_new(host, "acme", "widget").unwrap()
}

fn change(repo: RepoRef, number: u64, title: &str) -> ChangeRequest {
	ChangeRequest {
		repo,
		number,
		title: Str::new(title),
		body: Str::empty(),
		state: RequestState::Open,
		draft: false,
		base_ref: Str::new_static("main"),
		head_ref: Str::new_static("topic"),
		head_commit: Str::new_static("abc123"),
		base_commit: Str::new_static("def456"),
		web_url: Str::empty(),
	}
}

fn issue(repo: RepoRef, number: u64, title: &str) -> Issue {
	Issue {
		repo,
		number,
		title: Str::new(title),
		body: Str::empty(),
		state: IssueState::Open,
		web_url: Str::empty(),
	}
}

impl ForgeHost for Memory {
	fn view_repo(
		&self,
		repo: &RepoRef,
	) -> impl Future<Output = Result<Repository, ForgeError>> + Send {
		self.note(ForgeOp::RepoView);
		let repo = repo.clone();
		let label = self.label;
		async move {
			Ok(Repository {
				repo,
				default_branch: Str::new_static("main"),
				clone_url: Str::empty(),
				web_url: Str::empty(),
				description: Str::new(label),
			})
		}
	}

	fn read_file(
		&self,
		query: &FileQuery,
	) -> impl Future<Output = Result<FileBody, ForgeError>> + Send {
		self.note(ForgeOp::FileRead);
		let query = query.clone();
		let label = self.label;
		async move { Ok(FileBody { query, content: Bytes::from_static(label.as_bytes()) }) }
	}

	fn view_change_request(
		&self,
		repo: &RepoRef,
		number: u64,
	) -> impl Future<Output = Result<ChangeRequest, ForgeError>> + Send {
		self.note(ForgeOp::ChangeRequestView);
		let repo = repo.clone();
		let label = self.label;
		async move { Ok(change(repo, number, label)) }
	}

	fn view_issue(
		&self,
		repo: &RepoRef,
		number: u64,
	) -> impl Future<Output = Result<Issue, ForgeError>> + Send {
		self.note(ForgeOp::IssueView);
		let repo = repo.clone();
		let label = self.label;
		async move { Ok(issue(repo, number, label)) }
	}

	fn list_checks(
		&self,
		repo: &RepoRef,
		target: &CheckTarget,
	) -> impl Future<Output = Result<ForgeChecks, ForgeError>> + Send {
		self.note(ForgeOp::ListChecks);
		let _ = repo;
		let target = target.clone();
		async move { Ok(ForgeChecks::new(target, [])) }
	}

	fn search(
		&self,
		query: SearchQuery,
	) -> impl Future<Output = Result<SearchPage, ForgeError>> + Send {
		self.note(query.kind.op());
		async move { Ok(SearchPage::new(query.kind, [])) }
	}

	fn create_change_request(
		&self,
		draft: ChangeRequestDraft,
	) -> impl Future<Output = Result<ChangeRequest, ForgeError>> + Send {
		self.note(ForgeOp::ChangeRequestCreate);
		let label = self.label;
		async move { Ok(change(draft.repo, 9, label)) }
	}

	fn checkout_change_request(
		&self,
		request: &CheckoutRequest,
	) -> impl Future<Output = Result<CheckoutReceipt, ForgeError>> + Send {
		self.note(ForgeOp::ChangeRequestCheckout);
		let request = request.clone();
		let label = self.label;
		async move {
			Ok(CheckoutReceipt {
				head:     CommitRef::try_new(request.repo.clone(), "abc123").unwrap(),
				repo:     request.repo,
				number:   request.number,
				base_ref: Str::new_static("main"),
				head_ref: Str::new_static("topic"),
				worktree: Str::new(label),
			})
		}
	}

	fn push_change_request(
		&self,
		request: &PushRequest,
	) -> impl Future<Output = Result<PushReceipt, ForgeError>> + Send {
		self.note(ForgeOp::ChangeRequestPush);
		let request = request.clone();
		let label = self.label;
		async move {
			Ok(PushReceipt {
				repo:        request.repo,
				number:      request.number,
				head_commit: Str::new(label),
			})
		}
	}

	fn create_issue(
		&self,
		draft: IssueDraft,
	) -> impl Future<Output = Result<Issue, ForgeError>> + Send {
		self.note(ForgeOp::IssueCreate);
		let label = self.label;
		async move { Ok(issue(draft.repo, 4, label)) }
	}
}

fn router_with<G: ApprovalGate>(gate: G) -> (Delegated<Memory, G>, Memory, Memory) {
	let left = Memory::new("a");
	let right = Memory::new("b");
	let mut builder = Delegated::builder(gate);
	builder.register("forge-a.example", left.clone()).unwrap();
	builder.register("forge-b.example", right.clone()).unwrap();
	(builder.build(), left, right)
}

fn file_query(repo: &RepoRef) -> FileQuery {
	FileQuery::try_new(repo.clone(), "src/lib.rs", "main").unwrap()
}

fn search(repo: &RepoRef) -> SearchQuery {
	SearchQuery {
		kind:  SearchKind::Code,
		repo:  Some(repo.clone()),
		text:  Str::new_static("widget"),
		limit: 0,
	}
}

fn draft(repo: &RepoRef) -> ChangeRequestDraft {
	ChangeRequestDraft::new(repo.clone(), "title", "main", "topic")
}

fn push(repo: &RepoRef) -> PushRequest {
	PushRequest {
		repo:             repo.clone(),
		number:           7,
		head_ref:         Str::new_static("topic"),
		force_with_lease: true,
	}
}

const READS: [ForgeOp; 6] = [
	ForgeOp::RepoView,
	ForgeOp::FileRead,
	ForgeOp::ChangeRequestView,
	ForgeOp::IssueView,
	ForgeOp::ListChecks,
	ForgeOp::SearchCode,
];

const WRITES: [ForgeOp; 4] = [
	ForgeOp::ChangeRequestCreate,
	ForgeOp::ChangeRequestCheckout,
	ForgeOp::ChangeRequestPush,
	ForgeOp::IssueCreate,
];

fn read_all(router: &impl ForgeHost, repo: &RepoRef, label: &str) {
	let viewed = block_on(router.view_repo(repo)).unwrap();
	assert_eq!(viewed.description.as_str(), label);
	let body = block_on(router.read_file(&file_query(repo))).unwrap();
	assert_eq!(body.content.as_ref(), label.as_bytes());
	let change_request = block_on(router.view_change_request(repo, 7)).unwrap();
	assert_eq!(change_request.title.as_str(), label);
	let opened = block_on(router.view_issue(repo, 3)).unwrap();
	assert_eq!(opened.title.as_str(), label);
	let checks =
		block_on(router.list_checks(repo, &CheckTarget::ChangeRequest { number: 7 })).unwrap();
	assert!(checks.as_slice().is_empty());
	let page = block_on(router.search(search(repo))).unwrap();
	assert_eq!(page.kind, SearchKind::Code);
	assert!(page.as_slice().is_empty());
}

fn write_all(router: &impl ForgeHost, repo: &RepoRef, label: &str) {
	let created = block_on(router.create_change_request(draft(repo))).unwrap();
	assert_eq!(created.title.as_str(), label);
	let checked =
		block_on(router.checkout_change_request(&CheckoutRequest::new(repo.clone(), 7))).unwrap();
	assert_eq!(checked.worktree.as_str(), label);
	let pushed = block_on(router.push_change_request(&push(repo))).unwrap();
	assert_eq!(pushed.head_commit.as_str(), label);
	let opened = block_on(router.create_issue(IssueDraft::new(repo.clone(), "title"))).unwrap();
	assert_eq!(opened.title.as_str(), label);
}

#[test]
fn requests_for_one_host_do_not_reach_the_other() {
	let (router, left, right) = router_with(PermitAll);
	let a = repo("forge-a.example");
	let b = repo("forge-b.example");

	read_all(&router, &a, "a");
	write_all(&router, &a, "a");
	assert_eq!(left.ops(), [READS.as_slice(), WRITES.as_slice()].concat());
	assert!(right.ops().is_empty());

	read_all(&router, &b, "b");
	write_all(&router, &b, "b");
	assert_eq!(left.ops().len(), READS.len() + WRITES.len());
	assert_eq!(right.ops(), [READS.as_slice(), WRITES.as_slice()].concat());
}

#[test]
fn unknown_host_names_the_method_operation_and_skips_the_gate() {
	let (gate, seen) = RecordGate::new(true);
	let (router, left, right) = router_with(gate);
	let missing = repo("Missing.Example");

	let cases = [
		block_on(router.view_repo(&missing)).unwrap_err(),
		block_on(router.read_file(&file_query(&missing))).unwrap_err(),
		block_on(router.view_change_request(&missing, 1)).unwrap_err(),
		block_on(router.view_issue(&missing, 1)).unwrap_err(),
		block_on(router.list_checks(&missing, &CheckTarget::Commit { id: Str::new_static("abc") }))
			.unwrap_err(),
		block_on(router.search(search(&missing))).unwrap_err(),
		block_on(router.create_change_request(draft(&missing))).unwrap_err(),
		block_on(router.checkout_change_request(&CheckoutRequest::new(missing.clone(), 1)))
			.unwrap_err(),
		block_on(router.push_change_request(&push(&missing))).unwrap_err(),
		block_on(router.create_issue(IssueDraft::new(missing, "title"))).unwrap_err(),
	];
	let mut ops = Vec::new();
	for err in cases {
		match err {
			ForgeError::Unregistered { host, op } => {
				assert_eq!(host.as_str(), "missing.example");
				ops.push(op);
			},
			other => panic!("unexpected {other:?}"),
		}
	}
	assert_eq!(ops, [READS.as_slice(), WRITES.as_slice()].concat());
	assert!(seen.lock().is_empty());
	assert!(left.ops().is_empty());
	assert!(right.ops().is_empty());
}

#[test]
fn writes_hit_the_gate_with_the_method_operation_and_reads_do_not() {
	let (gate, seen) = RecordGate::new(true);
	let (router, left, right) = router_with(gate);
	let a = repo("Forge-A.Example");

	read_all(&router, &a, "a");
	assert!(seen.lock().is_empty(), "reads must not ask the gate");
	assert_eq!(left.ops(), READS);
	assert!(right.ops().is_empty());

	write_all(&router, &a, "a");
	let recorded: Vec<_> = seen
		.lock()
		.iter()
		.map(|hit| (hit.op, hit.host.clone()))
		.collect();
	assert_eq!(
		recorded,
		WRITES
			.into_iter()
			.map(|op| (op, Str::new("Forge-A.Example")))
			.collect::<Vec<_>>()
	);
	assert_eq!(left.ops(), [READS.as_slice(), WRITES.as_slice()].concat());
	assert!(right.ops().is_empty());
}

#[test]
fn a_refused_write_does_not_reach_the_adapter() {
	let (router, left, right) = router_with(DenyAll);
	let a = repo("forge-a.example");

	read_all(&router, &a, "a");
	assert_eq!(left.ops(), READS);

	let refused = [
		block_on(router.create_change_request(draft(&a))).unwrap_err(),
		block_on(router.checkout_change_request(&CheckoutRequest::new(a.clone(), 7))).unwrap_err(),
		block_on(router.push_change_request(&push(&a))).unwrap_err(),
		block_on(router.create_issue(IssueDraft::new(a, "title"))).unwrap_err(),
	];
	for (err, op) in refused.into_iter().zip(WRITES) {
		match err {
			ForgeError::NotApproved { host, op: got } => {
				assert_eq!(host.as_str(), "forge-a.example");
				assert_eq!(got, op);
			},
			other => panic!("unexpected {other:?}"),
		}
	}
	assert_eq!(left.ops(), READS, "refused writes must not reach the adapter");
	assert!(right.ops().is_empty());
}

#[test]
fn search_without_a_repository_is_unscoped() {
	let (gate, seen) = RecordGate::new(true);
	let (router, left, right) = router_with(gate);
	let query =
		SearchQuery { kind: SearchKind::Issues, repo: None, text: Str::empty(), limit: 0 };
	match block_on(router.search(query)).unwrap_err() {
		ForgeError::Unscoped { op } => assert_eq!(op, ForgeOp::SearchIssues),
		other => panic!("unexpected {other:?}"),
	}
	assert!(seen.lock().is_empty());
	assert!(left.ops().is_empty());
	assert!(right.ops().is_empty());
}

#[test]
fn host_match_folds_ascii_case_and_keeps_a_port() {
	let plain = Memory::new("plain");
	let port = Memory::new("port");
	let mut builder: DelegatedBuilder<Memory, PermitAll> = Delegated::builder(PermitAll);
	builder.register("forge-a.example", plain.clone()).unwrap();
	builder
		.register("forge-a.example:443", port.clone())
		.unwrap();
	let router = builder.build();

	let folded = block_on(router.view_repo(&repo("FORGE-A.EXAMPLE"))).unwrap();
	assert_eq!(folded.description.as_str(), "plain");
	let with_port = block_on(router.view_repo(&repo("forge-a.example:443"))).unwrap();
	assert_eq!(with_port.description.as_str(), "port");
	assert_eq!(plain.ops(), [ForgeOp::RepoView]);
	assert_eq!(port.ops(), [ForgeOp::RepoView]);
}

#[test]
fn duplicate_and_invalid_hosts_are_rejected() {
	let mut builder: DelegatedBuilder<Marker, PermitAll> = Delegated::builder(PermitAll);
	builder.register("Forge-A.Example", Marker).unwrap();
	match builder.register("forge-a.example", Marker).unwrap_err() {
		RegisterError::Duplicate { host } => assert_eq!(host.as_str(), "forge-a.example"),
		other => panic!("unexpected {other:?}"),
	}
	match builder.register("", Marker).unwrap_err() {
		RegisterError::Component(ComponentError::Empty { component }) => {
			assert_eq!(component, PathComponent::Host);
		},
		other => panic!("unexpected {other:?}"),
	}
	match builder.register("bad host", Marker).unwrap_err() {
		RegisterError::Component(ComponentError::Invalid { component }) => {
			assert_eq!(component, PathComponent::Host);
		},
		other => panic!("unexpected {other:?}"),
	}
	let _ = builder.build();
}
