//! [`AcpDelegatedSession`]: one live ACP agent session implementing
//! [`DelegatedSession`].
//!
//! The session drives the ACP v1 handshake, turn lifecycle, event
//! normalization, permission routing, interrupt, and orderly termination.
//! Transport is supplied by the caller; in production the executor hands it
//! the stdio pipes of a spawned child.

use std::collections::HashMap;

use omp_agent::delegated::{
	CallId, CliSessionId, DelegatedEvent, DelegatedPrompt, DelegatedSession, ExecutorError,
	ExitFact, FrameFault, LifecycleEvent, PermissionAnswer, PermissionKind, PermissionRequest,
	PromptId, ProtocolViolation, SessionInfo, SessionRequest, ToolEvent, ToolKind, ToolOutcome,
	TurnAccepted, TurnEnd, TurnId, TurnInput,
};
use omp_core::Str;
use serde::de::{DeserializeOwned, IgnoredAny};
use tokio::{
	io::{AsyncRead, AsyncWrite},
	time::timeout,
};

use super::{
	AcpSession, InboundMessage,
	policy::ACP_POLICY,
	session::SessionError,
	wire::{
		self, CancelParams, LoadSessionParams, Method, NewSessionParams, PermissionOptionKind,
		PermissionOutcome, PromptParams, RequestPermissionResponse, TextContentBlock,
	},
};
use crate::child_tree::ChildTree;

/// A vendor notification that reports a missing login with a typed cause, such
/// as Devin's `_cognition.ai/agent_stopped` with `cause: "auth_required"`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LoginNotice {
	/// The notification method name, exactly as the agent sends it.
	pub method: Str,
	/// The `cause` value that means the login is missing.
	pub cause:  Str,
}

/// What one CLI needs on top of the generic ACP client.
///
/// ACP has no sandbox parameter, so a profile asks for session setup that makes
/// the agent enforce a policy itself, and names the vendor signals the client
/// may trust. It never carries a credential.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct AcpProfile {
	/// Sent as `session/set_mode` right after the session opens. An agent that
	/// rejects it cannot enforce the policy, so the open fails.
	pub session_mode:   Option<Str>,
	/// Sent as `session/set_config_option`, in order, after the session opens.
	pub config_options: Vec<(Str, Str)>,
	/// The vendor's typed missing-login notification, when it has one.
	pub login_notice:   Option<LoginNotice>,
}

/// Configuration for an [`AcpDelegatedSession`].
#[derive(Clone)]
pub struct AcpSessionConfig {
	/// How long the `initialize` + `session/new` exchange may take.
	pub handshake_timeout: std::time::Duration,
	/// How long `terminate` waits for a cancelled turn to settle before
	/// killing the tree.
	pub terminate_grace:   std::time::Duration,
	/// Maximum inbound frame size.
	pub max_frame_bytes:   usize,
	/// What the CLI behind this session needs.
	pub profile:           AcpProfile,
}

impl Default for AcpSessionConfig {
	fn default() -> Self {
		Self {
			handshake_timeout: std::time::Duration::from_secs(30),
			terminate_grace:   std::time::Duration::from_secs(2),
			max_frame_bytes:   super::MAX_FRAME_BYTES,
			profile:           AcpProfile::default(),
		}
	}
}

/// A pending `session/request_permission` from the agent.
struct PendingPermission {
	/// Original JSON-RPC request id, needed to send the response.
	rpc_id:  wire::RequestId,
	/// The option ids available, so the answer can be validated.
	options: Vec<(Str, PermissionOptionKind)>,
}

/// A live ACP session implementing [`DelegatedSession`].
pub struct AcpDelegatedSession {
	executor:       omp_agent::delegated::ExecutorId,
	session_id:     Str,
	/// The underlying transport.
	inner:          AcpSession<Box<dyn AsyncRead + Unpin + Send>>,
	/// The owned child process tree; absent for in-memory transports.
	tree:           Option<ChildTree>,
	cfg:            AcpSessionConfig,
	/// Turn currently in flight (session/prompt sent; not yet completed).
	turn_live:      bool,
	/// Buffered events not yet consumed by `next_event`.
	queue:          std::collections::VecDeque<DelegatedEvent>,
	/// Active permission prompts.
	pending:        HashMap<PromptId, PendingPermission>,
	/// The turn in flight, named by the request id of its `session/prompt`. ACP
	/// has no turn id of its own, and the host requires every event of a turn
	/// to carry the id `start_turn` returned, so each event is stamped with it.
	turn:           TurnId,
	terminated:     bool,
	/// A profile's typed missing-login notice arrived during this turn.
	login_required: bool,
}

