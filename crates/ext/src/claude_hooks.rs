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

use std::{
	collections::BTreeMap,
	fmt::{self, Display, Write as _},
	fs,
	path::Path,
	time::Duration,
};

use omp_core::Str;
use regex::Regex;
use serde::Deserialize;
use serde_json::value::RawValue;
use strum::{Display, EnumString, IntoStaticStr, VariantArray};

use crate::{
	claude_plugin::{ConfigDeclaration, PluginComponent, PluginDiagnostic, expand_plugin_vars},
	plugin_command::{PluginLaunch, PluginLaunchKind},
};

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
	/// `before_agent_start` admission: `UserPromptSubmit` in a main session,
	/// `SubagentStart` on a subagent's first prompt.
	BeforeAgentStart,
	/// `agent_settled` yield decision (`Stop`, `SubagentStop`).
	AgentSettled,
	/// `session_start` admission (`SessionStart`).
	SessionStart,
	/// `session_shutdown`, awaited within the shutdown budget (`SessionEnd`).
	SessionShutdown,
	/// `agent_end` observation of a turn that failed on a provider error
	/// (`StopFailure`).
	AgentEnd,
	/// `compaction` admission (`PreCompact`).
	Compaction,
	/// `compaction_done` observation (`PostCompact`).
	CompactionDone,
	/// `tool_approval_requested` observation (`Notification`,
	/// `permission_prompt`).
	ToolApprovalRequested,
}

impl ClaudeHookEvent {
	/// The omp seam running this event; `None` when omp has no lifecycle
	/// point with the event's semantics. The crate README tabulates every
	/// event, mapped or not, and why.
	#[must_use]
	pub const fn seam(self) -> Option<HookSeam> {
		Some(match self {
			Self::PreToolUse => HookSeam::ToolCall,
			Self::PostToolUse | Self::PostToolUseFailure => HookSeam::ToolResult,
			Self::UserPromptSubmit | Self::SubagentStart => HookSeam::BeforeAgentStart,
			Self::Stop | Self::SubagentStop => HookSeam::AgentSettled,
			Self::SessionStart => HookSeam::SessionStart,
			Self::SessionEnd => HookSeam::SessionShutdown,
			Self::StopFailure => HookSeam::AgentEnd,
			Self::PreCompact => HookSeam::Compaction,
			Self::PostCompact => HookSeam::CompactionDone,
			Self::Notification => HookSeam::ToolApprovalRequested,
			Self::Setup
			| Self::UserPromptExpansion
			| Self::PostToolBatch
			| Self::PermissionRequest
			| Self::PermissionDenied
			| Self::TaskCreated
			| Self::TaskCompleted
			| Self::TeammateIdle
			| Self::FileChanged
			| Self::ConfigChange
			| Self::CwdChanged
			| Self::DirectoryAdded
			| Self::InstructionsLoaded
			| Self::WorktreeCreate
			| Self::WorktreeRemove
			| Self::PreModelSwitch
			| Self::PostModelSwitch
			| Self::Elicitation
			| Self::ElicitationResult
			| Self::MessageDisplay => return None,
		})
	}

	/// Whether the event's matcher filters tool names.
	#[must_use]
	pub const fn matches_tools(self) -> bool {
		matches!(self, Self::PreToolUse | Self::PostToolUse | Self::PostToolUseFailure)
	}

	/// The spec's default command-hook timeout for this event. A plugin's
	/// `SessionEnd` hook never runs past the shutdown budget, whatever it
	/// declares.
	#[must_use]
	pub const fn default_timeout(self) -> Duration {
		match self {
			Self::UserPromptSubmit => Duration::from_secs(30),
			Self::SessionEnd => Duration::from_millis(1500),
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

/// The canonical matcher text: `*` for every subject, the exact names joined
/// with `|`, or the pattern's source. Distinct matchers never share a text:
/// an exact list holds only characters a pattern must go beyond.
impl Display for HookMatcher {
	fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			Self::All => formatter.write_str("*"),
			Self::Exact(names) => {
				for (index, name) in names.iter().enumerate() {
					if index > 0 {
						formatter.write_str("|")?;
					}
					formatter.write_str(name)?;
				}
				Ok(())
			},
			Self::Pattern(pattern) => formatter.write_str(pattern.as_str()),
		}
	}
}

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

impl HookCommand {
	/// The script omp's in-process shell runs for this handler: the
	/// shell-form command as declared, or the exec-form command and each
	/// argument as one single-quoted word. `expand` maps every declared word
	/// (plugin variables) first.
	#[must_use]
	pub fn script(&self, mut expand: impl FnMut(&Str) -> Str) -> String {
		let Some(args) = &self.args else {
			return expand(&self.command).to_string();
		};
		let mut script = String::new();
		for word in std::iter::once(&self.command).chain(args.iter()) {
			if !script.is_empty() {
				script.push(' ');
			}
			push_quoted(&mut script, &expand(word));
		}
		script
	}
}

/// Appends `word` single-quoted, each embedded quote closed, escaped, and
/// reopened.
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

impl PluginHook {
	/// What triggers the hook: its event, then its matcher unless it matches
	/// everything (`PreToolUse Bash|Edit`, `Stop`).
	#[must_use]
	pub fn trigger(&self) -> Str {
		let event: &'static str = self.event.into();
		if matches!(self.matcher, HookMatcher::All) {
			return Str::new_static(event);
		}
		let mut trigger = String::from(event);
		let _ = write!(trigger, " {}", self.matcher);
		Str::new(trigger)
	}

