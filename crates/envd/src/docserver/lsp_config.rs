//! Native LSP catalog loading, layered field merging, validation, and
//! provenance.

use std::{
	collections::BTreeMap,
	io,
	path::{Path, PathBuf},
	sync::Arc,
};

use omp_core::{
	Str,
	project_file::{self, Containment, ProjectFileError, containment_root},
};
use omp_ext::{
	claude_plugin::{
		ClaudePlugin, ConfigDeclaration, PluginComponent, PluginDiagnostic, expand_plugin_vars,
		resolve_plugin_command,
	},
	plugin_command::{PluginLaunch, PluginLaunchKind},
	workspace_trust::inventory::{LSP_CONFIG_NAMES, PROJECT_DIR},
};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use thiserror::Error;

use crate::docserver::lsp_process::{
	LspProcessConfig, LspProcessSelectorConfig, LspTransportSettings,
};

const MAX_CONFIG_BYTES: u64 = 1024 * 1024;
const MAX_VALUE_DEPTH: usize = 64;
const MAX_VALUE_NODES: usize = 100_000;

/// Origin class of one native LSP declaration.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, strum::IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum LspConfigSourceKind {
	/// Bundled OMP catalog.
	Builtin,
	/// User-owned OMP configuration.
	User,
	/// Project-owned OMP configuration.
	Project,
	/// Project-root dotfile configuration.
	Dotfile,
	/// Validated native extension-manifest contribution.
	Manifest,
	/// Installed, enabled Claude-format marketplace plugin declaration (root
	/// `.lsp.json` family or manifest `lspServers`), validated on its own
	/// before it joins the merge.
	Plugin,
}

/// Stable source identity retained on every resolved field.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct LspConfigProvenance {
	/// Source class.
	pub kind:   LspConfigSourceKind,
	/// Native file or manifest identity.
	pub source: Str,
}

/// A resolved field and the declaration that last wrote it.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Provenanced<T> {
	/// Resolved value.
	pub value:      T,
	/// Winning source.
	pub provenance: LspConfigProvenance,
}

/// One ordered native configuration input. Inputs are merged in slice order.
#[derive(Clone, Debug)]
pub struct LspConfigSource {
	/// Source provenance.
	pub provenance:  LspConfigProvenance,
	/// Configuration bytes.
	pub bytes:       Arc<[u8]>,
	/// Whether the bytes use YAML rather than JSON.
	pub yaml:        bool,
	/// Installed plugin root for a [`LspConfigSourceKind::Plugin`] source:
	/// `${CLAUDE_PLUGIN_ROOT}` in `command`, `args` and `env` expands to it and
	/// a path-like relative `command` resolves against it.
	pub plugin_root: Option<Arc<Path>>,
}

impl LspConfigSource {
	/// Reads a bounded native JSON/YAML source.
	pub fn read(kind: LspConfigSourceKind, path: &Path) -> Result<Self, LspConfigError> {
		Self::read_contained(kind, path, Containment::Unconfined)?.ok_or_else(|| {
			ProjectFileError::Io { path: path.to_owned(), source: io::ErrorKind::NotFound.into() }
				.into()
		})
	}

	/// [`LspConfigSource::read`] through the contained project-file reader:
	/// `None` when `path` is absent, an error when it is not a regular file
	/// of at most one MiB resolving inside `containment`.
	pub fn read_contained(
		kind: LspConfigSourceKind,
		path: &Path,
		containment: Containment<'_>,
	) -> Result<Option<Self>, LspConfigError> {
		let Some(bytes) = project_file::read_bytes(path, containment, MAX_CONFIG_BYTES)? else {
			return Ok(None);
		};
		let yaml = matches!(path.extension().and_then(|value| value.to_str()), Some("yaml" | "yml"));
		Ok(Some(Self {
			provenance: LspConfigProvenance { kind, source: Str::new(path.to_string_lossy()) },
			bytes: bytes.into(),
			yaml,
			plugin_root: None,
		}))
	}

