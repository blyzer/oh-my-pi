//! The `agents` step: v1 custom agents (`agents/<name>.md`) into v2 agent
//! classes (owner decision #11, option A).
//!
//! # Sources and destinations
//!
//! - **User agents.** Each v1 profile's `agent/agents/*.md` goes to the paired
//!   v2 profile: the class cfg `<profile config root>/<name>.cfg` (where
//!   [`crate::cfg::CfgFiles`] resolves `<agent>.cfg` for a child) and the rule
//!   `<profile config root>/agent/rules/agent-<name>.md` (the native user rules
//!   directory [`crate::discovery::rules::ActiveRules`] reads), once, through
//!   the step marker.
//! - **Project agents.** `<project>/.omp/agents/*.md` goes to
//!   `<project>/.omp/<name>.cfg` and `<project>/.omp/rules/agent-<name>.md`
//!   ([`import_project_agents`]). Like project settings, it follows the project
//!   omp runs in, with its own marker per project under the v2 state root, so
//!   the import never adds a file a repository would commit.
//!
//! # What is written
//!
//! The class name is the frontmatter `name`, as v1 named it; the file name
//! only orders the directory (v1 kept the first agent of a name).
//!
//! - The cfg holds the frontmatter: `description` as its first `//` line (the
//!   agents overlay shows it), `model` as `ai_model`, `thinkingLevel` (or
//!   `thinking`) as `ai_thinking`, and `tools` as the `sv_tools` allowlist.
//! - `sv_tools` keeps what v1 advertised to the child: the listed tools
//!   (`search`/`find` read as `grep`/`glob`, `exec` as `eval` and `bash`), plus
//!   `task` when the agent may spawn, plus `yield` and `hub`, which v1 added to
//!   every explicit list. `spawns: none` withholds `task`.
//! - The rule holds the Markdown body, with `alwaysApply: true` and `agents:
//!   [<name>]`, so only that class's children carry it.
//!
//! # What is reported
//!
//! Every frontmatter key with no class-level home (`output`, `blocking`,
//! `prewalk`, …), each v1 tool with no v2 counterpart, a model selector v2
//! cannot take, a thinking level v2 does not have, and a `spawns` allowlist are
//! [`NotMigratable`]. The class cfg and rule follow the asset steps' copy
//! rule: a destination with different content is kept and reported as
//! [`Attention::Conflict`], and then nothing is written for that agent; one
//! with identical content is [`SkipReason::AlreadyPresent`]. A class named
//! after a built-in v2 class (`task`) is a conflict too.

use std::{
	collections::{BTreeMap, BTreeSet},
	fmt::Write as _,
	fs, io,
	path::{Path, PathBuf},
};

use omp_catalog::{BUILTIN_ROLE_IDS, ReasoningEffort, parse_selector};
use omp_con::Value;
use omp_core::{Hash32, Str, sf};
use serde::{Deserialize, Serialize, de::IgnoredAny};
use thiserror::Error;

use super::{
	AssetError, ImportEntry, ImportError, ImportMode, ImportOutcome, ImportStep, StepContext,
	V1Item, V1Source, V2Roots,
	assets::{Placed, inside_v1_root, place_contents},
	report::{Attention, NotMigratable, SkipReason},
	step::atomic_replace,
};
use crate::discovery::rules::split_frontmatter;

/// The project configuration directory, v1's and v2's alike.
const PROJECT_DIR: &str = ".omp";
/// v1's agent directory name, under the agent directory or [`PROJECT_DIR`].
const AGENTS_DIR: &str = "agents";
/// Native user rules, relative to the v2 profile configuration root.
const USER_RULES_DIR: &str = "agent/rules";
/// Native project rules, relative to [`PROJECT_DIR`].
const PROJECT_RULES_DIR: &str = "rules";
/// File-name prefix of the rule carrying an agent's body.
const RULE_PREFIX: &str = "agent-";

