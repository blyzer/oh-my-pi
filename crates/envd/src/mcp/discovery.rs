//! Read-only discovery of MCP declarations owned by other agent ecosystems.
//!
//! Native OMP files remain the writable authority. These adapters only
//! normalize foreign declarations into the same typed server contract; a
//! missing or malformed foreign file is contained to that source and cannot
//! suppress independent sources.

use std::{
	collections::BTreeMap,
	fs, io,
	path::{Path, PathBuf},
};

use omp_core::{Str, sf};
use omp_ext::{
	claude_plugin::{
		ClaudePlugin, ConfigDeclaration, PluginScope, expand_plugin_vars, resolve_plugin_command,
	},
	plugin_command::{CommandApprovals, PluginId, PluginLaunch, PluginLaunchKind},
};
use serde::Deserialize;

use super::{
	McpConfigPaths,
	config::{
		ConfigSource, ConfigSourceKind, McpConfigFile, McpServerConfig, RequestIdFormat,
		TransportKind,
	},
};
use crate::plugin_commands::AgentPluginLaunches;

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ServerDocument {
	#[serde(default)]
	mcp_servers: BTreeMap<Str, ForeignServer>,
	#[serde(default)]
	servers:     BTreeMap<Str, ForeignServer>,
}

#[derive(Debug, Default, Deserialize)]
struct OpenCodeDocument {
	#[serde(default)]
	mcp: BTreeMap<Str, ForeignServer>,
}

#[derive(Debug, Default, Deserialize)]
struct VsCodeDocument {
	#[serde(default)]
	servers: BTreeMap<Str, ForeignServer>,
	#[serde(default)]
	mcp:     VsCodeMcp,
}

