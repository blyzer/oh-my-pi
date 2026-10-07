//! Executable P7 proof for the real chat TUI, interruption, and terminal
//! restoration.

#![feature(impl_trait_in_assoc_type)]
#![cfg(unix)]

use std::{
	collections::VecDeque,
	fmt::Write as _,
	fs,
	io::{self, BufRead as _, BufReader, Read as _, Write as _},
	os::{
		fd::{self, AsFd as _, AsRawFd as _},
		unix::net::UnixStream,
	},
	path::{Path, PathBuf},
	process::{self, Child, Command, Stdio},
	sync::{
		Arc, LazyLock,
		atomic::{AtomicBool, Ordering},
	},
	task::{Context, Poll},
	thread,
	time::{Duration, Instant},
};

use bytes::Bytes;
use flume::{Receiver, Sender};
use futures::StreamExt as _;
use nix::{
	errno::Errno,
	fcntl::{FcntlArg, OFlag, fcntl},
	poll::{PollFd, PollFlags, PollTimeout, poll},
	pty::{Winsize, openpty},
	sys::termios::{LocalFlags, Termios, cfgetispeed, cfgetospeed, tcgetattr},
	unistd::ttyname,
};
use omp_ai::{
	Answer, Error as InferenceError, Registry,
	answer::{AnswerBody, ChatStream},
	call::{Call, OpaqueJson, OperationCall},
	event::{BlockKind, ChatEvent, Completion, FinishReason, ToolCall, WorkflowResponse},
	id::ToolCallId,
	layer::{LayerCall, stack::RouteProviderService},
	provider::fake::{FakeProvider, FakeScript},
	receipt::{Cost, ExecutionReceipt, ReasonId, Usage, UsageSource},
	registry::RouteUnavailable,
	session::ConversationSessionPlanner,
};
use omp_app::{
	daemon::{DaemonConfig, DaemonHandle},
	endpoint::LocalEndpoint,
};
use omp_catalog::{
	ManagementCapabilities, OperationBits, OperationKind,
	snapshot::{Catalog, SnapshotProvenance},
};
use omp_core::{Str, sf};
use omp_session::{ComponentRegistry, Session};
use omp_tool::{Claims, Constraint, Effects, Precedence, Presentation, Rev, ToolSpec};
use parking_lot::Mutex;
use serde_json::{Value, json};
use tokio::time;
use tower::Service;

const READY_TIMEOUT: Duration = Duration::from_secs(30);
const CHECKPOINT_TIMEOUT: Duration = Duration::from_secs(30);
const IO_TIMEOUT: Duration = Duration::from_secs(2);
/// The glyph tier every `PtyChild` chat renders in. The chat host infers its
/// charset from the inherited terminal identity (`TERM_PROGRAM`,
/// `KITTY_WINDOW_ID`, ...), so a host running the suite inside Ghostty, kitty,
/// or `WezTerm` would paint Nerd Font glyphs; `PtyChild::spawn` pins this tier
/// through `OMP_TUI_CHARSET` so the screens are identical on every host.
const CHARSET: omp_tui::Charset = omp_tui::Charset::Unicode;
/// `OMP_TUI_CHARSET` spelling of [`CHARSET`].
const CHARSET_ENV: &str = "unicode";
/// The pi-parity composer prompt gutter painted on the input row.
const COMPOSER_PROMPT: &str = "╰─ ";

#[derive(Clone)]
struct GatedRoute {
	fake:            FakeProvider,
	gates:           Arc<Mutex<VecDeque<Receiver<()>>>>,
	captures:        Arc<Mutex<Vec<Call>>>,
	preview_reached: Sender<()>,
	preview_release: Receiver<()>,
}

/// Origin for every timeline mark, so the scenario thread and the provider
/// layer report against one clock.
static TIMELINE: LazyLock<Instant> = LazyLock::new(Instant::now);

/// Records one ordered step of the interrupt scenario on stderr.
///
/// These exist to separate two explanations of a detached job appearing where
/// a cancellation belongs. A long-running tool detaches once it outlives
/// `DispatchPolicy::blocking_limit`, 30s by default, and this scenario sleeps
/// for exactly that long — so the keypress and the blocking limit race. An
/// `escape` marked well before the sixth provider call means the interrupt was
/// delivered and did not stop the tool, which is a defect in the interrupt
/// path. One marked at or after it means the scenario simply lost the race,
/// which is a defect in the scenario.
fn mark(step: &str) {
	eprintln!("[t+{:>6}ms] {step}", TIMELINE.elapsed().as_millis());
}

/// Shortens one rendered value so a panic stays readable.
fn clipped(text: &str, limit: usize) -> String {
	match text.char_indices().nth(limit) {
		Some((cut, _)) => format!("{}…", &text[..cut]),
		None => text.to_owned(),
	}
}

/// One bounded line identifying a provider call.
///
/// `Call::session` is empty at this layer, so the thread itself is what
/// discriminates: the role sequence shows the shape of the conversation the
/// call carries, and the final message shows what prompted it. An extra call
/// whose thread ends in a tool result is a continuation of the turn that
/// issued that tool; one ending in a user or assistant message began a new
/// turn. The whole `Call` is not printed — it would bury both.
fn describe_call(call: &Call) -> String {
	match &call.operation {
		OperationCall::Chat(chat) => {
			let roles = chat
				.messages
				.iter()
				.map(|message| format!("{:?}", message.role))
				.collect::<Vec<_>>()
				.join(",");
			let last = chat
				.messages
				.last()
				.map_or_else(|| "<none>".to_owned(), |message| clipped(&format!("{message:?}"), 240));
			format!(
				"id={:?} Chat messages={} roles=[{roles}] last={last}",
				call.id,
				chat.messages.len()
			)
		},
		other => format!("id={:?} {}", call.id, clipped(&format!("{other:?}"), 160)),
	}
}

impl Service<LayerCall<Call>> for GatedRoute {
	type Error = InferenceError;
	type Response = Answer;

	type Future = impl Future<Output = Result<Answer, InferenceError>> + Send;

	fn poll_ready(&mut self, context: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
		<FakeProvider as Service<Call>>::poll_ready(&mut self.fake, context)
	}

	fn call(&mut self, request: LayerCall<Call>) -> Self::Future {
		mark(&format!("provider call #{}", self.captures.lock().len()));
		let gate = self.gates.lock().pop_front().unwrap_or_else(|| {
			let captures = self.captures.lock();
			let scripted = captures
				.iter()
				.enumerate()
				.map(|(index, call)| format!("  {index}: {}", describe_call(call)))
				.collect::<Vec<_>>()
				.join("\n");
			panic!(
				"the provider was called {} times but the scenario scripts {}.\n\nScripts stand in \
				 for nondeterministic provider output only, so an extra call means production issued \
				 a turn this scenario does not describe. Compare the turn ids: one that repeats the \
				 previous turn is a continuation or retry inside it, while a new turn id means \
				 another turn began.\n\nscripted:\n{scripted}\nunscripted:\n  {}: {}",
				captures.len() + 1,
				captures.len(),
				captures.len(),
				describe_call(&request.payload),
			)
		});
		let call_index = {
			let mut captures = self.captures.lock();
			let index = captures.len();
			captures.push(request.payload.clone());
			index
		};
		let response = <FakeProvider as Service<Call>>::call(&mut self.fake, request.payload);
		let preview_reached = self.preview_reached.clone();
		let preview_release = self.preview_release.clone();
		async move {
			gate
				.recv_async()
				.await
				.expect("scripted provider gate remains open");
			let Answer { meta, receipt, body } = response.await?;
			let body = if call_index == 1 {
				match body {
					AnswerBody::Chat(mut chat) => {
						let events = async_stream::stream! {
							let mut pause_pending = true;
							while let Some(event) = chat.next().await {
								let pause = pause_pending
									&& matches!(&event, Ok(ChatEvent::ToolArgumentsDelta { .. }));
								pause_pending &= !pause;
								yield event;
								if pause {
									preview_reached
										.send_async(())
										.await
										.expect("preview observer remains open");
									preview_release
										.recv_async()
										.await
										.expect("preview release remains open");
								}
							}
						};
						AnswerBody::Chat(ChatStream::ordinary(Box::pin(events)))
					},
					body => body,
				}
			} else {
				body
			};
			Ok(Answer { meta, receipt, body })
		}
	}
}

struct ScriptedGateway {
	_handle:         DaemonHandle,
	model:           String,
	permits:         Vec<Sender<()>>,
	captures:        Arc<Mutex<Vec<Call>>>,
	preview_reached: Receiver<()>,
	preview_release: Sender<()>,
	_responses:      Receiver<WorkflowResponse>,
}

impl ScriptedGateway {
	async fn start(scratch: &Path, socket: &Path, shell_release: &Path) -> Self {
		let scripts = scripts(shell_release);
		Self::start_with_scripts(scratch, socket, scripts).await
	}

