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
//! * language servers: the [`LSP_CONFIG_FILES`] at the root plus manifest
//!   `lspServers` (an inline server map or contained file paths); debug
//!   adapters: the [`DAP_CONFIG_FILES`] at the root. This module only locates
//!   them; the document authority validates each declaration with its own LSP
//!   and DAP parsers and reports a rejected one as
//!   [`PluginDiagnostic::InvalidComponent`];
//! * `agents/`, `hooks/hooks.json`, `output-styles/`, `tools/` have no runtime
//!   home yet and surface as [`PluginDiagnostic::Unsupported`] instead of being
//!   dropped silently.
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

use std::{
	collections::{BTreeMap, BTreeSet},
	fs, io,
	path::{Path, PathBuf},
};

use omp_core::Str;
use serde::{Deserialize, Serialize, de::IgnoredAny};
use serde_json::value::RawValue;
use strum::{Display, IntoStaticStr};

/// Registry file name inside a scope's plugin directory.
pub const REGISTRY_FILE: &str = "installed_plugins.json";

/// Registry schema version `omp ext` reads and writes.
pub const INSTALLED_PLUGINS_VERSION: u32 = 2;

/// Agent Plugins 1.0 root-manifest schema.
pub const AGENT_PLUGIN_SCHEMA: &str = "https://agent-plugins.org/schemas/1.0.0/plugin.schema.json";

/// Language-server declaration files a plugin root may carry, lowest
/// precedence first (the names OMP v1 probed at every plugin root).
pub const LSP_CONFIG_FILES: [&str; 6] =
	["lsp.json", ".lsp.json", "lsp.yaml", ".lsp.yaml", "lsp.yml", ".lsp.yml"];

/// Debug-adapter declaration files a plugin root may carry, lowest precedence
/// first (the names OMP v1 probed at every plugin root).
pub const DAP_CONFIG_FILES: [&str; 6] =
	["dap.json", ".dap.json", "dap.yaml", ".dap.yaml", "dap.yml", ".dap.yml"];

/// The user-scope plugin directory beneath the data directory.
#[must_use]
pub fn user_plugins_dir(data_dir: &Path) -> PathBuf {
	data_dir.join("plugins")
}

/// The project-scope plugin directory beneath a project root.
#[must_use]
pub fn project_plugins_dir(project_root: &Path) -> PathBuf {
	project_root.join(".omp/plugins")
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
		let registry: Self = serde_json::from_slice(&bytes)
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
}

/// One enabled, resolved plugin install.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClaudePlugin {
	/// Registry id, `name@marketplace`.
	pub id:          Str,
	/// Plugin name (the id before `@`); namespaces commands and MCP servers.
	pub name:        Str,
	/// Marketplace name (the id after `@`).
	pub marketplace: Str,
	/// Recorded version.
	pub version:     Str,
	/// Registry the install came from.
	pub scope:       PluginScope,
	/// Canonical plugin root (the value of `${CLAUDE_PLUGIN_ROOT}`).
	pub root:        PathBuf,
	/// Package format and its loadable components.
	pub layout:      PluginLayout,
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
	/// [`LSP_CONFIG_FILES`], then manifest `lspServers`.
	pub lsp:      Box<[ConfigDeclaration]>,
	/// Debug-adapter declarations, lowest precedence first: the root
	/// [`DAP_CONFIG_FILES`].
	pub dap:      Box<[ConfigDeclaration]>,
}

