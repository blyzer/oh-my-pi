//! Agent Client Protocol adapter over the journal-first kernel and session.

use std::{borrow::Cow, fs, io, mem, path::Path, sync::Arc};

use miette::{IntoDiagnostic as _, miette};
use omp_agent::{
	ApprovalDecision, ApprovalScope, ApprovalSource, Inference, Kernel, RunControl, TurnInput, Up,
};
use omp_core::{Str, base64};
use omp_driver::{headless::kernel::SessionHome, sessions::SessionIndex};
use omp_session::{AttachmentInput, Session, SessionError};
use serde::Deserialize as _;
use serde_json::{Map, Value, json};
use tokio::io::{
	AsyncBufRead, AsyncBufReadExt as _, AsyncRead, AsyncWrite, AsyncWriteExt as _, BufReader, stdin,
	stdout,
};

use crate::{
	acp_client::{
		AcpClient, AcpSettings, Answer, ClientCapabilities, EditorDocumentsHost, PermissionOptionId,
		PermissionOutcome, RequestPermissionResponse, RpcError,
	},
	acp_events::AcpEventMapper,
	chat_cmd::{Launch, LaunchEnv},
	cli::{AcpArgs, ChatArgs},
};

/// Maximum number of sessions returned by one `session/list` request.
const SESSION_PAGE_SIZE: usize = 50;

/// Largest inbound NDJSON frame the adapter accepts (ADR 0037 §1). It must
/// hold the largest document an editor may send back (the 4 MiB snapshot cap)
/// after worst-case JSON escaping, and base64 prompt images ride the same
/// frames. A longer frame is discarded up to its newline and answered with an
/// invalid-request error; the connection stays up.
const MAX_FRAME_BYTES: usize = 64 * 1024 * 1024;

const _: () = assert!(
	MAX_FRAME_BYTES >= 6 * omp_tools::read::SNAPSHOT_MAX_BYTES + 64 * 1024,
	"an escaped maximum-size editor buffer must fit one ACP frame"
);

/// Runs ACP using stdin for NDJSON requests and stdout for NDJSON responses.
pub async fn run(args: AcpArgs) -> miette::Result<()> {
	let max_time = args.max_time.map(|duration| duration.0);
	let future = run_inner(args.launch);
	match max_time {
		Some(limit) => tokio::time::timeout(limit, future)
			.await
			.map_err(|_| miette!("ACP mode exceeded --max-time"))?,
		None => future.await,
	}
}

async fn run_inner(args: ChatArgs) -> miette::Result<()> {
	let project = fs::canonicalize(&args.project).into_diagnostic()?;
	let ctx = Arc::new(crate::process_ctx(&project)?);
	let connection = AcpConnection::new(AcpSettings::from_con(&ctx));
	let env = LaunchEnv::production(&project, args.gateway.is_some())?;
	let mut launch = Launch::prepare(args, ctx, env).await?;
	let mut input = FrameReader::new(BufReader::new(stdin()), MAX_FRAME_BYTES);
	let mut output = stdout();
	let Some(capabilities) = initialize_transport(&mut input, &mut output).await? else {
		return Ok(());
	};
	connection.client.initialize(capabilities);
	let (kernel, session) = launch.compose().await?;
	connection.bind_documents(Arc::new(kernel.inference().editor_documents()));
	let home = SessionHome::new(
		&launch.data_dir,
		&launch.project,
		&launch.options,
		launch.model.clone(),
		kernel.mailbox(),
	)
	.into_diagnostic()?
	.with_facts_of(&session)
	.with_rules(Arc::clone(kernel.inference().rule_scope()));
	serve_acp_state(kernel, session, home, input, output, connection, true).await
}

async fn initialize_transport<R, W>(
	input: &mut FrameReader<R>,
	output: &mut W,
) -> miette::Result<Option<ClientCapabilities>>
where
	R: AsyncBufRead + Unpin,
	W: AsyncWrite + Unpin,
{
	loop {
		match input.next().await.into_diagnostic()? {
			Frame::Eof => return Ok(None),
			Frame::Oversize => {
				write_frame(output, &error(Value::Null, -32600, OVERSIZE_FRAME)).await?;
				continue;
			},
			Frame::Line => {},
		}
		if input.is_blank() {
			continue;
		}
		let frame: Value = match serde_json::from_slice(input.line()) {
			Ok(frame) => frame,
			Err(source) => {
				write_frame(output, &error(Value::Null, -32700, &source.to_string())).await?;
				continue;
			},
		};
		let id = frame.get("id").cloned();
		if frame.get("method").and_then(Value::as_str) != Some("initialize") {
			if let Some(id) = id {
				write_frame(
					output,
					&error(id, -32002, "initialize must complete before other requests"),
				)
				.await?;
			}
			continue;
		}
		let params = frame.get("params").and_then(Value::as_object);
		let version = params
			.and_then(|params| params.get("protocolVersion"))
			.and_then(Value::as_u64);
		if version != Some(1) {
			if let Some(id) = id {
				write_frame(output, &error(id, -32602, "unsupported ACP protocol version")).await?;
			}
			continue;
		}
		let capabilities = params.map_or_else(ClientCapabilities::default, |params| {
			ClientCapabilities::from_initialize(params)
		});
		if let Some(id) = id {
			write_frame(output, &success(id, initialize_response(capabilities.auth.terminal))).await?;
		}
		return Ok(Some(capabilities));
	}
}

/// Answer to an inbound frame longer than [`MAX_FRAME_BYTES`].
const OVERSIZE_FRAME: &str = "frame exceeds the ACP frame size limit";

/// What [`FrameReader::next`] found.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Frame {
	/// A complete line, readable through [`FrameReader::line`] until the next
	/// read.
	Line,
	/// A line longer than the limit, discarded through its newline.
	Oversize,
	/// End of input.
	Eof,
}

/// Bounded NDJSON line reader. One owned line buffer is reused across frames;
/// a line past the limit is discarded without buffering it. Cancel-safe: all
/// progress lives in `self`, so a `select!` may drop [`Self::next`] at any
/// await point.
struct FrameReader<R> {
	reader:     R,
	line:       Vec<u8>,
	limit:      usize,
	complete:   bool,
	discarding: bool,
}

impl<R: AsyncBufRead + Unpin> FrameReader<R> {
	const fn new(reader: R, limit: usize) -> Self {
		Self { reader, line: Vec::new(), limit, complete: false, discarding: false }
	}

