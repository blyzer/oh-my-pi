//! Installed Claude-format marketplace plugins.
//!
//! `omp ext install` (and the `/plugins` overlay) materialize marketplace
//! plugins into `<data>/plugins/cache/plugins/...` and record them in an
//! `installed_plugins.json` registry: `<data>/plugins/` for the user scope,
//! `<project>/.omp/plugins/` for the project scope. This module owns that
//! registry schema and resolves every *enabled* install into a
//! [`ClaudePlugin`]: a canonical, contained plugin root plus the component
//! locations the runtime's discovery seams load.
//!
//! Layout (Claude Code plugin reference):
//!
//! * `.claude-plugin/plugin.json` (optional manifest; `.omp-plugin/plugin.json`
//!   wins when both exist) may redirect `skills`, `commands`, `agents`,
//!   `hooks`, `mcpServers`, `outputStyles`, and `lspServers`;
//! * `skills/<name>/SKILL.md`, `commands/*.md`, `rules/*.md` (an OMP v1
//!   extension), `.mcp.json`;
//! * language servers: the [`LSP_CONFIG_NAMES`] at the root plus manifest
//!   `lspServers` (an inline server map or contained file paths); debug
//!   adapters: the [`DAP_CONFIG_NAMES`] at the root. This module only locates
//!   them; the document authority validates each declaration with its own LSP
//!   and DAP parsers and reports a rejected one as
//!   [`PluginDiagnostic::InvalidComponent`];
//! * hooks: `hooks/hooks.json` plus manifest `hooks` (a path, an inline event
//!   map, or an array of either), parsed into [`PluginHook`]s by
//!   [`crate::claude_hooks`]; the runtime runs them on its hook surface;
//! * `agents/`, `output-styles/`, `tools/`, and OMP v1's JS/TS `hooks/pre|post`
//!   factories have no runtime home yet and surface as
//!   [`PluginDiagnostic::Unsupported`] instead of being dropped silently.
//!
//! No process a plugin declares (MCP server, language server, debug adapter,
//! hook command) runs before the operator approved it: resolution attaches
//! each plugin's
//! approved launch digests from the local grant file, and every launching
//! seam gates through [`ClaudePlugin::admit_launch`]
//! ([`crate::plugin_command`]).
//!
//! Plugin-shipped paths use `${CLAUDE_PLUGIN_ROOT}`; [`expand_plugin_vars`]
//! and [`resolve_plugin_command`] are the one expansion every runtime seam
//! applies.
//!
//! An install whose root carries an Agent Plugins 1.0 `plugin.json` resolves
//! to [`PluginLayout::AgentPlugins`]: its components belong to the Agent
//! Plugins discovery seams, never to this Claude-layout projection.
//!
//! Precedence mirrors the `omp ext` projection: an enabled project install
//! shadows the user install of the same plugin id; a disabled entry never
//! loads and never shadows.
//!
//! Claude Code's own registry (`<claude>/plugins/installed_plugins.json`,
//! [`ClaudeCodeHome`]) merges in read-only, as OMP v1 did: an id any omp
//! registry records wins over Claude's entries for it, Claude's
//! `enabledPlugins` settings override its registry's enabled state, and a
//! project- or local-scope Claude entry loads only in its `projectPath`
//! unless `enabledPlugins` opts it in. Those plugins carry
//! [`PluginSource::ClaudeCode`].

use std::{
	collections::{BTreeMap, BTreeSet},
	fs, io, iter,
	path::{Path, PathBuf},
	sync::Arc,
};

use omp_core::{Hash32, Str};
use serde::{Deserialize, Serialize, de::IgnoredAny};
use serde_json::value::RawValue;
use strum::{Display, IntoStaticStr};

use crate::{
	claude_hooks::{ClaudeHookEvent, HookHandlerGap, PluginHook},
	plugin_command::{
		CommandApprovals, PluginCommandBlocked, PluginId, PluginLaunch, plugin_command_digest,
	},
	trust::{GrantsFile, GrantsFileError, grants_path},
	workspace_trust::inventory::{
		CLAUDE_SETTINGS_FILES, DAP_CONFIG_NAMES, LSP_CONFIG_NAMES, PROJECT_PLUGINS_DIR,
	},
};

/// Registry file name inside a scope's plugin directory.
pub const REGISTRY_FILE: &str = "installed_plugins.json";

/// Registry schema version `omp ext` reads and writes.
pub const INSTALLED_PLUGINS_VERSION: u32 = 2;

/// Agent Plugins 1.0 root-manifest schema.
pub const AGENT_PLUGIN_SCHEMA: &str = "https://agent-plugins.org/schemas/1.0.0/plugin.schema.json";

/// The user-scope plugin directory beneath the data directory.
#[must_use]
pub fn user_plugins_dir(data_dir: &Path) -> PathBuf {
	data_dir.join("plugins")
}

/// The materialized-plugin cache beneath the user-scope plugin directory
/// ([`user_plugins_dir`]): `omp ext install` copies every plugin here,
/// whichever scope's registry records it.
#[must_use]
pub fn plugin_cache_dir(user_plugins: &Path) -> PathBuf {
	user_plugins.join("cache/plugins")
}

/// The project-scope plugin directory beneath a project root.
#[must_use]
pub fn project_plugins_dir(project_root: &Path) -> PathBuf {
	project_root.join(PROJECT_PLUGINS_DIR)
}

/// `installed_plugins.json`: plugin id (`name@marketplace`) to its installs.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct InstalledPluginsRegistry {
	/// Schema version; [`INSTALLED_PLUGINS_VERSION`].
	#[serde(default = "installed_plugins_version")]
	pub version: u32,
	/// Installs keyed by `name@marketplace`.
	#[serde(default)]
	pub plugins: BTreeMap<Str, Vec<InstalledPluginEntry>>,
}

impl Default for InstalledPluginsRegistry {
	fn default() -> Self {
		Self { version: INSTALLED_PLUGINS_VERSION, plugins: BTreeMap::new() }
	}
}

const fn installed_plugins_version() -> u32 {
	INSTALLED_PLUGINS_VERSION
}

impl InstalledPluginsRegistry {
	/// Reads a registry; a missing file is the empty registry.
	///
	/// # Errors
	///
	/// [`RegistryError`] when the file is unreadable, malformed, or written by
	/// an unsupported schema version.
	pub fn read(path: &Path) -> Result<Self, RegistryError> {
		let bytes = match fs::read(path) {
			Ok(bytes) => bytes,
			Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Self::default()),
			Err(source) => return Err(RegistryError::Read { path: path.to_owned(), source }),
		};
		Self::from_slice(path, &bytes)
	}

	/// Parses registry `bytes` read from `path`, which errors name.
	///
	/// # Errors
	///
	/// [`RegistryError`] when the bytes are malformed or written by an
	/// unsupported schema version.
	pub fn from_slice(path: &Path, bytes: &[u8]) -> Result<Self, RegistryError> {
		let registry: Self = serde_json::from_slice(bytes)
			.map_err(|source| RegistryError::Parse { path: path.to_owned(), source })?;
		if registry.version != INSTALLED_PLUGINS_VERSION {
			return Err(RegistryError::Version {
				path:    path.to_owned(),
				version: registry.version,
			});
		}
		Ok(registry)
	}
}

/// One recorded installation of a plugin.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InstalledPluginEntry {
	/// Scope the install was recorded for.
	pub scope:          InstallScope,
	/// Materialized plugin root.
	pub install_path:   PathBuf,
	/// Installed version.
	pub version:        Str,
	/// First installation time.
	pub installed_at:   Str,
	/// Last (re)installation time.
	pub last_updated:   Str,
	/// Resolved commit of a git-sourced install.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub git_commit_sha: Option<Str>,
	/// Whether the runtime loads this install.
	#[serde(default = "enabled_by_default")]
	pub enabled:        bool,
}

const fn enabled_by_default() -> bool {
	true
}

/// Scope recorded on a registry entry (Claude Code's vocabulary).
#[derive(
	Clone,
	Copy,
	Debug,
	Deserialize,
	Display,
	Eq,
	IntoStaticStr,
	Ord,
	PartialEq,
	PartialOrd,
	Serialize,
)]
#[serde(rename_all = "lowercase")]
#[strum(serialize_all = "lowercase")]
pub enum InstallScope {
	/// Installed for the user.
	User,
	/// Installed for one project.
	Project,
	/// Claude Code's project-local install.
	Local,
}

/// Where a resolved plugin sits in the precedence ladder: the registry file
/// it came from.
#[derive(Clone, Copy, Debug, Display, Eq, IntoStaticStr, Ord, PartialEq, PartialOrd)]
#[strum(serialize_all = "lowercase")]
pub enum PluginScope {
	/// `<project>/.omp/plugins/installed_plugins.json`.
	Project,
	/// `<data>/plugins/installed_plugins.json`.
	User,
}

