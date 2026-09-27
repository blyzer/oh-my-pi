//! Claude-format plugin hooks on the generic hook surface.
//!
//! Installed plugins declare Claude Code hooks (`hooks/hooks.json`, manifest
//! `hooks`); [`omp_ext::claude_hooks`] parses them and maps each event onto
//! an omp lifecycle seam. [`PluginHookHost`] is one in-process
//! [`NativeHookHost`] attached to the session's [`HookGate`] exactly where
//! extension hosts subscribe; the agent loop has no plugin-specific branch.
//!
//! Each matching hook runs as one command in its own session of the
//! environment's in-process shell ([`EnvClient`]): process-group ownership,
//! TERM-then-KILL teardown on timeout, the project root as its working
//! directory, `${CLAUDE_PLUGIN_ROOT}`, `${CLAUDE_PLUGIN_DATA}`, and
//! `${CLAUDE_PROJECT_DIR}` expanded in the command and exported to it. The
//! Claude Code hook protocol then maps onto omp's typed verdicts:
//!
//! * the JSON event payload arrives on stdin (typed serde, never `json!`);
//! * exit 0 reads stdout as JSON output when it is one object, else as plain
//!   text (context for `UserPromptSubmit` and `SessionStart`);
//! * exit 2 blocks with stderr as the reason; any other exit is a non-blocking
//!   error unless it printed valid JSON output;
//! * a blocked `PreToolUse` denies the call with the reason the model reads, a
//!   blocked `Stop` continues the loop with the reason as context,
//!   `additionalContext` becomes model-visible hook context, `systemMessage`
//!   and failures become `<notice kind=hook>`, and `continue: false` stops.
//!
//! A failing hook is a typed [`PluginHookError`] rendered into a notice; it
//! never fails the turn.

use std::{
	collections::BTreeMap,
	fmt::Write as _,
	path::{Path, PathBuf},
	sync::{
		Arc, OnceLock, Weak,
		atomic::{AtomicU32, Ordering},
	},
	time::Duration,
};

use bytes::Bytes;
use omp_agent::{
	BoxFut, EnvEvent, HookContext, HookGate, NativeHookHost, NativeReply, NativeVerdict, Up,
};
use omp_core::{EnvPath, Str, sf};
use omp_env::{ClientError, EnvClient, ExecEvent};
use omp_ext::{
	claude_hooks::{ClaudeHookEvent, HookSeam, PluginHook, claude_tool_name},
	claude_plugin::{ClaudePlugins, expand_plugin_vars, plugin_data_dir},
};
use omp_proto::{
	env::v1::{
		CloseSessionRequest, EnvironmentDelta, ExecOutcome, ExecRequest, OpenSessionRequest,
		OutputChannel, Script,
	},
	toolhost::v1::HookEventId,
};
use omp_session::custom_message::{CustomMessage, CustomMessageKind};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use smallvec::SmallVec;

/// Captured bytes kept per output stream; the rest of a chatty hook's output
/// is dropped.
const OUTPUT_CAP: usize = 1 << 20;
/// Consecutive stop-hook continuations before the next block is overridden
/// (the spec's default cap).
const STOP_CONTINUATION_CAP: u32 = 8;

/// A plugin hook that could not produce a decision; rendered once into a
/// `<notice kind=hook>` and never fatal to the turn.
#[derive(Debug, thiserror::Error)]
pub enum PluginHookError {
	/// The environment could not open a shell session or start the command.
	#[error("plugin `{plugin}` {event} hook could not start")]
	Start {
		/// Plugin id.
		plugin: Str,
		/// Hook event.
		event:  ClaudeHookEvent,
		/// Environment failure.
		#[source]
		source: ClientError,
	},
	/// The command outlived its timeout; it was terminated and its output
	/// discarded.
	#[error("plugin `{plugin}` {event} hook timed out after {}ms; its output was discarded", timeout.as_millis())]
	TimedOut {
		/// Plugin id.
		plugin:  Str,
		/// Hook event.
		event:   ClaudeHookEvent,
		/// The timeout that fired.
		timeout: Duration,
	},
	/// The command ended without an exit code (killed by a signal, denied by
	/// the sandbox, or cancelled).
	#[error("plugin `{plugin}` {event} hook ended without an exit status ({outcome:?})")]
	NoExitStatus {
		/// Plugin id.
		plugin:  Str,
		/// Hook event.
		event:   ClaudeHookEvent,
		/// The environment's outcome.
		outcome: ExecOutcome,
	},
	/// A non-zero, non-2 exit without valid JSON output.
	#[error("plugin `{plugin}` {event} hook failed with non-blocking status code {code}: {stderr}")]
	NonBlocking {
		/// Plugin id.
		plugin: Str,
		/// Hook event.
		event:  ClaudeHookEvent,
		/// Exit code.
		code:   i32,
		/// First stderr line the hook printed.
		stderr: Str,
	},
	/// Stdout looked like a JSON object but is not valid hook output.
	#[error("plugin `{plugin}` {event} hook printed invalid JSON output")]
	MalformedOutput {
		/// Plugin id.
		plugin: Str,
		/// Hook event.
		event:  ClaudeHookEvent,
		/// Decoding failure.
		#[source]
		source: serde_json::Error,
	},
}

