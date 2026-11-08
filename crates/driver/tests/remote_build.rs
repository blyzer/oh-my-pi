//! The `remote_build` capability on a real kernel turn: a scripted model calls
//! the registered tool, the host answers its approval prompt with
//! `Up::Approve`, and the producer journals the execution job. The backend is a
//! fake that counts connections and starts (a REAPI upload happens in
//! `start`), and its run settles at once, so no test touches the network and
//! no turn waits on remote work.

use std::{
	collections::VecDeque,
	future::ready,
	sync::{
		Arc,
		atomic::{AtomicUsize, Ordering},
	},
	time::Duration,
};

use futures::stream;
use omp_agent::{
	ApprovalDecision, ApprovalScope, ApprovalSource, ApprovalTicket, DispatchPolicy, Inference,
	JobBoard, JobRecord, Kernel, KernelEvent, RunControl, StaticPrompt, TicketState, TurnInput, Up,
};
use omp_ai::{
	BlockKind, ChatEvent, ChatRequest, ChatStream, Completion, ContentPart, ExecutionReceipt,
	FinishReason, RequestId, ResponseMeta, ToolCall, ToolCallId, ToolResultContent, Usage,
};
use omp_catalog::{ProviderId, RouteId};
use omp_core::Str;
use omp_dom::{PropKey, Value};
use omp_driver::{
	exec_composer::{
		ExecutionSettings, PreparedExecution, REMOTE_POSTURE, RemoteError, SV_EXEC_REMOTE_ENDPOINT,
		SV_EXEC_REMOTE_IMAGE,
	},
	remote_build::{RemoteBuildHost, RemoteBuildTool, SV_EXEC_AGENT_RUNS, enabled},
};
use omp_exec::{
	BackendCapabilities, BackendId, BuildCommand, BuildSystemAdapter as _, CancelOutcome, CargoJust,
	ExecutionBackend, ExecutionError, ExecutionEvent, ExecutionId, ExecutionRequest, ExecutionRun,
	ReattachContext, ReattachHandle, RepoPins,
};
use omp_session::{ComponentRegistry, Session, components::jobs};
use parking_lot::Mutex;
use serde_json::{Value as Json, json};
use tokio_util::sync::CancellationToken;

