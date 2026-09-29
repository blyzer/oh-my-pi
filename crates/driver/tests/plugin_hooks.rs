//! Claude-format plugin hooks through the production seams: an installed
//! plugin resolved from omp's registry, a real project environment, the
//! kernel's hook gate, and each hook command run by the environment's
//! in-process shell. A scripted model calls `bash`; the hooks block it,
//! annotate its result, add prompt context, time out, or never run — never
//! without the operator's approval of the hook's command and trigger.

use std::{
	future::ready,
	path::{Path, PathBuf},
	sync::Arc,
	time::{Duration, Instant},
};

use futures::stream;
use omp_agent::{
	DispatchPolicy, Inference, Kernel, LifecycleHooks, RunControl, SessionShutdown, SessionStart,
	ShutdownReason, StaticPrompt, SwitchReason, TurnInput,
};
use omp_ai::{
	BlockKind, ChatEvent, ChatRequest, ChatStream, Completion, ErrorKind, ErrorPhase,
	ExecutionReceipt, FinishReason, RequestId, ResponseMeta, RetryAction, ToolCall, ToolCallId,
	Usage,
};
use omp_catalog::{ProviderId, RouteId};
use omp_core::Str;
use omp_driver::{
	headless::kernel::{EnvToolExecutor, SettingsAdmission},
	plugin_commands::{approve_launch, blocked_launches, plugin_launches},
	plugin_hooks::{PluginHookHost, PluginHookSession},
	subagent::AgentName,
};
use omp_envd::{
	AttachOptions, ProjectEnvironment, RegistryBridges, mcp::McpConfigPaths,
	tool_settings::ApprovalMode,
};
use omp_ext::{
	claude_plugin::{
		ClaudePlugins, InstallScope, InstalledPluginEntry, InstalledPluginsRegistry, REGISTRY_FILE,
	},
	plugin_command::{PluginCommandBlocked, PluginLaunchKind},
};
use omp_proto::toolhost::v1::HookEventId;
use omp_session::{ComponentRegistry, Session};
use parking_lot::Mutex;

/// One `bash` call, then a closing text turn; every request's messages are
/// recorded so tests can see what reached the model. With `fail`, the first
/// request fails with that provider error instead.
struct BashThenText {
	command:  String,
	turns:    usize,
	requests: Arc<Mutex<Vec<String>>>,
	fail:     Option<ErrorKind>,
}