/// One loaded hook with its plugin's identity and directories.
#[derive(Clone)]
struct LoadedHook {
	plugin: Str,
	root:   PathBuf,
	data:   PathBuf,
	hook:   PluginHook,
}

/// Session facts every hook input carries.
#[derive(Clone, Debug)]
pub struct PluginHookSession {
	/// The session identifier (the journal's stem).
	pub session_id:   Str,
	/// The session journal (`transcript_path`).
	pub transcript:   PathBuf,
	/// Project root: the hook's working directory and `CLAUDE_PROJECT_DIR`.
	pub project_root: PathBuf,
	/// omp's data directory, holding each plugin's `${CLAUDE_PLUGIN_DATA}`.
	pub data_dir:     PathBuf,
	/// Whether this kernel runs a subagent: `SubagentStop` replaces `Stop`,
	/// and prompt/session events stay with the main session.
	pub subagent:     bool,
}

/// The in-process hook host running every enabled plugin's hooks.
pub struct PluginHookHost {
	this:               Weak<Self>,
	client:             EnvClient,
	session:            PluginHookSession,
	hooks:              Box<[LoadedHook]>,
	mailbox:            OnceLock<flume::Sender<Up>>,
	stop_continuations: AtomicU32,
	session_context:    Mutex<Vec<HookContext>>,
}

impl PluginHookHost {
	/// Builds the host over every Claude-layout plugin's hooks; `None` when
	/// no enabled plugin declares a hook this session runs.
	#[must_use]
	pub fn new(
		client: EnvClient,
		session: PluginHookSession,
		plugins: &ClaudePlugins,
	) -> Option<Arc<Self>> {
		let data_dir = session.data_dir.as_path();
		let hooks = plugins
			.plugins
			.iter()
			.filter_map(|plugin| Some((plugin, plugin.claude_components()?)))
			.flat_map(|(plugin, components)| {
				components.hooks.iter().map(move |hook| LoadedHook {
					plugin: plugin.id.clone(),
					root:   plugin.root.clone(),
					data:   plugin_data_dir(data_dir, &plugin.id),
					hook:   hook.clone(),
				})
			})
			.filter(|loaded| runs_in(loaded.hook.event, session.subagent))
			.collect::<Box<[_]>>();
		(!hooks.is_empty()).then(|| {
			Arc::new_cyclic(|this| Self {
				this: this.clone(),
				client,
				session,
				hooks,
				mailbox: OnceLock::new(),
				stop_continuations: AtomicU32::new(0),
				session_context: Mutex::new(Vec::new()),
			})
		})
	}

	/// Binds the kernel mailbox notices and tool-seam context travel on.
	pub fn bind_mailbox(&self, mailbox: flume::Sender<Up>) {
		let _ = self.mailbox.set(mailbox);
	}

	/// Attaches this host to `gate` for every seam its hooks use.
	pub fn attach(self: &Arc<Self>, gate: &HookGate) {
		gate.attach_native(Arc::clone(self) as Arc<dyn NativeHookHost>, &self.events());
	}

	/// The hook events this host answers.
	#[must_use]
	pub fn events(&self) -> SmallVec<HookEventId, 8> {
		let mut events = SmallVec::<HookEventId, 8>::new();
		let mut push = |event| {
			if !events.contains(&event) {
				events.push(event);
			}
		};
		for loaded in &*self.hooks {
			push(seam_event(loaded.hook.seam));
			// Stop's continuation cap resets, and SessionStart context is
			// delivered, when the next prompt starts.
			if matches!(loaded.hook.seam, HookSeam::AgentSettled | HookSeam::SessionStart) {
				push(HookEventId::HookEventBeforeAgentStart);
			}
		}
		events
	}

