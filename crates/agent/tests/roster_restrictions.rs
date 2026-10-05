//! Dispatch enforces the per-request roster restrictions the advertised tool
//! list is derived from: `sv_tools` (user allowlist, Plan/Vibe binds), the
//! `turn_start` hook's `enabled_tools`, and Plan's plan-file write scope.

use std::{
	collections::VecDeque,
	future::Future,
	sync::{
		Arc,
		atomic::{AtomicUsize, Ordering},
	},
};

use bytes::Bytes;
use omp_agent::{
	DirectorRegistry, DirectorStack, DispatchPolicy, GateDecision, HookGate, HookPatch, HookPhase,
	Inference, Kernel, OnFailure, RunControl, RuntimeFlags, SourceRef, StaticPrompt, TurnInput,
	When,
	directors::{plan::Plan, vibe::Vibe},
};
use omp_ai::{ChatRequest, ChatStream};
use omp_con::Ctx;
use omp_core::{Str, sf};
use omp_journal::blob::BlobStore;
use omp_proto::toolhost::v1::HookEventId;
use omp_session::{ComponentRegistry, Session};
use omp_tool::ROSTER_RESTRICTED;

mod support;

use support::{
	Requests, ScriptedInference, TestTool, fresh_session, registry, result_text, spec, text_script,
	tool_script,
};

const PLAN_FILE: &str = "local://PLAN.md";

const fn flags() -> RuntimeFlags {
	RuntimeFlags {
		automatic_compaction:     false,
		goal_enabled:             false,
		autolearn_enabled:        false,
		autolearn_min_tool_calls: 5,
		recover_inline_edits:     false,
	}
}

/// One tool with probes for opened execution units (speculative preview) and
/// committed starts.
struct Probe {
	opened:  Arc<AtomicUsize>,
	started: Arc<AtomicUsize>,
}

impl Probe {
	fn new() -> Self {
		Self { opened: Arc::new(AtomicUsize::new(0)), started: Arc::new(AtomicUsize::new(0)) }
	}

	fn tool(&self, name: &str) -> TestTool {
		spec(name, 1, &format!("{name} ran"))
			.opened_probe(Arc::clone(&self.opened))
			.concurrency_probe(Arc::clone(&self.started), Arc::new(tokio::sync::Barrier::new(1)))
	}

	fn opened(&self) -> usize {
		self.opened.load(Ordering::SeqCst)
	}

	fn started(&self) -> usize {
		self.started.load(Ordering::SeqCst)
	}
}

fn kernel<C: Inference>(
	inference: C,
	tools: impl IntoIterator<Item = TestTool>,
	blobs: &std::path::Path,
	con: &Arc<Ctx>,
) -> Kernel<C> {
	Kernel::new(
		inference,
		registry(tools),
		DispatchPolicy::new(BlobStore::open(blobs).expect("blobs")),
		StaticPrompt(sf!("system")),
	)
	.with_director_registry(DirectorRegistry::standard())
	.with_con_context(Arc::clone(con))
	.with_runtime_flags(flags())
}

#[allow(
	clippy::future_not_send,
	reason = "the kernel turn future is driven on the test's own task, never sent"
)]
async fn run(kernel: &mut Kernel<impl Inference>, session: &mut Session) {
	kernel
		.run_turn(
			session,
			TurnInput { text: sf!("go"), attachments: Vec::new() },
			RunControl::default(),
		)
		.await
		.expect("turn");
}

fn advertised(requests: &Requests, index: usize) -> Vec<String> {
	requests.lock()[index]
		.tools
		.iter()
		.map(|tool| tool.name.to_string())
		.collect()
}

fn engage(session: &mut Session, director: Box<dyn omp_agent::Director>) {
	DirectorStack::from_dom(session.dom(), &DirectorRegistry::standard())
		.engage(session, director)
		.expect("director engages");
}

fn journal(session: &Session) -> String {
	std::fs::read_to_string(session.journal_path()).expect("journal reads")
}