	async fn start_with_scripts(scratch: &Path, socket: &Path, scripts: Vec<FakeScript>) -> Self {
		let mut senders = Vec::with_capacity(scripts.len());
		let mut receivers = VecDeque::with_capacity(scripts.len());
		for _ in 0..scripts.len() {
			let (sender, receiver) = flume::bounded(1);
			senders.push(sender);
			receivers.push_back(receiver);
		}
		let captures = Arc::new(Mutex::new(Vec::with_capacity(scripts.len())));
		let (preview_reached_tx, preview_reached) = flume::bounded(1);
		let (preview_release, preview_release_rx) = flume::bounded(1);
		let (registry, sessions, fake, model) = scripted_registry(
			scratch,
			receivers,
			Arc::clone(&captures),
			preview_reached_tx,
			preview_release_rx,
		);
		fake.extend(scripts);

		let mut tools = omp_tool::Registry::new();
		for name in [
			"checkpoint",
			"rewind",
			"ask",
			"ast_edit",
			"ast_grep",
			"bash",
			"debug",
			"edit",
			"eval",
			"glob",
			"grep",
			"hub",
			"lsp",
			"task",
			"think",
			"todo",
			"web_search",
			"write",
			"read",
		] {
			tools
				.register_worker(
					ToolSpec {
						name:            sf!(name),
						rev:             Rev {
							family: if name == "edit" {
								sf!("hl")
							} else {
								Str::default()
							},
							n:      1,
						},
						description:     sf!("P7 gateway executor declaration"),
						schema:          Bytes::from_static(br#"{"type":"object"}"#),
						constraint:      Constraint::None,
						effects:         Effects::empty(),
						confinement:     omp_tool::Confinement::Host,
						projection_code: [0; 32],
					},
					Presentation::Device,
					Claims {
						precedence: Precedence::DEFAULT,
						claimant:   sf!("test/worker"),
						replaces:   None,
					},
				)
				.expect("proof tool registers");
		}
		let (responses, incoming) = flume::bounded(32);
		let handle = time::timeout(
			READY_TIMEOUT,
			DaemonHandle::start_for_test(
				DaemonConfig::local(LocalEndpoint::from(socket.to_path_buf()))
					.with_data_dir(scratch.join("gateway-state")),
				registry,
				sessions,
				Arc::new(tools),
				responses,
			),
		)
		.await
		.expect("gateway startup timed out")
		.expect("scripted gateway starts");
		Self {
			_handle: handle,
			model,
			permits: senders,
			captures,
			preview_reached,
			preview_release,
			_responses: incoming,
		}
	}

	fn release(&self, call: usize) {
		self.permits[call]
			.send(())
			.expect("scripted call gate remains open");
	}

	async fn await_preview(&self) {
		match time::timeout(CHECKPOINT_TIMEOUT, self.preview_reached.recv_async()).await {
			Ok(Ok(())) => {},
			Ok(Err(error)) => panic!("edit preview stream observer closed: {error}"),
			Err(_) => panic!(
				"edit preview stream pause timed out after {} captured provider calls",
				self.captures.lock().len()
			),
		}
	}

	fn release_preview(&self) {
		self
			.preview_release
			.send(())
			.expect("edit preview stream remains paused");
	}
}

fn scripted_registry(
	scratch: &Path,
	gates: VecDeque<Receiver<()>>,
	captures: Arc<Mutex<Vec<Call>>>,
	preview_reached: Sender<()>,
	preview_release: Receiver<()>,
) -> (Registry, ConversationSessionPlanner, FakeProvider, String) {
	let mut compiled = Catalog::embedded().compiled().clone();
	for provider in &mut compiled.providers {
		provider.management = ManagementCapabilities {
			operations:        OperationBits::empty(),
			multiple_accounts: false,
			refresh:           false,
			principal_quota:   false,
		};
	}
	let artifacts = Catalog::encode(compiled, SnapshotProvenance { source_digest: [0; 32] })
		.expect("catalog snapshot");
	let catalog = Arc::new(Catalog::decode(&artifacts.postcard).expect("catalog decode"));
	let model = catalog
		.models()
		.iter()
		.find(|candidate| {
			candidate
				.capabilities
				.operations
				.contains_kind(OperationKind::Chat)
		})
		.expect("chat model");
	let model_key = model.key.as_str().to_owned();
	let route_id = model.routes.first().expect("chat route").clone();
	let route = catalog.route(&route_id).expect("selected route");
	let fake = FakeProvider::new(route.provider.clone(), route_id.clone());
	let route_service = RouteProviderService::new(GatedRoute {
		fake: fake.clone(),
		gates: Arc::new(Mutex::new(gates)),
		captures,
		preview_reached,
		preview_release,
	});
	let mut builder = Registry::builder(catalog.clone());
	for candidate in catalog.routes() {
		builder = if candidate.id == route_id {
			builder
				.register_route(candidate.id.clone(), route_service.clone())
				.expect("scripted route registers")
		} else {
			builder
				.register_unavailable(RouteUnavailable::new(
					candidate.id.clone(),
					ReasonId(sf!("p7-scripted-route-only")),
					None,
				))
				.expect("unavailable route registers")
		};
	}
	let sessions = ConversationSessionPlanner::open(scratch.join("sessions.db"), catalog)
		.expect("conversation store opens");
	(builder.build().expect("base registry"), sessions, fake, model_key)
}

fn tool_script(calls: &[(&str, &str, Value)]) -> FakeScript {
	let mut events = Vec::with_capacity(calls.len() * 3 + 1);
	for (index, (id, name, arguments)) in calls.iter().enumerate() {
		let index = u32::try_from(index).expect("small scripted batch");
		let id = ToolCallId::from(*id);
		events.push(Ok(ChatEvent::ToolCallStarted { index, id: id.clone(), name: Str::from(*name) }));
		events.push(Ok(ChatEvent::ToolArgumentsDelta {
			index,
			bytes: Bytes::from(serde_json::to_vec(arguments).expect("tool args encode")),
		}));
		events.push(Ok(ChatEvent::ToolCallReady {
			index,
			call: ToolCall {
				id,
				name: Str::from(*name),
				arguments: OpaqueJson::new(arguments.clone()),
			},
		}));
	}
	events.push(Ok(completed(FinishReason::ToolCalls, calls.len())));
	FakeScript::chat(events)
}

/// A provider stream whose thinking block is closed implicitly by the
/// following text block, mirroring reasoning-capable providers.
fn thinking_text_script(thinking: &'static str, answer: &'static str) -> FakeScript {
	FakeScript::chat(vec![
		Ok(ChatEvent::BlockStarted { index: 0, kind: BlockKind::Thinking }),
		Ok(ChatEvent::ThinkingDelta { index: 0, text: Str::from(thinking) }),
		Ok(ChatEvent::BlockStarted { index: 1, kind: BlockKind::Text }),
		Ok(ChatEvent::TextDelta { index: 1, text: Str::from(answer) }),
		Ok(completed(FinishReason::Stop, 2)),
	])
}

fn metered_text_script(text: &'static str) -> FakeScript {
	let usage = Usage {
		input_tokens: 4_096,
		output_tokens: 128,
		source: UsageSource::Provider,
		..Usage::default()
	};
	let receipt = ExecutionReceipt {
		usage,
		cost: Cost::from_micro_usd(1_500_000),
		..ExecutionReceipt::default()
	};
	FakeScript::chat(vec![
		Ok(ChatEvent::BlockStarted { index: 0, kind: BlockKind::Text }),
		Ok(ChatEvent::TextDelta { index: 0, text: Str::from(text) }),
		Ok(ChatEvent::Completed(Completion {
			reason: FinishReason::Stop,
			blocks: 1,
			usage,
			receipt: receipt.into(),
		})),
	])
}

fn streaming_edit_script() -> FakeScript {
	let arguments = json!({ "input": "[scratch.txt#5C9F]\nPUT 1.=1:\n+new" });
	let call = ToolCall {
		id:        ToolCallId::from("edit-1"),
		name:      sf!("edit"),
		arguments: OpaqueJson::new(arguments),
	};
	FakeScript::chat(vec![
		Ok(ChatEvent::ToolCallStarted { index: 0, id: call.id.clone(), name: call.name.clone() }),
		Ok(ChatEvent::ToolArgumentsDelta {
			index: 0,
			bytes: Bytes::from_static(br#"{"input":"[scratch.txt#5C9F]\nPUT 1.=1:\n+new""#),
		}),
		Ok(ChatEvent::ToolArgumentsDelta { index: 0, bytes: Bytes::from_static(br"}") }),
		Ok(ChatEvent::ToolCallReady { index: 0, call }),
		Ok(completed(FinishReason::ToolCalls, 1)),
	])
}

fn completed(reason: FinishReason, blocks: usize) -> ChatEvent {
	ChatEvent::Completed(Completion {
		reason,
		blocks: blocks.try_into().unwrap(),
		usage: Usage::default(),
		receipt: ExecutionReceipt::default().into(),
	})
}

fn scripts(_shell_release: &Path) -> Vec<FakeScript> {
	vec![
		tool_script(&[("read-1", "read", json!({ "path": "scratch.txt" }))]),
		streaming_edit_script(),
		tool_script(&[("shell-1", "bash", json!({ "command": "printf 'shell-ok\\n'" }))]),
		metered_text_script("The deterministic tool sequence is complete."),
		tool_script(&[(
			"slow-shell",
			"bash",
			json!({ "command": "printf 'interrupt-ready\\n'; sleep 30" }),
		)]),
	]
}

#[derive(Clone, Debug)]
struct Snapshot {
	text:  String,
	frame: String,
}

impl Snapshot {
	fn combined(&self) -> String {
		format!("{}\n{}", self.text, self.frame)
	}
}

struct DebugClient {
	reader: BufReader<UnixStream>,
	writer: UnixStream,
}

impl DebugClient {
	fn connect(path: &Path, deadline: Instant, process: &mut PtyChild) -> Self {
		loop {
			let problem = match UnixStream::connect(path) {
				Ok(stream) => {
					stream
						.set_read_timeout(Some(IO_TIMEOUT))
						.expect("debug read timeout");
					stream
						.set_write_timeout(Some(IO_TIMEOUT))
						.expect("debug write timeout");
					let writer = stream.try_clone().expect("clone debug socket");
					let mut client = Self { reader: BufReader::new(stream), writer };
					match client.op("info") {
						Ok(_) => return client,
						Err(error) => error,
					}
				},
				Err(error) => error.to_string(),
			};
			if let Some(status) = process
				.child
				.try_wait()
				.expect("poll chat during debug startup")
			{
				let mut stdout = String::new();
				let mut stderr = String::new();
				if let Some(mut pipe) = process.child.stdout.take() {
					pipe.read_to_string(&mut stdout).expect("read early stdout");
				}
				if let Some(mut pipe) = process.child.stderr.take() {
					pipe.read_to_string(&mut stderr).expect("read early stderr");
				}
				panic!(
					"chat exited before debug socket: {status}\nconnect: {problem}\nstdout: \
					 {stdout}\nstderr: {stderr}\nraw PTY:\n{}",
					visible(&process.raw()),
				);
			}
			assert!(
				Instant::now() < deadline,
				"debug socket did not become ready: {problem}\nraw PTY:\n{}",
				visible(&process.raw()),
			);
			thread::sleep(Duration::from_millis(20));
		}
	}

	fn request(&mut self, request: Value) -> Result<Value, String> {
		serde_json::to_writer(&mut self.writer, &request).map_err(|error| error.to_string())?;
		self
			.writer
			.write_all(b"\n")
			.map_err(|error| error.to_string())?;
		self.writer.flush().map_err(|error| error.to_string())?;
		let mut line = String::new();
		self
			.reader
			.read_line(&mut line)
			.map_err(|error| error.to_string())?;
		if line.is_empty() {
			return Err("debug socket closed".to_owned());
		}
		let response: Value = serde_json::from_str(&line).map_err(|error| error.to_string())?;
		if response.get("ok").and_then(Value::as_bool) != Some(true) {
			return Err(format!("debug request {request} failed: {response}"));
		}
		Ok(response)
	}

	fn op(&mut self, op: &'static str) -> Result<Value, String> {
		self.request(json!({ "op": op }))
	}

	fn keys(&mut self, keys: &str) {
		self
			.request(json!({ "op": "keys", "keys": keys }))
			.unwrap_or_else(|error| panic!("key injection failed: {error}"));
	}

	fn snapshot(&mut self) -> Result<Snapshot, String> {
		let text = lines(&self.op("text")?);
		let frame = lines(&self.op("frame")?);
		Ok(Snapshot { text, frame })
	}
}

fn lines(response: &Value) -> String {
	response
		.get("lines")
		.and_then(Value::as_array)
		.into_iter()
		.flatten()
		.filter_map(Value::as_str)
		.collect::<Vec<_>>()
		.join("\n")
}

/// The state directory of every omp process a case starts under `home`.
fn isolated_state_dir(home: &Path) -> PathBuf {
	home.join("state")
}

/// `HOME` and every omp root (configuration, data, state, cache) under the
/// isolated `home`. The explicit `OMP_*` roots win over any inherited
/// `XDG_*` variable, so a process started with these resolves nothing
/// outside `home`.
fn isolated_roots(home: &Path) -> [(&'static str, PathBuf); 5] {
	[
		("HOME", home.to_path_buf()),
		("OMP_CONFIG_DIR", home.join(omp_core::dirs::CONFIG_DIR_NAME)),
		("OMP_DATA_DIR", home.join("data")),
		("OMP_STATE_DIR", isolated_state_dir(home)),
		("OMP_CACHE_DIR", home.join("cache")),
	]
}

struct PtyChild {
	child:      Child,
	master:     fd::OwnedFd,
	slave:      fd::OwnedFd,
	before:     Termios,
	raw:        Arc<Mutex<Vec<u8>>>,
	reader_end: Arc<AtomicBool>,
	reader:     Option<thread::JoinHandle<()>>,
}

impl PtyChild {
	fn spawn(binary: &Path, args: &[String], project: &Path, debug: &Path) -> Self {
		let window = Winsize { ws_row: 48, ws_col: 120, ws_xpixel: 0, ws_ypixel: 0 };
		let pty = openpty(Some(&window), None).expect("open PTY");
		let device = ttyname(&pty.slave).expect("PTY slave path");
		let before = tcgetattr(&pty.slave).expect("initial PTY termios");
		fcntl(&pty.master, FcntlArg::F_SETFL(OFlag::O_NONBLOCK)).expect("nonblocking PTY master");
		let reader_fd = pty.master.try_clone().expect("clone PTY master");
		let raw = Arc::new(Mutex::new(Vec::new()));
		let reader_raw = raw.clone();
		let reader_end = Arc::new(AtomicBool::new(false));
		let reader_stop = reader_end.clone();
		// Block in poll(2) until the master is readable instead of sleeping on EAGAIN:
		// a sleeping reader drains one small kernel buffer per wake, which on macOS
		// (tiny PTY buffers, loaded hosted runners) throttles the host's blocking
		// terminal writes to tens of KiB/s. The bounded wait exists only so the
		// reader observes `reader_end`.
		let reader = thread::spawn(move || {
			let mut buffer = [0_u8; 16 * 1024];
			loop {
				let mut ready = [PollFd::new(reader_fd.as_fd(), PollFlags::POLLIN)];
				match poll(&mut ready, PollTimeout::from(50_u16)) {
					Ok(_) | Err(Errno::EINTR) => {},
					Err(error) => panic!("PTY poll failed: {error}"),
				}
				// Drain everything available before polling again.
				loop {
					match nix::unistd::read(&reader_fd, &mut buffer) {
						Ok(0) if reader_stop.load(Ordering::Acquire) => return,
						Ok(0) => {
							thread::sleep(Duration::from_millis(5));
							break;
						},
						Ok(count) => reader_raw.lock().extend_from_slice(&buffer[..count]),
						Err(Errno::EAGAIN) => break,
						Err(Errno::EINTR) => {},
						Err(Errno::EIO) => return,
						Err(error) => panic!("PTY read failed: {error}"),
					}
				}
				if reader_stop.load(Ordering::Acquire) {
					return;
				}
			}
		});

		let home = project.parent().expect("project has parent").join("home");
		fs::create_dir_all(&home).expect("create isolated home");
		// Every omp root is pinned under the isolated home, and `OMP_LOG` is
		// cleared, so nothing in the developer's or runner's environment (an
		// `OMP_CONFIG_DIR` turning the sandbox off, an `XDG_STATE_HOME` sharing
		// a log directory, an `OMP_LOG=off`) reaches chat or the daemon it
		// spawns, and chat logs at its default filter.
		let child = Command::new(binary)
			.args(args)
			.current_dir(project)
			.env("TERM", "xterm-256color")
			.envs(isolated_roots(&home))
			.env_remove("OMP_LOG")
			.env("OMP_TTY", &device)
			.env("OMP_TUI_CHARSET", CHARSET_ENV)
			.env("OMP_TUI_DEBUG", debug)
			.env("NO_COLOR", "1")
			.stdout(Stdio::piped())
			.stderr(Stdio::piped())
			.spawn()
			.expect("spawn omp chat");
		Self {
			child,
			master: pty.master,
			slave: pty.slave,
			before,
			raw,
			reader_end,
			reader: Some(reader),
		}
	}

	fn resize(&self, rows: u16, cols: u16) {
		let window = libc::winsize { ws_row: rows, ws_col: cols, ws_xpixel: 0, ws_ypixel: 0 };
		// SAFETY: master is a live PTY and window is a valid winsize value.
		let result =
			unsafe { libc::ioctl(self.master.as_fd().as_raw_fd(), libc::TIOCSWINSZ, &window) };
		assert_eq!(result, 0, "TIOCSWINSZ failed: {}", io::Error::last_os_error());
	}

	fn raw(&self) -> Vec<u8> {
		self.raw.lock().clone()
	}

	fn wait(mut self, timeout: Duration) -> (process::ExitStatus, Vec<u8>, String, String, Termios) {
		let deadline = Instant::now() + timeout;
		let status = loop {
			match self.child.try_wait().expect("poll omp chat") {
				Some(status) => break status,
				None if Instant::now() < deadline => thread::sleep(Duration::from_millis(20)),
				None => {
					let raw = visible(&self.raw());
					let _ = self.child.kill();
					panic!("omp chat did not exit in {timeout:?}; raw PTY:\n{raw}");
				},
			}
		};
		self.reader_end.store(true, Ordering::Release);
		if let Some(reader) = self.reader.take() {
			reader.join().expect("PTY reader joins");
		}
		let mut stdout = String::new();
		let mut stderr = String::new();
		if let Some(mut pipe) = self.child.stdout.take() {
			pipe.read_to_string(&mut stdout).expect("read child stdout");
		}
		if let Some(mut pipe) = self.child.stderr.take() {
			pipe.read_to_string(&mut stderr).expect("read child stderr");
		}
		let after = tcgetattr(&self.slave).expect("final PTY termios");
		(status, self.raw(), stdout, stderr, after)
	}
}

/// Kills a chat the proof never saw exit, so a failing case leaves no chat
/// running, and with it no project daemon: a daemon chat spawned idles out
/// once chat's connection closes (`--envd-idle-timeout`).
impl Drop for PtyChild {
	fn drop(&mut self) {
		if matches!(self.child.try_wait(), Ok(None)) {
			let _ = self.child.kill();
			let _ = self.child.wait();
		}
		self.reader_end.store(true, Ordering::Release);
		if let Some(reader) = self.reader.take() {
			let _ = reader.join();
		}
	}
}

fn wait_snapshot(
	debug: &mut DebugClient,
	raw: &Arc<Mutex<Vec<u8>>>,
	label: &str,
	mut ready: impl FnMut(&Snapshot) -> bool,
) -> Snapshot {
	let started = Instant::now();
	let deadline = started + CHECKPOINT_TIMEOUT;
	let mut last = None;
	let mut error = None;
	loop {
		match debug.snapshot() {
			Ok(snapshot) if ready(&snapshot) => return snapshot,
			Ok(snapshot) => last = Some(snapshot),
			Err(problem) => error = Some(problem),
		}
		if Instant::now() >= deadline {
			let snapshot = last.map_or_else(|| "<none>".to_owned(), |value| format!("{value:#?}"));
			// Geometry the host last published (cols, rows, window_top, doc_height) and how
			// much terminal output the harness has drained: tells a host that never
			// repainted at the new size from one that painted without the expected rows.
			let info = debug
				.op("info")
				.map_or_else(|problem| format!("<info failed: {problem}>"), |value| value.to_string());
			let drained = raw.lock().len();
			panic!(
				"checkpoint {label:?} timed out after {:?}\nlast error: {error:?}\ninfo: {info}\nPTY \
				 bytes drained: {drained}\nlast snapshot:\n{snapshot}\nraw PTY:\n{}",
				started.elapsed(),
				visible(&raw.lock()),
			);
		}
		thread::sleep(Duration::from_millis(15));
	}
}

fn wait_info(debug: &mut DebugClient, label: &str, mut ready: impl FnMut(&Value) -> bool) -> Value {
	let deadline = Instant::now() + CHECKPOINT_TIMEOUT;
	loop {
		let info = debug
			.op("info")
			.unwrap_or_else(|error| panic!("{label}: {error}"));
		if ready(&info) {
			return info;
		}
		assert!(Instant::now() < deadline, "checkpoint {label:?} timed out: {info}");
		thread::sleep(Duration::from_millis(15));
	}
}

fn assert_surface(snapshot: &Snapshot, label: &str) {
	assert!(!snapshot.text.trim().is_empty(), "{label}: published terminal surface is empty");
}

fn visible(bytes: &[u8]) -> String {
	let mut out = String::new();
	for &byte in &bytes[bytes.len().saturating_sub(96 * 1024)..] {
		match byte {
			b'\n' => out.push('\n'),
			b'\r' => out.push_str("\\r"),
			b'\t' => out.push_str("\\t"),
			0x20..=0x7e => out.push(char::from(byte)),
			_ => write!(out, "\\x{byte:02x}").expect("writing to String cannot fail"),
		}
	}
	out
}

/// Creates one authoritative resumable `.oms` journal.
fn seed_session(path: &Path) {
	Session::create(path, ComponentRegistry::standard()).expect("create resumable TUI session");
}

fn journal(path: &Path) -> String {
	fs::read_to_string(path).expect("read session journal")
}

/// Yields each SSE frame in a session journal.
fn journal_frames(text: &str) -> impl Iterator<Item = &str> {
	text.split("\n\n").filter(|frame| frame.contains("event: "))
}

/// Returns one frame's field value, if the frame carries it.
fn frame_field<'f>(frame: &'f str, prefix: &str) -> Option<&'f str> {
	frame.lines().find_map(|line| line.strip_prefix(prefix))
}

/// Returns the journal entry id of the tool call the script issued under
/// `call_id`.
///
/// Every scripted call carries its own id, so anchoring on it names one
/// execution outright — no dependence on journal ordering, and no risk of
/// matching a sibling call of the same tool.
fn tool_call_id<'j>(text: &'j str, call_id: &str) -> Option<&'j str> {
	let scripted = format!("\"call_id\":\"{call_id}\"");
	journal_frames(text)
		.filter(|frame| frame.contains("event: tool.call@1"))
		.find(|frame| frame_field(frame, "data: ").is_some_and(|data| data.contains(&scripted)))
		.and_then(|frame| frame_field(frame, "id: "))
}

