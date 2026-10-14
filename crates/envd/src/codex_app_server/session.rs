//! One live Codex `app-server` conversation over a pair of byte streams.
//!
//! The session is transport-agnostic: the production executor feeds it the
//! pipes of a spawned child ([`ChildTree`]), tests feed it in-memory pipes.
//!
//! Concurrency model: a writer task owns the output stream and drains an
//! unbounded queue, so sending never blocks the reader; the reader is polled
//! inline by [`CodexSession::next_event`] through a cancel-safe
//! [`LineReader`]. Everything the reader sees is decoded into typed
//! [`wire`] messages and normalized into [`DelegatedEvent`]s.

use std::{collections::VecDeque, path::PathBuf, time::Duration};

use bytes::Bytes;
use omp_agent::delegated::{
	CallId, CliSessionId, CliVersion, DelegatedEvent, DelegatedPrompt, DelegatedSession,
	ExecutorError, ExecutorId, ExitFact, Feature, FrameFault, LifecycleEvent, PermissionAnswer,
	PermissionKind, PermissionRequest, PromptId, PromptRole, PromptSection, ProtocolViolation,
	RemoteFailure, SessionInfo, SessionRequest, TeardownFault, ToolEvent, ToolKind, ToolOutcome,
	TurnAccepted, TurnEnd, TurnId, TurnInput, VersionRange,
};
use omp_ai::{Usage, UsageSource};
use omp_core::{Str, sf};
use serde::{
	Serialize,
	de::{DeserializeOwned, IgnoredAny},
};
use serde_json::value::RawValue;
use tokio::{
	io::{AsyncRead, AsyncWrite, AsyncWriteExt as _},
	process::ChildStderr,
	task::JoinHandle,
	time::timeout,
};

use super::{
	frame::{FrameError, LineReader},
	policy::{WirePolicy, applied_policy, wire_policy},
	wire::{self, Envelope, JsonRpcMessage, Method, RequestId},
};
use crate::child_tree::ChildTree;

/// The boxed input stream; boxed once, at construction.
pub(crate) type BoxRead = Box<dyn AsyncRead + Send + Unpin>;
/// The boxed output stream; boxed once, at construction.
pub(crate) type BoxWrite = Box<dyn AsyncWrite + Send + Unpin>;

/// Everything a session needs from its transport.
pub(crate) struct Transport {
	/// Messages from the CLI.
	pub(crate) reader: BoxRead,
	/// Messages to the CLI.
	pub(crate) writer: BoxWrite,
	/// The CLI's stderr, drained and discarded so it can never fill a pipe.
	pub(crate) stderr: Option<ChildStderr>,
	/// The owned process tree, absent for in-memory transports.
	pub(crate) tree:   Option<ChildTree>,
}

/// Limits and identity of one session.
#[derive(Clone, Copy)]
pub(crate) struct SessionConfig {
	/// Longest accepted inbound line.
	pub(crate) max_frame_bytes: usize,
	/// How long [`DelegatedSession::terminate`] waits for an interrupted turn
	/// to settle before it kills the tree.
	pub(crate) terminate_grace: Duration,
	/// CLI versions the executor supports; checked against the server's
	/// user agent as a second line after the host's `--version` probe.
	pub(crate) versions:        VersionRange,
}

/// What the server asked and how to answer it.
enum PendingKind {
	/// Command approval: answered with a decision.
	Command,
	/// File-change approval: answered with a decision.
	FileChange,
	/// Permission-profile approval: answered with the granted profile.
	Permissions(wire::PermissionProfile),
}

/// A server request waiting for the host's answer.
struct Pending {
	prompt:   PromptId,
	id:       RequestId,
	kind:     PendingKind,
	/// The server withdrew the request (`serverRequest/resolved`).
	resolved: bool,
}

/// A response routed out of the inbound stream.
struct Reply {
	id:      RequestId,
	outcome: Result<Box<RawValue>, i64>,
}

/// A live Codex session.
pub struct CodexSession {
	executor:   ExecutorId,
	reader:     LineReader<BoxRead>,
	out:        flume::Sender<Bytes>,
	tasks:      Vec<JoinHandle<()>>,
	tree:       Option<ChildTree>,
	cfg:        SessionConfig,
	wire:       Option<WirePolicy>,
	workdir:    String,
	thread:     Str,
	next_id:    i64,
	frames:     u64,
	queue:      VecDeque<DelegatedEvent>,
	/// The turn in flight, from `turn/start` until `turn/completed`.
	turn:       Option<Str>,
	pending:    Vec<Pending>,
	/// Agent-message items already delivered as deltas.
	streamed:   Vec<Str>,
	usage_base: Option<wire::TokenBreakdown>,
	last_total: Option<wire::TokenBreakdown>,
	last_error: Option<(Str, Option<wire::ErrorInfo>)>,
	terminated: bool,
}

