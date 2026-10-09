//! The `models` and `models-keys` steps: v1 model config (or a `models.toml`
//! an earlier omp2 build kept in the data directory) into `models.toml`, and
//! its literal `apiKey`s into the encrypted credential store.
//!
//! Discovery reads only the native `models.toml`
//! ([`crate::discovery::models::load_configured_models`]); the legacy
//! decoders below run only inside these steps.

use std::{
	collections::BTreeMap,
	fs, io,
	path::{Path, PathBuf},
};

use omp_catalog::ProviderId;
use omp_core::Str;
use serde::Deserialize;
use strum::{Display, EnumString, IntoStaticStr};
use thiserror::Error;
use toml::{de, ser};

use super::{
	ImportEntry, ImportError, ImportMode, ImportOutcome, ImportPair, ImportStep, StepContext,
	V1Item, V1Layout,
	report::{Attention, SkipReason},
	step::atomic_replace,
};
use crate::discovery::models::{
	ModelConfig, ModelsConfig, ModelsConfigError, ProviderConfig, ProviderDiscovery,
	validate_discovery,
};

/// Why model config could not be imported.
#[derive(Debug, Error)]
pub enum ModelsImportError {
	/// The converted config fails v2's own validation.
	#[error(transparent)]
	Config(#[from] ModelsConfigError),
	/// Reading a source, or writing `models.toml` or a marker, failed.
	#[error(transparent)]
	Io(#[from] io::Error),
	/// A `models.toml` an earlier build kept is malformed.
	#[error(transparent)]
	Toml(#[from] de::Error),
	/// A v1 YAML source is malformed.
	#[error(transparent)]
	Yaml(#[from] serde_yaml::Error),
	/// A v1 JSON/JSONC source is malformed.
	#[error(transparent)]
	Json(#[from] omp_core::slopjson::ParseError),
	/// Encoding the native TOML failed.
	#[error(transparent)]
	Encode(#[from] ser::Error),
	/// A v1 model list entry has no `id` to key it by.
	#[error("a model listed under provider {provider} in the v1 model config has no `id`")]
	LegacyModelWithoutId {
		/// Provider listing the model.
		provider: Str,
	},
	/// Saving an imported v1 key to the encrypted store failed.
	#[error(transparent)]
	CredentialStore(#[from] omp_ai::auth::StoreError),
}

pub(super) fn import_models(cx: &StepContext<'_>) -> Result<Vec<ImportEntry>, ImportError> {
	let location = ModelsConfigLocation::for_pair(cx.pair);
	let (path, outcome) = match import_legacy_models(&location, cx.mode)? {
		LegacyModelsImport::NativePresent(native) => {
			(Some(native), ImportOutcome::Skipped(SkipReason::TargetExists))
		},
		LegacyModelsImport::AlreadyImported => {
			(location.legacy_path(), ImportOutcome::Skipped(SkipReason::MarkerPresent))
		},
		LegacyModelsImport::NothingToImport => (None, ImportOutcome::NothingToImport),
		LegacyModelsImport::Imported(source) => {
			let path = match source {
				ModelsConfigSource::MovedToml(path)
				| ModelsConfigSource::LegacyJson(path)
				| ModelsConfigSource::LegacyYaml(path) => path,
			};
			(Some(path), match cx.mode {
				ImportMode::Apply => ImportOutcome::Imported,
				ImportMode::DryRun => ImportOutcome::WouldImport,
			})
		},
	};
	Ok(vec![ImportEntry::new(ImportStep::Models, V1Item::Models, path, outcome)])
}

pub(super) fn import_keys(cx: &StepContext<'_>) -> Result<Vec<ImportEntry>, ImportError> {
	let location = ModelsConfigLocation::for_pair(cx.pair);
	let path = location.legacy_path();
	// A dry pass classifies every key without touching any store; the store
	// is opened only when a literal key is there to be written.
	let mut report = import_legacy_api_keys(&location, LegacyKeyTarget::DryRun)?;
	if cx.mode == ImportMode::Apply {
		if report
			.iter()
			.any(|key| matches!(key, LegacyApiKeyImport::WouldStore { .. }))
		{
			let control = cx.credentials.get()?;
			report = import_legacy_api_keys(&location, LegacyKeyTarget::Store(&control))?;
		} else {
			ImportStep::ModelsKeys
				.marker(&location.config_dir)
				.set(None)
				.map_err(ModelsImportError::from)?;
		}
	}
	if report.is_empty() {
		return Ok(vec![ImportEntry::new(
			ImportStep::ModelsKeys,
			V1Item::Models,
			path,
			ImportOutcome::NothingToImport,
		)]);
	}
	Ok(report
		.into_iter()
		.map(|key| {
			let (provider, outcome) = match key {
				LegacyApiKeyImport::Stored { provider } => (provider, ImportOutcome::Imported),
				LegacyApiKeyImport::WouldStore { provider } => (provider, ImportOutcome::WouldImport),
				LegacyApiKeyImport::AlreadyLoggedIn { provider } => {
					(provider, ImportOutcome::Skipped(SkipReason::AccountExists))
				},
				LegacyApiKeyImport::NeedsEnvironment { provider } => {
					(provider, ImportOutcome::NeedsAttention(Attention::KeyNeedsEnvironment))
				},
			};
			ImportEntry {
				step: ImportStep::ModelsKeys,
				item: V1Item::Models,
				path: path.clone(),
				subject: Some(provider),
				outcome,
			}
		})
		.collect())
}

/// Where the one-time import found the model config it converted.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ModelsConfigSource {
	/// Native TOML moved from a directory an earlier build used.
	MovedToml(PathBuf),
	/// Imported legacy JSON.
	LegacyJson(PathBuf),
	/// Imported legacy YAML.
	LegacyYaml(PathBuf),
}

/// Where `models.toml` lives and where earlier builds left model config.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelsConfigLocation {
	/// Directory holding `models.toml`: the profile's configuration root.
	pub config_dir:  PathBuf,
	/// Directories earlier omp2 builds kept model config in (the data
	/// directory), searched once, in order, before v1.
	pub legacy_dirs: Vec<PathBuf>,
	/// The v1 profile whose model config is imported once when no
	/// `models.toml` exists yet, as the v1 locator found it.
	pub v1:          Option<V1Layout>,
}

impl ModelsConfigLocation {
	/// The location one v1 → v2 profile pair imports through: the target's
	/// configuration root, its data directory, then the v1 profile.
	#[must_use]
	pub fn for_pair(pair: &ImportPair) -> Self {
		Self {
			config_dir:  pair.target.config_dir.clone(),
			legacy_dirs: vec![pair.target.data_dir.clone()],
			v1:          Some(pair.source.clone()),
		}
	}

	fn native(&self) -> PathBuf {
		self.config_dir.join("models.toml")
	}

	/// The first legacy source file present, in search order: an earlier
	/// omp2 directory, then the file v1 itself reads.
	fn legacy_source(&self) -> Option<(PathBuf, LegacyFormat)> {
		const NAMES: [(&str, LegacyFormat); 4] = [
			("models.toml", LegacyFormat::Toml),
			("models.json", LegacyFormat::Json),
			("models.yml", LegacyFormat::Yaml),
			("models.yaml", LegacyFormat::Yaml),
		];
		self
			.legacy_dirs
			.iter()
			.find_map(|directory| {
				NAMES
					.iter()
					.map(|&(name, format)| (directory.join(name), format))
					.find(|(path, _)| path.is_file())
			})
			.or_else(|| {
				let path = self.v1.as_ref()?.locate(V1Item::Models)?;
				let format = path.extension()?.to_str()?.parse().ok()?;
				Some((path, format))
			})
	}

	/// The model config file the one-time import reads, if any.
	#[must_use]
	pub fn legacy_path(&self) -> Option<PathBuf> {
		self.legacy_source().map(|(path, _)| path)
	}
}

/// Legacy model-config encodings; parsed from the file extension, and
/// labelled in the import marker.
#[derive(Clone, Copy, Debug, Display, EnumString, Eq, IntoStaticStr, PartialEq)]
enum LegacyFormat {
	/// A `models.toml` an earlier omp2 build kept in the data directory.
	#[strum(to_string = "moved-toml", serialize = "toml")]
	Toml,
	/// v1 JSON/JSONC.
	#[strum(to_string = "legacy-json", serialize = "json")]
	Json,
	/// v1 YAML.
	#[strum(to_string = "legacy-yaml", serialize = "yml", serialize = "yaml")]
	Yaml,
}

/// v1 `models.yml` shape, decoded only by the one-time import.
#[derive(Deserialize)]
struct LegacyModelsConfig {
	#[serde(default)]
	providers: BTreeMap<Str, LegacyProviderConfig>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LegacyProviderConfig {
	base_url:             Option<Str>,
	api:                  Option<Str>,
	#[serde(default)]
	headers:              BTreeMap<Str, Str>,
	auth:                 Option<Str>,
	/// v1 secret: a literal key, an environment variable name, or `!cmd`.
	/// Never written to `models.toml`; see [`import_legacy_api_keys`].
	api_key:              Option<Str>,
	discovery:            Option<ProviderDiscovery>,
	compat:               Option<toml::Value>,
	disable_strict_tools: Option<bool>,
	#[serde(default)]
	model_overrides:      BTreeMap<Str, ModelConfig>,
	#[serde(default)]
	models:               LegacyModels,
}

/// v1 lists models (`- id: …`); omp2 keys them by id.
#[derive(Deserialize)]
#[serde(untagged)]
enum LegacyModels {
	List(Vec<ModelConfig>),
	Map(BTreeMap<Str, ModelConfig>),
}

impl Default for LegacyModels {
	fn default() -> Self {
		Self::Map(BTreeMap::new())
	}
}

impl LegacyProviderConfig {
	fn into_native(self, provider: &str) -> Result<ProviderConfig, ModelsImportError> {
		let models = match self.models {
			LegacyModels::Map(models) => models,
			LegacyModels::List(models) => {
				let mut keyed = BTreeMap::new();
				for model in models {
					let id =
						model
							.id
							.clone()
							.ok_or_else(|| ModelsImportError::LegacyModelWithoutId {
								provider: Str::new(provider),
							})?;
					keyed.insert(id, model);
				}
				keyed
			},
		};
		// v1 authenticated with `apiKey` unless told otherwise, and has no
		// configured-provider OAuth; omp2 would otherwise inherit the
		// template route's credentials.
		let auth = match self.auth.as_deref() {
			Some(auth) if auth.eq_ignore_ascii_case("oauth") => None,
			Some(_) => self.auth,
			None => self.api_key.as_ref().map(|_| Str::new_static("apiKey")),
		};
		Ok(ProviderConfig {
			base_url: self.base_url,
			api: self.api,
			headers: self.headers,
			auth,
			discovery: self.discovery,
			compat: self.compat,
			disable_strict_tools: self.disable_strict_tools,
			model_overrides: self.model_overrides,
			models,
		})
	}
}

fn decode_legacy(
	path: &Path,
	format: LegacyFormat,
) -> Result<LegacyModelsConfig, ModelsImportError> {
	let text = fs::read_to_string(path)?;
	Ok(match format {
		LegacyFormat::Toml => toml::from_str(&text)?,
		LegacyFormat::Json => omp_core::slopjson::from_str(&text)?,
		LegacyFormat::Yaml => serde_yaml::from_str(&text)?,
	})
}

/// What the one-time model-config import found.
#[derive(Clone, Debug)]
pub enum LegacyModelsImport {
	/// `models.toml` already exists; nothing was read.
	NativePresent(PathBuf),
	/// The import marker records an earlier run.
	AlreadyImported,
	/// No legacy source exists; in [`ImportMode::Apply`] the marker now
	/// records that.
	NothingToImport,
	/// The legacy source, converted: written as `models.toml` in
	/// [`ImportMode::Apply`], only decoded and checked in
	/// [`ImportMode::DryRun`].
	Imported(ModelsConfigSource),
}

/// Imports `models.toml` once from the first legacy source, unless it exists
/// or the `models` step's marker is set.
///
/// Legacy files are only read, never changed: v1 may still be using them. A
/// dry run decodes the source and writes nothing.
pub fn import_legacy_models(
	location: &ModelsConfigLocation,
	mode: ImportMode,
) -> Result<LegacyModelsImport, ModelsImportError> {
	let native = location.native();
	if native.exists() {
		return Ok(LegacyModelsImport::NativePresent(native));
	}
	let marker = ImportStep::Models.marker(&location.config_dir);
	if marker.is_set() {
		return Ok(LegacyModelsImport::AlreadyImported);
	}
	let Some((path, format)) = location.legacy_source() else {
		if mode == ImportMode::Apply {
			marker.set(None)?;
		}
		return Ok(LegacyModelsImport::NothingToImport);
	};
	let legacy = decode_legacy(&path, format)?;
	let mut config = ModelsConfig::default();
	for (provider, definition) in legacy.providers {
		let definition = definition.into_native(&provider)?;
		config.providers.insert(provider, definition);
	}
	validate_discovery(&config)?;
	if mode == ImportMode::Apply {
		fs::create_dir_all(&location.config_dir)?;
		atomic_replace(&native, toml::to_string_pretty(&config)?.as_bytes())?;
		marker.set(Some(format.into()))?;
	}
	Ok(LegacyModelsImport::Imported(match format {
		LegacyFormat::Toml => ModelsConfigSource::MovedToml(path),
		LegacyFormat::Json => ModelsConfigSource::LegacyJson(path),
		LegacyFormat::Yaml => ModelsConfigSource::LegacyYaml(path),
	}))
}

/// What happened to one v1 `apiKey` during the one-time key import.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LegacyApiKeyImport {
	/// A literal key was saved to the encrypted store, as `/login` would.
	Stored {
		/// Provider the key belongs to.
		provider: Str,
	},
	/// A dry run found a literal key a real run would store.
	WouldStore {
		/// Provider the key belongs to.
		provider: Str,
	},
	/// The provider already had a stored account; the key was left out.
	AlreadyLoggedIn {
		/// Provider with an existing account.
		provider: Str,
	},
	/// The key named an environment variable or a `!command`; omp2 reads
	/// `OMP_<PROVIDER>_API_KEY` instead.
	NeedsEnvironment {
		/// Provider whose key was not imported.
		provider: Str,
	},
}

/// Where [`import_legacy_api_keys`] puts literal keys.
#[derive(Clone, Copy)]
pub enum LegacyKeyTarget<'a> {
	/// Report what would be stored; write nothing.
	DryRun,
	/// Store them through this control handle.
	Store(&'a omp_ai::auth::AuthControlHandle),
}

/// Moves literal v1 `apiKey` values into the encrypted credential store once.
///
/// The v1 value resolved as `!command`, then an environment variable of that
/// exact name, then the literal. Only a literal can be carried over: the
/// others are reported so the owner can set `OMP_<PROVIDER>_API_KEY`. A
/// literal is stored as an `api-key` through the control-plane write
/// ([`omp_ai::auth::AuthControlHandle::store`]): it stays an `api-key` under
/// the `apiKey` auth `models.toml` gives a keyed provider, and becomes `bearer`
/// under a `bearer` auth only for a provider whose bundled routes take a key
/// as a bearer token (Hugging Face, Z.ai, GitHub Copilot, a provider only
/// `models.toml` defines). Any other provider's key stays an `api-key`, which
/// the routes of a `bearer` auth do not take: its requests report
/// `kind_mismatch` until `OMP_<PROVIDER>_API_KEY` is set or the auth is
/// removed, so removing the auth never sends the key another way. A provider
/// that already has a stored account keeps it. Secrets never pass through
/// `models.toml`. A dry run neither stores keys nor sets the marker.
pub fn import_legacy_api_keys(
	location: &ModelsConfigLocation,
	target: LegacyKeyTarget<'_>,
) -> Result<Vec<LegacyApiKeyImport>, ModelsImportError> {
	let marker = ImportStep::ModelsKeys.marker(&location.config_dir);
	if marker.is_set() {
		return Ok(Vec::new());
	}
	let Some((path, format)) = location
		.legacy_source()
		.filter(|(_, format)| *format != LegacyFormat::Toml)
	else {
		if matches!(target, LegacyKeyTarget::Store(_)) && location.config_dir.is_dir() {
			marker.set(None)?;
		}
		return Ok(Vec::new());
	};
	let legacy = decode_legacy(&path, format)?;
	let mut report = Vec::new();
	for (provider, definition) in legacy.providers {
		let Some(key) = definition.api_key else {
			continue;
		};
		let looks_like_variable = key.chars().all(|character| {
			character.is_ascii_uppercase() || character.is_ascii_digit() || character == '_'
		}) && key
			.starts_with(|character: char| character.is_ascii_uppercase());
		let LegacyKeyTarget::Store(control) = target else {
			report.push(if key.starts_with('!') || looks_like_variable {
				LegacyApiKeyImport::NeedsEnvironment { provider }
			} else {
				LegacyApiKeyImport::WouldStore { provider }
			});
			continue;
		};
		if !control
			.accounts(Some(ProviderId::from_ref(provider.as_str())))
			.is_empty()
		{
			report.push(LegacyApiKeyImport::AlreadyLoggedIn { provider });
			continue;
		}
		if key.starts_with('!') || looks_like_variable {
			report.push(LegacyApiKeyImport::NeedsEnvironment { provider });
			continue;
		}
		control.store(omp_ai::auth::CredentialControlWrite {
			provider:      ProviderId::from(provider.as_str()),
			principal:     omp_ai::PrincipalId::from("models-yml"),
			identity:      Some(Str::new_static("models-yml")),
			kind:          Str::new_static(omp_ai::auth::CredentialKind::ApiKey.into()),
			secret:        omp_core::Secret::from(key.as_bytes().to_vec()),
			expires_at_ms: None,
		})?;
		report.push(LegacyApiKeyImport::Stored { provider });
	}
	if matches!(target, LegacyKeyTarget::Store(_)) {
		marker.set(None)?;
	}
	Ok(report)
}

#[cfg(test)]
#[path = "models_tests.rs"]
mod tests;