impl std::fmt::Debug for AcpDelegatedSession {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("AcpDelegatedSession")
			.field("executor", &self.executor)
			.field("session_id", &self.session_id)
			.field("turn_live", &self.turn_live)
			.field("terminated", &self.terminated)
			.finish_non_exhaustive()
	}
}

impl AcpDelegatedSession {
	/// Drives the ACP handshake (`initialize` → `session/new` or
	/// `session/load`) and returns a ready session plus its `SessionInfo`.
	///
	/// `writer` is a separate object because [`AcpSession`] takes `R:
	/// AsyncRead`; we box both here.
	pub async fn handshake<R, W>(
		executor_id: omp_agent::delegated::ExecutorId,
		reader: R,
		writer: W,
		tree: Option<ChildTree>,
		request: &SessionRequest,
		cfg: AcpSessionConfig,
	) -> Result<(Self, SessionInfo), ExecutorError>
	where
		R: AsyncRead + Unpin + Send + 'static,
		W: AsyncWrite + Unpin + Send + 'static,
	{
		if !request.workdir.is_absolute() {
			return Err(ExecutorError::WorkdirNotAbsolute { path: request.workdir.clone() });
		}
		let Some(cwd) = request.workdir.to_str() else {
			return Err(ExecutorError::WorkdirNotUtf8 { path: request.workdir.clone() });
		};

		// Policy check: ACP can only guarantee STRICTEST.
		if request.policy != ACP_POLICY {
			return Err(ExecutorError::PolicyUnmappable {
				executor:  executor_id.clone(),
				requested: request.policy,
			});
		}

		let boxed_reader: Box<dyn AsyncRead + Unpin + Send> = Box::new(reader);
		let mut session = AcpSession::new(boxed_reader, writer, cfg.max_frame_bytes);

		// ── initialize ──────────────────────────────────────────────────────
		let init_id = session
			.send_request("initialize", wire::InitializeParams {
				protocol_version:    wire::PROTOCOL_VERSION,
				client_info:         Some(wire::Implementation {
					name:    "omp",
					version: Some(env!("CARGO_PKG_VERSION")),
				}),
				client_capabilities: wire::ClientCapabilities {},
			})
			.map_err(|error| session_error(&executor_id, error))?;

		let init_result: wire::InitializeResult =
			await_response(&mut session, &executor_id, init_id, "initialize", cfg.handshake_timeout)
				.await?;

		let load_session = init_result.agent_capabilities.load_session;

		// ── session/new or session/load ──────────────────────────────────────
		let session_id: Str = if let Some(resume_id) = &request.resume {
			if !load_session {
				// Agent does not support session/load; resume requested but
				// unavailable → ResumeFailed.
				return Err(ExecutorError::ResumeFailed {
					executor:  executor_id,
					requested: resume_id.clone(),
				});
			}
			let id = session
				.send_request("session/load", LoadSessionParams { session_id: resume_id.as_str() })
				.map_err(|error| session_error(&executor_id, error))?;
			let result: wire::LoadSessionResult =
				await_response(&mut session, &executor_id, id, "session/load", cfg.handshake_timeout)
					.await?;
			result.session_id
		} else {
			let id = session
				.send_request("session/new", NewSessionParams { cwd, mcp_servers: &[] })
				.map_err(|error| session_error(&executor_id, error))?;
			let result: wire::NewSessionResult =
				await_response(&mut session, &executor_id, id, "session/new", cfg.handshake_timeout)
					.await?;
			result.session_id
		};

		// ── session setup the profile asks for ───────────────────────────────
		if let Some(mode) = &cfg.profile.session_mode {
			let id = session
				.send_request("session/set_mode", wire::SetModeParams {
					session_id: session_id.as_str(),
					mode_id:    mode.as_str(),
				})
				.map_err(|error| session_error(&executor_id, error))?;
			match await_response::<_, IgnoredAny>(
				&mut session,
				&executor_id,
				id,
				"session/set_mode",
				cfg.handshake_timeout,
			)
			.await
			{
				Ok(_) => {},
				// An agent that refuses the mode cannot enforce what was asked,
				// so the session must not open.
				Err(ExecutorError::Protocol {
					violation: ProtocolViolation::RequestRejected { .. },
					..
				}) => {
					return Err(ExecutorError::PolicyUnmappable {
						executor:  executor_id,
						requested: request.policy,
					});
				},
				Err(other) => return Err(other),
			}
		}
		for (config_id, value) in &cfg.profile.config_options {
			let id = session
				.send_request("session/set_config_option", wire::SetConfigOptionParams {
					session_id: session_id.as_str(),
					config_id:  config_id.as_str(),
					value:      value.as_str(),
				})
				.map_err(|error| session_error(&executor_id, error))?;
			let IgnoredAny = await_response(
				&mut session,
				&executor_id,
				id,
				"session/set_config_option",
				cfg.handshake_timeout,
			)
			.await?;
		}

		let resumed = request.resume.is_some();
		let info = SessionInfo {
			session: CliSessionId::new(session_id.clone()),
			resumed,
			workdir: request.workdir.clone(),
			policy: ACP_POLICY,
		};

		Ok((
			Self {
				executor: executor_id,
				session_id,
				inner: session,
				tree,
				cfg,
				turn_live: false,
				queue: std::collections::VecDeque::new(),
				pending: HashMap::new(),
				terminated: false,
				turn: TurnId::new(Str::default()),
				login_required: false,
			},
			info,
		))
	}