impl CodexSession {
	/// Builds the session around `transport`; nothing is exchanged yet.
	pub(crate) fn new(executor: ExecutorId, transport: Transport, cfg: SessionConfig) -> Self {
		let Transport { reader, writer, stderr, tree } = transport;
		let (out, queue) = flume::unbounded::<Bytes>();
		let mut tasks = vec![tokio::spawn(write_loop(writer, queue))];
		if let Some(mut stderr) = stderr {
			tasks.push(tokio::spawn(async move {
				let _ = tokio::io::copy(&mut stderr, &mut tokio::io::sink()).await;
			}));
		}
		Self {
			executor,
			reader: LineReader::new(reader, cfg.max_frame_bytes),
			out,
			tasks,
			tree,
			cfg,
			wire: None,
			workdir: String::new(),
			thread: Str::empty(),
			next_id: 1,
			frames: 0,
			queue: VecDeque::new(),
			turn: None,
			pending: Vec::new(),
			streamed: Vec::new(),
			usage_base: None,
			last_total: None,
			last_error: None,
			terminated: false,
		}
	}

	// -----------------------------------------------------------------------
	// Errors
	// -----------------------------------------------------------------------

	fn malformed(&self, fault: FrameFault) -> ExecutorError {
		ExecutorError::MalformedStream {
			executor: self.executor.clone(),
			frame: self.frames.saturating_sub(1),
			fault,
		}
	}

	fn protocol(&self, violation: ProtocolViolation) -> ExecutorError {
		ExecutorError::Protocol { executor: self.executor.clone(), violation }
	}

	async fn crashed(&mut self) -> ExecutorError {
		let exit = match self.tree.as_mut() {
			Some(tree) => match timeout(Duration::from_secs(1), tree.wait()).await {
				Ok(Some(status)) => exit_fact(status),
				Ok(None) | Err(_) => ExitFact::Unknown,
			},
			None => ExitFact::Unknown,
		};
		ExecutorError::Crashed { executor: self.executor.clone(), exit }
	}

	// -----------------------------------------------------------------------
	// Outbound
	// -----------------------------------------------------------------------

	fn encode<S: Serialize>(value: &S) -> Bytes {
		let mut line = serde_json::to_vec(value).expect("typed wire structs always serialize");
		line.push(b'\n');
		Bytes::from(line)
	}

	async fn send_bytes(&mut self, line: Bytes) -> Result<(), ExecutorError> {
		if self.out.send(line).is_err() {
			return Err(self.crashed().await);
		}
		Ok(())
	}

	/// Allocates an id and encodes a request line.
	fn request_line<P: Serialize>(&mut self, method: &'static str, params: &P) -> (i64, Bytes) {
		let id = self.next_id;
		self.next_id += 1;
		(id, Self::encode(&wire::OutRequest { id, method, params }))
	}

	async fn respond<R: Serialize>(
		&mut self,
		id: &RequestId,
		result: &R,
	) -> Result<(), ExecutorError> {
		self
			.send_bytes(Self::encode(&wire::OutResponse { id, result }))
			.await
	}

	async fn refuse(&mut self, id: &RequestId) -> Result<(), ExecutorError> {
		let error = wire::OutError {
			id,
			error: wire::OutErrorBody {
				code:    wire::METHOD_NOT_FOUND,
				message: "unsupported by this client",
			},
		};
		self.send_bytes(Self::encode(&error)).await
	}

	/// Sends a request and waits for its response, routing everything else.
	///
	/// Not cancel-safe: the host ends the session when it abandons a call.
	async fn call<P: Serialize, T: DeserializeOwned>(
		&mut self,
		method: &'static str,
		params: &P,
	) -> Result<Result<T, i64>, ExecutorError> {
		let (id, line) = self.request_line(method, params);
		self.send_bytes(line).await?;
		self.await_response::<T>(id).await
	}

	async fn await_response<T: DeserializeOwned>(
		&mut self,
		id: i64,
	) -> Result<Result<T, i64>, ExecutorError> {
		loop {
			let line = self.read_line().await?;
			let Some(reply) = self.route(&line).await? else {
				continue;
			};
			if reply.id != RequestId::Int(id) {
				continue;
			}
			return match reply.outcome {
				Ok(raw) => serde_json::from_str(raw.get())
					.map(Ok)
					.map_err(|_| self.malformed(FrameFault::MissingField { field: "result" })),
				Err(code) => Ok(Err(code)),
			};
		}
	}