/// Which installer recorded a resolved plugin.
#[derive(Clone, Copy, Debug, Display, Eq, IntoStaticStr, Ord, PartialEq, PartialOrd)]
#[strum(serialize_all = "kebab-case")]
pub enum PluginSource {
	/// omp's registries, managed by `omp ext` and `/plugins`.
	Omp,
	/// Claude Code's registry: loaded read-only, managed by Claude Code.
	ClaudeCode,
}

/// Claude Code's configuration directory: `$CLAUDE_CONFIG_DIR`, else
/// `~/.claude`. Plugin resolution only ever reads it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClaudeCodeHome {
	config_dir: PathBuf,
}

impl ClaudeCodeHome {
	/// Claude Code's configuration directory for this process:
	/// `$CLAUDE_CONFIG_DIR` (Claude Code's own override), else `<home>/.claude`
	/// with the home directory every omp path derives from.
	#[must_use]
	pub fn detect() -> Option<Self> {
		std::env::var_os("CLAUDE_CONFIG_DIR")
			.filter(|dir| !dir.is_empty())
			.map(PathBuf::from)
			.or_else(|| omp_core::dirs::home_dir().map(|home| home.join(".claude")))
			.map(Self::at)
	}

	/// A Claude Code configuration directory at `config_dir`.
	#[must_use]
	pub fn at(config_dir: impl Into<PathBuf>) -> Self {
		Self { config_dir: config_dir.into() }
	}

	/// The configuration directory.
	#[must_use]
	pub fn config_dir(&self) -> &Path {
		&self.config_dir
	}

	/// Claude Code's `plugins/installed_plugins.json`.
	#[must_use]
	pub fn registry(&self) -> PathBuf {
		self.config_dir.join("plugins").join(REGISTRY_FILE)
	}
}

/// A plugin component kind, as diagnostics name it.
#[derive(Clone, Copy, Debug, Display, Eq, IntoStaticStr, Ord, PartialEq, PartialOrd)]
pub enum PluginComponent {
	/// `skills/<name>/SKILL.md`.
	#[strum(to_string = "skills")]
	Skills,
	/// `commands/*.md` slash commands.
	#[strum(to_string = "commands")]
	Commands,
	/// `rules/*.md` rules.
	#[strum(to_string = "rules")]
	Rules,
	/// `.mcp.json` / manifest `mcpServers`.
	#[strum(to_string = "MCP servers")]
	McpServers,
	/// `hooks/hooks.json`, manifest `hooks`, or v1 `hooks/pre|post` scripts.
	#[strum(to_string = "hooks")]
	Hooks,
	/// `agents/*.md` subagent definitions.
	#[strum(to_string = "agents")]
	Agents,
	/// `output-styles/` / manifest `outputStyles`.
	#[strum(to_string = "output styles")]
	OutputStyles,
	/// `.lsp.json` / manifest `lspServers`.
	#[strum(to_string = "LSP servers")]
	LspServers,
	/// `.dap.json` debug adapters.
	#[strum(to_string = "DAP adapters")]
	DapAdapters,
	/// v1 `tools/*.ts|js` custom tools.
	#[strum(to_string = "custom tools")]
	Tools,
}

/// Why a plugin registry could not be read.
#[derive(Debug, thiserror::Error)]
pub enum RegistryError {
	/// The registry exists but could not be read.
	#[error("cannot read plugin registry {}", path.display())]
	Read {
		/// Registry path.
		path:   PathBuf,
		/// I/O failure.
		#[source]
		source: io::Error,
	},
	/// The registry is not valid JSON of the expected shape.
	#[error("plugin registry {} is malformed", path.display())]
	Parse {
		/// Registry path.
		path:   PathBuf,
		/// Decoding failure.
		#[source]
		source: serde_json::Error,
	},
	/// The registry was written by an unsupported schema version.
	#[error("plugin registry {} has unsupported version {version}", path.display())]
	Version {
		/// Registry path.
		path:    PathBuf,
		/// Recorded version.
		version: u32,
	},
}

/// A non-fatal problem found while resolving installed plugins. Each one
/// names what did not load; the rest of the plugin set still loads.
#[derive(Debug, thiserror::Error)]
pub enum PluginDiagnostic {
	/// A whole registry was skipped.
	#[error(transparent)]
	Registry(#[from] RegistryError),
	/// A registry key is not `name@marketplace`.
	#[error("plugin registry {} has invalid plugin id `{id}` (expected name@marketplace)", registry.display())]
	InvalidId {
		/// Registry path.
		registry: PathBuf,
		/// Offending key.
		id:       Str,
	},
	/// The recorded install directory does not exist or cannot be resolved.
	#[error("plugin `{plugin}` is installed at {}, which cannot be opened", path.display())]
	InstallMissing {
		/// Plugin id.
		plugin: Str,
		/// Recorded install path.
		path:   PathBuf,
		/// Resolution failure.
		#[source]
		source: io::Error,
	},
	/// The recorded install path is not a directory.
	#[error("plugin `{plugin}` install path {} is not a directory", path.display())]
	InstallNotDirectory {
		/// Plugin id.
		plugin: Str,
		/// Recorded install path.
		path:   PathBuf,
	},
	/// The plugin manifest exists but cannot be read.
	#[error("plugin `{plugin}` manifest {} cannot be read; plugin not loaded", path.display())]
	ManifestRead {
		/// Plugin id.
		plugin: Str,
		/// Manifest path.
		path:   PathBuf,
		/// I/O failure.
		#[source]
		source: io::Error,
	},
	/// The plugin manifest is malformed.
	#[error("plugin `{plugin}` manifest {} is malformed; plugin not loaded", path.display())]
	ManifestParse {
		/// Plugin id.
		plugin: Str,
		/// Manifest path.
		path:   PathBuf,
		/// Decoding failure.
		#[source]
		source: serde_json::Error,
	},
	/// A manifest-declared component path escapes the plugin root.
	#[error("plugin `{plugin}` declares {component} outside its root: {}", path.display())]
	ComponentOutsideRoot {
		/// Plugin id.
		plugin:    Str,
		/// Component kind.
		component: PluginComponent,
		/// Declared path.
		path:      PathBuf,
	},
	/// A manifest-declared component path does not exist.
	#[error("plugin `{plugin}` declares {component} at {}, which does not exist", path.display())]
	ComponentMissing {
		/// Plugin id.
		plugin:    Str,
		/// Component kind.
		component: PluginComponent,
		/// Declared path.
		path:      PathBuf,
	},
	/// The plugin ships a component the runtime has no home for.
	#[error("plugin `{plugin}` ships {component} at {}, which omp does not load", path.display())]
	Unsupported {
		/// Plugin id.
		plugin:    Str,
		/// Component kind.
		component: PluginComponent,
		/// Where the component was found.
		path:      PathBuf,
	},
	/// A hooks declaration names an event Claude Code does not define.
	#[error("plugin `{plugin}` hooks unknown event `{event}` in {}; not loaded", path.display())]
	UnknownHookEvent {
		/// Plugin id.
		plugin: Str,
		/// Declared event name.
		event:  Str,
		/// Declaration file, or the manifest for an inline declaration.
		path:   PathBuf,
	},
	/// A hook event with no faithful omp counterpart; only that event's hooks
	/// are skipped. Reported once per plugin and event, and named to the
	/// operator at launch ([`ClaudePlugins::unsupported_hook_events`]).
	#[error(
		"plugin `{plugin}` declares a {event} hook: {event} is not supported by omp; this hook will not run ({})",
		path.display()
	)]
	UnsupportedHookEvent {
		/// Plugin id.
		plugin: Str,
		/// The unsupported event.
		event:  ClaudeHookEvent,
		/// Declaration file, or the manifest for an inline declaration.
		path:   PathBuf,
	},
	/// One hook handler omp cannot run as declared; the event's other
	/// handlers still load.
	#[error("plugin `{plugin}` {event} hook in {} does not run", path.display())]
	UnsupportedHookHandler {
		/// Plugin id.
		plugin: Str,
		/// The handler's event.
		event:  ClaudeHookEvent,
		/// What omp does not support.
		#[source]
		reason: HookHandlerGap,
		/// Declaration file, or the manifest for an inline declaration.
		path:   PathBuf,
	},
	/// A tool matcher names tools omp does not have; those names never match.
	#[error(
		"plugin `{plugin}` {event} matcher names tools omp does not have ({}) in {}; they never match",
		tools.join(", "),
		path.display()
	)]
	UnmappedHookMatcher {
		/// Plugin id.
		plugin: Str,
		/// The matcher's event.
		event:  ClaudeHookEvent,
		/// Unmapped names (or the pattern matching no mapped name).
		tools:  Box<[Str]>,
		/// Declaration file, or the manifest for an inline declaration.
		path:   PathBuf,
	},
	/// A matcher is not a valid regular expression; its group does not load.
	#[error("plugin `{plugin}` {event} matcher in {} is not a valid pattern", path.display())]
	InvalidHookMatcher {
		/// Plugin id.
		plugin: Str,
		/// The matcher's event.
		event:  ClaudeHookEvent,
		/// Declaration file, or the manifest for an inline declaration.
		path:   PathBuf,
		/// Compilation failure.
		#[source]
		source: regex::Error,
	},
	/// A Claude Code settings file is not JSON; its `enabledPlugins` are
	/// ignored.
	#[error("Claude Code settings {} are malformed; their enabledPlugins are ignored", path.display())]
	ClaudeSettings {
		/// Settings path.
		path:   PathBuf,
		/// Decoding failure.
		#[source]
		source: serde_json::Error,
	},
	/// A Claude Code registry entry has no `installPath`.
	#[error("Claude Code plugin `{plugin}` entry in {} has no installPath", registry.display())]
	ClaudeEntryWithoutPath {
		/// Plugin id.
		plugin:   Str,
		/// Registry path.
		registry: PathBuf,
	},
	/// A located declaration failed the owning runtime seam's validation; it
	/// does not load and the rest of the plugin set still does.
	#[error("plugin `{plugin}` declares invalid {component} in {}; not loaded", path.display())]
	InvalidComponent {
		/// Plugin id.
		plugin:    Str,
		/// Component kind.
		component: PluginComponent,
		/// Declaration file, or the manifest for an inline declaration.
		path:      PathBuf,
		/// The validating parser's typed error. Erased because that parser
		/// (the document authority's LSP/DAP configuration) lives in a crate
		/// downstream of this one; the concrete error stays downcastable.
		#[source]
		source:    Box<dyn std::error::Error + Send + Sync>,
	},
	/// A plugin server would launch a command the operator has not
	/// approved; that server does not start.
	#[error(transparent)]
	CommandNotApproved(#[from] PluginCommandBlocked),
	/// The local grant file could not be read, so no plugin launch counts as
	/// approved.
	#[error(
		"plugin command approvals cannot be read; plugin servers and hooks that run commands do not \
		 start"
	)]
	CommandApprovals(#[source] GrantsFileError),
}