	/// Reads one installed plugin's declaration: a root declaration file
	/// (YAML by extension) or a manifest `lspServers` map.
	pub fn plugin(
		plugin: &ClaudePlugin,
		declaration: &ConfigDeclaration,
	) -> Result<Self, LspConfigError> {
		let mut source = match declaration {
			ConfigDeclaration::File(path) => Self::read(LspConfigSourceKind::Plugin, path)?,
			ConfigDeclaration::Inline { manifest, servers } => Self {
				provenance:  LspConfigProvenance {
					kind:   LspConfigSourceKind::Plugin,
					source: Str::new(manifest.to_string_lossy()),
				},
				bytes:       format!(r#"{{"servers":{servers}}}"#).into_bytes().into(),
				yaml:        false,
				plugin_root: None,
			},
		};
		source.plugin_root = Some(Arc::from(plugin.root.as_path()));
		Ok(source)
	}

	/// Creates a bounded native manifest contribution already validated by the
	/// extension authority.
	pub fn manifest(identity: impl AsRef<str>, bytes: impl Into<Arc<[u8]>>, yaml: bool) -> Self {
		Self {
			provenance: LspConfigProvenance {
				kind:   LspConfigSourceKind::Manifest,
				source: Str::new(identity.as_ref()),
			},
			bytes: bytes.into(),
			yaml,
			plugin_root: None,
		}
	}

	/// Creates and validates a manifest contribution whose commands must name
	/// lock-materialized binaries or explicitly granted environment executables.
	pub fn manifest_checked(
		identity: impl AsRef<str>,
		bytes: impl Into<Arc<[u8]>>,
		yaml: bool,
		allowed_commands: impl IntoIterator<Item = Str>,
	) -> Result<Self, LspConfigError> {
		let source = Self::manifest(identity, bytes, yaml);
		let allowed = allowed_commands
			.into_iter()
			.collect::<std::collections::BTreeSet<_>>();
		let resolved = load_lsp_config(std::slice::from_ref(&source))?;
		for server in resolved.servers.values() {
			let command = &server.command.value;
			if command.contains('/') || command.contains('\\') || !allowed.contains(command) {
				return Err(LspConfigError::UndeclaredManifestCommand {
					server:  server.name.clone(),
					command: command.clone(),
				});
			}
		}
		Ok(source)
	}
}

/// Partially specified server declaration used during field-wise merging.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
struct LspServerPatch {
	command:                Option<Str>,
	args:                   Option<Vec<Str>>,
	file_types:             Option<Vec<Str>>,
	root_markers:           Option<Vec<Str>>,
	language_id:            Option<Str>,
	init_options:           Option<Value>,
	initialization_options: Option<Value>,
	settings:               Option<Value>,
	capabilities:           Option<Value>,
	priority:               Option<i32>,
	is_linter:              Option<bool>,
	disabled:               Option<bool>,
	warmup_timeout_ms:      Option<u64>,
	idle_timeout_ms:        Option<u64>,
	readiness_timeout_ms:   Option<u64>,
	env:                    Option<BTreeMap<Str, Str>>,
	/// Claude Code's `{".ext": "languageId"}` map: its keys stand in for
	/// `fileTypes`, and it implies the project-root marker `.` when no
	/// `rootMarkers` are declared (OMP v1 parity).
	extension_to_language:  Option<BTreeMap<Str, Str>>,
}

impl LspServerPatch {
	/// Expands `${CLAUDE_PLUGIN_ROOT}` (and its aliases) in `command`, `args`
	/// and `env`, then roots a path-like relative `command` at the plugin.
	fn expand_plugin_root(&mut self, root: &Path) {
		if let Some(command) = self.command.take() {
			self.command = Some(resolve_plugin_command(expand_plugin_vars(command, root, None), root));
		}
		for arg in self.args.iter_mut().flatten() {
			*arg = expand_plugin_vars(arg.clone(), root, None);
		}
		for value in self.env.iter_mut().flat_map(BTreeMap::values_mut) {
			*value = expand_plugin_vars(value.clone(), root, None);
		}
	}
}

#[derive(Default)]
struct MergedServer {
	command:              Option<Provenanced<Str>>,
	args:                 Option<Provenanced<Vec<Str>>>,
	file_types:           Option<Provenanced<Vec<Str>>>,
	root_markers:         Option<Provenanced<Vec<Str>>>,
	language_id:          Option<Provenanced<Option<Str>>>,
	init_options:         Option<Provenanced<Value>>,
	settings:             Option<Provenanced<Value>>,
	capabilities:         Option<Provenanced<Value>>,
	priority:             Option<Provenanced<i32>>,
	is_linter:            Option<Provenanced<bool>>,
	disabled:             Option<Provenanced<bool>>,
	warmup_timeout_ms:    Option<Provenanced<u64>>,
	idle_timeout_ms:      Option<Provenanced<Option<u64>>>,
	readiness_timeout_ms: Option<Provenanced<u64>>,
	env:                  Option<Provenanced<BTreeMap<Str, Str>>>,
}

/// Fully validated native language-server declaration.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ResolvedLspServer {
	/// Stable declaration name.
	pub name:                 Str,
	/// Executable name/path.
	pub command:              Provenanced<Str>,
	/// Exact command arguments.
	pub args:                 Provenanced<Vec<Str>>,
	/// Accepted extensions or exact filenames.
	pub file_types:           Provenanced<Vec<Str>>,
	/// Ancestor root markers, including single-level globs.
	pub root_markers:         Provenanced<Vec<Str>>,
	/// Optional explicit LSP language identifier.
	pub language_id:          Provenanced<Option<Str>>,
	/// Initialize options.
	pub init_options:         Provenanced<Value>,
	/// Configuration settings.
	pub settings:             Provenanced<Value>,
	/// Native capability hints.
	pub capabilities:         Provenanced<Value>,
	/// Explicit priority; larger values run first.
	pub priority:             Provenanced<i32>,
	/// Whether this declaration is a checker/linter rather than a primary
	/// server.
	pub is_linter:            Provenanced<bool>,
	/// Whether startup is disabled.
	pub disabled:             Provenanced<bool>,
	/// Startup warmup bound.
	pub warmup_timeout_ms:    Provenanced<u64>,
	/// Optional inactivity timeout.
	pub idle_timeout_ms:      Provenanced<Option<u64>>,
	/// Workspace readiness bound.
	pub readiness_timeout_ms: Provenanced<u64>,
	/// Extra process environment.
	pub env:                  Provenanced<BTreeMap<Str, Str>>,
}

/// A resolved catalog plus global timing policy.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ResolvedLspConfig {
	/// Declarations in deterministic name order.
	pub servers:         BTreeMap<Str, ResolvedLspServer>,
	/// Optional global idle timeout.
	pub idle_timeout_ms: Option<Provenanced<u64>>,
}

impl ResolvedLspServer {
	/// Lowers a resolved declaration into the process-owned startup shape.
	pub fn to_process_config(&self) -> LspProcessConfig {
		let path_patterns = self
			.file_types
			.value
			.iter()
			.map(|file_type| {
				let value = file_type.as_str();
				if value.starts_with('.') {
					Str::new(format!("**/*{value}"))
				} else if value.contains('.') {
					Str::new(format!("**/*.{value}"))
				} else {
					Str::new(format!("**/{value}"))
				}
			})
			.collect();
		let languages = self.language_id.value.iter().cloned().collect();
		let mut transport = LspTransportSettings::default();
		transport.initialize_timeout_ms = self.warmup_timeout_ms.value.clamp(1, 120_000);
		LspProcessConfig {
			name: self.name.clone(),
			priority: self.priority.value,
			selector: LspProcessSelectorConfig {
				languages,
				schemes: vec![Str::new_static("file")],
				path_patterns,
			},
			executable: PathBuf::from(self.command.value.as_str()),
			args: self.args.value.clone(),
			env: self.env.value.clone(),
			initialization_options: Some(self.init_options.value.clone()),
			settings: Some(self.settings.value.clone()),
			root_markers: self.root_markers.value.clone(),
			is_linter: self.is_linter.value,
			idle_timeout_ms: self.idle_timeout_ms.value,
			readiness_timeout_ms: self.readiness_timeout_ms.value,
			transport,
		}
	}
}