	// -----------------------------------------------------------------------
	// Handshake
	// -----------------------------------------------------------------------

	/// `initialize`, `initialized`, then `thread/start` or `thread/resume`,
	/// verifying the policy the server reports.
	pub(crate) async fn handshake(
		&mut self,
		request: &SessionRequest,
	) -> Result<SessionInfo, ExecutorError> {
		let requested = request.policy;
		let wire_policy = wire_policy(requested).ok_or_else(|| ExecutorError::PolicyUnmappable {
			executor: self.executor.clone(),
			requested,
		})?;
		let Some(cwd) = request.workdir.to_str() else {
			return Err(ExecutorError::WorkdirNotUtf8 { path: request.workdir.clone() });
		};
		let cwd = cwd.to_owned();

		let init = wire::InitializeParams {
			client_info: wire::ClientInfo { name: "omp", version: env!("CARGO_PKG_VERSION") },
		};
		let init: wire::InitializeResult = match self.call(wire::INITIALIZE, &init).await? {
			Ok(result) => result,
			Err(code) => return Err(self.protocol(ProtocolViolation::RequestRejected { code })),
		};
		if let Some(found) = user_agent_version(&init.user_agent)
			&& !self.cfg.versions.contains(found)
		{
			return Err(ExecutorError::UnsupportedVersion {
				executor: self.executor.clone(),
				found,
				supported: self.cfg.versions,
			});
		}
		self
			.send_bytes(Self::encode(&wire::OutNotification { method: wire::INITIALIZED }))
			.await?;

		let reviewer = WirePolicy::REVIEWER;
		let outcome: Result<wire::ThreadOpened, i64> = match &request.resume {
			Some(wanted) => {
				let params = wire::ThreadResumeParams {
					thread_id:          wanted.as_str(),
					cwd:                &cwd,
					approval_policy:    wire_policy.approval,
					approvals_reviewer: reviewer,
					sandbox:            wire_policy.sandbox,
					config:             wire_policy.config(),
					exclude_turns:      true,
				};
				self.call(wire::THREAD_RESUME, &params).await?
			},
			None => {
				let params = wire::ThreadStartParams {
					cwd:                &cwd,
					approval_policy:    wire_policy.approval,
					approvals_reviewer: reviewer,
					sandbox:            wire_policy.sandbox,
					config:             wire_policy.config(),
				};
				self.call(wire::THREAD_START, &params).await?
			},
		};
		let opened = match (outcome, &request.resume) {
			(Ok(opened), _) => opened,
			(Err(_), Some(wanted)) => {
				return Err(ExecutorError::ResumeFailed {
					executor:  self.executor.clone(),
					requested: wanted.clone(),
				});
			},
			(Err(code), None) => {
				return Err(self.protocol(ProtocolViolation::RequestRejected { code }));
			},
		};
		let applied = applied_policy(&opened, &cwd).ok_or_else(|| {
			ExecutorError::PolicyUnmappable { executor: self.executor.clone(), requested }
		})?;

		self.wire = Some(wire_policy);
		self.workdir = cwd;
		self.thread = opened.thread.id.clone();
		Ok(SessionInfo {
			session: CliSessionId::new(opened.thread.id),
			resumed: request.resume.is_some(),
			workdir: PathBuf::from(opened.cwd.as_str()),
			policy:  applied,
		})
	}

	// -----------------------------------------------------------------------
	// Inbound
	// -----------------------------------------------------------------------

	async fn read_line(&mut self) -> Result<Bytes, ExecutorError> {
		match self.reader.next_line().await {
			Ok(Some(line)) => {
				self.frames += 1;
				Ok(line)
			},
			Ok(None) | Err(FrameError::Truncated | FrameError::Io(_)) => Err(self.crashed().await),
			Err(FrameError::Oversize { limit }) => {
				self.frames += 1;
				Err(self.malformed(FrameFault::Oversize { limit }))
			},
		}
	}

	fn decode<T: DeserializeOwned>(
		&self,
		params: Option<&RawValue>,
		field: &'static str,
	) -> Result<T, ExecutorError> {
		params
			.and_then(|raw| serde_json::from_str(raw.get()).ok())
			.ok_or_else(|| self.malformed(FrameFault::MissingField { field }))
	}