/// A scripted model: the request that opens a turn makes the next scripted
/// `remote_build` call, under its scripted call id, and every other request
/// (after a tool result or a job's async result) closes the turn.
struct Script {
	calls:  VecDeque<(&'static str, Json)>,
	/// Turn inputs seen so far: a request carrying one more opens a turn.
	opened: usize,
	seen:   Arc<Mutex<Seen>>,
}

/// What the scripted model was sent.
#[derive(Default)]
struct Seen {
	/// The tool names each request offered, in request order.
	offered: Vec<Vec<Str>>,
	/// The tool results the latest request carried: text, and whether each is
	/// an error.
	results: Vec<(String, bool)>,
}

impl Inference for Script {
	fn chat(
		&mut self,
		request: ChatRequest,
	) -> impl Future<Output = Result<ChatStream, omp_ai::Error>> + Send {
		{
			let mut seen = self.seen.lock();
			seen
				.offered
				.push(request.tools.iter().map(|tool| tool.name.clone()).collect());
			seen.results = results(&request);
		}
		let inputs = turn_inputs(&request);
		let call = if inputs > self.opened {
			self.opened = inputs;
			self.calls.pop_front()
		} else {
			None
		};
		let events = match call {
			Some((id, arguments)) => tool_call(id, &arguments),
			None => closing_text(),
		};
		ready(Ok(ChatStream::ordinary(Box::pin(stream::iter(events.into_iter().map(Ok))))))
	}
}

/// How many turn inputs `request` carries.
fn turn_inputs(request: &ChatRequest) -> usize {
	request
		.messages
		.iter()
		.flat_map(|message| message.content.iter())
		.filter(|part| matches!(part, ContentPart::Text { text, .. } if text.contains(TURN_INPUT)))
		.count()
}

const TURN_INPUT: &str = "run the build";

fn results(request: &ChatRequest) -> Vec<(String, bool)> {
	request
		.messages
		.iter()
		.flat_map(|message| message.content.iter())
		.filter_map(|part| match part {
			ContentPart::ToolResult { content, is_error, .. } => Some((
				content
					.iter()
					.filter_map(|content| match content {
						ToolResultContent::Text(text) => Some(text.as_str()),
						_ => None,
					})
					.collect(),
				*is_error,
			)),
			_ => None,
		})
		.collect()
}

fn meta() -> ResponseMeta {
	ResponseMeta {
		request_id:          RequestId::from("remote-build-test"),
		provider:            ProviderId::from("test"),
		route:               RouteId::from("test/route"),
		model:               None,
		provider_request_id: None,
		created_at:          std::time::SystemTime::UNIX_EPOCH,
	}
}

/// One `remote_build` call with the provider call id `id`.
fn tool_call(id: &str, arguments: &Json) -> Vec<ChatEvent> {
	let call = ToolCall {
		id:        ToolCallId::from(id),
		name:      Str::new_static("remote_build"),
		arguments: omp_ai::OpaqueJson::new(arguments.clone()),
	};
	vec![
		ChatEvent::Started(meta()),
		ChatEvent::ToolCallStarted { index: 0, id: call.id.clone(), name: call.name.clone() },
		ChatEvent::ToolArgumentsDelta {
			index: 0,
			bytes: bytes::Bytes::from(serde_json::to_vec(arguments).expect("arguments")),
		},
		ChatEvent::ToolCallReady { index: 0, call },
		ChatEvent::Completed(Completion {
			reason:  FinishReason::ToolCalls,
			blocks:  1,
			usage:   Usage::default(),
			receipt: ExecutionReceipt::default().into(),
		}),
	]
}

fn closing_text() -> Vec<ChatEvent> {
	vec![
		ChatEvent::Started(meta()),
		ChatEvent::BlockStarted { index: 0, kind: BlockKind::Text },
		ChatEvent::TextDelta { index: 0, text: Str::new_static("done") },
		ChatEvent::Completed(Completion {
			reason:  FinishReason::Stop,
			blocks:  1,
			usage:   Usage::default(),
			receipt: ExecutionReceipt::default().into(),
		}),
	]
}

/// The host half of a remote build, recording every call it gets.
#[derive(Clone, Default)]
struct FakeHost(Arc<HostCalls>);

#[derive(Default)]
struct HostCalls {
	prepared:  AtomicUsize,
	/// The transport URL of every connection, in order.
	connected: Mutex<Vec<String>>,
	/// Backend starts: a REAPI upload happens in `start`.
	started:   AtomicUsize,
}

impl FakeHost {
	/// (prepares, connections, starts) so far.
	fn calls(&self) -> (usize, usize, usize) {
		(
			self.0.prepared.load(Ordering::SeqCst),
			self.0.connected.lock().len(),
			self.0.started.load(Ordering::SeqCst),
		)
	}

	fn connected(&self) -> Vec<String> {
		self.0.connected.lock().clone()
	}

	/// Waits for the job's starting owner to call `start`.
	async fn started(&self, count: usize) {
		tokio::time::timeout(Duration::from_secs(5), async {
			while self.0.started.load(Ordering::SeqCst) < count {
				tokio::time::sleep(Duration::from_millis(1)).await;
			}
		})
		.await
		.expect("the starting owner calls start");
	}
}

impl RemoteBuildHost for FakeHost {
	type Backend = FakeBackend;

	fn prepare<'a>(
		&'a self,
		_settings: &'a ExecutionSettings,
		command: &'a BuildCommand,
	) -> impl Future<Output = Result<PreparedExecution, RemoteError>> + Send + 'a {
		self.0.prepared.fetch_add(1, Ordering::SeqCst);
		let pins = pins();
		ready(
			CargoJust::default()
				.plan(command, &pins)
				.map(|plan| PreparedExecution { request: plan.requests[0].clone(), pins })
				.map_err(RemoteError::Plan),
		)
	}

	fn connect<'a>(
		&'a self,
		settings: &'a ExecutionSettings,
		_pins: &'a RepoPins,
		_cancel: &'a CancellationToken,
	) -> impl Future<Output = Result<FakeBackend, RemoteError>> + Send + 'a {
		self
			.0
			.connected
			.lock()
			.push(settings.destination_identity().endpoint.to_string());
		ready(Ok(FakeBackend {
			id:           BackendId::new_static("remote"),
			capabilities: capabilities(),
			calls:        Arc::clone(&self.0),
		}))
	}
}