	async fn next(&mut self) -> io::Result<Frame> {
		if mem::take(&mut self.complete) {
			self.line.clear();
		}
		loop {
			let available = self.reader.fill_buf().await?;
			if available.is_empty() {
				// A final line without a newline is still a frame.
				if !self.line.is_empty() && !self.discarding {
					self.complete = true;
					return Ok(Frame::Line);
				}
				self.line.clear();
				self.discarding = false;
				return Ok(Frame::Eof);
			}
			let newline = available.iter().position(|byte| *byte == b'\n');
			let chunk = &available[..newline.unwrap_or(available.len())];
			if !self.discarding {
				if self.line.len() + chunk.len() > self.limit {
					self.discarding = true;
					self.line = Vec::new();
				} else {
					self.line.extend_from_slice(chunk);
				}
			}
			let used = newline.map_or(available.len(), |index| index + 1);
			self.reader.consume(used);
			if newline.is_some() {
				if mem::take(&mut self.discarding) {
					return Ok(Frame::Oversize);
				}
				self.complete = true;
				return Ok(Frame::Line);
			}
		}
	}

	/// The line the last [`Frame::Line`] completed, without its newline.
	fn line(&self) -> &[u8] {
		&self.line
	}

	/// Whether the completed line holds only whitespace.
	fn is_blank(&self) -> bool {
		self.line.iter().all(u8::is_ascii_whitespace)
	}
}

async fn write_frame<W: AsyncWrite + Unpin>(output: &mut W, value: &Value) -> miette::Result<()> {
	let mut bytes = serde_json::to_vec(value).into_diagnostic()?;
	bytes.push(b'\n');
	output.write_all(&bytes).await.into_diagnostic()?;
	output.flush().await.into_diagnostic()
}

struct TurnCompletion<C> {
	kernel:   Kernel<C>,
	session:  Session,
	id:       Option<Value>,
	response: Result<Value, (i64, &'static str)>,
}

enum InputEvent<C> {
	Frame(Frame),
	Turn(TurnCompletion<C>),
}

/// One ACP connection: the controller loop over an NDJSON transport, and the
/// [`AcpClient`] through which the agent reaches the editor on the other end.
pub struct AcpConnection {
	client: AcpClient,
	output: flume::Sender<Value>,
	frames: flume::Receiver<Value>,
}

impl AcpConnection {
	/// Creates a connection with `settings`. Nothing is served until
	/// [`Self::serve`].
	#[must_use]
	pub fn new(settings: AcpSettings) -> Self {
		let (output, frames) = flume::unbounded();
		Self { client: AcpClient::new(settings, output.clone()), output, frames }
	}

	/// The editor handle of this connection. It outlives the connection; once
	/// the transport is gone every request fails with
	/// [`crate::acp_client::ClientRequestError::Disconnected`].
	#[must_use]
	pub fn client(&self) -> AcpClient {
		self.client.clone()
	}

	/// Binds the editor of this connection into `host` as the document base
	/// whenever the live session is eligible (ADR 0037 §1.2): the client
	/// advertises `fs.readTextFile`, `sv_acp_fs` is not `off`, and the session
	/// was made live with a `cwd` matching the project root. The binding is
	/// renewed on every session switch and dropped on `session/close`,
	/// `shutdown` and EOF.
	pub fn bind_documents(&self, host: Arc<dyn EditorDocumentsHost>) {
		self.client.bind_documents(host);
	}