/// v2's built-in agent classes: `task` is the spawner's default class.
const BUILTIN_CLASSES: &[&str] = &["task"];
/// Cfg names beside class cfgs that are not agent classes.
const RESERVED_CFGS: &[&str] = &["config", "subagent"];
/// v1 names the top-level session and an unnamed subagent use; v1 rejected
/// agents claiming them.
const V1_SENTINEL_NAMES: &[&str] = &["main", "sub"];

/// v1 `model` values that make an agent follow the session's model
/// (`isSessionInheritedAgentPattern`): no `ai_model` line.
const INHERITED_MODELS: &[&str] = &["default", "@default", "*", "pi/default", "@task", "pi/task"];
/// v1's legacy role prefix; `@` is the current one.
const V1_LEGACY_ROLE_PREFIX: &str = "pi/";

/// v1's canonical tool names (`BUILTIN_TOOL_NAMES` + `HIDDEN_TOOL_NAMES`),
/// which v1 matched case-insensitively; other names kept their spelling.
const V1_TOOL_NAMES: &[&str] = &[
	"read",
	"bash",
	"edit",
	"ast_grep",
	"ast_edit",
	"ask",
	"debug",
	"eval",
	"github",
	"glob",
	"grep",
	"lsp",
	"checkpoint",
	"rewind",
	"context_notes",
	"new_context",
	"security_scan",
	"task",
	"hub",
	"todo",
	"web_search",
	"write",
	"memory_edit",
	"retain",
	"recall",
	"reflect",
	"learn",
	"manage_skill",
	"yield",
	"goal",
	"think",
];
/// v1's legacy tool aliases (`LEGACY_BUILTIN_TOOL_NAME_ALIASES`).
const V1_TOOL_ALIASES: &[(&str, &str)] = &[("search", "grep"), ("find", "glob")];
/// v1's `exec` pseudo-tool, which its executor expanded into these.
const V1_EXEC: (&str, &[&str]) = ("exec", &["eval", "bash"]);
/// The subagent tool; v1 advertised it when `spawns` was set.
const TASK_TOOL: &str = "task";
/// Tools v1 appended to every explicit list: `yield` (`parseAgentFields`) and
/// `hub` (the executor's always-on collaboration tool).
const IMPLIED_TOOLS: &[&str] = &["yield", "hub"];

