//! Tests for the Codex `app-server` adapter.
//!
//! Protocol behavior runs against a deterministic in-process JSON-RPC peer
//! over in-memory pipes: no `codex` binary is started, no login exists and no
//! file under any Codex home is touched. Process-tree behavior (hung child,
//! crash, kill, drop) runs against throwaway `sh` children that are not Codex.

use std::{path::PathBuf, time::Duration};

use omp_agent::{
	approvals::{ApprovalDecision, ApprovalScope, ApprovalSource},
	delegated::{
		ActiveSession, ApprovalPosture, AuthStatus, Capabilities, CliSessionId, CliVersion,
		DelegatedEvent, DelegatedExecutor, DelegatedPolicy, DelegatedPrompt, DelegatedSession,
		ExecutionMode, ExecutorError, ExitFact, FrameFault, LifecycleEvent, NetworkPosture,
		PermissionKind, PermissionRequest, PermissionResolver, PermissionTally, ProbeReport,
		PromptRole, PromptSection, ProtocolViolation, RemoteFailure, SandboxPosture, SessionInfo,
		SessionRequest, ToolEvent, ToolKind, ToolOutcome, TurnEnd, TurnInput, TurnLimits, TurnReport,
		UnknownAuth, start_session,
	},
};
use omp_ai::UsageSource;
use omp_core::{Hash32, Str};
use serde::Deserialize;
use tokio::{
	io::{AsyncWriteExt as _, DuplexStream, duplex},
	time::timeout,
};
use tokio_util::sync::CancellationToken;

use super::{
	executor::{CODEX_VERSIONS, CodexExecutor, capabilities},
	frame::LineReader,
	policy::wire_policy,
	session::{CodexSession, SessionConfig, Transport, parse_version},
	wire,
};

type Active = ActiveSession<CodexSession>;

const WORKSPACE: DelegatedPolicy = DelegatedPolicy::new(
	SandboxPosture::WorkspaceWrite,
	NetworkPosture::Disabled,
	ApprovalPosture::AlwaysAsk,
);
const STRICT: DelegatedPolicy = DelegatedPolicy::STRICTEST;

const AGENT_OK: &str = r#"{"userAgent":"codex_cli_rs/0.160.0 (test)"}"#;
const READ_ONLY: &str = r#"{"type":"readOnly","networkAccess":false}"#;
const WORKSPACE_WRITE: &str =
	r#"{"type":"workspaceWrite","networkAccess":false,"writableRoots":[]}"#;

// ---------------------------------------------------------------------------
// The mock peer
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct Head {
	id:     Option<i64>,
	method: Option<String>,
}

/// One line the client wrote.
struct Frame {
	head: Head,
	line: String,
}

impl Frame {
	fn id(&self) -> i64 {
		self.head.id.expect("frame has an id")
	}
}

/// The server side of the in-memory pipes.
struct Peer {
	from_client: LineReader<DuplexStream>,
	to_client:   DuplexStream,
}

impl Peer {
	async fn try_recv(&mut self) -> Option<Frame> {
		let line = timeout(Duration::from_secs(30), self.from_client.next_line())
			.await
			.expect("the client wrote nothing for 30s")
			.ok()??;
		let line = String::from_utf8(line.to_vec()).expect("client lines are UTF-8");
		let head = serde_json::from_str(&line).expect("client lines are JSON-RPC");
		Some(Frame { head, line })
	}

	async fn recv(&mut self) -> Frame {
		self.try_recv().await.expect("the client closed its output")
	}

	async fn expect(&mut self, method: &str) -> Frame {
		let frame = self.recv().await;
		assert_eq!(frame.head.method.as_deref(), Some(method), "unexpected frame: {}", frame.line);
		frame
	}

	async fn send(&mut self, line: &str) {
		self
			.to_client
			.write_all(line.as_bytes())
			.await
			.expect("write");
		self.to_client.write_all(b"\n").await.expect("write");
	}