#[derive(Clone)]
struct FakeBackend {
	id:           BackendId,
	capabilities: BackendCapabilities,
	calls:        Arc<HostCalls>,
}

impl ExecutionBackend for FakeBackend {
	type Run = FakeRun;

	fn id(&self) -> &BackendId<str> {
		&self.id
	}

	fn capabilities(&self) -> &BackendCapabilities {
		&self.capabilities
	}

	async fn start(&self, _request: ExecutionRequest) -> Result<FakeRun, ExecutionError> {
		self.calls.started.fetch_add(1, Ordering::SeqCst);
		Ok(FakeRun { settled: false })
	}

	async fn reattach(
		&self,
		_handle: ReattachHandle,
		_context: ReattachContext,
	) -> Result<FakeRun, ExecutionError> {
		Err(ExecutionError::ResumeUnsupported { backend: self.id.clone() })
	}
}

/// A run that settles on its first event: the backend had no capacity.
struct FakeRun {
	settled: bool,
}

impl ExecutionRun for FakeRun {
	async fn next_event(&mut self) -> Option<ExecutionEvent> {
		(!std::mem::replace(&mut self.settled, true)).then(|| {
			ExecutionEvent::Settled(Err(ExecutionError::Capacity {
				backend: BackendId::new_static("remote"),
			}))
		})
	}

	async fn cancel(&self) -> CancelOutcome {
		CancelOutcome::Cancelled
	}

	fn detach(self) -> ReattachHandle {
		ReattachHandle::new(
			BackendId::new_static("remote"),
			Str::new_static("operations/1"),
			ExecutionId::new_static("detached"),
		)
	}
}

fn capabilities() -> BackendCapabilities {
	BackendCapabilities {
		platforms:      [omp_exec::Platform::LINUX_X86_64].into(),
		needs:          Default::default(),
		max_timeout:    None,
		action_cache:   omp_exec::CacheSupport::None,
		cancel:         omp_exec::CancelSupport::BestEffort,
		resume:         omp_exec::ResumeSupport::Reattach,
		max_blob_bytes: None,
		warm_caches:    false,
		enforced:       REMOTE_POSTURE,
	}
}

fn pins() -> RepoPins {
	RepoPins {
		rust_toolchain: Str::new_static("nightly-2026-08-08"),
		nextest:        Str::new_static("0.9.146"),
		cpython_tag:    Str::new_static("20260807"),
		cpython:        Str::new_static("3.14.7"),
	}
}

/// The operator's console: a digest-pinned image and `endpoint`, with agent
/// runs left at their default.
fn operator(endpoint: &'static str) -> Arc<omp_con::Ctx> {
	let ctx = omp_con::Ctx::new();
	SV_EXEC_REMOTE_ENDPOINT
		.set(&ctx, Str::new_static(endpoint))
		.expect("endpoint");
	SV_EXEC_REMOTE_IMAGE
		.set(&ctx, Str::new_static("image@sha256:abc"))
		.expect("image");
	Arc::new(ctx)
}

/// [`operator`] with `sv_exec_agent_runs` set.
fn agent_runs(endpoint: &'static str) -> Arc<omp_con::Ctx> {
	let ctx = operator(endpoint);
	SV_EXEC_AGENT_RUNS.set(&ctx, true).expect("agent runs");
	ctx
}

fn test_pkg() -> Json {
	json!({ "recipe": "test-pkg", "package": "omp-exec", "i": "Testing remotely" })
}

/// The user's answer. An approval is given for the whole session, the widest
/// answer there is, so a test can show it covers no other prompt.
fn answer(approved: bool) -> ApprovalDecision {
	ApprovalDecision {
		approved,
		scope: if approved {
			ApprovalScope::Session
		} else {
			ApprovalScope::Once
		},
		source: ApprovalSource::User,
		decided_by: None,
		reason: None,
		audited: false,
	}
}

/// How the host answers one approval prompt.
#[derive(Clone, Copy)]
enum Reply {
	/// [`answer`]: approve or deny.
	Answer(bool),
	/// Interrupt the turn instead of answering.
	Interrupt,
}