	fn make_crashed(&self) -> ExecutorError {
		ExecutorError::Crashed { executor: self.executor.clone(), exit: ExitFact::Unknown }
	}

	fn malformed(&self, fault: FrameFault) -> ExecutorError {
		ExecutorError::MalformedStream { executor: self.executor.clone(), frame: 0, fault }
	}

	/// Reads and dispatches one inbound message. Returns a queued event if one
	/// was produced, or `None` when the message required no event (e.g. an
	/// unknown notification).
	async fn read_one(&mut self) -> Result<(), ExecutorError> {
		let msg = match self.inner.next_message().await {
			Ok(Some(msg)) => msg,
			Ok(None) => return Err(self.make_crashed()),
			Err(SessionError::Frame(crate::line_reader::FrameError::Oversize { limit })) => {
				return Err(self.malformed(FrameFault::Oversize { limit }));
			},
			Err(SessionError::Frame(_)) => return Err(self.make_crashed()),
			Err(SessionError::Serialize(_)) => {
				return Err(self.malformed(FrameFault::NotJson));
			},
		};
		self.dispatch(msg)?;
		Ok(())
	}

	/// Classifies an inbound message and appends events to `self.queue`.
	fn dispatch(&mut self, msg: InboundMessage) -> Result<(), ExecutorError> {
		match msg {
			InboundMessage::Notification { method: Method::SessionUpdate, params, .. } => {
				let Some(raw) = params else {
					return Err(
						self.malformed(FrameFault::MissingField { field: "session/update.params" }),
					);
				};
				let update: wire::UpdateParams = serde_json::from_str(raw.get()).map_err(|_| {
					self.malformed(FrameFault::MissingField { field: "session/update.params" })
				})?;
				self.on_update(update);
			},
			InboundMessage::Malformed { .. } => {
				return Err(self.malformed(FrameFault::NotJson));
			},
			InboundMessage::Notification { name, params, .. } => {
				// A profile's typed missing-login notice. Every other
				// notification is forward-compat noise and is ignored.
				if let Some(notice) = &self.cfg.profile.login_notice
					&& name == notice.method
					&& let Some(raw) = params
					&& let Ok(stopped) = serde_json::from_str::<wire::AgentStoppedParams>(raw.get())
					&& stopped.cause.as_ref() == Some(&notice.cause)
				{
					self.login_required = true;
				}
			},
			InboundMessage::AgentRequest { id, method: Method::SessionRequestPermission, params } => {
				let Some(raw) = params else {
					// Bad permission request: refuse it.
					let _ = self.inner.answer_error(
						&id,
						wire::ERR_REQUEST_CANCELLED,
						Str::new_static("missing params"),
					);
					return Ok(());
				};
				let params: wire::RequestPermissionParams =
					serde_json::from_str(raw.get()).map_err(|_| {
						self.malformed(FrameFault::MissingField {
							field: "session/request_permission.params",
						})
					})?;
				self.on_permission(id, params);
			},
			InboundMessage::AgentRequest { id, .. } => {
				// Refuse unrecognized agent requests.
				let _ = self.inner.answer_error(
					&id,
					-32601,
					Str::new_static("method not supported by this client"),
				);
			},
			InboundMessage::Response { .. } | InboundMessage::ErrorResponse { .. } => {
				// Stray responses after the handshake; ignore.
			},
		}
		Ok(())
	}