/// Why the agents step failed for one file or directory. Reported per item;
/// the marker stays unset and the next run retries.
#[derive(Debug, Error)]
pub enum AgentsImportError {
	/// The v1 agent directory could not be listed.
	#[error("could not list the v1 agents in {}", path.display())]
	ListAgents {
		/// The v1 `agents/` directory.
		path:   PathBuf,
		/// Filesystem failure.
		#[source]
		source: io::Error,
	},
	/// A v1 agent file could not be read.
	#[error("could not read the v1 agent {}", path.display())]
	ReadAgent {
		/// The v1 agent file.
		path:   PathBuf,
		/// Filesystem failure.
		#[source]
		source: io::Error,
	},
	/// A v2 class cfg or rule could not be compared or written.
	#[error("could not place the agent's v2 file")]
	Place(#[source] AssetError),
	/// The rule frontmatter could not be rendered.
	#[error("could not render the rule frontmatter for agent {name}")]
	RuleHeader {
		/// The agent class.
		name:   Str,
		/// Serialization failure.
		#[source]
		source: serde_yaml::Error,
	},
	/// The import marker could not be written.
	#[error("could not record the agents import marker {}", path.display())]
	Marker {
		/// The marker file.
		path:   PathBuf,
		/// Filesystem failure.
		#[source]
		source: io::Error,
	},
}

/// The `agents` step for one profile pair.
pub(super) fn import_agents(cx: &StepContext<'_>) -> Result<Vec<ImportEntry>, ImportError> {
	let config_dir = &cx.pair.target.config_dir;
	let destination =
		Destination { cfg_dir: config_dir.clone(), rules_dir: config_dir.join(USER_RULES_DIR) };
	let source = cx.locate(V1Item::Agents);
	let (entries, failed) = match &source {
		Some(source) => import_dir(source, &destination, cx.mode),
		None => (vec![whole(None, ImportOutcome::NothingToImport)], false),
	};
	if cx.mode == ImportMode::Apply && !failed {
		let marker = ImportStep::Agents.marker(config_dir);
		marker
			.set(None)
			.map_err(|source| AgentsImportError::Marker { path: marker.path().to_owned(), source })?;
	}
	Ok(entries)
}

/// Imports `project`'s own v1 agents (`<project>/.omp/agents/*.md`) into its
/// `.omp` directory, once per project.
///
/// This mirrors [`super::import_project_assets`] for the project's other
/// v1-only files: a project `.omp/` inside the v1 install (omp run from
/// `$HOME`) is never written.
///
/// The marker lives under the v2 state root ([`project_agents_marker`]) and is
/// written in [`ImportMode::Apply`] only for a project that had v1 agents and
/// imported them without a failure.
pub fn import_project_agents(
	project: &Path,
	v1: &V1Source,
	roots: &V2Roots,
	mode: ImportMode,
) -> Vec<ImportEntry> {
	let root = project.join(PROJECT_DIR);
	let source = root.join(AGENTS_DIR);
	if !source.is_dir() {
		return vec![whole(Some(&source), ImportOutcome::NothingToImport)];
	}
	if inside_v1_root(&root, v1) {
		return vec![whole(Some(&source), ImportOutcome::Skipped(SkipReason::InsideV1Root))];
	}
	let marker = project_agents_marker(project, roots);
	if marker.exists() {
		return vec![whole(Some(&source), ImportOutcome::Skipped(SkipReason::MarkerPresent))];
	}
	let destination = Destination { rules_dir: root.join(PROJECT_RULES_DIR), cfg_dir: root };
	let (mut entries, failed) = import_dir(&source, &destination, mode);
	if mode == ImportMode::Apply
		&& !failed
		&& let Err(error) = write_project_marker(&marker, project)
	{
		entries.push(failure(&source, error));
	}
	entries
}

/// Where `project`'s agents import marker lives: under the v2 state root,
/// named by the SHA-256 of the project's canonical path.
#[must_use]
pub fn project_agents_marker(project: &Path, roots: &V2Roots) -> PathBuf {
	let canonical = fs::canonicalize(project).unwrap_or_else(|_| project.to_owned());
	let digest = Hash32::sum(canonical.as_os_str().as_encoded_bytes());
	let mut name = String::with_capacity(".project-agents-migration-v1-".len() + 64);
	let _ = write!(name, ".project-agents-migration-v1-{}", digest.to_hex().as_str());
	roots.state_dir.join("v1-import").join(name)
}

fn write_project_marker(marker: &Path, project: &Path) -> Result<(), AgentsImportError> {
	let failed = |source| AgentsImportError::Marker { path: marker.to_owned(), source };
	if let Some(parent) = marker.parent() {
		fs::create_dir_all(parent).map_err(failed)?;
	}
	let mut contents = String::with_capacity(64);
	let _ = writeln!(contents, "revision = {}", super::Marker::REVISION);
	let _ = writeln!(contents, "project = {:?}", project.display().to_string());
	atomic_replace(marker, contents.as_bytes()).map_err(failed)
}

/// Where one scope's classes land.
struct Destination {
	/// Directory of the class cfgs.
	cfg_dir:   PathBuf,
	/// Native rules directory of the same scope.
	rules_dir: PathBuf,
}

/// Imports every `*.md` agent in `source`, in v1's order. The flag reports a
/// failure, which keeps the marker unset.
fn import_dir(
	source: &Path,
	destination: &Destination,
	mode: ImportMode,
) -> (Vec<ImportEntry>, bool) {
	let files = match agent_files(source) {
		Ok(files) => files,
		Err(error) => return (vec![failure(source, error)], true),
	};
	if files.is_empty() {
		return (vec![whole(Some(source), ImportOutcome::NothingToImport)], false);
	}
	let lsp = omp_tools::lsp::spec().name;
	let roster = Roster { lsp: lsp.as_str() };
	let mut seen = BTreeSet::<Str>::new();
	let mut entries = Vec::new();
	let mut failed = false;
	for file in files {
		match import_file(&file, destination, mode, &roster, &mut seen, &mut entries) {
			Ok(()) => {},
			Err(error) => {
				failed = true;
				entries.push(failure(&file, error));
			},
		}
	}
	(entries, failed)
}

/// `*.md` files (or links to them) directly in `dir`, sorted by name as v1
/// loaded them.
fn agent_files(dir: &Path) -> Result<Vec<PathBuf>, AgentsImportError> {
	let listed = |source| AgentsImportError::ListAgents { path: dir.to_owned(), source };
	let mut files = Vec::new();
	for entry in fs::read_dir(dir).map_err(listed)? {
		let path = entry.map_err(listed)?.path();
		if path.extension().is_some_and(|extension| extension == "md") && path.is_file() {
			files.push(path);
		}
	}
	files.sort();
	Ok(files)
}

/// v2 tool names a v1 tool can map onto: the native builtin roster plus the
/// environment's `lsp` tool.
struct Roster<'a> {
	lsp: &'a str,
}

