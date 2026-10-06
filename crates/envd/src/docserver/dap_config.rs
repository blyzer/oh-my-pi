//! Native DAP adapter discovery and provenance-preserving field merges.

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
};
use serde::Deserialize;
use serde_json::Map;

use crate::docserver::dap_adapter::{
	DapAdapterError, DapAdapterSpec, DapTransport, builtin_adapters,
};

const MAX_CONFIG_BYTES: u64 = 1024 * 1024;
const CONFIG_NAMES: [&str; 6] =
	["dap.json", ".dap.json", "dap.yaml", ".dap.yaml", "dap.yml", ".dap.yml"];

/// Native DAP declaration origin.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DapConfigSourceKind {
	/// Built-in adapter catalog.
	Builtin,
	/// User OMP configuration.
	User,
	/// Project OMP configuration.
	Project,
	/// Project-root dotfile.
	Dotfile,
	/// Validated native extension contribution.
	Manifest,
	/// Installed, enabled Claude-format marketplace plugin declaration (root
	/// `.dap.json` family), validated on its own before it joins the merge.
	Plugin,
}

/// Exact source retained on resolved fields.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DapConfigProvenance {
	/// Source class.
	pub kind:   DapConfigSourceKind,
	/// Path or manifest identity.
	pub source: Str,
}

/// One source-annotated value.
#[derive(Clone, Debug, PartialEq)]
pub struct DapProvenanced<T> {
	/// Winning value.
	pub value:      T,
	/// Winning declaration.
	pub provenance: DapConfigProvenance,
}

/// Ordered native configuration input.
#[derive(Clone, Debug)]
pub struct DapConfigSource {
	/// Input identity.
	pub provenance:  DapConfigProvenance,
	/// Input bytes.
	pub bytes:       Arc<[u8]>,
	/// YAML rather than JSON.
	pub yaml:        bool,
	/// Installed plugin root for a [`DapConfigSourceKind::Plugin`] source:
	/// `${CLAUDE_PLUGIN_ROOT}` in `command` and `args` expands to it and a
	/// path-like relative `command` resolves against it.
	pub plugin_root: Option<Arc<Path>>,
}

impl DapConfigSource {
	/// Reads one bounded source.
	pub fn read(kind: DapConfigSourceKind, path: &Path) -> Result<Self, DapConfigError> {
		Self::read_contained(kind, path, Containment::Unconfined)?.ok_or_else(|| {
			ProjectFileError::Io { path: path.to_owned(), source: io::ErrorKind::NotFound.into() }
				.into()
		})
	}

	/// [`DapConfigSource::read`] through the contained project-file reader:
	/// `None` when `path` is absent, an error when it is not a regular file
	/// of at most one MiB resolving inside `containment`.
	pub fn read_contained(
		kind: DapConfigSourceKind,
		path: &Path,
		containment: Containment<'_>,
	) -> Result<Option<Self>, DapConfigError> {
		let Some(bytes) = project_file::read_bytes(path, containment, MAX_CONFIG_BYTES)? else {
			return Ok(None);
		};
		Ok(Some(Self {
			provenance:  DapConfigProvenance { kind, source: Str::new(path.to_string_lossy()) },
			yaml:        matches!(
				path.extension().and_then(|value| value.to_str()),
				Some("yaml" | "yml")
			),
			bytes:       bytes.into(),
			plugin_root: None,
		}))
	}

