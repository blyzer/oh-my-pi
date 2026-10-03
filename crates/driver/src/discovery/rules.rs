//! Context files and rules: the standing project guidance the system prompt
//! carries (`<repo-rules>`, `<generic-rules>`, `<domain-rules>`) and serves
//! as `rule://<name>`.
//!
//! The runtime discovers two capabilities:
//!
//! * **Context files**: `AGENTS.md`, `CLAUDE.md` and friends walked up from the
//!   project root — one file per directory depth, the highest-priority provider
//!   winning a tie (`.omp/AGENTS.md`, Claude, Gemini, then standalone
//!   `AGENTS.md` / `CLAUDE.md`) — plus one user-level winner from native,
//!   Claude, Codex, Gemini, and OpenCode. Injected whole, farthest first so the
//!   closest file reads last.
//! * **Rules**: Markdown documents with optional frontmatter (`description`,
//!   `globs`, `alwaysApply`, `condition`, `scope`, `agents`) from `.omp/rules`,
//!   `<config root>/agent/rules`, the sticky `RULES.md`, `.agent[s]/rules`,
//!   `.cursor/rules`, `.windsurf/rules`, `.clinerules`, and the legacy
//!   `.cursorrules` / `.windsurfrules` files. Name conflicts resolve
//!   first-source-wins in that order. `alwaysApply` rules are injected in full;
//!   described rules are listed by name and globs for the model to read through
//!   `rule://<name>`.
//!
//! A rule's `agents:` frontmatter (a list or comma-separated string of globs)
//! scopes it to agent classes: `*` spans any run and `?` one character, matched
//! case-insensitively against the [`AgentName`] the kernel runs as —
//! [`MAIN_AGENT`](crate::subagent::MAIN_AGENT) (`main`) for the top-level
//! session, the spawned class (`task`, `scout`, ...) for a subagent. A rule
//! without `agents:` reaches every agent; `agents: [main]` keeps a rule out of
//! every subagent, and `agents: [scout]` confines it to `scout` children.
//!
//! An entry prefixed with `!` excludes the classes its glob matches. The
//! positive entries define the admitted set (every agent when there are none)
//! and the negated entries subtract from it, so a negation always wins:
//! `agents: [!reviewer]` reaches every agent except `reviewer`, and
//! `agents: [review-*, !review-bot]` reaches every `review-*` class but
//! `review-bot`. Every spelling works — quoted (`["!reviewer"]`), bare (YAML
//! reads `!reviewer` as a tag; the parser takes it back as a negation), and
//! the comma-separated string (`agents: "!reviewer, !scout"`).
//!
//! The scope holds for every surface: an excluded rule is neither injected
//! nor listed in the prompt, and `rule://<name>` refuses it with
//! [`RuleLookupError::Excluded`] rather than serving its body. The class a
//! session runs as is journaled on the session itself
//! ([`journal_agent`](crate::subagent::journal_agent)), so resuming a child
//! session — from the main chat or with `--resume` — keeps its own scope.

use std::{
	collections::BTreeSet,
	fs,
	path::{Path, PathBuf},
	sync::Arc,
};

use omp_core::{CowBytes, Str};
use omp_dom::Dom;
use omp_envd::ContentResolver;
use omp_ext::claude_plugin::{ClaudePlugins, PluginScope};
use omp_session::{Session, SessionError};
use omp_tools::read::{
	Fault,
	resolver::{
		LineOffsetCache, ResourceCompletion, ResourceEntry, ResourceList, Scheme, SchemeEntry,
		fuzzy_score,
	},
	selector::ParsedSelector,
};
use parking_lot::RwLock;
use serde::Deserialize;

use crate::subagent::{AgentName, session_agent};

/// Where a discovered document sits in the precedence ladder.
#[derive(Clone, Copy, Debug, Eq, PartialEq, strum::IntoStaticStr)]
#[strum(serialize_all = "lowercase")]
pub enum Level {
	/// Project walk-up roots.
	Project,
	/// The configuration root.
	User,
}

/// Non-fatal discovery diagnostic.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Warning {
	/// Offending file or directory.
	pub path:    PathBuf,
	/// Human-readable reason.
	pub message: Str,
}

/// One persistent-instruction file.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContextFile {
	/// Canonical path.
	pub path:     PathBuf,
	/// Whole file body.
	pub content:  Str,
	/// User or project level.
	pub level:    Level,
	/// Directories between the project root and the file (`0` = in the
	/// project root); `0` for user-level files.
	pub depth:    usize,
	/// Provider identity (`native`, `claude`, `agents-md`, `claude-md`).
	pub provider: Str,
}

/// The context files one session injects, user level first, then project
/// files farthest first.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ContextFiles {
	/// Winning files in injection order.
	pub files:    Vec<ContextFile>,
	/// Unreadable files.
	pub warnings: Vec<Warning>,
}

/// Project context candidates in provider-priority order. Native context is
/// admitted from the nearest `.omp` root, Claude/Gemini only from the active
/// project root, and the standalone files walk every project depth.
const PROJECT_CONTEXT_PROVIDERS: [(&str, &str); 5] = [
	("native", ".omp/AGENTS.md"),
	("claude", ".claude/CLAUDE.md"),
	("gemini", ".gemini/GEMINI.md"),
	("agents-md", "AGENTS.md"),
	("claude-md", "CLAUDE.md"),
];

/// User context candidates in provider-priority order. The capability admits
/// one user context file, so a higher-priority ecosystem owns the scope even
/// when lower-priority files also exist.
const USER_CONTEXT_PROVIDERS: [(&str, &str); 5] = [
	("native", "agent/AGENTS.md"),
	("claude", ".claude/CLAUDE.md"),
	("codex", ".codex/AGENTS.md"),
	("gemini", ".gemini/GEMINI.md"),
	("opencode", ".config/opencode/AGENTS.md"),
];

impl ContextFiles {
	/// Discovers context files for `project_root`.
	#[must_use]
	pub fn discover(project_root: &Path, home: &Path, config_root: &Path) -> Self {
		let mut out = Self::default();
		for (provider, relative) in USER_CONTEXT_PROVIDERS {
			let path = if provider == "native" {
				config_root.join(relative)
			} else {
				home.join(relative)
			};
			let Some(content) = read_non_empty(&path, &mut out.warnings) else {
				continue;
			};
			out.files.push(ContextFile {
				path,
				content,
				level: Level::User,
				depth: 0,
				provider: Str::new_static(provider),
			});
			break;
		}
		let mut project = Vec::new();
		let ancestors = walk_up(project_root, home);
		// `.omp/AGENTS.md` is read from the
		// nearest `.omp/` directory only; the standalone files walk every
		// level.
		let nearest_config = ancestors.iter().position(|dir| dir.join(".omp").is_dir());
		for (depth, dir) in ancestors.iter().enumerate() {
			for (provider, relative) in PROJECT_CONTEXT_PROVIDERS {
				if provider == "native" && nearest_config != Some(depth) {
					continue;
				}
				if matches!(provider, "claude" | "gemini") && depth != 0 {
					continue;
				}
				let path = dir.join(relative);
				let Some(content) = read_non_empty(&path, &mut out.warnings) else {
					continue;
				};
				project.push(ContextFile {
					path,
					content,
					level: Level::Project,
					depth,
					provider: Str::new_static(provider),
				});
				// One file per depth: the first provider to claim it wins.
				break;
			}
		}
		project.reverse();
		out.files.extend(project);
		out
	}