impl Roster<'_> {
	fn has(&self, name: &str) -> bool {
		name == self.lsp
			|| omp_tools::builtin_tool_identities()
				.iter()
				.any(|tool| tool.name == name)
	}
}

/// v1 agent frontmatter (`parseAgentFields`). Every key this step does not
/// map lands in `other` and is reported by name.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct V1AgentHeader {
	name:           Option<String>,
	description:    Option<String>,
	tools:          Option<OneOrMany>,
	spawns:         Option<OneOrMany>,
	model:          Option<OneOrMany>,
	thinking_level: Option<String>,
	thinking:       Option<String>,
	#[serde(flatten)]
	#[allow(
		clippy::zero_sized_map_values,
		reason = "serde flatten collects the unmapped keys by name; their values are skipped"
	)]
	other:          BTreeMap<String, IgnoredAny>,
}

/// A v1 list field: a list, or one comma-separated string
/// (`parseArrayOrCSV`).
#[derive(Deserialize)]
#[serde(untagged)]
enum OneOrMany {
	One(String),
	Many(Vec<String>),
}

impl OneOrMany {
	/// Trimmed, non-empty entries; `None` for an empty string, which v1 read as
	/// unset. An explicit empty list stays an empty list.
	fn entries(&self) -> Option<Vec<&str>> {
		match self {
			Self::One(text) => {
				let entries = text
					.split(',')
					.map(str::trim)
					.filter(|entry| !entry.is_empty())
					.collect::<Vec<_>>();
				(!entries.is_empty()).then_some(entries)
			},
			Self::Many(items) => Some(
				items
					.iter()
					.map(|item| item.trim())
					.filter(|item| !item.is_empty())
					.collect(),
			),
		}
	}
}

/// The rule frontmatter scoping the body to its class.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RuleHeader<'a> {
	always_apply: bool,
	agents:       [&'a str; 1],
}

/// One agent converted into its two v2 files.
struct Converted {
	cfg:   String,
	rule:  Option<String>,
	/// What the class could not take, one report subject each.
	notes: Vec<Str>,
}