	/// Maps a `session/update` notification to zero or more queued events.
	fn on_update(&mut self, update: wire::UpdateParams) {
		let turn = self.turn.clone();
		match update.update {
			wire::SessionUpdateEnvelope::AgentMessageChunk { ref content, .. } => {
				if let Some(text) = &content.text {
					self
						.queue
						.push_back(DelegatedEvent::AgentTextDelta { turn, text: text.clone() });
				}
			},
			wire::SessionUpdateEnvelope::ToolCall { tool_call_id, title, status } => {
				let label = title.unwrap_or_else(|| Str::new_static("tool"));
				let outcome = tool_status_to_outcome(status);
				if outcome.is_some() {
					// Started and immediately finished (unusual but possible).
					self
						.queue
						.push_back(DelegatedEvent::Tool(ToolEvent::Started {
							turn: turn.clone(),
							call: CallId::new(tool_call_id.clone()),
							kind: ToolKind::External,
							label,
						}));
					self
						.queue
						.push_back(DelegatedEvent::Tool(ToolEvent::Finished {
							turn,
							call: CallId::new(tool_call_id),
							outcome: outcome.unwrap_or(ToolOutcome::Succeeded),
						}));
				} else {
					self
						.queue
						.push_back(DelegatedEvent::Tool(ToolEvent::Started {
							turn,
							call: CallId::new(tool_call_id),
							kind: ToolKind::External,
							label,
						}));
				}
			},
			wire::SessionUpdateEnvelope::ToolCallUpdate { tool_call_id, status, .. } => {
				if let Some(outcome) = status.and_then(tool_status_to_outcome) {
					self
						.queue
						.push_back(DelegatedEvent::Tool(ToolEvent::Finished {
							turn,
							call: CallId::new(tool_call_id),
							outcome,
						}));
				}
			},
			// Plan, usage, notices, compaction — noted but not emitted as events.
			wire::SessionUpdateEnvelope::UsageUpdate { .. }
			| wire::SessionUpdateEnvelope::Plan { .. }
			| wire::SessionUpdateEnvelope::Notice { .. }
			| wire::SessionUpdateEnvelope::AvailableCommandsUpdate { .. }
			| wire::SessionUpdateEnvelope::CurrentModeUpdate { .. }
			| wire::SessionUpdateEnvelope::ConfigOptionUpdate { .. }
			| wire::SessionUpdateEnvelope::SessionInfoUpdate { .. }
			| wire::SessionUpdateEnvelope::CompactionUpdate { .. }
			| wire::SessionUpdateEnvelope::CompactionSummaryChunk { .. }
			| wire::SessionUpdateEnvelope::AgentThoughtChunk { .. }
			| wire::SessionUpdateEnvelope::UserMessageChunk { .. }
			| wire::SessionUpdateEnvelope::Unknown => {},
		}
	}

	/// Queues a `PermissionRequest` from an incoming
	/// `session/request_permission`.
	fn on_permission(&mut self, id: wire::RequestId, params: wire::RequestPermissionParams) {
		// Extract the most useful kind from the options offered.
		let kind = classify_permission_options(&params.options);
		let subject = params
			.tool_call
			.title
			.clone()
			.unwrap_or_else(|| Str::new_static("tool action"));
		let prompt = PromptId::new(params.tool_call.tool_call_id.clone());

		let options: Vec<(Str, PermissionOptionKind)> = params
			.options
			.iter()
			.map(|opt| (opt.option_id.clone(), opt.kind))
			.collect();

		self
			.pending
			.insert(prompt.clone(), PendingPermission { rpc_id: id, options });

		// A permission request belongs to the turn in flight; ACP does not carry
		// a turn id in it, so it takes the id `start_turn` returned.
		let turn = self.turn.clone();
		let call = Some(CallId::new(params.tool_call.tool_call_id));

		self
			.queue
			.push_back(DelegatedEvent::Permission(PermissionRequest {
				prompt,
				turn,
				call,
				kind,
				subject,
				session_grant: false,
			}));
	}