/// One enabled, resolved plugin install.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClaudePlugin {
	/// Registry id, `name@marketplace`: its approval identity, and the
	/// namespace of its MCP servers and hooks.
	pub id:                PluginId,
	/// Plugin name (the id before `@`); namespaces its commands.
	pub name:              Str,
	/// Marketplace name (the id after `@`).
	pub marketplace:       Str,
	/// Recorded version.
	pub version:           Str,
	/// Registry scope the install came from.
	pub scope:             PluginScope,
	/// Which installer recorded it.
	pub source:            PluginSource,
	/// Canonical plugin root (the value of `${CLAUDE_PLUGIN_ROOT}`).
	pub root:              PathBuf,
	/// Package format and its loadable components.
	pub layout:            PluginLayout,
	/// [`plugin_command_digest`]s of the launches the operator approved for
	/// this plugin, read from the local grant file at resolution.
	pub approved_commands: Arc<[Hash32]>,
}

/// The package format of one install.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PluginLayout {
	/// Claude Code plugin layout, with its resolved loadable components.
	Claude(ClaudeComponents),
	/// Agent Plugins 1.0 package, loaded by the Agent Plugins seams.
	AgentPlugins,
}

/// Contained, existing component locations of a Claude-layout plugin.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ClaudeComponents {
	/// Directories of `<name>/SKILL.md` skills.
	pub skills:   Box<[PathBuf]>,
	/// Command directories or single `.md` command files.
	pub commands: Box<[PathBuf]>,
	/// `rules/` directory.
	pub rules:    Option<PathBuf>,
	/// MCP server declarations.
	pub mcp:      Box<[ConfigDeclaration]>,
	/// Language-server declarations, lowest precedence first: the root
	/// [`LSP_CONFIG_NAMES`], then manifest `lspServers`.
	pub lsp:      Box<[ConfigDeclaration]>,
	/// Debug-adapter declarations, lowest precedence first: the root
	/// [`DAP_CONFIG_NAMES`].
	pub dap:      Box<[ConfigDeclaration]>,
	/// Hooks from `hooks/hooks.json` then manifest `hooks`, in declaration
	/// order.
	pub hooks:    Box<[PluginHook]>,
}

/// Where a plugin declares a server map (MCP servers, language servers, or
/// debug adapters) or a hook-event map.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ConfigDeclaration {
	/// A JSON (or, for LSP/DAP, YAML by extension) file: a `{kind: {...}}`
	/// wrapper or a flat `{name: server}` map.
	File(PathBuf),
	/// A server map written inline in the plugin manifest.
	Inline {
		/// Manifest path (the declaration's source).
		manifest: PathBuf,
		/// The raw JSON object (servers, or hook events).
		servers:  Str,
	},
}

impl ConfigDeclaration {
	/// The file the declaration was read from: the declaration file itself, or
	/// the manifest for an inline map.
	#[must_use]
	pub fn path(&self) -> &Path {
		match self {
			Self::File(path) | Self::Inline { manifest: path, .. } => path,
		}
	}
}

/// Expands the plugin path variables in one plugin-declared string.
///
/// `${CLAUDE_PLUGIN_ROOT}`, `${OMP_PLUGIN_ROOT}` and `${PLUGIN_ROOT}` become
/// `root`; `${CLAUDE_PLUGIN_DATA}` and `${PLUGIN_DATA}` become `data` when the
/// plugin has a data directory. A value without variables is returned as is.
#[must_use]
pub fn expand_plugin_vars(value: Str, root: &Path, data: Option<&Path>) -> Str {
	if !value.contains("PLUGIN_ROOT}") && !value.contains("PLUGIN_DATA}") {
		return value;
	}
	let root = root.to_string_lossy();
	let replaced = value
		.replace("${PLUGIN_ROOT}", root.as_ref())
		.replace("${CLAUDE_PLUGIN_ROOT}", root.as_ref())
		.replace("${OMP_PLUGIN_ROOT}", root.as_ref());
	match data {
		Some(data) => {
			let data = data.to_string_lossy();
			Str::new(
				replaced
					.replace("${PLUGIN_DATA}", data.as_ref())
					.replace("${CLAUDE_PLUGIN_DATA}", data.as_ref()),
			)
		},
		None => Str::new(replaced),
	}
}

/// A plugin's `${CLAUDE_PLUGIN_DATA}` directory under omp's data directory.
///
/// `<data>/plugins/data/<id>`, the id with every character but ASCII
/// letters, digits, `_`, and `-` replaced by `-`, as Claude Code names its
/// own. Never Claude Code's directory, which omp does not write.
#[must_use]
pub fn plugin_data_dir(data_dir: &Path, id: &str) -> PathBuf {
	user_plugins_dir(data_dir)
		.join("data")
		.join(PluginId::from_ref(id).dir_name())
}

/// Resolves a plugin's path-like relative command (`./bin/server`,
/// `../tool`) against `base`, the directory it names a file in; bare
/// executables (`npx`) and absolute paths are returned unchanged.
#[must_use]
pub fn resolve_plugin_command(command: Str, base: &Path) -> Str {
	if command.starts_with("./") || command.starts_with("../") {
		let relative = command
			.as_str()
			.strip_prefix("./")
			.unwrap_or(command.as_str());
		Str::new(base.join(relative).to_string_lossy())
	} else {
		command
	}
}

impl ClaudePlugin {
	/// Claude-layout components; `None` for an Agent Plugins package.
	#[must_use]
	pub const fn claude_components(&self) -> Option<&ClaudeComponents> {
		match &self.layout {
			PluginLayout::Claude(components) => Some(components),
			PluginLayout::AgentPlugins => None,
		}
	}

	/// The approval key of `launch` for this plugin at its recorded version.
	#[must_use]
	pub fn command_digest(&self, launch: &PluginLaunch) -> Hash32 {
		plugin_command_digest(&self.id, &self.version, launch)
	}

	/// Admits `launch` when the operator approved it and every plugin file it
	/// names could be read; otherwise returns the diagnostic naming the
	/// plugin, the command, and how to approve it (or the file that cannot
	/// be read). A launching seam never starts a refused launch.
	pub fn admit_launch(&self, launch: PluginLaunch) -> Result<(), PluginCommandBlocked> {
		let digest = self.command_digest(&launch);
		if launch.files.unreadable().is_none() && self.approved_commands.contains(&digest) {
			return Ok(());
		}
		Err(
			PluginCommandBlocked::new(self.id.clone(), launch, digest)
				.with_other_approvals(!self.approved_commands.is_empty()),
		)
	}

	/// Every hook this plugin declares with the launch the approval gate
	/// keys it on ([`PluginHook::launch`]); empty for an Agent Plugins
	/// package.
	pub fn hook_launches(&self) -> impl Iterator<Item = (&PluginHook, PluginLaunch)> + '_ {
		self
			.claude_components()
			.into_iter()
			.flat_map(|components| components.hooks.iter())
			.map(|hook| (hook, hook.launch(&self.root)))
	}
}