impl Inference for BashThenText {
	fn chat(
		&mut self,
		request: ChatRequest,
	) -> impl Future<Output = Result<ChatStream, omp_ai::Error>> + Send {
		self.turns += 1;
		self.requests.lock().push(format!("{:?}", request.messages));
		if let Some(kind) = self.fail {
			return ready(Err(omp_ai::Error::new(
				kind,
				ErrorPhase::Planning,
				RetryAction::Never,
				ExecutionReceipt::default(),
			)));
		}
		let meta = ResponseMeta {
			request_id:          RequestId::from("plugin-hooks"),
			provider:            ProviderId::from("test"),
			route:               RouteId::from("test/route"),
			model:               None,
			provider_request_id: None,
			created_at:          std::time::SystemTime::UNIX_EPOCH,
		};
		let events = if self.turns == 1 {
			let arguments =
				serde_json::json!({ "command": self.command, "i": "Proving plugin hooks" });
			let call = ToolCall {
				id:        ToolCallId::from("bash-1"),
				name:      Str::new_static("bash"),
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

/// A workspace, an omp data directory, and one installed plugin.
struct Fixture {
	scratch: tempfile::TempDir,
	root:    PathBuf,
	data:    PathBuf,
	plugin:  PathBuf,
}

impl Fixture {
	/// Installs plugin `hooky@m` (enabled or not) declaring `hooks_json`.
	fn new(hooks_json: &str, enabled: bool) -> Self {
		let scratch = tempfile::tempdir().expect("scratch");
		let root = scratch.path().join("workspace");
		let data = scratch.path().join("data");
		let plugin = scratch.path().join("plugins/hooky");
		std::fs::create_dir_all(&root).expect("workspace");
		std::fs::create_dir_all(plugin.join("hooks")).expect("plugin");
		std::fs::write(plugin.join("hooks/hooks.json"), hooks_json).expect("hooks.json");
		let mut registry = InstalledPluginsRegistry::default();
		registry
			.plugins
			.insert(Str::new_static("hooky@m"), vec![InstalledPluginEntry {
				scope: InstallScope::User,
				install_path: plugin.clone(),
				version: Str::new_static("1.0.0"),
				installed_at: Str::new_static("2026-01-01T00:00:00Z"),
				last_updated: Str::new_static("2026-01-01T00:00:00Z"),
				git_commit_sha: None,
				enabled,
			}]);
		let registry_path = data.join("plugins").join(REGISTRY_FILE);
		std::fs::create_dir_all(registry_path.parent().expect("parent")).expect("plugins dir");
		std::fs::write(&registry_path, serde_json::to_vec(&registry).expect("registry"))
			.expect("registry file");
		Self { scratch, root, data, plugin }
	}

	/// Approves every hook command the plugin currently declares, as
	/// `omp ext trust hooky@m --approve-commands` does.
	fn approved(self) -> Self {
		let plugins = ClaudePlugins::resolve(&self.data, &self.root, None);
		for plugin in &plugins.plugins {
			for launch in plugin_launches(plugin) {
				approve_launch(
					&self.data,
					&plugin.id,
					&plugin.version,
					&launch,
					Str::new_static("cli"),
				)
				.expect("persist approval");
			}
		}
		self
	}

	/// Every hook launch of the plugin the operator has not approved.
	fn blocked(&self) -> Vec<PluginCommandBlocked> {
		let plugins = ClaudePlugins::resolve(&self.data, &self.root, None);
		let agent_plugins = McpConfigPaths::new(&self.scratch.path().join("home/.o2"), &self.root)
			.with_command_approvals(plugins.command_approvals.clone());
		blocked_launches(&plugins, &agent_plugins, &omp_envd::mcp::McpSettings::default())
	}

	/// The plugin's `${CLAUDE_PLUGIN_DATA}`.
	fn plugin_data(&self) -> PathBuf {
		omp_ext::claude_plugin::plugin_data_dir(&self.data, "hooky@m")
	}

	/// Runs one turn whose model calls `bash` with `command`.
	async fn run(&self, command: &str) -> Outcome {
		let harness = self.harness(command, Kind::default()).await;
		harness.turn().await
	}

	/// A kernel and session in the workspace, the plugin host attached.
	async fn harness(&self, command: &str, kind: Kind) -> Harness {
		let plugins = ClaudePlugins::resolve(&self.data, &self.root, None);
		assert!(plugins.diagnostics.is_empty(), "{:?}", plugins.diagnostics);
		let state = self.scratch.path().join("state");
		std::fs::create_dir_all(&state).expect("state");
		let environment = ProjectEnvironment::attach(&self.root, &state, AttachOptions {
			py_eval:            false,
			approval_mode:      Some(ApprovalMode::Yolo),
			trusted_extensions: Vec::new(),
			contributed_values: Vec::new(),
			con:                Arc::new(omp_con::Ctx::new()),
			bridges:            RegistryBridges::default(),
			spawn_idle_timeout: Some(2),
		})
		.await
		.expect("environment");
		let spill =
			omp_journal::blob::BlobStore::open(self.scratch.path().join("artifacts")).expect("spill");
		let requests = Arc::new(Mutex::new(Vec::new()));
		let gate = environment.admission_gate();
		let kernel = Kernel::new(
			BashThenText {
				command:  command.to_owned(),
				turns:    0,
				requests: Arc::clone(&requests),
				fail:     kind.fail,
			},
			environment.registry(),
			DispatchPolicy::new(spill.clone()),
			StaticPrompt(Str::new_static("test")),
		)
		.with_hook_gate(Arc::clone(&gate));
		let approvals = kernel.approval_route();
		let kernel = kernel
			.with_external_executor(Arc::new(EnvToolExecutor::new(
				environment.client().clone(),
				approvals,
			)))
			.with_tool_admission(Arc::new(SettingsAdmission::new(
				&omp_con::Ctx::new(),
				Some(ApprovalMode::Yolo),
			)));
		let journal = self.scratch.path().join("hooks.oms");
		let host = PluginHookHost::new(
			environment.client().clone(),
			PluginHookSession {
				session_id:   Str::new_static("hooks"),
				transcript:   journal.clone(),
				project_root: std::fs::canonicalize(&self.root).expect("root"),
				data_dir:     self.data.clone(),
				subagent:     kind.subagent.is_some(),
				agent:        kind
					.subagent
					.map(|name| AgentName::new(Str::new_static(name))),
				idle_prompt:  kind.idle_prompt,
			},
			&plugins,
		);
		let installed = host.is_some();
		if let Some(host) = host {
			host.bind_mailbox(kernel.mailbox());
			host.attach(&gate);
		}
		let session = Session::create_with_blob_store(&journal, ComponentRegistry::standard(), spill)
			.expect("session");
		Harness { environment, kernel, session, requests, installed }
	}
}

/// What kind of kernel a harness runs.
#[derive(Clone, Copy, Default)]
struct Kind {
	/// A subagent of this agent class.
	subagent:    Option<&'static str>,
	/// The model's first request fails with this provider error.
	fail:        Option<ErrorKind>,
	/// The idle wait before `idle_prompt`; `None` never raises it.
	idle_prompt: Option<Duration>,
}

/// A live kernel, its environment, and its session.
struct Harness {
	environment: ProjectEnvironment,
	kernel:      Kernel<BashThenText>,
	session:     Session,
	requests:    Arc<Mutex<Vec<String>>>,
	installed:   bool,
}

impl Harness {
	fn lifecycle(&self) -> LifecycleHooks {
		self
			.kernel
			.lifecycle_hooks()
			.expect("the kernel has a hook gate")
	}

	/// Runs one turn and settles; a failing turn is an outcome, not a panic.
	async fn run_turn(&mut self) -> Duration {
		let started = Instant::now();
		let _ = tokio::time::timeout(
			Duration::from_secs(90),
			self.kernel.run_turn(
				&mut self.session,
				TurnInput { text: Str::new_static("run it"), attachments: Vec::new() },
				RunControl::default(),
			),
		)
		.await
		.expect("turn settles");
		started.elapsed()
	}

	/// Runs one turn that must succeed, then tears the kernel down.
	async fn turn(mut self) -> Outcome {
		let started = Instant::now();
		tokio::time::timeout(
			Duration::from_secs(90),
			self.kernel.run_turn(
				&mut self.session,
				TurnInput { text: Str::new_static("run it"), attachments: Vec::new() },
				RunControl::default(),
			),
		)
		.await
		.expect("turn settles")
		.expect("turn");
		let elapsed = started.elapsed();
		self.finish(elapsed)
	}

	fn finish(self, elapsed: Duration) -> Outcome {
		let journal = std::fs::read_to_string(self.session.journal_path()).expect("journal");
		let Self { environment, kernel, session, requests, installed } = self;
		drop(kernel);
		drop(session);
		drop(environment);
		let requests = requests.lock().clone();
		Outcome { journal, requests, elapsed, installed }
	}
}

/// Waits (bounded) for a background hook to write `path`.
async fn written(path: &Path) -> String {
	let deadline = Instant::now() + Duration::from_secs(30);
	loop {
		if let Ok(text) = std::fs::read_to_string(path)
			&& !text.is_empty()
		{
			return text;
		}
		assert!(Instant::now() < deadline, "{} was never written", path.display());
		tokio::time::sleep(Duration::from_millis(50)).await;
	}
}

fn json(text: &str) -> serde_json::Value {
	serde_json::from_str(text).unwrap_or_else(|error| panic!("hook stdin is JSON ({error}): {text}"))
}

struct Outcome {
	journal:   String,
	requests:  Vec<String>,
	elapsed:   Duration,
	installed: bool,
}

fn read(path: &Path) -> String {
	std::fs::read_to_string(path).unwrap_or_default()
}

const BASH: &str = "echo hi-from-bash; echo ran > marker.txt";

#[tokio::test]
async fn pre_tool_use_exit_2_blocks_the_mapped_tool_with_stderr_as_the_reason() {
	let fixture = Fixture::new(
		r#"{"hooks":{"PreToolUse":[{"matcher":"Bash","hooks":[{"type":"command",
			"command":"cat > \"$CLAUDE_PLUGIN_DATA/pre.json\"; echo 'rm is not allowed here' >&2; exit 2"}]}]}}"#,
		true,
	)
	.approved();
	let outcome = fixture.run(BASH).await;
	assert!(!fixture.root.join("marker.txt").exists(), "the blocked bash never ran");
	assert!(
		outcome.journal.contains("rm is not allowed here"),
		"the model reads the block reason: {}",
		outcome.journal
	);
	let input = read(&fixture.plugin_data().join("pre.json"));
	let input: serde_json::Value = serde_json::from_str(&input).expect("stdin payload is JSON");
	assert_eq!(input["hook_event_name"], "PreToolUse");
	assert_eq!(input["tool_name"], "Bash");
	assert_eq!(input["tool_input"]["command"], BASH);
	assert_eq!(input["tool_use_id"], "bash-1");
	assert_eq!(input["session_id"], "hooks");
}

