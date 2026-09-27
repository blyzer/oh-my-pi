//! Claude Code plugin hooks: `hooks/hooks.json` and the manifest `hooks` key.
//!
//! A declaration is Claude Code's hook configuration: an event name (see
//! [`ClaudeHookEvent`]) maps to matcher groups, each a `matcher` plus `{type:
//! "command", command, args?, timeout?}` handlers. This module parses those
//! declarations into [`PluginHook`]s and maps them onto omp's vocabulary as
//! data: every event onto the omp lifecycle seam that runs it
//! ([`HookSeam`]), every Claude tool name onto the omp tool families
//! ([`CLAUDE_TOOL_NAMES`]). Whatever has no faithful omp counterpart — an
//! event, a handler type, a matcher naming a tool omp lacks — is a typed
//! [`PluginDiagnostic`] at load, never a half-run hook.
//!
//! Spec: Claude Code hooks reference, `code.claude.com/docs/en/hooks`, and the
//! plugin manifest reference, `code.claude.com/docs/en/plugins-reference`.

use std::{collections::BTreeMap, fs, path::Path, time::Duration};

use omp_core::Str;
use regex::Regex;
use serde::Deserialize;
use serde_json::value::RawValue;
use strum::{Display, EnumString, IntoStaticStr, VariantArray};

use crate::claude_plugin::{ConfigDeclaration, PluginComponent, PluginDiagnostic};

/// Every hook event the Claude Code hooks reference names.
#[derive(
	Clone,
	Copy,
	Debug,
	Display,
	EnumString,
	Eq,
	Hash,
	IntoStaticStr,
	Ord,
	PartialEq,
	PartialOrd,
	VariantArray,
)]
pub enum ClaudeHookEvent {
	/// A session begins or resumes.
	SessionStart,
	/// A session ends.
	SessionEnd,
	/// One-time `--init` / `--maintenance` preparation.
	Setup,
	/// The user submitted a prompt.
	UserPromptSubmit,
	/// A slash command expanded into a prompt.
	UserPromptExpansion,
	/// The main agent finished responding.
	Stop,
	/// A turn ended on an API error.
	StopFailure,
	/// Before a tool call runs.
	PreToolUse,
	/// After a tool call succeeded.
	PostToolUse,
	/// After a tool call failed.
	PostToolUseFailure,
	/// After a parallel tool batch resolved.
	PostToolBatch,
	/// A tool needs a permission decision.
	PermissionRequest,
	/// Auto mode denied a tool call.
	PermissionDenied,
	/// A subagent was spawned.
	SubagentStart,
	/// A subagent finished responding.
	SubagentStop,
	/// A task was created.
	TaskCreated,
	/// A task was completed.
	TaskCompleted,
	/// An agent-team teammate went idle.
	TeammateIdle,
	/// A watched file changed.
	FileChanged,
	/// A configuration file changed.
	ConfigChange,
	/// The working directory changed.
	CwdChanged,
	/// A working directory was added.
	DirectoryAdded,
	/// An instructions file was loaded.
	InstructionsLoaded,
	/// A worktree is being created.
	WorktreeCreate,
	/// A worktree is being removed.
	WorktreeRemove,
	/// Before a model switch.
	PreModelSwitch,
	/// After a model switch.
	PostModelSwitch,
	/// An MCP server asked for user input.
	Elicitation,
	/// The user answered an MCP elicitation.
	ElicitationResult,
	/// Claude Code sent a notification.
	Notification,
	/// Assistant text is streaming to the display.
	MessageDisplay,
	/// Before context compaction.
	PreCompact,
	/// After context compaction.
	PostCompact,
}