	fn hooks_for<'a>(
		&'a self,
		event: ClaudeHookEvent,
		subject: Subject<'a>,
	) -> impl Iterator<Item = &'a LoadedHook> + 'a {
		self.hooks.iter().filter(move |loaded| {
			loaded.hook.event == event
				&& match subject {
					Subject::Tool(tool) => loaded.hook.matcher.matches_tool(tool),
					Subject::Value(value) => loaded.hook.matcher.matches(value),
					Subject::None => true,
				}
		})
	}

	/// Runs every matching hook concurrently; detached (`async`) hooks run in
	/// the background and contribute nothing.
	async fn run_all(
		&self,
		event: ClaudeHookEvent,
		subject: Subject<'_>,
		fields: EventFields<'_>,
	) -> Vec<HookEffect> {
		// The payload is serialized once, on the first matching hook.
		let mut input = None::<Bytes>;
		let mut foreground = Vec::new();
		for loaded in self.hooks_for(event, subject) {
			let bytes = match &input {
				Some(bytes) => bytes.clone(),
				None => {
					let payload = HookInput {
						session_id:      &self.session.session_id,
						transcript_path: &self.session.transcript,
						cwd:             &self.session.project_root,
						permission_mode: "default",
						hook_event_name: event.into(),
						fields:          &fields,
					};
					let Ok(bytes) = serde_json::to_vec(&payload).map(Bytes::from) else {
						return Vec::new();
					};
					input.insert(bytes).clone()
				},
			};
			if !loaded.hook.command.detached {
				foreground.push(self.effect_of(loaded, bytes));
			} else if let Some(host) = self.this.upgrade() {
				let loaded = loaded.clone();
				tokio::spawn(async move {
					if let Err(error) = host.run(&loaded, bytes).await {
						tracing::debug!(
							error = &error as &dyn std::error::Error,
							"background plugin hook failed"
						);
					}
				});
			}
		}
		futures::future::join_all(foreground).await
	}

	async fn effect_of(&self, loaded: &LoadedHook, input: Bytes) -> HookEffect {
		let event = loaded.hook.event;
		let mut effect = HookEffect::new(loaded.plugin.clone());
		match self.run(loaded, input).await {
			Ok(finished) => effect.interpret(event, finished),
			Err(error) => effect.notices.push(render(&error)),
		}
		effect
	}

	/// Runs one hook command in its own in-process shell session.
	async fn run(&self, loaded: &LoadedHook, input: Bytes) -> Result<Finished, PluginHookError> {
		let event = loaded.hook.event;
		let start = |source| PluginHookError::Start { plugin: loaded.plugin.clone(), event, source };
		let _ = std::fs::create_dir_all(&loaded.data);
		let script = self.script(loaded);
		let cwd = EnvPath::new(Str::new(self.session.project_root.to_string_lossy()))
			.unwrap_or_else(|_| EnvPath::new(Str::new_static(".")).expect("non-empty literal"));
		let mut set = BTreeMap::new();
		for (name, value) in [
			("PWD", &self.session.project_root),
			("CLAUDE_PROJECT_DIR", &self.session.project_root),
			("CLAUDE_PLUGIN_ROOT", &loaded.root),
			("CLAUDE_PLUGIN_DATA", &loaded.data),
		] {
			set.insert(name.to_owned(), value.to_string_lossy().into_owned());
		}
		let opened = self
			.client
			.open_session(&cwd, OpenSessionRequest {
				env_delta: Some(EnvironmentDelta { set, unset: Vec::new(), props: None }),
				..OpenSessionRequest::default()
			})
			.await
			.map_err(start)?;
		let session = opened.session.clone();
		let outcome = match self
			.client
			.exec(ExecRequest {
				session: opened.session,
				source: Some(Script { text: script, ..Script::default() }),
				..ExecRequest::default()
			})
			.await
		{
			Ok(mut run) => {
				let timeout = loaded.hook.command.timeout;
				// Dropping the run on timeout tears the command's process
				// groups down TERM-then-KILL in the environment.
				match tokio::time::timeout(timeout, drive(&self.client, &mut run, input)).await {
					Ok(result) => result.map_err(start),
					Err(_) => {
						Err(PluginHookError::TimedOut { plugin: loaded.plugin.clone(), event, timeout })
					},
				}
			},
			Err(source) => Err(start(source)),
		};
		let _ = self
			.client
			.close_session(CloseSessionRequest { session, ..CloseSessionRequest::default() })
			.await;
		let finished = outcome?;
		if finished.code.is_none() {
			return Err(PluginHookError::NoExitStatus {
				plugin: loaded.plugin.clone(),
				event,
				outcome: finished.outcome,
			});
		}
		Ok(finished)
	}

	/// The script the in-process shell runs: the expanded shell-form command,
	/// or the exec-form command and arguments, each one quoted word.
	fn script(&self, loaded: &LoadedHook) -> String {
		let expand = |value: &Str| {
			let expanded = expand_plugin_vars(value.clone(), &loaded.root, Some(&loaded.data));
			if expanded.contains("${CLAUDE_PROJECT_DIR}") {
				expanded.replace(
					"${CLAUDE_PROJECT_DIR}",
					self.session.project_root.to_string_lossy().as_ref(),
				)
			} else {
				expanded.to_string()
			}
		};
		let command = &loaded.hook.command;
		match &command.args {
			None => expand(&command.command),
			Some(args) => {
				let mut script = String::new();
				for word in std::iter::once(&command.command).chain(args.iter()) {
					if !script.is_empty() {
						script.push(' ');
					}
					push_quoted(&mut script, &expand(word));
				}
				script
			},
		}
	}

	fn post(&self, message: Up) {
		match self.mailbox.get() {
			Some(mailbox) => {
				let _ = mailbox.send(message);
			},
			None => tracing::debug!("plugin hook notice before the kernel mailbox was bound"),
		}
	}

	/// Journals user-visible notices and, for `continue: false`, the stop.
	fn publish(&self, effects: &[HookEffect]) {
		for effect in effects {
			for body in effect.notices.iter().chain(effect.halt.iter()) {
				self.post(Up::Env(EnvEvent::Notice {
					kind: Str::new_static("hook"),
					name: Some(effect.plugin.clone()),
					body: body.clone(),
				}));
			}
		}
	}

	fn halted(effects: &[HookEffect]) -> Option<Str> {
		effects.iter().find_map(|effect| effect.halt.clone())
	}

	async fn tool_call(&self, payload: &JsonValue) -> NativeReply {
		let Ok(call) = ToolCallView::deserialize(payload) else {
			return NativeReply::defer();
		};
		let tool = call.target.name.as_str();
		let input = ToolInput::of(tool, &call.args, &self.session.project_root);
		let effects = self
			.run_all(ClaudeHookEvent::PreToolUse, Subject::Tool(tool), EventFields::PreTool {
				tool_name:   claude_tool_name(tool),
				tool_input:  input,
				tool_use_id: &call.call_id,
			})
			.await;
		self.publish(&effects);
		// PreToolUse context lands next to the tool result: the mailbox is
		// drained into the turn before the next inference.
		for effect in &effects {
			for body in &effect.context {
				self.post(Up::Env(EnvEvent::CustomMessage(hook_message(&effect.plugin, body))));
			}
		}
		if let Some(reason) = Self::halted(&effects) {
			self.post(Up::Interrupt);
			return NativeReply { verdict: NativeVerdict::Deny(reason), context: Vec::new() };
		}
		match effects.into_iter().find_map(|effect| effect.block) {
			Some(reason) => NativeReply { verdict: NativeVerdict::Deny(reason), context: Vec::new() },
			None => NativeReply::defer(),
		}
	}

	async fn tool_result(&self, payload: &JsonValue) -> NativeReply {
		let Ok(mut result) = ToolResultView::deserialize(payload) else {
			return NativeReply::defer();
		};
		let tool = result.target.name.clone();
		let input = ToolInput::of(&tool, &result.target.args, &self.session.project_root);
		let (event, fields) = match result.outcome.as_str() {
			"ok" => (ClaudeHookEvent::PostToolUse, EventFields::PostTool {
				tool_name:     claude_tool_name(&tool),
				tool_input:    input,
				tool_use_id:   &result.call_id,
				tool_response: result.payload.as_ref().unwrap_or(&JsonValue::Null),
			}),
			"faulted" => (ClaudeHookEvent::PostToolUseFailure, EventFields::ToolFailure {
				tool_name:    claude_tool_name(&tool),
				tool_input:   input,
				tool_use_id:  &result.call_id,
				error:        fault_text(result.fault.as_ref()),
				is_interrupt: false,
			}),
			_ => return NativeReply::defer(),
		};
		let effects = self.run_all(event, Subject::Tool(&tool), fields).await;
		self.publish(&effects);
		if Self::halted(&effects).is_some() {
			self.post(Up::Interrupt);
		}
		let mut changed = false;
		for effect in effects {
			// The tool already ran: a block and any context annotate its
			// result where the model reads it.
			for text in effect.block.into_iter().chain(effect.context) {
				if let Ok(annotation) =
					serde_json::to_value(Annotation { kind: Str::new_static("hook"), text })
				{
					result.annotate.push(annotation);
					changed = true;
				}
			}
		}
		if !changed {
			return NativeReply::defer();
		}
		match serde_json::to_value(&result) {
			Ok(payload) => {
				NativeReply { verdict: NativeVerdict::Modify(payload), context: Vec::new() }
			},
			Err(_) => NativeReply::defer(),
		}
	}

	async fn before_agent_start(&self, payload: &JsonValue) -> NativeReply {
		self.stop_continuations.store(0, Ordering::Relaxed);
		let mut context = std::mem::take(&mut *self.session_context.lock());
		if self.session.subagent {
			return NativeReply { verdict: NativeVerdict::Defer, context };
		}
		let Ok(prompt) = PromptView::deserialize(payload) else {
			return NativeReply { verdict: NativeVerdict::Defer, context };
		};
		let effects = self
			.run_all(ClaudeHookEvent::UserPromptSubmit, Subject::None, EventFields::Prompt {
				prompt: &prompt.text,
			})
			.await;
		self.publish(&effects);
		if let Some(reason) = Self::halted(&effects) {
			return NativeReply { verdict: NativeVerdict::Deny(reason), context: Vec::new() };
		}
		let mut block = None;
		for effect in effects {
			block = block.or(effect.block);
			context.extend(
				effect
					.context
					.into_iter()
					.map(|body| HookContext { source: effect.plugin.clone(), body }),
			);
		}
		match block {
			Some(reason) => NativeReply { verdict: NativeVerdict::Deny(reason), context: Vec::new() },
			None => NativeReply { verdict: NativeVerdict::Defer, context },
		}
	}

	async fn agent_settled(&self) -> NativeReply {
		let event = if self.session.subagent {
			ClaudeHookEvent::SubagentStop
		} else {
			ClaudeHookEvent::Stop
		};
		let used = self.stop_continuations.load(Ordering::Relaxed);
		if used >= STOP_CONTINUATION_CAP {
			self.stop_continuations.store(0, Ordering::Relaxed);
			return NativeReply::defer();
		}
		let effects = self
			.run_all(event, Subject::None, EventFields::Stop { stop_hook_active: used > 0 })
			.await;
		self.publish(&effects);
		if Self::halted(&effects).is_some() {
			self.stop_continuations.store(0, Ordering::Relaxed);
			return NativeReply::defer();
		}
		let context = effects
			.into_iter()
			.flat_map(|effect| {
				let plugin = effect.plugin;
				effect
					.block
					.into_iter()
					.chain(effect.context)
					.map(move |body| HookContext { source: plugin.clone(), body })
			})
			.collect::<Vec<_>>();
		if context.is_empty() {
			self.stop_continuations.store(0, Ordering::Relaxed);
			return NativeReply::defer();
		}
		self.stop_continuations.fetch_add(1, Ordering::Relaxed);
		NativeReply { verdict: NativeVerdict::Continue, context }
	}

	async fn session_start(&self, payload: &JsonValue) -> NativeReply {
		let source = match SessionStartView::deserialize(payload) {
			Ok(SessionStartView { resumed: true }) => "resume",
			_ => "startup",
		};
		let effects = self
			.run_all(
				ClaudeHookEvent::SessionStart,
				Subject::Value(source),
				EventFields::SessionStart { source },
			)
			.await;
		self.publish(&effects);
		let mut pending = self.session_context.lock();
		for effect in effects {
			// Exit 2 only shows stderr to the user here.
			if let Some(reason) = effect.block {
				self.post(Up::Env(EnvEvent::Notice {
					kind: Str::new_static("hook"),
					name: Some(effect.plugin.clone()),
					body: reason,
				}));
			}
			pending.extend(
				effect
					.context
					.into_iter()
					.map(|body| HookContext { source: effect.plugin.clone(), body }),
			);
		}
		NativeReply::defer()
	}

	async fn compaction(&self, payload: &JsonValue) -> NativeReply {
		let Ok(view) = CompactionView::deserialize(payload) else {
			return NativeReply::defer();
		};
		let trigger = if view.reason == "manual" {
			"manual"
		} else {
			"auto"
		};
		let effects = self
			.run_all(ClaudeHookEvent::PreCompact, Subject::Value(trigger), EventFields::PreCompact {
				trigger,
				custom_instructions: view.custom_instructions.as_deref(),
			})
			.await;
		// PreCompact discards `systemMessage` and `continue`.
		match effects.into_iter().find_map(|effect| effect.block) {
			Some(reason) => NativeReply { verdict: NativeVerdict::Deny(reason), context: Vec::new() },
			None => NativeReply::defer(),
		}
	}
}