/// Loads the bundled language-server catalog.
pub fn bundled_lsp_defaults() -> Result<LspConfigSource, LspConfigError> {
	let bytes: Arc<[u8]> = include_bytes!("../../data/lsp-defaults.json")
		.as_slice()
		.into();
	Ok(LspConfigSource {
		provenance: LspConfigProvenance {
			kind:   LspConfigSourceKind::Builtin,
			source: Str::new_static("omp:lsp-defaults.json"),
		},
		bytes,
		yaml: false,
		plugin_root: None,
	})
}

/// Enumerates only native user/project paths. Foreign roots are never
/// considered. Existing paths are returned from low to high precedence.
pub fn discover_native_lsp_sources(
	user_root: Option<&Path>,
	project_root: &Path,
) -> Result<Vec<LspConfigSource>, LspConfigError> {
	discover_lsp_sources(user_root, project_root, Vec::new(), &[]).map(|found| found.sources)
}

/// Ordered sources plus the plugin declarations that did not load.
#[derive(Debug)]
pub struct DiscoveredLspSources {
	/// Sources from low to high precedence, ready for [`load_lsp_config`].
	pub sources:     Vec<LspConfigSource>,
	/// One diagnostic per rejected plugin declaration.
	pub diagnostics: Vec<PluginDiagnostic>,
}

/// Enumerates built-in, extension-manifest, installed-plugin, user, and
/// project sources in increasing precedence.
///
/// The only non-native roots ever read are the `plugins` handed in: the
/// installed, enabled marketplace plugins the composition resolved
/// ([`omp_ext::claude_plugin::ClaudePlugins`]), each contributing only the
/// declarations its [`ClaudeComponents::lsp`] located inside its own root.
/// `.claude`, `.codex`, and other foreign configuration roots are never
/// probed. Plugin declarations sit below every user and project file (OMP
/// v1's order), so a same-named user or project server overrides a plugin's
/// fields; a project-scope plugin overrides a user-scope one. Each plugin
/// declaration is validated on its own against the layers beneath it, so an
/// invalid one becomes a [`PluginDiagnostic::InvalidComponent`] instead of
/// failing the roster.
///
/// [`ClaudeComponents::lsp`]: omp_ext::claude_plugin::ClaudeComponents::lsp
pub fn discover_lsp_sources(
	user_root: Option<&Path>,
	project_root: &Path,
	mut manifests: Vec<LspConfigSource>,
	plugins: &[ClaudePlugin],
) -> Result<DiscoveredLspSources, LspConfigError> {
	let mut sources = vec![bundled_lsp_defaults()?];
	manifests.sort_by(|left, right| left.provenance.source.cmp(&right.provenance.source));
	sources.extend(manifests);
	let mut diagnostics = Vec::new();
	append_plugin_sources(&mut sources, plugins, &mut diagnostics);
	let user = Containment::Unconfined;
	if let Some(user_root) = user_root {
		append_existing(&mut sources, user_root, LspConfigSourceKind::User, user)?;
		append_existing(&mut sources, &user_root.join("agent"), LspConfigSourceKind::User, user)?;
	}
	let project = Containment::Within(containment_root(project_root));
	append_existing(
		&mut sources,
		&project_root.join(PROJECT_DIR),
		LspConfigSourceKind::Project,
		project,
	)?;
	append_existing(&mut sources, project_root, LspConfigSourceKind::Dotfile, project)?;
	Ok(DiscoveredLspSources { sources, diagnostics })
}

/// Appends every plugin declaration that merges cleanly over `sources`,
/// user-scope plugins first so project-scope ones win.
fn append_plugin_sources(
	sources: &mut Vec<LspConfigSource>,
	plugins: &[ClaudePlugin],
	diagnostics: &mut Vec<PluginDiagnostic>,
) {
	for plugin in plugins.iter().rev() {
		let Some(components) = plugin.claude_components() else {
			continue;
		};
		for declaration in &components.lsp {
			let admitted = LspConfigSource::plugin(plugin, declaration)
				.and_then(|source| gate_plugin_source(plugin, source, diagnostics))
				.and_then(|source| {
					sources.push(source);
					load_lsp_config(sources).map(drop).inspect_err(|_| {
						sources.pop();
					})
				});
			if let Err(error) = admitted {
				diagnostics.push(PluginDiagnostic::InvalidComponent {
					plugin:    plugin.id.clone().into(),
					component: PluginComponent::LspServers,
					path:      declaration.path().to_path_buf(),
					source:    Box::new(error),
				});
			}
		}
	}
}

/// Drops from one plugin declaration every server whose launch the operator
/// has not approved ([`ClaudePlugin::admit_launch`]), recording each refusal
/// as a [`PluginDiagnostic::CommandNotApproved`]; the approved servers still
/// load.
fn gate_plugin_source(
	plugin: &ClaudePlugin,
	mut source: LspConfigSource,
	diagnostics: &mut Vec<PluginDiagnostic>,
) -> Result<LspConfigSource, LspConfigError> {
	let (mut document, launches) = plugin_source_launches(&source)?;
	let mut blocked = Vec::new();
	for (name, launch) in launches {
		if let Err(refused) = plugin.admit_launch(launch) {
			diagnostics.push(refused.into());
			blocked.push(name);
		}
	}
	if blocked.is_empty() {
		return Ok(source);
	}
	// `plugin_source_launches` normalized this document, so it is an object
	// whose `servers`, when present, is the server map.
	if let Some(object) = document.as_object_mut() {
		let servers = if object.contains_key("servers") {
			object.get_mut("servers").and_then(Value::as_object_mut)
		} else {
			Some(object)
		};
		if let Some(servers) = servers {
			for name in &blocked {
				servers.remove(name.as_str());
			}
		}
	}
	source.bytes = serde_json::to_vec(&document)
		.map_err(|source| LspConfigError::InvalidDocument { source })?
		.into();
	source.yaml = false;
	Ok(source)
}