#[tokio::test]
async fn plan_mode_refuses_hidden_tools_and_off_plan_writes_at_dispatch() {
	let temp = tempfile::tempdir().expect("tempdir");
	let con = Arc::new(Ctx::new());
	let edit = Probe::new();
	let write = Probe::new();
	let read = Probe::new();
	let (inference, requests) = ScriptedInference::new([
		tool_script("edit-1", "edit", serde_json::json!({"path": "src/lib.rs", "input": "x"})),
		tool_script("write-off", "write", serde_json::json!({"path": "src/main.rs", "content": "x"})),
		tool_script(
			"write-escape",
			"write",
			serde_json::json!({"path": "local://x/../../PLAN.md", "content": "x"}),
		),
		tool_script("write-plan", "write", serde_json::json!({"path": PLAN_FILE, "content": "plan"})),
		tool_script("read-1", "read", serde_json::json!({})),
		tool_script("ask-1", "ask", serde_json::json!({"question": "approve?"})),
		text_script("presented"),
	]);
	let mut kernel = kernel(
		inference,
		[edit.tool("edit"), write.tool("write"), read.tool("read"), spec("ask", 1, "asked")],
		&temp.path().join("blobs"),
		&con,
	);
	let mut session = fresh_session(&temp.path().join("plan.oms"));
	engage(&mut session, Box::new(Plan::new(PLAN_FILE)));
	run(&mut kernel, &mut session).await;

	// The wire roster is fixed: Plan's bind never narrows it, so the model still
	// sees `edit` and the refusal tells it what plan mode leaves callable.
	let roster = advertised(&requests, 0);
	assert!(roster.iter().any(|name| name == "edit"), "{roster:?}");
	let sent = requests.lock().len();
	for index in 1..sent {
		assert_eq!(
			advertised(&requests, index),
			roster,
			"request {index} advertises the same roster"
		);
	}
	let available = roster
		.iter()
		.filter(|name| *name != "edit")
		.cloned()
		.collect::<Vec<_>>()
		.join(", ");

	assert_eq!(edit.opened(), 0, "a refused call never opens a unit, so it cannot preview");
	assert_eq!(result_text(&session, "edit-1"), [format!(
		"skipped: `edit` is not available while plan mode is active. No action was taken. Available \
		 now: {available}."
	)]);
	let off_plan = "skipped: `write` is limited to the plan file local://PLAN.md while plan mode \
	                is active; no action was taken.";
	assert_eq!(result_text(&session, "write-off"), [off_plan]);
	assert_eq!(result_text(&session, "write-escape"), [off_plan]);
	assert_eq!(result_text(&session, "write-plan"), ["write ran"]);
	assert_eq!(
		(write.opened(), write.started()),
		(1, 1),
		"only the plan-file write opened a unit and ran"
	);
	assert_eq!(result_text(&session, "read-1"), ["read ran"], "allowed tools are unaffected");
	assert_eq!(read.started(), 1);

	// The refusal reaches the model on the next request.
	let next = format!("{:?}", requests.lock()[1].messages);
	assert!(next.contains("is not available while plan mode is active"), "{next}");

	// Durable evidence: typed policy denials with their rule ids, surviving replay.
	let journal = journal(&session);
	assert!(journal.contains("\"policy_denied\""), "{journal}");
	assert!(journal.contains(ROSTER_RESTRICTED), "{journal}");
	assert!(journal.contains("director:plan/sv_tools"), "{journal}");
	assert!(journal.contains("director:plan/plan_file"), "{journal}");
	let path = session.journal_path().to_path_buf();
	drop(session);
	let replayed = Session::open(path, ComponentRegistry::default()).expect("session replays");
	assert_eq!(result_text(&replayed, "write-off"), [off_plan]);
}

#[tokio::test]
async fn vibe_bind_and_user_allowlist_refuse_with_their_own_reason() {
	let temp = tempfile::tempdir().expect("tempdir");
	let con = Arc::new(Ctx::new());
	let write = Probe::new();
	let (inference, _) = ScriptedInference::new([
		tool_script("write-1", "write", serde_json::json!({"path": "a.rs", "content": "x"})),
		text_script("coordinated"),
	]);
	let mut vibe_kernel = kernel(
		inference,
		[write.tool("write"), spec("read", 1, "read ran")],
		&temp.path().join("v"),
		&con,
	);
	let mut session = fresh_session(&temp.path().join("vibe.oms"));
	engage(&mut session, Box::new(Vibe::new()));
	run(&mut vibe_kernel, &mut session).await;
	assert_eq!(write.opened(), 0);
	let text = result_text(&session, "write-1").concat();
	assert!(
		text.starts_with("skipped: `write` is not available while vibe mode is active."),
		"{text}"
	);
	assert!(journal(&session).contains("director:vibe/sv_tools"));

	let user = Arc::new(Ctx::new());
	omp_agent::SV_TOOLS
		.set(&user, vec![Str::new_static("read")])
		.expect("allowlist");
	let bash = Probe::new();
	let (inference, _) = ScriptedInference::new([
		tool_script("bash-1", "bash", serde_json::json!({"command": "ls"})),
		tool_script("read-1", "read", serde_json::json!({})),
		text_script("done"),
	]);
	let mut user_kernel = kernel(
		inference,
		[bash.tool("bash"), spec("read", 1, "read ran")],
		&temp.path().join("u"),
		&user,
	);
	let mut session = fresh_session(&temp.path().join("user.oms"));
	run(&mut user_kernel, &mut session).await;
	assert_eq!(bash.opened(), 0);
	assert_eq!(result_text(&session, "bash-1"), ["skipped: `bash` is not in the `sv_tools` \
	                                              allowlist. No action was taken. Available now: \
	                                              read."]);
	assert_eq!(result_text(&session, "read-1"), ["read ran"]);
	let journal = journal(&session);
	assert!(journal.contains("\"rules\":[\"sv_tools\"]"), "{journal}");
}