	/// `{origin, content}` rows for the prompt's `<repo-rules>` block.
	#[must_use]
	pub fn prompt_facts(&self) -> Vec<serde_json::Value> {
		self
			.files
			.iter()
			.map(|file| {
				serde_json::json!({
					"origin": file.path.to_string_lossy(),
					"content": file.content.as_str(),
					"level": <&'static str>::from(file.level),
					"depth": file.depth,
				})
			})
			.collect()
	}
}

/// One rule document.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Rule {
	/// Unique name: the file stem, or a provider-fixed name for whole-file
	/// rules (`RULES`, `RULES@project`, `cursorrules`, `clinerules`,
	/// `windsurfrules`, `global_rules`).
	pub name:           Str,
	/// Canonical path.
	pub path:           PathBuf,
	/// Body after the frontmatter.
	pub content:        Str,
	/// Frontmatter `description`.
	pub description:    Option<Str>,
	/// Frontmatter `globs` this rule applies to.
	pub globs:          Vec<Str>,
	/// Frontmatter `alwaysApply`: injected in full every turn.
	pub always_apply:   bool,
	/// Frontmatter `condition`: regex triggers for the TTSR director.
	pub condition:      Vec<Str>,
	/// Frontmatter `scope`: TTSR stream scope tokens.
	pub scope:          Vec<Str>,
	/// Frontmatter `interruptMode` override for stream rules.
	pub interrupt_mode: Option<Str>,
	/// Frontmatter `agents`: lowercased agent-class globs, a `!` prefix
	/// negating one; empty admits every agent (see [`Rule::admits`]).
	pub agents:         Vec<Str>,
	/// Provider identity.
	pub provider:       Str,
	/// User or project level.
	pub level:          Level,
}

impl Rule {
	/// Whether this rule's `agents:` scope admits the agent class `agent`,
	/// case-insensitively: no positive glob, or one matching, and no `!` glob
	/// matching.
	#[must_use]
	pub fn admits(&self, agent: &AgentName<str>) -> bool {
		self.admits_lowercase(&agent.to_ascii_lowercase())
	}

	/// [`Self::admits`] for an already lowercased class.
	fn admits_lowercase(&self, agent: &str) -> bool {
		let mut positive = false;
		let mut included = false;
		for pattern in self.agents.iter().map(Str::as_str) {
			if let Some(negated) = pattern.strip_prefix('!') {
				if super::skills::glob_matches(negated.trim_start(), agent) {
					return false;
				}
			} else {
				positive = true;
				included = included || super::skills::glob_matches(pattern, agent);
			}
		}
		included || !positive
	}
}

/// Why `rule://<name>` served no rule body.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum RuleLookupError {
	/// No rule admitted for the agent is named `name`.
	#[error("Unknown rule: {name}\nAvailable: {}", name_list(available))]
	Unknown {
		/// Requested rule name.
		name:      Str,
		/// Rules the agent may read, in discovery order.
		available: Box<[Str]>,
	},
	/// The rule exists, but its `agents:` frontmatter excludes the agent.
	#[error("Rule `{rule}` is excluded for agent `{agent}` by its `agents:` frontmatter")]
	Excluded {
		/// Requested rule name.
		rule:  Str,
		/// Agent class the rule excludes.
		agent: AgentName,
	},
}

/// `a, b, c`, or `none` for an empty list.
fn name_list(names: &[Str]) -> String {
	if names.is_empty() {
		return "none".to_owned();
	}
	names.iter().map(Str::as_str).collect::<Vec<_>>().join(", ")
}

/// `rule://` answers with the model-facing read diagnostic; the typed lookup
/// error renders exactly once, here.
impl From<RuleLookupError> for Fault {
	fn from(error: RuleLookupError) -> Self {
		Self::Source { message: Str::new(error.to_string()) }
	}
}

/// The rules one session admitted, in discovery order.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ActiveRules {
	/// Winning rules, name-unique.
	pub rules:    Vec<Rule>,
	/// Malformed and colliding documents.
	pub warnings: Vec<Warning>,
}