	/// Decodes and handles one inbound line. Responses are returned; requests
	/// and notifications are handled here.
	async fn route(&mut self, line: &[u8]) -> Result<Option<Reply>, ExecutorError> {
		let Ok(text) = str::from_utf8(line) else {
			return Err(self.malformed(FrameFault::NotJson));
		};
		let envelope: Envelope<'_> =
			serde_json::from_str(text).map_err(|_| self.malformed(FrameFault::NotJson))?;
		match envelope.classify() {
			None => Err(self.malformed(FrameFault::UnknownMessage)),
			Some(JsonRpcMessage::Response { id, result }) => {
				Ok(Some(Reply { id, outcome: Ok(result.to_owned()) }))
			},
			Some(JsonRpcMessage::Error { id, error }) => {
				Ok(Some(Reply { id, outcome: Err(error.code) }))
			},
			Some(JsonRpcMessage::Notification { method, params }) => {
				self.on_notification(&method, params)?;
				Ok(None)
			},
			Some(JsonRpcMessage::Request { id, method, params }) => {
				self.on_request(id, &method, params).await?;
				Ok(None)
			},
		}
	}

	fn on_notification(
		&mut self,
		method: &str,
		params: Option<&RawValue>,
	) -> Result<(), ExecutorError> {
		match Method::parse(method) {
			Method::TurnStarted => {
				let params: wire::TurnStartedParams = self.decode(params, "turn/started.params")?;
				self
					.queue
					.push_back(DelegatedEvent::Lifecycle(LifecycleEvent::TurnStarted {
						turn: TurnId::new(params.turn.id),
					}));
			},
			Method::TurnCompleted => {
				let params: wire::TurnCompletedParams = self.decode(params, "turn/completed.params")?;
				self.on_turn_completed(params.turn)?;
			},
			Method::AgentMessageDelta => {
				let params: wire::AgentDeltaParams =
					self.decode(params, "item/agentMessage/delta.params")?;
				if !self.streamed.contains(&params.item_id) {
					self.streamed.push(params.item_id);
				}
				self.queue.push_back(DelegatedEvent::AgentTextDelta {
					turn: TurnId::new(params.turn_id),
					text: params.delta,
				});
			},
			Method::ItemStarted => {
				let params: wire::ItemParams = self.decode(params, "item/started.params")?;
				self.on_item(params, false);
			},
			Method::ItemCompleted => {
				let params: wire::ItemParams = self.decode(params, "item/completed.params")?;
				self.on_item(params, true);
			},
			Method::TokenUsageUpdated => {
				let params: wire::TokenUsageParams =
					self.decode(params, "thread/tokenUsage/updated.params")?;
				self.on_usage(params);
			},
			Method::Error => {
				let params: wire::ErrorParams = self.decode(params, "error.params")?;
				if !params.will_retry {
					self.last_error = Some((params.turn_id, params.error.codex_error_info));
				}
			},
			Method::ThreadClosed => {
				let params: wire::ThreadClosedParams = self.decode(params, "thread/closed.params")?;
				if params.thread_id == self.thread {
					self
						.queue
						.push_back(DelegatedEvent::Lifecycle(LifecycleEvent::SessionClosed {
							session: CliSessionId::new(self.thread.clone()),
						}));
				}
			},
			Method::ServerRequestResolved => {
				let params: wire::RequestResolvedParams =
					self.decode(params, "serverRequest/resolved.params")?;
				for pending in &mut self.pending {
					if pending.id == params.request_id {
						pending.resolved = true;
					}
				}
			},
			Method::Inert => {},
			// A request method arriving without an id, or a method of another
			// protocol version: outside the vocabulary this executor advertised.
			Method::CommandApproval
			| Method::FileChangeApproval
			| Method::PermissionsApproval
			| Method::UserInput
			| Method::Elicitation
			| Method::DynamicToolCall
			| Method::LegacyPatchApproval
			| Method::LegacyExecApproval
			| Method::CredentialRequest
			| Method::Unknown => {
				tracing::warn!(method, "codex app-server sent an unknown notification");
				return Err(ExecutorError::UndeclaredEvent {
					executor: self.executor.clone(),
					feature:  Feature::ToolEvents,
				});
			},
		}
		Ok(())
	}