	/// Serves ACP over caller-provided NDJSON transport halves. The client
	/// must `initialize` before anything else.
	pub async fn serve<C, R, W>(
		self,
		kernel: Kernel<C>,
		session: Session,
		home: SessionHome,
		input: R,
		output: W,
	) -> miette::Result<()>
	where
		C: Inference + Send + Sync + 'static,
		R: AsyncRead + Unpin,
		W: AsyncWrite + Unpin + Send + 'static,
	{
		let input = FrameReader::new(BufReader::new(input), MAX_FRAME_BYTES);
		serve_acp_state(kernel, session, home, input, output, self, false).await
	}
}

async fn serve_acp_state<C, R, W>(
	mut kernel: Kernel<C>,
	mut session: Session,
	home: SessionHome,
	mut lines: FrameReader<R>,
	mut output: W,
	connection: AcpConnection,
	mut initialized: bool,
) -> miette::Result<()>
where
	C: Inference + Send + Sync + 'static,
	R: AsyncBufRead + Unpin,
	W: AsyncWrite + Unpin + Send + 'static,
{
	let AcpConnection { client, output: output_tx, frames: output_rx } = connection;
	kernel.reconcile_jobs(&mut session).into_diagnostic()?;
	home.register(&session);
	if let Some(lifecycle) = kernel.lifecycle_hooks() {
		lifecycle
			.session_start(&omp_agent::SessionStart::launch(&session, &home.project_root))
			.await
			.into_diagnostic()?;
	}
	let mut session_id = session_identifier(&session);
	// The session a connection starts with was never given a `cwd`, so it is
	// not eligible for editor file I/O (ADR 0037 §2).
	client.switch_session(session_id.clone(), false, &home.project_root);
	let writer = tokio::spawn(async move {
		while let Ok(value) = output_rx.recv_async().await {
			let mut bytes = serde_json::to_vec(&value).into_diagnostic()?;
			bytes.push(b'\n');
			output.write_all(&bytes).await.into_diagnostic()?;
			output.flush().await.into_diagnostic()?;
		}
		Ok::<(), miette::Report>(())
	});
	let (snapshot, events) = session.subscribe();
	let mut forwarder = Some(
		start_forwarder(
			snapshot,
			events,
			output_tx.clone(),
			session_id.clone(),
			home.project_root.clone(),
			session.blobs().clone(),
			false,
		)
		.await?,
	);
	let mailbox = kernel.mailbox();
	// Every journaled approval prompt becomes
	// one `session/request_permission` request; the client's selected
	// option answers the prompt (`session/approve` remains for clients that
	// answer by prompt id).
	let _permissions = request_permissions(kernel.subscribe(), client.clone());
	let mut controller = Some((kernel, session));
	let mut active: Option<tokio::task::JoinHandle<TurnCompletion<C>>> = None;
	let mut closed = false;

	loop {
		let input_event: InputEvent<C> = if let Some(turn) = active.as_mut() {
			tokio::select! {
				completed = turn => InputEvent::Turn(completed.into_diagnostic()?),
				frame = lines.next() => InputEvent::Frame(frame.into_diagnostic()?),
			}
		} else {
			InputEvent::Frame(lines.next().await.into_diagnostic()?)
		};
		match input_event {
			InputEvent::Turn(completed) => {
				active = None;
				restore_turn(completed, &mut controller, &output_tx, forwarder.as_ref()).await?;
				continue;
			},
			InputEvent::Frame(Frame::Line) => {},
			InputEvent::Frame(Frame::Oversize) => {
				output_tx
					.send(error(Value::Null, -32600, OVERSIZE_FRAME))
					.into_diagnostic()?;
				continue;
			},
			InputEvent::Frame(Frame::Eof) => {
				// Transport loss: no answer can arrive any more, so requests in
				// flight fail now rather than at their deadlines.
				client.disconnect();
				if let Some(turn) = active.take() {
					let _ = mailbox.send(Up::Interrupt);
					restore_turn(
						turn.await.into_diagnostic()?,
						&mut controller,
						&output_tx,
						forwarder.as_ref(),
					)
					.await?;
				}
				break;
			},
		}
		if lines.is_blank() {
			continue;
		}
		let mut frame: Value = match serde_json::from_slice(lines.line()) {
			Ok(frame) => frame,
			Err(source) => {
				output_tx
					.send(error(Value::Null, -32700, &source.to_string()))
					.into_diagnostic()?;
				continue;
			},
		};
		let id = frame.get("id").cloned();
		let Some(method) = frame.get("method").and_then(Value::as_str) else {
			match (id, client_answer(&mut frame)) {
				// A response to one of our client requests. An approval answer
				// reaches the mailbox in line; a response nobody awaits is dropped.
				(Some(id), Some(answer)) => {
					if let Some((prompt_id, answer)) = client.answer(&id, answer) {
						let decision = permission_decision(answer);
						let _ = mailbox.send(Up::Approve { id: prompt_id, decision });
					}
				},
				(Some(id), None) => {
					output_tx
						.send(error(id, -32600, "request has no method"))
						.into_diagnostic()?;
				},
				(None, _) => {},
			}
			continue;
		};
		let params = frame
			.get("params")
			.and_then(Value::as_object)
			.cloned()
			.unwrap_or_default();
		if method != "initialize" && !initialized {
			if let Some(id) = id {
				output_tx
					.send(error(id, -32002, "initialize must complete before other requests"))
					.into_diagnostic()?;
			}
			continue;
		}
		if method == "session/prompt"
			&& targets_session(&params, session_id.as_str())
			&& let Some(turn) = active.take()
		{
			let _ = mailbox.send(Up::Interrupt);
			restore_turn(
				turn.await.into_diagnostic()?,
				&mut controller,
				&output_tx,
				forwarder.as_ref(),
			)
			.await?;
		}
		let result = match method {
			"initialize" => {
				let version = params.get("protocolVersion").and_then(Value::as_u64);
				if version != Some(1) {
					Err((-32602, "unsupported ACP protocol version"))
				} else {
					initialized = true;
					let capabilities = ClientCapabilities::from_initialize(&params);
					client.initialize(capabilities);
					Ok(initialize_response(capabilities.auth.terminal))
				}
			},
			"authenticate" => {
				let method = params.get("methodId").and_then(Value::as_str);
				if matches!(method, Some("agent"))
					|| client.capabilities().auth.terminal && matches!(method, Some("terminal"))
				{
					Ok(json!({}))
				} else {
					Err((-32602, "unknown ACP authentication method"))
				}
			},
			"session/new" if active.is_some() => Err((-32001, "a turn is already running")),
			"session/new" => {
				let cwd = validate_session_cwd(&home, &params);
				if let Err(message) = cwd {
					Err((-32602, message))
				} else {
					let next = match home.create(None) {
						Ok(next) => next,
						Err(source) => {
							if let Some(id) = id {
								output_tx
									.send(error(id, -32000, &source.to_string()))
									.into_diagnostic()?;
							}
							continue;
						},
					};
					switch_session(
						&mut controller,
						next,
						&home,
						&output_tx,
						&mut forwarder,
						&mut session_id,
						&client,
						cwd == Ok(true),
						false,
						omp_agent::SwitchReason::New,
						closed,
					)
					.await?;
					closed = false;
					Ok(new_session_descriptor(session_id.as_str(), home.model.as_str()))
				}
			},
			"session/load" | "session/resume" if active.is_some() => {
				Err((-32001, "a turn is already running"))
			},
			"session/load" | "session/resume" => {
				let cwd = validate_session_cwd(&home, &params);
				if let Err(message) = cwd {
					Err((-32602, message))
				} else {
					let selector = match requested_session(&params) {
						Ok(selector) => selector,
						Err(message) => {
							if let Some(id) = id {
								output_tx
									.send(error(id, -32602, message))
									.into_diagnostic()?;
							}
							continue;
						},
					};
					let replay = method == "session/load";
					let next = match home.open(Path::new(selector)) {
						Ok(next) => next,
						Err(source) => {
							if let Some(id) = id {
								output_tx
									.send(error(id, -32000, &source.to_string()))
									.into_diagnostic()?;
							}
							continue;
						},
					};
					switch_session(
						&mut controller,
						next,
						&home,
						&output_tx,
						&mut forwarder,
						&mut session_id,
						&client,
						cwd == Ok(true),
						replay,
						omp_agent::SwitchReason::Resume,
						closed,
					)
					.await?;
					closed = false;
					Ok(session_state(home.model.as_str()))
				}
			},
			// Stored sessions are newest first, paged by an
			// offset cursor, optionally scoped to one `cwd`. The live session
			// is flushed to disk by construction (journal-first), so the scan
			// already sees it.
			"session/list" => match list_sessions(&home, &params) {
				Ok(page) => Ok(page),
				Err(message) => Err((-32602, message)),
			},
			// Copy the source journal (the whole
			// branch tree travels) and switch authority to the copy.
			"session/fork" if active.is_some() => Err((-32001, "a turn is already running")),
			"session/fork" => {
				let cwd = validate_session_cwd(&home, &params);
				if let Err(message) = cwd {
					Err((-32602, message))
				} else {
					let selector = match requested_session(&params) {
						Ok(selector) => selector,
						Err(message) => {
							if let Some(id) = id {
								output_tx
									.send(error(id, -32602, message))
									.into_diagnostic()?;
							}
							continue;
						},
					};
					let next = match home.fork(Path::new(selector)) {
						Ok(next) => next,
						Err(source) => {
							if let Some(id) = id {
								output_tx
									.send(error(id, -32000, &source.to_string()))
									.into_diagnostic()?;
							}
							continue;
						},
					};
					switch_session(
						&mut controller,
						next,
						&home,
						&output_tx,
						&mut forwarder,
						&mut session_id,
						&client,
						cwd == Ok(true),
						false,
						omp_agent::SwitchReason::Fork,
						closed,
					)
					.await?;
					closed = false;
					Ok(new_session_descriptor(session_id.as_str(), home.model.as_str()))
				}
			},
			"session/set_mode" if !targets_session(&params, session_id.as_str()) => {
				Err((-32000, "unsupported ACP session"))
			},
			"session/set_mode" => {
				if params.get("modeId").and_then(Value::as_str) != Some("default") {
					Err((-32602, "unsupported ACP session mode"))
				} else {
					output_tx
						.send(session_update(
							session_id.as_str(),
							json!({
								"sessionUpdate": "current_mode_update",
								"currentModeId": "default",
							}),
						))
						.into_diagnostic()?;
					Ok(json!({}))
				}
			},
			"session/set_config_option" if !targets_session(&params, session_id.as_str()) => {
				Err((-32000, "unsupported ACP session"))
			},
			"session/set_config_option" => {
				let valid = params.get("configId").and_then(Value::as_str) == Some("model")
					&& params.get("value").and_then(Value::as_str) == Some(home.model.as_str());
				if !valid {
					Err((-32602, "unsupported ACP session config option"))
				} else {
					let state = session_state(home.model.as_str());
					output_tx
						.send(session_update(
							session_id.as_str(),
							json!({
								"sessionUpdate": "config_option_update",
								"configOptions": state["configOptions"].clone(),
							}),
						))
						.into_diagnostic()?;
					Ok(json!({"configOptions": state["configOptions"].clone()}))
				}
			},
			"session/prompt" if closed => Err((-32000, "ACP session is closed")),
			"session/prompt" if !targets_session(&params, session_id.as_str()) => {
				Err((-32000, "unsupported ACP session"))
			},
			"session/prompt" => match prompt_input(&params) {
				Ok(prompt) => {
					let (mut kernel, mut session) = controller
						.take()
						.expect("idle ACP controller owns its kernel and session");
					let input = match prompt.into_turn_input(&session) {
						Ok(input) => input,
						Err(source) => {
							controller = Some((kernel, session));
							if let Some(id) = id {
								output_tx
									.send(error(id, -32000, &source.to_string()))
									.into_diagnostic()?;
							}
							continue;
						},
					};
					let turn_output = output_tx.clone();
					let turn_session = session_id.clone();
					active = Some(tokio::spawn(async move {
						let response = match kernel
							.run_turn(&mut session, input, RunControl::default())
							.await
						{
							Ok(outcome) => Ok(prompt_response(&session, &outcome)),
							Err(source) => {
								let text = source.to_string();
								let message_id = session
									.head()
									.map(|entry| entry.to_string())
									.unwrap_or_else(|| "error".to_owned());
								let _ = turn_output.send(session_update(
									turn_session.as_str(),
									json!({
										"sessionUpdate": "agent_message_chunk",
										"content": {"type": "text", "text": text},
										"messageId": message_id,
									}),
								));
								Ok(json!({"stopReason": error_stop_reason(&text)}))
							},
						};
						TurnCompletion { kernel, session, id, response }
					}));
					continue;
				},
				Err(message) => Err((-32602, message)),
			},
			// ACP names the notification `cancel`; `session/cancel` is the
			// legacy spelling earlier omp clients used.
			"cancel" | "session/cancel" if !targets_session(&params, session_id.as_str()) => {
				Err((-32000, "unsupported ACP session"))
			},
			"cancel" | "session/cancel" => {
				if active.is_some() {
					let _ = mailbox.send(Up::Interrupt);
				}
				Ok(json!({}))
			},
			"session/approve" => match approval(&params) {
				Ok((id, decision)) => {
					if active.is_some() {
						let _ = mailbox.send(Up::Approve { id, decision });
					}
					Ok(json!({}))
				},
				Err(message) => Err((-32602, message)),
			},
			"session/close" if !targets_session(&params, session_id.as_str()) => Ok(json!({})),
			"session/close" if active.is_some() => Err((-32001, "a turn is already running")),
			"session/close" => {
				if !closed {
					if let Some((kernel, session)) = controller.as_mut() {
						if let Some(lifecycle) = kernel.lifecycle_hooks() {
							lifecycle
								.session_shutdown(&omp_agent::SessionShutdown::new(
									session,
									omp_agent::ShutdownReason::UserExit,
								))
								.await;
						}
						session.session_switch().into_diagnostic()?;
						home.unregister(session);
					}
					client.close_session();
					closed = true;
				}
				Ok(json!({}))
			},
			"shutdown" => {
				if let Some(id) = id {
					output_tx.send(success(id, json!({}))).into_diagnostic()?;
				}
				// No frame is read after `shutdown`, so no answer can arrive.
				client.disconnect();
				if let Some(turn) = active.take() {
					// ACP shutdown is graceful: it waits for the active prompt's
					// delivery handlers before disposing the session. EOF remains
					// the abrupt transport-loss path that interrupts the turn.
					restore_turn(
						turn.await.into_diagnostic()?,
						&mut controller,
						&output_tx,
						forwarder.as_ref(),
					)
					.await?;
				}
				break;
			},
			_ => Err((-32601, "unknown ACP method")),
		};
		if let Some(id) = id {
			let succeeded = result.is_ok();
			let response = match result {
				Ok(value) => success(id, value),
				Err((code, message)) => error(id, code, message),
			};
			output_tx.send(response).into_diagnostic()?;
			if succeeded
				&& matches!(method, "session/new" | "session/load" | "session/resume" | "session/fork")
			{
				schedule_bootstrap_updates(output_tx.clone(), session_id.clone(), home.model.clone());
			}
		}
	}

	client.disconnect();
	let (kernel, mut session) = controller
		.take()
		.expect("ACP controller owns its kernel and session after active turn completion");
	if !closed {
		session
			.record_exit(omp_session::ExitCause::Normal)
			.into_diagnostic()?;
		if let Some(lifecycle) = kernel.lifecycle_hooks() {
			lifecycle
				.session_shutdown(&omp_agent::SessionShutdown::new(
					&session,
					omp_agent::ShutdownReason::UserExit,
				))
				.await;
		}
		home.unregister(&session);
	}
	drop(session);
	drop(kernel);
	if let Some(forwarder) = forwarder {
		forwarder.finish().await?;
	}
	drop(output_tx);
	writer.await.into_diagnostic()??;
	Ok(())
}

/// Splits a client response off its JSON-RPC envelope: its `result`, or its
/// `error` object. `None` when the frame is neither.
fn client_answer(frame: &mut Value) -> Option<Answer> {
	let frame = frame.as_object_mut()?;
	if let Some(result) = frame.remove("result") {
		return Some(Ok(result));
	}
	let error = frame.remove("error")?;
	Some(Err(
		RpcError::deserialize(&error)
			.unwrap_or_else(|_| RpcError { code: -32603, message: Str::new_static("") }),
	))
}

/// Maps a `session/request_permission` answer to the kernel's decision:
/// option ids `allow_once`/`allow_always`/`reject_once`/`reject_always`; an
/// error, a `cancelled` outcome, a malformed answer, or an unknown option
/// fails closed.
fn permission_decision(answer: Answer) -> ApprovalDecision {
	let option = answer
		.ok()
		.and_then(|result| RequestPermissionResponse::deserialize(&result).ok())
		.and_then(|response| match response.outcome {
			PermissionOutcome::Selected { option_id } => Some(option_id),
			PermissionOutcome::Cancelled => None,
		});
	let (approved, scope) = match option {
		Some(PermissionOptionId::AllowOnce) => (true, ApprovalScope::Once),
		Some(PermissionOptionId::AllowAlways) => (true, ApprovalScope::Session),
		Some(PermissionOptionId::RejectAlways) => (false, ApprovalScope::Session),
		Some(PermissionOptionId::RejectOnce | PermissionOptionId::Unknown) | None => {
			(false, ApprovalScope::Once)
		},
	};
	ApprovalDecision {
		approved,
		scope,
		source: ApprovalSource::External,
		decided_by: None,
		reason: (!approved).then(|| Str::new_static("rejected by ACP client")),
		audited: false,
	}
}

/// Sends one `session/request_permission` per kernel approval ticket through
/// the connection's request table. Ends when the kernel's event stream or
/// the transport does.
fn request_permissions(
	events: flume::Receiver<omp_agent::KernelEvent>,
	client: AcpClient,
) -> tokio::task::JoinHandle<()> {
	tokio::spawn(async move {
		while let Ok(event) = events.recv_async().await {
			let omp_agent::KernelEvent::ApprovalRequested(ticket) = event else {
				continue;
			};
			let first = ticket.reasons.first();
			let mut tool_call = json!({
				"toolCallId": ticket.invocation_id.as_deref().unwrap_or(ticket.ticket_id.as_str()),
				"title": first.map_or("Approval required", |spec| spec.title.as_str()),
				"status": "pending",
				"rawInput": {
					"subject": first.map(|spec| spec.subject.as_str()),
					"body": first.map(|spec| spec.body.as_str()),
				},
			});
			if let Some(spec) = first {
				let kind = match spec.kind.as_str() {
					"exec" | "execute" | "bash" | "shell" => "execute",
					"write" | "edit" => "edit",
					"delete" => "delete",
					"move" => "move",
					"read" => "read",
					_ => "other",
				};
				tool_call["kind"] = Value::String(kind.to_owned());
				if kind == "execute" {
					tool_call["content"] = json!([{
						"type": "content",
						"content": {"type": "text", "text": format!("$ {}", spec.subject)},
					}]);
				}
			}
			if let Err(error) = client.request_permission(ticket.ticket_id.clone(), tool_call) {
				tracing::debug!(
					error = &error as &dyn std::error::Error,
					"ACP permission request not sent"
				);
				break;
			}
		}
	})
}

struct EventForwarder {
	flush: flume::Sender<tokio::sync::oneshot::Sender<()>>,
	task:  tokio::task::JoinHandle<miette::Result<()>>,
}

impl EventForwarder {
	async fn flush(&self) -> miette::Result<()> {
		let (tx, rx) = tokio::sync::oneshot::channel();
		self.flush.send_async(tx).await.into_diagnostic()?;
		rx.await.into_diagnostic()
	}