	/// Reads one installed plugin's declaration: a root declaration file
	/// (YAML by extension) or an inline adapter map.
	pub fn plugin(
		plugin: &ClaudePlugin,
		declaration: &ConfigDeclaration,
	) -> Result<Self, DapConfigError> {
		let mut source = match declaration {
			ConfigDeclaration::File(path) => Self::read(DapConfigSourceKind::Plugin, path)?,
			ConfigDeclaration::Inline { manifest, servers } => Self {
				provenance:  DapConfigProvenance {
					kind:   DapConfigSourceKind::Plugin,
					source: Str::new(manifest.to_string_lossy()),
				},
				bytes:       format!(r#"{{"adapters":{servers}}}"#).into_bytes().into(),
				yaml:        false,
				plugin_root: None,
			},
		};
		source.plugin_root = Some(Arc::from(plugin.root.as_path()));
		Ok(source)
	}

	/// Creates a contribution from a validated native extension manifest.
	pub fn manifest(identity: impl AsRef<str>, bytes: impl Into<Arc<[u8]>>, yaml: bool) -> Self {
		Self {
			provenance: DapConfigProvenance {
				kind:   DapConfigSourceKind::Manifest,
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
	) -> Result<Self, DapConfigError> {
		let source = Self::manifest(identity, bytes, yaml);
		let allowed = allowed_commands
			.into_iter()
			.collect::<std::collections::BTreeSet<_>>();
		let resolved = load_dap_config([], std::slice::from_ref(&source))?;
		for adapter in resolved.values() {
			let command = &adapter.command.value;
			if command.contains('/') || command.contains('\\') || !allowed.contains(command) {
				return Err(DapConfigError::UndeclaredManifestCommand {
					adapter: adapter.name.clone(),
					command: command.clone(),
				});
			}
		}
		Ok(source)
	}
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
struct DapAdapterPatch {
	command: Option<Str>,
	args: Option<Vec<Str>>,
	languages: Option<Vec<Str>>,
	file_types: Option<Vec<Str>>,
	root_markers: Option<Vec<Str>>,
	launch_defaults: Option<Map<String, serde_json::Value>>,
	attach_defaults: Option<Map<String, serde_json::Value>>,
	accepts_directory_program: Option<bool>,
	connect_mode: Option<Str>,
	preference: Option<u16>,
}

impl DapAdapterPatch {
	/// Expands `${CLAUDE_PLUGIN_ROOT}` (and its aliases) in `command` and
	/// `args`, then roots a path-like relative `command` at the plugin.
	fn expand_plugin_root(&mut self, root: &Path) {
		if let Some(command) = self.command.take() {
			self.command = Some(resolve_plugin_command(expand_plugin_vars(command, root, None), root));
		}
		for arg in self.args.iter_mut().flatten() {
			*arg = expand_plugin_vars(arg.clone(), root, None);
		}
	}
}

#[derive(Default)]
struct MergedAdapter {
	command: Option<DapProvenanced<Str>>,
	args: Option<DapProvenanced<Vec<Str>>>,
	languages: Option<DapProvenanced<Vec<Str>>>,
	file_types: Option<DapProvenanced<Vec<Str>>>,
	root_markers: Option<DapProvenanced<Vec<Str>>>,
	launch_defaults: Option<DapProvenanced<Map<String, serde_json::Value>>>,
	attach_defaults: Option<DapProvenanced<Map<String, serde_json::Value>>>,
	accepts_directory_program: Option<DapProvenanced<bool>>,
	connect_mode: Option<DapProvenanced<Option<Str>>>,
	preference: Option<DapProvenanced<u16>>,
}

/// Resolved adapter plus per-field provenance.
#[derive(Clone, Debug, PartialEq)]
pub struct ResolvedDapAdapter {
	/// Adapter name.
	pub name: Str,
	/// Command.
	pub command: DapProvenanced<Str>,
	/// Arguments.
	pub args: DapProvenanced<Vec<Str>>,
	/// Language identifiers.
	pub languages: DapProvenanced<Vec<Str>>,
	/// Extensions/exact filenames.
	pub file_types: DapProvenanced<Vec<Str>>,
	/// Project markers.
	pub root_markers: DapProvenanced<Vec<Str>>,
	/// Launch defaults.
	pub launch_defaults: DapProvenanced<Map<String, serde_json::Value>>,
	/// Attach defaults.
	/// `skipAttachRequest: true` marks an adapter that connected before DAP
	/// startup.
	pub attach_defaults: DapProvenanced<Map<String, serde_json::Value>>,
	/// Directory launch support.
	pub accepts_directory_program: DapProvenanced<bool>,
	/// Optional socket/TCP connection mode.
	pub connect_mode: DapProvenanced<Option<Str>>,
	/// Selection preference.
	pub preference: DapProvenanced<u16>,
}

impl ResolvedDapAdapter {
	/// Converts the resolved declaration into the runtime registry shape.
	pub fn to_spec(&self) -> Result<DapAdapterSpec, DapConfigError> {
		let mut spec = DapAdapterSpec::new(self.name.as_str(), self.command.value.as_str())?;
		spec.args = self.args.value.clone();
		spec.extensions = self
			.file_types
			.value
			.iter()
			.map(|value| Str::new(value.trim_start_matches('.')))
			.collect();
		spec.root_markers = self.root_markers.value.clone();
		spec.accepts_directory_program = self.accepts_directory_program.value;
		spec.launch_defaults = self.launch_defaults.value.clone();
		spec.attach_defaults = self.attach_defaults.value.clone();
		spec.preference = self.preference.value;
		spec.transport = match self.connect_mode.value.as_deref() {
			Some("tcp") => DapTransport::Tcp { port_argument: Str::new_static("${port}") },
			Some("socket" | "unix") => {
				DapTransport::Unix { socket_argument: Str::new_static("${socket}") }
			},
			Some(mode) => {
				return Err(DapConfigError::InvalidConnectMode {
					adapter: self.name.clone(),
					mode:    Str::new(mode),
				});
			},
			None => DapTransport::Stdio,
		};
		Ok(spec)
	}
}

/// Discovers only native user/project DAP files, low to high precedence.
/// Foreign roots are never considered.
pub fn discover_native_dap_sources(
	user_root: Option<&Path>,
	project_root: &Path,
) -> Result<Vec<DapConfigSource>, DapConfigError> {
	discover_dap_sources(user_root, project_root, Vec::new(), &[]).map(|found| found.sources)
}

/// Ordered sources plus the plugin declarations that did not load.
#[derive(Debug)]
pub struct DiscoveredDapSources {
	/// Sources from low to high precedence; built-ins merge beneath them.
	pub sources:     Vec<DapConfigSource>,
	/// One diagnostic per rejected plugin declaration.
	pub diagnostics: Vec<PluginDiagnostic>,
}

/// Enumerates extension-manifest, installed-plugin, user, and project
/// sources in increasing precedence; built-ins are merged before this
/// returned list.
///
/// The only non-native roots ever read are the `plugins` handed in: the
/// installed, enabled marketplace plugins the composition resolved, each
/// contributing only the declarations its
/// [`ClaudeComponents::dap`](omp_ext::claude_plugin::ClaudeComponents::dap)
/// located inside its own root. Foreign configuration roots are never
/// probed. Plugin declarations sit below every user and project file (OMP
/// v1's order); a project-scope plugin overrides a user-scope one. Each
/// plugin declaration is validated on its own against the layers beneath it,
/// so an invalid one becomes a [`PluginDiagnostic::InvalidComponent`] instead
/// of discarding every override.
pub fn discover_dap_sources(
	user_root: Option<&Path>,
	project_root: &Path,
	mut manifests: Vec<DapConfigSource>,
	plugins: &[ClaudePlugin],
) -> Result<DiscoveredDapSources, DapConfigError> {
	let mut sources = Vec::new();
	manifests.sort_by(|left, right| left.provenance.source.cmp(&right.provenance.source));
	sources.extend(manifests);
	let mut diagnostics = Vec::new();
	append_plugin_sources(&mut sources, plugins, &mut diagnostics);
	let user = Containment::Unconfined;
	if let Some(user_root) = user_root {
		append_existing(&mut sources, user_root, DapConfigSourceKind::User, user)?;
		append_existing(&mut sources, &user_root.join("agent"), DapConfigSourceKind::User, user)?;
	}
	let project = Containment::Within(containment_root(project_root));
	append_existing(
		&mut sources,
		&project_root.join(".omp"),
		DapConfigSourceKind::Project,
		project,
	)?;
	append_existing(&mut sources, project_root, DapConfigSourceKind::Dotfile, project)?;
	Ok(DiscoveredDapSources { sources, diagnostics })
}

/// Appends every plugin declaration that merges and converts cleanly over
/// the built-ins and `sources`, user-scope plugins first so project-scope
/// ones win.
fn append_plugin_sources(
	sources: &mut Vec<DapConfigSource>,
	plugins: &[ClaudePlugin],
	diagnostics: &mut Vec<PluginDiagnostic>,
) {
	for plugin in plugins.iter().rev() {
		let Some(components) = plugin.claude_components() else {
			continue;
		};
		for declaration in &components.dap {
			let admitted = DapConfigSource::plugin(plugin, declaration)
				.and_then(|source| gate_plugin_source(plugin, source, diagnostics))
				.and_then(|source| {
					sources.push(source);
					load_dap_config(builtin_adapters(), sources)
						.and_then(|adapters| {
							adapters
								.values()
								.try_for_each(|adapter| adapter.to_spec().map(drop))
						})
						.inspect_err(|_| {
							sources.pop();
						})
				});
			if let Err(error) = admitted {
				diagnostics.push(PluginDiagnostic::InvalidComponent {
					plugin:    plugin.id.clone().into(),
					component: PluginComponent::DapAdapters,
					path:      declaration.path().to_path_buf(),
					source:    Box::new(error),
				});
			}
		}
	}
}

/// Appends each present config file of `directory`. A file the contained
/// reader refuses (outside the project, a special file, oversize) is skipped
/// with a warning rather than failing discovery: dropping a source can only
/// remove adapters.
fn append_existing(
	sources: &mut Vec<DapConfigSource>,
	directory: &Path,
	kind: DapConfigSourceKind,
	containment: Containment<'_>,
) -> Result<(), DapConfigError> {
	for name in CONFIG_NAMES {
		let path = directory.join(name);
		match DapConfigSource::read_contained(kind, &path, containment) {
			Ok(Some(source)) => sources.push(source),
			Ok(None) => {},
			Err(DapConfigError::File(error @ ProjectFileError::Refused { .. })) => {
				tracing::warn!(
					path = %error.path().display(),
					reason = error.refusal().map_or("unreadable", <&str>::from),
					"DAP configuration file skipped"
				);
			},
			Err(error) => return Err(error),
		}
	}
	Ok(())
}

/// Merges native adapter declarations per field. Object-valued launch/attach
/// defaults are themselves shallow-field merged.
pub fn load_dap_config(
	builtins: impl IntoIterator<Item = DapAdapterSpec>,
	sources: &[DapConfigSource],
) -> Result<BTreeMap<Str, ResolvedDapAdapter>, DapConfigError> {
	let builtin_provenance = DapConfigProvenance {
		kind:   DapConfigSourceKind::Builtin,
		source: Str::new_static("omp:dap-builtins"),
	};
	let mut merged = BTreeMap::new();
	for spec in builtins {
		let patch = DapAdapterPatch {
			command: Some(spec.command),
			args: Some(spec.args),
			languages: Some(Vec::new()),
			file_types: Some(spec.extensions),
			root_markers: Some(spec.root_markers),
			launch_defaults: Some(spec.launch_defaults),
			attach_defaults: Some(spec.attach_defaults),
			accepts_directory_program: Some(spec.accepts_directory_program),
			connect_mode: match spec.transport {
				DapTransport::Stdio => None,
				DapTransport::Tcp { .. } => Some(Str::new_static("tcp")),
				DapTransport::Unix { .. } => Some(Str::new_static("socket")),
			},
			preference: Some(spec.preference),
		};
		merge_adapter(merged.entry(spec.name).or_default(), patch, &builtin_provenance);
	}
	for source in sources {
		if source.bytes.len() as u64 > MAX_CONFIG_BYTES {
			return Err(DapConfigError::TooLarge {
				path: PathBuf::from(source.provenance.source.as_str()),
			});
		}
		for (name, patch) in source_adapters(source, parse_source(source)?)? {
			merge_adapter(merged.entry(name).or_default(), patch, &source.provenance);
		}
	}
	merged
		.into_iter()
		.map(|(name, adapter)| resolve_adapter(name.clone(), adapter).map(|adapter| (name, adapter)))
		.collect()
}

/// Parses one bounded source document.
fn parse_source(source: &DapConfigSource) -> Result<serde_json::Value, DapConfigError> {
	if source.yaml {
		serde_yaml::from_slice(&source.bytes).map_err(|error| DapConfigError::ParseYaml {
			source_name: source.provenance.source.clone(),
			source:      error,
		})
	} else {
		serde_json::from_slice(&source.bytes).map_err(|error| DapConfigError::ParseJson {
			source_name: source.provenance.source.clone(),
			source:      error,
		})
	}
}

/// The adapter patches of one parsed source document (the `adapters`
/// wrapper or a flat map), plugin-root expanded for a plugin source.
fn source_adapters(
	source: &DapConfigSource,
	document: serde_json::Value,
) -> Result<Vec<(Str, DapAdapterPatch)>, DapConfigError> {
	let serde_json::Value::Object(mut object) = document else {
		return Err(DapConfigError::TopLevelObject);
	};
	let adapters = match object.remove("adapters") {
		Some(serde_json::Value::Object(value)) => value,
		Some(_) => return Err(DapConfigError::AdaptersObject),
		None => object,
	};
	adapters
		.into_iter()
		.map(|(name, value)| {
			let mut patch: DapAdapterPatch =
				serde_json::from_value(value).map_err(|source_error| {
					DapConfigError::InvalidAdapter { adapter: Str::new(&name), source: source_error }
				})?;
			if let Some(root) = &source.plugin_root {
				patch.expand_plugin_root(root);
			}
			Ok((Str::new(name), patch))
		})
		.collect()
}

/// The parsed document of a plugin source plus, per adapter that sets its
/// `command` or `args`, the launch it declares after plugin-root expansion,
/// bound to the plugin files it names.
fn plugin_source_launches(
	source: &DapConfigSource,
) -> Result<(serde_json::Value, Vec<(Str, PluginLaunch)>), DapConfigError> {
	let document = parse_source(source)?;
	let launches = source_adapters(source, document.clone())?
		.into_iter()
		.filter_map(|(name, patch)| {
			if patch.command.is_none() && patch.args.is_none() {
				return None;
			}
			let launch = PluginLaunch::new(
				PluginLaunchKind::DebugAdapter,
				name.clone(),
				patch.command.unwrap_or_default(),
				patch.args.unwrap_or_default(),
				[],
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

/// Drops from one plugin declaration every adapter whose launch the operator
/// has not approved ([`ClaudePlugin::admit_launch`]), recording each refusal
/// as a [`PluginDiagnostic::CommandNotApproved`]; the approved adapters still
/// load.
fn gate_plugin_source(
	plugin: &ClaudePlugin,
	mut source: DapConfigSource,
	diagnostics: &mut Vec<PluginDiagnostic>,
) -> Result<DapConfigSource, DapConfigError> {
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
	// whose `adapters`, when present, is the adapter map.
	if let Some(object) = document.as_object_mut() {
		let adapters = if object.contains_key("adapters") {
			object
				.get_mut("adapters")
				.and_then(serde_json::Value::as_object_mut)
		} else {
			Some(object)
		};
		if let Some(adapters) = adapters {
			for name in &blocked {
				adapters.remove(name.as_str());
			}
		}
	}
	source.bytes = serde_json::to_vec(&document)
		.map_err(|error| DapConfigError::ParseJson {
			source_name: source.provenance.source.clone(),
			source:      error,
		})?
		.into();
	source.yaml = false;
	Ok(source)
}

/// Every process `plugin`'s debug-adapter declarations would launch, exactly
/// as discovery gates them; a declaration that does not parse contributes
/// nothing (it never loads).
pub(crate) fn plugin_dap_launches(plugin: &ClaudePlugin) -> Vec<PluginLaunch> {
	let Some(components) = plugin.claude_components() else {
		return Vec::new();
	};
	components
		.dap
		.iter()
		.filter_map(|declaration| {
			let source = DapConfigSource::plugin(plugin, declaration).ok()?;
			plugin_source_launches(&source).ok()
		})
		.flat_map(|(_, launches)| launches.into_iter().map(|(_, launch)| launch))
		.collect()
}

fn sourced<T>(value: T, provenance: &DapConfigProvenance) -> DapProvenanced<T> {
	DapProvenanced { value, provenance: provenance.clone() }
}
fn merge_adapter(
	target: &mut MergedAdapter,
	patch: DapAdapterPatch,
	provenance: &DapConfigProvenance,
) {
	if let Some(value) = patch.command {
		target.command = Some(sourced(value, provenance));
	}
	if let Some(value) = patch.args {
		target.args = Some(sourced(value, provenance));
	}
	if let Some(value) = patch.languages {
		target.languages = Some(sourced(value, provenance));
	}
	if let Some(value) = patch.file_types {
		target.file_types = Some(sourced(value, provenance));
	}
	if let Some(value) = patch.root_markers {
		target.root_markers = Some(sourced(value, provenance));
	}
	if let Some(value) = patch.launch_defaults {
		let mut merged = target
			.launch_defaults
			.as_ref()
			.map_or_else(Map::new, |prior| prior.value.clone());
		merged.extend(value);
		target.launch_defaults = Some(sourced(merged, provenance));
	}
	if let Some(value) = patch.attach_defaults {
		let mut merged = target
			.attach_defaults
			.as_ref()
			.map_or_else(Map::new, |prior| prior.value.clone());
		merged.extend(value);
		target.attach_defaults = Some(sourced(merged, provenance));
	}
	if let Some(value) = patch.accepts_directory_program {
		target.accepts_directory_program = Some(sourced(value, provenance));
	}
	if let Some(value) = patch.connect_mode {
		target.connect_mode = Some(sourced(Some(value), provenance));
	}
	if let Some(value) = patch.preference {
		target.preference = Some(sourced(value, provenance));
	}
}

fn resolve_adapter(
	name: Str,
	adapter: MergedAdapter,
) -> Result<ResolvedDapAdapter, DapConfigError> {
	let command = adapter
		.command
		.ok_or_else(|| DapConfigError::MissingCommand { adapter: name.clone() })?;
	if command.value.is_empty() {
		return Err(DapConfigError::MissingCommand { adapter: name });
	}
	let p = command.provenance.clone();
	Ok(ResolvedDapAdapter {
		name,
		command,
		args: adapter.args.unwrap_or_else(|| sourced(Vec::new(), &p)),
		languages: adapter.languages.unwrap_or_else(|| sourced(Vec::new(), &p)),
		file_types: adapter
			.file_types
			.unwrap_or_else(|| sourced(Vec::new(), &p)),
		root_markers: adapter
			.root_markers
			.unwrap_or_else(|| sourced(Vec::new(), &p)),
		launch_defaults: adapter
			.launch_defaults
			.unwrap_or_else(|| sourced(Map::new(), &p)),
		attach_defaults: adapter
			.attach_defaults
			.unwrap_or_else(|| sourced(Map::new(), &p)),
		accepts_directory_program: adapter
			.accepts_directory_program
			.unwrap_or_else(|| sourced(false, &p)),
		connect_mode: adapter.connect_mode.unwrap_or_else(|| sourced(None, &p)),
		preference: adapter.preference.unwrap_or_else(|| sourced(u16::MAX, &p)),
	})
}

/// Native DAP configuration failure.
#[derive(Debug, thiserror::Error)]
pub enum DapConfigError {
	/// A source could not be read, or the contained reader refused it.
	#[error(transparent)]
	File(#[from] ProjectFileError),
	/// Byte bound exceeded.
	#[error("DAP configuration {} exceeds its byte bound", path.display())]
	TooLarge {
		/// Oversized source path.
		path: PathBuf,
	},
	/// Invalid JSON.
	#[error("invalid JSON DAP configuration {source_name}: {source}")]
	ParseJson {
		/// Source identity.
		source_name: Str,
		/// JSON decoder failure.
		#[source]
		source:      serde_json::Error,
	},
	/// Invalid YAML.
	#[error("invalid YAML DAP configuration {source_name}: {source}")]
	ParseYaml {
		/// Source identity.
		source_name: Str,
		/// YAML decoder failure.
		#[source]
		source:      serde_yaml::Error,
	},
	/// Wrong top-level shape.
	#[error("DAP configuration must be an object")]
	TopLevelObject,
	/// Wrong adapters shape.
	#[error("DAP adapters field must be an object")]
	AdaptersObject,
	/// A manifest command was neither materialized nor explicitly granted.
	#[error("manifest DAP adapter {adapter} references undeclared command {command}")]
	UndeclaredManifestCommand {
		/// Adapter declaration name.
		adapter: Str,
		/// Rejected command.
		command: Str,
	},
	/// Invalid adapter declaration.
	#[error("invalid DAP adapter {adapter}: {source}")]
	InvalidAdapter {
		/// Adapter name.
		adapter: Str,
		/// Schema decoder failure.
		#[source]
		source:  serde_json::Error,
	},
	/// Required command absent.
	#[error("DAP adapter {adapter} is missing a command")]
	MissingCommand {
		/// Adapter name.
		adapter: Str,
	},
	/// Unsupported connection mode.
	#[error("DAP adapter {adapter} has unsupported connect mode {mode}")]
	InvalidConnectMode {
		/// Adapter name.
		adapter: Str,
		/// Rejected mode.
		mode:    Str,
	},
	/// Runtime declaration validation failed.
	#[error(transparent)]
	Adapter(#[from] DapAdapterError),
}

#[cfg(test)]
mod tests {

	use std::{fs, iter::empty};

	use super::*;

	/// A project DAP file that is a symlink out of the repository or an
	/// oversize file is skipped, not read; an inside symlink loads.
	#[cfg(unix)]
	#[test]
	fn project_dap_files_outside_the_repository_or_oversize_are_skipped() {
		use std::os::unix::fs::symlink;

		use crate::docserver::lsp_config::tests::write;

		let temp = tempfile::tempdir().unwrap();
		let project = temp.path().join("project");
		fs::create_dir_all(project.join(".git")).unwrap();
		fs::create_dir_all(project.join(".omp")).unwrap();
		let body = r#"{"adapters":{"acme":{"command":"acme-dbg"}}}"#;
		write(&temp.path().join("outside.json"), body);
		symlink(temp.path().join("outside.json"), project.join(".omp/dap.json")).unwrap();
		write(&project.join("real/dap.json"), body);
		symlink("real/dap.json", project.join("dap.json")).unwrap();
		let oversize = vec![b' '; usize::try_from(MAX_CONFIG_BYTES).unwrap() + 1];
		fs::write(project.join(".dap.json"), oversize).unwrap();

		let found = discover_dap_sources(None, &project, Vec::new(), &[]).unwrap();
		let sources = found
			.sources
			.iter()
			.map(|source| source.provenance.source.to_string())
			.collect::<Vec<_>>();
		assert_eq!(sources, [project.join("dap.json").to_string_lossy()]);
	}

	#[test]
	fn yaml_field_merge_preserves_object_members_and_provenance() {
		let source = DapConfigSource {
			provenance:  DapConfigProvenance {
				kind:   DapConfigSourceKind::Project,
				source: Str::new_static("fixture"),
			},
			bytes:       Arc::from(
				&b"adapters:\n  debugpy:\n    launchDefaults:\n      stopOnEntry: false\n"[..],
			),
			yaml:        true,
			plugin_root: None,
		};
		let adapters = load_dap_config(builtin_adapters(), &[source]).unwrap();
		let debugpy = &adapters["debugpy"];
		assert_eq!(debugpy.launch_defaults.value["request"], "launch");
		assert_eq!(debugpy.launch_defaults.value["stopOnEntry"], false);
		assert_eq!(debugpy.launch_defaults.provenance.kind, DapConfigSourceKind::Project);
	}

	#[test]
	fn preattached_option_is_preserved_in_adapter_attach_defaults() {
		let source = DapConfigSource {
			provenance:  DapConfigProvenance {
				kind:   DapConfigSourceKind::Project,
				source: Str::new_static("fixture"),
			},
			bytes:       Arc::from(
				&br#"{
					"adapters": {
						"pico-openocd": {
							"command": "gdb",
							"attachDefaults": {
								"request": "attach",
								"skipAttachRequest": true
							}
						}
					}
				}"#[..],
			),
			yaml:        false,
			plugin_root: None,
		};
		let adapters = load_dap_config(empty::<DapAdapterSpec>(), &[source]).unwrap();
		let adapter = adapters["pico-openocd"].to_spec().unwrap();

		assert!(adapter.skip_attach_request());
		assert_eq!(adapter.merged_arguments(true, &Map::new())["skipAttachRequest"], true);
	}

	#[test]
	fn enabled_plugin_adapters_load_below_user_and_disabled_or_invalid_ones_do_not() {
		use crate::docserver::lsp_config::tests::{installed_plugins, write};

		let temp = tempfile::tempdir().unwrap();
		let project = temp.path().join("project");
		let user = temp.path().join("user");
		let on = temp.path().join("on");
		let off = temp.path().join("off");
		let broken = temp.path().join("broken");
		write(
			&on.join(".dap.json"),
			r#"{"adapters":{
				"acme-dbg":{"command":"${CLAUDE_PLUGIN_ROOT}/bin/dbg","args":["--data=${CLAUDE_PLUGIN_ROOT}/d"],"fileTypes":[".acme"]},
				"relative-dbg":{"command":"./bin/rel"},
				"shadowed":{"command":"plugin-shadowed","args":["--plugin"]}
			}}"#,
		);
		write(&off.join(".dap.json"), r#"{"ghost-dbg":{"command":"ghost"}}"#);
		write(&broken.join("dap.yaml"), "wrong:\n  command: x\n  connectMode: carrier-pigeon\n");
		write(&user.join("dap.json"), r#"{"shadowed":{"command":"user-shadowed"}}"#);
		let plugins = installed_plugins(&temp.path().join("data"), &[
			("on@m", &on, true),
			("off@m", &off, false),
			("broken@m", &broken, true),
		]);

		let found =
			discover_dap_sources(Some(&user), &project, Vec::new(), &plugins.plugins).unwrap();
		let adapters = load_dap_config(builtin_adapters(), &found.sources).unwrap();

		let root = fs::canonicalize(&on).unwrap();
		let root = root.to_string_lossy();
		let acme = &adapters["acme-dbg"];
		assert_eq!(acme.command.value, format!("{root}/bin/dbg").as_str());
		assert_eq!(acme.args.value, [format!("--data={root}/d").as_str()]);
		assert_eq!(acme.command.provenance.kind, DapConfigSourceKind::Plugin);
		assert_eq!(adapters["relative-dbg"].command.value, format!("{root}/bin/rel").as_str());
		let shadowed = &adapters["shadowed"];
		assert_eq!(shadowed.command.value, "user-shadowed");
		assert_eq!(shadowed.command.provenance.kind, DapConfigSourceKind::User);
		assert_eq!(shadowed.args.provenance.kind, DapConfigSourceKind::Plugin);
		assert!(!adapters.contains_key("ghost-dbg"), "a disabled plugin contributed");
		assert!(!adapters.contains_key("wrong"), "an invalid declaration loaded");
		assert!(
			matches!(found.diagnostics.as_slice(), [PluginDiagnostic::InvalidComponent {
				plugin,
				component: PluginComponent::DapAdapters,
				source,
				..
			}] if plugin == "broken@m" && matches!(
				source.downcast_ref::<DapConfigError>(),
				Some(DapConfigError::InvalidConnectMode { .. })
			)),
			"{:?}",
			found.diagnostics
		);
	}

	#[test]
	fn unapproved_plugin_adapters_are_left_out_with_a_diagnostic() {
		use crate::docserver::lsp_config::tests::{installed_plugins, write};

		let temp = tempfile::tempdir().unwrap();
		let project = temp.path().join("project");
		let plugin = temp.path().join("plugin");
		write(
			&plugin.join(".dap.json"),
			r#"{"adapters":{
				"acme-dbg":{"command":"./bin/dbg","args":["--stdio"],"fileTypes":[".acme"]},
				"trusted-dbg":{"command":"trusted","fileTypes":[".t"]}
			}}"#,
		);
		let mut plugins = installed_plugins(&temp.path().join("data"), &[("p@m", &plugin, true)]);
		let entry = &mut plugins.plugins[0];
		let trusted = crate::plugin_commands::plugin_launches(entry)
			.into_iter()
			.find(|launch| launch.server == "trusted-dbg")
			.unwrap();
		entry.approved_commands = [entry.command_digest(&trusted)].into();

		let found = discover_dap_sources(None, &project, Vec::new(), &plugins.plugins).unwrap();
		let adapters = load_dap_config(builtin_adapters(), &found.sources).unwrap();

		assert!(adapters.contains_key("trusted-dbg"));
		assert!(!adapters.contains_key("acme-dbg"), "an unapproved adapter loaded");
		let [PluginDiagnostic::CommandNotApproved(blocked)] = &found.diagnostics[..] else {
			panic!("{:?}", found.diagnostics);
		};
		assert_eq!(blocked.plugin, "p@m");
		assert_eq!(blocked.server, "acme-dbg");
		let root = fs::canonicalize(&plugin).unwrap();
		assert_eq!(*blocked.command(), root.join("bin/dbg").to_string_lossy().as_ref());
	}

	#[test]
	fn manifest_commands_require_declared_executables() {
		let bytes = br#"{"adapters":{"acme":{"command":"acme-dap"}}}"#;
		assert!(DapConfigSource::manifest_checked("acme:dap", bytes.as_slice(), false, []).is_err());
		let source =
			DapConfigSource::manifest_checked("acme:dap", bytes.as_slice(), false, [Str::new_static(
				"acme-dap",
			)])
			.unwrap();
		let adapters = load_dap_config(empty::<DapAdapterSpec>(), &[source]).unwrap();
		assert_eq!(adapters["acme"].command.provenance.kind, DapConfigSourceKind::Manifest);
	}
}
