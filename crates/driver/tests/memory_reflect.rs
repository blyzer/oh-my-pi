//! Memory `reflect` on a real project environment, its reflection bound with
//! the driver's `bind_reflection` as `compose_kernel` binds it: the kernel
//! admits the call at the `exec` tier its declared inference request gives
//! it, and an admitted call synthesizes with exactly one request through the
//! session's inference capability. A refused call makes none. A session
//! attached to a project daemon gets the same synthesis: the daemon runs
//! `reflect` and relays its synthesis back to the issuing session.

mod support;

use std::{
	fs,
	future::ready,
	sync::Arc,
	time::{Duration, SystemTime},
};

use futures::stream;
use omp_agent::{
	ApprovalDecision, ApprovalScope, ApprovalSource, DispatchPolicy, Inference, Kernel, KernelEvent,
	RunControl, StaticPrompt, TicketState, TurnInput, Up,
};
use omp_ai::{
	BlockKind, ChatEvent, ChatRequest, ChatStream, Completion, ExecutionReceipt, FinishReason,
	OpaqueJson, RequestId, ResponseMeta, ToolCall, ToolCallId, Usage,
};
use omp_catalog::{ProviderId, RouteId};
use omp_core::Str;
use omp_driver::headless::{
	kernel::{EnvToolExecutor, SettingsAdmission, bind_environment_approvals},
	reflection::{REFLECTION_SELECTOR, bind_reflection},
};
use omp_envd::{
	AttachOptions, ProjectEnvironment, RegistryBridges,
	host_settings::{AI_MEMORY_BACKEND, MemoryBackendSetting},
	tool_settings::ApprovalMode,
};
use omp_journal::{blob::BlobStore, kind};
use omp_memory::runtime::SaveRequest;
use omp_session::{ComponentRegistry, Session, components::prompts::prompts_handle};
use parking_lot::Mutex;
use tokio::time::timeout;

/// The session model: one tool call per turn from `calls`, then a closing
/// text turn.
struct Script {
	calls: Vec<(&'static str, serde_json::Value)>,
	turns: usize,
}

impl Script {
	fn reflect() -> Self {
		Self { calls: vec![reflect_call()], turns: 0 }
	}
}

fn reflect_call() -> (&'static str, serde_json::Value) {
	("reflect", serde_json::json!({ "query": "deploy target", "i": "Proving memory reflection" }))
}

impl Inference for Script {
	fn chat(
		&mut self,
		_request: ChatRequest,
	) -> impl Future<Output = Result<ChatStream, omp_ai::Error>> + Send {
		self.turns += 1;
		let meta = ResponseMeta {
			request_id:          RequestId::from("reflect-test"),
			provider:            ProviderId::from("test"),
			route:               RouteId::from("test/route"),
			model:               None,
			provider_request_id: None,
			created_at:          SystemTime::UNIX_EPOCH,
		};
		let events = if let Some((name, arguments)) = self.calls.get(self.turns - 1).cloned() {
			let call = ToolCall {
				id:        ToolCallId::from(format!("call-{}", self.turns)),
				name:      Str::new_static(name),
				arguments: OpaqueJson::new(arguments.clone()),
			};
			vec![
				ChatEvent::Started(meta),
				ChatEvent::ToolCallStarted {
					index: 0,
					id:    call.id.clone(),
					name:  call.name.clone(),
				},
				ChatEvent::ToolArgumentsDelta {
					index: 0,
					bytes: bytes::Bytes::from(serde_json::to_vec(&arguments).expect("args")),
				},
				ChatEvent::ToolCallReady { index: 0, call },
				ChatEvent::Completed(completion(FinishReason::ToolCalls)),
			]
		} else {
			vec![
				ChatEvent::Started(meta),
				ChatEvent::BlockStarted { index: 0, kind: BlockKind::Text },
				ChatEvent::TextDelta { index: 0, text: Str::new_static("done") },
				ChatEvent::Completed(completion(FinishReason::Stop)),
			]
		};
		ready(Ok(ChatStream::ordinary(Box::pin(stream::iter(events.into_iter().map(Ok))))))
	}
}

fn completion(reason: FinishReason) -> Completion {
	Completion {
		reason,
		blocks: 1,
		usage: Usage::default(),
		receipt: ExecutionReceipt::default().into(),
	}
}

/// The session's inference capability as reflection sees it: it records the
/// selector of every request and answers each with one synthesized line.
#[derive(Clone, Default)]
struct Synthesis {
	selectors: Arc<Mutex<Vec<Option<Str>>>>,
}

impl Synthesis {
	fn answer(&self, selector: Option<&str>) -> ChatStream {
		self.selectors.lock().push(selector.map(Str::new));
		let events = [
			ChatEvent::TextDelta { index: 0, text: Str::new_static("Deploys go to fly.io.") },
			ChatEvent::Completed(completion(FinishReason::Stop)),
		];
		ChatStream::ordinary(Box::pin(stream::iter(events.into_iter().map(Ok))))
	}
}

impl Inference for Synthesis {
	fn chat(
		&mut self,
		_request: ChatRequest,
	) -> impl Future<Output = Result<ChatStream, omp_ai::Error>> + Send {
		ready(Ok(self.answer(None)))
	}