	async fn finish(self) -> miette::Result<()> {
		drop(self.flush);
		self.task.await.into_diagnostic()?
	}
}

async fn start_forwarder(
	snapshot: omp_dom::Snapshot,
	events: flume::Receiver<omp_dom::Event>,
	output: flume::Sender<Value>,
	session_id: Str,
	cwd: std::path::PathBuf,
	blobs: omp_journal::blob::BlobStore,
	replay: bool,
) -> miette::Result<EventForwarder> {
	let (flush_tx, flush_rx) = flume::unbounded::<tokio::sync::oneshot::Sender<()>>();
	let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
	let task = tokio::spawn(async move {
		let mut mapper = AcpEventMapper::new(&snapshot, cwd, blobs);
		if replay {
			for update in mapper.replay_updates().into_diagnostic()? {
				if output
					.send(session_update(session_id.as_str(), update))
					.is_err()
				{
					let _ = ready_tx.send(());
					return Ok(());
				}
			}
		}
		let _ = ready_tx.send(());
		loop {
			tokio::select! {
				biased;
				flush = flush_rx.recv_async() => {
					let Ok(flush) = flush else { break };
					while let Ok(event) = events.try_recv() {
						for update in mapper.map_event(&event).into_diagnostic()? {
							if output.send(session_update(session_id.as_str(), update)).is_err() {
								let _ = flush.send(());
								return Ok(());
							}
						}
					}
					let _ = flush.send(());
				},
				event = events.recv_async() => {
					let Ok(event) = event else { break };
					for update in mapper.map_event(&event).into_diagnostic()? {
						if output.send(session_update(session_id.as_str(), update)).is_err() {
							return Ok(());
						}
					}
				},
			}
		}
		Ok(())
	});
	ready_rx.await.into_diagnostic()?;
	Ok(EventForwarder { flush: flush_tx, task })
}

async fn restore_turn<C>(
	completed: TurnCompletion<C>,
	controller: &mut Option<(Kernel<C>, Session)>,
	output: &flume::Sender<Value>,
	forwarder: Option<&EventForwarder>,
) -> miette::Result<()> {
	let TurnCompletion { kernel, session, id, response } = completed;
	*controller = Some((kernel, session));
	if let Some(forwarder) = forwarder {
		forwarder.flush().await?;
	}
	if let Some(id) = id {
		let response = match response {
			Ok(value) => success(id, value),
			Err((code, message)) => error(id, code, message),
		};
		output.send(response).into_diagnostic()?;
	}
	Ok(())
}

async fn switch_session<C>(
	controller: &mut Option<(Kernel<C>, Session)>,
	mut next: Session,
	home: &SessionHome,
	output: &flume::Sender<Value>,
	forwarder: &mut Option<EventForwarder>,
	session_id: &mut Str,
	client: &AcpClient,
	cwd_matched: bool,
	replay: bool,
	reason: omp_agent::SwitchReason,
	closed: bool,
) -> miette::Result<()> {
	let (kernel, mut previous) = controller
		.take()
		.expect("idle ACP controller owns its kernel and session");
	kernel.reconcile_jobs(&mut next).into_diagnostic()?;
	// A live previous session ends here; a closed one already ended. The
	// in-process hook hosts follow the switch either way, and `next` starts
	// once the switch is committed.
	let start = omp_agent::SessionStart::switched(&previous, &next, reason, &home.project_root);
	let lifecycle = kernel.lifecycle_hooks();
	if let Some(lifecycle) = &lifecycle {
		lifecycle
			.session_switch(&previous, &next, (!closed).then_some(reason))
			.await;
	}
	let (snapshot, events) = next.subscribe();
	let _ = previous.session_switch();
	home.unregister(&previous);
	drop(previous);
	if let Some(previous_forwarder) = forwarder.take() {
		previous_forwarder.finish().await?;
	}
	home.register(&next);
	*session_id = session_identifier(&next);
	client.switch_session(session_id.clone(), cwd_matched, &home.project_root);
	*forwarder = Some(
		start_forwarder(
			snapshot,
			events,
			output.clone(),
			session_id.clone(),
			home.project_root.clone(),
			next.blobs().clone(),
			replay,
		)
		.await?,
	);
	*controller = Some((kernel, next));
	// The switch cannot be undone here: a host refusing the new session's
	// start is logged, and the session stays live.
	if let Some(lifecycle) = lifecycle
		&& let Err(error) = lifecycle.session_start(&start).await
	{
		tracing::warn!(
			error = &error as &dyn std::error::Error,
			"session_start refused after an ACP session switch"
		);
	}
	Ok(())
}

fn session_identifier(session: &Session) -> Str {
	session
		.journal_path()
		.file_stem()
		.and_then(|value| value.to_str())
		.map_or_else(|| Str::new_static("session"), Str::new)
}

fn requested_session(params: &Map<String, Value>) -> Result<&str, &'static str> {
	params
		.get("sessionId")
		.or_else(|| params.get("session"))
		.and_then(Value::as_str)
		.ok_or("sessionId is required")
}

