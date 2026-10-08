//! Production approval composition: a real project environment under
//! `--approval-mode always-ask`, the kernel-bound approval route, and a
//! `write` call whose admission prompt is journaled and answered by the host
//! with `Up::Approve` (deny → skipped, allow → the file lands). With no
//! sandbox in force, the default `yolo` is downgraded to `write`, so a `bash`
//! call prompts; an explicit `yolo` is respected and runs unconfined. Each
//! says so in one typed notice. On the attached path a project daemon served
//! in this process runs the command, and its sandbox amendment reaches the
//! issuing session only through the approval relay. A network endpoint approved
//! for the session holds, on either path, until the conversation leaves the
//! journal that approved it, by a rewind or a session switch. A fetch is asked
//! once per host the environment names for it, and a session grant for one
//! host never covers another.

mod support;

use std::{
	future::ready,
	path::{Path, PathBuf},
	sync::Arc,
	time::Duration,
};

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
use omp_driver::headless::kernel::{
	EnvToolExecutor, SettingsAdmission, bind_environment_approvals,
};
use omp_envd::{
	AttachOptions, ProjectEnvironment, RegistryBridges,
	exec_settings::{ExecSandboxMode, SV_SANDBOX_MODE},
	tool_settings::ApprovalMode,
};
use omp_journal::kind;
use omp_session::{ComponentRegistry, Session};

/// One scripted tool call, then a closing text turn, for every turn the
/// kernel runs: turn `n` calls `call-n`. `write` declares document write
/// effects, so its tier is `write` and always-ask prompts before it starts.
/// `bash` declares no effects: its spawn/fs effects are confined by the
/// sandbox, and without one it is process authority.
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
		ready(Ok(tool_then_text(self.turns, self.tool, &self.arguments)))
	}
}