fn import_file(
	file: &Path,
	destination: &Destination,
	mode: ImportMode,
	roster: &Roster<'_>,
	seen: &mut BTreeSet<Str>,
	entries: &mut Vec<ImportEntry>,
) -> Result<(), AgentsImportError> {
	let text = fs::read_to_string(file)
		.map_err(|source| AgentsImportError::ReadAgent { path: file.to_owned(), source })?;
	let file_name = file
		.file_name()
		.map_or_else(|| file.display().to_string(), |name| name.to_string_lossy().into_owned());
	let not_migratable = |subject: Str| ImportEntry {
		step:    ImportStep::Agents,
		item:    V1Item::Agents,
		path:    Some(file.to_owned()),
		subject: Some(subject),
		outcome: ImportOutcome::NotMigratable(NotMigratable::NoV2Equivalent),
	};
	let (header, body) = split_frontmatter(&text);
	let header = header.and_then(|header| serde_yaml::from_str::<V1AgentHeader>(header).ok());
	let Some(header) = header else {
		// v1 rejected these too: no frontmatter, or none it could read.
		entries.push(not_migratable(sf!("{file_name}: no readable v1 agent frontmatter")));
		return Ok(());
	};
	let (Some(name), Some(description)) = (header.name.as_deref(), header.description.as_deref())
	else {
		entries.push(not_migratable(sf!("{file_name}: v1 agent without name and description")));
		return Ok(());
	};
	if V1_SENTINEL_NAMES.contains(&name.trim().to_ascii_lowercase().as_str()) {
		entries.push(not_migratable(sf!("{file_name}: v1 rejects the agent name {name:?}")));
		return Ok(());
	}
	let name = Str::new(name);
	if !seen.insert(name.clone()) {
		entries.push(not_migratable(sf!("{name}: {file_name} is shadowed by an earlier agent")));
		return Ok(());
	}
	if RESERVED_CFGS.contains(&name.as_str()) || crate::cfg::validate_name(&name).is_err() {
		entries.push(not_migratable(sf!("{name}: not a valid v2 agent class name")));
		return Ok(());
	}
	let entry = |subject: Str, outcome| ImportEntry {
		step: ImportStep::Agents,
		item: V1Item::Agents,
		path: Some(file.to_owned()),
		subject: Some(subject),
		outcome,
	};
	if BUILTIN_CLASSES.contains(&name.as_str()) {
		entries.push(entry(
			sf!("{name}: built-in v2 agent class"),
			ImportOutcome::NeedsAttention(Attention::Conflict),
		));
		return Ok(());
	}
	let converted = convert(&name, description, &header, body, roster)?;
	let cfg_path = destination.cfg_dir.join(format!("{name}.cfg"));
	let rule_path = destination
		.rules_dir
		.join(format!("{RULE_PREFIX}{name}.md"));
	let planned = [Some((cfg_path, converted.cfg)), converted.rule.map(|rule| (rule_path, rule))];
	// The shared copy rule, classified for both files before either is
	// written: a class is imported whole or not at all.
	let mut pending = Vec::new();
	let mut collided = false;
	for (path, contents) in planned.into_iter().flatten() {
		match place_contents(contents.as_bytes(), &path, ImportMode::DryRun)
			.map_err(AgentsImportError::Place)?
		{
			Placed::Copied => pending.push((path, contents)),
			Placed::Present => {},
			Placed::Conflict => {
				collided = true;
				entries.push(entry(
					sf!("{name}: {}", path.display()),
					ImportOutcome::NeedsAttention(Attention::Conflict),
				));
			},
		}
	}
	if collided {
		return Ok(());
	}
	if pending.is_empty() {
		entries.push(entry(name, ImportOutcome::Skipped(SkipReason::AlreadyPresent)));
		return Ok(());
	}
	if mode == ImportMode::Apply {
		for (path, contents) in &pending {
			place_contents(contents.as_bytes(), path, mode).map_err(AgentsImportError::Place)?;
		}
	}
	entries.push(entry(name, match mode {
		ImportMode::Apply => ImportOutcome::Imported,
		ImportMode::DryRun => ImportOutcome::WouldImport,
	}));
	entries.extend(converted.notes.into_iter().map(not_migratable));
	Ok(())
}