/// Writes `sv_tools` while a request streams, after its snapshot was taken.
struct WritingInference {
	inner:  ScriptedInference,
	con:    Arc<Ctx>,
	writes: VecDeque<Option<Vec<Str>>>,
}

impl Inference for WritingInference {
	fn chat(
		&mut self,
		request: ChatRequest,
	) -> impl Future<Output = Result<ChatStream, omp_ai::Error>> + Send {
		if let Some(Some(roster)) = self.writes.pop_front() {
			omp_agent::SV_TOOLS
				.set(&self.con, roster)
				.expect("allowlist write");
		}
		self.inner.chat(request)
	}
}

#[tokio::test]
async fn restrictions_are_snapshotted_per_request() {
	let temp = tempfile::tempdir().expect("tempdir");
	let con = Arc::new(Ctx::new());
	omp_agent::SV_TOOLS
		.set(&con, vec![Str::new_static("read"), Str::new_static("grep")])
		.expect("allowlist");
	let grep = Probe::new();
	let (inner, requests) = ScriptedInference::new([
		tool_script("grep-1", "grep", serde_json::json!({})),
		tool_script("grep-2", "grep", serde_json::json!({})),
		text_script("done"),
	]);
	// The narrowing write lands while request 0 streams: calls sampled under
	// request 0's instructions still run; request 1 is checked under the
	// narrowed allowlist while its wire roster stays the latched one.
	let inference = WritingInference {
		inner,
		con: Arc::clone(&con),
		writes: VecDeque::from([Some(vec![Str::new_static("read")]), None, None]),
	};
	let mut kernel = kernel(
		inference,
		[grep.tool("grep"), spec("read", 1, "read ran")],
		&temp.path().join("blobs"),
		&con,
	);
	let mut session = fresh_session(&temp.path().join("snapshot.oms"));
	run(&mut kernel, &mut session).await;
	assert_eq!(advertised(&requests, 0), ["grep", "read"]);
	assert_eq!(
		advertised(&requests, 1),
		["grep", "read"],
		"a mid-turn write never reshapes the wire"
	);
	assert_eq!(result_text(&session, "grep-1"), ["grep ran"]);
	assert_eq!(result_text(&session, "grep-2"), ["skipped: `grep` is not in the `sv_tools` \
	                                              allowlist. No action was taken. Available now: \
	                                              read."]);
	assert_eq!(grep.started(), 1);
}

fn turn_start_subscription() -> omp_agent::hooks::Subscription {
	omp_agent::hooks::Subscription {
		host:       sf!("test"),
		source:     SourceRef {
			layer:        0,
			publisher:    sf!("test"),
			extension_id: sf!("roster"),
		},
		id:         1,
		event:      HookEventId::HookEventTurnStart,
		phase:      HookPhase::Transform,
		order:      0,
		on_failure: OnFailure::Deny,
		when:       When::default(),
	}
}