#[tokio::test]
async fn pre_tool_use_json_decision_block_blocks_the_mapped_tool() {
	let fixture = Fixture::new(
		r#"{"hooks":{"PreToolUse":[{"matcher":"Edit|Bash","hooks":[{"type":"command",
			"command":"printf '%s' '{\"decision\":\"block\",\"reason\":\"json says no\"}'"}]}]}}"#,
		true,
	)
	.approved();
	let outcome = fixture.run(BASH).await;
	eprintln!("DEBUGJOURNAL {}", outcome.journal);
	assert!(!fixture.root.join("marker.txt").exists(), "the blocked bash never ran");
	assert!(outcome.journal.contains("json says no"), "{}", outcome.journal);
}

#[tokio::test]
async fn post_tool_use_receives_the_tool_response_and_annotates_the_result() {
	let fixture = Fixture::new(
		r#"{"hooks":{"PostToolUse":[{"matcher":"Bash","hooks":[{"type":"command",
			"command":"cat > \"$CLAUDE_PLUGIN_DATA/post.json\"; printf '%s' '{\"hookSpecificOutput\":{\"hookEventName\":\"PostToolUse\",\"additionalContext\":\"post-context-7\"}}'"}]}]}}"#,
		true,
	)
	.approved();
	let outcome = fixture.run(BASH).await;
	assert!(fixture.root.join("marker.txt").exists(), "PostToolUse never blocks");
	let input = read(&fixture.plugin_data().join("post.json"));
	let input: serde_json::Value = serde_json::from_str(&input).expect("stdin payload is JSON");
	assert_eq!(input["hook_event_name"], "PostToolUse");
	assert_eq!(input["tool_name"], "Bash");
	assert!(
		input["tool_response"].to_string().contains("hi-from-bash"),
		"the hook reads the tool's output: {input}"
	);
	assert!(
		outcome
			.requests
			.last()
			.is_some_and(|request| request.contains("post-context-7")),
		"the context annotates the result the model reads: {:?}",
		outcome.requests
	);
}

#[tokio::test]
async fn user_prompt_submit_context_reaches_the_model_as_hook_context() {
	let fixture = Fixture::new(
		r#"{"hooks":{"UserPromptSubmit":[{"hooks":[{"type":"command",
			"command":"printf '%s' '{\"hookSpecificOutput\":{\"hookEventName\":\"UserPromptSubmit\",\"additionalContext\":\"prompt-context-42\"},\"systemMessage\":\"hello from hooky\"}'"}]}]}}"#,
		true,
	)
	.approved();
	let outcome = fixture.run(BASH).await;
	assert!(
		outcome.requests[0].contains("prompt-context-42"),
		"the first request carries the context: {:?}",
		outcome.requests
	);
	assert!(outcome.journal.contains("prompt-context-42"));
	assert!(
		outcome.journal.contains("hello from hooky"),
		"systemMessage is a hook notice: {}",
		outcome.journal
	);
}

#[tokio::test]
async fn a_hook_past_its_timeout_is_terminated_and_reported_without_blocking() {
	let fixture = Fixture::new(
		r#"{"hooks":{"PreToolUse":[{"matcher":"Bash","hooks":[{"type":"command",
			"command":"sleep 30; exit 2","timeout":1}]}]}}"#,
		true,
	)
	.approved();
	let outcome = fixture.run(BASH).await;
	assert!(fixture.root.join("marker.txt").exists(), "a timed-out hook renders no decision");
	assert!(
		outcome.elapsed < Duration::from_secs(20),
		"terminated at its timeout: {:?}",
		outcome.elapsed
	);
	assert!(outcome.journal.contains("timed out after 1000ms"), "{}", outcome.journal);
}