/// The omp lifecycle seam that runs a Claude hook event. Events without one
/// are [`PluginDiagnostic::UnsupportedHookEvent`].
#[derive(Clone, Copy, Debug, Display, Eq, Hash, IntoStaticStr, Ord, PartialEq, PartialOrd)]
#[strum(serialize_all = "snake_case")]
pub enum HookSeam {
	/// `tool_call` admission (`PreToolUse`).
	ToolCall,
	/// `tool_result` transform (`PostToolUse`, `PostToolUseFailure`).
	ToolResult,
	/// `before_agent_start` admission (`UserPromptSubmit`).
	BeforeAgentStart,
	/// `agent_settled` yield decision (`Stop`, `SubagentStop`).
	AgentSettled,
	/// `session_start` admission (`SessionStart`).
	SessionStart,
	/// `compaction` admission (`PreCompact`).
	Compaction,
}

impl ClaudeHookEvent {
	/// The omp seam running this event; `None` when omp has no faithful
	/// counterpart. `SessionEnd` is one: omp's `session_shutdown` is a lossy
	/// observation the process does not wait for, so a command could not run
	/// to completion.
	#[must_use]
	pub const fn seam(self) -> Option<HookSeam> {
		Some(match self {
			Self::PreToolUse => HookSeam::ToolCall,
			Self::PostToolUse | Self::PostToolUseFailure => HookSeam::ToolResult,
			Self::UserPromptSubmit => HookSeam::BeforeAgentStart,
			Self::Stop | Self::SubagentStop => HookSeam::AgentSettled,
			Self::SessionStart => HookSeam::SessionStart,
			Self::PreCompact => HookSeam::Compaction,
			_ => return None,
		})
	}

	/// Whether the event's matcher filters tool names.
	#[must_use]
	pub const fn matches_tools(self) -> bool {
		matches!(self, Self::PreToolUse | Self::PostToolUse | Self::PostToolUseFailure)
	}

	/// The spec's default command-hook timeout for this event.
	#[must_use]
	pub const fn default_timeout(self) -> Duration {
		match self {
			Self::UserPromptSubmit => Duration::from_secs(30),
			_ => Duration::from_secs(600),
		}
	}
}

/// Claude Code tool names and the omp tool families each maps onto. A
/// matcher naming a tool outside this table never matches and is reported
/// once at load.
///
/// `Bash` maps to `bash` only: omp's `eval` runs Python, so a Bash hook
/// reading `tool_input.command` has nothing faithful to read there.
pub const CLAUDE_TOOL_NAMES: [(&str, &[&str]); 11] = [
	("Bash", &["bash"]),
	("Read", &["read"]),
	("Write", &["write"]),
	("Edit", &["edit"]),
	("MultiEdit", &["edit"]),
	("Grep", &["grep"]),
	("Glob", &["glob"]),
	("WebSearch", &["web_search"]),
	("Agent", &["task"]),
	("Task", &["task"]),
	("TodoWrite", &["todo"]),
];

/// The omp tool families a Claude tool name maps onto; empty when omp lacks
/// the tool.
#[must_use]
pub fn omp_tools_for(claude: &str) -> &'static [&'static str] {
	CLAUDE_TOOL_NAMES
		.iter()
		.find(|(name, _)| *name == claude)
		.map_or(&[], |(_, tools)| tools)
}

/// Claude names mapping onto one omp tool family, in table order.
pub fn claude_names_for(omp_tool: &str) -> impl Iterator<Item = &'static str> + Clone + '_ {
	CLAUDE_TOOL_NAMES
		.iter()
		.filter(move |(_, tools)| tools.contains(&omp_tool))
		.map(|(name, _)| *name)
}

/// The Claude-facing tool name hooks receive for an omp tool: its first
/// mapped Claude name, else the omp name itself.
#[must_use]
pub fn claude_tool_name(omp_tool: &str) -> &str {
	claude_names_for(omp_tool).next().unwrap_or(omp_tool)
}

/// A hook matcher, evaluated as the spec says: empty or `*` matches all;
/// only `[A-Za-z0-9_\- ,|]` is a `|`/`,` list of exact names; anything else is
/// an unanchored regular expression.
#[derive(Clone, Debug)]
pub enum HookMatcher {
	/// Matches every subject.
	All,
	/// Matches any of these exact subjects.
	Exact(Box<[Str]>),
	/// Matches subjects the unanchored pattern finds a match in.
	Pattern(Regex),
}

