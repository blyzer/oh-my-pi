//! The kernel-bound approval route: a policy prompt filed while a tool runs
//! is journaled under `<queues><prompts>`, surfaced as
//! `KernelEvent::ApprovalRequested`, and answered by `Up::Approve` — the
//! decision reaches the waiting policy only after the journal recorded it.

use std::{
	mem,
	sync::{
		Arc,
		atomic::{AtomicUsize, Ordering},
	},
	time::Duration,
};

use async_stream::stream;
use bytes::Bytes;
use futures::Stream;
use omp_agent::{
	ApprovalDecision, ApprovalRoute, ApprovalScope, ApprovalSource, ApprovalSpec, DispatchPolicy,
	Kernel, KernelEvent, RunControl, StaticPrompt, TicketState, TurnInput, Up,
};
use omp_core::{Str, sf};
use omp_journal::blob::BlobStore;
use omp_session::Session;
use omp_tool::{
	Claims, Constraint, Effects, Ev, IncomingParams, Part, Precedence, Presentation, PromptCaps,
	Registry, Rev, Tool, ToolSpec, ToolTerminal,
};
use parking_lot::Mutex;
use serde_json::Value;

mod support;

use support::{ScriptedInference, fresh_session, text_script, tool_script};

/// A tool that asks the session's approval authority before acting, exactly
/// as the environment executor does for an admission query.
struct GatedTool {
	spec:  ToolSpec,
	route: Arc<Mutex<Option<ApprovalRoute>>>,
}

/// The requirement the gated tool files for `subject` of `kind`, offering
/// `offered`.
fn spec(kind: &str, subject: &str, offered: Vec<Str>) -> ApprovalSpec {
	ApprovalSpec {
		title:         sf!("Run bash"),
		body:          sf!("$ {subject}"),
		subject:       Str::new(subject),
		kind:          Str::new(kind),
		scopes:        offered,
		default:       Some(false),
		route:         sf!("user"),
		approver:      None,
		timeout_ms:    0,
		unreachable:   sf!("deny"),
		require_human: true,
		pattern:       None,
		evidence:      Vec::new(),
	}
}

impl Tool for GatedTool {
	type Fault = Value;
	type Params = Value;
	type Payload = Value;
	type Update = Value;

	fn spec(&self) -> &ToolSpec {
		&self.spec
	}

	fn call<'c>(
		&'c self,
		mut params: IncomingParams<'c>,
	) -> impl Stream<Item = Ev<Self::Update, Self::Payload, Self::Fault>> + Send + 'c {
		stream! {
			let args = params.whole::<Value>().await.expect("args");
			let command = args["command"].as_str().unwrap_or("").to_owned();
			// A call names the scopes its prompt offers; by default `once` and
			// `session`, like a tool admission prompt.
			let scopes = args["scopes"].as_array().map_or_else(
				|| vec![sf!("once"), sf!("session")],
				|scopes| scopes.iter().filter_map(Value::as_str).map(Str::new).collect(),
			);
			// `exec`, like a tool admission prompt, unless the call names another.
			let kind = args["kind"].as_str().unwrap_or("exec").to_owned();
			let route = self.route.lock().clone().expect("route bound before the turn");
			let ticket =
				route.request(Some(sf!("gated-1")), vec![spec(&kind, &command, scopes)], 1).await;
			let decision = ticket.decision.expect("route returns a decided ticket");
			if decision.approved {
				yield Ev::Done(ToolTerminal::Done {
					result: Ok(serde_json::json!({"ran": command, "scope": decision.scope.as_str()})),
					useless: false,
				});
			} else {
				yield Ev::Done(ToolTerminal::Done {
					result: Err(serde_json::json!({"denied": decision.reason})),
					useless: false,
				});
			}
		}
	}

	fn prompt(&self, view: Result<&Value, &Value>, _: &PromptCaps) -> Vec<Part> {
		vec![Part::Json {
			json: Bytes::from(serde_json::to_vec(view.unwrap_or_else(|fault| fault)).expect("JSON")),
		}]
	}
}