#[tokio::test]
async fn a_disabled_plugin_contributes_no_hooks() {
	let fixture = Fixture::new(
		r#"{"hooks":{"PreToolUse":[{"hooks":[{"type":"command",
			"command":"touch \"$CLAUDE_PLUGIN_DATA/ran\"; exit 2"}]}]}}"#,
		false,
	);
	let outcome = fixture.run(BASH).await;
	assert!(!outcome.installed, "no enabled plugin, no hook host");
	assert!(fixture.root.join("marker.txt").exists(), "nothing blocked the call");
	assert!(!fixture.plugin_data().join("ran").exists(), "the hook never ran");
}

#[tokio::test]
async fn commands_run_in_the_in_process_shell_with_the_plugin_root_expanded_and_exported() {
	let fixture = Fixture::new(
		r#"{"hooks":{"PreToolUse":[{"matcher":"Bash","hooks":[{"type":"command",
			"command":"printf '%s|%s|%s|%s' \"${CLAUDE_PLUGIN_ROOT}\" \"$CLAUDE_PLUGIN_ROOT\" \"$(type -t cat)\" \"$(pwd):$PWD\" > \"${CLAUDE_PLUGIN_ROOT}/env.txt\""},
			{"type":"command","command":"touch","args":["${CLAUDE_PLUGIN_ROOT}/it's exec form.txt"]}]}]}}"#,
		true,
	)
	.approved();
	fixture.run(BASH).await;
	let root = std::fs::canonicalize(&fixture.plugin).expect("plugin root");
	let workspace = std::fs::canonicalize(&fixture.root).expect("workspace");
	assert!(
		root.join("it's exec form.txt").exists(),
		"exec form expands `${{CLAUDE_PLUGIN_ROOT}}` in args and keeps each arg one word"
	);
	let recorded = read(&root.join("env.txt"));
	let fields = recorded.split('|').collect::<Vec<_>>();
	assert_eq!(fields.len(), 4, "{recorded}");
	assert_eq!(fields[0], root.to_string_lossy(), "`${{CLAUDE_PLUGIN_ROOT}}` is expanded");
	assert_eq!(fields[1], root.to_string_lossy(), "`CLAUDE_PLUGIN_ROOT` is exported");
	assert_eq!(
		fields[2], "builtin",
		"omp's in-process interpreter (builtin coreutils) ran the hook"
	);
	let workspace = workspace.to_string_lossy();
	assert_eq!(fields[3], format!("{workspace}:{workspace}"), "the hook runs in the project root");
}

#[tokio::test]
async fn a_blocking_stop_hook_continues_the_loop_with_its_reason() {
	let fixture = Fixture::new(
		r#"{"hooks":{"Stop":[{"hooks":[{"type":"command",
			"command":"if [ -f \"$CLAUDE_PLUGIN_DATA/once\" ]; then exit 0; fi; touch \"$CLAUDE_PLUGIN_DATA/once\"; echo 'run the tests first' >&2; exit 2"}]}]}}"#,
		true,
	)
	.approved();
	let outcome = fixture.run(BASH).await;
	assert_eq!(outcome.requests.len(), 3, "the stop was blocked once: {:?}", outcome.requests);
	assert!(
		outcome.requests[2].contains("run the tests first"),
		"the continuation carries the reason: {:?}",
		outcome.requests
	);
}

const GUARD: &str = r#"{"hooks":{"PreToolUse":[{"matcher":"Bash","hooks":[{"type":"command",
	"command":"touch \"$CLAUDE_PLUGIN_DATA/ran\"; echo 'guarded' >&2; exit 2"}]}]}}"#;

