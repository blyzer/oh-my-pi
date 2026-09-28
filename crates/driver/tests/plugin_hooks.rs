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
use omp_agent::{DispatchPolicy, Inference, Kernel, RunControl, StaticPrompt, TurnInput};
use omp_ai::{
	BlockKind, ChatEvent, ChatRequest, ChatStream, Completion, ExecutionReceipt, FinishReason,
	RequestId, ResponseMeta, ToolCall, ToolCallId, Usage,
};
use omp_catalog::{ProviderId, RouteId};
use omp_core::Str;
use omp_driver::{
	headless::kernel::{EnvToolExecutor, SettingsAdmission},
	plugin_commands::{approve_launch, blocked_launches, plugin_launches},
	plugin_hooks::{PluginHookHost, PluginHookSession},
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
use omp_session::{ComponentRegistry, Session};
use parking_lot::Mutex;

/// One `bash` call, then a closing text turn; every request's messages are
/// recorded so tests can see what reached the model.
struct BashThenText {
	command:  String,
	turns:    usize,
	requests: Arc<Mutex<Vec<String>>>,
}

impl Inference for BashThenText {
	fn chat(
		&mut self,
		request: ChatRequest,
	) -> impl Future<Output = Result<ChatStream, omp_ai::Error>> + Send {
		self.turns += 1;
		self.requests.lock().push(format!("{:?}", request.messages));
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
		blocked_launches(&plugins, &agent_plugins)
	}

	/// The plugin's `${CLAUDE_PLUGIN_DATA}`.
	fn plugin_data(&self) -> PathBuf {
		omp_ext::claude_plugin::plugin_data_dir(&self.data, "hooky@m")
	}

	/// Runs one turn whose model calls `bash` with `command`.
	async fn run(&self, command: &str) -> Outcome {
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
			},
			environment.registry(),
			DispatchPolicy::new(spill.clone()),
			StaticPrompt(Str::new_static("test")),
		)
		.with_hook_gate(Arc::clone(&gate));
		let approvals = kernel.approval_route();
		let mut kernel = kernel
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
				subagent:     false,
			},
			&plugins,
		);
		let installed = host.is_some();
		if let Some(host) = host {
			host.bind_mailbox(kernel.mailbox());
			host.attach(&gate);
		}
		let mut session =
			Session::create_with_blob_store(&journal, ComponentRegistry::standard(), spill)
				.expect("session");
		let started = Instant::now();
		tokio::time::timeout(
			Duration::from_secs(90),
			kernel.run_turn(
				&mut session,
				TurnInput { text: Str::new_static("run it"), attachments: Vec::new() },
				RunControl::default(),
			),
		)
		.await
		.expect("turn settles")
		.expect("turn");
		let elapsed = started.elapsed();
		let journal = std::fs::read_to_string(session.journal_path()).expect("journal");
		drop(kernel);
		drop(environment);
		let requests = requests.lock().clone();
		Outcome { journal, requests, elapsed, installed }
	}
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