fn gated_registry(route: Arc<Mutex<Option<ApprovalRoute>>>) -> Arc<Registry> {
	let mut registry = Registry::new();
	registry
		.register(
			GatedTool {
				spec: ToolSpec {
					name: sf!("gated"),
					rev: Rev { family: sf!("test"), n: 1 },
					description: sf!("asks before acting"),
					schema: Bytes::from_static(
						br#"{"type":"object","properties":{"command":{"type":"string"},"kind":{"type":"string"},"scopes":{"type":"array","items":{"type":"string"}}},"required":["command"],"additionalProperties":false}"#,
					),
					constraint: Constraint::None,
					effects: Effects::empty(),
					confinement: omp_tool::Confinement::Host,
					projection_code: [9; 32],
				},
				route,
			},
			Presentation::Slot,
			Claims { precedence: Precedence::CORE, claimant: sf!("omp/core"), replaces: None },
		)
		.expect("gated tool registers");
	Arc::new(registry)
}

fn decision(approved: bool, scope: ApprovalScope) -> ApprovalDecision {
	ApprovalDecision {
		approved,
		scope,
		source: ApprovalSource::User,
		decided_by: None,
		reason: (!approved).then(|| sf!("denied by user")),
		audited: false,
	}
}

fn prompts(session: &Session) -> Vec<omp_agent::ApprovalTicket> {
	let dom = session.dom();
	let prompts = omp_session::components::prompts::prompts_handle(dom).expect("prompts");
	dom.children(prompts)
		.iter()
		.filter_map(|handle| dom.get(*handle))
		.filter_map(|node| {
			node
				.prop(&omp_dom::PropKey::Custom(Str::new_static("ticket")))
				.and_then(omp_dom::Value::as_str)
		})
		.map(|encoded| serde_json::from_str(encoded).expect("ticket JSON"))
		.collect()
}

/// Every projected tool-result part (text or JSON) as a string.
fn results(session: &Session) -> Vec<String> {
	use omp_proto::thread::v1::{item, part};
	omp_session::project_thread(session.dom())
		.into_iter()
		.filter_map(|item| match item.kind? {
			item::Kind::ToolResult(result) => Some(result.parts),
			_ => None,
		})
		.flatten()
		.filter_map(|part| match part.kind? {
			part::Kind::Text(text) => Some(text),
			part::Kind::Blob(blob) => Some(String::from_utf8_lossy(&blob.inline).into_owned()),
			_ => None,
		})
		.collect()
}

struct Harness {
	kernel:  Kernel<ScriptedInference>,
	session: Session,
	events:  flume::Receiver<KernelEvent>,
	_temp:   tempfile::TempDir,
}

fn harness(scripts: Vec<Vec<omp_ai::ChatEvent>>) -> Harness {
	let temp = tempfile::tempdir().expect("tempdir");
	let session = fresh_session(&temp.path().join("approvals.oms"));
	harness_in(temp, session, scripts, |kernel| kernel)
}

/// A harness over `session`, whose journal lives in `temp`; `compose`
/// finishes the kernel.
fn harness_in(
	temp: tempfile::TempDir,
	session: Session,
	scripts: Vec<Vec<omp_ai::ChatEvent>>,
	compose: impl FnOnce(Kernel<ScriptedInference>) -> Kernel<ScriptedInference>,
) -> Harness {
	let route = Arc::new(Mutex::new(None));
	let (inference, _) = ScriptedInference::new(scripts);
	let mut kernel = compose(Kernel::new(
		inference,
		gated_registry(Arc::clone(&route)),
		DispatchPolicy::new(BlobStore::open(temp.path().join("blobs")).expect("blobs")),
		StaticPrompt(sf!("system")),
	));
	*route.lock() = Some(kernel.approval_route());
	let events = kernel.subscribe();
	Harness { kernel, session, events, _temp: temp }
}

/// Runs one turn while `answer` decides every filed prompt.
async fn run(
	harness: &mut Harness,
	answer: impl Fn(&omp_agent::ApprovalTicket) -> Option<ApprovalDecision> + Send + 'static,
) -> Vec<omp_agent::ApprovalTicket> {
	let mailbox = harness.kernel.mailbox();
	let events = harness.events.clone();
	let seen = Arc::new(Mutex::new(Vec::new()));
	let host = {
		let seen = Arc::clone(&seen);
		tokio::spawn(async move {
			while let Ok(event) = events.recv_async().await {
				if let KernelEvent::ApprovalRequested(ticket) = event {
					seen.lock().push(ticket.clone());
					if let Some(decision) = answer(&ticket) {
						let _ = mailbox.send(Up::Approve { id: ticket.ticket_id.clone(), decision });
					}
				}
			}
		})
	};
	tokio::time::timeout(
		Duration::from_secs(10),
		harness.kernel.run_turn(
			&mut harness.session,
			TurnInput { text: sf!("go"), attachments: Vec::new() },
			RunControl::default(),
		),
	)
	.await
	.expect("turn settles")
	.expect("turn");
	host.abort();
	let seen = seen.lock().clone();
	seen
}