fn targets_session(params: &Map<String, Value>, current: &str) -> bool {
	params
		.get("sessionId")
		.and_then(Value::as_str)
		.is_none_or(|requested| requested == current)
}

/// Checks a session request's optional `cwd` against the project root.
/// `Ok(true)` means a `cwd` was supplied and matched, which editor file I/O
/// requires (ADR 0037 §2); `Ok(false)` means none was supplied.
fn validate_session_cwd(
	home: &SessionHome,
	params: &Map<String, Value>,
) -> Result<bool, &'static str> {
	let Some(cwd) = params.get("cwd").and_then(Value::as_str) else {
		return Ok(false);
	};
	let path = Path::new(cwd);
	if !path.is_absolute() {
		return Err("cwd must be an absolute path");
	}
	let path = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
	if path != home.project_root {
		return Err("cwd does not match the configured ACP project");
	}
	Ok(true)
}

/// `session/list {cwd?, cursor?}` → `{sessions, nextCursor?}` pages every
/// journal in the session directory, newest first; `cwd` scopes by genesis
/// working directory and `cursor` offsets that ordering.
fn list_sessions(home: &SessionHome, params: &Map<String, Value>) -> Result<Value, &'static str> {
	let cwd = match params.get("cwd").and_then(Value::as_str) {
		Some(cwd) => {
			let path = Path::new(cwd);
			if !path.is_absolute() {
				return Err("cwd must be an absolute path");
			}
			Some(fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf()))
		},
		None => None,
	};
	let offset = match params.get("cursor") {
		None | Some(Value::Null) => 0,
		Some(cursor) => cursor
			.as_str()
			.and_then(|cursor| cursor.parse::<usize>().ok())
			.or_else(|| {
				cursor
					.as_u64()
					.and_then(|cursor| usize::try_from(cursor).ok())
			})
			.ok_or("invalid session cursor")?,
	};
	let index =
		SessionIndex::open(&home.sessions_dir).map_err(|_| "session directory unreadable")?;
	let mut rows = index.list();
	if let Some(cwd) = &cwd {
		rows.retain(|row| {
			let recorded = Path::new(row.cwd.as_str());
			recorded == cwd || fs::canonicalize(recorded).is_ok_and(|recorded| recorded == *cwd)
		});
	}
	let total = rows.len();
	let page: Vec<Value> = rows
		.iter()
		.skip(offset)
		.take(SESSION_PAGE_SIZE)
		.map(|row| {
			let size = fs::metadata(&row.path).map(|meta| meta.len()).unwrap_or(0);
			json!({
				"sessionId": row.id,
				"cwd": row.cwd,
				"title": row.title,
				"updatedAt": jiff::Timestamp::from_millisecond(i64::try_from(row.updated_ms).unwrap_or(i64::MAX))
					.map(|stamp| stamp.to_string())
					.unwrap_or_default(),
				"_meta": {"messageCount": row.messages, "size": size},
			})
		})
		.collect();
	let next = offset.saturating_add(page.len());
	let mut result = json!({ "sessions": page });
	if next < total {
		result["nextCursor"] = Value::String(next.to_string());
	}
	Ok(result)
}