impl ActiveRules {
	/// Discovers rules for `project_root` from the native, agents, installed
	/// marketplace plugin (`rules/` of each enabled Claude-layout install),
	/// cursor, windsurf, and cline providers in priority order.
	#[must_use]
	pub fn discover(
		project_root: &Path,
		home: &Path,
		config_root: &Path,
		plugins: &ClaudePlugins,
	) -> Self {
		let mut out = Self::default();
		let mut names = BTreeSet::<Str>::new();
		let mut admit = |rule: Rule, warnings: &mut Vec<Warning>| {
			if names.insert(rule.name.clone()) {
				out.rules.push(rule);
			} else {
				warnings.push(Warning {
					path:    rule.path,
					message: Str::new(format!(
						"rule name collision: \"{}\" already loaded, skipping this one",
						rule.name
					)),
				});
			}
		};
		let mut warnings = Vec::new();
		let ancestors = walk_up(project_root, home);
		let nearest_config = ancestors
			.iter()
			.map(|dir| dir.join(".omp"))
			.find(|dir| dir.is_dir());

		// native (100): project `.omp/rules`, user `agent/rules`, sticky RULES.md.
		if let Some(config) = &nearest_config {
			for rule in rules_in_dir(
				&config.join("rules"),
				"native",
				Level::Project,
				&["md", "mdc"],
				&mut warnings,
			) {
				admit(rule, &mut warnings);
			}
		}
		for rule in rules_in_dir(
			&config_root.join("agent/rules"),
			"native",
			Level::User,
			&["md", "mdc"],
			&mut warnings,
		) {
			admit(rule, &mut warnings);
		}
		if let Some(rule) = whole_file_rule(
			&config_root.join("agent/RULES.md"),
			"RULES",
			"native",
			Level::User,
			&mut warnings,
		) {
			admit(rule, &mut warnings);
		}
		if let Some(config) = &nearest_config
			&& let Some(rule) = whole_file_rule(
				&config.join("RULES.md"),
				"RULES@project",
				"native",
				Level::Project,
				&mut warnings,
			) {
			admit(rule, &mut warnings);
		}
		// agents: `.agent/rules` and `.agents/rules` (project walk-up + home).
		for dir in &ancestors {
			for name in [".agent/rules", ".agents/rules"] {
				for rule in rules_in_dir(
					&dir.join(name),
					"agents",
					Level::Project,
					&["md", "mdc"],
					&mut warnings,
				) {
					admit(rule, &mut warnings);
				}
			}
		}
		for name in [".agent/rules", ".agents/rules"] {
			for rule in
				rules_in_dir(&home.join(name), "agents", Level::User, &["md", "mdc"], &mut warnings)
			{
				admit(rule, &mut warnings);
			}
		}
		// claude-plugins: installed marketplace plugins, project installs first.
		for plugin in &plugins.plugins {
			let Some(dir) = plugin
				.claude_components()
				.and_then(|components| components.rules.as_deref())
			else {
				continue;
			};
			let level = match plugin.scope {
				PluginScope::Project => Level::Project,
				PluginScope::User => Level::User,
			};
			for rule in rules_in_dir(dir, "claude-plugins", level, &["md", "mdc"], &mut warnings) {
				admit(rule, &mut warnings);
			}
		}
		// cursor: user rules precede project rules within the provider, then
		// the legacy project `.cursorrules` file.
		for rule in rules_in_dir(
			&home.join(".cursor/rules"),
			"cursor",
			Level::User,
			&["mdc", "md"],
			&mut warnings,
		) {
			admit(rule, &mut warnings);
		}
		for rule in rules_in_dir(
			&project_root.join(".cursor/rules"),
			"cursor",
			Level::Project,
			&["mdc", "md"],
			&mut warnings,
		) {
			admit(rule, &mut warnings);
		}
		if let Some(rule) = whole_file_rule(
			&project_root.join(".cursorrules"),
			"cursorrules",
			"cursor",
			Level::Project,
			&mut warnings,
		) {
			admit(rule, &mut warnings);
		}
		// windsurf: user memories precede project rules within the provider.
		if let Some(rule) = whole_file_rule(
			&home.join(".codeium/windsurf/memories/global_rules.md"),
			"global_rules",
			"windsurf",
			Level::User,
			&mut warnings,
		) {
			admit(rule, &mut warnings);
		}
		for rule in rules_in_dir(
			&project_root.join(".windsurf/rules"),
			"windsurf",
			Level::Project,
			&["md"],
			&mut warnings,
		) {
			admit(rule, &mut warnings);
		}
		if let Some(rule) = whole_file_rule(
			&project_root.join(".windsurfrules"),
			"windsurfrules",
			"windsurf",
			Level::Project,
			&mut warnings,
		) {
			admit(rule, &mut warnings);
		}
		// cline: `.clinerules` file or directory, nearest ancestor.
		if let Some(found) = ancestors
			.iter()
			.map(|dir| dir.join(".clinerules"))
			.find(|path| path.exists())
		{
			if found.is_dir() {
				for rule in rules_in_dir(&found, "cline", Level::Project, &["md"], &mut warnings) {
					admit(rule, &mut warnings);
				}
			} else if let Some(rule) =
				whole_file_rule(&found, "clinerules", "cline", Level::Project, &mut warnings)
			{
				admit(rule, &mut warnings);
			}
		}
		out.warnings = warnings;
		out
	}

	/// The rule named `name`, when admitted.
	#[must_use]
	pub fn get(&self, name: &str) -> Option<&Rule> {
		self.rules.iter().find(|rule| rule.name.as_str() == name)
	}

	/// Rules admitted for the agent class `agent` ([`Rule::admits`]).
	pub fn for_agent<'a>(
		&'a self,
		agent: &'a AgentName<str>,
	) -> impl Iterator<Item = &'a Rule> + 'a {
		let agent = agent.to_ascii_lowercase();
		self
			.rules
			.iter()
			.filter(move |rule| rule.admits_lowercase(&agent))
	}

	/// The rule named `name` as the agent class `agent` may read it: an
	/// admitted rule, [`RuleLookupError::Excluded`] when its `agents:` scope
	/// leaves `agent` out, else [`RuleLookupError::Unknown`] listing only the
	/// rules `agent` may read.
	pub fn lookup(&self, name: &str, agent: &AgentName<str>) -> Result<&Rule, RuleLookupError> {
		match self.get(name) {
			Some(rule) if rule.admits(agent) => Ok(rule),
			Some(rule) => {
				Err(RuleLookupError::Excluded { rule: rule.name.clone(), agent: agent.to_owned() })
			},
			None => Err(RuleLookupError::Unknown {
				name:      Str::new(name),
				available: self
					.for_agent(agent)
					.map(|rule| rule.name.clone())
					.collect(),
			}),
		}
	}

	/// Prompt rows for `agent`: `always_apply_rules` are `{name, content, path}`
	/// injected whole; `rules` are the described
	/// rulebook entries `{name, description, globs, path}` the model reads on
	/// demand. A rule with neither `alwaysApply` nor a description is reachable
	/// only through `rule://`.
	#[must_use]
	pub fn prompt_facts(&self, agent: &AgentName<str>) -> RulePromptFacts {
		let mut facts = RulePromptFacts::default();
		for rule in self.for_agent(agent) {
			// Conditional documents are delivered by the stream-rules Director,
			// never as static instructions that bypass their condition.
			if rule
				.condition
				.iter()
				.any(|pattern| regex::Regex::new(pattern).is_ok())
			{
				continue;
			}
			if rule.always_apply {
				facts.always_apply.push(serde_json::json!({
					"name": rule.name.as_str(),
					"content": rule.content.as_str(),
					"path": rule.path.to_string_lossy(),
				}));
			} else if let Some(description) = &rule.description {
				facts.rulebook.push(serde_json::json!({
					"name": rule.name.as_str(),
					"description": description.as_str(),
					"globs": rule.globs.iter().map(Str::as_str).collect::<Vec<_>>(),
					"path": rule.path.to_string_lossy(),
				}));
			}
		}
		facts
	}
}

/// One kernel's discovered rules and the agent class its live session runs
/// as: the scope `rule://` and the prompt rule facts evaluate `agents:` with.
///
/// The class is not a second source of truth. It is rehydrated from the live
/// session's journal ([`session_agent`]) on every session switch through
/// [`omp_agent::SessionStateBridge::resync`], so a child session resumed from
/// the main chat reads rules as its own class.
#[derive(Debug)]
pub struct RuleScope {
	rules: Arc<ActiveRules>,
	agent: RwLock<AgentName>,
}

impl RuleScope {
	/// A scope over `rules` serving the agent class `agent`.
	#[must_use]
	pub const fn new(rules: Arc<ActiveRules>, agent: AgentName) -> Self {
		Self { rules, agent: RwLock::new(agent) }
	}

	/// The discovered rules, unfiltered.
	#[must_use]
	pub const fn rules(&self) -> &Arc<ActiveRules> {
		&self.rules
	}

	/// The agent class the live session runs as.
	#[must_use]
	pub fn agent(&self) -> AgentName {
		self.agent.read().clone()
	}

	/// Serves `agent` from now on.
	pub fn select(&self, agent: AgentName) {
		*self.agent.write() = agent;
	}

	/// The `rule://` resolver over this scope, installed through
	/// [`omp_envd::RegistryBridges::url_resolvers`]: it reads, lists, and
	/// completes only the rules the live agent class admits.
	#[must_use]
	pub fn resolver(self: &Arc<Self>) -> Arc<dyn ContentResolver> {
		Arc::new(RuleResolver { scope: Arc::clone(self), lines: LineOffsetCache::default() })
	}
}