	fn on_turn_completed(&mut self, done: wire::TurnDone) -> Result<(), ExecutorError> {
		let turn = TurnId::new(done.id.clone());
		self.turn = None;
		self.pending.clear();
		let event = match done.status {
			wire::TurnStatus::Completed => DelegatedEvent::Lifecycle(LifecycleEvent::TurnCompleted {
				turn,
				outcome: TurnEnd::Completed,
			}),
			wire::TurnStatus::Interrupted => {
				DelegatedEvent::Lifecycle(LifecycleEvent::TurnCompleted {
					turn,
					outcome: TurnEnd::Interrupted,
				})
			},
			wire::TurnStatus::Failed => {
				let stored = match self.last_error.take() {
					Some((id, info)) if id == done.id => info,
					_ => None,
				};
				let info = done
					.error
					.and_then(|error| error.codex_error_info)
					.or(stored);
				DelegatedEvent::Failure { turn, failure: remote_failure(info) }
			},
			wire::TurnStatus::InProgress | wire::TurnStatus::Unknown => {
				return Err(self.malformed(FrameFault::UnknownMessage));
			},
		};
		self.queue.push_back(event);
		Ok(())
	}

	fn on_item(&mut self, params: wire::ItemParams, completed: bool) {
		let turn = TurnId::new(params.turn_id);
		let (id, kind, label, status) = match params.item {
			wire::ThreadItem::AgentMessage { id, text } => {
				if completed && !self.streamed.contains(&id) {
					self
						.queue
						.push_back(DelegatedEvent::AgentMessage { turn, text });
				}
				return;
			},
			wire::ThreadItem::CommandExecution { id, command, status } => {
				(id, ToolKind::Command, command, status)
			},
			wire::ThreadItem::FileChange { id, status } => {
				(id, ToolKind::FileChange, Str::new_static("file change"), status)
			},
			wire::ThreadItem::McpToolCall { id, tool, status }
			| wire::ThreadItem::DynamicToolCall { id, tool, status } => {
				(id, ToolKind::External, tool, status)
			},
			wire::ThreadItem::CollabAgentToolCall { id, tool, status } => {
				(id, ToolKind::Other, tool, status)
			},
			wire::ThreadItem::WebSearch { id, query } => {
				(id, ToolKind::Other, query, Some(wire::ItemStatus::Completed))
			},
			wire::ThreadItem::Other => return,
		};
		let call = CallId::new(id);
		let event = if completed {
			ToolEvent::Finished { turn, call, outcome: tool_outcome(status) }
		} else {
			ToolEvent::Started { turn, call, kind, label }
		};
		self.queue.push_back(DelegatedEvent::Tool(event));
	}

	fn on_usage(&mut self, params: wire::TokenUsageParams) {
		let total = params.token_usage.total;
		let base = *self
			.usage_base
			.get_or_insert_with(|| total.minus(params.token_usage.last));
		self.last_total = Some(total);
		self.queue.push_back(DelegatedEvent::Usage {
			turn:  TurnId::new(params.turn_id),
			usage: usage_of(total.minus(base)),
		});
	}

	async fn on_request(
		&mut self,
		id: RequestId,
		method: &str,
		params: Option<&RawValue>,
	) -> Result<(), ExecutorError> {
		match Method::parse(method) {
			Method::CommandApproval => {
				let params: wire::CommandApprovalParams =
					self.decode(params, "item/commandExecution/requestApproval.params")?;
				let (kind, subject) =
					match (&params.kind, params.network_approval_context, params.command) {
						(wire::CommandKind::Command, Some(network), _) => {
							(PermissionKind::Network, network.host)
						},
						(wire::CommandKind::Command, None, Some(command)) => {
							(PermissionKind::Exec, command)
						},
						// Terminal input and anything unclassifiable: the host refuses
						// `Unclassified` without asking anyone.
						_ => (PermissionKind::Unclassified, Str::empty()),
					};
				self.raise(id, PendingKind::Command, params.turn_id, params.item_id, kind, subject);
			},
			Method::FileChangeApproval => {
				let params: wire::FileApprovalParams =
					self.decode(params, "item/fileChange/requestApproval.params")?;
				let subject = params
					.grant_root
					.or(params.reason)
					.unwrap_or_else(|| Str::new_static("file change"));
				self.raise(
					id,
					PendingKind::FileChange,
					params.turn_id,
					params.item_id,
					PermissionKind::Write,
					subject,
				);
			},
			Method::PermissionsApproval => {
				let params: wire::PermissionsApprovalParams =
					self.decode(params, "item/permissions/requestApproval.params")?;
				let (kind, subject) = classify_profile(&params.permissions);
				self.raise(
					id,
					PendingKind::Permissions(params.permissions),
					params.turn_id,
					params.item_id,
					kind,
					subject,
				);
			},
			Method::LegacyPatchApproval | Method::LegacyExecApproval => {
				tracing::warn!(method, "legacy codex approval request denied");
				self
					.respond(&id, &wire::LegacyDenied { decision: wire::LegacyDecision::Denied })
					.await?;
			},
			Method::Elicitation => {
				tracing::warn!("codex mcp elicitation cancelled");
				self
					.respond(&id, &wire::ElicitationCancelled {
						action: wire::ElicitationAction::Cancel,
					})
					.await?;
			},
			Method::DynamicToolCall => {
				tracing::warn!("codex dynamic tool call refused");
				self
					.respond(&id, &wire::ToolCallRefused { content_items: [], success: false })
					.await?;
			},
			Method::UserInput | Method::CredentialRequest => {
				// Credentials are never supplied and no human is asked on the
				// CLI's behalf.
				tracing::warn!(method, "codex request refused");
				self.refuse(&id).await?;
			},
			_ => {
				tracing::warn!(method, "codex app-server sent an unknown request");
				self.refuse(&id).await?;
				return Err(ExecutorError::UndeclaredEvent {
					executor: self.executor.clone(),
					feature:  Feature::Permissions,
				});
			},
		}
		Ok(())
	}