/// Returns whether the journal records a non-pty execution for `call`.
///
/// `Session::call_update` writes these updates parented to their tool call, so
/// a match is evidence the execution reached the journal through the
/// production lifecycle. Scoping by `by:` is what ties the record to one
/// execution: every bash call emits `terminal`, so an unscoped search answers
/// for whichever ran first.
fn records_non_terminal(text: &str, call: &str) -> bool {
	journal_frames(text)
		.filter(|frame| frame.contains("event: tool.update@1"))
		.filter(|frame| frame_field(frame, "by: ") == Some(call))
		.any(|frame| {
			frame_field(frame, "data: ").is_some_and(|data| data.contains("\"terminal\":false"))
		})
}

fn assert_journal_chain(text: &str) {
	let frames = text
		.split("\n\n")
		.filter(|frame| frame.contains("event:"))
		.collect::<Vec<_>>();
	assert!(!frames.is_empty(), "journal has no SSE frames");
	for frame in frames.iter().skip(1) {
		assert!(
			frame.lines().any(|line| line.starts_with("by: ")),
			"non-genesis frame has no by: {frame}"
		);
	}
}

fn assert_restored(raw: &[u8], before: &Termios, after: &Termios, diagnostics: &str) {
	let alt_enter = raw.windows(8).rposition(|window| window == b"\x1b[?1049h");
	let alt_exit = raw.windows(8).rposition(|window| window == b"\x1b[?1049l");
	assert!(
		alt_enter.is_none() || alt_exit.is_some_and(|exit| Some(exit) > alt_enter),
		"alternate buffer was not restored; enter={alt_enter:?} exit={alt_exit:?}\n{diagnostics}"
	);
	for sequence in ["\x1b[?1047h", "\x1b[?47h"] {
		assert!(
			!raw
				.windows(sequence.len())
				.any(|window| window == sequence.as_bytes()),
			"legacy alternate-buffer entry {sequence:?} observed\n{diagnostics}"
		);
	}
	for mode in [1000, 1002, 1003, 1006] {
		let enable = format!("\x1b[?{mode}h");
		let disable = format!("\x1b[?{mode}l");
		let enabled = raw
			.windows(enable.len())
			.rposition(|window| window == enable.as_bytes());
		let disabled = raw
			.windows(disable.len())
			.rposition(|window| window == disable.as_bytes());
		assert!(
			enabled.is_none() || disabled.is_some_and(|exit| Some(exit) > enabled),
			"mouse tracking mode {mode} was not restored; enable={enabled:?} \
			 disable={disabled:?}\n{diagnostics}"
		);
	}
	let hide = raw.windows(6).rposition(|window| window == b"\x1b[?25l");
	let show = raw.windows(6).rposition(|window| window == b"\x1b[?25h");
	assert!(
		show.is_some() && hide.is_none_or(|hidden| show > Some(hidden)),
		"cursor was not restored; hide={hide:?} show={show:?}\n{diagnostics}"
	);
	assert_eq!(after.input_flags, before.input_flags, "input flags not restored\n{diagnostics}");
	assert_eq!(after.output_flags, before.output_flags, "output flags not restored\n{diagnostics}");
	assert_eq!(
		after.control_flags, before.control_flags,
		"control flags not restored\n{diagnostics}"
	);
	// PENDIN is tty state, not a mode: XNU sets it whenever tcsetattr turns
	// ICANON back on, keeps it across every later tcsetattr, and clears it
	// only on the next read, input byte or flush. A chat that restores the
	// original termios exactly still reads back PENDIN on macOS.
	assert_eq!(
		after.local_flags - LocalFlags::PENDIN,
		before.local_flags - LocalFlags::PENDIN,
		"local flags not restored\n{diagnostics}"
	);
	assert_eq!(
		after.control_chars, before.control_chars,
		"control characters not restored\n{diagnostics}"
	);
	assert_eq!(
		cfgetispeed(after),
		cfgetispeed(before),
		"input baud rate not restored\n{diagnostics}"
	);
	assert_eq!(
		cfgetospeed(after),
		cfgetospeed(before),
		"output baud rate not restored\n{diagnostics}"
	);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn chat_tui_drives_real_pty_tools_interrupt_resize_and_clean_quit() {
	use std::os::unix::fs::PermissionsExt;
	omp_e2e::support::install_omp_binary_env().expect("install Cargo-built omp binary");
	let scratch = tempfile::tempdir().expect("scratch root");
	fs::set_permissions(scratch.path(), <fs::Permissions>::from_mode(0o700))
		.expect("secure scratch root");
	let project = scratch.path().join("project");
	fs::create_dir(&project).expect("project directory");
	let project = fs::canonicalize(&project).expect("canonical project root");
	fs::write(project.join("scratch.txt"), "old\n").expect("write read/edit fixture");
	let metadata_dir = project.join(".omp");
	fs::create_dir(&metadata_dir).expect("project metadata directory");
	fs::set_permissions(&metadata_dir, <fs::Permissions>::from_mode(0o755))
		.expect("use standard project metadata permissions");

	let shell_release = scratch.path().join("unused-shell-release");
	let gateway_socket = scratch.path().join("gateway.sock");
	let debug_socket = scratch.path().join("tui-debug.sock");
	let gateway = ScriptedGateway::start(scratch.path(), &gateway_socket, &shell_release).await;
	let session_path = scratch.path().join("p7-tools.oms");
	seed_session(&session_path);
	gateway.release(0);

	let binary = omp_e2e::support::omp_binary().expect("locate omp binary");
	let args = vec![
		"chat".to_owned(),
		"--model".to_owned(),
		gateway.model.clone(),
		"--project".to_owned(),
		project.display().to_string(),
		"--gateway".to_owned(),
		gateway_socket.display().to_string(),
		"--session".to_owned(),
		session_path.display().to_string(),
		"--envd-idle-timeout".to_owned(),
		"2".to_owned(),
		// Scripted `bash` calls must run on every host, with or without an OS
		// sandbox. An explicit `yolo` is respected unconfined; the default one
		// would prompt where the sandbox cannot be built (a Linux runner
		// without bubblewrap), and nobody answers a prompt in this proof.
		"--approval-mode".to_owned(),
		"yolo".to_owned(),
	];
	let mut process = PtyChild::spawn(&binary, &args, &project, &debug_socket);
	let raw_capture = process.raw.clone();
	let mut debug =
		DebugClient::connect(&debug_socket, Instant::now() + READY_TIMEOUT, &mut process);
	let ready = wait_snapshot(&mut debug, &raw_capture, "chat shell ready", |snapshot| {
		let surface = snapshot.combined();
		surface.contains("Welcome back!")
			&& surface.contains("omp v")
			&& surface.contains(&gateway.model)
			&& surface.contains(project.to_string_lossy().as_ref())
			&& surface.contains("turn 0 · 0 in / 0 out")
			&& surface.contains(COMPOSER_PROMPT)
	});
	assert_surface(&ready, "ready");

	debug.keys("'exercise deterministic tools' enter");
	let read = wait_snapshot(&mut debug, &raw_capture, "read card settled", |snapshot| {
		let surface = snapshot.combined();
		surface.contains("Read scratch.txt") && surface.contains("old")
	});
	assert_surface(&read, "read card");
	let first_journal = journal(&session_path);
	assert!(first_journal.contains("event: tool.call@1"), "read call absent from journal");
	assert!(first_journal.contains("event: tool.result@1"), "read result absent from journal");
	assert_journal_chain(&first_journal);

	gateway.release(1);
	gateway.await_preview().await;
	let preview = wait_snapshot(&mut debug, &raw_capture, "edit live preview", |snapshot| {
		let surface = snapshot.combined();
		surface.contains("edit arguments")
			&& fs::read_to_string(project.join("scratch.txt")).is_ok_and(|text| text == "old\n")
	});
	assert_surface(&preview, "edit preview");
	gateway.release_preview();
	let final_edit = wait_snapshot(&mut debug, &raw_capture, "edit card settled", |snapshot| {
		let surface = snapshot.combined();
		surface.contains("Edit:")
			&& surface.contains("scratch.txt")
			&& fs::read_to_string(project.join("scratch.txt")).is_ok_and(|text| text == "new\n")
			&& journal(&session_path)
				.matches("event: tool.result@1")
				.count() >= 2
	});
	assert_surface(&final_edit, "edit final");
	let edit_journal = journal(&session_path);
	assert!(edit_journal.matches("event: tool.call@1").count() >= 2);
	assert!(edit_journal.matches("event: tool.result@1").count() >= 2);
	assert_journal_chain(&edit_journal);

	gateway.release(2);
	let shell = wait_snapshot(&mut debug, &raw_capture, "bash card settled", |snapshot| {
		let surface = snapshot.combined();
		surface.contains("$ printf 'shell-ok")
			&& surface.contains("shell-ok")
			&& journal(&session_path)
				.matches("event: tool.result@1")
				.count() >= 3
	});
	assert_surface(&shell, "bash final");
	let shell_journal = journal(&session_path);
	assert!(shell_journal.matches("event: tool.call@1").count() >= 3);
	assert!(shell_journal.matches("event: tool.result@1").count() >= 3);
	assert_journal_chain(&shell_journal);

	gateway.release(3);
	let summary = wait_snapshot(&mut debug, &raw_capture, "tool turn complete", |snapshot| {
		let surface = snapshot.combined();
		surface.contains("The deterministic tool sequence is complete.")
			&& surface.contains("turn 1")
			&& surface.contains("4096 in / 128 out")
	});
	assert_surface(&summary, "tool summary");

	debug.keys("'interrupt the next tool' enter");
	mark("released the interruptible bash script");
	gateway.release(4);
	// `shell::Update` carries `terminal`, but the dispatcher blanks its `data`
	// and `project_update` drops that shape so the bounded output stream stays
	// the single authority for ordered bytes. The field therefore never reaches
	// a rendered surface; the journal is where production records it. Liveness
	// is still asserted on the card, and the non-pty guarantee now reads the
	// record production actually writes, tied to the execution that made it.
	let running = wait_snapshot(&mut debug, &raw_capture, "interruptible bash live", |snapshot| {
		let text = journal(&session_path);
		snapshot.combined().contains("bash running")
			&& tool_call_id(&text, "slow-shell").is_some_and(|call| records_non_terminal(&text, call))
	});
	assert_surface(&running, "interruptible bash");
	mark("bash is live and journalled");
	let live_journal = journal(&session_path);
	let call = tool_call_id(&live_journal, "slow-shell").expect("the interruptible bash tool call");
	assert!(
		records_non_terminal(&live_journal, call),
		"the interruptible bash call must record a non-pty execution\n{live_journal}"
	);

	mark("sending resize");
	process.resize(32, 92);
	debug
		.op("resize")
		.unwrap_or_else(|error| panic!("resize injection failed: {error}"));
	// At 32 rows the settled cards retire into native scrollback; the live
	// card, band, and composer must survive the rebuild.
	let resized = wait_snapshot(&mut debug, &raw_capture, "streaming resize", |snapshot| {
		let surface = snapshot.combined();
		surface.contains("sleep 30")
			&& surface.contains("interrupt the next tool")
			&& surface.contains(COMPOSER_PROMPT)
	});
	assert_surface(&resized, "resized");
	let info = wait_info(&mut debug, "settled streaming resize", |info| {
		info.get("rows").and_then(Value::as_u64) == Some(32)
			&& info.get("cols").and_then(Value::as_u64) == Some(92)
	});
	assert_eq!(info.get("rows").and_then(Value::as_u64), Some(32), "resize rows: {info}");
	assert_eq!(info.get("cols").and_then(Value::as_u64), Some(92), "resize cols: {info}");
	mark("resize settled");

	// Escape, not `ctrl+c`: `omp_chat::ctrl_c_action` resolves a first `C-c`
	// press to `Clear` and only a repeat within 500ms to `Quit`, so it never
	// reaches the turn. Escape is the interrupt rung the chat host routes to
	// `HostCommand::Interrupt`, which is the path ADR 0011 makes this scenario
	// prove.
	mark("sending escape");
	debug.keys("escape");
	mark("escape sent");
	let interrupted =
		wait_snapshot(&mut debug, &raw_capture, "turn interrupted and responsive", |snapshot| {
			let surface = snapshot.combined();
			surface.contains(COMPOSER_PROMPT)
				&& journal(&session_path)
					.matches("event: tool.result@1")
					.count() >= 4
		});
	assert_surface(&interrupted, "interrupt");
	let interrupted_journal = journal(&session_path);
	assert!(interrupted_journal.matches("event: tool.call@1").count() >= 4);
	assert!(interrupted_journal.matches("event: tool.result@1").count() >= 4);
	assert!(interrupted_journal.contains("event: msg.assistant.end@1"));
	assert_journal_chain(&interrupted_journal);

	// One `C-c` only arms exit; the repeat inside the 500ms window quits.
	debug.keys("ctrl+c ctrl+c");
	drop(debug);
	let before = process.before.clone();
	let (status, raw, stdout, stderr, after) = process.wait(READY_TIMEOUT);
	let diagnostics = format!(
		"status={status}\nstdout={stdout}\nstderr={stderr}\nlast frame={}\nraw={}",
		interrupted.frame,
		visible(&raw),
	);
	assert!(status.success(), "omp chat did not exit cleanly\n{diagnostics}");
	assert_restored(&raw, &before, &after, &diagnostics);
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn chat_tui_persists_thinking_blocks_across_turns_and_resume() {
	use std::os::unix::fs::PermissionsExt;
	omp_e2e::support::install_omp_binary_env().expect("install Cargo-built omp binary");
	let scratch = tempfile::tempdir().expect("scratch root");
	fs::set_permissions(scratch.path(), <fs::Permissions>::from_mode(0o700))
		.expect("secure scratch root");
	let project = scratch.path().join("project");
	fs::create_dir(&project).expect("project directory");
	let project = fs::canonicalize(&project).expect("canonical project root");
	let metadata_dir = project.join(".omp");
	fs::create_dir(&metadata_dir).expect("project metadata directory");
	fs::set_permissions(&metadata_dir, <fs::Permissions>::from_mode(0o755))
		.expect("use standard project metadata permissions");
	let gateway_socket = scratch.path().join("gateway.sock");
	let debug_socket = scratch.path().join("tui-debug.sock");
	let gateway = ScriptedGateway::start_with_scripts(scratch.path(), &gateway_socket, vec![
		thinking_text_script(
			"Weighing the first request.\nThe deterministic option is safest.",
			"First answer settled.",
		),
		thinking_text_script("Second deliberation paragraph.", "Second answer settled."),
	])
	.await;
	gateway.release(0);
	gateway.release(1);

	let sessions_dir = scratch.path().join("sessions");
	fs::create_dir(&sessions_dir).expect("session directory");
	let session_path = sessions_dir.join("thinking.oms");
	seed_session(&session_path);
	let binary = omp_e2e::support::omp_binary().expect("locate omp binary");
	let base_args = vec![
		"chat".to_owned(),
		"--model".to_owned(),
		gateway.model.clone(),
		"--project".to_owned(),
		project.display().to_string(),
		"--gateway".to_owned(),
		gateway_socket.display().to_string(),
		"--session-dir".to_owned(),
		sessions_dir.display().to_string(),
		"--envd-idle-timeout".to_owned(),
		"2".to_owned(),
	];
	let mut args = base_args.clone();
	args.extend(["--session".to_owned(), session_path.display().to_string()]);
	let mut process = PtyChild::spawn(&binary, &args, &project, &debug_socket);
	let raw_capture = process.raw.clone();
	let mut debug =
		DebugClient::connect(&debug_socket, Instant::now() + READY_TIMEOUT, &mut process);
	let ready = wait_snapshot(&mut debug, &raw_capture, "chat shell ready", |snapshot| {
		let surface = snapshot.combined();
		surface.contains("Welcome back!")
			&& surface.contains("omp v")
			&& surface.contains(&gateway.model)
			&& surface.contains(project.to_string_lossy().as_ref())
			&& surface.contains("turn 0 · 0 in / 0 out")
			&& surface.contains(COMPOSER_PROMPT)
	});
	assert_surface(&ready, "ready");

	debug.keys("'first prompt' enter");
	let first = wait_snapshot(&mut debug, &raw_capture, "first turn keeps thinking", |snapshot| {
		snapshot.frame.contains("First answer settled.")
			&& snapshot.frame.contains("Weighing the first request")
	});
	assert_surface(&first, "first turn");
	let first_journal = journal(&session_path);
	assert!(first_journal.contains("event: msg.assistant.end@1"));
	assert_journal_chain(&first_journal);

	debug.keys("ctrl+t");
	let hidden = wait_snapshot(&mut debug, &raw_capture, "ctrl+t hides thinking", |snapshot| {
		snapshot.frame.contains("First answer settled.")
			&& !snapshot.frame.contains("Weighing the first request")
	});
	assert_surface(&hidden, "hidden thinking");
	assert_eq!(
		journal(&session_path),
		first_journal,
		"visibility toggle changed the session DOM journal"
	);
	debug.keys("ctrl+t");
	wait_snapshot(&mut debug, &raw_capture, "ctrl+t restores thinking", |snapshot| {
		snapshot.frame.contains("Weighing the first request")
	});
	assert_eq!(
		journal(&session_path),
		first_journal,
		"restoring visibility changed the session DOM journal"
	);

	debug.keys("'second prompt' enter");
	let second = wait_snapshot(&mut debug, &raw_capture, "second turn keeps history", |snapshot| {
		let surface = snapshot.combined();
		surface.contains("Second answer settled.")
			&& surface.contains("Second deliberation paragraph.")
			&& surface.contains("First answer settled.")
	});
	assert_surface(&second, "second turn");
	let second_journal = journal(&session_path);
	assert!(second_journal.matches("event: msg.assistant.end@1").count() >= 2);
	assert_journal_chain(&second_journal);

	// Ctrl+C on an idle composer arms exit; a second press within the window
	// quits.
	debug.keys("ctrl+c ctrl+c");
	drop(debug);
	let before = process.before.clone();
	let (status, raw, stdout, stderr, after) = process.wait(READY_TIMEOUT);
	let diagnostics =
		format!("status={status}\nstdout={stdout}\nstderr={stderr}\nraw={}", visible(&raw));
	assert!(status.success(), "omp chat did not exit cleanly\n{diagnostics}");
	assert_restored(&raw, &before, &after, &diagnostics);

	let resume_socket = scratch.path().join("resume-tui-debug.sock");
	let mut resume_args = base_args;
	resume_args.push("-c".to_owned());
	let mut resumed = PtyChild::spawn(&binary, &resume_args, &project, &resume_socket);
	let resumed_raw = resumed.raw.clone();
	let mut resume_debug =
		DebugClient::connect(&resume_socket, Instant::now() + READY_TIMEOUT, &mut resumed);
	let rehydrated = wait_snapshot(
		&mut resume_debug,
		&resumed_raw,
		"resumed transcript keeps thinking bodies",
		|snapshot| {
			let all = snapshot.combined();
			all.contains("First answer settled.")
				&& all.contains("Second answer settled.")
				&& all.contains("Weighing the first request")
				&& all.contains("Second deliberation paragraph.")
		},
	);
	assert_surface(&rehydrated, "resumed thinking transcript");
	let resumed_journal = journal(&session_path);
	assert!(
		resumed_journal.starts_with(&second_journal),
		"resume did not preserve the authoritative journal prefix"
	);
	assert_journal_chain(&resumed_journal);

	resume_debug.keys("ctrl+c ctrl+c");
	drop(resume_debug);
	let resumed_before = resumed.before.clone();
	let (resumed_status, resumed_bytes, resumed_stdout, resumed_stderr, resumed_after) =
		resumed.wait(READY_TIMEOUT);
	let resumed_diagnostics = format!(
		"status={resumed_status}\nstdout={resumed_stdout}\nstderr={resumed_stderr}\nraw={}",
		visible(&resumed_bytes)
	);
	assert!(
		resumed_status.success(),
		"resumed omp chat did not exit cleanly\n{resumed_diagnostics}"
	);
	assert_restored(&resumed_bytes, &resumed_before, &resumed_after, &resumed_diagnostics);
}

/// The text-only response `text` in one block.
fn text_script(text: &'static str) -> FakeScript {
	FakeScript::chat(vec![
		Ok(ChatEvent::BlockStarted { index: 0, kind: BlockKind::Text }),
		Ok(ChatEvent::TextDelta { index: 0, text: Str::from(text) }),
		Ok(completed(FinishReason::Stop, 1)),
	])
}

/// ADR 0038 surfaces on a real PTY: a project stream rule interrupts the
/// first response, the TUI paints the redirect notice card and the
/// resampled answer, both survive a resize, and quitting restores the
/// terminal.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn chat_tui_renders_a_stream_rule_redirect_through_resize_and_clean_quit() {
	use std::os::unix::fs::PermissionsExt;
	omp_e2e::support::install_omp_binary_env().expect("install Cargo-built omp binary");
	let scratch = tempfile::tempdir().expect("scratch root");
	fs::set_permissions(scratch.path(), <fs::Permissions>::from_mode(0o700))
		.expect("secure scratch root");
	let project = scratch.path().join("project");
	fs::create_dir(&project).expect("project directory");
	let project = fs::canonicalize(&project).expect("canonical project root");
	let metadata_dir = project.join(".omp");
	fs::create_dir(&metadata_dir).expect("project metadata directory");
	fs::set_permissions(&metadata_dir, <fs::Permissions>::from_mode(0o755))
		.expect("use standard project metadata permissions");
	write_fixture(
		&metadata_dir.join("rules/no-forbidden.md"),
		"---\ncondition: forbidden\nscope: text\n---\nNever use the forbidden word.\n",
	);
	let gateway_socket = scratch.path().join("gateway.sock");
	let debug_socket = scratch.path().join("tui-debug.sock");
	let gateway = ScriptedGateway::start_with_scripts(scratch.path(), &gateway_socket, vec![
		text_script("Here is the forbidden word you asked about."),
		text_script("Clean answer after the rule."),
	])
	.await;
	gateway.release(0);
	gateway.release(1);

	let sessions_dir = scratch.path().join("sessions");
	fs::create_dir(&sessions_dir).expect("session directory");
	let session_path = sessions_dir.join("stream-rule.oms");
	seed_session(&session_path);
	let binary = omp_e2e::support::omp_binary().expect("locate omp binary");
	let args = vec![
		"chat".to_owned(),
		"--model".to_owned(),
		gateway.model,
		"--project".to_owned(),
		project.display().to_string(),
		"--gateway".to_owned(),
		gateway_socket.display().to_string(),
		"--session-dir".to_owned(),
		sessions_dir.display().to_string(),
		"--envd-idle-timeout".to_owned(),
		"2".to_owned(),
		"--session".to_owned(),
		session_path.display().to_string(),
	];
	let mut process = PtyChild::spawn(&binary, &args, &project, &debug_socket);
	let raw_capture = process.raw.clone();
	let mut debug =
		DebugClient::connect(&debug_socket, Instant::now() + READY_TIMEOUT, &mut process);
	let ready = wait_snapshot(&mut debug, &raw_capture, "chat shell ready", |snapshot| {
		snapshot.combined().contains(COMPOSER_PROMPT)
	});
	assert_surface(&ready, "ready");

	const NOTICE: &str =
		"Stream rule no-forbidden matched the text output; the response was redirected.";
	debug.keys("'use the word' enter");
	let redirected = wait_snapshot(&mut debug, &raw_capture, "stream rule redirect", |snapshot| {
		let surface = snapshot.combined();
		surface.contains(NOTICE) && surface.contains("Clean answer after the rule.")
	});
	assert_surface(&redirected, "redirected");
	let card = redirected
		.combined()
		.lines()
		.find(|line| line.contains(NOTICE))
		.map(str::to_owned)
		.expect("notice row");
	assert!(card.contains('⚖'), "the stream-rule card leads with the themed rule glyph: {card:?}");
	let session_journal = journal(&session_path);
	assert!(session_journal.contains("director.stream-interrupt"), "{session_journal}");
	assert_journal_chain(&session_journal);
	mark("stream rule redirect painted");

	process.resize(30, 96);
	debug
		.op("resize")
		.unwrap_or_else(|error| panic!("resize injection failed: {error}"));
	let resized = wait_snapshot(&mut debug, &raw_capture, "redirect survives resize", |snapshot| {
		let surface = snapshot.combined();
		surface.contains(NOTICE)
			&& surface.contains("Clean answer after the rule.")
			&& surface.contains(COMPOSER_PROMPT)
	});
	assert_surface(&resized, "resized");
	let info = wait_info(&mut debug, "settled resize", |info| {
		info.get("rows").and_then(Value::as_u64) == Some(30)
			&& info.get("cols").and_then(Value::as_u64) == Some(96)
	});
	assert_eq!(info.get("cols").and_then(Value::as_u64), Some(96), "resize cols: {info}");
	mark("resize settled");

	debug.keys("ctrl+c ctrl+c");
	drop(debug);
	let before = process.before.clone();
	let (status, raw, stdout, stderr, after) = process.wait(READY_TIMEOUT);
	let diagnostics =
		format!("status={status}\nstdout={stdout}\nstderr={stderr}\nraw={}", visible(&raw));
	assert!(status.success(), "omp chat did not exit cleanly\n{diagnostics}");
	assert_restored(&raw, &before, &after, &diagnostics);
}

/// Writes `body` to `path`, creating its parent directories.
fn write_fixture(path: &Path, body: &str) {
	fs::create_dir_all(path.parent().expect("fixture has a parent")).expect("fixture directory");
	fs::write(path, body).expect("write fixture");
}

/// Installs each `(id, root)` marketplace plugin, enabled at user scope, in
/// the plugin registry under `data`, the way `omp ext install` records one.
fn install_plugins(data: &Path, plugins: &[(&'static str, &Path)]) {
	use omp_ext::claude_plugin::{
		InstallScope, InstalledPluginEntry, InstalledPluginsRegistry, REGISTRY_FILE,
	};
	let mut registry = InstalledPluginsRegistry::default();
	for (id, root) in plugins {
		registry
			.plugins
			.insert(Str::new_static(id), vec![InstalledPluginEntry {
				scope:          InstallScope::User,
				install_path:   root.to_path_buf(),
				version:        Str::new_static("1.0.0"),
				installed_at:   Str::new_static("2026-01-01T00:00:00Z"),
				last_updated:   Str::new_static("2026-01-01T00:00:00Z"),
				git_commit_sha: None,
				enabled:        true,
			}]);
	}
	write_fixture(
		&data.join("plugins").join(REGISTRY_FILE),
		&serde_json::to_string(&registry).expect("plugin registry encodes"),
	);
}

/// The approval state `omp ext trust <plugin> --show` reports for the
/// command it leads with `label`, run against the chat's own home,
/// configuration and data directories, and project.
async fn trust_status(binary: &Path, home: &Path, project: &Path, label: &str) -> &'static str {
	let plugin = label.split(' ').next().expect("label names its plugin");
	let mut command = tokio::process::Command::new(binary);
	command
		.args(["ext", "trust", plugin, "--show", "--project"])
		.arg(project)
		.current_dir(project)
		.envs(isolated_roots(home))
		.env("NO_COLOR", "1");
	let output = omp_e2e::support::OwnedProcess::output(command, READY_TIMEOUT)
		.await
		.unwrap_or_else(|error| panic!("omp ext trust {plugin} --show: {error}"));
	let shown = String::from_utf8_lossy(&output.stdout);
	assert!(
		output.status.success(),
		"omp ext trust {plugin} --show failed: {}\nstdout={shown}\nstderr={}",
		output.status,
		String::from_utf8_lossy(&output.stderr),
	);
	let line = shown
		.lines()
		.find(|line| line.starts_with(label))
		.unwrap_or_else(|| panic!("omp ext trust {plugin} --show omits {label}:\n{shown}"));
	["approved", "blocked", "unreadable"]
		.into_iter()
		.find(|status| line.contains(&format!("` {status} digest=")))
		.unwrap_or_else(|| panic!("no approval state in {line:?}"))
}

/// The glyph painted at display column `column` of `row`.
fn glyph_at(row: &str, column: usize) -> Option<char> {
	let mut at = 0;
	for glyph in row.chars() {
		if at == column {
			return Some(glyph);
		}
		at += xutf::width_char(glyph);
		if at > column {
			return None;
		}
	}
	None
}

/// The painted rows of the bordered overlay titled `title` in `screen`, from
/// its top border to its bottom border, after checking that it is one intact
/// box: the title painted once, every row inside `cols`, and every row
/// between the corners carrying its left and right border on the corners'
/// display columns.
fn overlay_box<'s>(screen: &'s str, title: &str, cols: usize) -> Vec<&'s str> {
	let rows = screen.lines().collect::<Vec<_>>();
	for row in &rows {
		assert!(xutf::width_str(row) <= cols, "row wider than {cols} columns: {row:?}\n{screen}");
	}
	assert_eq!(screen.matches(title).count(), 1, "{title:?} painted more than once:\n{screen}");
	let top = rows
		.iter()
		.position(|row| row.contains(title))
		.expect("overlay title row");
	// The corner nearest the title: a centered panel may share its top row
	// with the welcome box's own border.
	let title_at = rows[top].find(title).expect("title on its row");
	let corner = rows[top][..title_at]
		.rfind('╭')
		.unwrap_or_else(|| panic!("overlay top border is torn: {:?}\n{screen}", rows[top]));
	let left = xutf::width_str(&rows[top][..corner]);
	let right = xutf::width_str(&rows[top][..rows[top].rfind('╮').expect("top-right corner")]);
	for (bottom, row) in rows.iter().enumerate().skip(top + 1) {
		match (glyph_at(row, left), glyph_at(row, right)) {
			(Some('╰'), Some('╯')) => return rows[top..=bottom].to_vec(),
			(Some('│'), Some('│')) | (Some('├'), Some('┤')) => {},
			sides => panic!("overlay row lost its borders {sides:?}: {row:?}\n{screen}"),
		}
	}
	panic!("overlay has no bottom border:\n{screen}")
}