impl omp_agent::SessionStateBridge for RuleScope {
	fn flush(&self, _session: &mut Session) -> Result<(), SessionError> {
		Ok(())
	}

	fn resync(&self, dom: &Dom) {
		self.select(session_agent(dom));
	}
}

/// The two prompt buckets of [`ActiveRules::prompt_facts`].
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RulePromptFacts {
	/// `<generic-rules>` bodies.
	pub always_apply: Vec<serde_json::Value>,
	/// `<domain-rules>` index rows.
	pub rulebook:     Vec<serde_json::Value>,
}

/// Project walk-up directories from `project_root` outward, closest first,
/// with this boundary: stop at the repository root (nearest `.git`), except
/// that a repository nested below the home
/// directory keeps walking up to — but never into — the home directory; a
/// project outside any repository stops at the home directory inclusive when
/// beneath it, and never reaches the filesystem root otherwise.
fn walk_up(project_root: &Path, home: &Path) -> Vec<PathBuf> {
	let repo_root = project_root
		.ancestors()
		.find(|dir| dir.join(".git").exists());
	let under_home = project_root.starts_with(home);
	let repo_is_home = repo_root == Some(home);
	let repo_under_home = repo_root.is_some_and(|root| root.starts_with(home)) && !repo_is_home;
	let scan_to_home = under_home && repo_under_home;
	let boundary = if scan_to_home {
		Some(home)
	} else {
		repo_root.or_else(|| under_home.then_some(home))
	};
	let include_boundary = match repo_root {
		None => under_home,
		Some(_) => boundary != Some(home) || repo_is_home,
	};
	let mut out = Vec::new();
	for dir in project_root.ancestors() {
		let at_boundary = Some(dir) == boundary;
		if at_boundary && !include_boundary {
			break;
		}
		if boundary.is_none() && dir.parent().is_none() {
			// No repository and not beneath home: the filesystem root itself
			// is never project context.
			break;
		}
		out.push(dir.to_path_buf());
		if at_boundary {
			break;
		}
	}
	out
}

/// Reads `path` when it is a non-empty file outside a hidden directory
/// Empty files contribute nothing and must not claim the depth scope.
fn read_non_empty(path: &Path, warnings: &mut Vec<Warning>) -> Option<Str> {
	if !path.is_file() {
		return None;
	}
	match fs::read_to_string(path) {
		Ok(text) if text.trim().is_empty() => None,
		Ok(text) => Some(Str::new(text)),
		Err(error) => {
			warnings.push(Warning {
				path:    path.to_path_buf(),
				message: Str::new(format!("Failed to read context file: {error}")),
			});
			None
		},
	}
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RuleHeader {
	description:    Option<String>,
	#[serde(default)]
	globs:          OneOrMany,
	#[serde(default)]
	always_apply:   bool,
	#[serde(default)]
	condition:      OneOrMany,
	#[serde(default)]
	scope:          OneOrMany,
	interrupt_mode: Option<String>,
	#[serde(default)]
	agents:         AgentScopes,
}

/// Frontmatter `agents`: one string (comma-separated), a list, or YAML's bare
/// `!name` spelling of a negated entry, which YAML parses as a tag on an empty
/// node rather than as text.
#[derive(Default)]
struct AgentScopes(Vec<String>);

impl<'de> Deserialize<'de> for AgentScopes {
	fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
		deserializer.deserialize_any(AgentScopesVisitor)
	}
}

struct AgentScopesVisitor;

impl<'de> serde::de::Visitor<'de> for AgentScopesVisitor {
	type Value = AgentScopes;

	fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		formatter.write_str("an agent glob, a comma-separated string of them, or a list")
	}

	fn visit_unit<E: serde::de::Error>(self) -> Result<Self::Value, E> {
		Ok(AgentScopes::default())
	}

	fn visit_none<E: serde::de::Error>(self) -> Result<Self::Value, E> {
		Ok(AgentScopes::default())
	}

	fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<Self::Value, E> {
		Ok(AgentScopes(value.split(',').map(str::to_owned).collect()))
	}

	fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
		let mut entries = Vec::with_capacity(seq.size_hint().unwrap_or(0));
		while let Some(AgentEntry(entry)) = seq.next_element()? {
			entries.push(entry);
		}
		Ok(AgentScopes(entries))
	}

	fn visit_enum<A: serde::de::EnumAccess<'de>>(self, data: A) -> Result<Self::Value, A::Error> {
		Ok(AgentScopes(vec![negated_tag(data)?]))
	}
}

/// One `agents:` list entry: text, or a bare `!name` tag.
struct AgentEntry(String);

impl<'de> Deserialize<'de> for AgentEntry {
	fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
		deserializer.deserialize_any(AgentEntryVisitor)
	}
}

struct AgentEntryVisitor;

impl<'de> serde::de::Visitor<'de> for AgentEntryVisitor {
	type Value = AgentEntry;

	fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		formatter.write_str("an agent glob")
	}

	fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<Self::Value, E> {
		Ok(AgentEntry(value.to_owned()))
	}

	fn visit_enum<A: serde::de::EnumAccess<'de>>(self, data: A) -> Result<Self::Value, A::Error> {
		negated_tag(data).map(AgentEntry)
	}
}

/// `!name` from a YAML tag: a named tag on an empty node (`[!reviewer]`), or
/// the non-specific `!` tag on a plain scalar (`[! reviewer]`).
fn negated_tag<'de, A: serde::de::EnumAccess<'de>>(data: A) -> Result<String, A::Error> {
	use serde::de::VariantAccess as _;
	let (tag, variant) = data.variant::<String>()?;
	let tag = tag.strip_prefix('!').unwrap_or(&tag);
	if tag.is_empty() {
		let name = variant.newtype_variant::<String>()?;
		return Ok(format!("!{}", name.trim()));
	}
	variant.unit_variant()?;
	Ok(format!("!{tag}"))
}

/// A frontmatter field accepts one string, a comma-separated string, or a list.
#[derive(Default, Deserialize)]
#[serde(untagged)]
enum OneOrMany {
	#[default]
	None,
	One(String),
	Many(Vec<String>),
}

impl OneOrMany {
	fn into_vec(self, split_commas: bool) -> Vec<Str> {
		let items = match self {
			Self::None => return Vec::new(),
			Self::One(value) if split_commas => value.split(',').map(str::to_owned).collect(),
			Self::One(value) => vec![value],
			Self::Many(values) => values,
		};
		items
			.iter()
			.map(|item| item.trim())
			.filter(|item| !item.is_empty())
			.map(Str::new)
			.collect()
	}
}

/// Splits `---` frontmatter from a Markdown document.
pub(crate) fn split_frontmatter(source: &str) -> (Option<&str>, &str) {
	let Some(rest) = source
		.strip_prefix("---\n")
		.or_else(|| source.strip_prefix("---\r\n"))
	else {
		return (None, source);
	};
	let Some(end) = rest.find("\n---") else {
		return (None, source);
	};
	let header = &rest[..end];
	let body = &rest[end + 4..];
	let body = body
		.strip_prefix("\r\n")
		.or_else(|| body.strip_prefix('\n'))
		.unwrap_or(body);
	(Some(header), body)
}