/// The installed, enabled plugin set of one project, plus everything that did
/// not load.
#[derive(Debug, Default)]
pub struct ClaudePlugins {
	/// Resolved plugins: project scope first, then user scope, each in id
	/// order.
	pub plugins:           Vec<ClaudePlugin>,
	/// Non-fatal resolution diagnostics, one per problem.
	pub diagnostics:       Vec<PluginDiagnostic>,
	/// Every plugin command approval in the local grant file, read at
	/// resolution; the gate of launches outside [`Self::plugins`] (Agent
	/// Plugins packages). Empty when the grant file cannot be read.
	pub command_approvals: CommandApprovals,
}

impl ClaudePlugins {
	/// Resolves the user registry under `data_dir`, the project registry
	/// under `project_root`, and, when `claude` is given, Claude Code's
	/// registry and `enabledPlugins` settings (read-only).
	///
	/// Order and precedence follow OMP v1: enabled project installs first,
	/// then Claude Code installs whose id no omp registry records, then user
	/// installs; an enabled project install shadows every other install of
	/// its id.
	#[must_use]
	pub fn resolve(data_dir: &Path, project_root: &Path, claude: Option<&ClaudeCodeHome>) -> Self {
		let project_root =
			fs::canonicalize(project_root).unwrap_or_else(|_| project_root.to_path_buf());
		let mut out = Self::default();
		let project_path = project_plugins_dir(&project_root).join(REGISTRY_FILE);
		let user_path = user_plugins_dir(data_dir).join(REGISTRY_FILE);
		let project = out.read_registry(&project_path);
		let user = out.read_registry(&user_path);
		let claude = claude.and_then(|home| out.read_claude_code(home, &project_root));
		let approvals = GrantsFile::read(&grants_path(data_dir)).map_or_else(
			|source| {
				out.diagnostics
					.push(PluginDiagnostic::CommandApprovals(source));
				CommandApprovals::default()
			},
			|grants| grants.command_approvals(),
		);
		let project_enabled = project
			.plugins
			.iter()
			.filter(|(_, entries)| entries.iter().any(|entry| entry.enabled))
			.map(|(id, _)| id.clone())
			.collect::<BTreeSet<_>>();
		// OMP v1: the omp registries are authoritative; any install they
		// record for an id drops Claude Code's entries for it.
		let omp_ids = user
			.plugins
			.iter()
			.filter(|(_, entries)| !entries.is_empty())
			.map(|(id, _)| id.clone())
			.chain(project_enabled.iter().cloned())
			.collect::<BTreeSet<_>>();
		let mut seen_roots = BTreeSet::<PathBuf>::new();
		for (id, entries) in project.plugins {
			let candidates = entries
				.into_iter()
				.filter(|entry| entry.enabled)
				.map(|entry| Candidate::omp(entry, PluginScope::Project));
			out.admit(&project_path, id, PluginSource::Omp, candidates, &mut seen_roots);
		}
		if let Some((registry_path, claude)) = claude {
			for (id, candidates) in claude {
				if !omp_ids.contains(&id) {
					out.admit(
						&registry_path,
						id,
						PluginSource::ClaudeCode,
						candidates.into_iter(),
						&mut seen_roots,
					);
				}
			}
		}
		for (id, entries) in user.plugins {
			if project_enabled.contains(&id) {
				continue;
			}
			let candidates = entries
				.into_iter()
				.filter(|entry| entry.enabled)
				.map(|entry| Candidate::omp(entry, PluginScope::User));
			out.admit(&user_path, id, PluginSource::Omp, candidates, &mut seen_roots);
		}
		for plugin in &mut out.plugins {
			plugin.approved_commands = approvals.of(&plugin.id).collect();
		}
		out.command_approvals = approvals;
		out
	}

	/// Every hook event a resolved plugin declares that omp does not run,
	/// once per plugin and event: the launching host names each to the
	/// operator instead of the hook silently never running.
	pub fn unsupported_hook_events(&self) -> impl Iterator<Item = &PluginDiagnostic> + Clone + '_ {
		self
			.diagnostics
			.iter()
			.filter(|diagnostic| matches!(diagnostic, PluginDiagnostic::UnsupportedHookEvent { .. }))
	}

	fn read_registry(&mut self, path: &Path) -> InstalledPluginsRegistry {
		InstalledPluginsRegistry::read(path).unwrap_or_else(|error| {
			self.diagnostics.push(error.into());
			InstalledPluginsRegistry::default()
		})
	}

	/// Claude Code's enabled, applicable installs keyed by id, with the
	/// registry path; `None` when it has no registry.
	fn read_claude_code(
		&mut self,
		home: &ClaudeCodeHome,
		project_root: &Path,
	) -> Option<(PathBuf, BTreeMap<Str, Vec<Candidate>>)> {
		let registry_path = home.registry();
		let bytes = match fs::read(&registry_path) {
			Ok(bytes) => bytes,
			Err(error) if error.kind() == io::ErrorKind::NotFound => return None,
			Err(source) => {
				self
					.diagnostics
					.push(RegistryError::Read { path: registry_path, source }.into());
				return None;
			},
		};
		let registry = match serde_json::from_slice::<ClaudeCodeRegistryWire>(&bytes) {
			Ok(registry) => registry,
			Err(source) => {
				self
					.diagnostics
					.push(RegistryError::Parse { path: registry_path, source }.into());
				return None;
			},
		};
		let overrides = self.claude_enabled_overrides(home, project_root);
		let mut installs = BTreeMap::new();
		for (id, entries) in registry.plugins {
			let override_enabled = overrides.get(&id).copied();
			if override_enabled == Some(false) {
				continue;
			}
			let mut candidates = Vec::new();
			for entry in entries {
				if entry.enabled == Some(false) {
					continue;
				}
				let Some(install_path) = entry.install_path else {
					self
						.diagnostics
						.push(PluginDiagnostic::ClaudeEntryWithoutPath {
							plugin:   id.clone(),
							registry: registry_path.clone(),
						});
					continue;
				};
				let project_bound = matches!(entry.scope.as_deref(), Some("project" | "local"));
				// `enabledPlugins: true` opts a project-bound install into
				// this project even when it was recorded for another one.
				let in_this_project = entry
					.project_path
					.and_then(|path| fs::canonicalize(path).ok())
					.is_some_and(|path| path == project_root);
				if project_bound && override_enabled != Some(true) && !in_this_project {
					continue;
				}
				candidates.push(Candidate {
					install_path,
					version: entry.version.unwrap_or_else(|| Str::new_static("unknown")),
					scope: if project_bound {
						PluginScope::Project
					} else {
						PluginScope::User
					},
				});
			}
			installs.insert(id, candidates);
		}
		Some((registry_path, installs))
	}

	/// Claude Code's `enabledPlugins`, merged in its own layer order: the
	/// user `settings.json`, then the project's `.claude/settings.json` and
	/// `.claude/settings.local.json`; later layers win.
	fn claude_enabled_overrides(
		&mut self,
		home: &ClaudeCodeHome,
		project_root: &Path,
	) -> BTreeMap<Str, bool> {
		let mut overrides = BTreeMap::new();
		let project = CLAUDE_SETTINGS_FILES
			.iter()
			.map(|settings| project_root.join(settings));
		for path in iter::once(home.config_dir().join("settings.json")).chain(project) {
			let Ok(bytes) = fs::read(&path) else {
				continue;
			};
			match enabled_plugin_overrides(&bytes) {
				Ok(declared) => overrides.extend(declared),
				Err(source) => self
					.diagnostics
					.push(PluginDiagnostic::ClaudeSettings { path, source }),
			}
		}
		overrides
	}

	/// Admits one id's candidate installs from one registry.
	fn admit(
		&mut self,
		registry: &Path,
		id: Str,
		source: PluginSource,
		candidates: impl Iterator<Item = Candidate>,
		seen_roots: &mut BTreeSet<PathBuf>,
	) {
		let Some((name, marketplace)) =
			split_plugin_id(&id).map(|(name, marketplace)| (Str::new(name), Str::new(marketplace)))
		else {
			self
				.diagnostics
				.push(PluginDiagnostic::InvalidId { registry: registry.to_owned(), id });
			return;
		};
		for candidate in candidates {
			let Some(root) = open_root(&id, &candidate.install_path, &mut self.diagnostics) else {
				continue;
			};
			if !seen_roots.insert(root.clone()) {
				continue;
			}
			let Some(layout) = resolve_layout(&id, &root, &mut self.diagnostics) else {
				continue;
			};
			self.plugins.push(ClaudePlugin {
				id: PluginId::from(id.clone()),
				name: name.clone(),
				marketplace: marketplace.clone(),
				version: candidate.version,
				scope: candidate.scope,
				source,
				root,
				layout,
				approved_commands: Arc::default(),
			});
		}
	}
}

/// One install a registry offers for resolution.
struct Candidate {
	install_path: PathBuf,
	version:      Str,
	scope:        PluginScope,
}

impl Candidate {
	fn omp(entry: InstalledPluginEntry, scope: PluginScope) -> Self {
		Self { install_path: entry.install_path, version: entry.version, scope }
	}
}