#[tokio::test]
async fn allow_journals_the_decision_and_the_tool_runs() {
	let mut harness = harness(vec![
		tool_script("gated-1", "gated", serde_json::json!({"command": "make build"})),
		text_script("done"),
	]);
	let seen = run(&mut harness, |_| Some(decision(true, ApprovalScope::Once))).await;
	assert_eq!(seen.len(), 1);
	assert_eq!(seen[0].invocation_id.as_deref(), Some("gated-1"));
	assert_eq!(seen[0].reasons[0].subject.as_str(), "make build");
	let journaled = prompts(&harness.session);
	assert_eq!(journaled.len(), 1);
	assert_eq!(journaled[0].ticket_id, seen[0].ticket_id);
	assert_eq!(journaled[0].state, TicketState::Decided);
	assert!(
		journaled[0]
			.decision
			.as_ref()
			.is_some_and(|decision| decision.approved)
	);
	let outputs = results(&harness.session);
	assert!(
		outputs
			.iter()
			.any(|text| text.contains("\"ran\":\"make build\"")),
		"{outputs:?}"
	);
	assert!(harness.kernel.waiting_approvals().is_empty());
}

#[tokio::test]
async fn deny_journals_the_denial_and_the_tool_reports_it() {
	let mut harness = harness(vec![
		tool_script("gated-1", "gated", serde_json::json!({"command": "rm -rf /"})),
		text_script("done"),
	]);
	let seen = run(&mut harness, |_| Some(decision(false, ApprovalScope::Once))).await;
	assert_eq!(seen.len(), 1);
	let journaled = prompts(&harness.session);
	assert_eq!(journaled[0].state, TicketState::Decided);
	assert!(
		journaled[0]
			.decision
			.as_ref()
			.is_some_and(|decision| !decision.approved)
	);
	let outputs = results(&harness.session);
	assert!(outputs.iter().any(|text| text.contains("denied by user")), "{outputs:?}");
}

#[tokio::test]
async fn session_grant_answers_a_repeated_subject_from_the_tree() {
	let mut harness = harness(vec![
		tool_script("gated-1", "gated", serde_json::json!({"command": "cargo test"})),
		text_script("first"),
		tool_script("gated-2", "gated", serde_json::json!({"command": "cargo test"})),
		text_script("second"),
	]);
	let seen = run(&mut harness, |_| Some(decision(true, ApprovalScope::Session))).await;
	assert_eq!(seen.len(), 1, "the session grant prompts once");
	let again = run(&mut harness, |_| panic!("a granted subject must not prompt again")).await;
	assert!(again.is_empty());
	let journaled = prompts(&harness.session);
	assert_eq!(journaled.len(), 2, "the auto-decision is journaled too");
	let auto = &journaled[1];
	assert_eq!(auto.state, TicketState::Decided);
	assert_eq!(auto.decision.as_ref().map(|decision| decision.source), Some(ApprovalSource::Config));
	assert!(
		results(&harness.session)
			.iter()
			.filter(|text| text.contains("\"ran\""))
			.count()
			>= 2
	);
}

/// A prompt that offers only `once` (a sandbox amendment) is asked every
/// time: an earlier session grant of the same subject never answers it.
#[tokio::test]
async fn a_session_grant_never_answers_a_prompt_that_offers_only_once() {
	let mut harness = harness(vec![
		tool_script("gated-1", "gated", serde_json::json!({"command": "git commit"})),
		text_script("first"),
		tool_script(
			"gated-2",
			"gated",
			serde_json::json!({"command": "git commit", "scopes": ["once"]}),
		),
		text_script("second"),
	]);
	let seen = run(&mut harness, |_| Some(decision(true, ApprovalScope::Session))).await;
	assert_eq!(seen.len(), 1);
	let again = run(&mut harness, |_| Some(decision(false, ApprovalScope::Once))).await;
	assert_eq!(again.len(), 1, "a once-only prompt is asked despite the session grant");
	let journaled = prompts(&harness.session);
	assert_eq!(journaled.len(), 2);
	let asked = journaled[1]
		.decision
		.as_ref()
		.expect("the repeat is decided");
	assert_eq!(asked.source, ApprovalSource::User, "{asked:?}");
	assert!(!asked.approved, "the user's answer, not the grant, decided the repeat");
}