impl NativeHookHost for PluginHookHost {
	fn decide<'a>(&'a self, event: HookEventId, payload: &'a JsonValue) -> BoxFut<'a, NativeReply> {
		Box::pin(async move {
			match event {
				HookEventId::HookEventToolCall => self.tool_call(payload).await,
				HookEventId::HookEventToolResult => self.tool_result(payload).await,
				HookEventId::HookEventBeforeAgentStart => self.before_agent_start(payload).await,
				HookEventId::HookEventAgentSettled => self.agent_settled().await,
				HookEventId::HookEventSessionStart => self.session_start(payload).await,
				HookEventId::HookEventCompaction => self.compaction(payload).await,
				_ => NativeReply::defer(),
			}
		})
	}
}

/// Whether `event`'s hooks run in a main (`subagent == false`) or subagent
/// kernel.
const fn runs_in(event: ClaudeHookEvent, subagent: bool) -> bool {
	match event {
		ClaudeHookEvent::Stop | ClaudeHookEvent::UserPromptSubmit | ClaudeHookEvent::SessionStart => {
			!subagent
		},
		ClaudeHookEvent::SubagentStop => subagent,
		_ => true,
	}
}

/// The omp hook event a seam runs on.
const fn seam_event(seam: HookSeam) -> HookEventId {
	match seam {
		HookSeam::ToolCall => HookEventId::HookEventToolCall,
		HookSeam::ToolResult => HookEventId::HookEventToolResult,
		HookSeam::BeforeAgentStart => HookEventId::HookEventBeforeAgentStart,
		HookSeam::AgentSettled => HookEventId::HookEventAgentSettled,
		HookSeam::SessionStart => HookEventId::HookEventSessionStart,
		HookSeam::Compaction => HookEventId::HookEventCompaction,
	}
}