	/// Sends `session/cancel` and drains until the `session/prompt` response
	/// arrives (signalling the turn ended), or until timeout.
	async fn drain_after_cancel(&mut self) -> Result<(), ExecutorError> {
		let _ = self
			.inner
			.send_notification("session/cancel", CancelParams {
				session_id: self.session_id.as_str(),
			});
		// Wait briefly for the turn to complete naturally.
		let grace = self.cfg.terminate_grace;
		let _ = timeout(grace, async { while self.read_one().await.is_ok() {} }).await;
		Ok(())
	}
}

#[allow(
	unknown_lints,
	reason = "unused_async_trait_impl is not available on every supported nightly"
)]
#[allow(
	clippy::unused_async_trait_impl,
	reason = "start_turn, answer and interrupt only enqueue frames; DelegatedSession requires \
	          async methods"
)]
impl DelegatedSession for AcpDelegatedSession {
	async fn start_turn<'a>(
		&'a mut self,
		input: &'a TurnInput,
	) -> Result<TurnAccepted, ExecutorError> {
		self.queue.clear();
		self.pending.clear();
		self.login_required = false;

		// ACP's `session/prompt` has no system field, so every section is delivered
		// verbatim as its own `text` block, in order. The digest is computed over
		// the very sections delivered, so it equals `DelegatedPrompt::digest` by
		// construction; merging sections would change the bytes and end every turn
		// with `PromptRewritten`. The system/user split cannot be expressed on the
		// wire: the agent sees an ordered list of text blocks.
		let sections = input.prompt.sections();
		let id = {
			let blocks: Vec<TextContentBlock<'_>> = sections
				.iter()
				.map(|section| TextContentBlock::text(section.text.as_str()))
				.collect();
			self
				.inner
				.send_request("session/prompt", PromptParams {
					session_id: self.session_id.as_str(),
					prompt:     blocks,
				})
				.map_err(|error| session_error(&self.executor, error))?
		};
		let forwarded = DelegatedPrompt::digest_of(sections, input.prompt.bands());

		self.turn_live = true;

		// Record turn id: ACP doesn't provide a separate turn id; we use the
		// RPC request id as the turn handle.
		let turn = TurnId::new(Str::from(id.to_string()));
		self.turn = turn.clone();

		// The session/prompt response (stop_reason) marks the end of the turn,
		// but events arrive as notifications beforehand. We enqueue
		// TurnStarted immediately so the host has a consistent lifecycle.
		self
			.queue
			.push_back(DelegatedEvent::Lifecycle(LifecycleEvent::TurnStarted { turn: turn.clone() }));

		// Park the rpc id so next_event knows when the turn ends.
		// We store it in a special pending entry with a sentinel key.
		self.pending.insert(
			PromptId::new(Str::from(format!("__prompt_rpc_{id}"))),
			PendingPermission {
				rpc_id:  wire::RequestId::Int(id),
				options: vec![(Str::new_static("__end"), PermissionOptionKind::RejectOnce)],
			},
		);

		Ok(TurnAccepted { turn, forwarded })
	}

	async fn next_event(&mut self) -> Result<DelegatedEvent, ExecutorError> {
		loop {
			if let Some(event) = self.queue.pop_front() {
				return Ok(event);
			}
			// Check if there's an in-flight prompt RPC to drain.
			let msg = match self.inner.next_message().await {
				Ok(Some(msg)) => msg,
				Ok(None) => return Err(self.make_crashed()),
				Err(SessionError::Frame(crate::line_reader::FrameError::Oversize { limit })) => {
					return Err(self.malformed(FrameFault::Oversize { limit }));
				},
				Err(SessionError::Frame(_)) => return Err(self.make_crashed()),
				Err(SessionError::Serialize(_)) => {
					return Err(self.malformed(FrameFault::NotJson));
				},
			};

			// Check for the prompt response (turn end).
			if let InboundMessage::Response { ref result, .. } = msg {
				// Decode as PromptResult to get the stop reason.
				if let Ok(prompt_result) = serde_json::from_str::<wire::PromptResult>(result.get()) {
					self.turn_live = false;
					// Remove the sentinel pending entry.
					self
						.pending
						.retain(|k, _| !k.as_str().starts_with("__prompt_rpc_"));

					let turn = self.turn.clone();
					let outcome = stop_reason_to_end(prompt_result.stop_reason);
					self
						.queue
						.push_back(DelegatedEvent::Lifecycle(LifecycleEvent::TurnCompleted {
							turn,
							outcome,
						}));
					continue;
				}
			}

			if let InboundMessage::ErrorResponse { code, ref data, .. } = msg {
				self.turn_live = false;
				self
					.pending
					.retain(|k, _| !k.as_str().starts_with("__prompt_rpc_"));
				let turn = self.turn.clone();
				let failure = if self.login_required || wire::is_missing_login(code, data.as_ref()) {
					omp_agent::delegated::RemoteFailure::AuthRequired
				} else {
					omp_agent::delegated::RemoteFailure::Internal
				};
				self
					.queue
					.push_back(DelegatedEvent::Failure { turn, failure });
				continue;
			}

			self.dispatch(msg)?;
		}
	}

	async fn answer<'a>(
		&'a mut self,
		prompt: &'a PromptId,
		answer: PermissionAnswer,
	) -> Result<(), ExecutorError> {
		let Some(pending) = self.pending.remove(prompt) else {
			return Err(ExecutorError::Protocol {
				executor:  self.executor.clone(),
				violation: ProtocolViolation::UnknownPrompt,
			});
		};

		// Find the option to send: prefer allow_once / reject_once; never
		// send allow_always or reject_always (would outlast the session).
		let option_id = match answer {
			PermissionAnswer::Deny
			| PermissionAnswer::AllowOnce
			| PermissionAnswer::AllowForSession => {
				// Find the best-fitting option.
				let target_kind = match answer {
					PermissionAnswer::Deny => PermissionOptionKind::RejectOnce,
					PermissionAnswer::AllowOnce | PermissionAnswer::AllowForSession => {
						PermissionOptionKind::AllowOnce
					},
				};
				// Try exact match first; fall back to a weaker match.
				pending
					.options
					.iter()
					.find(|(_, k)| *k == target_kind)
					.or_else(|| {
						// For deny: also accept reject_always (narrowed to once).
						// For allow: also accept allow_always (narrowed to once).
						let fallback = match answer {
							PermissionAnswer::Deny => PermissionOptionKind::RejectAlways,
							_ => PermissionOptionKind::AllowAlways,
						};
						pending.options.iter().find(|(_, k)| *k == fallback)
					})
					.map(|(id, _)| id.clone())
			},
		};

		let outcome = match option_id {
			Some(id) => PermissionOutcome::Selected { option_id: id },
			None => {
				// No mappable option → deny.
				PermissionOutcome::Cancelled
			},
		};

		self
			.inner
			.answer_request(&pending.rpc_id, RequestPermissionResponse { outcome })
			.map_err(|error| session_error(&self.executor, error))
	}

	async fn interrupt(&mut self) -> Result<(), ExecutorError> {
		if !self.turn_live {
			return Ok(());
		}
		let _ = self
			.inner
			.send_notification("session/cancel", CancelParams {
				session_id: self.session_id.as_str(),
			});
		Ok(())
	}

	async fn terminate(&mut self) -> Result<(), ExecutorError> {
		if self.terminated {
			return Ok(());
		}
		self.terminated = true;

		if self.turn_live {
			let _ = self.drain_after_cancel().await;
		}

		let reaped = match self.tree.as_mut() {
			Some(tree) => tree.terminate_tree().await,
			None => true,
		};

		if reaped {
			Ok(())
		} else {
			Err(ExecutorError::TeardownFailed {
				executor: self.executor.clone(),
				fault:    omp_agent::delegated::TeardownFault::Unreaped,
			})
		}
	}
}