/// Claude Code's `installed_plugins.json`, read leniently: any schema
/// version, optional fields.
#[derive(Deserialize)]
struct ClaudeCodeRegistryWire {
	#[serde(default)]
	plugins: BTreeMap<Str, Vec<ClaudeCodeEntryWire>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ClaudeCodeEntryWire {
	#[serde(default)]
	scope:        Option<Str>,
	#[serde(default)]
	install_path: Option<PathBuf>,
	#[serde(default)]
	version:      Option<Str>,
	#[serde(default)]
	enabled:      Option<bool>,
	#[serde(default)]
	project_path: Option<PathBuf>,
}

/// The one Claude Code settings key plugin resolution reads.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ClaudeSettingsWire {
	#[serde(default)]
	enabled_plugins: Option<EnabledPluginsWire>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum EnabledPluginsWire {
	Map(BTreeMap<Str, EnabledValueWire>),
	Other(IgnoredAny),
}

#[derive(Deserialize)]
#[serde(untagged)]
enum EnabledValueWire {
	Bool(bool),
	Other(IgnoredAny),
}

/// The `enabledPlugins` overrides one Claude Code settings file declares:
/// every plugin id with a boolean value. Another value, another shape, or no
/// `enabledPlugins` key declares nothing.
pub(crate) fn enabled_plugin_overrides(
	bytes: &[u8],
) -> Result<BTreeMap<Str, bool>, serde_json::Error> {
	let ClaudeSettingsWire { enabled_plugins } = serde_json::from_slice(bytes)?;
	let Some(EnabledPluginsWire::Map(map)) = enabled_plugins else {
		return Ok(BTreeMap::new());
	};
	Ok(map
		.into_iter()
		.filter_map(|(id, value)| match value {
			EnabledValueWire::Bool(enabled) => Some((id, enabled)),
			EnabledValueWire::Other(_) => None,
		})
		.collect())
}

/// The `(name, marketplace)` halves of a registry id `name@marketplace`;
/// `None` when either is empty, so the id never resolves.
pub(crate) fn split_plugin_id(id: &str) -> Option<(&str, &str)> {
	id.rsplit_once('@')
		.filter(|(name, marketplace)| !name.is_empty() && !marketplace.is_empty())
}

fn open_root(
	id: &Str,
	install_path: &Path,
	diagnostics: &mut Vec<PluginDiagnostic>,
) -> Option<PathBuf> {
	match fs::canonicalize(install_path) {
		Ok(root) if root.is_dir() => Some(root),
		Ok(_) => {
			diagnostics.push(PluginDiagnostic::InstallNotDirectory {
				plugin: id.clone(),
				path:   install_path.to_owned(),
			});
			None
		},
		Err(source) => {
			diagnostics.push(PluginDiagnostic::InstallMissing {
				plugin: id.clone(),
				path: install_path.to_owned(),
				source,
			});
			None
		},
	}
}

/// `.claude-plugin/plugin.json` (or `.omp-plugin/plugin.json`).
#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ManifestWire {
	#[serde(default)]
	skills:        Option<PathList>,
	#[serde(default, alias = "slash-commands")]
	commands:      Option<PathList>,
	#[serde(default)]
	agents:        Option<IgnoredAny>,
	#[serde(default)]
	hooks:         Option<Box<RawValue>>,
	#[serde(default)]
	mcp_servers:   Option<Box<RawValue>>,
	#[serde(default)]
	output_styles: Option<IgnoredAny>,
	#[serde(default)]
	lsp_servers:   Option<Box<RawValue>>,
}

/// A manifest path field: one path or several.
#[derive(Deserialize)]
#[serde(untagged)]
enum PathList {
	One(PathBuf),
	Many(Vec<PathBuf>),
}

impl PathList {
	fn into_vec(self) -> Vec<PathBuf> {
		match self {
			Self::One(path) => vec![path],
			Self::Many(paths) => paths,
		}
	}
}

#[derive(Deserialize)]
struct AgentPluginHeader {
	#[serde(rename = "$schema")]
	schema: Str,
}

fn is_agent_plugin(root: &Path) -> bool {
	let Ok(body) = fs::read(root.join("plugin.json")) else {
		return false;
	};
	serde_json::from_slice::<AgentPluginHeader>(&body)
		.is_ok_and(|header| header.schema == AGENT_PLUGIN_SCHEMA)
}

/// Reads the manifest and resolves every component; `None` when the plugin
/// cannot load at all (its diagnostic is already recorded).
fn resolve_layout(
	id: &Str,
	root: &Path,
	diagnostics: &mut Vec<PluginDiagnostic>,
) -> Option<PluginLayout> {
	if is_agent_plugin(root) {
		return Some(PluginLayout::AgentPlugins);
	}
	let (manifest_path, manifest) = read_manifest(id, root, diagnostics)?;
	let mut resolver = ComponentResolver { id, root, diagnostics };
	let mut components = ClaudeComponents::default();

	// Skills: the conventional directory plus any manifest-declared ones.
	let mut skills = resolver
		.existing_dir("skills")
		.into_iter()
		.collect::<Vec<_>>();
	for path in manifest.skills.map(PathList::into_vec).unwrap_or_default() {
		if let Some(dir) = resolver.declared(PluginComponent::Skills, &path)
			&& !skills.contains(&dir)
		{
			skills.push(dir);
		}
	}
	components.skills = skills.into_boxed_slice();

	// Commands: manifest paths replace the conventional directory.
	components.commands = match manifest.commands {
		Some(paths) => paths
			.into_vec()
			.iter()
			.filter_map(|path| resolver.declared(PluginComponent::Commands, path))
			.collect(),
		None => resolver.existing_dir("commands").into_iter().collect(),
	};

	components.rules = resolver.existing_dir("rules");

	// MCP: manifest `mcpServers` (inline map or file paths) replaces `.mcp.json`.
	let manifest_file = manifest_path.as_deref().unwrap_or(root);
	components.mcp = match manifest.mcp_servers {
		Some(raw) => resolver
			.manifest_servers(PluginComponent::McpServers, &raw, manifest_file)?
			.into(),
		None => resolver
			.existing_file(".mcp.json")
			.map(ConfigDeclaration::File)
			.into_iter()
			.collect(),
	};

	// LSP: the root declaration files, then manifest `lspServers` on top (OMP
	// v1 loaded every root file; Claude's manifest key supplements them).
	let mut lsp = resolver.existing_files(&LSP_CONFIG_NAMES);
	if let Some(raw) = manifest.lsp_servers {
		for declaration in
			resolver.manifest_servers(PluginComponent::LspServers, &raw, manifest_file)?
		{
			if !lsp.contains(&declaration) {
				lsp.push(declaration);
			}
		}
	}
	components.lsp = lsp.into_boxed_slice();
	components.dap = resolver
		.existing_files(&DAP_CONFIG_NAMES)
		.into_boxed_slice();

	// Hooks: `hooks/hooks.json`, then whatever the manifest declares merges in
	// (Claude's plugin reference: `hooks` merges with the default file).
	let mut hook_declarations = resolver
		.existing_file("hooks/hooks.json")
		.map(ConfigDeclaration::File)
		.into_iter()
		.collect::<Vec<_>>();
	if let Some(raw) = manifest.hooks {
		for declaration in resolver.manifest_hooks(&raw, manifest_file)? {
			if !hook_declarations.contains(&declaration) {
				hook_declarations.push(declaration);
			}
		}
	}
	components.hooks =
		crate::claude_hooks::load_hooks(id, &hook_declarations, resolver.diagnostics).into();

	// Components without a runtime home: surfaced, never silently dropped.
	let manifest_marker = manifest_path.unwrap_or_else(|| root.to_path_buf());
	let unsupported = [
		(PluginComponent::Hooks, false, &["hooks/pre", "hooks/post"][..]),
		(PluginComponent::Agents, manifest.agents.is_some(), &["agents"][..]),
		(PluginComponent::OutputStyles, manifest.output_styles.is_some(), &["output-styles"][..]),
		(PluginComponent::Tools, false, &["tools"][..]),
	];
	for (component, declared, conventional) in unsupported {
		let found = if declared {
			Some(manifest_marker.clone())
		} else {
			conventional
				.iter()
				.map(|name| root.join(name))
				.find(|path| path.exists())
		};
		if let Some(path) = found {
			resolver.diagnostics.push(PluginDiagnostic::Unsupported {
				plugin: id.clone(),
				component,
				path,
			});
		}
	}
	Some(PluginLayout::Claude(components))
}

fn read_manifest(
	id: &Str,
	root: &Path,
	diagnostics: &mut Vec<PluginDiagnostic>,
) -> Option<(Option<PathBuf>, ManifestWire)> {
	for dir in [".omp-plugin", ".claude-plugin"] {
		let path = root.join(dir).join("plugin.json");
		let bytes = match fs::read(&path) {
			Ok(bytes) => bytes,
			Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
			Err(source) => {
				diagnostics.push(PluginDiagnostic::ManifestRead { plugin: id.clone(), path, source });
				return None;
			},
		};
		return match serde_json::from_slice::<ManifestWire>(&bytes) {
			Ok(manifest) => Some((Some(path), manifest)),
			Err(source) => {
				diagnostics.push(PluginDiagnostic::ManifestParse { plugin: id.clone(), path, source });
				None
			},
		};
	}
	Some((None, ManifestWire::default()))
}