	fn chat_on(
		&mut self,
		selector: &str,
		_request: ChatRequest,
	) -> impl Future<Output = Result<ChatStream, omp_ai::Error>> + Send {
		ready(Ok(self.answer(Some(selector))))
	}
}

/// What one scripted turn left behind.
struct Turn {
	tickets:   Vec<omp_agent::ApprovalTicket>,
	/// Journaled tool-result data.
	result:    String,
	/// Selector of every synthesis request reflection made.
	selectors: Vec<Option<Str>>,
}

/// Runs one turn calling `reflect` under `mode` (`None`: the shipped default
/// posture) against a Mnemopi bank holding one fact; `approve` answers every
/// prompt the kernel journals.
async fn reflect_turn(mode: Option<ApprovalMode>, approve: bool) -> Turn {
	let scratch = tempfile::tempdir().expect("scratch");
	let root = scratch.path().join("workspace");
	let state = scratch.path().join("state");
	fs::create_dir_all(&root).expect("workspace");
	fs::create_dir_all(&state).expect("state");
	let root = fs::canonicalize(&root).expect("canonical workspace");
	let con = Arc::new(omp_con::Ctx::new());
	AI_MEMORY_BACKEND
		.set(&con, MemoryBackendSetting::Mnemopi)
		.expect("memory backend");
	// No project daemon can start from this test binary, so the application's
	// own attach falls back to the embedded environment, the composition whose
	// `reflect` the binding reaches.
	let environment = ProjectEnvironment::attach(&root, &state, AttachOptions {
		py_eval:            false,
		approval_mode:      mode,
		trusted_extensions: Vec::new(),
		contributed_values: Vec::new(),
		con:                Arc::clone(&con),
		bridges:            RegistryBridges::default(),
		spawn_idle_timeout: Some(2),
	})
	.await
	.expect("environment");
	assert!(environment.fallback_notice.is_some(), "the session must run an embedded environment");
	environment
		.memory_runtime()
		.save_batch(
			&[SaveRequest { content: "The deploy target is fly.io", context: None }],
			"reflect-test",
			0.75,
		)
		.expect("retain the fact");
	let synthesis = Synthesis::default();
	bind_reflection(&environment, synthesis.clone()).expect("bind reflection");
	drive(&environment, &con, &root, scratch.path(), mode, approve, Script::reflect(), &synthesis)
		.await
}

/// Runs one turn of `script` on `environment` under `mode`; `approve`
/// answers every prompt the kernel journals.
#[expect(clippy::too_many_arguments, reason = "one scripted turn's whole fixture")]
async fn drive(
	environment: &ProjectEnvironment,
	con: &Arc<omp_con::Ctx>,
	root: &std::path::Path,
	scratch: &std::path::Path,
	mode: Option<ApprovalMode>,
	approve: bool,
	script: Script,
	synthesis: &Synthesis,
) -> Turn {
	let spill = BlobStore::open(scratch.join("artifacts")).expect("spill");
	let kernel = Kernel::new(
		script,
		environment.registry(),
		DispatchPolicy::new(spill.clone()),
		StaticPrompt(Str::new_static("test")),
	);
	let approvals = bind_environment_approvals(&kernel, environment);
	let mut kernel = kernel
		.with_external_executor(Arc::new(EnvToolExecutor::new(
			environment.client().clone(),
			approvals,
		)))
		.with_tool_admission(Arc::new(SettingsAdmission::new(con, mode, root)));
	let events = kernel.subscribe();
	let mailbox = kernel.mailbox();
	let host = tokio::spawn(async move {
		while let Ok(event) = events.recv_async().await {
			if let KernelEvent::ApprovalRequested(ticket) = event {
				let _ = mailbox.send(Up::Approve {
					id:       ticket.ticket_id,
					decision: ApprovalDecision {
						approved:   approve,
						scope:      ApprovalScope::Once,
						source:     ApprovalSource::User,
						decided_by: None,
						reason:     (!approve).then(|| Str::new_static("not today")),
						audited:    false,
					},
				});
			}
		}
	});
	let mut session = Session::create_with_blob_store(
		scratch.join("reflect.oms"),
		ComponentRegistry::standard(),
		spill,
	)
	.expect("session");
	timeout(
		Duration::from_secs(60),
		kernel.run_turn(
			&mut session,
			TurnInput { text: Str::new_static("where do we deploy?"), attachments: Vec::new() },
			RunControl::default(),
		),
	)
	.await
	.expect("turn settles")
	.expect("turn");
	host.abort();
	let journal = fs::read_to_string(session.journal_path()).expect("journal");
	assert!(journal.contains(&format!("event: {}", kind::TOOL_RESULT)));
	let result = journal
		.lines()
		.filter(|line| line.starts_with("data: "))
		.filter(|line| line.contains("outcome") || line.contains("fault"))
		.collect::<Vec<_>>()
		.join("\n");
	let tickets = prompts(&session);
	drop(kernel);
	let selectors = synthesis.selectors.lock().clone();
	Turn { tickets, result, selectors }
}

fn prompts(session: &Session) -> Vec<omp_agent::ApprovalTicket> {
	let dom = session.dom();
	let prompts = prompts_handle(dom).expect("prompts");
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

/// The shipped default posture prompts for `reflect` as `exec` tier; once
/// approved, it synthesizes with exactly one request on the memory role.
#[tokio::test]
async fn default_posture_prompts_for_reflect_and_an_approved_call_is_one_request() {
	let Turn { tickets, result, selectors } = reflect_turn(None, true).await;

	assert_eq!(tickets.len(), 1, "one admission prompt: {tickets:?}");
	let reason = &tickets[0].reasons[0];
	assert_eq!((reason.kind.as_str(), reason.subject.as_str()), ("tool", "reflect"));
	assert!(
		reason.evidence[0].starts_with("exec tier, host confinement"),
		"reflect is admitted at the exec tier: {:?}",
		reason.evidence
	);
	assert_eq!(tickets[0].state, TicketState::Decided);
	assert_eq!(selectors, [Some(Str::new_static(REFLECTION_SELECTOR))]);
	assert!(result.contains("Deploys go to fly.io."), "the synthesized answer settles: {result}");
}

/// A refused `reflect` never reaches inference.
#[tokio::test]
async fn a_refused_reflect_makes_no_inference_request() {
	let Turn { tickets, result, selectors } =
		reflect_turn(Some(ApprovalMode::AlwaysAsk), false).await;

	assert_eq!(tickets.len(), 1, "one admission prompt: {tickets:?}");
	assert!(selectors.is_empty(), "a refused call made {} requests", selectors.len());
	assert!(result.contains("denied by user: not today"), "denied reflect must settle: {result}");
}

/// An explicit `yolo` runs `reflect` unprompted, still with one request.
#[tokio::test]
async fn explicit_yolo_runs_reflect_unprompted_with_one_request() {
	let Turn { tickets, result, selectors } = reflect_turn(Some(ApprovalMode::Yolo), true).await;

	assert!(tickets.is_empty(), "explicit yolo prompts for nothing: {tickets:?}");
	assert_eq!(selectors, [Some(Str::new_static(REFLECTION_SELECTOR))]);
	assert!(result.contains("Deploys go to fly.io."), "the synthesized answer settles: {result}");
}

/// A session attached to a project daemon: the daemon runs `retain` and
/// `reflect` over its own memory bank and relays the synthesis to the issuing
/// session, whose bound inference answers it with exactly one request.
#[tokio::test]
async fn an_attached_session_synthesizes_a_daemon_reflect_with_one_request() {
	let scratch = tempfile::tempdir().expect("scratch");
	let root = scratch.path().join("workspace");
	let state = scratch.path().join("state");
	fs::create_dir_all(&root).expect("workspace");
	fs::create_dir_all(&state).expect("state");
	let root = fs::canonicalize(&root).expect("canonical workspace");
	let con = Arc::new(omp_con::Ctx::new());
	AI_MEMORY_BACKEND
		.set(&con, MemoryBackendSetting::Mnemopi)
		.expect("memory backend");
	let _daemon = support::InProcessDaemon::serve(&root, &state, Arc::clone(&con)).await;
	let environment = ProjectEnvironment::attach(&root, &state, AttachOptions {
		py_eval:            false,
		approval_mode:      Some(ApprovalMode::Yolo),
		trusted_extensions: Vec::new(),
		contributed_values: Vec::new(),
		con:                Arc::clone(&con),
		bridges:            RegistryBridges::default(),
		spawn_idle_timeout: Some(2),
	})
	.await
	.expect("environment");
	assert!(
		environment.fallback_notice.is_none(),
		"the session fell back to an embedded environment: {:?}",
		environment.fallback_notice
	);
	let synthesis = Synthesis::default();
	bind_reflection(&environment, synthesis.clone()).expect("bind reflection");
	let script = Script {
		calls: vec![
			(
				"retain",
				serde_json::json!({
					"items": [{ "content": "The deploy target is fly.io" }],
					"i": "Remembering the deploy target",
				}),
			),
			reflect_call(),
		],
		turns: 0,
	};
	let Turn { tickets, result, selectors } = drive(
		&environment,
		&con,
		&root,
		scratch.path(),
		Some(ApprovalMode::Yolo),
		true,
		script,
		&synthesis,
	)
	.await;

	assert!(tickets.is_empty(), "explicit yolo prompts for nothing: {tickets:?}");
	assert_eq!(selectors, [Some(Str::new_static(REFLECTION_SELECTOR))]);
	assert!(result.contains("Deploys go to fly.io."), "the relayed synthesis settles: {result}");
	assert!(!result.contains("Based on recalled memories"), "no evidence fallback: {result}");
}