#[tokio::test]
async fn turn_start_hook_enabled_tools_gates_dispatch() {
	let (gate, receiver) = HookGate::channel();
	let gate = Arc::new(gate);
	gate
		.subscribe("test", [turn_start_subscription()])
		.expect("subscription");
	let responder = {
		let gate = Arc::clone(&gate);
		tokio::spawn(async move {
			while let Ok(dispatch) = receiver.recv_async().await {
				let mut payload: serde_json::Value =
					serde_json::from_slice(&dispatch.payload).expect("hook payload");
				payload["enabled_tools"] = serde_json::json!(["read"]);
				let _ = gate.answer(dispatch.dispatch_id, vec![(
					1,
					GateDecision::Modify(HookPatch {
						target: None,
						args:   Some(Bytes::from(serde_json::to_vec(&payload).expect("patch"))),
					}),
				)]);
			}
		})
	};
	let temp = tempfile::tempdir().expect("tempdir");
	let con = Arc::new(Ctx::new());
	let grep = Probe::new();
	let (inference, requests) = ScriptedInference::new([
		tool_script("grep-1", "grep", serde_json::json!({})),
		tool_script("read-1", "read", serde_json::json!({})),
		text_script("done"),
	]);
	let mut kernel = kernel(
		inference,
		[grep.tool("grep"), spec("read", 1, "read ran")],
		&temp.path().join("blobs"),
		&con,
	)
	.with_hook_gate(Arc::clone(&gate));
	let mut session = fresh_session(&temp.path().join("hook.oms"));
	run(&mut kernel, &mut session).await;
	assert_eq!(advertised(&requests, 0), ["grep", "read"], "the hook filters calls, not the wire");
	assert_eq!(advertised(&requests, 1), ["grep", "read"]);
	assert_eq!(grep.opened(), 0);
	assert_eq!(result_text(&session, "grep-1"), ["skipped: `grep` was disabled for this request \
	                                              by a turn_start hook. No action was taken. \
	                                              Available now: read."]);
	assert_eq!(result_text(&session, "read-1"), ["read ran"]);
	assert!(journal(&session).contains("hook:turn_start/enabled_tools"));
	drop(kernel);
	responder.abort();
}

/// Host executor that answers every call.
struct NoHost;

impl omp_tool::HostToolExecutor for NoHost {
	fn execute(
		&self,
		_invocation: omp_tool::HostToolInvocation,
		_updates: omp_tool::HostToolUpdateSink,
		_cancellation: tokio_util::sync::CancellationToken,
	) -> std::pin::Pin<
		Box<dyn Future<Output = Result<omp_tool::HostToolResult, Str>> + Send + 'static>,
	> {
		Box::pin(std::future::ready(Ok(omp_tool::HostToolResult {
			result:   serde_json::json!({"ok": true}),
			is_error: false,
		})))
	}
}

/// Replaces the host roster while a request streams, after its tools left.
struct ReplacingInference {
	inner:    ScriptedInference,
	registry: Arc<omp_tool::Registry>,
	calls:    usize,
}

impl Inference for ReplacingInference {
	fn chat(
		&mut self,
		request: ChatRequest,
	) -> impl Future<Output = Result<ChatStream, omp_ai::Error>> + Send {
		self.calls += 1;
		if self.calls == 1 {
			self
				.registry
				.replace_host_tools(sf!("rpc"), 2, Vec::new(), Arc::new(NoHost))
				.expect("host roster replaces");
		}
		self.inner.chat(request)
	}
}

/// A name the wire declares but the registry no longer resolves (a host roster
/// replaced after the request left) settles as a typed
/// `tool.roster.unavailable` denial, not a kernel error; the next request
/// re-lowers the replaced roster.
#[tokio::test]
async fn a_declared_tool_that_no_longer_resolves_settles_as_typed_unavailable() {
	let temp = tempfile::tempdir().expect("tempdir");
	let con = Arc::new(Ctx::new());
	let registry = registry([spec("read", 1, "read ran")]);
	registry
		.replace_host_tools(
			sf!("rpc"),
			1,
			vec![omp_tool::HostToolSpec {
				name:        sf!("fetch_ticket"),
				description: sf!("Fetch a ticket"),
				parameters:  serde_json::json!({"type": "object"}),
				rev:         None,
			}],
			Arc::new(NoHost),
		)
		.expect("host roster installs");
	let (inner, requests) = ScriptedInference::new([
		tool_script("ticket-1", "fetch_ticket", serde_json::json!({})),
		text_script("done"),
	]);
	let mut kernel = Kernel::new(
		ReplacingInference { inner, registry: Arc::clone(&registry), calls: 0 },
		Arc::clone(&registry),
		DispatchPolicy::new(BlobStore::open(temp.path().join("blobs")).expect("blobs")),
		StaticPrompt(sf!("system")),
	)
	.with_director_registry(DirectorRegistry::standard())
	.with_con_context(Arc::clone(&con))
	.with_runtime_flags(flags());
	let mut session = fresh_session(&temp.path().join("unavailable.oms"));
	run(&mut kernel, &mut session).await;
	let mut first = advertised(&requests, 0);
	first.sort_unstable();
	assert_eq!(first, ["fetch_ticket", "read"]);
	assert_eq!(
		advertised(&requests, 1),
		["read"],
		"the replaced host roster is an explicit boundary"
	);
	assert_eq!(result_text(&session, "ticket-1"), ["skipped: `fetch_ticket` is no longer \
	                                                available in this session. No action was \
	                                                taken."]);
	let journal = journal(&session);
	assert!(journal.contains("tool.roster.unavailable"), "{journal}");
}