#[tokio::test]
async fn an_unapproved_hook_is_never_registered_and_is_named() {
	let fixture = Fixture::new(GUARD, true);
	let blocked = fixture.blocked();
	let [hook] = &blocked[..] else {
		panic!("the hook awaits approval: {blocked:?}");
	};
	assert_eq!(hook.kind, PluginLaunchKind::Hook);
	assert_eq!(hook.plugin, "hooky@m");
	assert_eq!(hook.server, "PreToolUse Bash", "the notice names the event and matcher");
	let notice = hook.to_string();
	assert!(notice.contains(r#"would run `touch "$CLAUDE_PLUGIN_DATA/ran";"#), "{notice}");
	assert!(
		notice.contains(&format!("omp ext trust hooky@m --approve-command {}", hook.digest)),
		"{notice}"
	);

	let outcome = fixture.run(BASH).await;
	assert!(!outcome.installed, "no approved hook, no hook host");
	assert!(fixture.root.join("marker.txt").exists(), "nothing blocked the call");
	assert!(!fixture.plugin_data().join("ran").exists(), "the hook never ran");
	assert!(!outcome.journal.contains("guarded"), "{}", outcome.journal);
}

#[tokio::test]
async fn an_approved_hook_is_asked_again_when_its_command_or_trigger_changes() {
	// Every other test here runs an approved hook.
	let fixture = Fixture::new(GUARD, true).approved();
	assert!(fixture.blocked().is_empty());

	// Widening the matcher changes when the command runs: asked again.
	let widened = GUARD.replace(r#""matcher":"Bash""#, r#""matcher":"Bash|Edit""#);
	std::fs::write(fixture.plugin.join("hooks/hooks.json"), &widened).expect("widen");
	assert_eq!(
		fixture
			.blocked()
			.iter()
			.map(|blocked| blocked.server.as_str())
			.collect::<Vec<_>>(),
		["PreToolUse Bash|Edit"]
	);

	// A changed command is a new approval; the old one admits nothing.
	let changed = GUARD.replace("guarded", "changed");
	std::fs::write(fixture.plugin.join("hooks/hooks.json"), &changed).expect("change");
	assert_eq!(fixture.blocked().len(), 1);
	let outcome = fixture.run(BASH).await;
	assert!(!outcome.installed, "the changed hook is not registered");
	assert!(fixture.root.join("marker.txt").exists(), "nothing blocked the call");
	assert!(!fixture.plugin_data().join("ran").exists(), "the changed hook never ran");
}

#[tokio::test]
async fn session_end_runs_once_at_the_end_with_its_payload_and_cannot_outlive_the_budget() {
	let fixture = Fixture::new(
		r#"{"hooks":{"SessionEnd":[{"hooks":[{"type":"command","timeout":60,
			"command":"cat > \"$CLAUDE_PLUGIN_DATA/end.json\"; echo end >> \"$CLAUDE_PLUGIN_DATA/ends\"; sleep 30"}]}]}}"#,
		true,
	)
	.approved();
	let harness = fixture.harness(BASH, Kind::default()).await;
	assert!(harness.installed);
	let started = Instant::now();
	harness
		.lifecycle()
		.session_shutdown(&SessionShutdown::new(&harness.session, ShutdownReason::UserExit))
		.await;
	let waited = started.elapsed();
	assert!(
		waited < omp_agent::SESSION_SHUTDOWN_BUDGET + Duration::from_secs(2),
		"a hook sleeping past the budget does not hold the end: {waited:?}"
	);
	let input = json(&read(&fixture.plugin_data().join("end.json")));
	assert_eq!(input["hook_event_name"], "SessionEnd");
	assert_eq!(input["reason"], "prompt_input_exit");
	assert_eq!(input["session_id"], "hooks");
	assert!(
		input["transcript_path"]
			.as_str()
			.is_some_and(|path| path.ends_with("hooks.oms")),
		"{input}"
	);
	let root = std::fs::canonicalize(&fixture.root).expect("root");
	assert_eq!(input["cwd"], root.to_string_lossy().as_ref());
	assert_eq!(read(&fixture.plugin_data().join("ends")), "end\n", "it ran exactly once");
	drop(harness);
}

#[tokio::test]
async fn a_switch_ends_the_session_as_clear_and_the_host_follows_the_next_session() {
	let fixture = Fixture::new(
		r#"{"hooks":{"SessionEnd":[{"hooks":[{"type":"command",
			"command":"cat >> \"$CLAUDE_PLUGIN_DATA/ends.jsonl\"; echo >> \"$CLAUDE_PLUGIN_DATA/ends.jsonl\""}]}]}}"#,
		true,
	)
	.approved();
	let harness = fixture.harness(BASH, Kind::default()).await;
	let next_path = fixture.scratch.path().join("next.oms");
	let next = Session::create(&next_path, ComponentRegistry::standard()).expect("next session");
	let lifecycle = harness.lifecycle();
	lifecycle
		.session_switch(&harness.session, &next, Some(SwitchReason::New))
		.await;
	lifecycle
		.session_switch(&next, &harness.session, Some(SwitchReason::Resume))
		.await;
	let ends = read(&fixture.plugin_data().join("ends.jsonl"));
	let ends = ends
		.lines()
		.filter(|line| !line.trim().is_empty())
		.map(json)
		.collect::<Vec<_>>();
	assert_eq!(ends.len(), 2, "{ends:?}");
	assert_eq!(ends[0]["reason"], "clear", "`/new` is Claude Code's `/clear`");
	assert_eq!(ends[0]["session_id"], "hooks");
	assert_eq!(ends[1]["reason"], "resume");
	assert_eq!(ends[1]["session_id"], "next", "the host followed the switch");
	assert!(
		ends[1]["transcript_path"]
			.as_str()
			.is_some_and(|path| path.ends_with("next.oms")),
		"{:?}",
		ends[1]
	);
	drop(harness);
}

/// A switch away from a session that already ended (ACP `session/close`)
/// runs no second `SessionEnd`, yet the host still follows it: a later hook
/// names the next session. The plugin declares no `SessionEnd` hook, so the
/// host never sees `session_shutdown` at all.
#[tokio::test]
async fn the_host_follows_every_switch_even_when_no_session_end_runs() {
	let fixture = Fixture::new(
		r#"{"hooks":{"UserPromptSubmit":[{"hooks":[{"type":"command",
			"command":"cat > \"$CLAUDE_PLUGIN_DATA/prompt.json\""}]}]}}"#,
		true,
	)
	.approved();
	let mut harness = fixture.harness(BASH, Kind::default()).await;
	assert!(harness.installed);
	let next_path = fixture.scratch.path().join("next.oms");
	let next = Session::create(&next_path, ComponentRegistry::standard()).expect("next session");
	harness
		.lifecycle()
		.session_switch(&harness.session, &next, None)
		.await;
	harness.session = next;
	harness.run_turn().await;
	let input = json(&read(&fixture.plugin_data().join("prompt.json")));
	assert_eq!(input["hook_event_name"], "UserPromptSubmit");
	assert_eq!(input["session_id"], "next", "the host followed the switch: {input}");
	assert!(
		input["transcript_path"]
			.as_str()
			.is_some_and(|path| path.ends_with("next.oms")),
		"{input}"
	);
	drop(harness);
}