/// The `turn`th scripted response: an odd turn calls `tool` with `arguments`
/// as `call-n` (`n` counting the calls), an even turn closes with text.
fn tool_then_text(turn: usize, tool: &'static str, arguments: &serde_json::Value) -> ChatStream {
	let meta = ResponseMeta {
		request_id:          RequestId::from("approval-test"),
		provider:            ProviderId::from("test"),
		route:               RouteId::from("test/route"),
		model:               None,
		provider_request_id: None,
		created_at:          std::time::SystemTime::UNIX_EPOCH,
	};
	let events = if turn % 2 == 1 {
		let call = ToolCall {
			id:        ToolCallId::from(format!("call-{}", turn.div_ceil(2)).as_str()),
			name:      Str::new_static(tool),
			arguments: omp_ai::OpaqueJson::new(arguments.clone()),
		};
		vec![
			ChatEvent::Started(meta),
			ChatEvent::ToolCallStarted { index: 0, id: call.id.clone(), name: call.name.clone() },
			ChatEvent::ToolArgumentsDelta {
				index: 0,
				bytes: bytes::Bytes::from(serde_json::to_vec(arguments).expect("args")),
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
	ChatStream::ordinary(Box::pin(stream::iter(events.into_iter().map(Ok))))
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

/// One scratch project: its workspace, its project state directory, and the
/// session's control context.
struct Project {
	scratch: tempfile::TempDir,
	root:    PathBuf,
	state:   PathBuf,
	con:     Arc<omp_con::Ctx>,
}

impl Project {
	fn new(sandbox: ExecSandboxMode) -> Self {
		let scratch = tempfile::tempdir().expect("scratch");
		let root = scratch.path().join("workspace");
		let state = scratch.path().join("state");
		std::fs::create_dir_all(&root).expect("workspace");
		std::fs::create_dir_all(&state).expect("state");
		let root = std::fs::canonicalize(&root).expect("canonical workspace");
		Self { scratch, root, state, con: context(sandbox) }
	}

	/// Attaches one session composition the way the application does.
	async fn attach(&self, mode: Option<ApprovalMode>) -> ProjectEnvironment {
		ProjectEnvironment::attach(&self.root, &self.state, AttachOptions {
			py_eval:            false,
			approval_mode:      mode,
			trusted_extensions: Vec::new(),
			contributed_values: Vec::new(),
			con:                Arc::clone(&self.con),
			bridges:            RegistryBridges::default(),
			spawn_idle_timeout: Some(2),
		})
		.await
		.expect("environment")
	}

	/// Runs one turn calling `tool` under `mode` (`None`: the shipped default
	/// posture, no flag) against `environment`; `approve` answers every prompt
	/// the kernel journals. `target` is the file the call creates, relative to
	/// the workspace.
	async fn turn(
		&self,
		environment: ProjectEnvironment,
		tool: &'static str,
		arguments: impl FnOnce(&Path) -> serde_json::Value,
		target: &str,
		mode: Option<ApprovalMode>,
		approve: bool,
	) -> Turn {
		let target = self.root.join(target);
		let registry = environment.registry();
		let spill =
			omp_journal::blob::BlobStore::open(self.scratch.path().join("artifacts")).expect("spill");
		let kernel = Kernel::new(
			ToolThenText { tool, arguments: arguments(&target), turns: 0 },
			registry,
			DispatchPolicy::new(spill.clone()),
			StaticPrompt(Str::new_static("test")),
		);
		let approvals = bind_environment_approvals(&kernel, &environment);
		let notices = kernel.mailbox();
		let mut kernel = kernel
			.with_external_executor(Arc::new(EnvToolExecutor::new(
				environment.client().clone(),
				approvals,
			)))
			.with_tool_admission(Arc::new(
				SettingsAdmission::new(&self.con, mode, &self.root).with_notices(notices),
			));
		let events = kernel.subscribe();
		let mailbox = kernel.mailbox();
		let host = tokio::spawn(async move {
			while let Ok(event) = events.recv_async().await {
				if let KernelEvent::ApprovalRequested(ticket) = event {
					// A sandbox amendment is raised by the executor after the call, so
					// it carries no invocation id; every admission ticket carries this
					// one.
					if ticket.invocation_id.is_some() {
						assert_eq!(ticket.invocation_id.as_deref(), Some("call-1"));
					}
					let _ = mailbox
						.send(Up::Approve { id: ticket.ticket_id, decision: decision(approve) });
				}
			}
		});
		let mut session = Session::create_with_blob_store(
			self.scratch.path().join("approval.oms"),
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
}

/// Runs one turn calling `tool` under `mode` (`None`: the shipped default
/// posture, no flag), with the sandbox `sandbox`, on the composition
/// `ProjectEnvironment::attach` builds from this test binary; `approve`
/// answers the prompt the kernel journals. `target` is the file the call
/// creates, relative to the workspace.
async fn run(
	tool: &'static str,
	arguments: impl FnOnce(&Path) -> serde_json::Value,
	target: &str,
	mode: Option<ApprovalMode>,
	sandbox: ExecSandboxMode,
	approve: bool,
) -> Turn {
	let project = Project::new(sandbox);
	let environment = project.attach(mode).await;
	project
		.turn(environment, tool, arguments, target, mode, approve)
		.await
}

/// A control context with `sandbox` set, as one process loads it from the
/// user's configuration.
fn context(sandbox: ExecSandboxMode) -> Arc<omp_con::Ctx> {
	let con = omp_con::Ctx::new();
	SV_SANDBOX_MODE.set(&con, sandbox).expect("sandbox mode");
	Arc::new(con)
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
		Some(ApprovalMode::AlwaysAsk),
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
	let Turn { session, result, landed } = run(
		"write",
		write_call,
		"approved.txt",
		Some(ApprovalMode::AlwaysAsk),
		ExecSandboxMode::Off,
		true,
	)
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

/// The typed posture notice the session journaled, as JSON.
fn posture_notice(session: &Session) -> serde_json::Value {
	let notices = session
		.dom()
		.select("body turn notice")
		.expect("selector")
		.filter(|handle| {
			session
				.dom()
				.get(*handle)
				.and_then(|node| node.prop(&omp_dom::PropKey::Custom(Str::new_static("name"))))
				.and_then(omp_dom::Value::as_str)
				== Some("approval-posture")
		})
		.collect::<Vec<_>>();
	assert_eq!(notices.len(), 1, "exactly one posture notice per session");
	let notice = session.dom().get(notices[0]).expect("notice node");
	let Some(omp_dom::Value::Json(data)) = notice.prop(&omp_dom::PropId::Data.into()) else {
		panic!("the notice carries its typed payload");
	};
	serde_json::from_str(data.get()).expect("payload")
}

/// The shipped default, end to end: `yolo` is the default posture, no sandbox
/// is in force, so `write` holds and the unconfined shell is process authority
/// that prompts. Denial stops the command; one typed notice names what the
/// default asked for, what holds, and why.
#[tokio::test]
async fn default_yolo_without_a_sandbox_prompts_for_bash_and_says_why() {
	let Turn { session, result, landed } =
		run("bash", bash_call, "landed.txt", None, ExecSandboxMode::Off, false).await;
	let tickets = prompts(&session);
	assert_eq!(tickets.len(), 1, "a defaulted yolo without a sandbox must prompt once: {tickets:?}");
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
	assert_eq!(
		posture_notice(&session),
		serde_json::json!({
			"configured": "yolo",
			"provenance": "default",
			"effective": "write",
			"sandbox": { "state": "off" },
		})
	);
}

/// The same prompt, approved: the command runs.
#[tokio::test]
async fn default_yolo_without_a_sandbox_runs_bash_once_approved() {
	let Turn { session, result, landed } =
		run("bash", bash_call, "landed.txt", None, ExecSandboxMode::Off, true).await;
	assert_eq!(prompts(&session).len(), 1);
	assert!(landed, "approved bash ran: {result}");
}

/// An explicit `yolo` (`--approval-mode yolo`, `--yolo`, the user's config)
/// is respected without a sandbox: the command runs with no prompt, and the
/// session is told it is unconfined. This is the way out of headless denial.
#[tokio::test]
async fn explicit_yolo_without_a_sandbox_runs_bash_unprompted_and_says_so() {
	let Turn { session, result, landed } =
		run("bash", bash_call, "landed.txt", Some(ApprovalMode::Yolo), ExecSandboxMode::Off, false)
			.await;
	assert!(prompts(&session).is_empty(), "an explicit yolo never prompts");
	assert!(landed, "bash ran unprompted: {result}");
	assert_eq!(
		posture_notice(&session),
		serde_json::json!({
			"configured": "yolo",
			"provenance": "explicit",
			"effective": "yolo",
			"sandbox": { "state": "off" },
		})
	);
}

/// A real Seatbelt sandbox is active: the default `yolo` is honoured, so bash
/// runs unprompted and no posture notice is needed.
#[cfg(target_os = "macos")]
#[tokio::test]
async fn default_yolo_inside_an_active_sandbox_runs_bash_unprompted_and_silent() {
	let Turn { session, result, landed } =
		run("bash", bash_call, "landed.txt", None, ExecSandboxMode::WorkspaceWrite, false).await;
	assert!(prompts(&session).is_empty(), "a confined yolo never prompts");
	assert!(landed, "bash ran inside the workspace: {result}");
	let posture = session
		.dom()
		.select("body turn notice")
		.expect("selector")
		.filter(|handle| {
			session
				.dom()
				.get(*handle)
				.and_then(|node| node.prop(&omp_dom::PropKey::Custom(Str::new_static("name"))))
				.and_then(omp_dom::Value::as_str)
				== Some("approval-posture")
		})
		.count();
	assert_eq!(posture, 0, "an honoured yolo posts no posture notice");
}

/// The same sandbox refuses a write outside its roots: the denial becomes a
/// one-time, path-scoped `sandbox_amendment` prompt (ADR 0028), and a refusal
/// leaves the outside path untouched.
#[cfg(target_os = "macos")]
#[tokio::test]
async fn active_sandbox_denied_write_prompts_an_amendment_and_refusal_writes_nothing() {
	let outside = Path::new("/Users/Shared/omp-seatbelt-amendment-probe");
	let _ = std::fs::remove_file(outside);
	let Turn { session, landed, .. } = run(
		"bash",
		|_| {
			serde_json::json!({
				"command": format!("touch {}", outside.display()),
				"i": "Proving sandbox amendment",
			})
		},
		"landed.txt",
		None,
		ExecSandboxMode::WorkspaceWrite,
		false,
	)
	.await;
	let wrote = outside.exists();
	let _ = std::fs::remove_file(outside);
	let tickets = prompts(&session);
	assert_eq!(tickets.len(), 1, "one amendment prompt: {tickets:?}");
	assert_eq!(tickets[0].reasons[0].kind.as_str(), "sandbox_amendment");
	assert_eq!(tickets[0].reasons[0].subject.as_str(), "write /Users/Shared");
	assert!(!wrote && !landed, "a refused amendment writes nothing");
}

/// The default composition on its production path: `ProjectEnvironment::attach`
/// joins a project daemon instead of embedding an environment, and that
/// daemon's host binds no approval route, so a sandbox amendment of a command
/// it runs can reach a human only through the relay of the session that issued
/// the command.
#[cfg(target_os = "macos")]
mod attached_daemon {
	use omp_core::{Principal, sf};
	use omp_envd::{EnvServer, exthost::ConvarControlFactory, worker::ExtHostConfig};
	use omp_tool::Registry;
	use tokio::{net::UnixStream, task::JoinHandle};
	use tokio_util::sync::CancellationToken;

	use super::*;
	pub use crate::support::InProcessDaemon;

	impl InProcessDaemon {
		async fn for_project(project: &Project, sandbox: ExecSandboxMode) -> Self {
			Self::serve(&project.root, &project.state, context(sandbox)).await
		}
	}

	/// Writes into `.git`, which the workspace-write sandbox protects, so the
	/// command is denied and offered as a one-time amendment.
	fn protected_write(_target: &Path) -> serde_json::Value {
		serde_json::json!({
			"command": "echo amended > .git/amended.txt",
			"i": "Proving the daemon approval relay",
		})
	}

	/// A session attached to the daemon, which must not have fallen back to an
	/// embedded environment.
	pub async fn attached(project: &Project) -> ProjectEnvironment {
		let environment = project.attach(None).await;
		assert!(
			environment.fallback_notice.is_none(),
			"the session fell back to an embedded environment: {:?}",
			environment.fallback_notice
		);
		environment
	}

	/// The one journaled amendment prompt, decided as `approved`.
	fn amendment(project: &Project, session: &Session, approved: bool) {
		let tickets = prompts(session);
		assert_eq!(tickets.len(), 1, "one amendment prompt: {tickets:?}");
		let ticket = &tickets[0];
		assert_eq!(ticket.invocation_id, None, "a sandbox amendment blocks no invocation");
		assert_eq!(ticket.reasons[0].kind.as_str(), "sandbox_amendment");
		assert_eq!(
			ticket.reasons[0].subject.as_str(),
			format!("write {}", project.root.join(".git").display()),
		);
		assert_eq!(ticket.reasons[0].pattern.as_deref(), Some("echo amended > .git/amended.txt"));
		assert_eq!(ticket.state, TicketState::Decided);
		assert_eq!(
			ticket.decision.as_ref().map(|decision| decision.approved),
			Some(approved),
			"{ticket:?}"
		);
	}

	/// Approved: the daemon reruns the command once with the amended scope and
	/// the write lands. Only the issuing session is asked; another session
	/// attached to the same daemon never is.
	#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
	async fn an_approved_daemon_amendment_reruns_the_command() {
		let project = Project::new(ExecSandboxMode::WorkspaceWrite);
		std::fs::create_dir(project.root.join(".git")).expect("protected carve-out");
		let _daemon = InProcessDaemon::for_project(&project, ExecSandboxMode::WorkspaceWrite).await;
		let bystander = attached(&project).await;
		let (bystander_route, bystander_inbox) =
			omp_agent::ApprovalRoute::new(Arc::new(omp_agent::ApprovalBook::new()), None);
		bystander.bind_approval_authority(None, Some(bystander_route));
		let environment = attached(&project).await;
		let Turn { session, result, landed } = project
			.turn(environment, "bash", protected_write, ".git/amended.txt", None, true)
			.await;
		amendment(&project, &session, true);
		assert!(landed, "the approved rerun did not write: {result}");
		assert_eq!(
			std::fs::read_to_string(project.root.join(".git/amended.txt")).expect("amended write"),
			"amended\n"
		);
		assert!(
			bystander_inbox.try_recv().is_err(),
			"another session attached to the daemon was asked"
		);
	}

	/// Refused: the command ends `Denied` and nothing is written.
	#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
	async fn a_refused_daemon_amendment_denies_the_command() {
		let project = Project::new(ExecSandboxMode::WorkspaceWrite);
		std::fs::create_dir(project.root.join(".git")).expect("protected carve-out");
		let _daemon = InProcessDaemon::for_project(&project, ExecSandboxMode::WorkspaceWrite).await;
		let environment = attached(&project).await;
		let Turn { session, result, landed } = project
			.turn(environment, "bash", protected_write, ".git/amended.txt", None, false)
			.await;
		amendment(&project, &session, false);
		assert!(!landed, "a refused amendment wrote: {result}");
		assert!(result.contains("\"outcome\":\"denied\""), "the command was not denied: {result}");
	}
}

/// Network endpoints approved for the rest of the session, on both approval
/// bindings the production composition uses: the relay of a session attached to
/// a project daemon served in this process, and the in-process route of an
/// embedded composition. The kernel is wired by the production
/// `bind_environment_approvals`, so these prove that its session observer
/// revokes the environment's grants when the conversation leaves the journal
/// that approved them, by a rewind or by a switch to another session.
#[cfg(target_os = "macos")]
mod session_network_grants {
	use std::{
		fs,
		io::{BufRead as _, BufReader, Write as _},
		mem,
		net::{Ipv4Addr, TcpListener},
		thread,
	};

	use omp_journal::blob::BlobStore;
	use tokio::time;

	use super::{
		attached_daemon::{InProcessDaemon, attached},
		*,
	};

	/// A loopback HTTP server answering every request `200 ok`, standing in
	/// for one package host the broker may reach under
	/// `sv_sandbox_allow_localhost`. Its thread ends with the test process.
	fn loopback_upstream() -> u16 {
		let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("upstream");
		let port = listener.local_addr().expect("upstream address").port();
		thread::spawn(move || {
			for stream in listener.incoming() {
				let Ok(mut stream) = stream else { continue };
				let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
				let Ok(reading) = stream.try_clone() else {
					continue;
				};
				let mut reader = BufReader::new(reading);
				let mut line = String::new();
				while reader.read_line(&mut line).is_ok_and(|read| read > 0) && line != "\r\n" {
					line.clear();
				}
				let _ = stream
					.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\nConnection: close\r\n\r\nok\n");
			}
		});
		port
	}

	/// A control context with the workspace-write sandbox whose broker may
	/// reach loopback.
	fn loopback_context() -> Arc<omp_con::Ctx> {
		let con = context(ExecSandboxMode::WorkspaceWrite);
		con.run("sv_sandbox_allow_localhost 1")
			.expect("the broker may reach loopback");
		con
	}

	/// Runs one turn, whose scripted bash call the host answers.
	async fn fetch_turn(kernel: &mut Kernel<ToolThenText>, session: &mut Session) {
		time::timeout(
			Duration::from_secs(60),
			kernel.run_turn(
				session,
				TurnInput { text: Str::new_static("fetch it"), attachments: Vec::new() },
				RunControl::default(),
			),
		)
		.await
		.expect("turn settles")
		.expect("turn");
	}

	/// The journaled `sandbox_amendment` prompts, as `(source, scope)`.
	fn amendments(session: &Session) -> Vec<(ApprovalSource, ApprovalScope)> {
		prompts(session)
			.into_iter()
			.filter(|ticket| ticket.reasons[0].kind == "sandbox_amendment")
			.filter_map(|ticket| ticket.decision)
			.map(|decision| (decision.source, decision.scope))
			.collect()
	}

	/// How the conversation leaves the journal that approved the endpoint.
	#[derive(Clone, Copy)]
	enum Leave {
		/// The live session is rewound past the approval.
		Rewind,
		/// The host switches the kernel to a new session.
		Switch,
	}

	/// An endpoint approved for the session through `environment` holds for
	/// the session's later commands, which reach it without a prompt. Once the
	/// conversation `leave`s the journal that approved it, the next command is
	/// asked again, by a human, because the journal the kernel now serves holds
	/// no grant the desk could replay.
	async fn grants_follow_the_journal(
		project: &Project,
		environment: ProjectEnvironment,
		port: u16,
		leave: Leave,
	) {
		let spill = BlobStore::open(project.scratch.path().join("artifacts")).expect("spill");
		// Each successful fetch appends one line the test counts.
		let fetched = project.root.join("fetched.txt");
		let fetch = format!(
			"/usr/bin/curl --noproxy '' -sf http://localhost:{port}/ >> '{}'",
			fetched.display()
		);
		let fetches = || {
			fs::read_to_string(&fetched)
				.unwrap_or_default()
				.lines()
				.count()
		};
		let kernel = Kernel::new(
			ToolThenText {
				tool:      "bash",
				arguments: serde_json::json!({ "command": fetch, "i": "Fetching a package" }),
				turns:     0,
			},
			environment.registry(),
			DispatchPolicy::new(spill.clone()),
			StaticPrompt(Str::new_static("test")),
		);
		let approvals = bind_environment_approvals(&kernel, &environment);
		let mut kernel = kernel
			.with_external_executor(Arc::new(EnvToolExecutor::new(
				environment.client().clone(),
				approvals,
			)))
			.with_tool_admission(Arc::new(SettingsAdmission::new(&project.con, None, &project.root)));
		let events = kernel.subscribe();
		let mailbox = kernel.mailbox();
		let (asked, humans) = flume::unbounded();
		let host = tokio::spawn(async move {
			while let Ok(event) = events.recv_async().await {
				if let KernelEvent::ApprovalRequested(ticket) = event {
					let _ =
						asked.send((ticket.reasons[0].subject.clone(), ticket.reasons[0].scopes.clone()));
					let _ = mailbox.send(Up::Approve {
						id:       ticket.ticket_id,
						decision: ApprovalDecision {
							approved:   true,
							scope:      ApprovalScope::Session,
							source:     ApprovalSource::User,
							decided_by: None,
							reason:     None,
							audited:    false,
						},
					});
				}
			}
		});
		let session_at = |name: &str| {
			Session::create_with_blob_store(
				project.scratch.path().join(name),
				ComponentRegistry::standard(),
				spill.clone(),
			)
			.expect("session")
		};
		let mut session = session_at("grants.oms");
		let before = session.head().expect("head before the grant");

		fetch_turn(&mut kernel, &mut session).await;
		let subject = Str::from(format!("network localhost:{port}"));
		assert_eq!(humans.drain().collect::<Vec<_>>(), [(subject.clone(), vec![
			Str::new_static("once"),
			Str::new_static("session")
		])]);
		assert_eq!(amendments(&session), [(ApprovalSource::User, ApprovalScope::Session)]);
		assert_eq!(fetches(), 1, "the approved rerun fetched");

		fetch_turn(&mut kernel, &mut session).await;
		assert!(humans.is_empty(), "a granted endpoint prompted again");
		assert_eq!(amendments(&session).len(), 1, "the environment asked nothing");
		assert_eq!(fetches(), 2, "the second command fetched unprompted");

		match leave {
			Leave::Rewind => {
				let work = session.rewind(before).expect("rewind past the grant");
				kernel.apply_lifecycle(&session, &work).await;
			},
			Leave::Switch => {
				// What a host does once a switch commits: the next session is
				// live, the kernel is told, and its state is resynced from the
				// next journal.
				let previous = mem::replace(&mut session, session_at("next.oms"));
				kernel.session_switched();
				kernel.resync_session_state(&session);
				assert_eq!(amendments(&previous).len(), 1, "the previous journal keeps its grant");
			},
		}
		fetch_turn(&mut kernel, &mut session).await;
		assert_eq!(
			humans
				.drain()
				.map(|(subject, _)| subject)
				.collect::<Vec<_>>(),
			[subject],
			"the environment asks again, and the desk cannot answer it"
		);
		assert_eq!(amendments(&session), [(ApprovalSource::User, ApprovalScope::Session)]);
		assert_eq!(fetches(), 3, "the newly approved rerun fetched");
		host.abort();
	}

	/// A session attached to a daemon whose broker may reach loopback.
	async fn attached_grants(leave: Leave) {
		let project = Project::new(ExecSandboxMode::WorkspaceWrite);
		let port = loopback_upstream();
		let _daemon = InProcessDaemon::serve(&project.root, &project.state, loopback_context()).await;
		let environment = attached(&project).await;
		grants_follow_the_journal(&project, environment, port, leave).await;
	}

	/// An embedded composition whose broker may reach loopback: no daemon
	/// serves the project and this test binary cannot be spawned as one, so
	/// the session falls back to an in-process environment, whose route keeps
	/// the grants.
	async fn embedded_grants(leave: Leave) {
		let mut project = Project::new(ExecSandboxMode::WorkspaceWrite);
		project.con = loopback_context();
		let port = loopback_upstream();
		let environment = project.attach(None).await;
		assert!(environment.fallback_notice.is_some(), "the session attached to a daemon");
		grants_follow_the_journal(&project, environment, port, leave).await;
	}

	/// A rewind past the approval revokes the grant on the daemon
	/// (`RevokeApprovalGrants`).
	#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
	async fn an_attached_grant_holds_until_a_rewind_revokes_it() {
		attached_grants(Leave::Rewind).await;
	}

	/// A switch to another session revokes the grant on the daemon, so the
	/// next conversation on the same connection never inherits it.
	#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
	async fn an_attached_grant_never_reaches_the_next_session() {
		attached_grants(Leave::Switch).await;
	}

	/// A rewind past the approval clears the in-process route's grants.
	#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
	async fn an_embedded_grant_holds_until_a_rewind_revokes_it() {
		embedded_grants(Leave::Rewind).await;
	}

	/// A switch to another session clears the in-process route's grants.
	#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
	async fn an_embedded_grant_never_reaches_the_next_session() {
		embedded_grants(Leave::Switch).await;
	}
}

/// Host-keyed fetch approval through the production environment executor:
/// a project environment served in this process admits an argument-scoped
/// fetching tool under `always-ask`, `EnvToolExecutor` turns each admission
/// query into the session's prompt on a kernel-shaped route, and the approval
/// desk answers a repeated host from the session grant the journal holds.
mod fetch_host_grants {
	use std::sync::atomic::{AtomicUsize, Ordering};

	use futures::StreamExt as _;
	use omp_agent::{
		ApprovalDesk, ApprovalRoute, ExternalDispatchEvent, ExternalDispatchRequest,
		ExternalToolExecutor as _, KernelEvents,
	};
	use omp_core::{Principal, sf};
	use omp_envd::{EnvServer, exthost::ConvarControlFactory, worker::ExtHostConfig};
	use omp_journal::blob::BlobStore;
	use omp_proto::env::v1 as pb;
	use omp_tool::{Effects, IncomingParams, Registry, ToolIdentity, ToolTerminal};

	use super::*;

	/// Arguments of [`FetchProbe`]: the targets one call reads.
	#[derive(serde::Deserialize)]
	#[serde(deny_unknown_fields)]
	struct FetchProbeParams {
		targets: Vec<Str>,
	}

	/// A native tool reading local paths and fetching every URL target, as
	/// an argument-scoped `read` classifies its targets.
	struct FetchProbe {
		spec: omp_tool::ToolSpec,
		ran:  Arc<AtomicUsize>,
	}

	impl omp_tool::Tool for FetchProbe {
		type Fault = serde_json::Value;
		type Params = FetchProbeParams;
		type Payload = serde_json::Value;
		type Update = serde_json::Value;

		const ARGUMENT_SCOPED_EFFECTS: bool = true;

		fn spec(&self) -> &omp_tool::ToolSpec {
			&self.spec
		}

		fn invocation_effects(&self, params: &FetchProbeParams) -> Option<Effects> {
			let fetches = params.targets.iter().any(|target| target.contains("://"));
			Some(Effects {
				documents: Some(omp_tool::DocEffects { read: true, write_globs: Arc::from([]) }),
				fetch: fetches.then_some(omp_tool::FetchEffects { credentials: false }),
				..Effects::empty()
			})
		}

		fn fetch_locators(&self, params: &FetchProbeParams) -> Vec<Str> {
			params
				.targets
				.iter()
				.filter(|target| target.contains("://"))
				.cloned()
				.collect()
		}

		fn call<'c>(
			&'c self,
			mut params: IncomingParams<'c>,
		) -> impl futures::Stream<
			Item = omp_tool::Ev<serde_json::Value, serde_json::Value, serde_json::Value>,
		> + Send
		+ 'c {
			async_stream::stream! {
				params.committed().await.expect("probe commitment");
				self.ran.fetch_add(1, Ordering::Relaxed);
				yield omp_tool::Ev::Done(ToolTerminal::Done {
					result: Ok(serde_json::json!({"text": "fetched"})),
					useless: false,
				});
			}
		}

		fn prompt(
			&self,
			_view: Result<&serde_json::Value, &serde_json::Value>,
			_caps: &omp_tool::PromptCaps,
		) -> Vec<omp_tool::Part> {
			vec![omp_tool::Part::Text { text: sf!("fetched") }]
		}
	}

	/// The session side of the production executor: the kernel's route and
	/// approval desk, and a human who answers every prompt for the session.
	struct Host {
		executor: EnvToolExecutor,
		up:       flume::Receiver<Up>,
		desk:     ApprovalDesk,
		session:  Session,
		blobs:    BlobStore,
		identity: ToolIdentity,
	}

	impl Host {
		/// Runs one call reading `targets` to a successful outcome and returns
		/// the subjects of each prompt a human was asked.
		async fn call(&mut self, call_id: &str, targets: &[&str]) -> Vec<Vec<Str>> {
			let request = ExternalDispatchRequest {
				identity:       self.identity.clone(),
				session_id:     sf!("fetch-session"),
				blobs:          self.blobs.clone(),
				call_id:        Str::new(call_id),
				args:           serde_json::value::to_raw_value(
					&serde_json::json!({"targets": targets}),
				)
				.expect("arguments"),
				route:          omp_tool::ToolRoute::Remote,
				blocking_limit: Duration::from_secs(30),
				output_request: omp_tool::OutputRequest::Bounded,
				cancellation:   tokio_util::sync::CancellationToken::new(),
				restrictions:   None,
			};
			let mut stream = self.executor.invoke(request);
			let mut asked = Vec::new();
			let outcome = tokio::time::timeout(Duration::from_secs(60), async {
				loop {
					tokio::select! {
						event = stream.next() => match event {
							Some(
								ExternalDispatchEvent::Done { is_error, parts, .. }
								| ExternalDispatchEvent::DoneProjected { is_error, parts, .. },
							) => {
								break (!is_error).then_some(()).ok_or_else(|| format!("{parts:?}"));
							},
							Some(ExternalDispatchEvent::Aborted(abort)) => break Err(format!("{abort:?}")),
							None => break Err(String::from("the stream ended without an outcome")),
							Some(_) => {},
						},
						request = self.up.recv_async() => {
							let Ok(Up::Approval(request)) = request else { continue };
							let ticket =
								self.desk.file(&mut self.session, request).expect("prompt files");
							if ticket.state == TicketState::Pending {
								asked.push(
									ticket.reasons.iter().map(|reason| reason.subject.clone()).collect(),
								);
								self
									.desk
									.decide(&mut self.session, ticket.ticket_id.as_str(), ApprovalDecision {
										approved:   true,
										scope:      ApprovalScope::Session,
										source:     ApprovalSource::User,
										decided_by: None,
										reason:     None,
										audited:    false,
									})
									.expect("the human answers for the session");
							}
						},
					}
				}
			})
			.await
			.expect("the call settles");
			assert_eq!(outcome, Ok(()), "{call_id} ran to a successful outcome");
			asked
		}
	}

	/// A local environment served in this process whose registry holds the
	/// fetch probe as a native tool, and how often the probe ran.
	struct ProbeEnvironment {
		server: Arc<EnvServer>,
		con:    Arc<omp_con::Ctx>,
		root:   PathBuf,
		rev:    omp_tool::Rev,
		ran:    Arc<AtomicUsize>,
	}

	impl ProbeEnvironment {
		async fn open(scratch: &Path) -> Self {
			let (root, state) = (scratch.join("workspace"), scratch.join("state"));
			std::fs::create_dir_all(&root).expect("workspace");
			std::fs::create_dir_all(&state).expect("state");
			let ran = Arc::new(AtomicUsize::new(0));
			let mut registry = Registry::new();
			registry
				.register(
					FetchProbe {
						spec: omp_tool::ToolSpec {
							name:            sf!("fetch_probe"),
							rev:             omp_tool::Rev { family: Str::default(), n: 1 },
							description:     sf!("argument-scoped fetch probe"),
							schema:          bytes::Bytes::from_static(br#"{"type":"object"}"#),
							constraint:      omp_tool::Constraint::None,
							effects:         Effects {
								documents: Some(omp_tool::DocEffects {
									read:        true,
									write_globs: Arc::from([]),
								}),
								fetch: Some(omp_tool::FetchEffects { credentials: false }),
								..Effects::empty()
							},
							confinement:     omp_tool::Confinement::Host,
							projection_code: [0; 32],
						},
						ran:  Arc::clone(&ran),
					},
					omp_tool::Presentation::Slot,
					omp_tool::Claims {
						precedence: omp_tool::Precedence::DEFAULT,
						claimant:   sf!("omp/test"),
						replaces:   None,
					},
				)
				.expect("register the fetch probe");
			let con = Arc::new(omp_con::Ctx::new());
			let server = Arc::new(
				EnvServer::open_local(
					&root,
					&state,
					registry,
					ExtHostConfig::new(
						PathBuf::from("unused"),
						Principal::new(sf!("test-principal"), sf!("Test Principal")),
						sf!("test-session"),
						1,
					),
					&con,
					Arc::new(ConvarControlFactory::new(Arc::clone(&con))),
					RegistryBridges::default(),
				)
				.await
				.expect("local environment"),
			);
			let rev = server
				.registry()
				.live_identity("fetch_probe")
				.map(|(_, rev)| rev.clone())
				.expect("the fetch probe is registered");
			Self { server, con, root, rev, ran }
		}
	}

	/// Under `always-ask` a fetch is asked once per host per session: the
	/// first fetch from a host asks, a `session` answer lets every later fetch
	/// from that host run unasked, a fetch from another host asks again, a
	/// fetch reaching hosts granted one at a time runs unasked, and a local
	/// read never asks. A locator the environment cannot name never drops the
	/// hosts named beside it: a grant for the tool's own unnamed fetch never
	/// answers a fetch from a host no one granted.
	#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
	async fn always_ask_asks_once_per_fetched_host_per_session() {
		let scratch = tempfile::tempdir().expect("scratch");
		let probe = ProbeEnvironment::open(scratch.path()).await;
		let (client, transport) = omp_env::EnvClient::in_process(64);
		let serving = tokio::spawn({
			let server = Arc::clone(&probe.server);
			async move { server.serve_in_process(transport).await }
		});
		client
			.hello(pb::ClientHello {
				client: "fetch-host-grants".to_owned(),
				schema_rev: omp_proto::SCHEMA_REV,
				approval_mode: pb::ApprovalMode::AlwaysAsk as i32,
				..pb::ClientHello::default()
			})
			.await
			.expect("hello");

		// The kernel's route and desk: prompts land in the mailbox, the desk
		// journals them and answers a granted subject from the tree.
		let (mailbox, up) = flume::unbounded();
		let blobs = BlobStore::open(scratch.path().join("blobs")).expect("blobs");
		let mut host = Host {
			executor: EnvToolExecutor::new(client, ApprovalRoute::to_kernel(mailbox, None)),
			up,
			desk: ApprovalDesk::new(KernelEvents::default()),
			session: Session::create_with_blob_store(
				scratch.path().join("fetch.oms"),
				ComponentRegistry::standard(),
				blobs.clone(),
			)
			.expect("session"),
			blobs,
			identity: ToolIdentity { name: sf!("fetch_probe"), rev: probe.rev.clone() },
		};

		assert_eq!(host.call("call-1", &["https://docs.rs/serde"]).await, [vec![sf!(
			"http:docs.rs:443"
		)]]);
		assert!(
			host
				.call("call-2", &["https://docs.rs/tokio", "https://docs.rs/bytes"])
				.await
				.is_empty(),
			"the granted host is not asked again"
		);
		assert_eq!(
			host
				.call("call-3", &["https://crates.io/crates/serde"])
				.await,
			[vec![sf!("http:crates.io:443")]],
			"a grant for one host never covers another"
		);
		assert!(host.call("call-4", &["notes.txt"]).await.is_empty(), "a local read never asks");
		assert!(
			host
				.call("call-5", &["https://docs.rs/x", "https://crates.io/y"])
				.await
				.is_empty(),
			"hosts granted one at a time answer a fetch reaching both"
		);
		assert_eq!(
			host
				.call("call-6", &["https://evil.example/x", "mcp://unadvertised/resource"])
				.await,
			[vec![sf!("http:evil.example:443"), sf!("tool:fetch_probe")]],
			"an unnamed locator keeps the host named beside it"
		);
		assert!(
			host
				.call("call-7", &["https://docs.rs/z", "mcp://unadvertised/resource"])
				.await
				.is_empty(),
			"the granted host and the granted remainder answer"
		);
		assert_eq!(
			host
				.call("call-8", &["https://other.example/x", "mcp://unadvertised/resource"])
				.await,
			[vec![sf!("http:other.example:443"), sf!("tool:fetch_probe")]],
			"the remainder's grant never covers a host no one granted"
		);
		assert_eq!(probe.ran.load(Ordering::Relaxed), 8);
		serving.abort();
	}

	/// Scripted turns calling the fetch probe: turn `2n - 1` calls `call-n`
	/// reading the `n`th target list, turn `2n` closes with text.
	struct FetchTurns {
		targets: Vec<Vec<&'static str>>,
		turns:   usize,
	}

	impl Inference for FetchTurns {
		fn chat(
			&mut self,
			_request: ChatRequest,
		) -> impl Future<Output = Result<ChatStream, omp_ai::Error>> + Send {
			self.turns += 1;
			let targets = &self.targets[self.turns.div_ceil(2) - 1];
			ready(Ok(tool_then_text(
				self.turns,
				"fetch_probe",
				&serde_json::json!({ "targets": targets }),
			)))
		}
	}

	/// An embedded or isolated composition runs the environment's native tools
	/// in the kernel, outside the environment's admission gate: the kernel's
	/// `SettingsAdmission`, given the environment's fetch hosts namer, asks a
	/// fetch once per host per session exactly as the environment's gate does,
	/// and keeps every named host beside an unnamed locator.
	#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
	async fn an_embedded_kernel_asks_its_native_fetches_once_per_host() {
		let scratch = tempfile::tempdir().expect("scratch");
		let probe = ProbeEnvironment::open(scratch.path()).await;
		let registry = probe.server.registry();
		assert_eq!(
			registry.route("fetch_probe").expect("routed"),
			omp_tool::ToolRoute::Native,
			"the kernel runs the probe itself"
		);
		let targets = vec![
			vec!["https://docs.rs/serde"],
			vec!["https://docs.rs/tokio"],
			vec!["https://crates.io/crates/serde"],
			vec!["https://docs.rs/x", "https://crates.io/y"],
			vec!["https://evil.example/x", "mcp://unadvertised/resource"],
			vec!["https://docs.rs/z", "mcp://unadvertised/resource"],
			vec!["https://other.example/x", "mcp://unadvertised/resource"],
			vec!["notes.txt"],
		];
		let calls = targets.len();
		let spill = BlobStore::open(scratch.path().join("artifacts")).expect("spill");
		let mut kernel = Kernel::new(
			FetchTurns { targets, turns: 0 },
			registry,
			DispatchPolicy::new(spill.clone()),
			StaticPrompt(Str::new_static("test")),
		)
		.with_tool_admission(Arc::new(
			SettingsAdmission::new(&probe.con, Some(ApprovalMode::AlwaysAsk), &probe.root)
				.with_fetch_hosts(probe.server.fetch_hosts()),
		));
		let events = kernel.subscribe();
		let mailbox = kernel.mailbox();
		let asked = Arc::new(parking_lot::Mutex::new(Vec::<Vec<Str>>::new()));
		let host = tokio::spawn({
			let asked = Arc::clone(&asked);
			async move {
				while let Ok(event) = events.recv_async().await {
					if let KernelEvent::ApprovalRequested(ticket) = event {
						asked.lock().push(
							ticket
								.reasons
								.iter()
								.map(|reason| reason.subject.clone())
								.collect(),
						);
						let _ = mailbox.send(Up::Approve {
							id:       ticket.ticket_id,
							decision: ApprovalDecision {
								approved:   true,
								scope:      ApprovalScope::Session,
								source:     ApprovalSource::User,
								decided_by: None,
								reason:     None,
								audited:    false,
							},
						});
					}
				}
			}
		});
		let mut session = Session::create_with_blob_store(
			scratch.path().join("embedded-fetch.oms"),
			ComponentRegistry::standard(),
			spill,
		)
		.expect("session");
		let mut each = Vec::new();
		for _ in 0..calls {
			tokio::time::timeout(
				Duration::from_secs(60),
				kernel.run_turn(
					&mut session,
					TurnInput { text: Str::new_static("fetch"), attachments: Vec::new() },
					RunControl::default(),
				),
			)
			.await
			.expect("turn settles")
			.expect("turn");
			each.push(std::mem::take(&mut *asked.lock()));
		}
		host.abort();
		let host_subject = |subjects: &[&str]| -> Vec<Vec<Str>> {
			vec![subjects.iter().copied().map(Str::new).collect()]
		};
		assert_eq!(each, [
			host_subject(&["http:docs.rs:443"]),
			Vec::new(),
			host_subject(&["http:crates.io:443"]),
			Vec::new(),
			host_subject(&["http:evil.example:443", "tool:fetch_probe"]),
			Vec::new(),
			host_subject(&["http:other.example:443", "tool:fetch_probe"]),
			Vec::new(),
		]);
		assert_eq!(probe.ran.load(Ordering::Relaxed), calls, "every approved call ran");
	}
}