impl PartialEq for HookMatcher {
	fn eq(&self, other: &Self) -> bool {
		match (self, other) {
			(Self::All, Self::All) => true,
			(Self::Exact(left), Self::Exact(right)) => left == right,
			(Self::Pattern(left), Self::Pattern(right)) => left.as_str() == right.as_str(),
			_ => false,
		}
	}
}

impl Eq for HookMatcher {}

impl HookMatcher {
	/// Compiles a declared matcher.
	///
	/// # Errors
	///
	/// The pattern is not a valid regular expression.
	pub fn compile(matcher: Option<&str>) -> Result<Self, regex::Error> {
		let Some(matcher) = matcher
			.map(str::trim)
			.filter(|m| !m.is_empty() && *m != "*")
		else {
			return Ok(Self::All);
		};
		let exact = matcher
			.bytes()
			.all(|byte| byte.is_ascii_alphanumeric() || b"_- ,|".contains(&byte));
		if exact {
			return Ok(Self::Exact(
				matcher
					.split(['|', ','])
					.map(str::trim)
					.filter(|name| !name.is_empty())
					.map(Str::new)
					.collect(),
			));
		}
		Regex::new(matcher).map(Self::Pattern)
	}

	/// Whether a non-tool subject (session source, compaction trigger, …)
	/// matches.
	#[must_use]
	pub fn matches(&self, subject: &str) -> bool {
		match self {
			Self::All => true,
			Self::Exact(names) => names.iter().any(|name| name.as_str() == subject),
			Self::Pattern(pattern) => pattern.is_match(subject),
		}
	}

	/// Whether a call of the omp tool family `omp_tool` matches, testing
	/// every Claude name that maps onto it.
	#[must_use]
	pub fn matches_tool(&self, omp_tool: &str) -> bool {
		match self {
			Self::All => true,
			Self::Exact(names) => names
				.iter()
				.any(|name| omp_tools_for(name.as_str()).contains(&omp_tool)),
			Self::Pattern(pattern) => claude_names_for(omp_tool).any(|name| pattern.is_match(name)),
		}
	}

	/// The tool names this matcher can never match: exact names outside
	/// [`CLAUDE_TOOL_NAMES`], or a pattern matching none of them.
	#[must_use]
	pub fn unmapped_tools(&self) -> Vec<Str> {
		match self {
			Self::All => Vec::new(),
			Self::Exact(names) => names
				.iter()
				.filter(|name| omp_tools_for(name.as_str()).is_empty())
				.cloned()
				.collect(),
			Self::Pattern(pattern) => {
				if CLAUDE_TOOL_NAMES
					.iter()
					.any(|(name, _)| pattern.is_match(name))
				{
					Vec::new()
				} else {
					vec![Str::new(pattern.as_str())]
				}
			},
		}
	}
}

/// One `type: "command"` hook handler.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HookCommand {
	/// Shell-form script, or the executable in exec form.
	pub command:  Str,
	/// Exec-form argument vector; `None` is shell form.
	pub args:     Option<Box<[Str]>>,
	/// Declared timeout, else the event's default.
	pub timeout:  Duration,
	/// `async: true`: runs in the background, its output discarded.
	pub detached: bool,
}

/// One loaded plugin hook: an event, its matcher, and one command.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PluginHook {
	/// The Claude event it subscribes.
	pub event:   ClaudeHookEvent,
	/// The omp seam running it.
	pub seam:    HookSeam,
	/// The group's matcher.
	pub matcher: HookMatcher,
	/// The handler.
	pub command: HookCommand,
	/// The declaration it came from (a hooks file or the manifest).
	pub source:  std::path::PathBuf,
}

#[derive(Deserialize)]
struct MatcherGroupWire {
	#[serde(default)]
	matcher: Option<Str>,
	#[serde(default)]
	hooks:   Vec<HandlerWire>,
}