#[tokio::test]
async fn a_session_end_matcher_filters_on_the_reason() {
	let fixture = Fixture::new(
		r#"{"hooks":{"SessionEnd":[{"matcher":"clear","hooks":[{"type":"command",
			"command":"touch \"$CLAUDE_PLUGIN_DATA/cleared\""}]}]}}"#,
		true,
	)
	.approved();
	let harness = fixture.harness(BASH, Kind::default()).await;
	harness
		.lifecycle()
		.session_shutdown(&SessionShutdown::new(&harness.session, ShutdownReason::Signal))
		.await;
	assert!(!fixture.plugin_data().join("cleared").exists(), "a signal is `other`, not `clear`");
	drop(harness);
}

#[tokio::test]
async fn stop_failure_runs_when_a_turn_fails_on_a_provider_error() {
	let fixture = Fixture::new(
		r#"{"hooks":{"StopFailure":[{"matcher":"rate_limit","hooks":[{"type":"command",
			"command":"cat > \"$CLAUDE_PLUGIN_DATA/failure.json\""}]}],
			"Stop":[{"hooks":[{"type":"command","command":"touch \"$CLAUDE_PLUGIN_DATA/stopped\""}]}]}}"#,
		true,
	)
	.approved();
	let mut harness = fixture
		.harness(BASH, Kind { fail: Some(ErrorKind::RateLimited), ..Kind::default() })
		.await;
	harness.run_turn().await;
	let input = json(&written(&fixture.plugin_data().join("failure.json")).await);
	assert_eq!(input["hook_event_name"], "StopFailure");
	assert_eq!(input["error"], "rate_limit");
	assert_eq!(input["error_details"], "rate_limited");
	assert!(!fixture.plugin_data().join("stopped").exists(), "StopFailure runs instead of Stop");
	drop(harness);
}

#[tokio::test]
async fn post_compact_and_notification_run_at_their_observations() {
	let fixture = Fixture::new(
		r#"{"hooks":{"PostCompact":[{"matcher":"manual","hooks":[{"type":"command",
			"command":"cat > \"$CLAUDE_PLUGIN_DATA/compact.json\""}]}],
			"Notification":[{"matcher":"permission_prompt","hooks":[{"type":"command",
			"command":"cat > \"$CLAUDE_PLUGIN_DATA/notification.json\""}]}]}}"#,
		true,
	)
	.approved();
	let harness = fixture.harness(BASH, Kind::default()).await;
	let lifecycle = harness.lifecycle();
	// The payloads the compaction Director and the approval route publish.
	lifecycle
		.notify(
			HookEventId::HookEventCompactionDone,
			serde_json::json!({
				"preparation_id": "1", "tiers_run": ["local"], "from_extension": null,
				"tokens_before": 10, "tokens_after": 2, "first_kept_id": "1", "epoch": 1,
				"summary_bytes": 7, "warning": null, "reason": "manual", "summary": "summary",
			}),
		)
		.expect("notify");
	lifecycle
		.notify(
			HookEventId::HookEventToolApprovalRequested,
			serde_json::json!({
				"call_id": "bash-1", "ticket_id": 1,
				"target": {"kind": "core", "name": "rm -rf build", "rev": "", "args": {}},
				"reasons": ["run `rm -rf build`"], "requested_by": "user",
			}),
		)
		.expect("notify");
	let compact = json(&written(&fixture.plugin_data().join("compact.json")).await);
	assert_eq!(compact["hook_event_name"], "PostCompact");
	assert_eq!(compact["trigger"], "manual");
	assert_eq!(compact["compact_summary"], "summary");
	let notification = json(&written(&fixture.plugin_data().join("notification.json")).await);
	assert_eq!(notification["hook_event_name"], "Notification");
	assert_eq!(notification["notification_type"], "permission_prompt");
	assert!(
		notification["message"]
			.as_str()
			.is_some_and(|message| message.contains("rm -rf build")),
		"{notification}"
	);
	drop(harness);
}

#[tokio::test]
async fn subagent_start_runs_on_a_subagents_first_prompt_and_opens_its_context() {
	let fixture = Fixture::new(
		r#"{"hooks":{"SubagentStart":[{"matcher":"reviewer","hooks":[{"type":"command",
			"command":"cat > \"$CLAUDE_PLUGIN_DATA/start.json\"; printf '%s' '{\"hookSpecificOutput\":{\"hookEventName\":\"SubagentStart\",\"additionalContext\":\"subagent-context-9\"}}'"}]}],
			"UserPromptSubmit":[{"hooks":[{"type":"command","command":"touch \"$CLAUDE_PLUGIN_DATA/prompted\""}]}]}}"#,
		true,
	)
	.approved();
	let outcome = fixture
		.harness(BASH, Kind { subagent: Some("reviewer"), ..Kind::default() })
		.await
		.turn()
		.await;
	let input = json(&read(&fixture.plugin_data().join("start.json")));
	assert_eq!(input["hook_event_name"], "SubagentStart");
	assert_eq!(input["agent_type"], "reviewer");
	assert_eq!(input["agent_id"], "hooks");
	assert!(
		outcome.requests[0].contains("subagent-context-9"),
		"the subagent's first request carries the context: {:?}",
		outcome.requests
	);
	assert!(
		!fixture.plugin_data().join("prompted").exists(),
		"prompt events stay with the main session"
	);
}

const ENDING: &str = r#"{"hooks":{"SessionEnd":[{"hooks":[{"type":"command",
	"command":"touch \"$CLAUDE_PLUGIN_DATA/ended\""}]}],
	"PostCompact":[{"hooks":[{"type":"command","command":"touch \"$CLAUDE_PLUGIN_DATA/compacted\""}]}]}}"#;