fn hook_message(plugin: &Str, body: &Str) -> CustomMessage {
	let mut message = CustomMessage::new(plugin.clone(), body.clone());
	message.kind = CustomMessageKind::Hook;
	message.display = false;
	message
}

/// Renders a typed hook failure and its source chain once, for a notice.
fn render(error: &PluginHookError) -> Str {
	let mut text = String::new();
	let _ = write!(text, "{error}");
	let mut source = std::error::Error::source(error);
	while let Some(cause) = source {
		let _ = write!(text, ": {cause}");
		source = cause.source();
	}
	Str::new(text)
}

/// Appends `word` as one single-quoted shell word.
fn push_quoted(script: &mut String, word: &str) {
	script.push('\'');
	for (index, part) in word.split('\'').enumerate() {
		if index > 0 {
			script.push_str("'\\''");
		}
		script.push_str(part);
	}
	script.push('\'');
}

/// What a hook matcher is tested against.
#[derive(Clone, Copy)]
enum Subject<'a> {
	/// An omp tool family (tool events).
	Tool(&'a str),
	/// A session source or compaction trigger.
	Value(&'a str),
	/// Events without matcher support.
	None,
}

/// One finished command.
struct Finished {
	code:    Option<i32>,
	outcome: ExecOutcome,
	stdout:  Vec<u8>,
	stderr:  Vec<u8>,
}

/// Reads the command's events to its exit while stdin is written out of
/// band: a hook that never reads stdin (or exits first) must not fail the
/// event stream, and a large payload must not stall output.
async fn drive(
	client: &EnvClient,
	run: &mut omp_env::ExecRun,
	input: Bytes,
) -> Result<Finished, ClientError> {
	let (started_tx, started_rx) = flume::bounded::<Bytes>(1);
	let writer = async {
		// The reader drops the sender without an id when the command never
		// started; there is then nothing to write to.
		if let Ok(exec) = started_rx.recv_async().await {
			let control = client.active_exec_control(exec);
			if control.stdin(input).await.is_ok() {
				let _ = control.eof().await;
			}
		}
	};
	let reader = read_events(run, started_tx);
	let ((), finished) = tokio::join!(writer, reader);
	finished
}

async fn read_events(
	run: &mut omp_env::ExecRun,
	started_tx: flume::Sender<Bytes>,
) -> Result<Finished, ClientError> {
	let mut stdout = Vec::new();
	let mut stderr = Vec::new();
	let mut started_tx = Some(started_tx);
	loop {
		match run.next_event().await? {
			Some(ExecEvent::Started(started)) => {
				if let Some(tx) = started_tx.take() {
					let _ = tx.send(started.exec);
				}
			},
			Some(ExecEvent::Output(frame)) => {
				let sink = if frame.channel == OutputChannel::Stderr as i32 {
					&mut stderr
				} else {
					&mut stdout
				};
				let room = OUTPUT_CAP.saturating_sub(sink.len());
				sink.extend_from_slice(&frame.data[..frame.data.len().min(room)]);
			},
			Some(ExecEvent::Exit(exit)) => {
				let status = exit.status.unwrap_or_default();
				let outcome = ExecOutcome::try_from(status.outcome).unwrap_or(ExecOutcome::Unspecified);
				// A non-zero exit is `Failed` with its code; a timeout,
				// cancellation, or sandbox denial has no code to read.
				let code = matches!(outcome, ExecOutcome::Exited | ExecOutcome::Failed)
					.then_some(status.exit_code)
					.flatten();
				return Ok(Finished { code, outcome, stdout, stderr });
			},
			None => {
				return Ok(Finished { code: None, outcome: ExecOutcome::Cancelled, stdout, stderr });
			},
		}
	}
}

/// What one hook's result asks for, before the seam composes it.
struct HookEffect {
	plugin:  Str,
	/// A blocking reason: exit 2's stderr, or a JSON block/deny reason.
	block:   Option<Str>,
	/// Model-visible context.
	context: Vec<Str>,
	/// `continue: false` with its stop reason.
	halt:    Option<Str>,
	/// User-visible notices: `systemMessage` and failures.
	notices: Vec<Str>,
}

impl HookEffect {
	const fn new(plugin: Str) -> Self {
		Self { plugin, block: None, context: Vec::new(), halt: None, notices: Vec::new() }
	}

	fn interpret(&mut self, event: ClaudeHookEvent, finished: Finished) {
		let Some(code) = finished.code else {
			return;
		};
		let stdout = String::from_utf8_lossy(&finished.stdout);
		let stderr = String::from_utf8_lossy(&finished.stderr);
		let trimmed = stdout.trim();
		let output = if trimmed.starts_with('{') && trimmed.ends_with('}') {
			match serde_json::from_str::<HookOutputWire>(trimmed) {
				Ok(output) => Some(output),
				Err(source) => {
					self.notices.push(render(&PluginHookError::MalformedOutput {
						plugin: self.plugin.clone(),
						event,
						source,
					}));
					None
				},
			}
		} else {
			if code == 0
				&& !trimmed.is_empty()
				&& matches!(event, ClaudeHookEvent::UserPromptSubmit | ClaudeHookEvent::SessionStart)
			{
				self.context.push(Str::new(trimmed));
			}
			None
		};
		if let Some(output) = &output {
			self.apply(output);
		}
		if code == 2 {
			if self.block.is_none() {
				let reason = stderr.trim();
				self.block = Some(if reason.is_empty() {
					sf!("blocked by plugin `{}` {event} hook", self.plugin)
				} else {
					Str::new(reason)
				});
			}
		} else if code != 0 && output.is_none() {
			self.notices.push(render(&PluginHookError::NonBlocking {
				plugin: self.plugin.clone(),
				event,
				code,
				stderr: Str::new(stderr.lines().next().unwrap_or_default()),
			}));
		}
	}

	fn apply(&mut self, output: &HookOutputWire) {
		if output.proceed == Some(false) {
			self.halt = Some(
				output
					.stop_reason
					.clone()
					.unwrap_or_else(|| sf!("stopped by plugin `{}` hook", self.plugin)),
			);
		}
		if let Some(message) = &output.system_message {
			self.notices.push(message.clone());
		}
		let specific = output.hook_specific_output.as_ref();
		let permission_deny = specific
			.and_then(|specific| specific.permission_decision)
			.is_some_and(|decision| decision == PermissionWire::Deny);
		let blocked = output.decision == Some(DecisionWire::Block) || permission_deny;
		if blocked {
			let reason = specific
				.filter(|_| permission_deny)
				.and_then(|specific| specific.permission_decision_reason.clone())
				.or_else(|| output.reason.clone())
				.unwrap_or_else(|| sf!("blocked by plugin `{}` hook", self.plugin));
			self.block = Some(reason);
		}
		if let Some(context) = specific.and_then(|specific| specific.additional_context.clone()) {
			self.context.push(context);
		}
	}
}

/// The JSON a hook may print (lenient: unknown fields ignored).
#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct HookOutputWire {
	#[serde(default, rename = "continue")]
	proceed:              Option<bool>,
	#[serde(default)]
	stop_reason:          Option<Str>,
	#[serde(default)]
	system_message:       Option<Str>,
	#[serde(default)]
	decision:             Option<DecisionWire>,
	#[serde(default)]
	reason:               Option<Str>,
	#[serde(default)]
	hook_specific_output: Option<SpecificWire>,
}