/// One kernel composed with the capability as `compose_kernel` composes it,
/// serving a fresh session, and a host that answers every approval prompt.
struct Fixture {
	_scratch: tempfile::TempDir,
	kernel:   Kernel<Script>,
	session:  Session,
	host:     FakeHost,
	seen:     Arc<Mutex<Seen>>,
	/// Approval prompts the host was asked to answer.
	asked:    Arc<AtomicUsize>,
	answers:  tokio::task::JoinHandle<()>,
}

impl Fixture {
	/// `reply` answers each prompt from its 1-based number and the host its
	/// requirement names.
	fn compose(
		ctx: Arc<omp_con::Ctx>,
		calls: Vec<(&'static str, Json)>,
		reply: fn(usize, &str) -> Reply,
	) -> Self {
		let scratch = tempfile::tempdir().expect("scratch");
		let spill =
			omp_journal::blob::BlobStore::open(scratch.path().join("artifacts")).expect("spill");
		let session = Session::create_with_blob_store(
			scratch.path().join("remote-build.oms"),
			ComponentRegistry::standard(),
			spill.clone(),
		)
		.expect("session");
		let remote_build = enabled(&ctx);
		let registry = omp_tool::Registry::new();
		let registry = match remote_build {
			Some(remote_build) => remote_build
				.declare(&registry)
				.expect("declare remote_build"),
			None => registry,
		};
		let seen = Arc::<Mutex<Seen>>::default();
		let model = Script { calls: calls.into(), opened: 0, seen: Arc::clone(&seen) };
		let mut kernel = Kernel::new(
			model,
			Arc::new(registry),
			DispatchPolicy::new(spill),
			StaticPrompt(Str::new_static("test")),
		);
		let host = FakeHost::default();
		if let Some(remote_build) = remote_build {
			let approvals = kernel.approval_route();
			kernel = kernel.with_session_tool(Arc::new(RemoteBuildTool::new(
				remote_build,
				ctx,
				approvals,
				host.clone(),
				Str::new_static("Main"),
			)));
		}
		let events = kernel.subscribe();
		let mailbox = kernel.mailbox();
		let asked = Arc::new(AtomicUsize::new(0));
		let answers = tokio::spawn({
			let asked = Arc::clone(&asked);
			async move {
				while let Ok(event) = events.recv_async().await {
					if let KernelEvent::ApprovalRequested(ticket) = event {
						let number = asked.fetch_add(1, Ordering::SeqCst) + 1;
						let host = ticket
							.reasons
							.first()
							.map_or("", |reason| reason.subject.as_str());
						let message = match reply(number, host) {
							Reply::Answer(approved) => {
								Up::approve_local(ticket.ticket_id, answer(approved))
							},
							Reply::Interrupt => Up::Interrupt,
						};
						let _ = mailbox.send(message);
					}
				}
			}
		});
		Self { _scratch: scratch, kernel, session, host, seen, asked, answers }
	}

	async fn turn(&mut self) {
		tokio::time::timeout(
			Duration::from_secs(60),
			self.kernel.run_turn(
				&mut self.session,
				TurnInput { text: Str::new_static(TURN_INPUT), attachments: Vec::new() },
				RunControl::default(),
			),
		)
		.await
		.expect("the turn settles")
		.expect("turn");
	}

	/// Every approval prompt the session journaled, in filing order.
	fn prompts(&self) -> Vec<ApprovalTicket> {
		let dom = self.session.dom();
		let prompts = omp_session::components::prompts::prompts_handle(dom).expect("prompts");
		dom.children(prompts)
			.iter()
			.filter_map(|handle| dom.get(*handle))
			.filter_map(|node| {
				node
					.prop(&PropKey::Custom(Str::new_static("ticket")))
					.and_then(Value::as_str)
			})
			.map(|encoded| serde_json::from_str(encoded).expect("ticket JSON"))
			.collect()
	}

	/// Every job the session journaled.
	fn jobs(&self) -> Vec<JobRecord> {
		let board = JobBoard::new();
		board.rebuild(&self.session);
		board.list()
	}

	/// The model-visible text of the latest tool result, and whether it is an
	/// error.
	fn last_result(&self) -> (String, bool) {
		self
			.seen
			.lock()
			.results
			.last()
			.cloned()
			.expect("a tool result")
	}

	/// Whether any request offered the model `remote_build`.
	fn offered(&self) -> bool {
		self
			.seen
			.lock()
			.offered
			.iter()
			.any(|tools| tools.iter().any(|name| name.as_str() == "remote_build"))
	}
}

impl Drop for Fixture {
	fn drop(&mut self) {
		self.answers.abort();
	}
}

#[tokio::test]
async fn remote_build_is_off_by_default_and_unavailable() {
	let ctx = operator("grpcs://build-a.example");
	assert!(!SV_EXEC_AGENT_RUNS.get(&ctx));
	assert!(enabled(&ctx).is_none());
	let mut fixture = Fixture::compose(ctx, Vec::new(), |_, _| Reply::Answer(true));
	fixture.turn().await;
	assert!(!fixture.offered(), "the model was offered remote_build");
	assert!(matches!(
		fixture.kernel.tool_registry().live_spec("remote_build"),
		Err(omp_tool::RegistryError::UnknownTool { .. })
	));
	assert_eq!(fixture.host.calls(), (0, 0, 0));
}

#[tokio::test]
async fn a_denied_prompt_connects_uploads_and_journals_nothing() {
	let mut fixture = Fixture::compose(
		agent_runs("grpcs://build-a.example"),
		vec![("call-1", test_pkg())],
		|_, _| Reply::Answer(false),
	);
	fixture.turn().await;
	assert!(fixture.offered(), "the model was not offered remote_build");
	let prompts = fixture.prompts();
	assert_eq!(prompts.len(), 1, "{prompts:?}");
	let prompt = &prompts[0];
	assert_eq!(prompt.invocation_id.as_deref(), Some("call-1"));
	assert_eq!(prompt.state, TicketState::Decided);
	assert_eq!(prompt.decision.as_ref().map(|decision| decision.approved), Some(false));
	assert_eq!(prompt.reasons.len(), 1, "{prompt:?}");
	let reason = &prompt.reasons[0];
	assert_eq!(reason.kind.as_str(), "network");
	assert_eq!(reason.subject.as_str(), "build-a.example");
	assert_eq!(reason.scopes, [Str::new_static("once")]);
	assert_eq!(reason.default, Some(false));
	assert_eq!(reason.unreachable.as_str(), "deny");
	assert!(reason.require_human);
	assert_eq!(fixture.host.calls(), (1, 0, 0), "capture only: no connection and no start");
	assert!(fixture.jobs().is_empty(), "a denied run journals no job");
	let (result, is_error) = fixture.last_result();
	assert!(is_error, "{result}");
	assert!(result.contains(r#""kind":"not_approved","host":"build-a.example""#), "{result}");
}

#[tokio::test]
async fn a_grant_for_host_a_does_not_authorize_host_b() {
	let ctx = agent_runs("grpcs://build-a.example");
	let calls = vec![("call-1", test_pkg()), ("call-2", test_pkg())];
	let mut fixture =
		Fixture::compose(Arc::clone(&ctx), calls, |_, host| Reply::Answer(host == "build-a.example"));
	fixture.turn().await;
	let executions = fixture.jobs();
	assert_eq!(executions.len(), 1, "{executions:?}");
	let job = &executions[0];
	let (result, is_error) = fixture.last_result();
	assert!(!is_error, "{result}");
	assert!(result.contains(job.id.as_str()), "{result}");
	assert!(result.contains("https://build-a.example:443"), "{result}");
	fixture.host.started(1).await;
	assert_eq!(fixture.host.connected(), ["https://build-a.example:443"]);
	let prompts = fixture.prompts();
	assert_eq!(prompts.len(), 1, "{prompts:?}");
	let node = fixture.session.dom().get(job.handle).expect("job node");
	let recorded = |key: &'static str| {
		node
			.prop(&PropKey::Custom(Str::new_static(key)))
			.and_then(Value::as_str)
			.map(str::to_owned)
	};
	let destination = serde_json::to_string(
		&ExecutionSettings::generic(&ctx)
			.expect("settings")
			.destination_identity(),
	)
	.expect("destination");
	assert_eq!(recorded(jobs::EXECUTION_DESTINATION), Some(destination));
	assert_eq!(
		recorded(jobs::EXECUTION_APPROVAL).as_deref(),
		Some(prompts[0].ticket_id.as_str()),
		"the job names the journaled prompt that approved it"
	);
	assert_eq!(
		recorded(jobs::EXECUTION_REQUEST).as_deref(),
		Some(r#"{"recipe":"test-pkg","package":"omp-exec"}"#)
	);

	SV_EXEC_REMOTE_ENDPOINT
		.set(&ctx, Str::new_static("grpcs://build-b.example"))
		.expect("endpoint B");
	fixture.turn().await;
	let prompts = fixture.prompts();
	assert_eq!(prompts.len(), 2, "host B is asked for again: {prompts:?}");
	assert_eq!(fixture.asked.load(Ordering::SeqCst), 2);
	assert_eq!(prompts[1].reasons[0].subject.as_str(), "build-b.example");
	assert_eq!(
		prompts[1]
			.decision
			.as_ref()
			.map(|decision| decision.approved),
		Some(false),
		"host A's session-wide answer decided nothing for host B"
	);
	assert_eq!(fixture.host.connected(), ["https://build-a.example:443"], "B is never reached");
	assert_eq!(fixture.host.calls(), (2, 1, 1));
	assert_eq!(fixture.jobs().len(), 1);
	let (result, is_error) = fixture.last_result();
	assert!(is_error, "{result}");
	assert!(result.contains(r#""kind":"not_approved","host":"build-b.example""#), "{result}");
}

/// A call whose own prompt goes unanswered is not authorized by an earlier
/// approval journaled for the same call id, as a retried call's would be: it
/// connects to nothing.
#[tokio::test]
async fn an_earlier_approval_for_the_same_call_authorizes_nothing() {
	let calls = vec![("call-1", test_pkg()), ("call-1", test_pkg())];
	let mut fixture = Fixture::compose(agent_runs("grpcs://build-a.example"), calls, |number, _| {
		if number == 1 {
			Reply::Answer(true)
		} else {
			Reply::Interrupt
		}
	});
	fixture.turn().await;
	fixture.host.started(1).await;
	assert_eq!(fixture.jobs().len(), 1);

	fixture.turn().await;
	let prompts = fixture.prompts();
	assert_eq!(prompts.len(), 2, "{prompts:?}");
	assert!(
		prompts
			.iter()
			.all(|prompt| prompt.invocation_id.as_deref() == Some("call-1")),
		"{prompts:?}"
	);
	assert_eq!(
		prompts[0]
			.decision
			.as_ref()
			.map(|decision| decision.approved),
		Some(true)
	);
	assert_ne!(prompts[1].state, TicketState::Decided, "the second prompt was never answered");
	assert_eq!(fixture.host.calls(), (2, 1, 1), "the second call captured, then connected nothing");
	assert_eq!(fixture.jobs().len(), 1);
}

#[tokio::test]
async fn only_remote_eligible_cargo_just_recipes_run() {
	let calls = vec![
		("call-1", json!({ "recipe": "fmt", "i": "Formatting remotely" })),
		("call-2", json!({ "recipe": "cargo", "i": "Building remotely" })),
		("call-3", json!({ "recipe": "test-pkg", "package": "fmt", "i": "Testing remotely" })),
	];
	let refusals = [
		r#""kind":"ineligible","recipe":"fmt""#,
		r#""kind":"ineligible","recipe":"cargo""#,
		r#""kind":"invalid_package","package":"fmt""#,
	];
	let mut fixture =
		Fixture::compose(agent_runs("grpcs://build-a.example"), calls, |_, _| Reply::Answer(true));
	for refusal in refusals {
		fixture.turn().await;
		let (result, is_error) = fixture.last_result();
		assert!(is_error, "{result}");
		assert!(result.contains(refusal), "{result}");
	}
	assert!(fixture.prompts().is_empty());
	assert_eq!(fixture.asked.load(Ordering::SeqCst), 0);
	assert_eq!(fixture.host.calls(), (0, 0, 0), "refused before planning, asking or connecting");
	assert!(fixture.jobs().is_empty());
}