/// The parsed document of a plugin source plus, per server that sets its
/// `command`, `args`, or `env`, the launch it declares after plugin-root
/// expansion, bound to the plugin files it names.
fn plugin_source_launches(
	source: &LspConfigSource,
) -> Result<(Value, Vec<(Str, PluginLaunch)>), LspConfigError> {
	let document = parse_value(source)?;
	validate_value_bounds(&document)?;
	let (servers, _) = normalize_document(document.clone())?;
	let launches = servers
		.into_iter()
		.filter_map(|(name, mut patch)| {
			if let Some(root) = &source.plugin_root {
				patch.expand_plugin_root(root);
			}
			if patch.command.is_none() && patch.args.is_none() && patch.env.is_none() {
				return None;
			}
			let launch = PluginLaunch::new(
				PluginLaunchKind::LanguageServer,
				name.clone(),
				patch.command.unwrap_or_default(),
				patch.args.unwrap_or_default(),
				patch.env.unwrap_or_default(),
			);
			let launch = match &source.plugin_root {
				Some(root) => launch.with_plugin_files(root),
				None => launch,
			};
			Some((name, launch))
		})
		.collect();
	Ok((document, launches))
}

/// Every process `plugin`'s language-server declarations would launch,
/// exactly as discovery gates them; a declaration that does not parse
/// contributes nothing (it never loads).
pub(crate) fn plugin_lsp_launches(plugin: &ClaudePlugin) -> Vec<PluginLaunch> {
	let Some(components) = plugin.claude_components() else {
		return Vec::new();
	};
	components
		.lsp
		.iter()
		.filter_map(|declaration| {
			let source = LspConfigSource::plugin(plugin, declaration).ok()?;
			plugin_source_launches(&source).ok()
		})
		.flat_map(|(_, launches)| launches.into_iter().map(|(_, launch)| launch))
		.collect()
}

/// Appends each present config file of `directory`. A file the contained
/// reader refuses (outside the project, a special file, oversize) is skipped
/// with a warning rather than failing the roster: dropping a source can only
/// remove servers.
fn append_existing(
	sources: &mut Vec<LspConfigSource>,
	directory: &Path,
	kind: LspConfigSourceKind,
	containment: Containment<'_>,
) -> Result<(), LspConfigError> {
	for name in LSP_CONFIG_NAMES {
		let path = directory.join(name);
		match LspConfigSource::read_contained(kind, &path, containment) {
			Ok(Some(source)) => sources.push(source),
			Ok(None) => {},
			Err(LspConfigError::File(error @ ProjectFileError::Refused { .. })) => {
				tracing::warn!(
					path = %error.path().display(),
					reason = error.refusal().map_or("unreadable", <&str>::from),
					"LSP configuration file skipped"
				);
			},
			Err(error) => return Err(error),
		}
	}
	Ok(())
}

/// Merges ordered native sources field by field and retains winning
/// provenance.
pub fn load_lsp_config(sources: &[LspConfigSource]) -> Result<ResolvedLspConfig, LspConfigError> {
	let mut merged = BTreeMap::<Str, MergedServer>::new();
	let mut idle_timeout_ms = None;
	for source in sources {
		if source.bytes.len() as u64 > MAX_CONFIG_BYTES {
			return Err(LspConfigError::TooLarge {
				path:  PathBuf::from(source.provenance.source.as_str()),
				limit: MAX_CONFIG_BYTES,
			});
		}
		let value = parse_value(source)?;
		validate_value_bounds(&value)?;
		let (servers, idle) = normalize_document(value)?;
		if let Some(idle) = idle {
			idle_timeout_ms =
				Some(Provenanced { value: idle, provenance: source.provenance.clone() });
		}
		for (name, mut patch) in servers {
			if let Some(root) = &source.plugin_root {
				patch.expand_plugin_root(root);
			}
			merge_server(merged.entry(name).or_default(), patch, &source.provenance);
		}
	}
	let mut servers = BTreeMap::new();
	for (name, merged) in merged {
		let resolved = resolve_server(name.clone(), merged)?;
		servers.insert(name, resolved);
	}
	Ok(ResolvedLspConfig { servers, idle_timeout_ms })
}

fn parse_value(source: &LspConfigSource) -> Result<Value, LspConfigError> {
	if source.yaml {
		serde_yaml::from_slice(&source.bytes).map_err(|source_error| LspConfigError::ParseYaml {
			source_name: source.provenance.source.clone(),
			source:      source_error,
		})
	} else {
		serde_json::from_slice(&source.bytes).map_err(|source_error| LspConfigError::ParseJson {
			source_name: source.provenance.source.clone(),
			source:      source_error,
		})
	}
}

fn validate_value_bounds(value: &Value) -> Result<(), LspConfigError> {
	let mut nodes = 0_usize;
	let mut stack = vec![(value, 1_usize)];
	while let Some((value, depth)) = stack.pop() {
		nodes += 1;
		if depth > MAX_VALUE_DEPTH || nodes > MAX_VALUE_NODES {
			return Err(LspConfigError::StructureLimit);
		}
		match value {
			Value::Array(values) => stack.extend(values.iter().map(|value| (value, depth + 1))),
			Value::Object(values) => stack.extend(values.values().map(|value| (value, depth + 1))),
			_ => {},
		}
	}
	Ok(())
}

fn normalize_document(
	value: Value,
) -> Result<(BTreeMap<Str, LspServerPatch>, Option<u64>), LspConfigError> {
	let mut object = value
		.as_object()
		.cloned()
		.ok_or(LspConfigError::TopLevelObject)?;
	let idle = object
		.remove("idleTimeoutMs")
		.map(serde_json::from_value)
		.transpose()
		.map_err(|source| LspConfigError::InvalidDocument { source })?;
	let servers = match object.remove("servers") {
		Some(Value::Object(servers)) => servers,
		Some(_) => return Err(LspConfigError::ServersObject),
		None => object,
	};
	servers
		.into_iter()
		.map(|(name, value)| {
			let patch = serde_json::from_value(value)
				.map_err(|source| LspConfigError::InvalidServer { server: Str::new(&name), source })?;
			Ok((Str::new(name), patch))
		})
		.collect::<Result<BTreeMap<_, _>, _>>()
		.map(|servers| (servers, idle))
}