/// Top-level `decision`; PreToolUse's deprecated `approve`/`block` too.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "lowercase")]
enum DecisionWire {
	Block,
	Approve,
	#[serde(other)]
	Other,
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SpecificWire {
	#[serde(default)]
	permission_decision:        Option<PermissionWire>,
	#[serde(default)]
	permission_decision_reason: Option<Str>,
	#[serde(default)]
	additional_context:         Option<Str>,
}

/// `permissionDecision`: `deny` blocks; `allow`, `ask`, and `defer` leave
/// the call to omp's own approval policy.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "lowercase")]
enum PermissionWire {
	Allow,
	Deny,
	Ask,
	Defer,
	#[serde(other)]
	Other,
}

/// The stdin payload: common fields plus the event's own.
#[derive(Serialize)]
struct HookInput<'a> {
	session_id:      &'a str,
	transcript_path: &'a Path,
	cwd:             &'a Path,
	permission_mode: &'static str,
	hook_event_name: &'static str,
	#[serde(flatten)]
	fields:          &'a EventFields<'a>,
}

#[derive(Serialize)]
#[serde(untagged)]
enum EventFields<'a> {
	PreTool {
		tool_name:   &'a str,
		tool_input:  ToolInput<'a>,
		tool_use_id: &'a str,
	},
	PostTool {
		tool_name:     &'a str,
		tool_input:    ToolInput<'a>,
		tool_use_id:   &'a str,
		tool_response: &'a JsonValue,
	},
	ToolFailure {
		tool_name:    &'a str,
		tool_input:   ToolInput<'a>,
		tool_use_id:  &'a str,
		error:        Str,
		is_interrupt: bool,
	},
	Prompt {
		prompt: &'a str,
	},
	Stop {
		stop_hook_active: bool,
	},
	SessionStart {
		source: &'static str,
	},
	PreCompact {
		trigger:             &'static str,
		custom_instructions: Option<&'a str>,
	},
}