/// A `session` answer to a prompt that offered only `once` grants nothing:
/// the same subject is asked again even where a session grant is offered.
#[tokio::test]
async fn a_session_answer_the_prompt_never_offered_grants_nothing() {
	let mut harness = harness(vec![
		tool_script(
			"gated-1",
			"gated",
			serde_json::json!({"command": "git commit", "scopes": ["once"]}),
		),
		text_script("first"),
		tool_script("gated-2", "gated", serde_json::json!({"command": "git commit"})),
		text_script("second"),
	]);
	let seen = run(&mut harness, |_| Some(decision(true, ApprovalScope::Session))).await;
	assert_eq!(seen.len(), 1);
	let again = run(&mut harness, |_| Some(decision(true, ApprovalScope::Once))).await;
	assert_eq!(again.len(), 1, "an unoffered session answer must not answer the repeat");
	let journaled = prompts(&harness.session);
	assert_eq!(journaled.len(), 2);
	assert_eq!(
		journaled[1]
			.decision
			.as_ref()
			.map(|decision| decision.source),
		Some(ApprovalSource::User)
	);
}

/// Requirements granted for the session on separate prompts answer a later
/// prompt that raises them together, each covered by its own grant: two hosts
/// granted one at a time, a tool and a host granted on different prompts. A
/// requirement no grant covers, or one offering only `once`, still asks; the
/// answer is persisted only when every covering grant is.
#[test]
fn grants_from_separate_prompts_cover_a_prompt_raising_them_together() {
	use omp_agent::{ApprovalDesk, ApprovalTicket, KernelEvents};

	/// Files one prompt raising `reasons`, each offering every grant scope.
	fn file(
		desk: &ApprovalDesk,
		session: &mut Session,
		call: &str,
		reasons: &[(&str, &str)],
	) -> ApprovalTicket {
		let offered = || vec![sf!("once"), sf!("session"), sf!("persist")];
		desk
			.file_specs(
				session,
				Str::new(call),
				reasons
					.iter()
					.map(|(kind, subject)| spec(kind, subject, offered()))
					.collect(),
			)
			.expect("prompt files")
	}

	/// Answers a prompt that asked, approving it for `scope`.
	fn answer(
		desk: &ApprovalDesk,
		session: &mut Session,
		ticket: &ApprovalTicket,
		scope: ApprovalScope,
	) {
		assert_eq!(ticket.state, TicketState::Pending, "{} asks", ticket.ticket_id);
		desk
			.decide(session, ticket.ticket_id.as_str(), decision(true, scope))
			.expect("the answer is journaled");
	}

	let temp = tempfile::tempdir().expect("tempdir");
	let mut session = fresh_session(&temp.path().join("grants.oms"));
	let desk = ApprovalDesk::new(KernelEvents::default());
	let docs = ("network", "http:docs.rs:443");
	let crates = ("network", "http:crates.io:443");
	let evil = ("network", "http:evil.example:443");
	let download = ("tool", "download");
	let first = file(&desk, &mut session, "call-1", &[docs]);
	answer(&desk, &mut session, &first, ApprovalScope::Session);
	let second = file(&desk, &mut session, "call-2", &[crates]);
	answer(&desk, &mut session, &second, ApprovalScope::Session);
	let once = file(&desk, &mut session, "call-3", &[download, evil]);
	answer(&desk, &mut session, &once, ApprovalScope::Once);

	let both = file(&desk, &mut session, "call-4", &[docs, crates]);
	assert_eq!(both.state, TicketState::Decided, "each host was granted on its own prompt");
	let granted = both.decision.as_ref().expect("decided by the grants");
	assert!(granted.approved);
	assert_eq!(granted.source, ApprovalSource::Config);
	assert_eq!(granted.scope, ApprovalScope::Session);
	assert_eq!(
		granted.reason.as_deref(),
		Some(
			format!("granted by {}, {} for this session", first.ticket_id, second.ticket_id).as_str()
		)
	);

	let ungranted = file(&desk, &mut session, "call-5", &[docs, evil]);
	assert_eq!(ungranted.state, TicketState::Pending, "a host granted only once still asks");
	let tool_and_host = file(&desk, &mut session, "call-6", &[download, docs]);
	assert_eq!(tool_and_host.state, TicketState::Pending, "an ungranted tool still asks");
	answer(&desk, &mut session, &tool_and_host, ApprovalScope::Session);
	let tool_and_other_host = file(&desk, &mut session, "call-7", &[download, crates]);
	assert_eq!(
		tool_and_other_host.state,
		TicketState::Decided,
		"the tool's grant and the host's grant answer together"
	);
	let once_only = desk
		.file_specs(&mut session, sf!("call-8"), vec![
			spec(docs.0, docs.1, vec![sf!("once"), sf!("session")]),
			spec(crates.0, crates.1, vec![sf!("once")]),
		])
		.expect("prompt files");
	assert_eq!(once_only.state, TicketState::Pending, "a once-only requirement is always asked");

	let forge = ("network", "github:github.com");
	let persisted = file(&desk, &mut session, "call-9", &[forge]);
	answer(&desk, &mut session, &persisted, ApprovalScope::Persist);
	let again = file(&desk, &mut session, "call-10", &[forge]);
	assert_eq!(
		again
			.decision
			.as_ref()
			.map(|decision| decision.scope.clone()),
		Some(ApprovalScope::Persist),
		"a requirement covered only by a persisted grant stays persisted"
	);
	let mixed = file(&desk, &mut session, "call-11", &[forge, docs]);
	assert_eq!(
		mixed
			.decision
			.as_ref()
			.map(|decision| decision.scope.clone()),
		Some(ApprovalScope::Session),
		"a session grant among the covering ones lasts the session"
	);
}