fn schedule_bootstrap_updates(output: flume::Sender<Value>, session_id: Str, model: Str) {
	tokio::spawn(async move {
		tokio::time::sleep(std::time::Duration::from_millis(50)).await;
		for update in [
			json!({"sessionUpdate": "current_mode_update", "currentModeId": "default"}),
			json!({
				"sessionUpdate": "config_option_update",
				"configOptions": session_state(model.as_str())["configOptions"].clone(),
			}),
			json!({"sessionUpdate": "available_commands_update", "availableCommands": []}),
			json!({
				"sessionUpdate": "session_info_update",
				"updatedAt": jiff::Timestamp::now().to_string(),
			}),
		] {
			if output
				.send(session_update(session_id.as_str(), update))
				.is_err()
			{
				break;
			}
		}
	});
}

fn initialize_response(terminal_auth: bool) -> Value {
	let mut auth = vec![json!({
		"id": "agent",
		"name": "Use existing local credentials",
		"description": "Authenticate via the provider keys/OAuth state already configured under ~/.o2.",
	})];
	if terminal_auth {
		auth.push(json!({
			"type": "terminal",
			"id": "terminal",
			"name": "Set up Oh My Pi in terminal",
			"description": "Launch the omp TUI to add provider keys and select models.",
			"args": ["--acp-terminal-auth"],
		}));
	}
	json!({
		"protocolVersion": 1,
		"agentInfo": {
			"name": "oh-my-pi",
			"title": "Oh My Pi",
			"version": env!("CARGO_PKG_VERSION"),
		},
		"authMethods": auth,
		"agentCapabilities": {
			"loadSession": true,
			"mcpCapabilities": {"http": true, "sse": true},
			"sessionCapabilities": {"list": {}, "fork": {}, "resume": {}, "close": {}},
			"promptCapabilities": {"image": true, "embeddedContext": true},
		},
	})
}

fn session_state(model: &str) -> Value {
	json!({
		"configOptions": [{
			"type": "select",
			"id": "model",
			"name": "Model",
			"currentValue": model,
			"options": [{"value": model, "name": model}],
		}],
		"modes": {
			"currentModeId": "default",
			"availableModes": [{
				"id": "default",
				"name": "Default",
				"description": "Standard coding-agent behavior",
			}],
		},
	})
}

fn new_session_descriptor(session_id: &str, model: &str) -> Value {
	let mut descriptor = session_state(model);
	descriptor["sessionId"] = Value::String(session_id.to_owned());
	descriptor
}

fn prompt_response(session: &Session, outcome: &omp_agent::TurnOutcome) -> Value {
	let stop_reason = if outcome.stop == omp_agent::TurnStop::Cancelled {
		"cancelled"
	} else {
		session
			.dom()
			.children(session.dom().body())
			.iter()
			.rev()
			.filter_map(|turn| session.dom().get(*turn))
			.flat_map(|turn| turn.kids.iter().rev())
			.filter_map(|handle| session.dom().get(*handle))
			.find(|node| node.tag == omp_dom::Tag::Known(omp_dom::KnownTag::Assistant))
			.and_then(|node| node.prop(&omp_dom::PropId::StopReason.into()))
			.and_then(omp_dom::Value::as_str)
			.map(|reason| match reason {
				"length" => "max_tokens",
				"max_requests" | "max_turn_requests" => "max_turn_requests",
				"aborted" | "cancelled" => "cancelled",
				"refusal" | "content_filter" => "refusal",
				_ => "end_turn",
			})
			.unwrap_or("end_turn")
	};
	let mut response = json!({"stopReason": stop_reason});
	let total = outcome.tokens_in.saturating_add(outcome.tokens_out);
	if total != 0 {
		response["usage"] = json!({
			"totalTokens": total,
			"inputTokens": outcome.tokens_in,
			"outputTokens": outcome.tokens_out,
		});
	}
	response
}