#[tokio::test]
async fn newly_mapped_events_still_need_the_operators_approval() {
	let fixture = Fixture::new(ENDING, true);
	let mut blocked = fixture
		.blocked()
		.iter()
		.map(|blocked| blocked.server.to_string())
		.collect::<Vec<_>>();
	blocked.sort();
	assert_eq!(blocked, ["PostCompact", "SessionEnd"]);
	let harness = fixture.harness(BASH, Kind::default()).await;
	assert!(!harness.installed, "no approved hook, no hook host");
	harness
		.lifecycle()
		.session_shutdown(&SessionShutdown::new(&harness.session, ShutdownReason::UserExit))
		.await;
	assert!(!fixture.plugin_data().join("ended").exists(), "the unapproved hook never ran");
	drop(harness);
}

#[tokio::test]
async fn an_unsupported_event_is_named_once_per_plugin() {
	// The same unsupported event in the hooks file and the manifest.
	let fixture = Fixture::new(
		r#"{"hooks":{"CwdChanged":[{"hooks":[{"type":"command","command":"a"}]}],
			"PostToolBatch":[{"hooks":[{"type":"command","command":"b"}]}]}}"#,
		true,
	);
	std::fs::create_dir_all(fixture.plugin.join(".claude-plugin")).expect("manifest dir");
	std::fs::write(
		fixture.plugin.join(".claude-plugin/plugin.json"),
		r#"{"name":"hooky","hooks":{"CwdChanged":[{"hooks":[{"type":"command","command":"c"}]}]}}"#,
	)
	.expect("manifest");
	let plugins = ClaudePlugins::resolve(&fixture.data, &fixture.root, None);
	let unsupported = plugins
		.unsupported_hook_events()
		.map(ToString::to_string)
		.collect::<Vec<_>>();
	assert_eq!(unsupported.len(), 2, "{unsupported:?}");
	for (event, notice) in ["CwdChanged", "PostToolBatch"].iter().zip(&unsupported) {
		assert!(
			notice.contains("plugin `hooky@m`")
				&& notice.contains(event)
				&& notice.contains("not supported by omp; this hook will not run"),
			"{notice}"
		);
	}
	assert!(fixture.blocked().is_empty(), "an unsupported hook is never a launch awaiting approval");
}

/// The JSON lines a hook appended to `path`.
fn lines(path: &Path) -> Vec<serde_json::Value> {
	read(path)
		.lines()
		.filter(|line| !line.trim().is_empty())
		.map(json)
		.collect()
}

#[tokio::test]
async fn session_start_runs_on_every_start_with_the_source_that_started_it() {
	let fixture = Fixture::new(
		r#"{"hooks":{"SessionStart":[{"hooks":[{"type":"command",
			"command":"cat >> \"$CLAUDE_PLUGIN_DATA/starts.jsonl\"; echo >> \"$CLAUDE_PLUGIN_DATA/starts.jsonl\""}]}]}}"#,
		true,
	)
	.approved();
	let harness = fixture.harness(BASH, Kind::default()).await;
	let root = std::fs::canonicalize(&fixture.root).expect("root");
	let next =
		Session::create(fixture.scratch.path().join("next.oms"), ComponentRegistry::standard())
			.expect("next session");
	let lifecycle = harness.lifecycle();
	lifecycle
		.session_start(&SessionStart::launch(&harness.session, &root))
		.await
		.expect("launch");
	for reason in
		[SwitchReason::New, SwitchReason::Resume, SwitchReason::Fork, SwitchReason::Handoff]
	{
		lifecycle
			.session_switch(&harness.session, &next, Some(reason))
			.await;
		lifecycle
			.session_start(&SessionStart::switched(&harness.session, &next, reason, &root))
			.await
			.expect("switched start");
	}
	let starts = lines(&fixture.plugin_data().join("starts.jsonl"));
	let sources = starts
		.iter()
		.map(|start| start["source"].as_str().unwrap_or_default())
		.collect::<Vec<_>>();
	assert_eq!(sources, ["startup", "clear", "resume", "fork", "resume"], "{starts:?}");
	assert_eq!(starts[0]["session_id"], "hooks");
	assert_eq!(starts[1]["hook_event_name"], "SessionStart");
	assert_eq!(starts[1]["session_id"], "next", "the host serves the started session");
	drop(harness);
}

#[tokio::test]
async fn a_finished_print_run_ends_its_session_as_other() {
	let fixture = Fixture::new(
		r#"{"hooks":{"SessionEnd":[{"matcher":"other","hooks":[{"type":"command",
			"command":"cat > \"$CLAUDE_PLUGIN_DATA/end.json\""}]}]}}"#,
		true,
	)
	.approved();
	let harness = fixture.harness(BASH, Kind::default()).await;
	harness
		.lifecycle()
		.session_shutdown(&SessionShutdown::new(&harness.session, ShutdownReason::Completed))
		.await;
	let input = json(&read(&fixture.plugin_data().join("end.json")));
	assert_eq!(input["reason"], "other", "a finished `-p` run is no user exit: {input}");
	drop(harness);
}