#[tokio::test]
async fn resumed_session_replays_the_decided_prompt() {
	let mut harness = harness(vec![
		tool_script("gated-1", "gated", serde_json::json!({"command": "ls"})),
		text_script("done"),
	]);
	let _ = run(&mut harness, |_| Some(decision(true, ApprovalScope::Once))).await;
	let path = harness.session.journal_path().to_path_buf();
	let Harness { kernel, session, events, _temp } = harness;
	drop((kernel, session, events));
	let restored = Session::open(&path, omp_session::ComponentRegistry::default()).expect("resume");
	let journaled = prompts(&restored);
	assert_eq!(journaled.len(), 1);
	assert_eq!(journaled[0].state, TicketState::Decided);
}

/// A network sandbox amendment approved for the session is a journaled
/// decision, so it outlives the process: after a resume from the journal the
/// approval desk answers the same endpoint from it (source `config`) without
/// asking, which is how the environment's grant cache refills, and still asks
/// about any other endpoint.
#[tokio::test]
async fn a_resumed_session_answers_a_journaled_network_grant_and_asks_the_rest() {
	let amendment = |call: &str, endpoint: &str| {
		tool_script(
			call,
			"gated",
			serde_json::json!({
				"command": format!("network {endpoint}"),
				"kind": "sandbox_amendment",
				"scopes": ["once", "session"],
			}),
		)
	};
	let mut harness = harness(vec![amendment("gated-1", "pypi.org:443"), text_script("granted")]);
	let seen = run(&mut harness, |_| Some(decision(true, ApprovalScope::Session))).await;
	assert_eq!(seen.len(), 1);
	let path = harness.session.journal_path().to_path_buf();
	let Harness { kernel, session, events, _temp: temp } = harness;
	drop((kernel, session, events));

	let restored = Session::open(&path, omp_session::ComponentRegistry::default()).expect("resume");
	let journaled = prompts(&restored);
	assert_eq!(journaled.len(), 1);
	assert_eq!(
		journaled[0]
			.decision
			.as_ref()
			.map(|decision| &decision.scope),
		Some(&ApprovalScope::Session)
	);
	let mut resumed = harness_in(
		temp,
		restored,
		vec![
			amendment("gated-2", "pypi.org:443"),
			text_script("replayed"),
			amendment("gated-3", "files.pythonhosted.org:443"),
			text_script("asked"),
		],
		|kernel| kernel,
	);
	let replayed = run(&mut resumed, |_| panic!("a journaled grant must not prompt again")).await;
	assert!(replayed.is_empty());
	let asked = run(&mut resumed, |_| Some(decision(false, ApprovalScope::Once))).await;
	assert_eq!(asked.len(), 1, "another endpoint is asked");
	assert_eq!(asked[0].reasons[0].subject.as_str(), "network files.pythonhosted.org:443");
	let journaled = prompts(&resumed.session);
	assert_eq!(journaled.len(), 3);
	assert_eq!(
		journaled[1]
			.decision
			.as_ref()
			.map(|decision| decision.source),
		Some(ApprovalSource::Config),
		"the replay is journaled as the desk's answer"
	);
}