struct ComponentResolver<'a> {
	id:          &'a Str,
	root:        &'a Path,
	diagnostics: &'a mut Vec<PluginDiagnostic>,
}

impl ComponentResolver<'_> {
	/// A conventional directory, when present and contained.
	fn existing_dir(&self, name: &str) -> Option<PathBuf> {
		fs::canonicalize(self.root.join(name))
			.ok()
			.filter(|path| path.starts_with(self.root) && path.is_dir())
	}

	/// A conventional file, when present and contained.
	fn existing_file(&self, name: &str) -> Option<PathBuf> {
		fs::canonicalize(self.root.join(name))
			.ok()
			.filter(|path| path.starts_with(self.root) && path.is_file())
	}

	/// Every present, contained conventional file of `names`, in order.
	fn existing_files(&self, names: &[&str]) -> Vec<ConfigDeclaration> {
		let mut files = Vec::new();
		for path in names.iter().filter_map(|name| self.existing_file(name)) {
			let declaration = ConfigDeclaration::File(path);
			if !files.contains(&declaration) {
				files.push(declaration);
			}
		}
		files
	}

	/// A manifest server-map field: an inline object, or one or several
	/// contained file paths. `None` when the field has neither shape (the
	/// manifest diagnostic is recorded and the plugin does not load).
	fn manifest_servers(
		&mut self,
		component: PluginComponent,
		raw: &RawValue,
		manifest: &Path,
	) -> Option<Vec<ConfigDeclaration>> {
		if raw.get().trim_start().starts_with('{') {
			return Some(vec![ConfigDeclaration::Inline {
				manifest: manifest.to_path_buf(),
				servers:  Str::new(raw.get()),
			}]);
		}
		match serde_json::from_str::<PathList>(raw.get()) {
			Ok(paths) => Some(
				paths
					.into_vec()
					.iter()
					.filter_map(|path| self.declared(component, path))
					.map(ConfigDeclaration::File)
					.collect(),
			),
			Err(source) => {
				self.diagnostics.push(PluginDiagnostic::ManifestParse {
					plugin: self.id.clone(),
					path: manifest.to_path_buf(),
					source,
				});
				None
			},
		}
	}

	/// Manifest `hooks`: a file path, an inline event map, or an array mixing
	/// both. `None` when it has none of those shapes (the manifest diagnostic
	/// is recorded and the plugin does not load).
	fn manifest_hooks(&mut self, raw: &RawValue, manifest: &Path) -> Option<Vec<ConfigDeclaration>> {
		let entries = if raw.get().trim_start().starts_with('[') {
			serde_json::from_str::<Vec<Box<RawValue>>>(raw.get())
		} else {
			serde_json::from_str::<Box<RawValue>>(raw.get()).map(|entry| vec![entry])
		};
		let entries = match entries {
			Ok(entries) => entries,
			Err(source) => {
				return self.manifest_error(manifest, source);
			},
		};
		let mut declarations = Vec::new();
		for entry in entries {
			if entry.get().trim_start().starts_with('{') {
				declarations.push(ConfigDeclaration::Inline {
					manifest: manifest.to_path_buf(),
					servers:  Str::new(entry.get()),
				});
				continue;
			}
			match serde_json::from_str::<PathBuf>(entry.get()) {
				Ok(path) => declarations.extend(
					self
						.declared(PluginComponent::Hooks, &path)
						.map(ConfigDeclaration::File),
				),
				Err(source) => {
					return self.manifest_error(manifest, source);
				},
			}
		}
		Some(declarations)
	}

	/// Records a malformed manifest field; the plugin does not load.
	fn manifest_error<T>(&mut self, manifest: &Path, source: serde_json::Error) -> Option<T> {
		self.diagnostics.push(PluginDiagnostic::ManifestParse {
			plugin: self.id.clone(),
			path: manifest.to_path_buf(),
			source,
		});
		None
	}

	/// A manifest-declared path, resolved against the root and contained.
	fn declared(&mut self, component: PluginComponent, path: &Path) -> Option<PathBuf> {
		let joined = self.root.join(path);
		match fs::canonicalize(&joined) {
			Ok(resolved) if resolved.starts_with(self.root) => Some(resolved),
			Ok(_) => {
				self
					.diagnostics
					.push(PluginDiagnostic::ComponentOutsideRoot {
						plugin: self.id.clone(),
						component,
						path: path.to_owned(),
					});
				None
			},
			Err(_) => {
				self.diagnostics.push(PluginDiagnostic::ComponentMissing {
					plugin: self.id.clone(),
					component,
					path: joined,
				});
				None
			},
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn write(path: &Path, body: &str) {
		fs::create_dir_all(path.parent().unwrap()).unwrap();
		fs::write(path, body).unwrap();
	}

	fn registry(entries: &[(&str, &Path, bool)]) -> String {
		let mut registry = InstalledPluginsRegistry::default();
		for (id, path, enabled) in entries {
			registry
				.plugins
				.insert(Str::new(id), vec![InstalledPluginEntry {
					scope:          InstallScope::User,
					install_path:   path.to_path_buf(),
					version:        Str::new_static("1.0.0"),
					installed_at:   Str::new_static("2026-01-01T00:00:00Z"),
					last_updated:   Str::new_static("2026-01-01T00:00:00Z"),
					git_commit_sha: None,
					enabled:        *enabled,
				}]);
		}
		serde_json::to_string(&registry).unwrap()
	}

	#[test]
	fn enabled_installs_resolve_and_disabled_ones_never_load() {
		let temp = tempfile::tempdir().unwrap();
		let data = temp.path().join("data");
		let project = temp.path().join("project");
		fs::create_dir_all(&project).unwrap();
		let on = data.join("plugins/cache/plugins/m___on___1");
		let off = data.join("plugins/cache/plugins/m___off___1");
		write(&on.join("skills/review/SKILL.md"), "---\nname: review\n---\n");
		write(&on.join("commands/fix.md"), "Fix");
		write(&on.join(".mcp.json"), r#"{"mcpServers":{}}"#);
		write(&off.join("skills/other/SKILL.md"), "x");
		write(
			&data.join("plugins/installed_plugins.json"),
			&registry(&[("on@m", &on, true), ("off@m", &off, false)]),
		);

		let resolved = ClaudePlugins::resolve(&data, &project, None);

		assert!(resolved.diagnostics.is_empty(), "{:?}", resolved.diagnostics);
		assert_eq!(resolved.plugins.len(), 1);
		let plugin = &resolved.plugins[0];
		assert_eq!((plugin.name.as_str(), plugin.marketplace.as_str()), ("on", "m"));
		assert_eq!(plugin.scope, PluginScope::User);
		let root = fs::canonicalize(&on).unwrap();
		let components = plugin.claude_components().unwrap();
		assert_eq!(&*components.skills, [root.join("skills")]);
		assert_eq!(&*components.commands, [root.join("commands")]);
		assert_eq!(&*components.mcp, [ConfigDeclaration::File(root.join(".mcp.json"))]);
	}

	#[test]
	fn resolution_attaches_each_plugins_approved_launches_and_gates_the_rest() {
		use crate::{
			plugin_command::{PluginLaunch, PluginLaunchKind},
			trust::PluginCommandGrant,
		};

		let temp = tempfile::tempdir().unwrap();
		let data = temp.path().join("data");
		let project = temp.path().join("project");
		fs::create_dir_all(&project).unwrap();
		let one = data.join("plugins/cache/plugins/m___one___1");
		let two = data.join("plugins/cache/plugins/m___two___1");
		write(&one.join(".lsp.json"), "{}");
		write(&two.join(".lsp.json"), "{}");
		write(
			&data.join("plugins/installed_plugins.json"),
			&registry(&[("one@m", &one, true), ("two@m", &two, true)]),
		);
		let launch = |args: &[&'static str]| {
			PluginLaunch::new(
				PluginLaunchKind::LanguageServer,
				Str::new_static("srv"),
				Str::new_static("srv-bin"),
				args.iter().map(|arg| Str::new_static(arg)),
				[],
			)
		};
		let approved = launch(&["--stdio"]);
		let digest = plugin_command_digest(PluginId::from_ref("one@m"), "1.0.0", &approved);
		crate::trust::GrantsFile::persist_plugin_command(&grants_path(&data), PluginCommandGrant {
			plugin: PluginId::new_static("one@m"),
			version: Str::new_static("1.0.0"),
			kind: approved.kind,
			server: approved.server.clone(),
			command: approved.command.clone(),
			args: approved.args.to_vec(),
			digest,
			granted_at: Str::new_static("2026-09-27T00:00:00Z"),
			granted_by: Str::new_static("cli"),
		})
		.unwrap();

		let resolved = ClaudePlugins::resolve(&data, &project, None);
		assert!(resolved.diagnostics.is_empty(), "{:?}", resolved.diagnostics);
		let [first, second] = &resolved.plugins[..] else {
			panic!("two plugins: {:?}", resolved.plugins);
		};
		assert_eq!(first.id, "one@m");
		assert_eq!(&*first.approved_commands, [digest]);
		assert!(first.admit_launch(approved.clone()).is_ok());
		let changed = first
			.admit_launch(launch(&["--stdio", "--log"]))
			.expect_err("changed arguments need a new approval");
		assert_eq!(changed.plugin, "one@m");
		assert_eq!(changed.digest, first.command_digest(&launch(&["--stdio", "--log"])));
		assert!(second.approved_commands.is_empty());
		let other = second
			.admit_launch(approved)
			.expect_err("an approval never crosses plugins");
		assert_eq!(other.plugin, "two@m");
	}

	#[test]
	fn unreadable_approvals_fail_closed_with_a_diagnostic() {
		let temp = tempfile::tempdir().unwrap();
		let data = temp.path().join("data");
		let project = temp.path().join("project");
		fs::create_dir_all(&project).unwrap();
		let one = data.join("plugins/cache/plugins/m___one___1");
		write(&one.join(".lsp.json"), "{}");
		write(&data.join("plugins/installed_plugins.json"), &registry(&[("one@m", &one, true)]));
		write(&grants_path(&data), "plugin_command = 3");

		let resolved = ClaudePlugins::resolve(&data, &project, None);
		assert!(
			matches!(&resolved.diagnostics[..], [PluginDiagnostic::CommandApprovals(_)]),
			"{:?}",
			resolved.diagnostics
		);
		assert_eq!(resolved.plugins.len(), 1);
		assert!(resolved.plugins[0].approved_commands.is_empty());
	}

	#[test]
	fn missing_installs_and_malformed_manifests_are_diagnostics() {
		let temp = tempfile::tempdir().unwrap();
		let data = temp.path().join("data");
		let project = temp.path().join("project");
		let broken = temp.path().join("broken");
		write(&broken.join(".claude-plugin/plugin.json"), "{ not json");
		write(&broken.join("skills/a/SKILL.md"), "x");
		write(
			&data.join("plugins/installed_plugins.json"),
			&registry(&[
				("gone@m", &temp.path().join("gone"), true),
				("broken@m", &broken, true),
				("noid", &broken, true),
			]),
		);

		let resolved = ClaudePlugins::resolve(&data, &project, None);

		assert!(resolved.plugins.is_empty(), "{:?}", resolved.plugins);
		let kinds = resolved
			.diagnostics
			.iter()
			.map(|diagnostic| match diagnostic {
				PluginDiagnostic::InstallMissing { plugin, .. } => tag("missing", plugin),
				PluginDiagnostic::ManifestParse { plugin, .. } => tag("manifest", plugin),
				PluginDiagnostic::InvalidId { id, .. } => tag("id", id),
				other => panic!("unexpected {other:?}"),
			})
			.collect::<BTreeSet<_>>();
		assert_eq!(
			kinds,
			BTreeSet::from([
				"id:noid".to_owned(),
				"manifest:broken@m".to_owned(),
				"missing:gone@m".to_owned(),
			])
		);

		write(&data.join("plugins/installed_plugins.json"), "{ nope");
		let resolved = ClaudePlugins::resolve(&data, &project, None);
		assert!(matches!(resolved.diagnostics.as_slice(), [PluginDiagnostic::Registry(
			RegistryError::Parse { .. }
		)]));
	}

	fn tag(kind: &str, id: &Str) -> String {
		format!("{kind}:{id}")
	}

	#[test]
	fn project_installs_shadow_user_installs_and_unsupported_components_surface() {
		let temp = tempfile::tempdir().unwrap();
		let data = temp.path().join("data");
		let project = temp.path().join("project");
		let user_copy = temp.path().join("user-copy");
		let project_copy = temp.path().join("project-copy");
		write(&user_copy.join("skills/a/SKILL.md"), "x");
		write(
			&project_copy.join(".claude-plugin/plugin.json"),
			r#"{"name":"p","skills":"./extra","mcpServers":{"srv":{"command":"x"}},"hooks":"./hooks/hooks.json"}"#,
		);
		write(&project_copy.join("extra/b/SKILL.md"), "x");
		write(&project_copy.join("agents/helper.md"), "x");
		write(&project_copy.join("hooks/hooks.json"), "{}");
		// OMP v1's JS/TS hook factories have no runtime; `hooks.json` does.
		write(&project_copy.join("hooks/pre/guard.ts"), "x");
		write(&data.join("plugins/installed_plugins.json"), &registry(&[("p@m", &user_copy, true)]));
		write(
			&project.join(".omp/plugins/installed_plugins.json"),
			&registry(&[("p@m", &project_copy, true)]),
		);

		let resolved = ClaudePlugins::resolve(&data, &project, None);

		assert_eq!(resolved.plugins.len(), 1);
		let plugin = &resolved.plugins[0];
		assert_eq!(plugin.scope, PluginScope::Project);
		let root = fs::canonicalize(&project_copy).unwrap();
		let components = plugin.claude_components().unwrap();
		assert_eq!(&*components.skills, [root.join("extra")]);
		assert!(matches!(
			&*components.mcp,
			[ConfigDeclaration::Inline { servers, .. }] if servers.contains("\"srv\"")
		));
		let unsupported = resolved
			.diagnostics
			.iter()
			.map(|diagnostic| match diagnostic {
				PluginDiagnostic::Unsupported { component, .. } => *component,
				other => panic!("unexpected {other:?}"),
			})
			.collect::<Vec<_>>();
		assert_eq!(unsupported, [PluginComponent::Hooks, PluginComponent::Agents]);

		// A disabled project entry neither loads nor shadows the user install.
		write(
			&project.join(".omp/plugins/installed_plugins.json"),
			&registry(&[("p@m", &project_copy, false)]),
		);
		let resolved = ClaudePlugins::resolve(&data, &project, None);
		assert_eq!(resolved.plugins.len(), 1);
		assert_eq!(resolved.plugins[0].scope, PluginScope::User);
	}

	#[test]
	fn lsp_and_dap_declarations_resolve_as_components_not_unsupported() {
		let temp = tempfile::tempdir().unwrap();
		let data = temp.path().join("data");
		let inline = temp.path().join("inline");
		let by_path = temp.path().join("by-path");
		write(&inline.join(".lsp.json"), r#"{"go":{"command":"gopls"}}"#);
		write(&inline.join("lsp.yaml"), "servers: {}\n");
		write(&inline.join(".dap.yaml"), "adapters: {}\n");
		write(
			&inline.join(".claude-plugin/plugin.json"),
			r#"{"name":"inline","lspServers":{"zig":{"command":"zls"}}}"#,
		);
		write(&by_path.join("config/servers.json"), "{}");
		write(&by_path.join("dap.json"), "{}");
		write(
			&by_path.join(".claude-plugin/plugin.json"),
			r#"{"name":"by-path","lspServers":"./config/servers.json"}"#,
		);
		write(
			&data.join("plugins/installed_plugins.json"),
			&registry(&[("inline@m", &inline, true), ("by-path@m", &by_path, true)]),
		);

		let resolved = ClaudePlugins::resolve(&data, temp.path(), None);

		assert!(resolved.diagnostics.is_empty(), "{:?}", resolved.diagnostics);
		let components = |id: &str| {
			resolved
				.plugins
				.iter()
				.find(|plugin| plugin.id == id)
				.and_then(ClaudePlugin::claude_components)
				.unwrap()
				.clone()
		};
		let root = fs::canonicalize(&inline).unwrap();
		let inline_components = components("inline@m");
		assert_eq!(&inline_components.lsp[..2], [
			ConfigDeclaration::File(root.join(".lsp.json")),
			ConfigDeclaration::File(root.join("lsp.yaml")),
		]);
		assert!(matches!(
			&inline_components.lsp[2],
			ConfigDeclaration::Inline { manifest, servers }
				if manifest.ends_with(".claude-plugin/plugin.json") && servers.contains("\"zls\"")
		));
		assert_eq!(&*inline_components.dap, [ConfigDeclaration::File(root.join(".dap.yaml"))]);
		let root = fs::canonicalize(&by_path).unwrap();
		let by_path_components = components("by-path@m");
		assert_eq!(&*by_path_components.lsp, [ConfigDeclaration::File(
			root.join("config/servers.json")
		)]);
		assert_eq!(&*by_path_components.dap, [ConfigDeclaration::File(root.join("dap.json"))]);
	}

	#[test]
	fn plugin_vars_expand_and_relative_commands_root_at_the_base() {
		let root = Path::new("/plugins/acme");
		assert_eq!(
			expand_plugin_vars(Str::new_static("${CLAUDE_PLUGIN_ROOT}/bin/x"), root, None),
			"/plugins/acme/bin/x"
		);
		assert_eq!(expand_plugin_vars(Str::new_static("plain"), root, None), "plain");
		assert_eq!(resolve_plugin_command(Str::new_static("./bin/x"), root), "/plugins/acme/bin/x");
		assert_eq!(resolve_plugin_command(Str::new_static("gopls"), root), "gopls");
	}

	#[cfg(unix)]
	#[test]
	fn declared_paths_escaping_the_root_are_rejected() {
		let temp = tempfile::tempdir().unwrap();
		let data = temp.path().join("data");
		let plugin = temp.path().join("plugin");
		write(&temp.path().join("outside/x/SKILL.md"), "x");
		write(&plugin.join(".claude-plugin/plugin.json"), r#"{"skills":["../outside"]}"#);
		std::os::unix::fs::symlink(temp.path().join("outside"), plugin.join("skills")).unwrap();
		write(&data.join("plugins/installed_plugins.json"), &registry(&[("p@m", &plugin, true)]));

		let resolved = ClaudePlugins::resolve(&data, temp.path(), None);

		let components = resolved.plugins[0].claude_components().unwrap();
		assert!(components.skills.is_empty(), "{:?}", components.skills);
		assert!(matches!(resolved.diagnostics.as_slice(), [
			PluginDiagnostic::ComponentOutsideRoot { component: PluginComponent::Skills, .. }
		]));
	}

	#[test]
	fn hooks_json_and_manifest_hooks_merge_into_components() {
		let temp = tempfile::tempdir().unwrap();
		let data = temp.path().join("data");
		let plugin = temp.path().join("plugin");
		write(
			&plugin.join("hooks/hooks.json"),
			r#"{"hooks":{"PreToolUse":[{"matcher":"Bash","hooks":[{"type":"command","command":"a"}]}]}}"#,
		);
		write(
			&plugin.join("config/extra.json"),
			r#"{"hooks":{"Stop":[{"hooks":[{"type":"command","command":"b"}]}]}}"#,
		);
		write(
			&plugin.join(".claude-plugin/plugin.json"),
			r#"{"hooks":["./config/extra.json",{"PostToolUse":[{"hooks":[{"type":"command","command":"c"}]}]}]}"#,
		);
		write(&data.join("plugins/installed_plugins.json"), &registry(&[("p@m", &plugin, true)]));

		let resolved = ClaudePlugins::resolve(&data, temp.path(), None);

		assert!(resolved.diagnostics.is_empty(), "{:?}", resolved.diagnostics);
		let hooks = &resolved.plugins[0].claude_components().unwrap().hooks;
		let commands = hooks
			.iter()
			.map(|hook| (hook.event, hook.command.command.as_str()))
			.collect::<Vec<_>>();
		assert_eq!(commands, [
			(ClaudeHookEvent::PreToolUse, "a"),
			(ClaudeHookEvent::Stop, "b"),
			(ClaudeHookEvent::PostToolUse, "c"),
		]);
	}

	/// A fake Claude Code home: its registry plus user settings.
	struct ClaudeFixture {
		temp:    tempfile::TempDir,
		data:    PathBuf,
		project: PathBuf,
		home:    ClaudeCodeHome,
	}

	impl ClaudeFixture {
		fn new() -> Self {
			let temp = tempfile::tempdir().unwrap();
			let data = temp.path().join("data");
			let project = temp.path().join("project");
			fs::create_dir_all(&project).unwrap();
			let home = ClaudeCodeHome::at(temp.path().join("home/.claude"));
			Self { temp, data, project, home }
		}

		/// A Claude-installed plugin root with one skill.
		fn plugin(&self, name: &str) -> PathBuf {
			let root = self
				.home
				.config_dir()
				.join("plugins/cache/m")
				.join(name)
				.join("1.0.0");
			write(&root.join("skills/s/SKILL.md"), "x");
			root
		}

		fn claude_registry(&self, body: &str) {
			write(&self.home.registry(), body);
		}

		fn resolve(&self) -> ClaudePlugins {
			ClaudePlugins::resolve(&self.data, &self.project, Some(&self.home))
		}
	}

	fn claude_entry(scope: &str, root: &Path, project: Option<&Path>) -> String {
		let project = project.map_or_else(String::new, |path| {
			format!(r#","projectPath":{}"#, serde_json::to_string(path).unwrap())
		});
		format!(
			r#"{{"scope":"{scope}","installPath":{},"version":"1.0.0","installedAt":"t","lastUpdated":"t"{project}}}"#,
			serde_json::to_string(root).unwrap()
		)
	}

	#[test]
	fn claude_code_installs_load_read_only_and_marked_as_claude_code() {
		let fixture = ClaudeFixture::new();
		let root = fixture.plugin("cc");
		fixture.claude_registry(&format!(
			r#"{{"version":2,"plugins":{{"cc@m":[{}]}}}}"#,
			claude_entry("user", &root, None)
		));
		let snapshot = |dir: &Path| {
			let mut files = BTreeMap::new();
			let mut stack = vec![dir.to_path_buf()];
			while let Some(dir) = stack.pop() {
				for entry in fs::read_dir(&dir).unwrap() {
					let path = entry.unwrap().path();
					if path.is_dir() {
						stack.push(path);
					} else {
						files.insert(path.clone(), fs::read(&path).unwrap());
					}
				}
			}
			files
		};
		let before = snapshot(fixture.home.config_dir());

		let resolved = fixture.resolve();

		assert!(resolved.diagnostics.is_empty(), "{:?}", resolved.diagnostics);
		assert_eq!(resolved.plugins.len(), 1);
		let plugin = &resolved.plugins[0];
		assert_eq!(plugin.id, "cc@m");
		assert_eq!(plugin.source, PluginSource::ClaudeCode);
		assert_eq!(plugin.scope, PluginScope::User);
		assert_eq!(plugin.root, fs::canonicalize(&root).unwrap());
		assert_eq!(snapshot(fixture.home.config_dir()), before, "~/.claude stays byte-identical");
		// Without a Claude Code home nothing of Claude's loads.
		assert!(
			ClaudePlugins::resolve(&fixture.data, &fixture.project, None)
				.plugins
				.is_empty()
		);
	}

	#[test]
	fn an_omp_install_of_the_same_id_wins_over_claude_code() {
		let fixture = ClaudeFixture::new();
		let claude_root = fixture.plugin("dup");
		let omp_root = fixture.temp.path().join("omp-copy");
		write(&omp_root.join("skills/o/SKILL.md"), "x");
		fixture.claude_registry(&format!(
			r#"{{"version":2,"plugins":{{"dup@m":[{}]}}}}"#,
			claude_entry("user", &claude_root, None)
		));
		write(
			&fixture.data.join("plugins/installed_plugins.json"),
			&registry(&[("dup@m", &omp_root, true)]),
		);

		let resolved = fixture.resolve();

		assert_eq!(resolved.plugins.len(), 1, "{:?}", resolved.plugins);
		assert_eq!(resolved.plugins[0].source, PluginSource::Omp);
		assert_eq!(resolved.plugins[0].root, fs::canonicalize(&omp_root).unwrap());
	}

	#[test]
	fn enabled_plugins_false_disables_a_claude_code_install() {
		let fixture = ClaudeFixture::new();
		let root = fixture.plugin("off");
		fixture.claude_registry(&format!(
			r#"{{"version":2,"plugins":{{"off@m":[{}]}}}}"#,
			claude_entry("user", &root, None)
		));
		write(
			&fixture.home.config_dir().join("settings.json"),
			r#"{"theme":"dark","enabledPlugins":{"off@m":false,"other@m":"yes"}}"#,
		);
		let resolved = fixture.resolve();
		assert!(resolved.plugins.is_empty(), "{:?}", resolved.plugins);
		assert!(resolved.diagnostics.is_empty(), "{:?}", resolved.diagnostics);

		// The project's local settings are a later layer and win.
		write(
			&fixture.project.join(".claude/settings.local.json"),
			r#"{"enabledPlugins":{"off@m":true}}"#,
		);
		assert_eq!(fixture.resolve().plugins.len(), 1);
	}

	#[test]
	fn project_scope_claude_entries_bind_to_their_project_path() {
		let fixture = ClaudeFixture::new();
		let here = fixture.plugin("here");
		let there = fixture.plugin("there");
		let elsewhere = fixture.temp.path().join("elsewhere");
		fs::create_dir_all(&elsewhere).unwrap();
		fixture.claude_registry(&format!(
			r#"{{"version":2,"plugins":{{"here@m":[{}],"there@m":[{}]}}}}"#,
			claude_entry("project", &here, Some(&fixture.project)),
			claude_entry("local", &there, Some(&elsewhere)),
		));

		let resolved = fixture.resolve();

		let ids = resolved
			.plugins
			.iter()
			.map(|plugin| (plugin.id.as_str(), plugin.scope))
			.collect::<Vec<_>>();
		assert_eq!(ids, [("here@m", PluginScope::Project)]);

		// `enabledPlugins: true` opts the other project's install in.
		write(
			&fixture.project.join(".claude/settings.json"),
			r#"{"enabledPlugins":{"there@m":true}}"#,
		);
		assert_eq!(fixture.resolve().plugins.len(), 2);
	}
}