#[derive(Deserialize)]
struct HandlerWire {
	#[serde(rename = "type")]
	kind:      Str,
	#[serde(default)]
	command:   Option<Str>,
	#[serde(default)]
	args:      Option<Vec<Str>>,
	#[serde(default)]
	timeout:   Option<f64>,
	#[serde(default, rename = "async")]
	detached:  bool,
	#[serde(default)]
	shell:     Option<Str>,
	#[serde(default, rename = "if")]
	condition: Option<Str>,
}

/// Parses every hook declaration of one plugin; problems are diagnostics and
/// the rest still load.
pub(crate) fn load_hooks(
	plugin: &Str,
	declarations: &[ConfigDeclaration],
	diagnostics: &mut Vec<PluginDiagnostic>,
) -> Vec<PluginHook> {
	let mut hooks = Vec::new();
	for declaration in declarations {
		let path = declaration.path();
		let owned;
		let body = match declaration {
			ConfigDeclaration::Inline { servers, .. } => servers.as_str(),
			ConfigDeclaration::File(file) => match fs::read_to_string(file) {
				Ok(text) => {
					owned = text;
					owned.as_str()
				},
				Err(source) => {
					diagnostics.push(invalid(plugin, path, source));
					continue;
				},
			},
		};
		let events = match event_map(body) {
			Ok(events) => events,
			Err(source) => {
				diagnostics.push(invalid(plugin, path, source));
				continue;
			},
		};
		for (name, groups) in events {
			let Ok(event) = name.parse::<ClaudeHookEvent>() else {
				diagnostics.push(PluginDiagnostic::UnknownHookEvent {
					plugin: plugin.clone(),
					event:  name,
					path:   path.to_path_buf(),
				});
				continue;
			};
			let Some(seam) = event.seam() else {
				diagnostics.push(PluginDiagnostic::UnsupportedHookEvent {
					plugin: plugin.clone(),
					event,
					path: path.to_path_buf(),
				});
				continue;
			};
			let groups = match serde_json::from_str::<Vec<MatcherGroupWire>>(groups.get()) {
				Ok(groups) => groups,
				Err(source) => {
					diagnostics.push(invalid(plugin, path, source));
					continue;
				},
			};
			for group in groups {
				let matcher = match HookMatcher::compile(group.matcher.as_deref()) {
					Ok(matcher) => matcher,
					Err(source) => {
						diagnostics.push(PluginDiagnostic::InvalidHookMatcher {
							plugin: plugin.clone(),
							event,
							path: path.to_path_buf(),
							source,
						});
						continue;
					},
				};
				if event.matches_tools() {
					let unmapped = matcher.unmapped_tools();
					if !unmapped.is_empty() {
						diagnostics.push(PluginDiagnostic::UnmappedHookMatcher {
							plugin: plugin.clone(),
							event,
							tools: unmapped.into_boxed_slice(),
							path: path.to_path_buf(),
						});
					}
				}
				for handler in group.hooks {
					if let Some(command) = handler_command(plugin, event, path, handler, diagnostics) {
						hooks.push(PluginHook {
							event,
							seam,
							matcher: matcher.clone(),
							command,
							source: path.to_path_buf(),
						});
					}
				}
			}
		}
	}
	hooks
}

fn invalid(
	plugin: &Str,
	path: &Path,
	source: impl std::error::Error + Send + Sync + 'static,
) -> PluginDiagnostic {
	PluginDiagnostic::InvalidComponent {
		plugin:    plugin.clone(),
		component: PluginComponent::Hooks,
		path:      path.to_path_buf(),
		source:    Box::new(source),
	}
}