fn sourced<T>(value: T, provenance: &LspConfigProvenance) -> Provenanced<T> {
	Provenanced { value, provenance: provenance.clone() }
}

fn merge_server(
	target: &mut MergedServer,
	mut patch: LspServerPatch,
	provenance: &LspConfigProvenance,
) {
	if let Some(extensions) = patch.extension_to_language.take() {
		patch
			.file_types
			.get_or_insert_with(|| extensions.into_keys().collect());
		if patch.root_markers.is_none() && target.root_markers.is_none() {
			patch.root_markers = Some(vec![Str::new_static(".")]);
		}
	}
	if let Some(value) = patch.command {
		target.command = Some(sourced(value, provenance));
	}
	if let Some(value) = patch.args {
		target.args = Some(sourced(value, provenance));
	}
	if let Some(value) = patch.file_types {
		target.file_types = Some(sourced(value, provenance));
	}
	if let Some(value) = patch.root_markers {
		target.root_markers = Some(sourced(value, provenance));
	}
	if let Some(value) = patch.language_id {
		target.language_id = Some(sourced(Some(value), provenance));
	}
	if let Some(value) = patch.init_options.or(patch.initialization_options) {
		target.init_options = Some(sourced(value, provenance));
	}
	if let Some(value) = patch.settings {
		target.settings = Some(sourced(value, provenance));
	}
	if let Some(value) = patch.capabilities {
		target.capabilities = Some(sourced(value, provenance));
	}
	if let Some(value) = patch.priority {
		target.priority = Some(sourced(value, provenance));
	}
	if let Some(value) = patch.is_linter {
		target.is_linter = Some(sourced(value, provenance));
	}
	if let Some(value) = patch.disabled {
		target.disabled = Some(sourced(value, provenance));
	}
	if let Some(value) = patch.warmup_timeout_ms {
		target.warmup_timeout_ms = Some(sourced(value, provenance));
	}
	if let Some(value) = patch.idle_timeout_ms {
		target.idle_timeout_ms = Some(sourced(Some(value), provenance));
	}
	if let Some(value) = patch.readiness_timeout_ms {
		target.readiness_timeout_ms = Some(sourced(value, provenance));
	}
	if let Some(value) = patch.env {
		target.env = Some(sourced(value, provenance));
	}
}

fn resolve_server(name: Str, merged: MergedServer) -> Result<ResolvedLspServer, LspConfigError> {
	let command = merged
		.command
		.ok_or_else(|| LspConfigError::MissingField { server: name.clone(), field: "command" })?;
	let file_types = merged
		.file_types
		.ok_or_else(|| LspConfigError::MissingField { server: name.clone(), field: "fileTypes" })?;
	let root_markers = merged
		.root_markers
		.ok_or_else(|| LspConfigError::MissingField {
			server: name.clone(),
			field:  "rootMarkers",
		})?;
	if command.value.is_empty() || file_types.value.is_empty() || root_markers.value.is_empty() {
		return Err(LspConfigError::EmptyRequiredField { server: name });
	}
	let fallback = command.provenance.clone();
	Ok(ResolvedLspServer {
		name,
		command,
		args: merged
			.args
			.unwrap_or_else(|| sourced(Vec::new(), &fallback)),
		file_types,
		root_markers,
		language_id: merged
			.language_id
			.unwrap_or_else(|| sourced(None, &fallback)),
		init_options: merged
			.init_options
			.unwrap_or_else(|| sourced(Value::Object(Map::new()), &fallback)),
		settings: merged
			.settings
			.unwrap_or_else(|| sourced(Value::Object(Map::new()), &fallback)),
		capabilities: merged
			.capabilities
			.unwrap_or_else(|| sourced(Value::Object(Map::new()), &fallback)),
		priority: merged.priority.unwrap_or_else(|| sourced(0, &fallback)),
		is_linter: merged
			.is_linter
			.unwrap_or_else(|| sourced(false, &fallback)),
		disabled: merged.disabled.unwrap_or_else(|| sourced(false, &fallback)),
		warmup_timeout_ms: merged
			.warmup_timeout_ms
			.unwrap_or_else(|| sourced(10_000, &fallback)),
		idle_timeout_ms: merged
			.idle_timeout_ms
			.unwrap_or_else(|| sourced(None, &fallback)),
		readiness_timeout_ms: merged
			.readiness_timeout_ms
			.unwrap_or_else(|| sourced(30_000, &fallback)),
		env: merged
			.env
			.unwrap_or_else(|| sourced(BTreeMap::new(), &fallback)),
	})
}

/// Small resolved-config cache explicitly evicted during reload.
#[derive(Default)]
pub struct LspConfigCache {
	entries: Mutex<BTreeMap<PathBuf, Arc<ResolvedLspConfig>>>,
}

impl LspConfigCache {
	/// Returns a cached configuration.
	pub fn get(&self, workspace: &Path) -> Option<Arc<ResolvedLspConfig>> {
		self.entries.lock().get(workspace).cloned()
	}

	/// Stores one resolved configuration.
	pub fn insert(&self, workspace: PathBuf, config: Arc<ResolvedLspConfig>) {
		self.entries.lock().insert(workspace, config);
	}

	/// Evicts one workspace before reload.
	pub fn evict(&self, workspace: &Path) -> Option<Arc<ResolvedLspConfig>> {
		self.entries.lock().remove(workspace)
	}
}