#[derive(Debug, Default, Deserialize)]
struct VsCodeMcp {
	#[serde(default)]
	servers: BTreeMap<Str, ForeignServer>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ForeignServer {
	#[serde(default)]
	enabled:           Option<bool>,
	#[serde(default, rename = "type")]
	kind:              Option<Str>,
	#[serde(default)]
	transport:         Option<Str>,
	#[serde(default)]
	command:           Option<ForeignCommand>,
	#[serde(default)]
	args:              Vec<Str>,
	#[serde(default)]
	env:               BTreeMap<Str, Str>,
	#[serde(default)]
	environment:       BTreeMap<Str, Str>,
	#[serde(default)]
	cwd:               Option<PathBuf>,
	#[serde(default)]
	url:               Option<Str>,
	#[serde(default)]
	headers:           BTreeMap<Str, Str>,
	#[serde(default)]
	timeout:           Option<u64>,
	#[serde(default)]
	request_id_format: Option<RequestIdFormat>,
	#[serde(skip)]
	plugin_data:       Option<PathBuf>,
	/// Installed marketplace plugin root: `${CLAUDE_PLUGIN_ROOT}` and
	/// path-like relative commands resolve against it.
	#[serde(skip)]
	plugin_root:       Option<PathBuf>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(untagged)]
enum ForeignCommand {
	One(Str),
	Many(Vec<Str>),
}

#[derive(Debug, Default, Deserialize)]
struct CodexDocument {
	#[serde(default)]
	mcp_servers: BTreeMap<Str, CodexServer>,
}

#[derive(Debug, Default, Deserialize)]
struct CodexServer {
	#[serde(default)]
	enabled:          Option<bool>,
	#[serde(default)]
	command:          Option<Str>,
	#[serde(default)]
	args:             Vec<Str>,
	#[serde(default)]
	env:              BTreeMap<Str, Str>,
	#[serde(default)]
	url:              Option<Str>,
	#[serde(default)]
	http_headers:     BTreeMap<Str, Str>,
	#[serde(default)]
	cwd:              Option<PathBuf>,
	#[serde(default)]
	tool_timeout_sec: Option<u64>,
}

#[derive(Debug, Default, Deserialize)]
struct ClaudeUserDocument {
	#[serde(default, rename = "mcpServers")]
	mcp_servers: BTreeMap<Str, ForeignServer>,
	#[serde(default)]
	projects:    BTreeMap<PathBuf, ServerDocument>,
}

#[derive(Debug, Deserialize)]
struct AgentPluginManifest {
	#[serde(rename = "$schema")]
	schema:  Str,
	name:    Str,
	#[serde(default)]
	version: Option<Str>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct AgentPluginMcpDocument {
	#[serde(rename = "$schema")]
	schema:      Str,
	mcp_servers: BTreeMap<Str, ForeignServer>,
}

/// An installed plugin's MCP file: the nested `{"mcpServers": {...}}` shape
/// or the flat `{name: server}` marketplace shape.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum PluginMcpDocument {
	Nested {
		#[serde(rename = "mcpServers")]
		mcp_servers: BTreeMap<Str, ForeignServer>,
	},
	Flat(BTreeMap<Str, ForeignServer>),
}

impl PluginMcpDocument {
	fn into_servers(self) -> BTreeMap<Str, ForeignServer> {
		match self {
			Self::Nested { mcp_servers } => mcp_servers,
			Self::Flat(servers) => servers,
		}
	}
}

const AGENT_PLUGIN_SCHEMA: &str = omp_ext::claude_plugin::AGENT_PLUGIN_SCHEMA;
const AGENT_PLUGIN_MCP_SCHEMA: &str = "https://agent-plugins.org/schemas/1.0.0/mcp.schema.json";

/// Discovers every supported foreign MCP source in deterministic precedence
/// order. Native sources are loaded by the caller before these rows.
pub(super) fn sources(paths: &McpConfigPaths) -> Vec<ConfigSource> {
	let project = paths.root.parent().unwrap_or(Path::new("."));
	let home = &paths.home;
	let mut sources = Vec::new();

	// Claude: project declarations precede user declarations. `~/.claude.json`
	// may carry both a global map and a map keyed by canonical project path.
	push_json(
		&mut sources,
		project.join(".claude/.mcp.json"),
		ConfigSourceKind::ClaudeProject,
		JsonShape::Common,
	);
	let claude_user = home.join(".claude.json");
	if let Some(document) = read_json::<ClaudeUserDocument>(&claude_user) {
		let canonical_project = fs::canonicalize(project).unwrap_or_else(|_| project.to_path_buf());
		if let Some(project_document) = document
			.projects
			.get(project)
			.or_else(|| document.projects.get(&canonical_project))
		{
			push_document(
				&mut sources,
				claude_user.clone(),
				ConfigSourceKind::ClaudeProject,
				project_document.mcp_servers.clone(),
			);
		}
		push_document(&mut sources, claude_user, ConfigSourceKind::ClaudeUser, document.mcp_servers);
	}
	push_json(
		&mut sources,
		home.join(".claude/mcp.json"),
		ConfigSourceKind::ClaudeUser,
		JsonShape::Common,
	);

	for plugin in agent_plugins(paths) {
		push_agent_plugin(&mut sources, plugin, &paths.command_approvals);
	}
	push_claude_plugins(&mut sources, &paths.claude_plugins);

	push_codex(&mut sources, project.join(".codex/config.toml"), ConfigSourceKind::CodexProject);
	push_codex(&mut sources, home.join(".codex/config.toml"), ConfigSourceKind::CodexUser);
	push_json(
		&mut sources,
		project.join(".gemini/settings.json"),
		ConfigSourceKind::GeminiProject,
		JsonShape::Common,
	);
	push_json(
		&mut sources,
		home.join(".gemini/settings.json"),
		ConfigSourceKind::GeminiUser,
		JsonShape::Common,
	);

	// OpenCode merges low-to-high; emit the high-precedence files first because
	// the central resolver is first-wins within one provider.
	for path in [
		project.join(".opencode/opencode.jsonc"),
		project.join(".opencode/opencode.json"),
		project.join("opencode.jsonc"),
		project.join("opencode.json"),
	] {
		push_json(&mut sources, path, ConfigSourceKind::OpenCodeProject, JsonShape::OpenCode);
	}
	for path in
		[home.join(".config/opencode/opencode.jsonc"), home.join(".config/opencode/opencode.json")]
	{
		push_json(&mut sources, path, ConfigSourceKind::OpenCodeUser, JsonShape::OpenCode);
	}
	push_json(
		&mut sources,
		project.join(".cursor/mcp.json"),
		ConfigSourceKind::CursorProject,
		JsonShape::Common,
	);
	push_json(
		&mut sources,
		home.join(".cursor/mcp.json"),
		ConfigSourceKind::CursorUser,
		JsonShape::Common,
	);
	push_json(
		&mut sources,
		project.join(".windsurf/mcp_config.json"),
		ConfigSourceKind::WindsurfProject,
		JsonShape::Common,
	);
	push_json(
		&mut sources,
		home.join(".codeium/windsurf/mcp_config.json"),
		ConfigSourceKind::WindsurfUser,
		JsonShape::Common,
	);
	push_json(
		&mut sources,
		project.join(".vscode/mcp.json"),
		ConfigSourceKind::VsCodeProject,
		JsonShape::VsCode,
	);
	for path in [project.join("mcp.json"), project.join("mcp.config.json")] {
		push_json(&mut sources, path, ConfigSourceKind::StandaloneProject, JsonShape::Common);
	}
	sources
}

/// One Agent Plugins 1.0 package's MCP declaration, normalized.
struct AgentPluginMcp {
	/// The package's identity: a marketplace install's `name@marketplace`,
	/// else its manifest `name`.
	id:      PluginId,
	/// Manifest `version`; empty when the manifest records none.
	version: Str,
	/// Canonical package root.
	root:    PathBuf,
	/// The contained `mcp.json`.
	path:    PathBuf,
	kind:    ConfigSourceKind,
	/// Normalized servers in name order.
	servers: Vec<(Str, McpServerConfig)>,
}

/// Every Agent Plugins package the project and user plugin directories hold,
/// then every root beside them ([`McpConfigPaths::agent_plugin_roots`]), in
/// discovery precedence order.
fn agent_plugins(paths: &McpConfigPaths) -> Vec<AgentPluginMcp> {
	let project = paths.root.parent().unwrap_or_else(|| Path::new("."));
	let user_config_root = paths.user.parent().unwrap_or(&paths.home);
	let plugin_data_root = user_config_root.join("agent/plugin-data");
	let mut plugins = Vec::new();
	for (container, kind) in [
		(project.join(".omp/extensions"), ConfigSourceKind::AgentPluginProject),
		(project.join(".agent/plugins"), ConfigSourceKind::AgentPluginProject),
		(project.join(".agents/plugins"), ConfigSourceKind::AgentPluginProject),
		(user_config_root.join("extensions"), ConfigSourceKind::AgentPluginUser),
		(user_config_root.join("agent/plugins"), ConfigSourceKind::AgentPluginUser),
	] {
		let Ok(container_root) = fs::canonicalize(&container) else {
			continue;
		};
		let Ok(entries) = fs::read_dir(&container) else {
			continue;
		};
		let mut entries = entries.filter_map(Result::ok).collect::<Vec<_>>();
		entries.sort_by_key(std::fs::DirEntry::file_name);
		for entry in entries {
			let Ok(root) = fs::canonicalize(entry.path()) else {
				continue;
			};
			if !root.starts_with(&container_root) || !root.is_dir() {
				tracing::warn!(path = %entry.path().display(), "ignored Agent Plugin outside its discovery root");
				continue;
			}
			plugins.extend(read_agent_plugin(&plugin_data_root, &root, kind, None));
		}
	}
	// Only a package that comes from the project is project-scoped: one the
	// invocation names, or a user install, loads whatever the project MCP
	// policy (`ConfigSourceKind::loads`).
	for root in &paths.agent_plugin_roots {
		plugins.extend(read_agent_plugin(
			&plugin_data_root,
			&root.root,
			root.origin.source_kind(),
			root.origin.installed_id(),
		));
	}
	plugins
}

/// The package at `root`, joining discovery as `kind`. A marketplace install
/// (`installed`, its registry id) is identified, keeps its data, and names
/// its servers (`name@marketplace:server`) by that id, so two marketplaces
/// shipping the same package never collide; a local package by its manifest
/// `name`, its servers unprefixed.
fn read_agent_plugin(
	data_root: &Path,
	root: &Path,
	kind: ConfigSourceKind,
	installed: Option<&PluginId>,
) -> Option<AgentPluginMcp> {
	let root = fs::canonicalize(root).ok()?;
	let manifest_path = fs::canonicalize(root.join("plugin.json")).ok()?;
	if !manifest_path.starts_with(&root) {
		tracing::warn!(path = %manifest_path.display(), "ignored Agent Plugin manifest outside its package");
		return None;
	}
	let body = fs::read_to_string(manifest_path).ok()?;
	let manifest = serde_json::from_str::<AgentPluginManifest>(&body).ok()?;
	if manifest.schema != AGENT_PLUGIN_SCHEMA || !safe_plugin_name(&manifest.name) {
		return None;
	}
	let configured = root.join("mcp.json");
	let real = fs::canonicalize(&configured).ok()?;
	if !real.starts_with(&root) {
		tracing::warn!(path = %configured.display(), "ignored Agent Plugin MCP file outside its package");
		return None;
	}
	let document = read_json::<AgentPluginMcpDocument>(&real)?;
	if document.schema != AGENT_PLUGIN_MCP_SCHEMA {
		tracing::warn!(path = %real.display(), "ignored unsupported Agent Plugin MCP schema");
		return None;
	}
	let (id, data) = match installed {
		Some(id) => (id.clone(), data_root.join(id.dir_name())),
		None => (PluginId::from(manifest.name.clone()), data_root.join(manifest.name.as_str())),
	};
	let base = real
		.parent()
		.map_or_else(|| PathBuf::from("."), Path::to_path_buf);
	let servers = document
		.mcp_servers
		.into_iter()
		.filter_map(|(name, mut server)| {
			server.plugin_data = Some(data.clone());
			server
				.env
				.insert(Str::new_static("PLUGIN_ROOT"), Str::new(root.to_string_lossy()));
			server
				.env
				.insert(Str::new_static("PLUGIN_DATA"), Str::new(data.to_string_lossy()));
			if server.cwd.is_none() && server.command.is_some() {
				server.cwd = Some(root.clone());
			}
			let normalized = server.normalize(&base);
			if normalized.is_none() {
				tracing::warn!(path = %real.display(), server = %name, "ignored malformed foreign MCP declaration");
			}
			let name = if installed.is_some() { sf!("{id}:{name}") } else { name };
			Some((name, normalized?))
		})
		.collect();
	Some(AgentPluginMcp {
		id,
		version: manifest.version.unwrap_or_default(),
		root,
		path: real,
		kind,
		servers,
	})
}

/// One Agent Plugins package's servers. A stdio server whose launch the
/// operator has not approved under the package's identity
/// ([`CommandApprovals::admit`]) is left out, so it never starts.
fn push_agent_plugin(
	out: &mut Vec<ConfigSource>,
	plugin: AgentPluginMcp,
	approvals: &CommandApprovals,
) {
	let mut file = McpConfigFile::default();
	for (name, server) in plugin.servers {
		if let Some(launch) = plugin_mcp_launch(&name, &server, &plugin.root)
			&& let Err(blocked) = approvals.admit(&plugin.id, &plugin.version, launch)
		{
			tracing::warn!(
				error = &blocked as &(dyn std::error::Error + 'static),
				"Agent Plugin MCP server not started"
			);
			continue;
		}
		file.mcp_servers.insert(name, server);
	}
	if !file.mcp_servers.is_empty() {
		out.push(ConfigSource { path: plugin.path, kind: plugin.kind, file });
	}
}

/// Every Agent Plugins package discovery would load for `paths`, with the
/// stdio launches its MCP declaration performs, exactly as discovery gates
/// them.
pub fn agent_plugin_launches(paths: &McpConfigPaths) -> Vec<AgentPluginLaunches> {
	agent_plugins(paths)
		.into_iter()
		.map(|plugin| AgentPluginLaunches {
			launches: plugin
				.servers
				.iter()
				.filter_map(|(name, server)| plugin_mcp_launch(name, server, &plugin.root))
				.collect(),
			plugin:   plugin.id,
			version:  plugin.version,
			root:     plugin.root,
			kind:     plugin.kind,
		})
		.collect()
}

/// Installed Claude-layout marketplace plugins: each `.mcp.json` or manifest
/// `mcpServers` declaration, servers namespaced by the plugin's registry id
/// (`<name>@<marketplace>:<server>`, so two marketplaces shipping the same
/// plugin never collide) and
/// `${CLAUDE_PLUGIN_ROOT}` expanded to the plugin root. A stdio server whose
/// launch the operator has not approved
/// ([`ClaudePlugin::admit_launch`]) is left out, so it never starts.
fn push_claude_plugins(out: &mut Vec<ConfigSource>, plugins: &[ClaudePlugin]) {
	for plugin in plugins {
		let kind = claude_plugin_kind(plugin);
		for (path, servers) in plugin_mcp_declarations(plugin) {
			let mut file = McpConfigFile::default();
			for (name, server) in servers {
				if let Some(launch) = plugin_mcp_launch(&name, &server, &plugin.root)
					&& let Err(blocked) = plugin.admit_launch(launch)
				{
					tracing::warn!(
						error = &blocked as &(dyn std::error::Error + 'static),
						"installed plugin MCP server not started"
					);
					continue;
				}
				file.mcp_servers.insert(name, server);
			}
			if !file.mcp_servers.is_empty() {
				out.push(ConfigSource { path, kind, file });
			}
		}
	}
}

/// The source kind `plugin`'s MCP declarations join discovery as: project
/// scope for a plugin installed for the project, else user scope.
pub const fn claude_plugin_kind(plugin: &ClaudePlugin) -> ConfigSourceKind {
	match plugin.scope {
		PluginScope::Project => ConfigSourceKind::ClaudePluginProject,
		PluginScope::User => ConfigSourceKind::ClaudePluginUser,
	}
}

/// Every process `plugin`'s MCP declarations would launch, exactly as
/// discovery gates them.
pub fn plugin_mcp_launches(plugin: &ClaudePlugin) -> impl Iterator<Item = PluginLaunch> {
	plugin_mcp_declarations(plugin)
		.into_iter()
		.flat_map(|(_, servers)| servers)
		.filter_map(|(name, server)| plugin_mcp_launch(&name, &server, &plugin.root))
}

/// The launch a normalized MCP server of the plugin rooted at `root`
/// performs, bound to the plugin files it names: stdio servers only.
fn plugin_mcp_launch(name: &Str, server: &McpServerConfig, root: &Path) -> Option<PluginLaunch> {
	let command = server.command.clone()?;
	Some(
		PluginLaunch::new(
			PluginLaunchKind::McpServer,
			name.clone(),
			command,
			server.args.iter().cloned(),
			server
				.env
				.iter()
				.map(|(name, value)| (name.clone(), value.clone())),
		)
		.with_cwd(
			server
				.cwd
				.as_ref()
				.map(|cwd| Str::new(cwd.to_string_lossy())),
		)
		.with_plugin_files(root),
	)
}

/// `plugin`'s MCP declarations, each with its normalized, namespaced servers.
fn plugin_mcp_declarations(plugin: &ClaudePlugin) -> Vec<(PathBuf, Vec<(Str, McpServerConfig)>)> {
	let Some(components) = plugin.claude_components() else {
		return Vec::new();
	};
	let root = Str::new(plugin.root.to_string_lossy());
	let mut declarations = Vec::with_capacity(components.mcp.len());
	for declaration in &components.mcp {
		let (path, base, servers) = match declaration {
			ConfigDeclaration::File(path) => {
				let Some(document) = read_json::<PluginMcpDocument>(path) else {
					continue;
				};
				let base = path.parent().unwrap_or(&plugin.root).to_path_buf();
				(path.clone(), base, document.into_servers())
			},
			ConfigDeclaration::Inline { manifest, servers } => {
				match serde_json::from_str::<BTreeMap<Str, ForeignServer>>(servers) {
					Ok(servers) => (manifest.clone(), plugin.root.clone(), servers),
					Err(error) => {
						tracing::warn!(path = %manifest.display(), %error, "failed to parse plugin manifest mcpServers");
						continue;
					},
				}
			},
		};
		let servers = servers
			.into_iter()
			.filter_map(|(name, mut server)| {
				server.plugin_root = Some(plugin.root.clone());
				if server.command.is_some() {
					server
						.env
						.entry(Str::new_static("CLAUDE_PLUGIN_ROOT"))
						.or_insert_with(|| root.clone());
				}
				let normalized = server.normalize(&base);
				if normalized.is_none() {
					tracing::warn!(path = %path.display(), server = %name, "ignored malformed foreign MCP declaration");
				}
				Some((sf!("{}:{name}", plugin.id), normalized?))
			})
			.collect();
		declarations.push((path, servers));
	}
	declarations
}

#[derive(Clone, Copy)]
enum JsonShape {
	Common,
	OpenCode,
	VsCode,
}

fn push_json(out: &mut Vec<ConfigSource>, path: PathBuf, kind: ConfigSourceKind, shape: JsonShape) {
	let servers = match shape {
		JsonShape::Common => {
			let Some(document) = read_jsonc::<ServerDocument>(&path) else {
				return;
			};
			if document.mcp_servers.is_empty() {
				document.servers
			} else {
				document.mcp_servers
			}
		},
		JsonShape::OpenCode => {
			let Some(document) = read_jsonc::<OpenCodeDocument>(&path) else {
				return;
			};
			document.mcp
		},
		JsonShape::VsCode => {
			let Some(document) = read_jsonc::<VsCodeDocument>(&path) else {
				return;
			};
			if document.servers.is_empty() {
				document.mcp.servers
			} else {
				document.servers
			}
		},
	};
	push_document(out, path, kind, servers);
}

fn push_document(
	out: &mut Vec<ConfigSource>,
	path: PathBuf,
	kind: ConfigSourceKind,
	servers: BTreeMap<Str, ForeignServer>,
) {
	let base = path
		.parent()
		.map_or_else(|| PathBuf::from("."), Path::to_path_buf);
	push_document_at(out, path, &base, kind, servers);
}

/// Normalizes `servers` declared by `path`, resolving relative values against
/// `base`.
fn push_document_at(
	out: &mut Vec<ConfigSource>,
	path: PathBuf,
	base: &Path,
	kind: ConfigSourceKind,
	servers: BTreeMap<Str, ForeignServer>,
) {
	if servers.is_empty() {
		return;
	}
	let mut file = McpConfigFile::default();
	for (name, server) in servers {
		match server.normalize(base) {
			Some(server) => {
				file.mcp_servers.insert(name, server);
			},
			None => {
				tracing::warn!(path = %path.display(), server = %name, "ignored malformed foreign MCP declaration")
			},
		}
	}
	if !file.mcp_servers.is_empty() {
		out.push(ConfigSource { path, kind, file });
	}
}

impl ForeignServer {
	fn normalize(self, base: &Path) -> Option<McpServerConfig> {
		let plugin_data = self.plugin_data;
		let plugin_root = self.plugin_root;
		let replace = |value| {
			expand_plugin_vars(value, plugin_root.as_deref().unwrap_or(base), plugin_data.as_deref())
		};
		let (command, mut command_args) = match self.command {
			Some(ForeignCommand::One(command)) => (Some(replace(command)), Vec::new()),
			Some(ForeignCommand::Many(mut words)) if !words.is_empty() => {
				let command = replace(words.remove(0));
				for word in &mut words {
					*word = replace(word.clone());
				}
				(Some(command), words)
			},
			_ => (None, Vec::new()),
		};
		// A plugin's path-like relative command (`./bin/server`) names a file
		// in its package, not in the session cwd.
		let command = command.map(|command| {
			if plugin_root.is_some() {
				resolve_plugin_command(command, base)
			} else {
				command
			}
		});
		command_args.extend(self.args.into_iter().map(replace));
		let mut env = self.environment;
		env.extend(self.env);
		for value in env.values_mut() {
			*value = replace(value.clone());
		}
		let transport = match self.kind.as_deref().or(self.transport.as_deref()) {
			Some("http" | "remote") => Some(TransportKind::Http),
			Some("sse") => Some(TransportKind::Sse),
			Some("stdio" | "local") => Some(TransportKind::Stdio),
			Some(_) => return None,
			None => None,
		};
		let cwd = self.cwd.map(|cwd| {
			let encoded = cwd.to_str().map(str::to_owned);
			let cwd = encoded
				.map(|value| PathBuf::from(replace(Str::new(value)).as_str()))
				.unwrap_or(cwd);
			if cwd.is_absolute() {
				cwd
			} else {
				base.join(cwd)
			}
		});
		Some(McpServerConfig {
			transport,
			enabled: self.enabled.unwrap_or(true),
			command,
			args: command_args,
			env,
			env_policy: None,
			env_literal_keys: Default::default(),
			cwd,
			url: self.url.map(replace),
			headers: self
				.headers
				.into_iter()
				.map(|(name, value)| (name, replace(value)))
				.collect(),
			header_policy: None,
			timeout: self.timeout,
			request_id_format: self.request_id_format,
			auth: None,
			oauth: None,
			protocol_versions: Vec::new(),
		})
	}
}

fn safe_plugin_name(name: &str) -> bool {
	(1..=64).contains(&name.len())
		&& !name.contains("..")
		&& !name.contains("--")
		&& !name
			.as_bytes()
			.first()
			.is_some_and(|byte| matches!(*byte, b'.' | b'-'))
		&& !name
			.as_bytes()
			.last()
			.is_some_and(|byte| matches!(*byte, b'.' | b'-'))
		&& name.bytes().all(|byte| {
			byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'-')
		})
}

fn push_codex(out: &mut Vec<ConfigSource>, path: PathBuf, kind: ConfigSourceKind) {
	let Some(document) = read_toml::<CodexDocument>(&path) else {
		return;
	};
	if document.mcp_servers.is_empty() {
		return;
	}
	let base = path.parent().unwrap_or(Path::new("."));
	let mut file = McpConfigFile::default();
	for (name, server) in document.mcp_servers {
		let cwd = server.cwd.map(|cwd| {
			if cwd.is_absolute() {
				cwd
			} else {
				base.join(cwd)
			}
		});
		file.mcp_servers.insert(name, McpServerConfig {
			transport: Some(if server.url.is_some() {
				TransportKind::Http
			} else {
				TransportKind::Stdio
			}),
			enabled: server.enabled.unwrap_or(true),
			command: server.command,
			args: server.args,
			env: server.env,
			env_policy: None,
			env_literal_keys: Default::default(),
			cwd,
			url: server.url,
			headers: server.http_headers,
			header_policy: None,
			timeout: server
				.tool_timeout_sec
				.and_then(|seconds| seconds.checked_mul(1_000)),
			request_id_format: None,
			auth: None,
			oauth: None,
			protocol_versions: Vec::new(),
		});
	}
	out.push(ConfigSource { path, kind, file });
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Option<T> {
	read_source(path, |body| serde_json::from_str(body).map_err(ReadError::Json))
}

fn read_jsonc<T: for<'de> Deserialize<'de>>(path: &Path) -> Option<T> {
	read_source(path, |body| {
		let stripped = strip_json_comments(body);
		serde_json::from_str(&stripped).map_err(ReadError::Json)
	})
}

fn read_toml<T: for<'de> Deserialize<'de>>(path: &Path) -> Option<T> {
	read_source(path, |body| toml::from_str(body).map_err(ReadError::Toml))
}

fn read_source<T>(path: &Path, parse: impl FnOnce(&str) -> Result<T, ReadError>) -> Option<T> {
	let body = match fs::read_to_string(path) {
		Ok(body) => body,
		Err(error)
			if matches!(error.kind(), io::ErrorKind::NotFound | io::ErrorKind::NotADirectory) =>
		{
			return None;
		},
		Err(error) => {
			tracing::warn!(path = %path.display(), %error, "failed to read foreign MCP configuration");
			return None;
		},
	};
	match parse(&body) {
		Ok(value) => Some(value),
		Err(error) => {
			tracing::warn!(path = %path.display(), %error, "failed to parse foreign MCP configuration");
			None
		},
	}
}

#[derive(Debug, thiserror::Error)]
enum ReadError {
	#[error("JSON document is malformed")]
	Json(#[source] serde_json::Error),
	#[error("TOML document is malformed")]
	Toml(#[source] toml::de::Error),
}

fn strip_json_comments(source: &str) -> String {
	let mut out = Vec::with_capacity(source.len());
	let bytes = source.as_bytes();
	let (mut index, mut string, mut escaped) = (0, false, false);
	while index < bytes.len() {
		let byte = bytes[index];
		if string {
			out.push(byte);
			if escaped {
				escaped = false;
			} else if byte == b'\\' {
				escaped = true;
			} else if byte == b'"' {
				string = false;
			}
			index += 1;
			continue;
		}
		if byte == b'"' {
			string = true;
			out.push(b'"');
			index += 1;
		} else if byte == b'/' && bytes.get(index + 1) == Some(&b'/') {
			index += 2;
			while index < bytes.len() && bytes[index] != b'\n' {
				index += 1;
			}
		} else if byte == b'/' && bytes.get(index + 1) == Some(&b'*') {
			index += 2;
			while index + 1 < bytes.len() && !(bytes[index] == b'*' && bytes[index + 1] == b'/') {
				if bytes[index] == b'\n' {
					out.push(b'\n');
				}
				index += 1;
			}
			index = (index + 2).min(bytes.len());
		} else if byte == b',' {
			let mut next = index + 1;
			while bytes.get(next).is_some_and(u8::is_ascii_whitespace) {
				next += 1;
			}
			if matches!(bytes.get(next), Some(b'}' | b']')) {
				index += 1;
				continue;
			}
			out.push(byte);
			index += 1;
		} else {
			out.push(byte);
			index += 1;
		}
	}
	String::from_utf8(out).expect("comment stripping preserves UTF-8 bytes")
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::mcp::{AgentPluginOrigin, AgentPluginRoot, McpSettings};

	fn write(path: &Path, body: &str) {
		fs::create_dir_all(path.parent().unwrap()).unwrap();
		fs::write(path, body).unwrap();
	}

	#[test]
	fn provider_precedence_and_errors_are_source_local() {
		let temp = tempfile::tempdir().unwrap();
		let home = temp.path().join("home");
		let project = temp.path().join("project");
		let user_root = home.join(".o2");
		write(&project.join(".claude/.mcp.json"), r#"{"mcpServers":{"same":{"command":"claude"}}}"#);
		write(
			&project.join(".gemini/settings.json"),
			r#"{"mcpServers":{"same":{"command":"gemini"},"gemini":{"command":"g"}}}"#,
		);
		write(&project.join(".cursor/mcp.json"), "{");
		write(
			&project.join(".vscode/mcp.json"),
			r#"{"servers":{"vscode":{"type":"stdio","command":"v"}}}"#,
		);
		let paths = McpConfigPaths::new(&user_root, &project);
		let sources = sources(&paths);
		let resolved = super::super::config::resolve_sources(&sources, true);
		assert_eq!(resolved.servers["same"].config.command.as_deref(), Some("claude"));
		assert!(resolved.servers.contains_key("gemini"));
		assert!(resolved.servers.contains_key("vscode"));
	}

	#[cfg(unix)]
	#[test]
	fn agent_plugin_paths_are_contained_and_root_placeholders_expand() {
		use std::os::unix::fs::symlink;

		let temp = tempfile::tempdir().unwrap();
		let home = temp.path().join("home");
		let project = temp.path().join("project");
		let plugin = temp.path().join("external/portable");
		write(
			&plugin.join("plugin.json"),
			r#"{"$schema":"https://agent-plugins.org/schemas/1.0.0/plugin.schema.json","name":"portable"}"#,
		);
		write(
			&plugin.join("mcp.json"),
			r#"{"$schema":"https://agent-plugins.org/schemas/1.0.0/mcp.schema.json","mcpServers":{"portable":{"type":"stdio","command":"${PLUGIN_ROOT}/server"}}}"#,
		);
		let escaped = temp.path().join("external/escaped");
		write(
			&escaped.join("plugin.json"),
			r#"{"$schema":"https://agent-plugins.org/schemas/1.0.0/plugin.schema.json","name":"escaped"}"#,
		);
		let outside = temp.path().join("outside.json");
		write(&outside, r#"{"mcpServers":{"escaped":{"command":"bad"}}}"#);
		symlink(&outside, escaped.join("mcp.json")).unwrap();

		let paths = McpConfigPaths::new(&home.join(".o2"), &project).with_agent_plugin_roots(vec![
			AgentPluginRoot::explicit(plugin.clone()),
			AgentPluginRoot::explicit(escaped),
		]);
		let paths = paths
			.clone()
			.with_command_approvals(approve_agent_plugins(&paths));
		let discovered = sources(&paths);
		let resolved = super::super::config::resolve_sources(&discovered, true);
		let expected = fs::canonicalize(&plugin).unwrap().join("server");
		assert_eq!(
			resolved.servers["portable"].config.command.as_deref(),
			Some(expected.to_string_lossy().as_ref())
		);
		assert_eq!(
			resolved.servers["portable"].config.env["PLUGIN_DATA"],
			Str::new(
				home
					.join(".o2/agent/plugin-data/portable")
					.to_string_lossy()
			)
		);
		assert!(!resolved.servers.contains_key("escaped"));
	}

	/// Approvals of every Agent Plugins launch `paths` discovers, as the
	/// operator's `omp ext trust <name> --approve-commands` records them.
	fn approve_agent_plugins(paths: &McpConfigPaths) -> CommandApprovals {
		CommandApprovals::new(
			crate::plugin_commands::agent_plugin_launches(paths)
				.iter()
				.flat_map(|package| {
					package
						.launches
						.iter()
						.map(|launch| (package.plugin.clone(), package.command_digest(launch)))
				}),
		)
	}

	fn agent_plugin(root: &Path, name: &str, servers: &str) {
		write(
			&root.join("plugin.json"),
			&format!(
				r#"{{"$schema":"https://agent-plugins.org/schemas/1.0.0/plugin.schema.json","name":"{name}","version":"2.0.0"}}"#
			),
		);
		write(
			&root.join("mcp.json"),
			&format!(
				r#"{{"$schema":"https://agent-plugins.org/schemas/1.0.0/mcp.schema.json","mcpServers":{servers}}}"#
			),
		);
	}

	#[test]
	fn unapproved_agent_plugin_stdio_servers_never_reach_the_roster() {
		let temp = tempfile::tempdir().unwrap();
		let home = temp.path().join("home");
		let project = temp.path().join("project");
		let servers = r#"{
			"local":{"type":"stdio","command":"${PLUGIN_ROOT}/server","args":["--stdio"]},
			"other":{"type":"stdio","command":"other-server"},
			"remote":{"type":"http","url":"https://example.test/mcp"}}"#;
		// Discovered in the project's plugin directory, and passed explicitly.
		agent_plugin(&project.join(".agents/plugins/portable"), "portable", servers);
		let explicit = temp.path().join("explicit");
		agent_plugin(&explicit, "explicit", r#"{"tool":{"command":"./tool"}}"#);
		let paths = McpConfigPaths::new(&home.join(".o2"), &project)
			.with_agent_plugin_roots(vec![AgentPluginRoot::explicit(explicit.clone())]);

		let packages = crate::plugin_commands::agent_plugin_launches(&paths);
		assert_eq!(
			packages
				.iter()
				.map(|package| (
					package.plugin.as_str(),
					package.version.as_str(),
					package.launches.len()
				))
				.collect::<Vec<_>>(),
			[("portable", "2.0.0", 2), ("explicit", "2.0.0", 1)],
			"only stdio servers launch a process"
		);
		let portable = &packages[0];
		let root = fs::canonicalize(project.join(".agents/plugins/portable")).unwrap();
		let local = &portable.launches[0];
		assert_eq!(local.command, Str::new(root.join("server").to_string_lossy()));
		assert_eq!(local.cwd.as_deref(), Some(root.to_string_lossy().as_ref()));
		assert!(
			local
				.env
				.iter()
				.any(|(name, value)| name == "PLUGIN_ROOT" && value.as_str() == root.to_string_lossy()),
			"the package root is part of the approval key: {local:?}"
		);

		let blocked =
			crate::plugin_commands::blocked_agent_plugin_launches(&paths, &McpSettings::default());
		assert_eq!(
			blocked
				.iter()
				.map(|blocked| (blocked.plugin.as_str(), blocked.server.as_str()))
				.collect::<Vec<_>>(),
			[("portable", "local"), ("portable", "other"), ("explicit", "tool")]
		);
		assert!(
			blocked[0]
				.to_string()
				.contains(&format!("omp ext trust portable --approve-command {}", blocked[0].digest)),
			"{}",
			blocked[0]
		);
		// With project configuration disabled, discovery drops the project's
		// package and the report no longer names it; the root the invocation
		// named is the user's own choice and still loads and is reported.
		let project_disabled = McpSettings { enable_project_config: false };
		assert_eq!(
			crate::plugin_commands::blocked_agent_plugin_launches(&paths, &project_disabled)
				.iter()
				.map(|blocked| (blocked.plugin.as_str(), blocked.server.as_str()))
				.collect::<Vec<_>>(),
			[("explicit", "tool")]
		);
		let all_approved = paths
			.clone()
			.with_command_approvals(approve_agent_plugins(&paths));
		assert_eq!(
			super::super::config::resolve_sources(&sources(&all_approved), false)
				.servers
				.keys()
				.map(Str::as_str)
				.collect::<Vec<_>>(),
			["tool"],
			"the loader agrees with the report"
		);
		let resolved = super::super::config::resolve_sources(&sources(&paths), true);
		assert!(!resolved.servers.contains_key("local"), "an unapproved server loaded");
		assert!(!resolved.servers.contains_key("tool"), "an unapproved server loaded");
		assert!(resolved.servers.contains_key("remote"), "a remote server needs no approval");

		// One approval admits exactly its server; approvals never cross
		// package names.
		let approved = paths.clone().with_command_approvals(CommandApprovals::new([
			(PluginId::new_static("portable"), portable.command_digest(local)),
			(PluginId::new_static("explicit"), portable.command_digest(&portable.launches[1])),
		]));
		let resolved = super::super::config::resolve_sources(&sources(&approved), true);
		assert!(resolved.servers.contains_key("local"));
		assert!(!resolved.servers.contains_key("other"));
		assert!(!resolved.servers.contains_key("tool"));
		let all = paths
			.clone()
			.with_command_approvals(approve_agent_plugins(&paths));
		assert!(
			crate::plugin_commands::blocked_agent_plugin_launches(&all, &McpSettings::default())
				.is_empty()
		);
		let resolved = super::super::config::resolve_sources(&sources(&all), true);
		assert!(
			["local", "other", "tool", "remote"]
				.iter()
				.all(|name| resolved.servers.contains_key(*name))
		);

		// An edited command line, or a moved working directory, asks again.
		agent_plugin(&explicit, "explicit", r#"{"tool":{"command":"./tool","cwd":"sub"}}"#);
		assert_eq!(
			crate::plugin_commands::blocked_agent_plugin_launches(&all, &McpSettings::default()).len(),
			1
		);
	}

	/// Only a package that comes from the project is project-scoped: with
	/// project MCP configuration disabled, a user install and a package in the
	/// user plugin directories still load and are still reported, while a
	/// project install and a package in the project's plugin directory do
	/// neither.
	#[test]
	fn agent_plugin_scope_follows_where_the_package_came_from() {
		use omp_ext::claude_plugin::PluginScope;

		let temp = tempfile::tempdir().unwrap();
		let home = temp.path().join("home");
		let project = temp.path().join("project");
		let user_install = temp.path().join("cache/user-install");
		let project_install = temp.path().join("cache/project-install");
		agent_plugin(&user_install, "user-install", r#"{"a":{"command":"./a"}}"#);
		agent_plugin(&project_install, "project-install", r#"{"b":{"command":"./b"}}"#);
		agent_plugin(&project.join(".agents/plugins/dir"), "dir", r#"{"c":{"command":"./c"}}"#);
		agent_plugin(
			&home.join(".o2/agent/plugins/personal"),
			"personal",
			r#"{"d":{"command":"./d"}}"#,
		);
		let installed = |root: PathBuf, name: &str, scope| AgentPluginRoot {
			root,
			origin: AgentPluginOrigin::Installed { id: PluginId::installed(name, "m"), scope },
		};
		let paths = McpConfigPaths::new(&home.join(".o2"), &project).with_agent_plugin_roots(vec![
			installed(user_install, "user-install", PluginScope::User),
			installed(project_install, "project-install", PluginScope::Project),
		]);
		assert_eq!(
			crate::plugin_commands::agent_plugin_launches(&paths)
				.iter()
				.map(|package| (package.plugin.as_str(), package.kind))
				.collect::<Vec<_>>(),
			[
				("dir", ConfigSourceKind::AgentPluginProject),
				("personal", ConfigSourceKind::AgentPluginUser),
				("user-install@m", ConfigSourceKind::AgentPluginUser),
				("project-install@m", ConfigSourceKind::AgentPluginProject),
			]
		);
		let reported = |enable_project_config: bool| {
			crate::plugin_commands::blocked_agent_plugin_launches(&paths, &McpSettings {
				enable_project_config,
			})
			.into_iter()
			.map(|blocked| blocked.server)
			.collect::<Vec<_>>()
		};
		assert_eq!(reported(true), ["c", "d", "user-install@m:a", "project-install@m:b"]);
		assert_eq!(reported(false), ["d", "user-install@m:a"], "only project packages drop out");
		let approved = paths
			.clone()
			.with_command_approvals(approve_agent_plugins(&paths));
		let loaded = |enable_project_config: bool| {
			super::super::config::resolve_sources(&sources(&approved), enable_project_config)
				.servers
				.keys()
				.cloned()
				.collect::<Vec<_>>()
		};
		assert_eq!(loaded(true), ["c", "d", "project-install@m:b", "user-install@m:a"]);
		assert_eq!(loaded(false), ["d", "user-install@m:a"], "the loader agrees with the report");
	}

	/// A package installed from a marketplace is identified as
	/// `name@marketplace`: two marketplaces shipping the same package keep
	/// their approvals, data directories, and servers apart.
	#[test]
	fn two_marketplaces_shipping_one_package_never_collide() {
		use omp_ext::claude_plugin::PluginScope;

		let temp = tempfile::tempdir().unwrap();
		let home = temp.path().join("home");
		let project = temp.path().join("project");
		let official = temp.path().join("cache/official/docs");
		let mirror = temp.path().join("cache/mirror/docs");
		agent_plugin(&official, "docs", r#"{"search":{"command":"./search"}}"#);
		agent_plugin(&mirror, "docs", r#"{"search":{"command":"./search"}}"#);
		let installed = |root: &Path, marketplace: &str| AgentPluginRoot {
			root:   root.to_path_buf(),
			origin: AgentPluginOrigin::Installed {
				id:    PluginId::installed("docs", marketplace),
				scope: PluginScope::User,
			},
		};
		let paths = McpConfigPaths::new(&home.join(".o2"), &project).with_agent_plugin_roots(vec![
			installed(&official, "official"),
			installed(&mirror, "mirror"),
		]);
		let packages = crate::plugin_commands::agent_plugin_launches(&paths);
		assert_eq!(
			packages
				.iter()
				.map(|package| package.plugin.as_str())
				.collect::<Vec<_>>(),
			["docs@official", "docs@mirror"]
		);
		assert_ne!(
			packages[0].command_digest(&packages[0].launches[0]),
			packages[1].command_digest(&packages[1].launches[0]),
			"approving one marketplace's package never approves the other's"
		);
		// Approving the official package alone loads its server alone.
		let approved = paths.with_command_approvals(CommandApprovals::new([(
			packages[0].plugin.clone(),
			packages[0].command_digest(&packages[0].launches[0]),
		)]));
		let resolved = super::super::config::resolve_sources(&sources(&approved), true);
		assert_eq!(resolved.servers.keys().map(Str::as_str).collect::<Vec<_>>(), [
			"docs@official:search"
		]);
		assert_eq!(
			resolved.servers["docs@official:search"].config.env["PLUGIN_DATA"],
			Str::new(
				home
					.join(".o2/agent/plugin-data/docs-official")
					.to_string_lossy()
			)
		);
		assert_eq!(
			crate::plugin_commands::blocked_agent_plugin_launches(&approved, &McpSettings::default())
				.iter()
				.map(|blocked| (blocked.plugin.as_str(), blocked.server.as_str()))
				.collect::<Vec<_>>(),
			[("docs@mirror", "docs@mirror:search")]
		);
	}

	fn install_plugins(registry: &Path, entries: &[(&str, &Path, bool)]) {
		use omp_ext::claude_plugin::{InstallScope, InstalledPluginEntry, InstalledPluginsRegistry};

		let mut installed = InstalledPluginsRegistry::default();
		for (id, path, enabled) in entries {
			installed
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
		write(registry, &serde_json::to_string(&installed).unwrap());
	}

	#[test]
	fn unapproved_claude_plugin_stdio_servers_never_reach_the_roster() {
		let temp = tempfile::tempdir().unwrap();
		let home = temp.path().join("home");
		let data = temp.path().join("data");
		let project = temp.path().join("project");
		fs::create_dir_all(&project).unwrap();
		let plugin = data.join("plugins/cache/plugins/market___tools___1.0.0");
		write(
			&plugin.join(".mcp.json"),
			r#"{"mcpServers":{
				"db":{"command":"${CLAUDE_PLUGIN_ROOT}/bin/db","args":["--stdio"]},
				"ok":{"command":"ok-server"},
				"remote":{"type":"http","url":"https://example.test/mcp"}
			}}"#,
		);
		install_plugins(&data.join("plugins/installed_plugins.json"), &[(
			"tools@market",
			&plugin,
			true,
		)]);
		let mut plugins = omp_ext::claude_plugin::ClaudePlugins::resolve(&data, &project, None);
		let launches = crate::plugin_commands::plugin_launches(&plugins.plugins[0]);
		assert_eq!(
			launches
				.iter()
				.map(|launch| launch.server.as_str())
				.collect::<Vec<_>>(),
			["tools@market:db", "tools@market:ok"],
			"only stdio servers launch a process"
		);
		let entry = &mut plugins.plugins[0];
		entry.approved_commands = [entry.command_digest(&launches[1])].into();
		let blocked =
			crate::plugin_commands::blocked_launches(&plugins.plugins, &McpSettings::default());
		assert_eq!(blocked.len(), 1);
		assert_eq!(blocked[0].server, "tools@market:db");

		let discovered = sources(
			&McpConfigPaths::new(&home.join(".o2"), &project)
				.with_claude_plugins(plugins.plugins.into()),
		);
		let resolved = super::super::config::resolve_sources(&discovered, true);
		assert!(!resolved.servers.contains_key("tools@market:db"), "an unapproved server loaded");
		assert!(resolved.servers.contains_key("tools@market:ok"));
		assert!(
			resolved.servers.contains_key("tools@market:remote"),
			"a remote server needs no approval"
		);
	}

	#[test]
	fn installed_claude_plugin_servers_expand_the_plugin_root_unless_disabled() {
		let temp = tempfile::tempdir().unwrap();
		let home = temp.path().join("home");
		let data = temp.path().join("data");
		let project = temp.path().join("project");
		fs::create_dir_all(&project).unwrap();
		let cache = data.join("plugins/cache/plugins");
		let files = cache.join("market___files___1.0.0");
		let inline = cache.join("market___inline___1.0.0");
		let quiet = cache.join("market___quiet___1.0.0");
		write(
			&files.join(".mcp.json"),
			r#"{"mcpServers":{"db":{"command":"${CLAUDE_PLUGIN_ROOT}/bin/db","args":["--config","${CLAUDE_PLUGIN_ROOT}/db.toml"],"env":{"DB_HOME":"${CLAUDE_PLUGIN_ROOT}/state"}}}}"#,
		);
		write(
			&inline.join(".claude-plugin/plugin.json"),
			r#"{"name":"inline","mcpServers":{"api":{"command":"./server","args":["${CLAUDE_PLUGIN_ROOT}"]}}}"#,
		);
		write(&quiet.join(".mcp.json"), r#"{"hush":{"command":"hush"}}"#);
		install_plugins(&data.join("plugins/installed_plugins.json"), &[
			("files@market", &files, true),
			("inline@market", &inline, true),
			("quiet@market", &quiet, false),
		]);
		let mut plugins = omp_ext::claude_plugin::ClaudePlugins::resolve(&data, &project, None);
		assert!(plugins.diagnostics.is_empty(), "{:?}", plugins.diagnostics);
		crate::plugin_commands::approve_all(&mut plugins.plugins);

		let discovered = sources(
			&McpConfigPaths::new(&home.join(".o2"), &project)
				.with_claude_plugins(plugins.plugins.into()),
		);
		let resolved = super::super::config::resolve_sources(&discovered, true);

		let files = fs::canonicalize(&files).unwrap();
		let inline = fs::canonicalize(&inline).unwrap();
		let db = &resolved.servers["files@market:db"];
		assert_eq!(db.source_kind, ConfigSourceKind::ClaudePluginUser);
		assert_eq!(
			db.config.command.as_deref(),
			Some(files.join("bin/db").to_string_lossy().as_ref())
		);
		assert_eq!(db.config.args, [
			Str::new_static("--config"),
			Str::new(files.join("db.toml").to_string_lossy())
		]);
		assert_eq!(db.config.env["DB_HOME"], Str::new(files.join("state").to_string_lossy()));
		assert_eq!(db.config.env["CLAUDE_PLUGIN_ROOT"], Str::new(files.to_string_lossy()));
		let api = &resolved.servers["inline@market:api"];
		assert_eq!(
			api.config.command.as_deref(),
			Some(inline.join("server").to_string_lossy().as_ref())
		);
		assert_eq!(api.config.args, [Str::new(inline.to_string_lossy())]);
		assert!(!resolved.servers.contains_key("quiet@market:hush"), "a disabled plugin never loads");
		assert!(!resolved.servers.keys().any(|name| name.contains("${")));
	}

	#[test]
	fn jsonc_and_codex_commands_normalize() {
		let temp = tempfile::tempdir().unwrap();
		let home = temp.path().join("home");
		let project = temp.path().join("project");
		write(
			&project.join(".opencode/opencode.jsonc"),
			r#"{// comment
			"mcp":{"open":{"type":"local","command":["runner","serve"],"environment":{"A":"B"}}}}"#,
		);
		write(
			&project.join(".codex/config.toml"),
			"[mcp_servers.codex]\ncommand = \"runner\"\nargs = [\"serve\"]\ntool_timeout_sec = 3\n",
		);
		let sources = sources(&McpConfigPaths::new(&home.join(".o2"), &project));
		let resolved = super::super::config::resolve_sources(&sources, true);
		assert_eq!(resolved.servers["open"].config.args, [Str::new("serve")]);
		assert_eq!(resolved.servers["codex"].config.timeout, Some(3_000));
	}
}