/// Counts the rewinds and the session switches it is told about.
#[derive(Default)]
struct Transitions {
	rewinds:  AtomicUsize,
	switches: AtomicUsize,
}

impl Transitions {
	/// `(rewinds, switches)` told so far.
	fn seen(&self) -> (usize, usize) {
		(self.rewinds.load(Ordering::SeqCst), self.switches.load(Ordering::SeqCst))
	}
}

impl omp_agent::SessionObserver for Transitions {
	fn rewound(&self) {
		self.rewinds.fetch_add(1, Ordering::SeqCst);
	}

	fn switched(&self) {
		self.switches.fetch_add(1, Ordering::SeqCst);
	}
}

/// A harness over a fresh session whose kernel reports to `transitions`: the
/// first turn files one `gated` prompt for `cargo test`, the second the same
/// prompt again.
fn observed_harness(transitions: &Arc<Transitions>) -> Harness {
	let temp = tempfile::tempdir().expect("tempdir");
	let session = fresh_session(&temp.path().join("approvals.oms"));
	let transitions = Arc::clone(transitions);
	harness_in(
		temp,
		session,
		vec![
			tool_script("gated-1", "gated", serde_json::json!({"command": "cargo test"})),
			text_script("granted"),
			tool_script("gated-2", "gated", serde_json::json!({"command": "cargo test"})),
			text_script("asked again"),
		],
		move |kernel| kernel.with_session_observer(transitions),
	)
}

/// A rewind past a session grant takes the grant out of the journal, so the
/// same subject is asked again, and it tells every session observer the kernel
/// was composed with: the driver registers one through which the environment
/// drops the network grants it cached from that journal.
#[tokio::test]
async fn a_rewind_past_a_session_grant_asks_again_and_tells_session_observers() {
	let transitions = Arc::new(Transitions::default());
	let mut harness = observed_harness(&transitions);
	let before = harness.session.head().expect("head before the grant");
	let seen = run(&mut harness, |_| Some(decision(true, ApprovalScope::Session))).await;
	assert_eq!(seen.len(), 1);
	assert_eq!(transitions.seen(), (0, 0));

	let work = harness
		.session
		.rewind(before)
		.expect("rewind past the grant");
	harness
		.kernel
		.apply_lifecycle(&harness.session, &work)
		.await;
	assert_eq!(transitions.seen(), (1, 0), "the rewind was told, as a rewind");
	assert!(prompts(&harness.session).is_empty(), "the rewound journal holds no grant");

	let again = run(&mut harness, |_| Some(decision(false, ApprovalScope::Once))).await;
	assert_eq!(again.len(), 1, "a grant the rewind dropped answers nothing");
	assert_eq!(
		prompts(&harness.session)[0]
			.decision
			.as_ref()
			.map(|decision| decision.source),
		Some(ApprovalSource::User)
	);
}

/// The kernel outlives the session it served: when the host switches it to
/// another session, a session grant of the previous journal answers nothing in
/// the next one, and every session observer is told of the switch, so the
/// environment drops the network grants it cached from the previous journal.
#[tokio::test]
async fn a_switch_away_from_a_session_grant_asks_again_and_tells_session_observers() {
	let transitions = Arc::new(Transitions::default());
	let mut harness = observed_harness(&transitions);
	let seen = run(&mut harness, |_| Some(decision(true, ApprovalScope::Session))).await;
	assert_eq!(seen.len(), 1);
	assert_eq!(transitions.seen(), (0, 0));

	// What a host does once a switch commits: the next session is live, the
	// kernel is told, and its state is resynced from the next journal.
	let elsewhere = tempfile::tempdir().expect("tempdir");
	let next = fresh_session(&elsewhere.path().join("next.oms"));
	let previous = mem::replace(&mut harness.session, next);
	harness.kernel.session_switched();
	harness.kernel.resync_session_state(&harness.session);
	assert_eq!(transitions.seen(), (0, 1), "the switch was told, as a switch");
	assert_eq!(prompts(&previous).len(), 1, "the previous journal keeps its grant");
	drop(previous);

	let again = run(&mut harness, |_| Some(decision(false, ApprovalScope::Once))).await;
	assert_eq!(again.len(), 1, "the previous session's grant answers nothing here");
	assert_eq!(
		prompts(&harness.session)
			.iter()
			.map(|ticket| ticket.decision.as_ref().map(|decision| decision.source))
			.collect::<Vec<_>>(),
		[Some(ApprovalSource::User)],
		"a human decided the next session's prompt"
	);
}