/// Builds the class cfg and the rule for one agent.
fn convert(
	name: &Str,
	description: &str,
	header: &V1AgentHeader,
	body: &str,
	roster: &Roster<'_>,
) -> Result<Converted, AgentsImportError> {
	let mut notes = Vec::new();
	let mut cfg = String::with_capacity(128);
	cfg.push_str("//");
	for word in description.split_whitespace() {
		cfg.push(' ');
		cfg.push_str(word);
	}
	cfg.push('\n');

	let mut model_thinking = None;
	if let Some(model) = &header.model
		&& let Some(patterns) = model.entries()
		&& let Some((first, fallbacks)) = patterns.split_first()
	{
		match map_model(first) {
			ModelMapping::Inherit => {
				let _ = writeln!(cfg, "// v1 model {first} follows the session model");
			},
			ModelMapping::Selector { selector, thinking } => {
				let _ = writeln!(cfg, "ai_model {}", Value::Str(selector));
				model_thinking = thinking;
			},
			ModelMapping::Unmappable => notes.push(sf!("{name}: model {first}")),
		}
		for fallback in fallbacks {
			notes.push(sf!("{name}: model fallback {fallback}"));
		}
	}

	let thinking = header
		.thinking_level
		.as_deref()
		.or(header.thinking.as_deref());
	match thinking.map(|level| (level, level.trim().parse::<ReasoningEffort>())) {
		Some((_, Ok(effort))) => {
			let effort: &'static str = effort.into();
			let _ = writeln!(cfg, "ai_thinking {effort}");
		},
		Some((level, Err(_))) => notes.push(sf!("{name}: thinkingLevel {level}")),
		None => {
			if let Some(effort) = model_thinking {
				let _ = writeln!(cfg, "ai_thinking {effort}");
			}
		},
	}

	let spawns = header
		.spawns
		.as_ref()
		.and_then(OneOrMany::entries)
		.filter(|spawns| !spawns.is_empty());
	let withholds_task = spawns
		.as_deref()
		.is_some_and(|spawns| matches!(spawns, [only] if only.eq_ignore_ascii_case("none")));
	let spawns_any = spawns
		.as_deref()
		.is_some_and(|spawns| matches!(spawns, ["*"]));
	if let Some(spawns) = &spawns
		&& !withholds_task
		&& !spawns_any
	{
		notes.push(sf!("{name}: spawns {}", spawns.join(",")));
	}
	match header.tools.as_ref().and_then(OneOrMany::entries) {
		Some(tools) => {
			let mut listed = Vec::<Str>::with_capacity(tools.len());
			for tool in tools {
				let lower = tool.to_ascii_lowercase();
				let canonical = V1_TOOL_ALIASES
					.iter()
					.find(|(alias, _)| *alias == lower)
					.map(|(_, target)| *target)
					.or_else(|| V1_TOOL_NAMES.iter().copied().find(|known| *known == lower))
					.unwrap_or(tool);
				if canonical == V1_EXEC.0 {
					listed.extend(V1_EXEC.1.iter().copied().map(Str::new_static));
				} else {
					listed.push(Str::new(canonical));
				}
			}
			let spawns_task = spawns.is_some() || listed.iter().any(|tool| tool.as_str() == TASK_TOOL);
			let mut advertised = Vec::<Str>::with_capacity(listed.len() + 3);
			for tool in listed {
				if tool.as_str() == TASK_TOOL {
					continue;
				}
				if !roster.has(&tool) {
					notes.push(sf!("{name}: tool {tool}"));
				} else if !advertised.contains(&tool) {
					advertised.push(tool);
				}
			}
			let implied = (spawns_task && !withholds_task)
				.then_some(TASK_TOOL)
				.into_iter()
				.chain(IMPLIED_TOOLS.iter().copied());
			for tool in implied {
				if !advertised.iter().any(|listed| listed.as_str() == tool) {
					advertised.push(Str::new_static(tool));
				}
			}
			let list = Value::List(advertised.into_iter().map(Value::Str).collect());
			let _ = writeln!(cfg, "sv_tools {list}");
		},
		// Every tool stays advertised; only an explicit list can withhold one.
		None if withholds_task => notes.push(sf!("{name}: spawns none without a tools list")),
		None => {},
	}
	notes.extend(header.other.keys().map(|key| sf!("{name}: {key}")));

	let body = body.trim_start_matches(['\n', '\r']);
	let rule = if body.trim().is_empty() {
		None
	} else {
		let header =
			serde_yaml::to_string(&RuleHeader { always_apply: true, agents: [name.as_str()] })
				.map_err(|source| AgentsImportError::RuleHeader { name: name.clone(), source })?;
		let mut rule = String::with_capacity(header.len() + body.len() + 10);
		rule.push_str("---\n");
		rule.push_str(&header);
		rule.push_str("---\n");
		rule.push_str(body);
		if !rule.ends_with('\n') {
			rule.push('\n');
		}
		Some(rule)
	};
	Ok(Converted { cfg, rule, notes })
}

