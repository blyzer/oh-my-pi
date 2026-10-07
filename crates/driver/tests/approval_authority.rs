//! Production approval composition: a real project environment under
//! `--approval-mode always-ask`, the kernel-bound approval route, and a
//! `write` call whose admission prompt is journaled and answered by the host
//! with `Up::Approve` (deny → skipped, allow → the file lands). With no
//! sandbox in force, the default `yolo` is downgraded to `write`, so a `bash`
//! call prompts; an explicit `yolo` is respected and runs unconfined. Each
//! says so in one typed notice. On the attached path a project daemon served
//! in this process runs the command, and its sandbox amendment reaches the
//! issuing session only through the approval relay.

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
use omp_driver::headless::kernel::{EnvToolExecutor, SettingsAdmission};
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
		let meta = ResponseMeta {
			request_id:          RequestId::from("approval-test"),
			provider:            ProviderId::from("test"),
			route:               RouteId::from("test/route"),
			model:               None,
			provider_request_id: None,
			created_at:          std::time::SystemTime::UNIX_EPOCH,
		};
		let events = if self.turns % 2 == 1 {
			let arguments = self.arguments.clone();
			let call = ToolCall {
				id:        ToolCallId::from(format!("call-{}", self.turns.div_ceil(2)).as_str()),
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
	use std::{
		fs,
		io::{BufRead as _, BufReader, Write as _},
		net::{Ipv4Addr, TcpListener},
		thread,
	};

	use omp_core::{Principal, sf};
	use omp_envd::{EnvServer, SessionGrants, exthost::ConvarControlFactory, worker::ExtHostConfig};
	use omp_journal::blob::BlobStore;
	use omp_tool::Registry;
	use tokio::{net::UnixStream, task::JoinHandle, time};
	use tokio_util::sync::CancellationToken;

	use super::*;

	/// Bounded wait for the daemon's listener.
	const LISTEN_WAIT: Duration = Duration::from_secs(30);

	/// A project daemon this test process serves on the production socket of
	/// the project's state directory, opened by `EnvServer::open_project` as
	/// `omp envd` opens it; like it, its host binds no approval route.
	///
	/// `ProjectEnvironment::attach` finds it there and, because it runs the
	/// same executable and so has the same build id, joins it as a peer instead
	/// of spawning a daemon or falling back to an embedded environment.
	/// Dropping it stops serving.
	struct InProcessDaemon {
		shutdown: CancellationToken,
		serving:  JoinHandle<Result<(), omp_envd::EnvdError>>,
	}

	impl InProcessDaemon {
		async fn serve(project: &Project, sandbox: ExecSandboxMode) -> Self {
			Self::serve_with(project, context(sandbox)).await
		}

		/// [`Self::serve`] under the daemon's own control context `con`.
		async fn serve_with(project: &Project, con: Arc<omp_con::Ctx>) -> Self {
			let convars = Arc::new(ConvarControlFactory::new(Arc::clone(&con)));
			let server = EnvServer::open_project(
				&project.root,
				&project.state,
				&omp_env::project_state::document_socket(&project.state),
				Registry::new(),
				ExtHostConfig::current(
					Principal::new(sf!("daemon-tester"), sf!("Daemon Tester")),
					sf!("daemon-session"),
					1,
				)
				.expect("daemon host configuration"),
				None,
				false,
				None,
				&con,
				convars,
				RegistryBridges::default(),
			)
			.await
			.expect("project daemon");
			let socket = omp_env::project_state::environment_socket(&project.state);
			let shutdown = CancellationToken::new();
			let serving = tokio::spawn({
				let server = Arc::new(server);
				let socket = socket.clone();
				let shutdown = shutdown.clone();
				async move { server.serve_uds(&socket, shutdown, None).await }
			});
			tokio::time::timeout(LISTEN_WAIT, async {
				while UnixStream::connect(&socket).await.is_err() {
					tokio::time::sleep(Duration::from_millis(10)).await;
				}
			})
			.await
			.expect("the project daemon never listened");
			Self { shutdown, serving }
		}
	}

	impl Drop for InProcessDaemon {
		fn drop(&mut self) {
			self.shutdown.cancel();
			self.serving.abort();
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
	async fn attached(project: &Project) -> ProjectEnvironment {
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
		let _daemon = InProcessDaemon::serve(&project, ExecSandboxMode::WorkspaceWrite).await;
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
		let _daemon = InProcessDaemon::serve(&project, ExecSandboxMode::WorkspaceWrite).await;
		let environment = attached(&project).await;
		let Turn { session, result, landed } = project
			.turn(environment, "bash", protected_write, ".git/amended.txt", None, false)
			.await;
		amendment(&project, &session, false);
		assert!(!landed, "a refused amendment wrote: {result}");
		assert!(result.contains("\"outcome\":\"denied\""), "the command was not denied: {result}");
	}

	/// A loopback HTTP server answering every request `200 ok`, standing in
	/// for one package host the daemon's broker may reach under
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

	/// Revokes the attached session's network grants on every rewind, as the
	/// production composition does.
	struct RevokeOnRewind(SessionGrants);

	impl omp_agent::RewindObserver for RevokeOnRewind {
		fn rewound(&self) {
			self.0.revoke();
		}
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

	/// A network endpoint approved for the session through the daemon's relay
	/// holds for the attached session's later commands, which reach it
	/// without a prompt. Rewinding the conversation past the approval revokes
	/// it on the daemon, so the next command is asked again, by a human,
	/// because the rewound journal holds no grant the desk could replay.
	#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
	async fn a_session_network_grant_holds_until_a_rewind_revokes_it() {
		let project = Project::new(ExecSandboxMode::WorkspaceWrite);
		let port = loopback_upstream();
		let daemon = context(ExecSandboxMode::WorkspaceWrite);
		daemon
			.run("sv_sandbox_allow_localhost 1")
			.expect("the daemon's broker may reach loopback");
		let _daemon = InProcessDaemon::serve_with(&project, daemon).await;
		let environment = attached(&project).await;

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
		let approvals = kernel.approval_route();
		environment.bind_approval_authority(
			Some(Arc::new(omp_agent::ApprovalBook::new())),
			Some(approvals.clone()),
		);
		let mut kernel = kernel
			.with_rewind_observer(Arc::new(RevokeOnRewind(environment.session_grants())))
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
		let mut session = Session::create_with_blob_store(
			project.scratch.path().join("grants.oms"),
			ComponentRegistry::standard(),
			spill,
		)
		.expect("session");
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
		assert_eq!(amendments(&session).len(), 1, "the daemon asked nothing");
		assert_eq!(fetches(), 2, "the second command fetched unprompted");

		let work = session.rewind(before).expect("rewind past the grant");
		kernel.apply_lifecycle(&session, &work).await;
		fetch_turn(&mut kernel, &mut session).await;
		assert_eq!(
			humans
				.drain()
				.map(|(subject, _)| subject)
				.collect::<Vec<_>>(),
			[subject],
			"after the rewind the daemon asks again, and the desk cannot answer it"
		);
		assert_eq!(amendments(&session), [(ApprovalSource::User, ApprovalScope::Session)]);
		assert_eq!(fetches(), 3, "the newly approved rerun fetched");
		host.abort();
	}
}