/// A hooks document's event map: the `hooks` object of a `hooks.json`
/// wrapper (beside an optional `description`), or a bare event map (the
/// manifest's inline form).
fn event_map(body: &str) -> Result<BTreeMap<Str, Box<RawValue>>, serde_json::Error> {
	let mut top = serde_json::from_str::<BTreeMap<Str, Box<RawValue>>>(body)?;
	match top.remove("hooks") {
		Some(inner) if inner.get().trim_start().starts_with('{') => serde_json::from_str(inner.get()),
		Some(inner) => {
			top.insert(Str::new_static("hooks"), inner);
			Ok(top)
		},
		None => {
			top.remove("description");
			Ok(top)
		},
	}
}

fn handler_command(
	plugin: &Str,
	event: ClaudeHookEvent,
	path: &Path,
	handler: HandlerWire,
	diagnostics: &mut Vec<PluginDiagnostic>,
) -> Option<HookCommand> {
	let unsupported = |reason: HookHandlerGap| PluginDiagnostic::UnsupportedHookHandler {
		plugin: plugin.clone(),
		event,
		reason,
		path: path.to_path_buf(),
	};
	if handler.kind != "command" {
		diagnostics.push(unsupported(HookHandlerGap::Type { kind: handler.kind }));
		return None;
	}
	if handler
		.shell
		.as_deref()
		.is_some_and(|shell| shell != "bash" && handler.args.is_none())
	{
		diagnostics
			.push(unsupported(HookHandlerGap::Shell { shell: handler.shell.unwrap_or_default() }));
		return None;
	}
	if let Some(condition) = handler.condition {
		diagnostics.push(unsupported(HookHandlerGap::Condition { condition }));
		return None;
	}
	let Some(command) = handler.command.filter(|command| !command.trim().is_empty()) else {
		diagnostics.push(unsupported(HookHandlerGap::MissingCommand));
		return None;
	};
	let timeout = handler
		.timeout
		.filter(|seconds| seconds.is_finite() && *seconds > 0.0)
		.map_or_else(|| event.default_timeout(), Duration::from_secs_f64);
	Some(HookCommand {
		command,
		args: handler.args.map(Vec::into_boxed_slice),
		timeout,
		detached: handler.detached,
	})
}

/// Why a declared hook handler does not run.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum HookHandlerGap {
	/// Only `command` handlers run; `http`, `mcp_tool`, `prompt`, and `agent`
	/// do not.
	#[error("handler type `{kind}` is not supported (only `command`)")]
	Type {
		/// The declared `type`.
		kind: Str,
	},
	/// omp runs commands in its in-process bash interpreter only.
	#[error("shell `{shell}` is not supported (omp runs hooks in its in-process bash)")]
	Shell {
		/// The declared `shell`.
		shell: Str,
	},
	/// `if` permission-rule filters are not evaluated.
	#[error("`if` filter `{condition}` is not supported")]
	Condition {
		/// The declared filter.
		condition: Str,
	},
	/// A `command` handler without a command.
	#[error("command handler has no `command`")]
	MissingCommand,
}

#[cfg(test)]
mod tests {
	use super::*;

	fn load(body: &str) -> (Vec<PluginHook>, Vec<PluginDiagnostic>) {
		let mut diagnostics = Vec::new();
		let hooks = load_hooks(
			&Str::new_static("p@m"),
			&[ConfigDeclaration::Inline { manifest: "plugin.json".into(), servers: Str::new(body) }],
			&mut diagnostics,
		);
		(hooks, diagnostics)
	}