/// Native LSP configuration failure.
#[derive(Debug, Error)]
pub enum LspConfigError {
	/// A source could not be read, or the contained reader refused it.
	#[error(transparent)]
	File(#[from] ProjectFileError),
	/// A source exceeds the byte bound.
	#[error("LSP configuration {} exceeds {limit} bytes", path.display())]
	TooLarge {
		/// Oversized source path.
		path:  PathBuf,
		/// Maximum accepted bytes.
		limit: u64,
	},
	/// JSON parsing failed.
	#[error("invalid JSON LSP configuration {source_name}: {source}")]
	ParseJson {
		/// Source identity.
		source_name: Str,
		/// JSON decoder failure.
		#[source]
		source:      serde_json::Error,
	},
	/// YAML parsing failed.
	#[error("invalid YAML LSP configuration {source_name}: {source}")]
	ParseYaml {
		/// Source identity.
		source_name: Str,
		/// YAML decoder failure.
		#[source]
		source:      serde_yaml::Error,
	},
	/// The expanded structure exceeded depth/node bounds.
	#[error("LSP configuration exceeds structural bounds")]
	StructureLimit,
	/// Top-level configuration must be an object.
	#[error("LSP configuration must be an object")]
	TopLevelObject,
	/// `servers` must be an object.
	#[error("LSP configuration servers field must be an object")]
	ServersObject,
	/// A manifest command was neither materialized nor explicitly granted.
	#[error("manifest LSP server {server} references undeclared command {command}")]
	UndeclaredManifestCommand {
		/// Server declaration name.
		server:  Str,
		/// Rejected command.
		command: Str,
	},
	/// A top-level setting had the wrong type.
	#[error("invalid LSP configuration document: {source}")]
	InvalidDocument {
		/// Schema decoder failure.
		#[source]
		source: serde_json::Error,
	},
	/// A server declaration had the wrong shape.
	#[error("invalid LSP server {server}: {source}")]
	InvalidServer {
		/// Server name.
		server: Str,
		/// Schema decoder failure.
		#[source]
		source: serde_json::Error,
	},
	/// A required field was absent after merging.
	#[error("LSP server {server} is missing {field}")]
	MissingField {
		/// Server name.
		server: Str,
		/// Missing field name.
		field:  &'static str,
	},
	/// A required field was present but empty.
	#[error("LSP server {server} has an empty required field")]
	EmptyRequiredField {
		/// Server name.
		server: Str,
	},
}

#[cfg(test)]
pub(crate) mod tests {
	use std::fs;

	use omp_ext::{
		claude_plugin::{
			ClaudePlugins, InstallScope, InstalledPluginEntry, InstalledPluginsRegistry,
		},
		workspace_trust::inventory::lsp_config_files,
	};

	use super::*;

	/// Project LSP files the reader must not follow or block on: an outside
	/// symlink and a FIFO are skipped, an inside symlink loads, and the
	/// bundled and user sources survive.
	#[cfg(unix)]
	#[test]
	fn project_lsp_files_outside_the_repository_or_special_are_skipped() {
		use std::{os::unix::fs::symlink, process::Command, sync::mpsc, thread, time::Duration};

		let temp = tempfile::tempdir().unwrap();
		let project = temp.path().join("project");
		fs::create_dir_all(project.join(".git")).unwrap();
		fs::create_dir_all(project.join(".omp")).unwrap();
		let body =
			r#"{"servers":{"acme":{"command":"acme","fileTypes":[".a"],"rootMarkers":["."]}}}"#;
		write(&temp.path().join("outside.json"), body);
		symlink(temp.path().join("outside.json"), project.join(".omp/lsp.json")).unwrap();
		assert!(
			Command::new("mkfifo")
				.arg(project.join(".lsp.json"))
				.status()
				.unwrap()
				.success()
		);
		write(&project.join("real/lsp.json"), body);
		symlink("real/lsp.json", project.join("lsp.json")).unwrap();
		let (sender, receiver) = mpsc::channel();
		let root = project.clone();
		thread::spawn(move || {
			let _ = sender.send(discover_lsp_sources(None, &root, Vec::new(), &[]));
		});
		let found = receiver
			.recv_timeout(Duration::from_secs(10))
			.expect("a FIFO must be refused, not read")
			.unwrap();
		let project_sources = found
			.sources
			.iter()
			.filter(|source| {
				matches!(
					source.provenance.kind,
					LspConfigSourceKind::Project | LspConfigSourceKind::Dotfile
				)
			})
			.map(|source| source.provenance.source.to_string())
			.collect::<Vec<_>>();
		assert_eq!(project_sources, [project.join("lsp.json").to_string_lossy()]);
		assert!(
			load_lsp_config(&found.sources)
				.unwrap()
				.servers
				.contains_key("acme")
		);
	}

	/// The project files the loader reads are exactly the gated inventory's
	/// ([`lsp_config_files`]), in the loader's order, so no project LSP file
	/// loads outside the workspace trust digest.
	#[test]
	fn project_lsp_sources_are_exactly_the_gated_inventory_files() {
		let temp = tempfile::tempdir().unwrap();
		let project = temp.path().join("project");
		fs::create_dir_all(project.join(".git")).unwrap();
		let gated = lsp_config_files()
			.map(|file| file.under(&project))
			.collect::<Vec<_>>();
		for path in &gated {
			write(path, "{}");
		}
		for near_miss in
			["lsp.jsonc", ".omp/lsp.toml", ".omp/lsp/lsp.json", ".omp/.lsp.yml.bak", "dap.json"]
		{
			write(&project.join(near_miss), "{}");
		}
		let read = discover_lsp_sources(None, &project, Vec::new(), &[])
			.unwrap()
			.sources
			.iter()
			.filter(|source| {
				matches!(
					source.provenance.kind,
					LspConfigSourceKind::Project | LspConfigSourceKind::Dotfile
				)
			})
			.map(|source| PathBuf::from(source.provenance.source.as_str()))
			.collect::<Vec<_>>();
		assert_eq!(read, gated);
	}

	pub fn write(path: &Path, body: &str) {
		fs::create_dir_all(path.parent().unwrap()).unwrap();
		fs::write(path, body).unwrap();
	}