/// The selector's option row naming `label`, cursor or not.
fn listed<'r>(overlay: &[&'r str], label: &str) -> Option<&'r str> {
	// The cursor prefix is the one the selector paints: `Charset::cursor`,
	// blank-padded to the same width on the rows it is not on.
	let cursor = CHARSET.cursor();
	let (selected, unselected) = (
		format!("│ {cursor}{label}"),
		format!("│ {:width$}{label}", "", width = xutf::width_str(cursor)),
	);
	overlay
		.iter()
		.copied()
		.find(|row| row.contains(&selected) || row.contains(&unselected))
}

/// Proves in-chat approval of blocked plugin commands on a real PTY: the
/// launch names what it blocked, `/plugins approve` opens the selector over
/// exactly those commands, arrow navigation and Enter approve one through
/// the same admission `omp ext trust` uses (read back through that CLI while
/// chat still runs), the selector re-lays out across a resize, the direct
/// `/plugins approve <plugin> all` form approves the rest of a plugin, and
/// quitting restores the terminal.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn chat_tui_approves_blocked_plugin_commands_on_a_real_pty() {
	use std::os::unix::fs::PermissionsExt;
	omp_e2e::support::install_omp_binary_env().expect("install Cargo-built omp binary");
	let scratch = tempfile::tempdir().expect("scratch root");
	fs::set_permissions(scratch.path(), <fs::Permissions>::from_mode(0o700))
		.expect("secure scratch root");
	let project = scratch.path().join("project");
	fs::create_dir(&project).expect("project directory");
	let project = fs::canonicalize(&project).expect("canonical project root");
	let metadata_dir = project.join(".omp");
	fs::create_dir(&metadata_dir).expect("project metadata directory");
	fs::set_permissions(&metadata_dir, <fs::Permissions>::from_mode(0o755))
		.expect("use standard project metadata permissions");

	// Two installed Claude-layout plugins, each declaring a stdio MCP server
	// and a Stop hook nobody approved: the launch blocks all four commands.
	// `PtyChild` points HOME at the scratch `home`, so the host's own Claude
	// Code installs never join the fixture.
	let home = scratch.path().join("home");
	let data = home.join("data");
	let mut roots = Vec::with_capacity(2);
	for (name, server) in [("alpha", "db"), ("beta", "search")] {
		let root = data.join(format!("plugins/cache/plugins/market___{name}___1.0.0"));
		write_fixture(
			&root.join(".mcp.json"),
			&format!(
				r#"{{"mcpServers":{{"{server}":{{"command":"${{CLAUDE_PLUGIN_ROOT}}/bin/{server}","args":["--stdio"]}}}}}}"#
			),
		);
		write_fixture(
			&root.join("hooks/hooks.json"),
			r#"{"hooks":{"Stop":[{"hooks":[{"type":"command","command":"${CLAUDE_PLUGIN_ROOT}/bin/on-stop"}]}]}}"#,
		);
		roots.push(root);
	}
	install_plugins(&data, &[
		("alpha@market", roots[0].as_path()),
		("beta@market", roots[1].as_path()),
	]);
	// Each row the selector lists, as `omp ext trust --show` also leads its
	// line: plugin, component, and name.
	let labels = [
		"alpha@market MCP server `alpha@market:db`",
		"alpha@market hook `Stop`",
		"beta@market MCP server `beta@market:search`",
		"beta@market hook `Stop`",
	];
	// The tail of each command line, which the selector must keep visible.
	let tails = ["bin/db --stdio", "bin/on-stop", "bin/search --stdio", "bin/on-stop"];

	let binary = omp_e2e::support::omp_binary().expect("locate omp binary");
	for label in labels {
		assert_eq!(
			trust_status(&binary, &home, &project, label).await,
			"blocked",
			"{label} before launch"
		);
	}

	let gateway_socket = scratch.path().join("gateway.sock");
	let debug_socket = scratch.path().join("tui-debug.sock");
	// No turn runs: approving a command never reaches the provider, and an
	// unscripted call would fail the proof.
	let gateway =
		ScriptedGateway::start_with_scripts(scratch.path(), &gateway_socket, Vec::new()).await;
	let session_path = scratch.path().join("p7-plugins.oms");
	seed_session(&session_path);
	let args = vec![
		"chat".to_owned(),
		"--model".to_owned(),
		gateway.model.clone(),
		"--project".to_owned(),
		project.display().to_string(),
		"--gateway".to_owned(),
		gateway_socket.display().to_string(),
		"--session".to_owned(),
		session_path.display().to_string(),
		"--envd-idle-timeout".to_owned(),
		"2".to_owned(),
		// Scripted `bash` calls must run on every host, with or without an OS
		// sandbox. An explicit `yolo` is respected unconfined; the default one
		// would prompt where the sandbox cannot be built (a Linux runner
		// without bubblewrap), and nobody answers a prompt in this proof.
		"--approval-mode".to_owned(),
		"yolo".to_owned(),
	];
	let mut process = PtyChild::spawn(&binary, &args, &project, &debug_socket);
	let raw_capture = process.raw.clone();
	let mut debug =
		DebugClient::connect(&debug_socket, Instant::now() + READY_TIMEOUT, &mut process);

	// The launch's last notice, the one that stays visible, counts the
	// blocked commands and names the in-chat approval.
	let ready = wait_snapshot(&mut debug, &raw_capture, "blocked launch named", |snapshot| {
		snapshot.text.contains(COMPOSER_PROMPT)
			&& snapshot.text.contains(
				"4 plugin commands did not run: approve them with `/plugins approve`, then `/restart`",
			)
	});
	assert_surface(&ready, "blocked launch");

	// The summary is one of several launch notices: the row counts the ones
	// stacked under it, and `/notices` lists each blocked command's warning
	// and the `--plugin-dir` hint that the row could not show.
	let stacked = ready
		.text
		.split_once(" more (/notices)")
		.and_then(|(head, _)| {
			head
				.split_whitespace()
				.next_back()?
				.strip_prefix('+')?
				.parse::<usize>()
				.ok()
		})
		.unwrap_or_else(|| panic!("the launch row omits its `+N more (/notices)`:\n{}", ready.text));
	// Four blocked commands plus the `--plugin-dir` hint, under the summary.
	assert!(stacked >= 5, "only {stacked} notices stacked under the summary:\n{}", ready.text);
	let title = format!("Notices ({})", stacked + 1);
	debug.keys("'/notices' enter");
	let listed_notices = wait_snapshot(&mut debug, &raw_capture, "notice log open", |snapshot| {
		snapshot.text.contains(&title)
	});
	let log = overlay_box(&listed_notices.text, &title, 120);
	assert!(
		!listed_notices.text.contains("more (/notices)"),
		"the key that ran /notices left the row up:\n{}",
		listed_notices.text
	);
	// Markdown wraps a long notice across rows: compare the reading text.
	let reading = |log: &[&str]| {
		log.iter()
			.map(|row| row.trim_matches(|ch: char| ch == '│' || ch.is_whitespace()))
			.collect::<Vec<_>>()
			.join(" ")
	};
	let launch_lines = [
		"alpha@market:db",
		"beta@market:search",
		"omp ext trust",
		"--plugin-dir <path>",
		"4 plugin commands did not run",
	];
	let read = reading(&log);
	for text in launch_lines {
		assert!(read.contains(text), "the notice log omits {text:?}:\n{}", listed_notices.text);
	}
	assert!(
		read.find("alpha@market:db") < read.find("4 plugin commands did not run"),
		"oldest first, summary last:\n{}",
		listed_notices.text
	);

	// A resize re-lays the log out at the new geometry, nothing lost.
	process.resize(24, 72);
	debug
		.op("resize")
		.unwrap_or_else(|error| panic!("resize injection failed: {error}"));
	wait_info(&mut debug, "settled notice log resize", |info| {
		info.get("rows").and_then(Value::as_u64) == Some(24)
			&& info.get("cols").and_then(Value::as_u64) == Some(72)
			&& info.get("overlay").and_then(Value::as_bool) == Some(true)
	});
	let resized_log =
		wait_snapshot(&mut debug, &raw_capture, "notice log re-laid out", |snapshot| {
			snapshot.text.contains(&title) && snapshot.text.contains("Esc close")
		});
	// The 24-row panel scrolls: its first notice is in view, intact, and
	// paging down brings the last ones (the hint, the summary) into it.
	let read = reading(&overlay_box(&resized_log.text, &title, 72));
	assert!(
		read.contains("alpha@market:db"),
		"the resize lost the first notice:\n{}",
		resized_log.text
	);
	debug.keys("pgdn pgdn pgdn pgdn pgdn pgdn");
	let paged = wait_snapshot(&mut debug, &raw_capture, "notice log paged to its end", |snapshot| {
		snapshot.text.contains("4 plugin commands did not run")
	});
	let read = reading(&overlay_box(&paged.text, &title, 72));
	for text in ["--plugin-dir <path>", "4 plugin commands did not run"] {
		assert!(read.contains(text), "the resized log lost {text:?}:\n{}", paged.text);
	}
	process.resize(48, 120);
	debug
		.op("resize")
		.unwrap_or_else(|error| panic!("resize injection failed: {error}"));
	wait_info(&mut debug, "settled notice log restore", |info| {
		info.get("rows").and_then(Value::as_u64) == Some(48)
			&& info.get("cols").and_then(Value::as_u64) == Some(120)
	});
	debug.keys("escape");
	wait_snapshot(&mut debug, &raw_capture, "notice log closed", |snapshot| {
		!snapshot.text.contains(&title) && snapshot.text.contains(COMPOSER_PROMPT)
	});

	// The selector lists exactly the blocked commands, cursor on the first,
	// each command's executable and arguments visible, with the approval
	// count, the `/restart` rule, and the `--plugin-dir` alternative.
	debug.keys("'/plugins approve' enter");
	let opened = wait_snapshot(&mut debug, &raw_capture, "approval selector open", |snapshot| {
		snapshot.text.contains("Plugin commands")
			&& snapshot.text.contains("4 commands await approval")
	});
	let overlay = overlay_box(&opened.text, "Plugin commands", 120);
	let mut order = labels
		.iter()
		.zip(tails)
		.map(|(label, tail)| {
			let row = listed(&overlay, label)
				.unwrap_or_else(|| panic!("selector omits {label}:\n{}", opened.text));
			assert!(row.contains(tail), "{label} hides its command {tail:?}: {row:?}");
			let index = overlay
				.iter()
				.position(|candidate| *candidate == row)
				.expect("listed row");
			(index, *label)
		})
		.collect::<Vec<_>>();
	order.sort_unstable();
	for text in [
		"approved commands load after /restart",
		"start omp with --plugin-dir <path>",
		"Enter approve",
	] {
		assert!(opened.text.contains(text), "selector footer omits {text:?}:\n{}", opened.text);
	}
	let cursor = |overlay: &[&str], label: &str| {
		listed(overlay, label)
			.is_some_and(|row| row.contains(&format!("{}{label}", CHARSET.cursor())))
	};
	assert!(cursor(&overlay, order[0].1), "cursor starts on the first row:\n{}", opened.text);

	// Down moves the cursor to the second row; Enter approves that command.
	let target = order[1].1;
	debug.keys("down");
	wait_snapshot(&mut debug, &raw_capture, "cursor on the second command", |snapshot| {
		cursor(&overlay_box(&snapshot.text, "Plugin commands", 120), target)
	});
	debug.keys("enter");
	// The outcome lands in the selector's own status row, inside the intact
	// box, and the approved command leaves the list.
	let approved_line = format!("Approved 1 command; run /restart to load it: {target}.");
	let approved =
		wait_snapshot(&mut debug, &raw_capture, "selected command approved", |snapshot| {
			snapshot.text.contains(&approved_line)
				&& listed(&overlay_box(&snapshot.text, "Plugin commands", 120), target).is_none()
		});
	let overlay = overlay_box(&approved.text, "Plugin commands", 120);
	assert!(
		overlay
			.iter()
			.any(|row| row.contains(&format!("│ {approved_line}"))),
		"outcome outside the selector's status row:\n{}",
		approved.text
	);
	let left = order
		.iter()
		.map(|(_, label)| *label)
		.filter(|label| *label != target)
		.collect::<Vec<_>>();
	for label in &left {
		assert!(listed(&overlay, label).is_some(), "{label} left the list:\n{}", approved.text);
	}
	// Persisted, through the grant file `omp ext trust` reads, while the
	// session still runs.
	assert_eq!(
		trust_status(&binary, &home, &project, target).await,
		"approved",
		"{target} after Enter"
	);
	for label in &left {
		assert_eq!(
			trust_status(&binary, &home, &project, label).await,
			"blocked",
			"{label} after approving {target}"
		);
	}

	// The next key returns the status row to the count; the rebuilt list
	// started its cursor over, so Down selects the second remaining command.
	debug.keys("down");
	wait_snapshot(&mut debug, &raw_capture, "cursor on a remaining command", |snapshot| {
		snapshot.text.contains("3 commands await approval")
			&& cursor(&overlay_box(&snapshot.text, "Plugin commands", 120), left[1])
	});

	// A resize while the selector is open re-lays it out at the new
	// geometry as one intact box over the surviving composer, the cursor
	// still on the command the operator selected.
	process.resize(24, 72);
	debug
		.op("resize")
		.unwrap_or_else(|error| panic!("resize injection failed: {error}"));
	wait_info(&mut debug, "settled selector resize", |info| {
		info.get("rows").and_then(Value::as_u64) == Some(24)
			&& info.get("cols").and_then(Value::as_u64) == Some(72)
			&& info.get("overlay").and_then(Value::as_bool) == Some(true)
	});
	let resized = wait_snapshot(&mut debug, &raw_capture, "selector re-laid out", |snapshot| {
		snapshot.text.contains("3 commands await approval") && snapshot.text.contains(COMPOSER_PROMPT)
	});
	let overlay = overlay_box(&resized.text, "Plugin commands", 72);
	for label in &left {
		assert!(listed(&overlay, label).is_some(), "{label} lost on resize:\n{}", resized.text);
	}
	assert!(
		cursor(&overlay, left[1]),
		"the resize moved the cursor off {}:\n{}",
		left[1],
		resized.text
	);

	debug.keys("escape");
	wait_snapshot(&mut debug, &raw_capture, "selector closed", |snapshot| {
		!snapshot.text.contains("Plugin commands") && snapshot.text.contains(COMPOSER_PROMPT)
	});

	// The direct form approves every command of one plugin; the result line
	// keeps `/restart` in view even truncated at 72 columns.
	debug.keys("'/plugins approve beta@market all' enter");
	wait_snapshot(&mut debug, &raw_capture, "direct approval settled", |snapshot| {
		snapshot
			.text
			.contains("Approved 2 commands; run /restart to load them: beta@market")
	});
	for label in &labels[2..] {
		assert_eq!(
			trust_status(&binary, &home, &project, label).await,
			"approved",
			"{label} after `all`"
		);
	}

	// Reopened, the selector reads the grant file fresh: only the one alpha
	// command neither approval covered is left.
	let remaining = order
		.iter()
		.map(|(_, label)| *label)
		.find(|label| *label != target && label.starts_with("alpha@"))
		.expect("one alpha command stays blocked");
	debug.keys("'/plugins approve' enter");
	let reopened = wait_snapshot(&mut debug, &raw_capture, "selector reopened", |snapshot| {
		snapshot.text.contains("1 command awaits approval")
	});
	let overlay = overlay_box(&reopened.text, "Plugin commands", 72);
	assert!(listed(&overlay, remaining).is_some(), "{remaining} missing:\n{}", reopened.text);
	assert!(
		!overlay.iter().any(|row| row.contains("beta@market")),
		"approved beta commands still listed:\n{}",
		reopened.text
	);
	debug.keys("escape");
	let closed = wait_snapshot(&mut debug, &raw_capture, "selector closed again", |snapshot| {
		!snapshot.text.contains("Plugin commands") && snapshot.text.contains(COMPOSER_PROMPT)
	});

	debug.keys("ctrl+c ctrl+c");
	drop(debug);
	let before = process.before.clone();
	let (status, raw, stdout, stderr, after) = process.wait(READY_TIMEOUT);
	let diagnostics = format!(
		"status={status}\nstdout={stdout}\nstderr={stderr}\nlast screen={}\nraw={}",
		closed.text,
		visible(&raw),
	);
	assert!(status.success(), "omp chat did not exit cleanly\n{diagnostics}");
	assert_restored(&raw, &before, &after, &diagnostics);
	assert_eq!(
		trust_status(&binary, &home, &project, remaining).await,
		"blocked",
		"{remaining} after quit"
	);
	assert_eq!(
		trust_status(&binary, &home, &project, target).await,
		"approved",
		"{target} after quit"
	);
}