/// Builds a rule from a Markdown document.
fn load_rule(
	path: &Path,
	name: Str,
	provider: &'static str,
	level: Level,
	warnings: &mut Vec<Warning>,
) -> Option<Rule> {
	let canonical = match fs::canonicalize(path) {
		Ok(canonical) => canonical,
		Err(error) => {
			warnings.push(Warning {
				path:    path.to_path_buf(),
				message: Str::new(format!("Failed to read rule file: {error}")),
			});
			return None;
		},
	};
	let text = match fs::read_to_string(&canonical) {
		Ok(text) => text,
		Err(error) => {
			warnings.push(Warning {
				path:    canonical,
				message: Str::new(format!("Failed to read rule file: {error}")),
			});
			return None;
		},
	};
	let (header, body) = split_frontmatter(&text);
	let header = match header.map(serde_yaml::from_str::<RuleHeader>) {
		None => RuleHeader::default(),
		Some(Ok(header)) => header,
		Some(Err(error)) => {
			warnings.push(Warning {
				path:    canonical,
				message: Str::new(format!("failed to parse rule frontmatter: {error}")),
			});
			return None;
		},
	};
	Some(Rule {
		name,
		path: canonical,
		content: Str::new(body),
		description: header
			.description
			.as_deref()
			.map(str::trim)
			.filter(|description| !description.is_empty())
			.map(Str::new),
		globs: header.globs.into_vec(true),
		always_apply: header.always_apply,
		condition: header.condition.into_vec(false),
		scope: header.scope.into_vec(true),
		interrupt_mode: header
			.interrupt_mode
			.as_deref()
			.map(str::trim)
			.filter(|value| !value.is_empty())
			.map(Str::new),
		agents: header
			.agents
			.0
			.iter()
			.map(|agent| agent.trim())
			.filter(|agent| !agent.is_empty())
			.map(|agent| Str::new(agent.to_ascii_lowercase()))
			.collect(),
		provider: Str::new_static(provider),
		level,
	})
}

/// Rules from the files directly below `dir` with one of `extensions`, in
/// name order, without recursion.
fn rules_in_dir(
	dir: &Path,
	provider: &'static str,
	level: Level,
	extensions: &[&str],
	warnings: &mut Vec<Warning>,
) -> Vec<Rule> {
	let entries = match fs::read_dir(dir) {
		Ok(entries) => entries,
		Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
		Err(error) => {
			warnings.push(Warning {
				path:    dir.to_path_buf(),
				message: Str::new(format!("Failed to read rules directory: {error}")),
			});
			return Vec::new();
		},
	};
	let canonical_dir = match fs::canonicalize(dir) {
		Ok(path) => path,
		Err(error) => {
			warnings.push(Warning {
				path:    dir.to_path_buf(),
				message: Str::new(format!("Failed to resolve rules directory: {error}")),
			});
			return Vec::new();
		},
	};
	let mut files = entries
		.filter_map(Result::ok)
		.map(|entry| entry.path())
		.filter(|path| {
			path.is_file()
				&& !path
					.file_name()
					.is_some_and(|name| name.to_string_lossy().starts_with('.'))
				&& path
					.extension()
					.and_then(|extension| extension.to_str())
					.is_some_and(|extension| extensions.contains(&extension))
		})
		.collect::<Vec<_>>();
	files.sort();
	files
		.into_iter()
		.filter_map(|path| {
			let canonical = match fs::canonicalize(&path) {
				Ok(canonical) if canonical.starts_with(&canonical_dir) => canonical,
				Ok(_) => {
					warnings.push(Warning {
						path,
						message: Str::new_static("rule declaration resolves outside its discovery root"),
					});
					return None;
				},
				Err(error) => {
					warnings.push(Warning {
						path,
						message: Str::new(format!("Failed to resolve rule file: {error}")),
					});
					return None;
				},
			};
			let name = Str::new(canonical.file_stem()?.to_string_lossy());
			load_rule(&canonical, name, provider, level, warnings)
		})
		.collect()
}

/// A whole file as one rule under a fixed `name`: the sticky `RULES.md`
/// always applies regardless of frontmatter, and the single-file forms
/// `.clinerules`, `global_rules.md`, and the legacy
/// `.cursorrules` / `.windsurfrules`. Those are project-wide instructions by
/// construction, so they always apply unless their frontmatter opts them into
/// the rulebook with a description.
fn whole_file_rule(
	path: &Path,
	name: &'static str,
	provider: &'static str,
	level: Level,
	warnings: &mut Vec<Warning>,
) -> Option<Rule> {
	if !path.is_file() {
		return None;
	}
	let mut rule = load_rule(path, Str::new_static(name), provider, level, warnings)?;
	if rule.content.trim().is_empty() {
		return None;
	}
	let sticky = name.starts_with("RULES");
	rule.always_apply = sticky || rule.always_apply || rule.description.is_none();
	Some(rule)
}

/// `rule://<name>` reads a rule body; bare `rule://` lists every rule the
/// live agent class admits.
struct RuleResolver {
	scope: Arc<RuleScope>,
	lines: LineOffsetCache,
}

impl RuleResolver {
	fn rule(&self, name: &str) -> Result<&Rule, RuleLookupError> {
		self.scope.rules.lookup(name, &self.scope.agent())
	}

	fn index(&self) -> Vec<u8> {
		let agent = self.scope.agent();
		let mut text = String::from("# Rules\n\n");
		for rule in self.scope.rules.for_agent(&agent) {
			text.push_str("- rule://");
			text.push_str(&rule.name);
			if let Some(description) = &rule.description {
				text.push_str(": ");
				text.push_str(description);
			}
			if !rule.globs.is_empty() {
				text.push_str(" (");
				text.push_str(&rule.globs.join(", "));
				text.push(')');
			}
			text.push('\n');
		}
		text.into_bytes()
	}
}

#[async_trait::async_trait]
impl ContentResolver for RuleResolver {
	fn entry(&self) -> SchemeEntry {
		SchemeEntry::new(Scheme::Rule, true, false, "discovered project and user rules")
			.with_capabilities(true, true, true)
			.with_whole_body(true)
	}