	async fn reply(&mut self, id: i64, result: &str) {
		self
			.send(&format!(r#"{{"id":{id},"result":{result}}}"#))
			.await;
	}

	async fn close(&mut self) {
		self.to_client.shutdown().await.expect("shutdown");
	}

	/// `initialize`, `initialized`, then the thread request.
	async fn handshake(&mut self, user_agent: &str, thread_method: &str, opened: &str) -> String {
		let init = self.expect("initialize").await;
		assert!(init.line.contains(r#""clientInfo":{"name":"omp""#), "{}", init.line);
		assert!(!init.line.contains("jsonrpc"), "the wire has no jsonrpc member");
		self.reply(init.id(), user_agent).await;
		self.expect("initialized").await;
		let thread = self.expect(thread_method).await;
		self.reply(thread.id(), opened).await;
		thread.line
	}

	/// Answers `thread/inject_items` (when expected) and `turn/start`.
	async fn accept_turn(&mut self, turn: &str, system: bool) -> (Option<String>, String) {
		let mut inject = None;
		if system {
			let frame = self.expect("thread/inject_items").await;
			self.reply(frame.id(), "{}").await;
			inject = Some(frame.line);
		}
		let start = self.expect("turn/start").await;
		self
			.reply(start.id(), &format!(r#"{{"turn":{{"id":"{turn}"}}}}"#))
			.await;
		(inject, start.line)
	}
}

fn opened(thread: &str, approval: &str, sandbox: &str) -> String {
	format!(
		r#"{{"thread":{{"id":"{thread}"}},"approvalPolicy":"{approval}","approvalsReviewer":"user","cwd":"/work/project","sandbox":{sandbox}}}"#
	)
}

fn notification(method: &str, params: &str) -> String {
	format!(r#"{{"method":"{method}","params":{params}}}"#)
}

fn turn_started(turn: &str) -> String {
	notification(
		"turn/started",
		&format!(
			r#"{{"threadId":"thr-1","turn":{{"id":"{turn}","status":"inProgress","items":[]}}}}"#
		),
	)
}

fn turn_completed(turn: &str, status: &str) -> String {
	notification(
		"turn/completed",
		&format!(r#"{{"threadId":"thr-1","turn":{{"id":"{turn}","status":"{status}","items":[]}}}}"#),
	)
}

fn item(method: &str, turn: &str, body: &str) -> String {
	notification(
		method,
		&format!(r#"{{"threadId":"thr-1","turnId":"{turn}","startedAtMs":1,"item":{body}}}"#),
	)
}

fn server_request(id: i64, method: &str, params: &str) -> String {
	format!(r#"{{"id":{id},"method":"{method}","params":{params}}}"#)
}

// ---------------------------------------------------------------------------
// An executor over the in-memory peer
// ---------------------------------------------------------------------------

struct MemExecutor {
	caps:    Capabilities,
	version: CliVersion,
	cfg:     SessionConfig,
	peers:   flume::Sender<Peer>,
}

fn config() -> SessionConfig {
	SessionConfig {
		max_frame_bytes:   64 * 1024,
		terminate_grace:   Duration::from_millis(200),
		handshake_timeout: Duration::from_secs(5),
		versions:          CODEX_VERSIONS,
	}
}

fn mem(version: CliVersion, cfg: SessionConfig) -> (MemExecutor, flume::Receiver<Peer>) {
	let (peers, rx) = flume::unbounded();
	(MemExecutor { caps: capabilities(), version, cfg, peers }, rx)
}

impl DelegatedExecutor for MemExecutor {
	type Session = CodexSession;

	fn capabilities(&self) -> &Capabilities {
		&self.caps
	}

	async fn probe(&self) -> Result<ProbeReport, ExecutorError> {
		Ok(ProbeReport { version: self.version, auth: AuthStatus::Unknown })
	}

	async fn open<'a>(
		&'a self,
		request: &'a SessionRequest,
	) -> Result<(Self::Session, SessionInfo), ExecutorError> {
		let (client_out, server_in) = duplex(256 * 1024);
		let (server_out, client_in) = duplex(256 * 1024);
		self
			.peers
			.send(Peer {
				from_client: LineReader::new(server_in, 1024 * 1024),
				to_client:   server_out,
			})
			.expect("test peer channel");
		let transport = Transport {
			reader: Box::new(client_in),
			writer: Box::new(client_out),
			stderr: None,
			tree:   None,
		};
		let mut session = CodexSession::new(CodexExecutor::executor_id(), transport, self.cfg);
		let info = session.handshake(request).await?;
		Ok((session, info))
	}
}

fn workdir() -> PathBuf {
	PathBuf::from("/work/project")
}

fn request_with(policy: DelegatedPolicy, resume: Option<&str>) -> SessionRequest {
	SessionRequest {
		mode: ExecutionMode::CliManaged,
		workdir: workdir(),
		policy,
		resume: resume.map(CliSessionId::new),
		unknown_auth: UnknownAuth::Proceed,
	}
}

/// Opens a session whose peer answers with `user_agent`/`opened`.
async fn open_with(
	exec: &MemExecutor,
	rx: &flume::Receiver<Peer>,
	request: &SessionRequest,
	user_agent: &str,
	opened: &str,
) -> (Result<Active, ExecutorError>, Peer, String) {
	let thread_method = if request.resume.is_some() {
		wire::THREAD_RESUME
	} else {
		wire::THREAD_START
	};
	let serving = async {
		let mut peer = rx.recv_async().await.expect("peer");
		let line = peer.handshake(user_agent, thread_method, opened).await;
		(peer, line)
	};
	let (session, (peer, line)) = tokio::join!(start_session(exec, request), serving);
	(session, peer, line)
}

async fn open_policy(policy: DelegatedPolicy, sandbox: &str, approval: &str) -> (Active, Peer) {
	let (exec, rx) = mem(CliVersion::new(0, 160, 0), config());
	let (session, peer, _) = open_with(
		&exec,
		&rx,
		&request_with(policy, None),
		AGENT_OK,
		&opened("thr-1", approval, sandbox),
	)
	.await;
	(session.expect("session opens"), peer)
}

async fn open_workspace() -> (Active, Peer) {
	open_policy(WORKSPACE, WORKSPACE_WRITE, "untrusted").await
}

// ---------------------------------------------------------------------------
// Turn driving
// ---------------------------------------------------------------------------

const fn limits() -> TurnLimits {
	TurnLimits {
		timeout:         Duration::from_secs(10),
		idle:            Duration::from_secs(2),
		interrupt_grace: Duration::from_millis(500),
	}
}

fn bands() -> [Hash32; 4] {
	[1_u8, 2, 3, 4].map(|byte| Hash32::new([byte; 32]))
}

fn section(role: PromptRole, text: &str) -> PromptSection {
	PromptSection { role, text: Str::new(text) }
}

/// Text that would not survive a casual rewrite.
fn prompt() -> DelegatedPrompt {
	DelegatedPrompt::from_sections(
		vec![
			section(PromptRole::System, "Eres el agente de OMP — sistema ✓\n\n  indentado  \n"),
			section(PromptRole::System, "<rules>\n\tuse tabs\n</rules>"),
			section(PromptRole::User, "status: dirty\r\n"),
			section(PromptRole::User, "fix the bug in `src/lib.rs`"),
		],
		bands(),
	)
}

fn input() -> TurnInput {
	TurnInput { prompt: prompt() }
}

/// An allow/deny resolver that records what it was asked.
struct Scripted {
	decision: ApprovalDecision,
	asked:    Vec<PermissionRequest>,
}

fn decision(approved: bool, scope: ApprovalScope) -> ApprovalDecision {
	ApprovalDecision {
		approved,
		scope,
		source: ApprovalSource::User,
		decided_by: None,
		reason: None,
		audited: true,
	}
}

impl Scripted {
	fn new(decision: ApprovalDecision) -> Self {
		Self { decision, asked: Vec::new() }
	}
}

impl PermissionResolver for Scripted {
	fn resolve<'a>(
		&'a mut self,
		request: &'a PermissionRequest,
	) -> impl Future<Output = ApprovalDecision> + Send + 'a {
		self.asked.push(request.clone());
		std::future::ready(self.decision.clone())
	}
}

async fn run_input(
	session: &mut Active,
	input: &TurnInput,
	resolver: &mut Scripted,
	cancel: &CancellationToken,
) -> (Result<TurnReport, ExecutorError>, Vec<DelegatedEvent>) {
	let (tx, rx) = flume::unbounded();
	let result = session
		.run_turn(input, limits(), cancel, resolver, &tx)
		.await;
	(result, rx.drain().collect())
}

async fn run(
	session: &mut Active,
	resolver: &mut Scripted,
) -> (Result<TurnReport, ExecutorError>, Vec<DelegatedEvent>) {
	run_input(session, &input(), resolver, &CancellationToken::new()).await
}

fn deny() -> Scripted {
	Scripted::new(decision(false, ApprovalScope::Once))
}

fn fail<T: std::fmt::Debug>(result: Result<T, ExecutorError>) -> ExecutorError {
	result.expect_err("the call must fail")
}

// ---------------------------------------------------------------------------
// initialize, thread/start
// ---------------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn open_initializes_starts_a_thread_and_reports_the_applied_policy() {
	let (exec, rx) = mem(CliVersion::new(0, 160, 0), config());
	let (session, _peer, thread_line) = open_with(
		&exec,
		&rx,
		&request_with(WORKSPACE, None),
		AGENT_OK,
		&opened("thr-1", "untrusted", WORKSPACE_WRITE),
	)
	.await;
	let session = session.expect("opens");
	let info = session.info();
	assert_eq!(info.session.as_str(), "thr-1");
	assert!(!info.resumed);
	assert_eq!(info.workdir, workdir());
	assert_eq!(info.policy, WORKSPACE);
	for fragment in [
		r#""approvalPolicy":"untrusted""#,
		r#""approvalsReviewer":"user""#,
		r#""sandbox":"workspace-write""#,
		r#""cwd":"/work/project""#,
		r#""config":{"sandbox_workspace_write":{"network_access":false}}"#,
	] {
		assert!(thread_line.contains(fragment), "{fragment} missing from {thread_line}");
	}
}

#[tokio::test(start_paused = true)]
async fn a_read_only_thread_sends_no_network_override() {
	let (exec, rx) = mem(CliVersion::new(0, 160, 0), config());
	let (session, _peer, line) = open_with(
		&exec,
		&rx,
		&request_with(STRICT, None),
		AGENT_OK,
		&opened("thr-1", "untrusted", READ_ONLY),
	)
	.await;
	session.expect("opens");
	assert!(line.contains(r#""sandbox":"read-only""#));
	assert!(!line.contains("config"), "{line}");
}

#[tokio::test(start_paused = true)]
async fn a_server_of_another_version_is_refused_after_initialize() {
	let (exec, rx) = mem(CliVersion::new(0, 160, 0), config());
	let request = request_with(WORKSPACE, None);
	let serving = async {
		let mut peer = rx.recv_async().await.expect("peer");
		let init = peer.expect("initialize").await;
		peer
			.reply(init.id(), r#"{"userAgent":"codex_cli_rs/0.159.9 (test)"}"#)
			.await;
		peer
	};
	let (session, _peer) = tokio::join!(start_session(&exec, &request), serving);
	assert!(matches!(
		fail(session.map(|_| ())),
		ExecutorError::UnsupportedVersion { found, .. } if found == CliVersion::new(0, 159, 9)
	));
}

#[tokio::test(start_paused = true)]
async fn a_probe_outside_the_supported_range_is_refused_before_spawning() {
	let (exec, rx) = mem(CliVersion::new(0, 161, 0), config());
	let error = fail(
		start_session(&exec, &request_with(WORKSPACE, None))
			.await
			.map(|_| ()),
	);
	assert!(matches!(error, ExecutorError::UnsupportedVersion { .. }));
	assert!(rx.try_recv().is_err(), "no session was opened");
}

#[test]
fn versions_parse_strictly() {
	assert_eq!(parse_version("0.160.0"), Some(CliVersion::new(0, 160, 0)));
	assert_eq!(parse_version("0.160"), None);
	assert_eq!(parse_version("0.160.0.1"), None);
	assert_eq!(parse_version("codex-cli"), None);
}

#[tokio::test]
async fn a_missing_executable_is_typed() {
	let exec = CodexExecutor::new("/nonexistent/omp-codex-adapter-test");
	assert!(matches!(
		fail(exec.probe().await),
		ExecutorError::ExecutableMissing { ref executor } if executor.as_str() == "codex-app-server"
	));
	assert!(matches!(
		fail(exec.open(&request_with(WORKSPACE, None)).await.map(|_| ())),
		ExecutorError::ExecutableMissing { .. }
	));
}

// ---------------------------------------------------------------------------
// Policy
// ---------------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn policies_without_an_exact_codex_mapping_are_refused_up_front() {
	let (exec, rx) = mem(CliVersion::new(0, 160, 0), config());
	for policy in [
		DelegatedPolicy::new(
			SandboxPosture::Unrestricted,
			NetworkPosture::Open,
			ApprovalPosture::Yolo,
		),
		DelegatedPolicy::new(
			SandboxPosture::WorkspaceWrite,
			NetworkPosture::Scoped,
			ApprovalPosture::AlwaysAsk,
		),
		DelegatedPolicy::new(
			SandboxPosture::WorkspaceWrite,
			NetworkPosture::Disabled,
			ApprovalPosture::Write,
		),
		DelegatedPolicy::new(SandboxPosture::ReadOnly, NetworkPosture::Open, ApprovalPosture::Yolo),
	] {
		assert!(wire_policy(policy).is_none());
		let error = fail(
			start_session(&exec, &request_with(policy, None))
				.await
				.map(|_| ()),
		);
		assert!(
			matches!(error, ExecutorError::PolicyUnmappable { requested, .. } if requested == policy)
		);
	}
	assert!(rx.try_recv().is_err(), "nothing was spawned for an unmappable policy");
}

#[tokio::test(start_paused = true)]
async fn a_server_that_applies_another_policy_is_caught_as_drift() {
	for (approval, sandbox) in [
		("untrusted", READ_ONLY),
		("never", WORKSPACE_WRITE),
		("on-request", WORKSPACE_WRITE),
		("untrusted", r#"{"type":"workspaceWrite","networkAccess":true,"writableRoots":[]}"#),
		("untrusted", r#"{"type":"dangerFullAccess"}"#),
	] {
		let (exec, rx) = mem(CliVersion::new(0, 160, 0), config());
		let (session, _peer, _) = open_with(
			&exec,
			&rx,
			&request_with(WORKSPACE, None),
			AGENT_OK,
			&opened("thr-1", approval, sandbox),
		)
		.await;
		assert!(
			matches!(fail(session.map(|_| ())), ExecutorError::PolicyDrift { requested, .. } if requested == WORKSPACE),
			"{approval} {sandbox}"
		);
	}
}

#[tokio::test(start_paused = true)]
async fn an_unexpressible_applied_policy_is_unmappable() {
	let extra_root = r#"{"type":"workspaceWrite","networkAccess":false,"writableRoots":["/etc"]}"#;
	let external = r#"{"type":"externalSandbox","networkAccess":"restricted"}"#;
	for sandbox in [extra_root, external] {
		let (exec, rx) = mem(CliVersion::new(0, 160, 0), config());
		let (session, _peer, _) = open_with(
			&exec,
			&rx,
			&request_with(WORKSPACE, None),
			AGENT_OK,
			&opened("thr-1", "untrusted", sandbox),
		)
		.await;
		assert!(
			matches!(fail(session.map(|_| ())), ExecutorError::PolicyUnmappable { .. }),
			"{sandbox}"
		);
	}
	// A server that routes approvals to its own reviewer cannot be held to the
	// requested policy.
	let (exec, rx) = mem(CliVersion::new(0, 160, 0), config());
	let reviewer = format!(
		r#"{{"thread":{{"id":"thr-1"}},"approvalPolicy":"untrusted","approvalsReviewer":"auto_review","cwd":"/work/project","sandbox":{WORKSPACE_WRITE}}}"#
	);
	let (session, _peer, _) =
		open_with(&exec, &rx, &request_with(WORKSPACE, None), AGENT_OK, &reviewer).await;
	assert!(matches!(fail(session.map(|_| ())), ExecutorError::PolicyUnmappable { .. }));
}

// ---------------------------------------------------------------------------
// Resume
// ---------------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn resume_sends_thread_resume_and_reports_the_resumed_session() {
	let (exec, rx) = mem(CliVersion::new(0, 160, 0), config());
	let (session, _peer, line) = open_with(
		&exec,
		&rx,
		&request_with(WORKSPACE, Some("thr-9")),
		AGENT_OK,
		&opened("thr-9", "untrusted", WORKSPACE_WRITE),
	)
	.await;
	let session = session.expect("resumes");
	assert!(session.info().resumed);
	assert_eq!(session.info().session.as_str(), "thr-9");
	assert!(line.contains(r#""threadId":"thr-9""#), "{line}");
	assert!(line.contains(r#""excludeTurns":true"#), "{line}");
	assert!(line.contains(r#""approvalPolicy":"untrusted""#), "{line}");
	assert!(line.contains(r#""sandbox":"workspace-write""#), "{line}");
}

#[tokio::test(start_paused = true)]
async fn a_rejected_resume_is_a_typed_failure() {
	let (exec, rx) = mem(CliVersion::new(0, 160, 0), config());
	let request = request_with(WORKSPACE, Some("thr-9"));
	let serving = async {
		let mut peer = rx.recv_async().await.expect("peer");
		let init = peer.expect("initialize").await;
		peer.reply(init.id(), AGENT_OK).await;
		peer.expect("initialized").await;
		let resume = peer.expect("thread/resume").await;
		peer
			.send(&format!(
				r#"{{"id":{},"error":{{"code":-32600,"message":"no such thread"}}}}"#,
				resume.id()
			))
			.await;
		peer
	};
	let (session, _peer) = tokio::join!(start_session(&exec, &request), serving);
	assert!(matches!(
		fail(session.map(|_| ())),
		ExecutorError::ResumeFailed { ref requested, .. } if requested.as_str() == "thr-9"
	));
}

#[tokio::test(start_paused = true)]
async fn a_resume_answered_with_another_thread_is_caught_by_the_host() {
	let (exec, rx) = mem(CliVersion::new(0, 160, 0), config());
	let (session, _peer, _) = open_with(
		&exec,
		&rx,
		&request_with(WORKSPACE, Some("thr-9")),
		AGENT_OK,
		&opened("thr-fresh", "untrusted", WORKSPACE_WRITE),
	)
	.await;
	assert!(matches!(fail(session.map(|_| ())), ExecutorError::ResumeFailed { .. }));
}

// ---------------------------------------------------------------------------
// The exact prompt
// ---------------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn the_prompt_is_delivered_byte_for_byte_and_the_digest_is_of_what_was_sent() {
	let (mut session, mut peer) = open_workspace().await;
	let mut resolver = deny();
	let script = async {
		let (inject, start) = peer.accept_turn("t1", true).await;
		peer.send(&turn_started("t1")).await;
		peer.send(&turn_completed("t1", "completed")).await;
		(inject.expect("system text is injected"), start)
	};
	let ((result, _), (inject, start)) = tokio::join!(run(&mut session, &mut resolver), script);
	result.expect("the host accepts the digest the adapter computed");

	let injected: wire::Sent<wire::SentInject> = serde_json::from_str(&inject).expect("typed");
	let injected: Vec<String> = injected
		.params
		.items
		.into_iter()
		.flat_map(|message| message.content)
		.map(|part| part.text.to_string())
		.collect();
	assert_eq!(injected, [
		"Eres el agente de OMP — sistema ✓\n\n  indentado  \n".to_owned(),
		"<rules>\n\tuse tabs\n</rules>".to_owned()
	]);
	assert!(inject.contains(r#""role":"developer""#));

	let started: wire::Sent<wire::SentTurn> = serde_json::from_str(&start).expect("typed");
	let texts: Vec<String> = started
		.params
		.input
		.iter()
		.map(|part| part.text.to_string())
		.collect();
	assert_eq!(texts, ["status: dirty\r\n".to_owned(), "fix the bug in `src/lib.rs`".to_owned()]);
	assert!(start.contains(r#""threadId":"thr-1""#));
	assert!(start.contains(r#""approvalPolicy":"untrusted""#));
	assert!(start.contains(
		r#""sandboxPolicy":{"type":"workspaceWrite","networkAccess":false,"writableRoots":[]}"#
	));
	assert!(!start.contains("text_elements"), "{start}");
}

#[tokio::test(start_paused = true)]
async fn an_order_the_adapter_cannot_deliver_is_a_rewrite_and_ends_the_session() {
	let (mut session, mut peer) = open_workspace().await;
	let reordered = TurnInput {
		prompt: DelegatedPrompt::from_sections(
			vec![section(PromptRole::User, "first"), section(PromptRole::System, "second")],
			bands(),
		),
	};
	let script = async {
		peer.accept_turn("t1", true).await;
	};
	let ((result, _), ()) = tokio::join!(
		run_input(&mut session, &reordered, &mut deny(), &CancellationToken::new()),
		script
	);
	assert!(matches!(fail(result), ExecutorError::PromptRewritten { .. }));
	assert!(!session.is_open());
}

// ---------------------------------------------------------------------------
// Stream faults
// ---------------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn a_line_that_is_not_json_is_a_malformed_stream() {
	let (mut session, mut peer) = open_workspace().await;
	let script = async {
		peer.accept_turn("t1", true).await;
		peer.send("this is not json").await;
	};
	let ((result, _), ()) = tokio::join!(run(&mut session, &mut deny()), script);
	assert!(matches!(fail(result), ExecutorError::MalformedStream {
		fault: FrameFault::NotJson,
		..
	}));
	assert!(!session.is_open());
}

#[tokio::test(start_paused = true)]
async fn a_known_method_with_bad_params_is_a_missing_field() {
	let (mut session, mut peer) = open_workspace().await;
	let script = async {
		peer.accept_turn("t1", true).await;
		peer
			.send(&notification("turn/started", r#"{"threadId":"thr-1"}"#))
			.await;
	};
	let ((result, _), ()) = tokio::join!(run(&mut session, &mut deny()), script);
	assert!(matches!(fail(result), ExecutorError::MalformedStream {
		fault: FrameFault::MissingField { .. },
		..
	}));
}

#[tokio::test(start_paused = true)]
async fn json_that_is_not_json_rpc_is_an_unknown_message() {
	let (mut session, mut peer) = open_workspace().await;
	let script = async {
		peer.accept_turn("t1", true).await;
		peer.send(r#"{"hello":"world"}"#).await;
	};
	let ((result, _), ()) = tokio::join!(run(&mut session, &mut deny()), script);
	assert!(matches!(fail(result), ExecutorError::MalformedStream {
		fault: FrameFault::UnknownMessage,
		..
	}));
}

#[tokio::test(start_paused = true)]
async fn an_oversized_line_fails_the_session() {
	let cfg = SessionConfig { max_frame_bytes: 1024, ..config() };
	let (exec, rx) = mem(CliVersion::new(0, 160, 0), cfg);
	let (session, mut peer, _) = open_with(
		&exec,
		&rx,
		&request_with(WORKSPACE, None),
		AGENT_OK,
		&opened("thr-1", "untrusted", WORKSPACE_WRITE),
	)
	.await;
	let mut session = session.expect("opens");
	let script = async {
		peer.accept_turn("t1", true).await;
		peer.send(&"x".repeat(4096)).await;
	};
	let ((result, _), ()) = tokio::join!(run(&mut session, &mut deny()), script);
	assert!(matches!(fail(result), ExecutorError::MalformedStream {
		fault: FrameFault::Oversize { limit: 1024 },
		..
	}));
	assert!(!session.is_open());
}

#[tokio::test(start_paused = true)]
async fn an_unknown_notification_is_reported_as_an_undeclared_event() {
	let (mut session, mut peer) = open_workspace().await;
	let script = async {
		peer.accept_turn("t1", true).await;
		peer.send(&turn_started("t1")).await;
		peer.send(&notification("brandNew/feature", "{}")).await;
	};
	let ((result, events), ()) = tokio::join!(run(&mut session, &mut deny()), script);
	assert!(matches!(fail(result), ExecutorError::UndeclaredEvent { .. }));
	assert!(!session.is_open());
	assert_eq!(events.len(), 1, "the started event was delivered before the failure");
}

#[tokio::test(start_paused = true)]
async fn notifications_the_pinned_version_defines_but_the_model_ignores_do_not_fail_the_turn() {
	let (mut session, mut peer) = open_workspace().await;
	let script = async {
		peer.accept_turn("t1", true).await;
		peer.send(&turn_started("t1")).await;
		peer
			.send(&notification("warning", r#"{"message":"careful"}"#))
			.await;
		peer
			.send(&notification("thread/status/changed", r#"{"threadId":"thr-1"}"#))
			.await;
		peer
			.send(&notification("item/reasoning/textDelta", r#"{"delta":"hmm"}"#))
			.await;
		peer.send(&turn_completed("t1", "completed")).await;
	};
	let ((result, _), ()) = tokio::join!(run(&mut session, &mut deny()), script);
	assert_eq!(result.expect("completes").end, TurnEnd::Completed);
	assert!(session.is_open());
}

#[tokio::test(start_paused = true)]
async fn a_peer_that_goes_away_mid_turn_is_a_crash_not_a_clean_end() {
	let (mut session, mut peer) = open_workspace().await;
	let script = async {
		peer.accept_turn("t1", true).await;
		peer.send(&turn_started("t1")).await;
		peer.close().await;
	};
	let ((result, _), ()) = tokio::join!(run(&mut session, &mut deny()), script);
	assert!(matches!(fail(result), ExecutorError::Crashed { exit: ExitFact::Unknown, .. }));
	assert!(!session.is_open());
}

#[tokio::test(start_paused = true)]
async fn a_silent_peer_is_hung_and_the_session_is_interrupted_then_ended() {
	let (mut session, mut peer) = open_workspace().await;
	let script = async {
		peer.accept_turn("t1", true).await;
		peer.send(&turn_started("t1")).await;
	};
	let ((result, _), ()) = tokio::join!(run(&mut session, &mut deny()), script);
	assert!(matches!(fail(result), ExecutorError::Hung { .. }));
	assert!(!session.is_open());
	let interrupt = peer.expect("turn/interrupt").await;
	assert!(interrupt.line.contains(r#""turnId":"t1""#), "{}", interrupt.line);
	assert!(interrupt.line.contains(r#""threadId":"thr-1""#), "{}", interrupt.line);
}

// ---------------------------------------------------------------------------
// Interrupt
// ---------------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn a_cooperative_interrupt_settles_the_turn_and_keeps_the_session() {
	let (mut session, mut peer) = open_workspace().await;
	let cancel = CancellationToken::new();
	let script = async {
		peer.accept_turn("t1", true).await;
		peer.send(&turn_started("t1")).await;
		// Give the host the started event before cancelling.
		tokio::time::sleep(Duration::from_millis(10)).await;
		cancel.cancel();
		let interrupt = peer.expect("turn/interrupt").await;
		assert!(interrupt.line.contains(r#""turnId":"t1""#), "{}", interrupt.line);
		peer.reply(interrupt.id(), "{}").await;
		peer.send(&turn_completed("t1", "interrupted")).await;
	};
	let ((result, events), ()) =
		tokio::join!(run_input(&mut session, &input(), &mut deny(), &cancel), script);
	assert!(matches!(fail(result), ExecutorError::Cancelled { .. }));
	assert!(session.is_open(), "the CLI settled inside the grace period");
	assert!(events.iter().any(|event| matches!(
		event,
		DelegatedEvent::Lifecycle(LifecycleEvent::TurnCompleted {
			outcome: TurnEnd::Interrupted,
			..
		})
	)));
}

#[tokio::test(start_paused = true)]
async fn an_ignored_interrupt_ends_the_session() {
	let (mut session, mut peer) = open_workspace().await;
	let cancel = CancellationToken::new();
	let script = async {
		peer.accept_turn("t1", true).await;
		peer.send(&turn_started("t1")).await;
		tokio::time::sleep(Duration::from_millis(10)).await;
		cancel.cancel();
		peer.expect("turn/interrupt").await;
		// ... and never settle.
	};
	let ((result, _), ()) =
		tokio::join!(run_input(&mut session, &input(), &mut deny(), &cancel), script);
	assert!(matches!(fail(result), ExecutorError::Cancelled { .. }));
	assert!(!session.is_open(), "an unsettled interrupt means the tree is killed");
}

// ---------------------------------------------------------------------------
// Approvals
// ---------------------------------------------------------------------------

/// Runs a turn during which the server files one approval request and
/// returns the client's answer line, the host's tally and who was asked.
async fn approval_round(
	session: &mut Active,
	peer: &mut Peer,
	method: &str,
	params: &str,
	resolver: &mut Scripted,
) -> (String, PermissionTally) {
	let script = async {
		peer.accept_turn("t1", true).await;
		peer.send(&turn_started("t1")).await;
		peer.send(&server_request(900, method, params)).await;
		let answer = peer.recv().await;
		assert_eq!(answer.head.id, Some(900));
		assert!(answer.head.method.is_none(), "answers are responses");
		peer.send(&turn_completed("t1", "completed")).await;
		answer.line
	};
	let ((result, _), answer) = tokio::join!(run(session, resolver), script);
	(answer, result.expect("turn completes").permissions)
}

const COMMAND: &str = "item/commandExecution/requestApproval";
const FILE_CHANGE: &str = "item/fileChange/requestApproval";
const PERMISSIONS: &str = "item/permissions/requestApproval";

fn command_params(extra: &str) -> String {
	format!(
		r#"{{"threadId":"thr-1","turnId":"t1","itemId":"c1","startedAtMs":1,"command":"cargo test"{extra}}}"#
	)
}

#[tokio::test(start_paused = true)]
async fn an_allow_once_becomes_a_single_accept() {
	let (mut session, mut peer) = open_workspace().await;
	let mut resolver = Scripted::new(decision(true, ApprovalScope::Once));
	let (answer, tally) =
		approval_round(&mut session, &mut peer, COMMAND, &command_params(""), &mut resolver).await;
	assert!(answer.contains(r#""decision":"accept""#), "{answer}");
	assert_eq!((tally.asked, tally.allowed), (1, 1));
	assert_eq!(resolver.asked[0].kind, PermissionKind::Exec);
	assert_eq!(resolver.asked[0].subject.as_str(), "cargo test");
	assert_eq!(resolver.asked[0].call.as_ref().map(|call| call.as_str()), Some("c1"));
	assert!(!resolver.asked[0].session_grant, "no grant outlives the action it was asked about");
}

#[tokio::test(start_paused = true)]
async fn a_session_scoped_approval_is_narrowed_to_one_accept() {
	let (mut session, mut peer) = open_workspace().await;
	let mut resolver = Scripted::new(decision(true, ApprovalScope::Session));
	let (answer, tally) =
		approval_round(&mut session, &mut peer, COMMAND, &command_params(""), &mut resolver).await;
	assert!(answer.contains(r#""decision":"accept""#), "{answer}");
	assert!(!answer.contains("ForSession"), "{answer}");
	assert!(!answer.contains("mendment"), "{answer}");
	assert_eq!(tally.allowed, 1);
}

#[tokio::test(start_paused = true)]
async fn a_denial_becomes_decline() {
	let (mut session, mut peer) = open_workspace().await;
	let mut resolver = deny();
	let (answer, tally) =
		approval_round(&mut session, &mut peer, COMMAND, &command_params(""), &mut resolver).await;
	assert!(answer.contains(r#""decision":"decline""#), "{answer}");
	assert_eq!((tally.asked, tally.denied), (1, 1));
}

#[tokio::test(start_paused = true)]
async fn an_approval_with_no_codex_equivalent_fails_closed() {
	let (mut session, mut peer) = open_workspace().await;
	let mut resolver = Scripted::new(decision(true, ApprovalScope::Custom(Str::new("per-project"))));
	let (answer, tally) =
		approval_round(&mut session, &mut peer, COMMAND, &command_params(""), &mut resolver).await;
	assert!(answer.contains(r#""decision":"decline""#), "{answer}");
	assert_eq!((tally.allowed, tally.refused_by_policy), (0, 1));
}

#[tokio::test(start_paused = true)]
async fn an_unaudited_fail_open_approval_fails_closed() {
	let (mut session, mut peer) = open_workspace().await;
	let timed_out = ApprovalDecision {
		source: ApprovalSource::Timeout,
		audited: false,
		..decision(true, ApprovalScope::Once)
	};
	let mut resolver = Scripted::new(timed_out);
	let (answer, tally) =
		approval_round(&mut session, &mut peer, COMMAND, &command_params(""), &mut resolver).await;
	assert!(answer.contains(r#""decision":"decline""#), "{answer}");
	assert_eq!(tally.refused_by_policy, 1);
}

#[tokio::test(start_paused = true)]
async fn terminal_input_is_unclassified_and_never_put_to_the_user() {
	let (mut session, mut peer) = open_workspace().await;
	let mut resolver = Scripted::new(decision(true, ApprovalScope::Once));
	let (answer, tally) = approval_round(
		&mut session,
		&mut peer,
		COMMAND,
		&command_params(r#","kind":"writeStdin""#),
		&mut resolver,
	)
	.await;
	assert!(answer.contains(r#""decision":"decline""#), "{answer}");
	assert!(resolver.asked.is_empty());
	assert_eq!(tally.refused_by_policy, 1);
}

#[tokio::test(start_paused = true)]
async fn network_access_is_refused_by_a_policy_that_disables_the_network() {
	let (mut session, mut peer) = open_workspace().await;
	let mut resolver = Scripted::new(decision(true, ApprovalScope::Once));
	let (answer, tally) = approval_round(
		&mut session,
		&mut peer,
		COMMAND,
		&command_params(r#","networkApprovalContext":{"host":"example.com","protocol":"https"}"#),
		&mut resolver,
	)
	.await;
	assert!(answer.contains(r#""decision":"decline""#), "{answer}");
	assert!(resolver.asked.is_empty());
	assert_eq!(tally.refused_by_policy, 1);
}

#[tokio::test(start_paused = true)]
async fn a_write_is_refused_by_a_read_only_session_without_asking() {
	let (mut session, mut peer) = open_policy(STRICT, READ_ONLY, "untrusted").await;
	let mut resolver = Scripted::new(decision(true, ApprovalScope::Once));
	let params =
		r#"{"threadId":"thr-1","turnId":"t1","itemId":"f1","startedAtMs":1,"reason":"edit"}"#;
	let (answer, tally) =
		approval_round(&mut session, &mut peer, FILE_CHANGE, params, &mut resolver).await;
	assert!(answer.contains(r#""decision":"decline""#), "{answer}");
	assert!(resolver.asked.is_empty());
	assert_eq!(tally.refused_by_policy, 1);
}

#[tokio::test(start_paused = true)]
async fn a_file_change_approval_is_a_write_request() {
	let (mut session, mut peer) = open_workspace().await;
	let mut resolver = Scripted::new(decision(true, ApprovalScope::Once));
	let params = r#"{"threadId":"thr-1","turnId":"t1","itemId":"f1","startedAtMs":1,"reason":"edit","grantRoot":"/work/project/out"}"#;
	let (answer, _) =
		approval_round(&mut session, &mut peer, FILE_CHANGE, params, &mut resolver).await;
	assert!(answer.contains(r#""decision":"accept""#), "{answer}");
	assert_eq!(resolver.asked[0].kind, PermissionKind::Write);
	assert_eq!(resolver.asked[0].subject.as_str(), "/work/project/out");
}

fn permissions_params(profile: &str) -> String {
	format!(
		r#"{{"threadId":"thr-1","turnId":"t1","itemId":"p1","startedAtMs":1,"cwd":"/work/project","permissions":{profile}}}"#
	)
}

#[tokio::test(start_paused = true)]
async fn a_permission_profile_is_granted_exactly_as_asked_and_only_for_the_turn() {
	let (mut session, mut peer) = open_workspace().await;
	let mut resolver = Scripted::new(decision(true, ApprovalScope::Session));
	let profile = r#"{"fileSystem":{"write":["/work/project/out"]}}"#;
	let (answer, _) = approval_round(
		&mut session,
		&mut peer,
		PERMISSIONS,
		&permissions_params(profile),
		&mut resolver,
	)
	.await;
	assert!(
		answer.contains(r#""permissions":{"fileSystem":{"write":["/work/project/out"]}}"#),
		"{answer}"
	);
	assert!(answer.contains(r#""scope":"turn""#), "{answer}");
	assert_eq!(resolver.asked[0].kind, PermissionKind::Write);
}

#[tokio::test(start_paused = true)]
async fn a_denied_permission_profile_grants_nothing() {
	let (mut session, mut peer) = open_workspace().await;
	let mut resolver = deny();
	let profile = r#"{"fileSystem":{"write":["/work/project/out"]}}"#;
	let (answer, _) = approval_round(
		&mut session,
		&mut peer,
		PERMISSIONS,
		&permissions_params(profile),
		&mut resolver,
	)
	.await;
	assert!(answer.contains(r#""permissions":{}"#), "{answer}");
	assert!(answer.contains(r#""scope":"turn""#), "{answer}");
}

#[tokio::test(start_paused = true)]
async fn structured_or_mixed_permission_profiles_are_unclassified() {
	for profile in [
		r#"{"fileSystem":{"entries":[{"access":"write","path":{"type":"special","value":{"kind":"root"}}}]}}"#,
		r#"{"fileSystem":{"write":["/work/project/out"]},"network":{"enabled":true}}"#,
		r#"{}"#,
	] {
		let (mut session, mut peer) = open_workspace().await;
		let mut resolver = Scripted::new(decision(true, ApprovalScope::Once));
		let (answer, tally) = approval_round(
			&mut session,
			&mut peer,
			PERMISSIONS,
			&permissions_params(profile),
			&mut resolver,
		)
		.await;
		assert!(answer.contains(r#""permissions":{}"#), "{profile}: {answer}");
		assert!(resolver.asked.is_empty(), "{profile}");
		assert_eq!(tally.refused_by_policy, 1, "{profile}");
	}
}

#[tokio::test(start_paused = true)]
async fn requests_with_no_omp_counterpart_are_refused_and_never_answered_with_a_credential() {
	let (mut session, mut peer) = open_workspace().await;
	let script = async {
		peer.accept_turn("t1", true).await;
		peer.send(&turn_started("t1")).await;
		let mut answers = Vec::new();
		for (id, method, params) in [
			(
				901,
				"item/tool/call",
				r#"{"threadId":"thr-1","turnId":"t1","callId":"x","tool":"t","arguments":{}}"#,
			),
			(902, "account/chatgptAuthTokens/refresh", r#"{"reason":"unauthorized"}"#),
			(903, "mcpServer/elicitation/request", r#"{"threadId":"thr-1","serverName":"s"}"#),
			(
				904,
				"item/tool/requestUserInput",
				r#"{"threadId":"thr-1","turnId":"t1","itemId":"u","questions":[]}"#,
			),
			(
				905,
				"execCommandApproval",
				r#"{"conversationId":"c","callId":"x","command":["ls"],"cwd":"/","parsedCmd":[]}"#,
			),
			(906, "applyPatchApproval", r#"{"conversationId":"c","callId":"x","fileChanges":{}}"#),
		] {
			peer.send(&server_request(id, method, params)).await;
			let answer = peer.recv().await;
			assert_eq!(answer.head.id, Some(id));
			answers.push(answer.line);
		}
		peer.send(&turn_completed("t1", "completed")).await;
		answers
	};
	let ((result, _), answers) = tokio::join!(run(&mut session, &mut deny()), script);
	result.expect("refusals do not fail the turn");
	assert!(
		answers[0].contains(r#""success":false"#) && answers[0].contains(r#""contentItems":[]"#)
	);
	assert!(answers[1].contains(r#""error""#) && answers[1].contains("-32601"), "{}", answers[1]);
	assert!(!answers[1].contains("token"), "{}", answers[1]);
	assert!(answers[2].contains(r#""action":"cancel""#));
	assert!(answers[3].contains("-32601"));
	assert!(answers[4].contains(r#""decision":"denied""#));
	assert!(answers[5].contains(r#""decision":"denied""#));
}

#[tokio::test(start_paused = true)]
async fn an_unknown_server_request_is_refused_and_reported() {
	let (mut session, mut peer) = open_workspace().await;
	let script = async {
		peer.accept_turn("t1", true).await;
		peer.send(&turn_started("t1")).await;
		peer
			.send(&server_request(950, "brandNew/request", "{}"))
			.await;
		peer.recv().await
	};
	let ((result, _), answer) = tokio::join!(run(&mut session, &mut deny()), script);
	assert!(matches!(fail(result), ExecutorError::UndeclaredEvent { .. }));
	assert!(answer.line.contains("-32601"));
}

// ---------------------------------------------------------------------------
// Event normalization
// ---------------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn deltas_tools_and_usage_are_normalized_and_usage_is_per_turn() {
	let (mut session, mut peer) = open_workspace().await;
	let usage = |last: &str, total: &str| {
		notification(
			"thread/tokenUsage/updated",
			&format!(
				r#"{{"threadId":"thr-1","turnId":"t1","tokenUsage":{{"last":{last},"total":{total}}}}}"#
			),
		)
	};
	let first = r#"{"inputTokens":100,"cachedInputTokens":40,"outputTokens":10,"reasoningOutputTokens":2,"totalTokens":110}"#;
	let script = async {
		peer.accept_turn("t1", true).await;
		peer.send(&turn_started("t1")).await;
		let delta = |text: &str| {
			notification(
				"item/agentMessage/delta",
				&format!(r#"{{"threadId":"thr-1","turnId":"t1","itemId":"m1","delta":"{text}"}}"#),
			)
		};
		peer.send(&delta("Hel")).await;
		peer.send(&delta("lo")).await;
		peer.send(&item(
			"item/started",
			"t1",
			r#"{"type":"commandExecution","id":"c1","command":"ls -la","commandActions":[],"cwd":"/work/project","status":"inProgress"}"#,
		))
		.await;
		peer.send(&item(
			"item/completed",
			"t1",
			r#"{"type":"commandExecution","id":"c1","command":"ls -la","commandActions":[],"cwd":"/work/project","status":"completed","exitCode":0}"#,
		))
		.await;
		peer
			.send(&item(
				"item/started",
				"t1",
				r#"{"type":"fileChange","id":"f1","changes":[],"status":"inProgress"}"#,
			))
			.await;
		peer
			.send(&item(
				"item/completed",
				"t1",
				r#"{"type":"fileChange","id":"f1","changes":[],"status":"declined"}"#,
			))
			.await;
		peer.send(&item("item/started", "t1", r#"{"type":"mcpToolCall","id":"x1","server":"s","tool":"lookup","arguments":{},"status":"inProgress"}"#)).await;
		peer.send(&item("item/completed", "t1", r#"{"type":"mcpToolCall","id":"x1","server":"s","tool":"lookup","arguments":{},"status":"failed"}"#)).await;
		// Already streamed as deltas: not delivered a second time.
		peer
			.send(&item("item/completed", "t1", r#"{"type":"agentMessage","id":"m1","text":"Hello"}"#))
			.await;
		// Never streamed: delivered whole.
		peer
			.send(&item("item/completed", "t1", r#"{"type":"agentMessage","id":"m2","text":"Done."}"#))
			.await;
		// Items with no tool meaning produce nothing.
		peer
			.send(&item("item/completed", "t1", r#"{"type":"reasoning","id":"r1"}"#))
			.await;
		peer.send(&usage(first, first)).await;
		peer.send(&turn_completed("t1", "completed")).await;
	};
	let ((result, events), ()) = tokio::join!(run(&mut session, &mut deny()), script);
	let report = result.expect("completes");
	assert_eq!(report.end, TurnEnd::Completed);
	assert_eq!(report.usage.input_tokens, 60, "cached input is its own dimension");
	assert_eq!(report.usage.cache_read_tokens, 40);
	assert_eq!(report.usage.output_tokens, 10);
	assert_eq!(report.usage.reasoning_tokens, 2);
	assert_eq!(report.usage.source, UsageSource::Provider);

	let deltas: Vec<&str> = events
		.iter()
		.filter_map(|event| match event {
			DelegatedEvent::AgentTextDelta { text, .. } => Some(text.as_str()),
			_ => None,
		})
		.collect();
	assert_eq!(deltas, ["Hel", "lo"]);
	let messages: Vec<&str> = events
		.iter()
		.filter_map(|event| match event {
			DelegatedEvent::AgentMessage { text, .. } => Some(text.as_str()),
			_ => None,
		})
		.collect();
	assert_eq!(messages, ["Done."]);
	let tools: Vec<&ToolEvent> = events
		.iter()
		.filter_map(|event| match event {
			DelegatedEvent::Tool(tool) => Some(tool),
			_ => None,
		})
		.collect();
	assert_eq!(tools.len(), 6);
	assert!(
		matches!(tools[0], ToolEvent::Started { kind: ToolKind::Command, label, .. } if label.as_str() == "ls -la")
	);
	assert!(matches!(tools[1], ToolEvent::Finished { outcome: ToolOutcome::Succeeded, .. }));
	assert!(matches!(tools[2], ToolEvent::Started { kind: ToolKind::FileChange, .. }));
	assert!(matches!(tools[3], ToolEvent::Finished { outcome: ToolOutcome::Denied, .. }));
	assert!(matches!(tools[4], ToolEvent::Started { kind: ToolKind::External, .. }));
	assert!(matches!(tools[5], ToolEvent::Finished { outcome: ToolOutcome::Failed, .. }));

	// Second turn: the server's totals are cumulative over the thread; the
	// report is this turn's share.
	let second_total = r#"{"inputTokens":130,"cachedInputTokens":40,"outputTokens":15,"reasoningOutputTokens":2,"totalTokens":145}"#;
	let second_last = r#"{"inputTokens":30,"cachedInputTokens":0,"outputTokens":5,"reasoningOutputTokens":0,"totalTokens":35}"#;
	let script = async {
		peer.accept_turn("t2", true).await;
		peer.send(&turn_started("t2")).await;
		peer.send(&notification(
			"thread/tokenUsage/updated",
			&format!(r#"{{"threadId":"thr-1","turnId":"t2","tokenUsage":{{"last":{second_last},"total":{second_total}}}}}"#),
		))
		.await;
		peer.send(&turn_completed("t2", "completed")).await;
	};
	let ((result, _), ()) = tokio::join!(run(&mut session, &mut deny()), script);
	let report = result.expect("second turn");
	assert_eq!((report.usage.input_tokens, report.usage.output_tokens), (30, 5));
	assert_eq!(report.usage.cache_read_tokens, 0);
}

#[tokio::test(start_paused = true)]
async fn failures_are_typed() {
	for (info, expected) in [
		(r#""unauthorized""#, "auth_required"),
		(r#""rateLimitExceeded""#, "rate_limited"),
		(r#""contextWindowExceeded""#, "context_overflow"),
		(r#""badRequest""#, "refused"),
		(r#"{"httpConnectionFailed":{"httpStatusCode":502}}"#, "internal"),
	] {
		let (mut session, mut peer) = open_workspace().await;
		let script = async {
			peer.accept_turn("t1", true).await;
			peer.send(&turn_started("t1")).await;
			// A retried error is not the end of the turn.
			peer
				.send(&notification(
					"error",
					r#"{"threadId":"thr-1","turnId":"t1","willRetry":true,"error":{"message":"blip"}}"#,
				))
				.await;
			peer.send(&notification(
				"error",
				&format!(
					r#"{{"threadId":"thr-1","turnId":"t1","willRetry":false,"error":{{"message":"boom","codexErrorInfo":{info}}}}}"#
				),
			))
			.await;
			peer.send(&turn_completed("t1", "failed")).await;
		};
		let ((result, _), ()) = tokio::join!(run(&mut session, &mut deny()), script);
		let error = fail(result);
		match (expected, &error) {
			("auth_required", ExecutorError::AuthRequired { .. }) => {},
			("rate_limited", ExecutorError::Remote { failure: RemoteFailure::RateLimited, .. })
			| (
				"context_overflow",
				ExecutorError::Remote { failure: RemoteFailure::ContextOverflow, .. },
			)
			| ("refused", ExecutorError::Remote { failure: RemoteFailure::Refused, .. })
			| ("internal", ExecutorError::Remote { failure: RemoteFailure::Internal, .. }) => {},
			_ => panic!("{info} mapped to {error:?}, expected {expected}"),
		}
		assert!(session.is_open(), "a remote failure is turn-local");
	}
}

#[tokio::test(start_paused = true)]
async fn a_second_turn_runs_on_the_same_session() {
	let (mut session, mut peer) = open_workspace().await;
	for turn in ["t1", "t2"] {
		let script = async {
			peer.accept_turn(turn, true).await;
			peer.send(&turn_started(turn)).await;
			peer.send(&turn_completed(turn, "completed")).await;
		};
		let ((result, _), ()) = tokio::join!(run(&mut session, &mut deny()), script);
		assert_eq!(result.expect("turn").turn.as_str(), turn);
	}
}

#[tokio::test(start_paused = true)]
async fn an_event_of_another_turn_is_a_host_level_protocol_violation() {
	let (mut session, mut peer) = open_workspace().await;
	let script = async {
		peer.accept_turn("t1", true).await;
		peer.send(&turn_started("t9")).await;
	};
	let ((result, _), ()) = tokio::join!(run(&mut session, &mut deny()), script);
	assert!(matches!(fail(result), ExecutorError::Protocol {
		violation: ProtocolViolation::TurnMismatch { .. },
		..
	}));
}

#[tokio::test(start_paused = true)]
async fn the_rejection_of_turn_start_is_a_protocol_failure() {
	let (mut session, mut peer) = open_workspace().await;
	let script = async {
		let inject = peer.expect("thread/inject_items").await;
		peer.reply(inject.id(), "{}").await;
		let start = peer.expect("turn/start").await;
		peer
			.send(&format!(
				r#"{{"id":{},"error":{{"code":-32001,"message":"overloaded"}}}}"#,
				start.id()
			))
			.await;
	};
	let ((result, _), ()) = tokio::join!(run(&mut session, &mut deny()), script);
	assert!(matches!(fail(result), ExecutorError::Protocol {
		violation: ProtocolViolation::RequestRejected { code: -32001 },
		..
	}));
}

#[tokio::test(start_paused = true)]
async fn terminate_is_idempotent() {
	let (mut session, mut peer) = open_workspace().await;
	session.terminate().await.expect("first");
	session.terminate().await.expect("second");
	assert!(!session.is_open());
	assert!(peer.try_recv().await.is_none(), "the writer shut down");
	assert!(matches!(
		fail(
			session
				.run_turn(
					&input(),
					limits(),
					&CancellationToken::new(),
					&mut deny(),
					&flume::unbounded().0
				)
				.await
		),
		ExecutorError::SessionClosed { .. }
	));
}

// ---------------------------------------------------------------------------
// Real process trees (throwaway `sh` children, not Codex)
// ---------------------------------------------------------------------------

#[cfg(unix)]
mod processes {
	use nix::unistd::Pid;
	use tokio::process::Command;

	use super::*;
	use crate::{child_tree::ChildTree, test_process::process_terminated};

	/// A session over a real `sh -c <script>` child.
	fn shell_session(script: &str) -> CodexSession {
		let mut command = Command::new("sh");
		command.arg("-c").arg(script);
		let spawned = ChildTree::spawn(command).expect("spawn sh");
		let transport = Transport {
			reader: Box::new(spawned.stdout),
			writer: Box::new(spawned.stdin),
			stderr: Some(spawned.stderr),
			tree:   Some(spawned.tree),
		};
		CodexSession::new(CodexExecutor::executor_id(), transport, config())
	}

	/// Waits for the grandchild pid the script wrote.
	async fn grandchild(file: &std::path::Path) -> i32 {
		for _ in 0..200 {
			if let Ok(text) = std::fs::read_to_string(file)
				&& let Ok(pid) = text.trim().parse()
			{
				return pid;
			}
			tokio::time::sleep(Duration::from_millis(25)).await;
		}
		panic!("the child never wrote its grandchild pid");
	}

	async fn gone(pid: i32) -> bool {
		for _ in 0..200 {
			if process_terminated(Pid::from_raw(pid)) {
				return true;
			}
			tokio::time::sleep(Duration::from_millis(25)).await;
		}
		false
	}

	fn pid_script(file: &std::path::Path) -> String {
		format!("sleep 300 & echo $! > '{}'; wait", file.display())
	}

	#[tokio::test]
	async fn a_child_that_exits_is_crashed_with_its_status() {
		let mut session = shell_session("exit 3");
		let error = fail(session.handshake(&request_with(WORKSPACE, None)).await);
		assert!(matches!(error, ExecutorError::Crashed { exit: ExitFact::Code(3), .. }), "{error:?}");
	}

	#[tokio::test]
	async fn a_hung_child_times_out_and_terminate_kills_the_whole_tree() {
		let dir = tempfile::tempdir().expect("tempdir");
		let file = dir.path().join("pid");
		let mut session = shell_session(&pid_script(&file));
		let hung =
			timeout(Duration::from_millis(300), session.handshake(&request_with(WORKSPACE, None)))
				.await;
		assert!(hung.is_err(), "the silent child never answers initialize");
		let pid = grandchild(&file).await;
		assert!(!process_terminated(Pid::from_raw(pid)));
		DelegatedSession::terminate(&mut session)
			.await
			.expect("teardown");
		assert!(gone(pid).await, "the grandchild outlived terminate");
		DelegatedSession::terminate(&mut session)
			.await
			.expect("idempotent");
	}

	#[tokio::test]
	async fn dropping_a_session_kills_the_whole_tree() {
		let dir = tempfile::tempdir().expect("tempdir");
		let file = dir.path().join("pid");
		let session = shell_session(&pid_script(&file));
		let pid = grandchild(&file).await;
		drop(session);
		assert!(gone(pid).await, "the grandchild outlived the dropped session");
	}
}