/// `tool_input` in the Claude tool's shape where omp's arguments map onto it
/// faithfully, else omp's own arguments.
#[derive(Serialize)]
#[serde(untagged)]
enum ToolInput<'a> {
	Bash {
		command: Str,
		#[serde(skip_serializing_if = "Option::is_none")]
		timeout: Option<u64>,
	},
	File {
		file_path: Str,
		#[serde(skip_serializing_if = "Option::is_none")]
		content:   Option<Str>,
	},
	Search {
		pattern: Str,
		#[serde(skip_serializing_if = "Option::is_none")]
		path:    Option<Str>,
	},
	Native(&'a JsonValue),
}

#[derive(Deserialize)]
struct BashArgs {
	command: Str,
	#[serde(default)]
	timeout: Option<f64>,
}

#[derive(Deserialize)]
struct PathArgs {
	path:    Str,
	#[serde(default)]
	content: Option<Str>,
}

#[derive(Deserialize)]
struct GrepArgs {
	pattern: Str,
	#[serde(default)]
	path:    Option<Str>,
}

#[derive(Deserialize)]
struct GlobArgs {
	#[serde(default)]
	path: Option<Str>,
}

impl<'a> ToolInput<'a> {
	fn of(tool: &str, args: &'a JsonValue, root: &Path) -> Self {
		let absolute = |path: Str| {
			if path.contains("://") || Path::new(path.as_str()).is_absolute() {
				path
			} else {
				Str::new(root.join(path.as_str()).to_string_lossy())
			}
		};
		let typed = match tool {
			"bash" => BashArgs::deserialize(args).ok().map(|args| Self::Bash {
				command: args.command,
				timeout: args
					.timeout
					.filter(|seconds| seconds.is_finite() && *seconds >= 0.0)
					.map(|seconds| (seconds * 1000.0) as u64),
			}),
			"read" | "write" => PathArgs::deserialize(args)
				.ok()
				.map(|args| Self::File { file_path: absolute(args.path), content: args.content }),
			"grep" => GrepArgs::deserialize(args)
				.ok()
				.map(|args| Self::Search { pattern: args.pattern, path: args.path }),
			"glob" => GlobArgs::deserialize(args).ok().map(|args| Self::Search {
				pattern: args.path.unwrap_or_else(|| Str::new_static("**/*")),
				path:    None,
			}),
			_ => None,
		};
		typed.unwrap_or(Self::Native(args))
	}
}