impl Drop for AcpDelegatedSession {
	fn drop(&mut self) {
		// ChildTree::drop kills the process group.
	}
}

// ── helpers ──────────────────────────────────────────────────────────────────

/// Converts a session-level [`SessionError`] to [`ExecutorError`].
fn session_error(
	executor: &omp_agent::delegated::ExecutorId,
	error: SessionError,
) -> ExecutorError {
	match error {
		SessionError::Frame(_) => {
			ExecutorError::Crashed { executor: executor.clone(), exit: ExitFact::Unknown }
		},
		SessionError::Serialize(_) => ExecutorError::MalformedStream {
			executor: executor.clone(),
			frame:    0,
			fault:    FrameFault::NotJson,
		},
	}
}

/// Reads inbound messages until the response for `rpc_id` arrives, or until
/// `limit` elapses.
async fn await_response<R, T>(
	session: &mut AcpSession<R>,
	executor: &omp_agent::delegated::ExecutorId,
	rpc_id: i64,
	context: &'static str,
	limit: std::time::Duration,
) -> Result<T, ExecutorError>
where
	R: AsyncRead + Unpin,
	T: DeserializeOwned,
{
	let fut = async {
		loop {
			let msg = session.next_message().await.map_err(|error| {
				let fault = match error {
					SessionError::Frame(crate::line_reader::FrameError::Oversize { limit }) => {
						FrameFault::Oversize { limit }
					},
					_ => FrameFault::NotJson,
				};
				ExecutorError::MalformedStream { executor: executor.clone(), frame: 0, fault }
			})?;
			let Some(msg) = msg else {
				return Err(ExecutorError::Crashed {
					executor: executor.clone(),
					exit:     ExitFact::Unknown,
				});
			};
			match msg {
				InboundMessage::Response { id: wire::RequestId::Int(i), result } if i == rpc_id => {
					return serde_json::from_str(result.get()).map_err(|_| {
						ExecutorError::MalformedStream {
							executor: executor.clone(),
							frame:    0,
							fault:    FrameFault::MissingField { field: context },
						}
					});
				},
				InboundMessage::ErrorResponse { id: Some(wire::RequestId::Int(i)), code, data }
					if i == rpc_id =>
				{
					if wire::is_missing_login(code, data.as_ref()) {
						return Err(ExecutorError::AuthRequired { executor: executor.clone() });
					}
					return Err(ExecutorError::Protocol {
						executor:  executor.clone(),
						violation: ProtocolViolation::RequestRejected { code },
					});
				},
				// Any other message (stray notifications, unknown) during handshake — skip.
				_ => {},
			}
		}
	};

	timeout(limit, fut)
		.await
		.map_err(|_| ExecutorError::Hung { executor: executor.clone(), idle: limit })?
}