/// P7 on the default production path: chat attaches to the project daemon it
/// spawns, and a bash command that daemon runs asks this chat, through the
/// approval relay, for its sandbox amendment.
///
/// The proof needs Seatbelt, so it exists only on macOS, where an unavailable
/// Seatbelt fails it instead of skipping it.
#[cfg(target_os = "macos")]
mod daemon_amendment {
	use omp_agent::{ApprovalSource, ApprovalTicket, TicketState};
	use omp_envd::process_identity::ProcessIdentity;

	use super::*;

	/// Title the daemon gives every sandbox amendment prompt.
	const TITLE: &str = "Approve scoped sandbox amendment";
	/// What an embedded fallback says when chat could not join its daemon:
	/// both `ProjectEnvironment::fallback_notice` and the WARN line chat logs
	/// start with it.
	const FALLBACK: &str = "project daemon unavailable";
	/// The answers the overlay offers a once-only sandbox amendment: approve
	/// and deny, and no `a` (approve for session), which the daemon refuses.
	const AMENDMENT_ANSWERS: &str = "y approve n deny";
	/// The non-PTY bash tool detaches a command that runs 15 s. Each prompt
	/// must be painted within this long of releasing its call, which leaves
	/// the answer time to land before the call detaches.
	const PROMPT_DEADLINE: Duration = Duration::from_secs(12);
	/// The file the approved command writes into the protected carve-out.
	const APPROVED: &str = "omp-approved";
	/// The file the denied command would have written.
	const DENIED: &str = "omp-denied";
	/// The closing answer of the turn.
	const DONE: &str = "Both sandbox amendments were answered.";