/// The `tool_call` admission payload fields hooks read.
#[derive(Deserialize)]
struct ToolCallView {
	call_id: Str,
	target:  TargetView,
	#[serde(default)]
	args:    JsonValue,
}

#[derive(Deserialize, Serialize)]
struct TargetView {
	name: Str,
	#[serde(default)]
	args: JsonValue,
	#[serde(flatten)]
	rest: serde_json::Map<String, JsonValue>,
}

/// The `tool_result` transform payload; unknown fields round-trip.
#[derive(Deserialize, Serialize)]
struct ToolResultView {
	call_id:  Str,
	target:   TargetView,
	outcome:  Str,
	#[serde(default)]
	payload:  Option<JsonValue>,
	#[serde(default)]
	fault:    Option<JsonValue>,
	#[serde(default)]
	annotate: Vec<JsonValue>,
	#[serde(flatten)]
	rest:     serde_json::Map<String, JsonValue>,
}

/// One `tool_result` annotation, rendered as a `<diag>` on the result.
#[derive(Serialize)]
struct Annotation {
	kind: Str,
	text: Str,
}

#[derive(Deserialize)]
struct PromptView {
	text: Str,
}

#[derive(Deserialize)]
struct SessionStartView {
	#[serde(default)]
	resumed: bool,
}

#[derive(Deserialize)]
struct CompactionView {
	reason:              Str,
	#[serde(default)]
	custom_instructions: Option<Str>,
}

#[derive(Deserialize)]
struct FaultView {
	message: Str,
}

fn fault_text(fault: Option<&JsonValue>) -> Str {
	match fault {
		Some(fault) => FaultView::deserialize(fault)
			.map(|view| view.message)
			.unwrap_or_else(|_| Str::new(fault.to_string())),
		None => Str::default(),
	}
}