	/// Records `(id, root, enabled)` installs in a user registry under
	/// `data` and resolves them the way the composition does, with every
	/// plugin launch approved.
	pub fn installed_plugins(data: &Path, installs: &[(&str, &Path, bool)]) -> ClaudePlugins {
		let mut registry = InstalledPluginsRegistry::default();
		for (id, root, enabled) in installs {
			registry
				.plugins
				.insert(Str::new(id), vec![InstalledPluginEntry {
					scope:          InstallScope::User,
					install_path:   root.to_path_buf(),
					version:        Str::new_static("1.0.0"),
					installed_at:   Str::new_static("2026-01-01T00:00:00Z"),
					last_updated:   Str::new_static("2026-01-01T00:00:00Z"),
					git_commit_sha: None,
					enabled:        *enabled,
				}]);
		}
		write(
			&data.join("plugins/installed_plugins.json"),
			&serde_json::to_string(&registry).unwrap(),
		);
		let mut plugins = ClaudePlugins::resolve(data, data, None);
		crate::plugin_commands::approve_all(&mut plugins.plugins);
		plugins
	}

	#[test]
	fn enabled_plugin_servers_load_with_the_root_expanded_and_disabled_ones_never_do() {
		let temp = tempfile::tempdir().unwrap();
		let project = temp.path().join("project");
		fs::create_dir_all(&project).unwrap();
		let on = temp.path().join("on");
		let off = temp.path().join("off");
		write(
			&on.join(".lsp.json"),
			r#"{"acme":{
				"command":"${CLAUDE_PLUGIN_ROOT}/bin/acme-lsp",
				"args":["--home=${CLAUDE_PLUGIN_ROOT}","--stdio"],
				"env":{"ACME_HOME":"${CLAUDE_PLUGIN_ROOT}/share"},
				"extensionToLanguage":{".acme":"acme"}
			}}"#,
		);
		write(
			&on.join(".claude-plugin/plugin.json"),
			r#"{"name":"on","lspServers":{"zig":{
				"command":"./bin/zls","fileTypes":[".zig"],"rootMarkers":["build.zig"]
			}}}"#,
		);
		write(
			&off.join(".lsp.json"),
			r#"{"ghost":{"command":"ghost","extensionToLanguage":{".g":"g"}}}"#,
		);
		let plugins = installed_plugins(&temp.path().join("data"), &[
			("on@m", &on, true),
			("off@m", &off, false),
		]);
		assert!(plugins.diagnostics.is_empty(), "{:?}", plugins.diagnostics);

		let found = discover_lsp_sources(None, &project, Vec::new(), &plugins.plugins).unwrap();
		assert!(found.diagnostics.is_empty(), "{:?}", found.diagnostics);
		let config = load_lsp_config(&found.sources).unwrap();

		let root = fs::canonicalize(&on).unwrap();
		let root = root.to_string_lossy();
		let acme = &config.servers["acme"];
		assert_eq!(acme.command.value, format!("{root}/bin/acme-lsp").as_str());
		assert_eq!(acme.command.provenance.kind, LspConfigSourceKind::Plugin);
		assert_eq!(acme.args.value, [format!("--home={root}").as_str(), "--stdio"]);
		assert_eq!(acme.env.value["ACME_HOME"], format!("{root}/share").as_str());
		assert_eq!(acme.file_types.value, [".acme"]);
		assert_eq!(acme.root_markers.value, ["."]);
		let process = acme.to_process_config();
		assert_eq!(process.env["ACME_HOME"], format!("{root}/share").as_str());
		// Manifest inline `lspServers`: a path-like command roots at the plugin.
		let zig = &config.servers["zig"];
		assert_eq!(zig.command.value, format!("{root}/bin/zls").as_str());
		assert_eq!(zig.command.provenance.kind, LspConfigSourceKind::Plugin);
		assert!(!config.servers.contains_key("ghost"), "a disabled plugin contributed");
	}

	#[test]
	fn unapproved_plugin_launches_are_left_out_with_a_diagnostic() {
		let temp = tempfile::tempdir().unwrap();
		let project = temp.path().join("project");
		fs::create_dir_all(&project).unwrap();
		let plugin = temp.path().join("plugin");
		write(
			&plugin.join("lsp.yaml"),
			"servers:\n  acme:\n    command: ${CLAUDE_PLUGIN_ROOT}/bin/acme\n    args: [--stdio]\n    \
			 fileTypes: [.acme]\n    rootMarkers: [.]\n  trusted:\n    command: trusted-lsp\n    \
			 fileTypes: [.t]\n    rootMarkers: [.]\n  rust-analyzer:\n    settings: {check: clippy}\n",
		);
		let mut plugins = installed_plugins(&temp.path().join("data"), &[("p@m", &plugin, true)]);
		// Keep only `trusted` approved.
		let plugin_entry = &mut plugins.plugins[0];
		let trusted = crate::plugin_commands::plugin_launches(plugin_entry)
			.into_iter()
			.find(|launch| launch.server == "trusted")
			.unwrap();
		plugin_entry.approved_commands = [plugin_entry.command_digest(&trusted)].into();

		let found = discover_lsp_sources(None, &project, Vec::new(), &plugins.plugins).unwrap();
		let config = load_lsp_config(&found.sources).unwrap();

		assert!(config.servers.contains_key("trusted"));
		assert!(!config.servers.contains_key("acme"), "an unapproved server loaded");
		// A declaration that launches nothing needs no approval.
		assert_eq!(config.servers["rust-analyzer"].settings.value["check"], "clippy");
		let [PluginDiagnostic::CommandNotApproved(blocked)] = &found.diagnostics[..] else {
			panic!("{:?}", found.diagnostics);
		};
		let root = fs::canonicalize(&plugin).unwrap();
		assert_eq!(blocked.plugin, "p@m");
		assert_eq!(blocked.server, "acme");
		assert_eq!(*blocked.command(), root.join("bin/acme").to_string_lossy().as_ref());
		assert_eq!(blocked.args(), ["--stdio"]);
	}

	#[test]
	fn user_and_project_declarations_override_a_plugin_server_of_the_same_name() {
		let temp = tempfile::tempdir().unwrap();
		let project = temp.path().join("project");
		let user = temp.path().join("user");
		let plugin = temp.path().join("plugin");
		write(
			&plugin.join(".lsp.json"),
			r#"{"servers":{
				"acme":{"command":"plugin-acme","args":["--plugin"],"fileTypes":[".acme"],"rootMarkers":["."]},
				"rust-analyzer":{"args":["--from-plugin"]}
			}}"#,
		);
		write(&user.join("lsp.json"), r#"{"acme":{"command":"user-acme"}}"#);
		write(&project.join(".omp/lsp.json"), r#"{"rust-analyzer":{"args":["--from-project"]}}"#);
		let plugins = installed_plugins(&temp.path().join("data"), &[("p@m", &plugin, true)]);

		let found =
			discover_lsp_sources(Some(&user), &project, Vec::new(), &plugins.plugins).unwrap();
		let config = load_lsp_config(&found.sources).unwrap();

		let acme = &config.servers["acme"];
		assert_eq!(acme.command.value, "user-acme");
		assert_eq!(acme.command.provenance.kind, LspConfigSourceKind::User);
		assert_eq!(acme.args.value, ["--plugin"]);
		assert_eq!(acme.args.provenance.kind, LspConfigSourceKind::Plugin);
		let rust = &config.servers["rust-analyzer"];
		assert_eq!(rust.args.value, ["--from-project"]);
		assert_eq!(rust.args.provenance.kind, LspConfigSourceKind::Project);
		assert_eq!(rust.command.provenance.kind, LspConfigSourceKind::Builtin);
	}

	#[test]
	fn an_invalid_plugin_declaration_is_a_diagnostic_and_the_rest_still_load() {
		let temp = tempfile::tempdir().unwrap();
		let project = temp.path().join("project");
		let broken = temp.path().join("broken");
		let unknown = temp.path().join("unknown");
		let good = temp.path().join("good");
		write(&broken.join(".lsp.json"), "{ not json");
		write(
			&unknown.join("lsp.yaml"),
			"acme:\n  command: acme\n  fileTypes: [.a]\n  rootMarkers: [.]\n  bogus: 1\n",
		);
		write(
			&good.join(".lsp.json"),
			r#"{"good":{"command":"good","extensionToLanguage":{".g":"g"}}}"#,
		);
		let plugins = installed_plugins(&temp.path().join("data"), &[
			("broken@m", &broken, true),
			("unknown@m", &unknown, true),
			("good@m", &good, true),
		]);

		let found = discover_lsp_sources(None, &project, Vec::new(), &plugins.plugins).unwrap();
		let config = load_lsp_config(&found.sources).unwrap();

		assert!(config.servers.contains_key("good"));
		assert!(!config.servers.contains_key("acme"));
		let mut rejected = found
			.diagnostics
			.iter()
			.map(|diagnostic| match diagnostic {
				PluginDiagnostic::InvalidComponent {
					plugin,
					component: PluginComponent::LspServers,
					path,
					source,
				} => {
					let error = source
						.downcast_ref::<LspConfigError>()
						.expect("typed LSP error");
					let kind = match error {
						LspConfigError::ParseJson { .. } => "json",
						LspConfigError::InvalidServer { .. } => "schema",
						other => panic!("unexpected {other:?}"),
					};
					(plugin.to_string(), path.file_name().unwrap().to_owned(), kind)
				},
				other => panic!("unexpected {other:?}"),
			})
			.collect::<Vec<_>>();
		rejected.sort();
		assert_eq!(rejected, [
			("broken@m".to_owned(), ".lsp.json".into(), "json"),
			("unknown@m".to_owned(), "lsp.yaml".into(), "schema"),
		]);
	}

	#[test]
	fn bundled_catalog_is_complete_and_preserves_pi_fields() {
		let config = load_lsp_config(&[bundled_lsp_defaults().unwrap()]).unwrap();
		assert!(config.servers.len() >= 45);
		let rust = &config.servers["rust-analyzer"];
		assert_eq!(rust.command.value, "rust-analyzer");
		assert!(
			rust
				.root_markers
				.value
				.iter()
				.any(|marker| marker == "Cargo.toml")
		);
		assert_eq!(config.servers["swiftlint"].is_linter.value, true);
		assert_eq!(config.servers["omnisharp"].args.value[2], "$PID");
	}

	#[test]
	fn yaml_override_merges_fields_and_stamps_provenance() {
		let defaults = bundled_lsp_defaults().unwrap();
		let project = LspConfigSource {
			provenance:  LspConfigProvenance {
				kind:   LspConfigSourceKind::Project,
				source: Str::new_static("fixture"),
			},
			bytes:       Arc::from(
				&b"servers:\n  rust-analyzer:\n    disabled: true\n    warmupTimeoutMs: 321\n"[..],
			),
			yaml:        true,
			plugin_root: None,
		};
		let config = load_lsp_config(&[defaults, project]).unwrap();
		let rust = &config.servers["rust-analyzer"];
		assert!(rust.disabled.value);
		assert_eq!(rust.warmup_timeout_ms.value, 321);
		assert_eq!(rust.command.provenance.kind, LspConfigSourceKind::Builtin);
		assert_eq!(rust.disabled.provenance.kind, LspConfigSourceKind::Project);
	}

	#[test]
	fn manifest_commands_require_declared_executables() {
		let bytes = br#"{"servers":{"acme":{"command":"acme-lsp","fileTypes":["rs"],"rootMarkers":["Cargo.toml"]}}}"#;
		assert!(LspConfigSource::manifest_checked("acme:lsp", bytes.as_slice(), false, []).is_err());
		let source =
			LspConfigSource::manifest_checked("acme:lsp", bytes.as_slice(), false, [Str::new_static(
				"acme-lsp",
			)])
			.unwrap();
		let config = load_lsp_config(&[source]).unwrap();
		assert_eq!(config.servers["acme"].command.provenance.kind, LspConfigSourceKind::Manifest);
	}
}