	/// Files a permission prompt and queues the event for the host.
	fn raise(
		&mut self,
		id: RequestId,
		kind: PendingKind,
		turn: Str,
		item: Str,
		permission: PermissionKind,
		subject: Str,
	) {
		let prompt = match &id {
			RequestId::Int(number) => PromptId::new(sf!("{number}")),
			RequestId::Text(text) => PromptId::new(text.clone()),
		};
		self
			.pending
			.push(Pending { prompt: prompt.clone(), id, kind, resolved: false });
		self
			.queue
			.push_back(DelegatedEvent::Permission(PermissionRequest {
				prompt,
				turn: TurnId::new(turn),
				call: Some(CallId::new(item)),
				kind: permission,
				subject,
				// No answer this client sends outlives the action it was asked
				// about, so the host must never believe a grant is remembered.
				session_grant: false,
			}));
	}

	/// Reads and routes lines until the turn in flight completes.
	async fn drain_turn(&mut self) -> Result<(), ExecutorError> {
		while self.turn.is_some() {
			let line = self.read_line().await?;
			self.route(&line).await?;
		}
		Ok(())
	}

	async fn send_interrupt(&mut self, turn: &Str) -> Result<(), ExecutorError> {
		let params =
			wire::TurnInterruptParams { thread_id: self.thread.as_str(), turn_id: turn.as_str() };
		let (_, line) = self.request_line(wire::TURN_INTERRUPT, &params);
		self.send_bytes(line).await
	}
}