	/// The launch the operator approves for this hook in the plugin rooted
	/// at `root`: the script the in-process shell runs
	/// ([`HookCommand::script`]) with `${CLAUDE_PLUGIN_ROOT}` expanded, and
	/// the trigger ([`Self::trigger`]) as its name, which the approval digest
	/// covers. `${CLAUDE_PLUGIN_DATA}` and `${CLAUDE_PROJECT_DIR}` stay
	/// variables: the host fills them per data directory and project, so one
	/// approval holds in every project.
	#[must_use]
	pub fn launch(&self, root: &Path) -> PluginLaunch {
		let script = self
			.command
			.script(|word| expand_plugin_vars(word.clone(), root, None));
		PluginLaunch::new(PluginLaunchKind::Hook, self.trigger(), Str::new(script), [], [])
	}
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
	// An unsupported event is reported once per plugin, however many of its
	// declarations name it.
	let mut unsupported = Vec::<ClaudeHookEvent>::new();
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
				if !unsupported.contains(&event) {
					unsupported.push(event);
					diagnostics.push(PluginDiagnostic::UnsupportedHookEvent {
						plugin: plugin.clone(),
						event,
						path: path.to_path_buf(),
					});
				}
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
	use omp_core::Hash32;

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
				"CwdChanged":[{"hooks":[{"type":"command","command":"n"}]}],
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
		assert!(kinds.contains(&"event:CwdChanged".to_owned()), "{kinds:?}");
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

	#[test]
	fn newly_mapped_events_load_onto_their_seams() {
		let (hooks, diagnostics) = load(
			r#"{"SessionEnd":[{"matcher":"clear","hooks":[{"type":"command","command":"a","timeout":90}]}],
				"StopFailure":[{"matcher":"rate_limit","hooks":[{"type":"command","command":"b"}]}],
				"PostCompact":[{"matcher":"auto","hooks":[{"type":"command","command":"c"}]}],
				"SubagentStart":[{"matcher":"task","hooks":[{"type":"command","command":"d"}]}],
				"Notification":[{"matcher":"permission_prompt","hooks":[{"type":"command","command":"e"}]}]}"#,
		);
		assert!(diagnostics.is_empty(), "{diagnostics:?}");
		let seams = hooks
			.iter()
			.map(|hook| (hook.event, hook.seam))
			.collect::<Vec<_>>();
		for expected in [
			(ClaudeHookEvent::SessionEnd, HookSeam::SessionShutdown),
			(ClaudeHookEvent::StopFailure, HookSeam::AgentEnd),
			(ClaudeHookEvent::PostCompact, HookSeam::CompactionDone),
			(ClaudeHookEvent::SubagentStart, HookSeam::BeforeAgentStart),
			(ClaudeHookEvent::Notification, HookSeam::ToolApprovalRequested),
		] {
			assert!(seams.contains(&expected), "{expected:?} in {seams:?}");
		}
		// Each is a launch the operator approves under its own trigger.
		let end = hooks
			.iter()
			.find(|hook| hook.event == ClaudeHookEvent::SessionEnd)
			.unwrap();
		assert_eq!(end.launch(Path::new("/p")).server, "SessionEnd clear");
		assert_eq!(end.command.timeout, Duration::from_secs(90), "the host caps it at run");
	}