	async fn read(
		&self,
		resource: &str,
		selector: &ParsedSelector,
	) -> Result<CowBytes<'static>, Fault> {
		if resource.is_empty() {
			return Ok(CowBytes::from(self.index()));
		}
		let rule = self.rule(resource.trim_end_matches('/'))?;
		let bytes = CowBytes::from(rule.content.as_bytes().to_vec());
		let ParsedSelector::Lines { ranges, .. } = selector else {
			return Ok(bytes);
		};
		let mut output = Vec::new();
		for range in ranges {
			let piece = self
				.lines
				.slice(resource, &bytes, *range)
				.map_err(|error| Fault::Invalid { message: Str::new(error.to_string()) })?;
			output.extend_from_slice(&piece);
		}
		Ok(CowBytes::from(output))
	}

	async fn list(
		&self,
		resource: &str,
		max_entries: usize,
		_max_bytes: usize,
	) -> Result<ResourceList, Fault> {
		if !resource.is_empty() {
			return Err(Fault::Invalid {
				message: Str::new(format!("rule://{resource} is a document and cannot be listed.")),
			});
		}
		let agent = self.scope.agent();
		let mut entries = Vec::new();
		let mut truncated = false;
		for rule in self.scope.rules.for_agent(&agent) {
			if entries.len() == max_entries {
				truncated = true;
				break;
			}
			entries.push(ResourceEntry {
				uri:       Str::new(format!("rule://{}", rule.name)),
				name:      rule.name.clone(),
				directory: false,
				size:      rule.content.len() as u64,
			});
		}
		Ok(ResourceList { entries, truncated })
	}

	async fn path(&self, resource: &str) -> Result<Option<Str>, Fault> {
		if resource.is_empty() {
			return Ok(None);
		}
		let rule = self.rule(resource.trim_end_matches('/'))?;
		Ok(Some(Str::new(format!("file://{}", rule.path.display()))))
	}

	async fn complete(
		&self,
		query: &str,
		max_results: usize,
	) -> Result<Vec<ResourceCompletion>, Fault> {
		let agent = self.scope.agent();
		let mut matches = self
			.scope
			.rules
			.for_agent(&agent)
			.filter_map(|rule| {
				fuzzy_score(query, &rule.name).map(|score| ResourceCompletion {
					value: Str::new(format!("rule://{}", rule.name)),
					description: rule.description.clone().unwrap_or_default(),
					score,
				})
			})
			.collect::<Vec<_>>();
		matches.sort_by(|left, right| {
			right
				.score
				.cmp(&left.score)
				.then_with(|| left.value.cmp(&right.value))
		});
		matches.truncate(max_results);
		Ok(matches)
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::subagent::MAIN_AGENT;

	fn write(path: &Path, text: &str) {
		fs::create_dir_all(path.parent().unwrap()).unwrap();
		fs::write(path, text).unwrap();
	}

	/// A fake home with a repository two levels down and a project nested
	/// inside it: `home/work/repo/{.git}/crates/app`.
	fn layout() -> (tempfile::TempDir, PathBuf, PathBuf, PathBuf) {
		let temp = tempfile::tempdir().unwrap();
		let home = temp.path().canonicalize().unwrap();
		let repo = home.join("work/repo");
		fs::create_dir_all(repo.join(".git")).unwrap();
		let project = repo.join("crates/app");
		fs::create_dir_all(&project).unwrap();
		(temp, home, repo, project)
	}

	#[test]
	fn context_files_walk_up_with_pi_precedence_and_depth_order() {
		let (_temp, home, repo, project) = layout();
		let config_root = home.join(".o2");
		write(&config_root.join("agent/AGENTS.md"), "user guidance");
		write(&project.join("AGENTS.md"), "app agents");
		write(&project.join("CLAUDE.md"), "app claude (loses the tie)");
		write(&repo.join("crates/CLAUDE.md"), "crates claude");
		write(&repo.join(".omp/AGENTS.md"), "repo native");
		write(&repo.join("AGENTS.md"), "repo standalone (shadowed by .omp)");
		write(&home.join("work/AGENTS.md"), "workspace level");
		write(&home.join("AGENTS.md"), "home copy never loads as project context");
		write(&repo.join("crates/AGENTS.md"), "   \n");

		let files = ContextFiles::discover(&project, &home, &config_root);
		assert!(files.warnings.is_empty(), "{:?}", files.warnings);
		let rows = files
			.files
			.iter()
			.map(|file| (file.provider.as_str(), file.level, file.depth, file.content.as_str()))
			.collect::<Vec<_>>();
		assert_eq!(rows, [
			("native", Level::User, 0, "user guidance"),
			("agents-md", Level::Project, 3, "workspace level"),
			("native", Level::Project, 2, "repo native"),
			("claude-md", Level::Project, 1, "crates claude"),
			("agents-md", Level::Project, 0, "app agents"),
		]);
		let facts = files.prompt_facts();
		assert_eq!(facts[4]["origin"], project.join("AGENTS.md").to_string_lossy().as_ref());
		assert_eq!(facts[4]["content"], "app agents");
	}

	#[test]
	fn context_scope_uses_foreign_provider_precedence() {
		let (_temp, home, _repo, project) = layout();
		let config_root = home.join(".o2");
		write(&home.join(".codex/AGENTS.md"), "codex user");
		write(&home.join(".gemini/GEMINI.md"), "gemini user");
		write(&project.join(".gemini/GEMINI.md"), "gemini project");
		write(&project.join("AGENTS.md"), "standalone loses");
		let files = ContextFiles::discover(&project, &home, &config_root);
		assert_eq!(
			files
				.files
				.iter()
				.map(|file| (file.provider.as_str(), file.content.as_str()))
				.collect::<Vec<_>>(),
			[("codex", "codex user"), ("gemini", "gemini project")]
		);

		write(&home.join(".claude/CLAUDE.md"), "claude user");
		write(&project.join(".claude/CLAUDE.md"), "claude project");
		let files = ContextFiles::discover(&project, &home, &config_root);
		assert_eq!(files.files[0].provider, "claude");
		assert_eq!(files.files.last().unwrap().provider, "claude");
	}

	#[cfg(unix)]
	#[test]
	fn linked_rule_outside_provider_root_is_rejected_without_hiding_siblings() {
		use std::os::unix::fs::symlink;

		let (_temp, home, repo, project) = layout();
		let config_root = home.join(".o2");
		let rules = repo.join(".omp/rules");
		write(&rules.join("inside.md"), "---\nalwaysApply: true\n---\ninside");
		let outside = home.join("outside.md");
		write(&outside, "---\nalwaysApply: true\n---\noutside");
		symlink(&outside, rules.join("escape.md")).unwrap();

		let discovered =
			ActiveRules::discover(&project, &home, &config_root, &ClaudePlugins::default());
		assert!(discovered.get("inside").is_some());
		assert!(discovered.get("escape").is_none());
		assert!(
			discovered
				.warnings
				.iter()
				.any(|warning| warning.message.contains("outside its discovery root"))
		);
	}

	#[test]
	fn installed_plugin_rules_load_unless_the_plugin_is_disabled() {
		use omp_ext::claude_plugin::{InstallScope, InstalledPluginEntry, InstalledPluginsRegistry};

		let (_temp, home, _repo, project) = layout();
		let data = home.join("data");
		let on = data.join("plugins/cache/plugins/m___style___1");
		let off = data.join("plugins/cache/plugins/m___quiet___1");
		write(&on.join("rules/house-style.md"), "---\nalwaysApply: true\n---\nUse tabs.");
		write(&off.join("rules/hush.md"), "---\nalwaysApply: true\n---\nHush.");
		let mut registry = InstalledPluginsRegistry::default();
		for (id, path, enabled) in [("style@m", &on, true), ("quiet@m", &off, false)] {
			registry
				.plugins
				.insert(Str::new(id), vec![InstalledPluginEntry {
					scope: InstallScope::User,
					install_path: path.clone(),
					version: Str::new_static("1"),
					installed_at: Str::new_static("t"),
					last_updated: Str::new_static("t"),
					git_commit_sha: None,
					enabled,
				}]);
		}
		write(
			&data.join("plugins/installed_plugins.json"),
			&serde_json::to_string(&registry).unwrap(),
		);

		let plugins = ClaudePlugins::resolve(&data, &project, None);
		let rules = ActiveRules::discover(&project, &home, &home.join(".o2"), &plugins);
		let rule = rules.get("house-style").expect("installed plugin rule");
		assert_eq!(rule.provider, "claude-plugins");
		assert_eq!(rule.level, Level::User);
		assert!(rules.get("hush").is_none(), "a disabled plugin never loads");
	}

	#[test]
	fn walk_up_stops_at_the_repository_root_outside_home() {
		let temp = tempfile::tempdir().unwrap();
		let root = temp.path().canonicalize().unwrap();
		let home = root.join("home");
		let repo = root.join("srv/repo");
		fs::create_dir_all(repo.join(".git")).unwrap();
		let project = repo.join("pkg");
		fs::create_dir_all(&project).unwrap();
		fs::create_dir_all(&home).unwrap();
		assert_eq!(walk_up(&project, &home), [project.clone(), repo.clone()]);
		// No repository, beneath home: the home directory itself is included.
		let bare = home.join("scratch");
		fs::create_dir_all(&bare).unwrap();
		assert_eq!(walk_up(&bare, &home), [bare.clone(), home.clone()]);
		// A repository rooted at home keeps the home-level file.
		fs::create_dir_all(home.join(".git")).unwrap();
		assert_eq!(walk_up(&bare, &home), [bare, home.clone()]);
	}

	#[test]
	fn rules_bucket_by_frontmatter_and_resolve_name_collisions_first_wins() {
		let (_temp, home, repo, project) = layout();
		let config_root = home.join(".o2");
		write(
			&repo.join(".omp/rules/style.md"),
			"---\ndescription: House style\nglobs: \"*.rs, *.toml\"\n---\nUse tabs.\n",
		);
		write(
			&repo.join(".omp/rules/always.mdc"),
			"---\nalwaysApply: true\n---\nNever force-push.\n",
		);
		write(&repo.join(".omp/rules/hidden.md"), "no frontmatter, only rule:// reaches this\n");
		write(
			&repo.join(".omp/rules/sub-only.md"),
			"---\ndescription: Subagents\nagents: [sub, review-*]\n---\nbody\n",
		);
		write(
			&repo.join(".omp/RULES.md"),
			"---\ndescription: ignored for sticky\n---\nSticky project rules.\n",
		);
		write(
			&config_root.join("agent/rules/style.md"),
			"---\ndescription: shadowed by project\n---\nuser\n",
		);
		write(&config_root.join("agent/RULES.md"), "User sticky.\n");
		write(
			&project.join(".cursor/rules/cursor.mdc"),
			"---\ndescription: Cursor rule\nglobs:\n  - src/**\n---\ncursor body\n",
		);
		write(&project.join(".cursorrules"), "legacy cursor rules\n");
		write(&repo.join(".clinerules"), "legacy cline rules\n");
		write(&repo.join(".omp/rules/broken.md"), "---\ndescription: [unclosed\n---\nbody\n");

		let rules = ActiveRules::discover(&project, &home, &config_root, &ClaudePlugins::default());
		let names = rules
			.rules
			.iter()
			.map(|rule| rule.name.as_str())
			.collect::<Vec<_>>();
		assert_eq!(names, [
			"always",
			"hidden",
			"style",
			"sub-only",
			"RULES",
			"RULES@project",
			"cursor",
			"cursorrules",
			"clinerules"
		]);
		let style = rules.get("style").unwrap();
		assert_eq!(style.provider, "native");
		assert_eq!(style.level, Level::Project);
		assert_eq!(style.globs, [Str::new_static("*.rs"), Str::new_static("*.toml")]);
		assert_eq!(style.content, "Use tabs.\n");
		assert!(rules.get("RULES@project").unwrap().always_apply, "sticky RULES.md always applies");
		assert!(rules.get("cursorrules").unwrap().always_apply);
		assert_eq!(rules.get("sub-only").unwrap().agents, [
			Str::new_static("sub"),
			Str::new_static("review-*")
		]);
		let messages = rules
			.warnings
			.iter()
			.map(|warning| warning.message.as_str())
			.collect::<Vec<_>>();
		assert!(messages.iter().any(|m| m.contains("collision")), "{messages:?}");
		assert!(messages.iter().any(|m| m.contains("frontmatter")), "{messages:?}");

		let facts = rules.prompt_facts(MAIN_AGENT);
		let always = facts
			.always_apply
			.iter()
			.map(|row| row["name"].as_str().unwrap())
			.collect::<Vec<_>>();
		assert_eq!(always, ["always", "RULES", "RULES@project", "cursorrules", "clinerules"]);
		let rulebook = facts
			.rulebook
			.iter()
			.map(|row| (row["name"].as_str().unwrap(), row["globs"].as_array().unwrap().len()))
			.collect::<Vec<_>>();
		assert_eq!(rulebook, [("style", 2), ("cursor", 1)], "hidden and sub-only stay out");
		let sub = rules.prompt_facts(AgentName::from_ref("review-bot"));
		assert!(sub.rulebook.iter().any(|row| row["name"] == "sub-only"));
	}

	#[tokio::test]
	async fn rule_url_reads_lists_and_completes() {
		let (_temp, home, repo, project) = layout();
		write(
			&repo.join(".omp/rules/style.md"),
			"---\ndescription: House style\n---\nline one\nline two\n",
		);
		let rules = Arc::new(ActiveRules::discover(
			&project,
			&home,
			&home.join(".o2"),
			&ClaudePlugins::default(),
		));
		let resolver = Arc::new(RuleScope::new(rules, MAIN_AGENT.to_owned())).resolver();
		assert_eq!(resolver.entry().scheme, Scheme::Rule);
		let body = resolver.read("style", &ParsedSelector::None).await.unwrap();
		assert_eq!(std::str::from_utf8(&body).unwrap(), "line one\nline two\n");
		let index = resolver.read("", &ParsedSelector::None).await.unwrap();
		assert!(
			std::str::from_utf8(&index)
				.unwrap()
				.contains("- rule://style: House style")
		);
		let listing = resolver.list("", 10, usize::MAX).await.unwrap();
		assert_eq!(listing.entries[0].uri, "rule://style");
		let completions = resolver.complete("sty", 5).await.unwrap();
		assert_eq!(completions[0].value, "rule://style");
		let missing = resolver
			.read("nope", &ParsedSelector::None)
			.await
			.unwrap_err();
		assert!(matches!(missing, Fault::Source { .. }));
	}

	/// A rule scoped by `agents` (raw frontmatter entries), always applied.
	fn scoped_rule(name: &'static str, agents: &[&'static str]) -> Rule {
		Rule {
			name:           Str::new_static(name),
			path:           PathBuf::from(format!("/rules/{name}.md")),
			content:        Str::new_static("body\n"),
			description:    Some(Str::new_static("scoped")),
			globs:          Vec::new(),
			always_apply:   true,
			condition:      Vec::new(),
			scope:          Vec::new(),
			interrupt_mode: None,
			agents:         agents.iter().copied().map(Str::new_static).collect(),
			provider:       Str::new_static("native"),
			level:          Level::Project,
		}
	}

	fn admitted(rules: &ActiveRules, agent: &str) -> Vec<String> {
		rules
			.for_agent(AgentName::from_ref(agent))
			.map(|rule| rule.name.to_string())
			.collect()
	}

	#[test]
	fn negated_agents_subtract_from_the_positive_set() {
		let rules = ActiveRules {
			rules:    vec![
				scoped_rule("everyone", &[]),
				scoped_rule("not-reviewer", &["!reviewer"]),
				scoped_rule("not-scout-or-task", &["!scout", "!task"]),
				scoped_rule("reviewers-but-bot", &["review*", "!review-bot"]),
				scoped_rule("negation-wins", &["main", "!main"]),
				scoped_rule("spaced", &["! scout"]),
			],
			warnings: Vec::new(),
		};
		assert_eq!(admitted(&rules, "main"), [
			"everyone",
			"not-reviewer",
			"not-scout-or-task",
			"spaced"
		]);
		assert_eq!(admitted(&rules, "reviewer"), [
			"everyone",
			"not-scout-or-task",
			"reviewers-but-bot",
			"spaced"
		]);
		assert_eq!(admitted(&rules, "Review-Bot"), [
			"everyone",
			"not-reviewer",
			"not-scout-or-task",
			"spaced"
		]);
		assert_eq!(admitted(&rules, "scout"), ["everyone", "not-reviewer"]);
		assert_eq!(admitted(&rules, "task"), ["everyone", "not-reviewer", "spaced"]);
	}

	#[test]
	fn negated_agents_parse_from_every_frontmatter_spelling() {
		let (_temp, home, repo, project) = layout();
		for (name, agents) in [
			("quoted", "[\"!Reviewer\", \"review-*\"]"),
			("comma", "\"!scout, !task\""),
			("bare-flow", "[!reviewer, main, ! scout]"),
			("bare-one", "!reviewer"),
			("bare-block", "\n  - !reviewer\n  - \"!scout\""),
		] {
			write(
				&repo.join(format!(".omp/rules/{name}.md")),
				&format!("---\nalwaysApply: true\nagents: {agents}\n---\nbody\n"),
			);
		}
		let rules =
			ActiveRules::discover(&project, &home, &home.join(".o2"), &ClaudePlugins::default());
		assert!(rules.warnings.is_empty(), "{:?}", rules.warnings);
		let agents = |name: &str| {
			rules
				.get(name)
				.unwrap()
				.agents
				.iter()
				.map(Str::as_str)
				.collect::<Vec<_>>()
		};
		assert_eq!(agents("quoted"), ["!reviewer", "review-*"]);
		assert_eq!(agents("comma"), ["!scout", "!task"]);
		assert_eq!(agents("bare-flow"), ["!reviewer", "main", "!scout"]);
		assert_eq!(agents("bare-one"), ["!reviewer"]);
		assert_eq!(agents("bare-block"), ["!reviewer", "!scout"]);

		let mine = |agent: &str| admitted(&rules, agent);
		assert_eq!(mine("review-bot"), ["bare-block", "bare-one", "comma", "quoted"]);
		assert_eq!(mine("reviewer"), ["comma"]);
		assert_eq!(mine("scout"), ["bare-one"]);
		assert_eq!(mine("main"), ["bare-block", "bare-flow", "bare-one", "comma"]);
	}

	#[tokio::test]
	async fn rule_url_refuses_rules_excluded_for_the_live_agent() {
		let rules = Arc::new(ActiveRules {
			rules:    vec![
				scoped_rule("main-only", &["main"]),
				scoped_rule("not-scout", &["!scout"]),
				scoped_rule("scout-only", &["scout"]),
			],
			warnings: Vec::new(),
		});
		let scope = Arc::new(RuleScope::new(Arc::clone(&rules), AgentName::new("scout")));
		let resolver = scope.resolver();

		assert_eq!(
			rules.lookup("main-only", AgentName::from_ref("scout")),
			Err(RuleLookupError::Excluded {
				rule:  Str::new_static("main-only"),
				agent: AgentName::new("scout"),
			})
		);
		assert_eq!(
			rules.lookup("nope", AgentName::from_ref("scout")),
			Err(RuleLookupError::Unknown {
				name:      Str::new_static("nope"),
				available: Box::new([Str::new_static("scout-only")]),
			}),
			"the unknown-rule listing names only rules the agent may read"
		);
		assert_eq!(
			rules
				.lookup("scout-only", AgentName::from_ref("scout"))
				.unwrap()
				.name,
			"scout-only"
		);

		for excluded in ["main-only", "not-scout"] {
			let fault = resolver
				.read(excluded, &ParsedSelector::None)
				.await
				.unwrap_err();
			assert_eq!(
				fault,
				Fault::from(RuleLookupError::Excluded {
					rule:  Str::new(excluded),
					agent: AgentName::new("scout"),
				})
			);
			assert!(fault.message().contains("excluded for agent `scout`"), "{fault:?}");
			assert!(resolver.path(excluded).await.is_err(), "no path leaks for {excluded}");
		}
		let body = resolver
			.read("scout-only", &ParsedSelector::None)
			.await
			.unwrap();
		assert_eq!(&body[..], b"body\n");

		let index = resolver.read("", &ParsedSelector::None).await.unwrap();
		let index = std::str::from_utf8(&index).unwrap();
		assert!(index.contains("rule://scout-only"), "{index}");
		assert!(!index.contains("main-only") && !index.contains("not-scout"), "{index}");
		let listing = resolver.list("", 10, usize::MAX).await.unwrap();
		assert_eq!(
			listing
				.entries
				.iter()
				.map(|entry| entry.uri.as_str())
				.collect::<Vec<_>>(),
			["rule://scout-only"]
		);
		let completions = resolver.complete("", 10).await.unwrap();
		assert_eq!(
			completions
				.iter()
				.map(|row| row.value.as_str())
				.collect::<Vec<_>>(),
			["rule://scout-only"],
			"autocomplete never offers an excluded rule"
		);

		// The live session switches to `main`: the scope follows it.
		scope.select(MAIN_AGENT.to_owned());
		assert!(
			resolver
				.read("main-only", &ParsedSelector::None)
				.await
				.is_ok()
		);
		assert!(
			resolver
				.read("scout-only", &ParsedSelector::None)
				.await
				.is_err()
		);
		let completions = resolver.complete("", 10).await.unwrap();
		assert_eq!(
			completions
				.iter()
				.map(|row| row.value.as_str())
				.collect::<Vec<_>>(),
			["rule://main-only", "rule://not-scout"]
		);
	}
}