fn error_stop_reason(message: &str) -> &'static str {
	let message = message.to_ascii_lowercase();
	if message.contains("content_filter")
		|| message.contains("content filter")
		|| message.contains("refusal")
		|| message.contains("refused")
	{
		"refusal"
	} else {
		"end_turn"
	}
}

/// A `session/prompt` request reduced to the turn text and its image blocks,
/// each decoded with its declared `mimeType`.
struct PromptInput {
	text:   Str,
	images: Vec<AttachmentInput>,
}

impl PromptInput {
	/// Stores every image in the session's blob store and returns the turn
	/// input whose attachments reference them — the seam the chat composer's
	/// image chips also take.
	fn into_turn_input(self, session: &Session) -> Result<TurnInput, SessionError> {
		Ok(TurnInput { text: self.text, attachments: session.store_attachments(self.images)? })
	}
}

/// Reduces the request's `prompt` to text plus images: a bare string, a
/// `{text}` object, or the ACP content-block array (`text`, `image`,
/// `resource`, `resource_link`, `audio`).
fn prompt_input(params: &Map<String, Value>) -> Result<PromptInput, &'static str> {
	let prompt = params
		.get("prompt")
		.or_else(|| params.get("message"))
		.ok_or("session/prompt requires a prompt")?;
	if let Some(text) = prompt.as_str() {
		return Ok(PromptInput { text: Str::new(text), images: Vec::new() });
	}
	if let Some(text) = prompt.get("text").and_then(Value::as_str) {
		return Ok(PromptInput { text: Str::new(text), images: Vec::new() });
	}
	let blocks = prompt
		.as_array()
		.ok_or("session/prompt requires a prompt string or content blocks")?;
	let mut texts: Vec<Cow<'_, str>> = Vec::with_capacity(blocks.len());
	let mut images = Vec::new();
	for block in blocks {
		match block.get("type").and_then(Value::as_str) {
			Some("text") => {
				let text = block
					.get("text")
					.and_then(Value::as_str)
					.ok_or("text content block requires text")?;
				texts.push(Cow::Borrowed(text));
			},
			Some("image") => {
				let data = block
					.get("data")
					.and_then(Value::as_str)
					.ok_or("image content block requires base64 data")?;
				let mime = block
					.get("mimeType")
					.and_then(Value::as_str)
					.ok_or("image content block requires mimeType")?;
				images.push(decode_image(data, mime)?);
			},
			Some("resource") => {
				let resource = block
					.get("resource")
					.and_then(Value::as_object)
					.ok_or("resource block requires a resource object")?;
				let uri = resource
					.get("uri")
					.and_then(Value::as_str)
					.ok_or("resource block requires a resource uri")?;
				if let Some(text) = resource.get("text").and_then(Value::as_str) {
					texts.push(Cow::Borrowed(text));
				} else if let Some(mime) = resource
					.get("mimeType")
					.and_then(Value::as_str)
					.filter(|mime| mime.starts_with("image/"))
					&& let Some(blob) = resource.get("blob").and_then(Value::as_str)
				{
					images.push(decode_image(blob, mime)?);
				} else {
					texts.push(Cow::Owned(format!("[embedded resource: {uri}]")));
				}
			},
			Some("resource_link") => {
				let uri = block
					.get("uri")
					.and_then(Value::as_str)
					.ok_or("resource_link content block requires uri")?;
				texts.push(Cow::Borrowed(
					block
						.get("title")
						.or_else(|| block.get("name"))
						.and_then(Value::as_str)
						.unwrap_or(uri),
				));
			},
			Some("audio") => {
				block
					.get("data")
					.and_then(Value::as_str)
					.ok_or("audio content block requires base64 data")?;
				block
					.get("mimeType")
					.and_then(Value::as_str)
					.ok_or("audio content block requires mimeType")?;
				texts.push(Cow::Borrowed("[audio omitted]"));
			},
			_ => return Err("unsupported prompt content block"),
		}
	}
	let text = texts.join("\n\n");
	let text = text.trim();
	if text.is_empty() && images.is_empty() {
		return Err("prompt contains no text");
	}
	Ok(PromptInput { text: Str::new(text), images })
}

fn decode_image(data: &str, mime: &str) -> Result<AttachmentInput, &'static str> {
	base64::decode(data.as_bytes())
		.into_vec()
		.map(|bytes| AttachmentInput { mime: Str::new(mime), bytes: bytes.into() })
		.map_err(|_| "image content block data is not valid base64")
}

fn approval(params: &Map<String, Value>) -> Result<(Str, ApprovalDecision), &'static str> {
	let id = params
		.get("promptId")
		.or_else(|| params.get("id"))
		.and_then(Value::as_str)
		.ok_or("session/approve requires promptId")?;
	let approved = params
		.get("approved")
		.and_then(Value::as_bool)
		.unwrap_or(false);
	let scope = match params
		.get("scope")
		.and_then(Value::as_str)
		.unwrap_or("once")
	{
		"once" => ApprovalScope::Once,
		"call" => ApprovalScope::Call,
		"session" | "always" => ApprovalScope::Session,
		_ => return Err("session/approve has an invalid scope"),
	};
	Ok((Str::new(id), ApprovalDecision {
		approved,
		scope,
		source: ApprovalSource::External,
		decided_by: None,
		reason: None,
		audited: false,
	}))
}

fn session_update(session_id: &str, update: Value) -> Value {
	json!({
		"jsonrpc": "2.0",
		"method": "session/update",
		"params": {"sessionId": session_id, "update": update},
	})
}

fn success(id: Value, result: Value) -> Value {
	json!({"jsonrpc": "2.0", "id": id, "result": result})
}