	#[test]
	fn wrapped_and_bare_documents_load_with_spec_timeouts() {
		let (hooks, diagnostics) = load(
			r#"{"description":"d","hooks":{
				"PreToolUse":[{"matcher":"Bash","hooks":[{"type":"command","command":"x","timeout":5}]}],
				"UserPromptSubmit":[{"hooks":[{"type":"command","command":"y"}]}]}}"#,
		);
		assert!(diagnostics.is_empty(), "{diagnostics:?}");
		assert_eq!(hooks.len(), 2);
		let pre = hooks
			.iter()
			.find(|hook| hook.event == ClaudeHookEvent::PreToolUse)
			.unwrap();
		assert_eq!(pre.seam, HookSeam::ToolCall);
		assert_eq!(pre.command.timeout, Duration::from_secs(5));
		assert!(pre.matcher.matches_tool("bash") && !pre.matcher.matches_tool("eval"));
		let prompt = hooks
			.iter()
			.find(|hook| hook.event == ClaudeHookEvent::UserPromptSubmit)
			.unwrap();
		assert_eq!(prompt.command.timeout, Duration::from_secs(30));

		let (bare, diagnostics) = load(r#"{"Stop":[{"hooks":[{"type":"command","command":"z"}]}]}"#);
		assert!(diagnostics.is_empty(), "{diagnostics:?}");
		assert_eq!(bare[0].seam, HookSeam::AgentSettled);
	}

	#[test]
	fn matchers_follow_the_spec_forms() {
		let exact = HookMatcher::compile(Some("Edit|Write, MultiEdit")).unwrap();
		assert!(exact.matches_tool("edit") && exact.matches_tool("write"));
		assert!(!exact.matches_tool("bash"));
		let pattern = HookMatcher::compile(Some("^(Read|Gl.*)$")).unwrap();
		assert!(pattern.matches_tool("read") && pattern.matches_tool("glob"));
		assert_eq!(HookMatcher::compile(Some("*")).unwrap(), HookMatcher::All);
		assert!(
			HookMatcher::compile(Some(""))
				.unwrap()
				.matches_tool("anything")
		);
		assert!(exact.unmapped_tools().is_empty());
		assert_eq!(
			HookMatcher::compile(Some("Bash|NotebookEdit"))
				.unwrap()
				.unmapped_tools(),
			[Str::new_static("NotebookEdit")]
		);
		assert_eq!(
			HookMatcher::compile(Some("mcp__memory__.*"))
				.unwrap()
				.unmapped_tools(),
			[Str::new_static("mcp__memory__.*")]
		);
	}

	#[test]
	fn unsupported_events_handlers_and_unmapped_matchers_are_precise_diagnostics() {
		let (hooks, diagnostics) = load(
			r#"{"hooks":{
				"Notification":[{"hooks":[{"type":"command","command":"n"}]}],
				"Bogus":[],
				"PreToolUse":[
					{"matcher":"NotebookEdit","hooks":[{"type":"command","command":"a"}]},
					{"matcher":"Bash","hooks":[
						{"type":"http","url":"http://x"},
						{"type":"command","command":"b","if":"Bash(git *)"},
						{"type":"command","command":"c","shell":"powershell"},
						{"type":"command","command":"ok"}]}]}}"#,
		);
		let kinds = diagnostics
			.iter()
			.map(|diagnostic| match diagnostic {
				PluginDiagnostic::UnsupportedHookEvent { event, .. } => format!("event:{event}"),
				PluginDiagnostic::UnknownHookEvent { event, .. } => format!("unknown:{event}"),
				PluginDiagnostic::UnmappedHookMatcher { tools, .. } => format!("unmapped:{tools:?}"),
				PluginDiagnostic::UnsupportedHookHandler { reason, .. } => format!("handler:{reason}"),
				other => panic!("unexpected {other:?}"),
			})
			.collect::<Vec<_>>();
		assert!(kinds.contains(&"event:Notification".to_owned()), "{kinds:?}");
		assert!(kinds.contains(&"unknown:Bogus".to_owned()), "{kinds:?}");
		assert!(
			kinds
				.iter()
				.any(|kind| kind.starts_with("unmapped:") && kind.contains("NotebookEdit"))
		);
		assert_eq!(
			kinds
				.iter()
				.filter(|kind| kind.starts_with("handler:"))
				.count(),
			3,
			"{kinds:?}"
		);
		// The unmapped matcher still loads (it never matches); `ok` loads.
		assert_eq!(hooks.len(), 2);
		assert!(!hooks[0].matcher.matches_tool("bash"));
		assert_eq!(hooks[1].command.command, "ok");
	}
}