	/// A command whose redirection lands in `.git`, which the shipped
	/// `workspace-write` sandbox protects. The in-process shell expands `$$` to
	/// the process id of the host that runs it.
	fn protected_write(name: &str) -> String {
		format!("printf '%s' \"$$\" > .git/{name}")
	}

	/// The journaled data of the result the call `entry` settled with.
	fn tool_result<'j>(text: &'j str, entry: &str) -> Option<&'j str> {
		journal_frames(text)
			.filter(|frame| frame.contains("event: tool.result@1"))
			.find(|frame| frame_field(frame, "by: ") == Some(entry))
			.and_then(|frame| frame_field(frame, "data: "))
	}

	/// The approval tickets the session journal folds into its prompts queue.
	fn journaled_tickets(path: &Path) -> Vec<ApprovalTicket> {
		let session = Session::open(path, ComponentRegistry::standard()).expect("replay the journal");
		let dom = session.dom();
		let prompts = omp_session::components::prompts::prompts_handle(dom).expect("prompts queue");
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

	/// The ticket is the daemon's decided one-time amendment of `command`.
	fn assert_decided(ticket: &ApprovalTicket, project: &Path, command: &str, approved: bool) {
		assert_eq!(ticket.invocation_id, None, "a sandbox amendment blocks no invocation");
		assert_eq!(ticket.state, TicketState::Decided, "{ticket:?}");
		let [reason] = ticket.reasons.as_slice() else {
			panic!("expected one amendment requirement: {ticket:?}");
		};
		assert_eq!(reason.kind.as_str(), "sandbox_amendment");
		assert_eq!(reason.title.as_str(), TITLE);
		assert_eq!(reason.pattern.as_deref(), Some(command));
		assert_eq!(reason.subject.as_str(), format!("write {}", project.join(".git").display()));
		let decision = ticket
			.decision
			.as_ref()
			.expect("a decided ticket carries its decision");
		assert_eq!(decision.approved, approved, "{ticket:?}");
		assert_eq!(decision.source, ApprovalSource::User, "{ticket:?}");
		assert_eq!(decision.scope.as_str(), "once", "{ticket:?}");
	}

	/// Every log file the chat and its daemon wrote. `PtyChild::spawn` pins
	/// their state directory (and so the log directory) under the isolated
	/// home and clears `OMP_LOG`, so this reads only what this case's
	/// processes wrote, at the default filter, which keeps WARN lines.
	fn logs(home: &Path) -> Vec<(PathBuf, String)> {
		let directory = isolated_state_dir(home).join("logs");
		fs::read_dir(&directory)
			.unwrap_or_else(|error| panic!("read log directory {}: {error}", directory.display()))
			.map(|entry| entry.expect("log directory entry").path())
			.filter(|path| path.is_file())
			.map(|path| {
				let text = fs::read_to_string(&path)
					.unwrap_or_else(|error| panic!("read log {}: {error}", path.display()));
				(path, text)
			})
			.collect()
	}

	/// Waits for the amendment prompt of `command` painted over the chat, then
	/// checks it arrived in time to be answered before the call detaches.
	fn await_prompt(
		debug: &mut DebugClient,
		raw: &Arc<Mutex<Vec<u8>>>,
		label: &str,
		command: &str,
		started: Instant,
	) -> Snapshot {
		let prompt = wait_snapshot(debug, raw, label, |snapshot| {
			snapshot.text.contains(TITLE) && snapshot.combined().contains(command)
		});
		let waited = started.elapsed();
		mark(&format!("{label} after {waited:?}"));
		assert!(
			waited < PROMPT_DEADLINE,
			"{label} took {waited:?}; the bash call detaches 15 s after it starts, so the answer \
			 could no longer reach it in time"
		);
		assert_unpainted_fallback(&prompt, label);
		prompt
	}

	/// No fallback notice is on screen. This guards a notice the app may paint
	/// one day and proves nothing today: nothing in the app, chat or driver
	/// reads `ProjectEnvironment::fallback_notice`, so a fallback is never
	/// painted. The pid the rerun writes and chat's log decide it.
	fn assert_unpainted_fallback(snapshot: &Snapshot, label: &str) {
		assert!(
			!snapshot.combined().contains(FALLBACK),
			"{label}: chat fell back to an embedded environment:\n{}",
			snapshot.combined()
		);
	}

	/// The shipped default posture with chat attached to the project daemon it
	/// spawned (no test-owned daemon: the environment socket is keyed by the
	/// chat's own build). A scripted bash call writes `$$` into `.git`; the
	/// sandbox denies it, the daemon relays the amendment to this chat only,
	/// and the overlay answers it. The overlay offers only approve and deny,
	/// and `a` (approve for session) is no answer: `y` reruns the command once
	/// inside the daemon, whose pid it writes, and `Esc` denies the second
	/// command, which writes nothing. The journal holds both decided `once`
	/// prompts and quitting restores the terminal. That chat never fell back to
	/// an embedded environment rests on the written pid and on chat's log; the
	/// app paints no fallback notice, so the screen cannot show one.
	#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
	async fn chat_tui_answers_a_daemon_sandbox_amendment_from_its_overlay() {
		use std::os::unix::fs::PermissionsExt;
		if let Some(failure) = omp_sandbox::backend_status(omp_sandbox::Backend::Seatbelt).failure() {
			panic!("this P7 case needs the Seatbelt sandbox backend, and its probe failed: {failure}");
		}
		omp_e2e::support::install_omp_binary_env().expect("install Cargo-built omp binary");
		let scratch = tempfile::tempdir().expect("scratch root");
		fs::set_permissions(scratch.path(), <fs::Permissions>::from_mode(0o700))
			.expect("secure scratch root");
		let project = scratch.path().join("project");
		fs::create_dir(&project).expect("project directory");
		let project = fs::canonicalize(&project).expect("canonical project root");
		let metadata_dir = project.join(".omp");
		fs::create_dir(&metadata_dir).expect("project metadata directory");
		fs::set_permissions(&metadata_dir, <fs::Permissions>::from_mode(0o755))
			.expect("use standard project metadata permissions");
		fs::create_dir(project.join(".git")).expect("protected .git carve-out");

		let approved_command = protected_write(APPROVED);
		let denied_command = protected_write(DENIED);
		let gateway_socket = scratch.path().join("gateway.sock");
		let debug_socket = scratch.path().join("tui-debug.sock");
		let gateway = ScriptedGateway::start_with_scripts(scratch.path(), &gateway_socket, vec![
			tool_script(&[("amend-approve", "bash", json!({ "command": approved_command }))]),
			tool_script(&[("amend-deny", "bash", json!({ "command": denied_command }))]),
			text_script(DONE),
		])
		.await;
		let session_path = scratch.path().join("p7-amendment.oms");
		seed_session(&session_path);

		let binary = omp_e2e::support::omp_binary().expect("locate omp binary");
		// No `--approval-mode`: the shipped default `yolo` holds inside the
		// active sandbox, so the denial, not an admission, is what prompts.
		let args = vec![
			"chat".to_owned(),
			"--model".to_owned(),
			gateway.model.clone(),
			"--project".to_owned(),
			project.display().to_string(),
			"--gateway".to_owned(),
			gateway_socket.display().to_string(),
			"--session".to_owned(),
			session_path.display().to_string(),
			"--envd-idle-timeout".to_owned(),
			"2".to_owned(),
		];
		let mut process = PtyChild::spawn(&binary, &args, &project, &debug_socket);
		let chat_pid = process.child.id();
		let home = scratch.path().join("home");
		let raw_capture = process.raw.clone();
		let mut debug =
			DebugClient::connect(&debug_socket, Instant::now() + READY_TIMEOUT, &mut process);
		let ready = wait_snapshot(&mut debug, &raw_capture, "chat shell ready", |snapshot| {
			let surface = snapshot.combined();
			surface.contains("Welcome back!")
				&& surface.contains(&gateway.model)
				&& surface.contains(COMPOSER_PROMPT)
		});
		assert_surface(&ready, "ready");
		assert_unpainted_fallback(&ready, "ready");

		debug.keys("'answer two sandbox amendments' enter");
		let started = Instant::now();
		gateway.release(0);
		let prompt = await_prompt(
			&mut debug,
			&raw_capture,
			"approve prompt painted",
			&approved_command,
			started,
		);
		// The amendment offers only `once`, so the overlay offers approve and
		// deny and no session answer, which the daemon would refuse.
		assert!(
			prompt.text.contains("Scope: once") && prompt.text.contains(AMENDMENT_ANSWERS),
			"the overlay hides its answers:\n{}",
			prompt.text
		);
		assert!(
			!prompt.text.contains("approve for session"),
			"the overlay offers a session answer the amendment refuses:\n{}",
			prompt.text
		);
		// `a` is no answer here: the prompt stays open for `y`. Had it decided,
		// the command would end denied without writing, and the journal would
		// hold a session-scoped decision instead of the `once` one asserted
		// after quitting.
		debug.keys("a");
		debug.keys("y");
		let approved = project.join(".git").join(APPROVED);
		let settled = wait_snapshot(&mut debug, &raw_capture, "approved rerun settled", |snapshot| {
			let text = journal(&session_path);
			!snapshot.combined().contains(TITLE)
				&& approved.is_file()
				&& tool_call_id(&text, "amend-approve")
					.is_some_and(|call| tool_result(&text, call).is_some())
		});
		assert_surface(&settled, "approved rerun");
		let written = fs::read_to_string(&approved).expect("approved rerun write");
		let writer = written
			.parse::<u32>()
			.unwrap_or_else(|error| panic!("`$$` wrote {written:?}: {error}"));
		// `$$` names the host whose in-process shell ran the command. Embedded,
		// that is chat itself; here it is another live process of the same
		// executable, the daemon chat spawned, which outlives the command.
		assert_ne!(writer, chat_pid, "the approved rerun ran inside chat, not its project daemon");
		assert_ne!(writer, std::process::id(), "the approved rerun ran inside the test process");
		let identity = ProcessIdentity::capture(writer).unwrap_or_else(|error| {
			panic!("the process that ran the rerun ({writer}) is gone: {error}")
		});
		assert_eq!(
			fs::canonicalize(&identity.executable).expect("canonical daemon executable"),
			binary,
			"pid {writer} is not an omp process: {identity:?}"
		);

		let started = Instant::now();
		gateway.release(1);
		// The scripted route holds the second provider call's stream after its
		// first argument delta (the edit-preview checkpoint of the first P7
		// case); this case has nothing to observe there.
		gateway.await_preview().await;
		gateway.release_preview();
		await_prompt(&mut debug, &raw_capture, "deny prompt painted", &denied_command, started);
		debug.keys("escape");
		let denied = wait_snapshot(&mut debug, &raw_capture, "denied command settled", |snapshot| {
			let text = journal(&session_path);
			!snapshot.combined().contains(TITLE)
				&& tool_call_id(&text, "amend-deny")
					.is_some_and(|call| tool_result(&text, call).is_some())
		});
		assert_surface(&denied, "denied command");
		let settled_journal = journal(&session_path);
		let call = tool_call_id(&settled_journal, "amend-deny").expect("the denied bash call");
		let result = tool_result(&settled_journal, call).expect("the denied bash result");
		assert!(result.contains("\"outcome\":\"denied\""), "Esc did not deny the command: {result}");
		assert!(!project.join(".git").join(DENIED).exists(), "the denied command wrote");

		gateway.release(2);
		let finished = wait_snapshot(&mut debug, &raw_capture, "turn complete", |snapshot| {
			let surface = snapshot.combined();
			surface.contains(DONE) && surface.contains(COMPOSER_PROMPT)
		});
		assert_surface(&finished, "turn complete");
		assert_unpainted_fallback(&finished, "turn complete");
		assert_journal_chain(&journal(&session_path));

		debug.keys("ctrl+c ctrl+c");
		drop(debug);
		let before = process.before.clone();
		let (status, raw, stdout, stderr, after) = process.wait(READY_TIMEOUT);
		let diagnostics = format!(
			"status={status}\nstdout={stdout}\nstderr={stderr}\nlast frame={}\nraw={}",
			finished.frame,
			visible(&raw),
		);
		assert!(status.success(), "omp chat did not exit cleanly\n{diagnostics}");
		assert_restored(&raw, &before, &after, &diagnostics);

		let tickets = journaled_tickets(&session_path);
		let [approve, deny] = tickets.as_slice() else {
			panic!("expected two journaled amendment prompts: {tickets:?}");
		};
		assert_decided(approve, &project, &approved_command, true);
		assert_decided(deny, &project, &denied_command, false);

		// An embedded fallback is logged by chat, not painted: the app never
		// shows `fallback_notice`, so the screen checks above prove nothing
		// today. Besides the pid check, chat's own log decides its absence.
		let log_files = logs(&home);
		assert!(
			log_files.iter().any(|(path, _)| path
				.file_name()
				.and_then(|name| name.to_str())
				.is_some_and(|name| name.contains(&format!(".{chat_pid}.log")))),
			"chat wrote no log under its isolated home: {:?}",
			log_files.iter().map(|(path, _)| path).collect::<Vec<_>>()
		);
		for (path, text) in &log_files {
			let fallback = text
				.lines()
				.filter(|line| line.contains(FALLBACK))
				.collect::<Vec<_>>();
			assert!(
				fallback.is_empty(),
				"{} records an embedded fallback:\n{}",
				path.display(),
				fallback.join("\n")
			);
		}
	}
}