#[tokio::test]
async fn post_model_switch_runs_on_a_model_change_and_never_on_a_thinking_one() {
	let fixture = Fixture::new(
		r#"{"hooks":{"PostModelSwitch":[{"matcher":".*opus.*","hooks":[{"type":"command",
			"command":"cat >> \"$CLAUDE_PLUGIN_DATA/switches.jsonl\"; echo >> \"$CLAUDE_PLUGIN_DATA/switches.jsonl\""}]}]}}"#,
		true,
	)
	.approved();
	let harness = fixture.harness(BASH, Kind::default()).await;
	let lifecycle = harness.lifecycle();
	let change = |from: &str, to: &str, reason: &str, thinking: Option<&str>| {
		let model = |key: &str| {
			let (provider, model) = key.split_once('/').expect("catalog key");
			serde_json::json!({"provider": provider, "api": "", "model": model})
		};
		serde_json::json!({
			"from_model": model(from), "to_model": model(to), "role": "default",
			"reason": reason, "previous_thinking": "high", "thinking": thinking,
		})
	};
	let path = fixture.plugin_data().join("switches.jsonl");
	// One observation at a time: each hook appends its line before the next
	// change is published.
	let publish = async |payload: serde_json::Value, expected: usize| {
		lifecycle
			.notify(HookEventId::HookEventModelChanged, payload)
			.expect("notify");
		let deadline = Instant::now() + Duration::from_secs(30);
		while read(&path)
			.lines()
			.filter(|line| !line.trim().is_empty())
			.count()
			< expected
		{
			assert!(Instant::now() < deadline, "switch {expected} reaches the hook");
			tokio::time::sleep(Duration::from_millis(50)).await;
		}
	};
	// Thinking only: the model repeats, so no switch; nor does a target the
	// matcher filters out.
	lifecycle
		.notify(
			HookEventId::HookEventModelChanged,
			change("anthropic/claude-opus-5", "anthropic/claude-opus-5", "user", Some("low")),
		)
		.expect("notify");
	lifecycle
		.notify(
			HookEventId::HookEventModelChanged,
			change("anthropic/claude-opus-5", "openai/gpt-9", "user", Some("high")),
		)
		.expect("notify");
	publish(change("openai/gpt-9", "anthropic/claude-opus-5", "user", Some("medium")), 1).await;
	publish(change("anthropic/claude-opus-5", "bedrock/claude-opus-5", "fallback", None), 2).await;
	tokio::time::sleep(Duration::from_millis(500)).await;
	let switches = lines(&path);
	assert_eq!(switches.len(), 2, "{switches:?}");
	assert_eq!(switches[0]["hook_event_name"], "PostModelSwitch");
	assert_eq!(switches[0]["source"], "user_request");
	assert_eq!(switches[0]["from_model"], "gpt-9");
	assert_eq!(switches[0]["to_model"], "claude-opus-5");
	assert_eq!(switches[0]["effort"], serde_json::json!({"level": "medium"}));
	assert_eq!(switches[1]["source"], "automatic", "a fallback is omp's own switch");
	assert_eq!(switches[1]["from_model"], "claude-opus-5");
	assert_eq!(switches[1]["to_model"], "claude-opus-5");
	assert!(switches[1].get("effort").is_none(), "no reasoning request: {:?}", switches[1]);
	drop(harness);
}

#[tokio::test]
async fn idle_prompt_raises_once_a_run_end_stays_quiet_and_is_withdrawn_by_a_switch() {
	let fixture = Fixture::new(
		r#"{"hooks":{"Notification":[{"matcher":"idle_prompt","hooks":[{"type":"command",
			"command":"cat >> \"$CLAUDE_PLUGIN_DATA/idle.jsonl\"; echo >> \"$CLAUDE_PLUGIN_DATA/idle.jsonl\""}]}]}}"#,
		true,
	)
	.approved();
	let wait = Duration::from_millis(400);
	let harness = fixture
		.harness(BASH, Kind { idle_prompt: Some(wait), ..Kind::default() })
		.await;
	let lifecycle = harness.lifecycle();
	let run_end = serde_json::json!({
		"submission_id": "1",
		"summary": {"committed_turns": 1, "interrupted": false, "stop": "completed"},
		"continued": false, "error": null, "error_kind": null, "assistant_text": "done",
	});
	let path = fixture.plugin_data().join("idle.jsonl");
	// A switch inside the wait withdraws it.
	lifecycle
		.notify(HookEventId::HookEventAgentEnd, run_end.clone())
		.expect("notify");
	let next =
		Session::create(fixture.scratch.path().join("next.oms"), ComponentRegistry::standard())
			.expect("next session");
	lifecycle.session_switched(&next);
	tokio::time::sleep(wait * 3).await;
	assert!(lines(&path).is_empty(), "a switched-away run end raises nothing");
	// A quiet run end raises it once the wait passed.
	let ended = Instant::now();
	lifecycle
		.notify(HookEventId::HookEventAgentEnd, run_end)
		.expect("notify");
	let idle = json(&written(&path).await);
	assert!(ended.elapsed() >= wait, "not before the wait");
	assert_eq!(idle["hook_event_name"], "Notification");
	assert_eq!(idle["notification_type"], "idle_prompt");
	assert_eq!(idle["session_id"], "next");
	drop(harness);
}

#[tokio::test]
async fn agent_completed_runs_when_a_subagents_run_ends() {
	let fixture = Fixture::new(
		r#"{"hooks":{"Notification":[{"matcher":"agent_completed","hooks":[{"type":"command",
			"command":"cat > \"$CLAUDE_PLUGIN_DATA/completed.json\""}]}]}}"#,
		true,
	)
	.approved();
	let mut harness = fixture
		.harness(BASH, Kind {
			subagent: Some("reviewer"),
			idle_prompt: Some(Duration::from_millis(1)),
			..Kind::default()
		})
		.await;
	harness.run_turn().await;
	let completed = json(&written(&fixture.plugin_data().join("completed.json")).await);
	assert_eq!(completed["notification_type"], "agent_completed");
	assert_eq!(completed["agent_type"], "reviewer");
	assert_eq!(completed["agent_id"], "hooks");
	assert_eq!(completed["summary"], "done");
	drop(harness);
}