impl DelegatedSession for CodexSession {
	async fn start_turn<'a>(
		&'a mut self,
		input: &'a TurnInput,
	) -> Result<TurnAccepted, ExecutorError> {
		let Some(wire_policy) = self.wire else {
			return Err(ExecutorError::SessionClosed { executor: self.executor.clone() });
		};
		self.pending.clear();
		self.streamed.clear();
		self.last_error = None;
		self.usage_base = self.last_total;

		let sections = input.prompt.sections();
		let (system, user): (Vec<&PromptSection>, Vec<&PromptSection>) = sections
			.iter()
			.partition(|section| section.role == PromptRole::System);
		// Delivered bytes, decoded back out of the encoded lines and listed in
		// the order the CLI receives them: system text first, then user text.
		let mut delivered: Vec<PromptSection> = Vec::with_capacity(sections.len());

		if !system.is_empty() {
			let params = wire::InjectItemsParams {
				thread_id: self.thread.as_str(),
				items:     system
					.iter()
					.map(|section| wire::InjectedMessage {
						kind:    wire::MessageKind::Message,
						role:    wire::DeveloperRole::Developer,
						content: [wire::InjectedText {
							kind: wire::InputTextKind::InputText,
							text: section.text.clone(),
						}],
					})
					.collect(),
			};
			let (id, line) = self.request_line(wire::THREAD_INJECT_ITEMS, &params);
			let sent: wire::Sent<wire::SentInject> =
				serde_json::from_slice(&line).map_err(|_| self.malformed(FrameFault::NotJson))?;
			for message in sent.params.items {
				for part in message.content {
					delivered.push(PromptSection { role: PromptRole::System, text: part.text });
				}
			}
			self.send_bytes(line).await?;
			if let Err(code) = self.await_response::<IgnoredAny>(id).await? {
				return Err(self.protocol(ProtocolViolation::RequestRejected { code }));
			}
		}

		let params = wire::TurnStartParams {
			thread_id:       self.thread.as_str(),
			input:           user
				.iter()
				.map(|section| wire::TextInput {
					kind: wire::TextKind::Text,
					text: section.text.clone(),
				})
				.collect(),
			cwd:             &self.workdir,
			approval_policy: wire_policy.approval,
			sandbox_policy:  wire_policy.turn_sandbox(),
		};
		let (id, line) = self.request_line(wire::TURN_START, &params);
		let sent: wire::Sent<wire::SentTurn> =
			serde_json::from_slice(&line).map_err(|_| self.malformed(FrameFault::NotJson))?;
		for input in sent.params.input {
			delivered.push(PromptSection { role: PromptRole::User, text: input.text });
		}
		let forwarded = DelegatedPrompt::digest_of(&delivered, input.prompt.bands());

		self.send_bytes(line).await?;
		let started: wire::TurnStartResult = match self.await_response(id).await? {
			Ok(started) => started,
			Err(code) => return Err(self.protocol(ProtocolViolation::RequestRejected { code })),
		};
		self.turn = Some(started.turn.id.clone());
		Ok(TurnAccepted { turn: TurnId::new(started.turn.id), forwarded })
	}

	async fn next_event(&mut self) -> Result<DelegatedEvent, ExecutorError> {
		loop {
			if let Some(event) = self.queue.pop_front() {
				return Ok(event);
			}
			let line = self.read_line().await?;
			self.route(&line).await?;
		}
	}

	async fn answer<'a>(
		&'a mut self,
		prompt: &'a PromptId,
		answer: PermissionAnswer,
	) -> Result<(), ExecutorError> {
		let Some(at) = self
			.pending
			.iter()
			.position(|pending| pending.prompt == *prompt)
		else {
			return Err(self.protocol(ProtocolViolation::UnknownPrompt));
		};
		let pending = self.pending.swap_remove(at);
		if pending.resolved {
			return Ok(());
		}
		match pending.kind {
			PendingKind::Command | PendingKind::FileChange => {
				// `acceptForSession`, execpolicy and network-policy amendments
				// would outlive the action, so an allow is always a single accept.
				let decision = match answer {
					PermissionAnswer::Deny => wire::ApprovalDecision::Decline,
					PermissionAnswer::AllowOnce | PermissionAnswer::AllowForSession => {
						wire::ApprovalDecision::Accept
					},
				};
				self
					.respond(&pending.id, &wire::DecisionResponse { decision })
					.await
			},
			PendingKind::Permissions(requested) => {
				let permissions = match answer {
					PermissionAnswer::Deny => wire::PermissionProfile::default(),
					PermissionAnswer::AllowOnce | PermissionAnswer::AllowForSession => requested,
				};
				self
					.respond(&pending.id, &wire::PermissionsResponse {
						permissions,
						scope: wire::GrantScope::Turn,
					})
					.await
			},
		}
	}

	async fn interrupt(&mut self) -> Result<(), ExecutorError> {
		match self.turn.clone() {
			Some(turn) => self.send_interrupt(&turn).await,
			None => Ok(()),
		}
	}

	async fn terminate(&mut self) -> Result<(), ExecutorError> {
		if self.terminated {
			return Ok(());
		}
		self.terminated = true;
		if let Some(turn) = self.turn.clone()
			&& self.send_interrupt(&turn).await.is_ok()
		{
			let grace = self.cfg.terminate_grace;
			let _ = timeout(grace, self.drain_turn()).await;
		}
		for task in &self.tasks {
			task.abort();
		}
		match self.tree.as_mut() {
			Some(tree) if !tree.terminate_tree().await => Err(ExecutorError::TeardownFailed {
				executor: self.executor.clone(),
				fault:    TeardownFault::Unreaped,
			}),
			_ => Ok(()),
		}
	}
}

impl Drop for CodexSession {
	fn drop(&mut self) {
		for task in &self.tasks {
			task.abort();
		}
		// `ChildTree::drop` kills the whole process group.
	}
}

/// Drains `queue` into `writer` until either side closes.
async fn write_loop(mut writer: BoxWrite, queue: flume::Receiver<Bytes>) {
	while let Ok(line) = queue.recv_async().await {
		if writer.write_all(&line).await.is_err() || writer.flush().await.is_err() {
			break;
		}
	}
	let _ = writer.shutdown().await;
}