/// Where a plugin declares a server map (MCP servers, language servers, or
/// debug adapters).
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ConfigDeclaration {
	/// A JSON (or, for LSP/DAP, YAML by extension) file: a `{kind: {...}}`
	/// wrapper or a flat `{name: server}` map.
	File(PathBuf),
	/// A server map written inline in the plugin manifest.
	Inline {
		/// Manifest path (the declaration's source).
		manifest: PathBuf,
		/// The raw JSON object of servers.
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
/// `root`; `${PLUGIN_DATA}` becomes `data` when the plugin has a data
/// directory. A value without variables is returned as is.
#[must_use]
pub fn expand_plugin_vars(value: Str, root: &Path, data: Option<&Path>) -> Str {
	if !value.contains("${PLUGIN_ROOT}")
		&& !value.contains("${PLUGIN_DATA}")
		&& !value.contains("${CLAUDE_PLUGIN_ROOT}")
		&& !value.contains("${OMP_PLUGIN_ROOT}")
	{
		return value;
	}
	let root = root.to_string_lossy();
	let replaced = value
		.replace("${PLUGIN_ROOT}", root.as_ref())
		.replace("${CLAUDE_PLUGIN_ROOT}", root.as_ref())
		.replace("${OMP_PLUGIN_ROOT}", root.as_ref());
	match data {
		Some(data) => Str::new(replaced.replace("${PLUGIN_DATA}", data.to_string_lossy().as_ref())),
		None => Str::new(replaced),
	}
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
}

/// The installed, enabled plugin set of one project, plus everything that did
/// not load.
#[derive(Debug, Default)]
pub struct ClaudePlugins {
	/// Resolved plugins: project scope first, then user scope, each in id
	/// order.
	pub plugins:     Vec<ClaudePlugin>,
	/// Non-fatal resolution diagnostics, one per problem.
	pub diagnostics: Vec<PluginDiagnostic>,
}

impl ClaudePlugins {
	/// Resolves the user registry under `data_dir` and the project registry
	/// under `project_root`.
	#[must_use]
	pub fn resolve(data_dir: &Path, project_root: &Path) -> Self {
		let project_root =
			fs::canonicalize(project_root).unwrap_or_else(|_| project_root.to_path_buf());
		Self::resolve_registries(
			&project_plugins_dir(&project_root).join(REGISTRY_FILE),
			&user_plugins_dir(data_dir).join(REGISTRY_FILE),
		)
	}

	/// Resolves one project and one user registry file.
	#[must_use]
	pub fn resolve_registries(project_registry: &Path, user_registry: &Path) -> Self {
		let mut out = Self::default();
		let mut shadowed = BTreeSet::<Str>::new();
		let mut seen_roots = BTreeSet::<PathBuf>::new();
		for (scope, registry_path) in
			[(PluginScope::Project, project_registry), (PluginScope::User, user_registry)]
		{
			let registry = match InstalledPluginsRegistry::read(registry_path) {
				Ok(registry) => registry,
				Err(error) => {
					out.diagnostics.push(error.into());
					continue;
				},
			};
			let mut enabled_here = Vec::new();
			for (id, entries) in registry.plugins {
				let Some((name, marketplace)) = id
					.rsplit_once('@')
					.filter(|(name, marketplace)| !name.is_empty() && !marketplace.is_empty())
				else {
					out.diagnostics
						.push(PluginDiagnostic::InvalidId { registry: registry_path.to_owned(), id });
					continue;
				};
				if shadowed.contains(&id) {
					continue;
				}
				let (name, marketplace) = (Str::new(name), Str::new(marketplace));
				let mut any_enabled = false;
				for entry in entries.into_iter().filter(|entry| entry.enabled) {
					any_enabled = true;
					let Some(root) = open_root(&id, &entry.install_path, &mut out.diagnostics) else {
						continue;
					};
					if !seen_roots.insert(root.clone()) {
						continue;
					}
					let Some(layout) = resolve_layout(&id, &root, &mut out.diagnostics) else {
						continue;
					};
					out.plugins.push(ClaudePlugin {
						id: id.clone(),
						name: name.clone(),
						marketplace: marketplace.clone(),
						version: entry.version,
						scope,
						root,
						layout,
					});
				}
				if any_enabled {
					enabled_here.push(id);
				}
			}
			shadowed.extend(enabled_here);
		}
		out
	}
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
	hooks:         Option<IgnoredAny>,
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
	let mut lsp = resolver.existing_files(&LSP_CONFIG_FILES);
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
		.existing_files(&DAP_CONFIG_FILES)
		.into_boxed_slice();

	// Components without a runtime home: surfaced, never silently dropped.
	let manifest_marker = manifest_path.unwrap_or_else(|| root.to_path_buf());
	let unsupported = [
		(PluginComponent::Hooks, manifest.hooks.is_some(), &["hooks"][..]),
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

		let resolved = ClaudePlugins::resolve(&data, &project);

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

		let resolved = ClaudePlugins::resolve(&data, &project);

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
		let resolved = ClaudePlugins::resolve(&data, &project);
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
		write(&data.join("plugins/installed_plugins.json"), &registry(&[("p@m", &user_copy, true)]));
		write(
			&project.join(".omp/plugins/installed_plugins.json"),
			&registry(&[("p@m", &project_copy, true)]),
		);

		let resolved = ClaudePlugins::resolve(&data, &project);

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
		let resolved = ClaudePlugins::resolve(&data, &project);
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

		let resolved = ClaudePlugins::resolve(&data, temp.path());

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

		let resolved = ClaudePlugins::resolve(&data, temp.path());

		let components = resolved.plugins[0].claude_components().unwrap();
		assert!(components.skills.is_empty(), "{:?}", components.skills);
		assert!(matches!(resolved.diagnostics.as_slice(), [
			PluginDiagnostic::ComponentOutsideRoot { component: PluginComponent::Skills, .. }
		]));
	}
}