/// A v1 `model` pattern in v2 terms.
#[derive(Debug, Eq, PartialEq)]
enum ModelMapping {
	/// Follows the session model: no `ai_model` line.
	Inherit,
	/// An `ai_model` selector, and the thinking level a `:off` suffix asked
	/// for (v2 selectors carry no `:off`).
	Selector { selector: Str, thinking: Option<&'static str> },
	/// No v2 form: a role v2 does not define, or a selector v2 cannot parse.
	Unmappable,
}

/// Maps one v1 model pattern through v2's selector grammar: `@role` (or
/// v1's legacy `pi/role`) keeps a built-in v2 role, `*` is the default role,
/// and anything else must parse as a v2 selector.
fn map_model(pattern: &str) -> ModelMapping {
	let pattern = pattern.trim();
	if INHERITED_MODELS.contains(&pattern) {
		return ModelMapping::Inherit;
	}
	let (base, thinking) = match pattern.rsplit_once(':') {
		Some((base, "off")) => (base, Some("off")),
		Some((base, "inherit")) => (base, None),
		_ => (pattern, None),
	};
	// A role reference: `*` is the default role; `@role` and v1's legacy
	// `pi/role` name one. v2 knows only its built-in roles without config,
	// and takes a thinking level after `:`.
	let reference = if let Some(rest) = base
		.strip_prefix('*')
		.filter(|rest| rest.is_empty() || rest.starts_with(':'))
	{
		Some(("default", rest.strip_prefix(':')))
	} else {
		base
			.strip_prefix('@')
			.filter(|reference| !reference.contains('/'))
			.or_else(|| base.strip_prefix(V1_LEGACY_ROLE_PREFIX))
			.map(|reference| match reference.split_once(':') {
				Some((role, level)) => (role, Some(level)),
				None => (reference, None),
			})
	};
	let Some((role, level)) = reference else {
		return match parse_selector(base) {
			Ok(_) => ModelMapping::Selector { selector: Str::new(base), thinking },
			Err(_) => ModelMapping::Unmappable,
		};
	};
	let level_valid = level.is_none_or(|level| {
		level == "auto"
			|| level
				.parse::<ReasoningEffort>()
				.is_ok_and(|effort| effort != ReasoningEffort::Off)
	});
	if !BUILTIN_ROLE_IDS.contains(&role) || !level_valid {
		return ModelMapping::Unmappable;
	}
	let selector = match level {
		Some(level) => sf!("@{role}:{level}"),
		None => sf!("@{role}"),
	};
	ModelMapping::Selector { selector, thinking }
}

fn whole(path: Option<&Path>, outcome: ImportOutcome) -> ImportEntry {
	ImportEntry::new(ImportStep::Agents, V1Item::Agents, path.map(Path::to_owned), outcome)
}

fn failure(path: &Path, error: AgentsImportError) -> ImportEntry {
	whole(Some(path), ImportOutcome::NeedsAttention(Attention::Failed(ImportError::Agents(error))))
}

#[cfg(test)]
#[path = "agents_tests.rs"]
mod tests;