/// Maps an ACP `ToolCallStatus` to a `ToolOutcome` when the call is complete.
/// Returns `None` when the status means the call is still in progress.
const fn tool_status_to_outcome(status: wire::ToolCallStatus) -> Option<ToolOutcome> {
	match status {
		wire::ToolCallStatus::Completed => Some(ToolOutcome::Succeeded),
		wire::ToolCallStatus::Cancelled => Some(ToolOutcome::Interrupted),
		wire::ToolCallStatus::Error => Some(ToolOutcome::Failed),
		wire::ToolCallStatus::Pending | wire::ToolCallStatus::InProgress => None,
		wire::ToolCallStatus::Unknown => None,
	}
}

/// Maps an ACP `StopReason` to a `TurnEnd`.
const fn stop_reason_to_end(reason: wire::StopReason) -> TurnEnd {
	match reason {
		wire::StopReason::EndTurn
		| wire::StopReason::MaxTokens
		| wire::StopReason::MaxTurnRequests => TurnEnd::Completed,
		wire::StopReason::Cancelled => TurnEnd::Interrupted,
		wire::StopReason::Refusal => TurnEnd::Completed,
		wire::StopReason::Unknown => TurnEnd::Completed,
	}
}

/// Derives the best `PermissionKind` from the options the agent offers.
///
/// ACP does not classify the effect separately; we infer from the option kinds
/// what the agent is asking. When the options are ambiguous, we use
/// `Unclassified`, which the host will deny without asking.
fn classify_permission_options(options: &[wire::PermissionOption]) -> PermissionKind {
	let has_allow = options.iter().any(|o| {
		matches!(o.kind, PermissionOptionKind::AllowOnce | PermissionOptionKind::AllowAlways)
	});
	let has_reject = options.iter().any(|o| {
		matches!(o.kind, PermissionOptionKind::RejectOnce | PermissionOptionKind::RejectAlways)
	});
	if has_allow && has_reject {
		// Standard allow/deny pair — we can't tell what effect without
		// inspecting the tool call title; report Exec as the safest label.
		PermissionKind::Exec
	} else {
		PermissionKind::Unclassified
	}
}