fn exit_fact(status: std::process::ExitStatus) -> ExitFact {
	#[cfg(unix)]
	{
		use std::os::unix::process::ExitStatusExt as _;
		if let Some(signal) = status.signal() {
			return ExitFact::Signal(signal);
		}
	}
	status.code().map_or(ExitFact::Unknown, ExitFact::Code)
}

fn remote_failure(info: Option<wire::ErrorInfo>) -> RemoteFailure {
	use wire::ErrorCode;
	let Some(wire::ErrorInfo::Code(code)) = info else {
		return RemoteFailure::Internal;
	};
	match code {
		ErrorCode::Unauthorized => RemoteFailure::AuthRequired,
		ErrorCode::RateLimitExceeded
		| ErrorCode::UsageLimitExceeded
		| ErrorCode::ServerOverloaded => RemoteFailure::RateLimited,
		ErrorCode::ContextWindowExceeded => RemoteFailure::ContextOverflow,
		ErrorCode::CyberPolicy
		| ErrorCode::MisalignmentPolicyViolation
		| ErrorCode::BadRequest
		| ErrorCode::TooManyDenials
		| ErrorCode::SessionBudgetExceeded => RemoteFailure::Refused,
		ErrorCode::Other => RemoteFailure::Internal,
	}
}

/// A completed item whose status is not a clear success is a failure.
const fn tool_outcome(status: Option<wire::ItemStatus>) -> ToolOutcome {
	match status {
		Some(wire::ItemStatus::Completed) => ToolOutcome::Succeeded,
		Some(wire::ItemStatus::Declined) => ToolOutcome::Denied,
		Some(wire::ItemStatus::Interrupted) => ToolOutcome::Interrupted,
		Some(wire::ItemStatus::Failed | wire::ItemStatus::InProgress | wire::ItemStatus::Unknown)
		| None => ToolOutcome::Failed,
	}
}

fn usage_of(tokens: wire::TokenBreakdown) -> Usage {
	Usage {
		input_tokens: tokens
			.input_tokens
			.saturating_sub(tokens.cached_input_tokens),
		output_tokens: tokens.output_tokens,
		reasoning_tokens: tokens.reasoning_output_tokens,
		cache_read_tokens: tokens.cached_input_tokens,
		cache_write_tokens: tokens.cache_write_input_tokens,
		source: UsageSource::Provider,
		..Usage::default()
	}
}

/// One kind and subject for a permission profile. Anything that mixes effects,
/// is empty, or uses structured entries the answer could not echo faithfully
/// is [`PermissionKind::Unclassified`], which the host never grants.
fn classify_profile(profile: &wire::PermissionProfile) -> (PermissionKind, Str) {
	let fs = profile.file_system.as_ref();
	if fs.is_some_and(|fs| fs.entries.is_some() || fs.glob_scan_max_depth.is_some()) {
		return (PermissionKind::Unclassified, Str::empty());
	}
	let network = profile
		.network
		.as_ref()
		.is_some_and(|network| network.enabled == Some(true));
	let writes = fs
		.and_then(|fs| fs.write.as_deref())
		.filter(|paths| !paths.is_empty());
	let reads = fs
		.and_then(|fs| fs.read.as_deref())
		.filter(|paths| !paths.is_empty());
	match (network, writes, reads) {
		(true, None, None) => (PermissionKind::Network, Str::new_static("network")),
		(false, Some(paths), _) => (PermissionKind::Write, join(paths)),
		(false, None, Some(paths)) => (PermissionKind::Read, join(paths)),
		_ => (PermissionKind::Unclassified, Str::empty()),
	}
}

fn join(paths: &[Str]) -> Str {
	let mut out = String::new();
	for (index, path) in paths.iter().enumerate() {
		if index > 0 {
			out.push_str(", ");
		}
		out.push_str(path);
	}
	Str::from(out)
}

/// Parses the version after the first `/` of a user agent such as
/// `codex_cli_rs/0.160.0 (…)`. `None` when the shape is not recognised; the
/// host's `--version` probe is the authority, this is a second check.
fn user_agent_version(user_agent: &str) -> Option<CliVersion> {
	let (_, rest) = user_agent.split_once('/')?;
	parse_version(
		rest
			.split(|c: char| !(c.is_ascii_digit() || c == '.'))
			.next()?,
	)
}

/// Parses `major.minor.patch`; anything else is `None`.
pub(crate) fn parse_version(text: &str) -> Option<CliVersion> {
	let mut parts = text.split('.');
	let version = CliVersion::new(
		parts.next()?.parse().ok()?,
		parts.next()?.parse().ok()?,
		parts.next()?.parse().ok()?,
	);
	parts.next().is_none().then_some(version)
}