	#[test]
	fn every_event_is_either_mapped_or_reported_once_per_plugin() {
		for event in ClaudeHookEvent::VARIANTS {
			let name: &'static str = event.into();
			let body = format!(r#"{{"{name}":[{{"hooks":[{{"type":"command","command":"x"}}]}}]}}"#);
			let mut diagnostics = Vec::new();
			// The same event declared by the manifest and a hooks file.
			let hooks = load_hooks(
				&Str::new_static("p@m"),
				&[
					ConfigDeclaration::Inline {
						manifest: "plugin.json".into(),
						servers:  Str::new(body.clone()),
					},
					ConfigDeclaration::Inline {
						manifest: "hooks.json".into(),
						servers:  Str::new(body),
					},
				],
				&mut diagnostics,
			);
			if event.seam().is_some() {
				assert!(diagnostics.is_empty(), "{name}: {diagnostics:?}");
				assert_eq!(hooks.len(), 2, "{name}");
				continue;
			}
			assert!(hooks.is_empty(), "{name}");
			let [PluginDiagnostic::UnsupportedHookEvent { event: reported, .. }] = &diagnostics[..]
			else {
				panic!("{name}: one diagnostic, not {diagnostics:?}");
			};
			assert_eq!(reported, event);
			let text = diagnostics[0].to_string();
			assert!(
				text.contains("plugin `p@m`")
					&& text.contains(name)
					&& text.contains("not supported by omp; this hook will not run"),
				"{text}"
			);
		}
	}

	fn hook_digest(body: &str) -> Hash32 {
		let (hooks, diagnostics) = load(body);
		assert!(diagnostics.is_empty(), "{diagnostics:?}");
		let [hook] = &hooks[..] else {
			panic!("one hook: {hooks:?}");
		};
		crate::plugin_command::plugin_command_digest(
			"p@m",
			"1.0.0",
			&hook.launch(Path::new("/plugins/p")),
		)
	}

	#[test]
	fn a_hook_launch_is_its_script_under_its_trigger() {
		let (hooks, _) = load(
			r#"{"PreToolUse":[{"matcher":"Edit|Write","hooks":[
				{"type":"command","command":"${CLAUDE_PLUGIN_ROOT}/check.sh \"$CLAUDE_PROJECT_DIR\""},
				{"type":"command","command":"${CLAUDE_PLUGIN_ROOT}/bin/x","args":["it's","${CLAUDE_PLUGIN_DATA}/y"]}]}],
			"Stop":[{"hooks":[{"type":"command","command":"stop"}]}]}"#,
		);
		let launches = hooks
			.iter()
			.map(|hook| hook.launch(Path::new("/plugins/p")))
			.collect::<Vec<_>>();
		assert!(
			launches
				.iter()
				.all(|launch| launch.kind == PluginLaunchKind::Hook)
		);
		assert_eq!(launches[0].server, "PreToolUse Edit|Write");
		assert_eq!(launches[0].command, r#"/plugins/p/check.sh "$CLAUDE_PROJECT_DIR""#);
		assert_eq!(
			launches[1].command, r"'/plugins/p/bin/x' 'it'\''s' '${CLAUDE_PLUGIN_DATA}/y'",
			"exec form quotes each word; host variables stay for the host"
		);
		assert!(launches[1].args.is_empty() && launches[1].env.is_empty());
		assert_eq!(launches[2].server, "Stop", "a match-all matcher is left out");
		assert_eq!(
			launches[0].command_line().to_string(),
			r#"/plugins/p/check.sh "$CLAUDE_PROJECT_DIR""#,
			"a hook script is shown verbatim"
		);
	}

	#[test]
	fn a_hook_digest_covers_what_runs_and_when() {
		let base = hook_digest(
			r#"{"PreToolUse":[{"matcher":"Bash","hooks":[{"type":"command","command":"a"}]}]}"#,
		);
		for (changed, what) in [
			(
				r#"{"PreToolUse":[{"matcher":"Bash","hooks":[{"type":"command","command":"b"}]}]}"#,
				"command",
			),
			(
				r#"{"PostToolUse":[{"matcher":"Bash","hooks":[{"type":"command","command":"a"}]}]}"#,
				"event",
			),
			(
				r#"{"PreToolUse":[{"matcher":"Bash|Edit","hooks":[{"type":"command","command":"a"}]}]}"#,
				"matcher",
			),
			(r#"{"PreToolUse":[{"hooks":[{"type":"command","command":"a"}]}]}"#, "match-all"),
			(
				r#"{"PreToolUse":[{"matcher":"Bash","hooks":[{"type":"command","command":"a","args":[]}]}]}"#,
				"exec form",
			),
		] {
			assert_ne!(base, hook_digest(changed), "{what} re-asks");
		}
		for (same, what) in [
			(
				r#"{"PreToolUse":[{"matcher":"Bash","hooks":[{"type":"command","command":"a","timeout":3}]}]}"#,
				"timeout",
			),
			(
				r#"{"PreToolUse":[{"matcher":"Bash","hooks":[{"type":"command","command":"a","async":true}]}]}"#,
				"async",
			),
			(
				r#"{"PreToolUse":[{"matcher":" Bash ","hooks":[{"type":"command","command":"a"}]}]}"#,
				"matcher spacing",
			),
		] {
			assert_eq!(base, hook_digest(same), "{what} does not re-ask");
		}
		// The same command as a server is a different approval.
		let server = PluginLaunch::new(
			PluginLaunchKind::McpServer,
			Str::new_static("PreToolUse Bash"),
			Str::new_static("a"),
			[],
			[],
		);
		assert_ne!(base, crate::plugin_command::plugin_command_digest("p@m", "1.0.0", &server));
	}
}
