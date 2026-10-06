//! Production approval composition: a real project environment under
//! `--approval-mode always-ask`, the kernel-bound approval route, and a
//! `write` call whose admission prompt is journaled and answered by the host
//! with `Up::Approve` (deny → skipped, allow → the file lands). An explicit
//! `yolo` with no sandbox in force is downgraded to `write`, so a `bash` call
//! prompts and one typed notice says why.

use std::{future::ready, path::Path, sync::Arc, time::Duration};

use futures::stream;
use omp_agent::{
	ApprovalDecision, ApprovalScope, ApprovalSource, DispatchPolicy, Inference, Kernel, KernelEvent,
	RunControl, StaticPrompt, TicketState, TurnInput, Up,
};
use omp_ai::{
	BlockKind, ChatEvent, ChatRequest, ChatStream, Completion, ExecutionReceipt, FinishReason,
	RequestId, ResponseMeta, ToolCall, ToolCallId, Usage,
};
use omp_catalog::{ProviderId, RouteId};
use omp_core::Str;
use omp_driver::headless::kernel::{EnvToolExecutor, SettingsAdmission};
use omp_envd::{
	AttachOptions, ProjectEnvironment, RegistryBridges,
	exec_settings::{ExecSandboxMode, SV_SANDBOX_MODE},
	tool_settings::ApprovalMode,
};
use omp_journal::kind;
use omp_session::{ComponentRegistry, Session};

/// One scripted tool call, then a closing text turn. `write` declares document
/// write effects, so its tier is `write` and always-ask prompts before it
/// starts. `bash` declares no effects: its spawn/fs effects are confined by
/// the sandbox, and without one it is process authority.
struct ToolThenText {
	tool:      &'static str,
	arguments: serde_json::Value,
	turns:     usize,
}

impl Inference for ToolThenText {
	fn chat(
		&mut self,
		_request: ChatRequest,
	) -> impl Future<Output = Result<ChatStream, omp_ai::Error>> + Send {
		self.turns += 1;
		let meta = ResponseMeta {
			request_id:          RequestId::from("approval-test"),
			provider:            ProviderId::from("test"),
			route:               RouteId::from("test/route"),
			model:               None,
			provider_request_id: None,
			created_at:          std::time::SystemTime::UNIX_EPOCH,
		};
		let events = if self.turns == 1 {
			let arguments = self.arguments.clone();
			let call = ToolCall {
				id:        ToolCallId::from("call-1"),
				name:      Str::new_static(self.tool),
				arguments: omp_ai::OpaqueJson::new(arguments.clone()),
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
				ChatEvent::Completed(Completion {
					reason:  FinishReason::ToolCalls,
					blocks:  1,
					usage:   Usage::default(),
					receipt: ExecutionReceipt::default().into(),
				}),
			]
		} else {
			vec![
				ChatEvent::Started(meta),
				ChatEvent::BlockStarted { index: 0, kind: BlockKind::Text },
				ChatEvent::TextDelta { index: 0, text: Str::new_static("done") },
				ChatEvent::Completed(Completion {
					reason:  FinishReason::Stop,
					blocks:  1,
					usage:   Usage::default(),
					receipt: ExecutionReceipt::default().into(),
				}),
			]
		};
		ready(Ok(ChatStream::ordinary(Box::pin(stream::iter(events.into_iter().map(Ok))))))
	}
}