fn error(id: Value, code: i64, message: &str) -> Value {
	json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

#[cfg(test)]
mod tests {
	use super::*;

	fn params(value: Value) -> Map<String, Value> {
		value.as_object().expect("object params").clone()
	}

	#[test]
	fn initialize_capabilities_and_terminal_auth_match_pi() {
		let ordinary = initialize_response(false);
		assert_eq!(ordinary["protocolVersion"], 1);
		assert_eq!(ordinary["agentInfo"]["name"], "oh-my-pi");
		assert_eq!(ordinary["agentCapabilities"]["loadSession"], true);
		assert_eq!(ordinary["agentCapabilities"]["mcpCapabilities"]["http"], true);
		assert_eq!(ordinary["agentCapabilities"]["mcpCapabilities"]["sse"], true);
		assert_eq!(ordinary["agentCapabilities"]["promptCapabilities"]["embeddedContext"], true);
		assert_eq!(ordinary["agentCapabilities"]["promptCapabilities"]["image"], true);
		assert_eq!(ordinary["authMethods"].as_array().map(Vec::len), Some(1));
		assert!(ordinary["authMethods"][0].get("type").is_none());

		let terminal = initialize_response(true);
		assert_eq!(terminal["authMethods"].as_array().map(Vec::len), Some(2));
		assert_eq!(terminal["authMethods"][1]["type"], "terminal");
		assert_eq!(terminal["authMethods"][1]["args"], json!(["--acp-terminal-auth"]));
	}

	#[test]
	fn prompt_accepts_string_object_and_content_blocks() {
		let plain = prompt_input(&params(json!({"prompt": "hi"}))).expect("string prompt");
		assert_eq!(plain.text.as_str(), "hi");
		assert!(plain.images.is_empty());

		let object =
			prompt_input(&params(json!({"prompt": {"text": "structured"}}))).expect("object prompt");
		assert_eq!(object.text.as_str(), "structured");

		let blocks = prompt_input(&params(json!({"prompt": [
			{"type": "text", "text": "look"},
			{"type": "image", "data": "aGVsbG8=", "mimeType": "image/png"},
			{"type": "resource", "resource": {"uri": "file:///a.txt", "text": "alpha"}},
			{"type": "resource", "resource": {"uri": "file:///b.bin", "mimeType": "application/octet-stream", "blob": "AAAA"}},
			{"type": "resource", "resource": {"uri": "file:///c.png", "mimeType": "image/png", "blob": "d29ybGQ="}},
			{"type": "resource_link", "uri": "file:///d.md", "title": "Design"},
			{"type": "audio", "data": "", "mimeType": "audio/wav"},
		]})))
		.expect("content blocks");
		assert_eq!(
			blocks.text.as_str(),
			"look\n\nalpha\n\n[embedded resource: file:///b.bin]\n\nDesign\n\n[audio omitted]"
		);
		let images = blocks
			.images
			.iter()
			.map(|image| (image.mime.as_str(), image.bytes.as_ref()))
			.collect::<Vec<_>>();
		assert_eq!(images, vec![
			("image/png", b"hello".as_slice()),
			("image/png", b"world".as_slice())
		]);
		assert_eq!(
			prompt_input(&params(json!({"prompt": [{"type": "image", "data": "aGVsbG8="}]}))).err(),
			Some("image content block requires mimeType")
		);
	}

	#[tokio::test]
	async fn frame_reader_bounds_lines_across_chunks_and_keeps_reading() {
		let input: &[u8] = b"exactly8\nthis line is far too long\n  \r\nlast";
		// A four-byte buffer makes every line span several reads.
		let mut frames = FrameReader::new(BufReader::with_capacity(4, input), 8);
		assert_eq!(frames.next().await.expect("read"), Frame::Line);
		assert_eq!(frames.line(), b"exactly8", "a line at the limit is accepted");
		assert_eq!(frames.next().await.expect("read"), Frame::Oversize);
		assert_eq!(frames.next().await.expect("read"), Frame::Line);
		assert!(frames.is_blank());
		assert_eq!(frames.next().await.expect("read"), Frame::Line);
		assert_eq!(frames.line(), b"last", "a final line needs no newline");
		assert_eq!(frames.next().await.expect("read"), Frame::Eof);

		let mut unterminated = FrameReader::new(&b"ok\noverflowing"[..], 4);
		assert_eq!(unterminated.next().await.expect("read"), Frame::Line);
		assert_eq!(unterminated.next().await.expect("read"), Frame::Eof);
	}

	#[test]
	fn permission_answers_decide_or_fail_closed() {
		let selected =
			|option: &str| Ok(json!({"outcome": {"outcome": "selected", "optionId": option}}));
		let decide = |answer: Answer| {
			let decision = permission_decision(answer);
			(decision.approved, decision.scope)
		};
		assert_eq!(decide(selected("allow_once")), (true, ApprovalScope::Once));
		assert_eq!(decide(selected("allow_always")), (true, ApprovalScope::Session));
		assert_eq!(decide(selected("reject_once")), (false, ApprovalScope::Once));
		assert_eq!(decide(selected("reject_always")), (false, ApprovalScope::Session));
		assert_eq!(decide(selected("maybe")), (false, ApprovalScope::Once));
		assert_eq!(
			decide(Ok(json!({"outcome": {"outcome": "cancelled"}}))),
			(false, ApprovalScope::Once)
		);
		assert_eq!(decide(Ok(json!({"outcome": 3}))), (false, ApprovalScope::Once));
		assert_eq!(
			decide(Err(RpcError { code: -32603, message: Str::new_static("") })),
			(false, ApprovalScope::Once)
		);
		assert_eq!(
			permission_decision(selected("reject_once"))
				.reason
				.as_deref(),
			Some("rejected by ACP client")
		);
	}

	#[test]
	fn client_answers_split_result_and_error_envelopes() {
		let mut result = json!({"jsonrpc": "2.0", "id": 1, "result": {"content": "x"}});
		assert_eq!(client_answer(&mut result), Some(Ok(json!({"content": "x"}))));
		let mut failure =
			json!({"jsonrpc": "2.0", "id": 1, "error": {"code": -32002, "message": "gone"}});
		assert_eq!(
			client_answer(&mut failure),
			Some(Err(RpcError { code: -32002, message: Str::new_static("gone") }))
		);
		let mut odd_error = json!({"jsonrpc": "2.0", "id": 1, "error": "gone"});
		assert_eq!(client_answer(&mut odd_error).map(|answer| answer.is_err()), Some(true));
		assert_eq!(client_answer(&mut json!({"jsonrpc": "2.0", "id": 1})), None);
		assert_eq!(client_answer(&mut json!([1])), None);
	}

	#[test]
	fn prompt_rejects_missing_and_malformed_content() {
		assert_eq!(prompt_input(&params(json!({}))).err(), Some("session/prompt requires a prompt"));
		assert_eq!(
			prompt_input(&params(json!({"prompt": [{"type": "text", "text": "  "}]}))).err(),
			Some("prompt contains no text")
		);
		assert_eq!(
			prompt_input(&params(
				json!({"prompt": [{"type": "image", "data": "%%%", "mimeType": "image/png"}]})
			))
			.err(),
			Some("image content block data is not valid base64")
		);
		assert_eq!(
			prompt_input(&params(json!({"prompt": [{"type": "video"}]}))).err(),
			Some("unsupported prompt content block")
		);
		let image_only = prompt_input(&params(json!({"prompt": [
			{"type": "image", "data": "aGVsbG8=", "mimeType": "image/png"},
		]})))
		.expect("an image-only prompt is a valid turn");
		assert!(image_only.text.is_empty());
		assert_eq!(image_only.images.len(), 1);
	}
}