fn decision(approved: bool) -> ApprovalDecision {
	ApprovalDecision {
		approved,
		scope: ApprovalScope::Once,
		source: ApprovalSource::User,
		decided_by: None,
		reason: (!approved).then(|| Str::new_static("not today")),
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

/// What one scripted turn left behind.
struct Turn {
	session: Session,
	/// Journaled tool-result data.
	result:  String,
	/// Whether `target` exists afterwards.
	landed:  bool,
}

/// Runs one turn calling `tool` under `mode`, with the sandbox `sandbox`;
/// `approve` answers the prompt the kernel journals. `target` is the file the
/// call creates, relative to the workspace.
async fn run(
	tool: &'static str,
	arguments: impl FnOnce(&Path) -> serde_json::Value,
	target: &str,
	mode: ApprovalMode,
	sandbox: ExecSandboxMode,
	approve: bool,
) -> Turn {
	let scratch = tempfile::tempdir().expect("scratch");
	let root = scratch.path().join("workspace");
	let state = scratch.path().join("state");
	std::fs::create_dir_all(&root).expect("workspace");
	std::fs::create_dir_all(&state).expect("state");
	let root = std::fs::canonicalize(&root).expect("canonical workspace");
	let target = root.join(target);
	let con = omp_con::Ctx::new();
	SV_SANDBOX_MODE.set(&con, sandbox).expect("sandbox mode");
	let con = Arc::new(con);
	let environment = ProjectEnvironment::attach(&root, &state, AttachOptions {
		py_eval:            false,
		approval_mode:      Some(mode),
		trusted_extensions: Vec::new(),
		contributed_values: Vec::new(),
		con:                Arc::clone(&con),
		bridges:            RegistryBridges::default(),
		spawn_idle_timeout: Some(2),
	})
	.await
	.expect("environment");
	let registry = environment.registry();
	let spill = omp_journal::blob::BlobStore::open(scratch.path().join("artifacts")).expect("spill");
	let kernel = Kernel::new(
		ToolThenText { tool, arguments: arguments(&target), turns: 0 },
		registry,
		DispatchPolicy::new(spill.clone()),
		StaticPrompt(Str::new_static("test")),
	);
	let approvals = kernel.approval_route();
	let notices = kernel.mailbox();
	environment.bind_approval_authority(
		Some(Arc::new(omp_agent::ApprovalBook::new())),
		Some(approvals.clone()),
	);
	let mut kernel = kernel
		.with_external_executor(Arc::new(EnvToolExecutor::new(
			environment.client().clone(),
			approvals,
		)))
		.with_tool_admission(Arc::new(
			SettingsAdmission::new(&con, Some(mode), &root).with_notices(notices),
		));
	let events = kernel.subscribe();
	let mailbox = kernel.mailbox();
	let host = tokio::spawn(async move {
		while let Ok(event) = events.recv_async().await {
			if let KernelEvent::ApprovalRequested(ticket) = event {
				assert_eq!(ticket.invocation_id.as_deref(), Some("call-1"));
				let _ = mailbox
					.send(Up::Approve { id: ticket.ticket_id, decision: decision(approve) });
			}
		}
	});
	let mut session = Session::create_with_blob_store(
		scratch.path().join("approval.oms"),
		ComponentRegistry::standard(),
		spill,
	)
	.expect("session");
	tokio::time::timeout(
		Duration::from_secs(60),
		kernel.run_turn(
			&mut session,
			TurnInput { text: Str::new_static("run it"), attachments: Vec::new() },
			RunControl::default(),
		),
	)
	.await
	.expect("turn settles")
	.expect("turn");
	host.abort();
	let journal = std::fs::read_to_string(session.journal_path()).expect("journal");
	assert!(journal.contains(&format!("event: {}", kind::TOOL_CALL)));
	assert!(journal.contains(&format!("event: {}", kind::TOOL_RESULT)));
	let result = journal
		.lines()
		.filter(|line| line.starts_with("data: "))
		.filter(|line| line.contains("outcome") || line.contains("fault"))
		.collect::<Vec<_>>()
		.join("\n");
	let landed = target.exists();
	drop(kernel);
	drop(environment);
	Turn { session, result, landed }
}

fn write_call(target: &Path) -> serde_json::Value {
	serde_json::json!({
		"path": target,
		"content": "approved content\n",
		"i": "Proving approval routing",
	})
}

fn bash_call(_target: &Path) -> serde_json::Value {
	serde_json::json!({ "command": "touch landed.txt", "i": "Proving approval routing" })
}

#[tokio::test]
async fn approval_always_ask_write_deny_journals_a_denied_result() {
	let Turn { session, result, landed } = run(
		"write",
		write_call,
		"approved.txt",
		ApprovalMode::AlwaysAsk,
		ExecSandboxMode::Off,
		false,
	)
	.await;
	let tickets = prompts(&session);
	assert_eq!(tickets.len(), 1, "one journaled approval prompt: {tickets:?}");
	assert_eq!(tickets[0].reasons[0].kind.as_str(), "tool");
	assert_eq!(tickets[0].reasons[0].subject.as_str(), "write");
	assert_eq!(tickets[0].state, TicketState::Decided);
	assert!(
		tickets[0]
			.decision
			.as_ref()
			.is_some_and(|decision| !decision.approved)
	);
	assert!(result.contains("denied by user: not today"), "denied write must settle: {result}");
	assert!(!landed, "denied write never ran");
}

#[tokio::test]
async fn approval_always_ask_write_allow_runs_the_tool() {
	let Turn { session, result, landed } =
		run("write", write_call, "approved.txt", ApprovalMode::AlwaysAsk, ExecSandboxMode::Off, true)
			.await;
	let tickets = prompts(&session);
	assert_eq!(tickets.len(), 1);
	assert_eq!(tickets[0].state, TicketState::Decided);
	assert!(
		tickets[0]
			.decision
			.as_ref()
			.is_some_and(|decision| decision.approved)
	);
	assert!(landed, "approved write ran: {result}");
	assert!(result.contains("\"kind\":\"ok\""), "approved write settled ok: {result}");
}

/// The downgrade, end to end: the user asked for `yolo`, no sandbox is on, so
/// `write` is in force and the unconfined shell is process authority that
/// prompts. Denial stops the command; one typed notice names what was asked,
/// what holds, and why.
#[tokio::test]
async fn explicit_yolo_without_a_sandbox_prompts_for_bash_and_says_why() {
	let Turn { session, result, landed } =
		run("bash", bash_call, "landed.txt", ApprovalMode::Yolo, ExecSandboxMode::Off, false).await;
	let tickets = prompts(&session);
	assert_eq!(tickets.len(), 1, "yolo without a sandbox must prompt exactly once: {tickets:?}");
	assert_eq!(tickets[0].reasons[0].kind.as_str(), "exec");
	assert_eq!(tickets[0].reasons[0].subject.as_str(), "touch landed.txt");
	assert!(
		tickets[0]
			.decision
			.as_ref()
			.is_some_and(|decision| !decision.approved)
	);
	assert!(result.contains("denied by user: not today"), "denied bash must settle: {result}");
	assert!(!landed, "a denied command never ran");

	let notices = session
		.dom()
		.select("body turn notice[name=approval-downgrade]")
		.expect("selector")
		.collect::<Vec<_>>();
	assert_eq!(notices.len(), 1, "one downgrade notice per session");
	let notice = session.dom().get(notices[0]).expect("notice node");
	let Some(omp_dom::Value::Json(data)) = notice.prop(&omp_dom::PropId::Data.into()) else {
		panic!("the notice carries its typed payload");
	};
	assert_eq!(
		serde_json::from_str::<serde_json::Value>(data.get()).expect("payload"),
		serde_json::json!({
			"configured": "yolo",
			"effective": "write",
			"sandbox": { "state": "off" },
		})
	);
}

/// The same prompt, approved: the command runs, and the downgrade did not
/// block the session.
#[tokio::test]
async fn explicit_yolo_without_a_sandbox_runs_bash_once_approved() {
	let Turn { session, result, landed } =
		run("bash", bash_call, "landed.txt", ApprovalMode::Yolo, ExecSandboxMode::Off, true).await;
	assert_eq!(prompts(&session).len(), 1);
	assert!(landed, "approved bash ran: {result}");
}
