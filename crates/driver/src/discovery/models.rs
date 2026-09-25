//! Native `models.toml` decoding for configured catalog overlays.

use std::{
	cmp::Reverse,
	collections::BTreeMap,
	env, fs, io,
	iter::FusedIterator,
	path::{Path, PathBuf},
	sync::Arc,
	time::SystemTime,
};

use omp_ai::{
	auth::{
		CredentialNeed, CredentialSource, CredentialStore, HeaderPlacement, StoredCredentialSource,
	},
	discovery::DiscoveryProbe,
};
use omp_catalog::{
	AccountScope, AuthSpec, AuthSpecKind, Availability, CatalogOverlay, CatalogOverlayBuilder,
	ClassId, ContextStrategy, CredentialSourceSpec, EvidenceConfidence, ModalityBits,
	ModelAvailability, ModelKey, ModelLimits, ModelOverlay, ModelPatch, ModelProvenance, ModelSpec,
	OverlaySource, OverlayStack, OverlayStore, PremiumMultiplier, Pricing, ProvenanceKind,
	ProvenanceSource, ProviderDef, ProviderId, RouteDef, RouteId, RouteOverlay, RoutePatch,
	ThinkingPolicy, ThinkingRouting, UnsafeTrustScope, WireModelId,
};
use omp_core::{Str, string_id};
use serde::{Deserialize, Serialize};
use strum::{Display, EnumString, IntoStaticStr};
use toml::{de, ser};

fn atomic_replace(path: &Path, contents: &str) -> io::Result<()> {
	let mut temporary = path.as_os_str().to_owned();
	temporary.push(format!(".tmp-{}", std::process::id()));
	let temporary = PathBuf::from(temporary);
	let result = (|| {
		let file = fs::File::create(&temporary)?;
		io::Write::write_all(&mut &file, contents.as_bytes())?;
		file.sync_all()?;
		fs::rename(&temporary, path)
	})();
	if result.is_err() {
		let _ = fs::remove_file(&temporary);
	}
	result
}

/// Native model configuration. TOML is OMP's native serialization.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct ModelsConfig {
	/// Provider definitions keyed by stable provider id.
	#[serde(default)]
	pub providers: BTreeMap<Str, ProviderConfig>,
}

/// Closed local/configured provider discovery protocols.
#[derive(
	Clone, Copy, Debug, Display, EnumString, Eq, IntoStaticStr, PartialEq, Deserialize, Serialize,
)]
pub enum ProviderDiscoveryKind {
	/// Ollama `/api/tags` plus per-model `/api/show`.
	#[serde(rename = "ollama")]
	#[strum(serialize = "ollama")]
	Ollama,
	/// llama.cpp native `/models` and `/props`.
	#[serde(rename = "llama.cpp")]
	#[strum(serialize = "llama.cpp")]
	LlamaCpp,
	/// LM Studio native v0 metadata.
	#[serde(rename = "lm-studio")]
	#[strum(serialize = "lm-studio")]
	LmStudio,
	/// LiteLLM rich model metadata with `/v1/models` fallback.
	#[serde(rename = "litellm")]
	#[strum(serialize = "litellm")]
	LiteLlm,
	/// Generic OpenAI-compatible model list.
	#[serde(rename = "openai-models-list")]
	#[strum(serialize = "openai-models-list")]
	OpenAiModelsList,
	/// Dual Anthropic/OpenAI compatible proxy.
	#[serde(rename = "proxy")]
	#[strum(serialize = "proxy")]
	Proxy,
}

/// Typed provider discovery policy.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderDiscovery {
	/// Discovery wire protocol.
	#[serde(rename = "type")]
	pub kind:       ProviderDiscoveryKind,
	/// Optional complete-probe timeout.
	pub timeout_ms: Option<u64>,
	/// Whether OpenAI model-list discovery injects `/v1`.
	pub inject_v1:  Option<bool>,
}

/// Provider-level configuration facts.
#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderConfig {
	/// Route base URL override.
	pub base_url:             Option<Str>,
	/// Provider-wide wire API selector.
	pub api:                  Option<Str>,
	/// Static request headers.
	#[serde(default)]
	pub headers:              BTreeMap<Str, Str>,
	/// Authentication mode.
	pub auth:                 Option<Str>,
	/// Provider model-discovery configuration.
	pub discovery:            Option<ProviderDiscovery>,
	/// Wire compatibility configuration.
	pub compat:               Option<toml::Value>,
	/// Whether strict tool schemas are disabled.
	pub disable_strict_tools: Option<bool>,
	/// Per-model replacement facts keyed by model id.
	#[serde(default)]
	pub model_overrides:      BTreeMap<Str, ModelConfig>,
	/// Provider model definitions keyed by model id.
	#[serde(default)]
	pub models:               BTreeMap<Str, ModelConfig>,
}

impl std::fmt::Debug for ProviderConfig {
	fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		formatter
			.debug_struct("ProviderConfig")
			.field("base_url", &self.base_url.as_ref().map(|_| "<configured>"))
			.field("api", &self.api)
			.field("header_names", &self.headers.keys().collect::<Vec<_>>())
			.field("auth", &self.auth)
			.field("discovery", &self.discovery)
			.field("compat", &self.compat.as_ref().map(|_| "<configured>"))
			.field("disable_strict_tools", &self.disable_strict_tools)
			.field("model_override_count", &self.model_overrides.len())
			.field("model_count", &self.models.len())
			.finish()
	}
}

/// Declarative configured header value source.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum HeaderValueSource {
	/// Safe static public value.
	Public(Str),
	/// Environment-owned secret name.
	Environment(Str),
	/// Environment-executed secret command.
	Command(Str),
}

impl ProviderConfig {
	/// Every model entry this provider declares, `models` then
	/// `modelOverrides`, with its table name.
	pub fn configured_models(
		&self,
	) -> impl DoubleEndedIterator<Item = (&str, &ModelConfig)> + FusedIterator + Clone + '_ {
		self
			.models
			.iter()
			.chain(self.model_overrides.iter())
			.map(|(name, model)| (name.as_str(), model))
	}

	/// Whether this provider's `models.toml` table declares the model `key`
	/// names, keyed `<provider>/<id>` like every catalog model.
	pub fn declares(&self, provider: &ProviderId<str>, key: &ModelKey<str>) -> bool {
		key.scoped_model(provider)
			.is_some_and(|model| self.declares_wire(model))
	}

	/// Whether this provider's `models.toml` table declares the provider
	/// model id `wire`.
	pub fn declares_wire(&self, wire: &str) -> bool {
		self
			.configured_models()
			.any(|(name, model)| model.wire_id(name) == wire)
	}

	/// Classifies configured headers without resolving or copying secret
	/// material into the catalog.
	pub fn header_sources(&self) -> Vec<(Str, HeaderValueSource)> {
		self
			.headers
			.iter()
			.map(|(name, value)| {
				let source = if let Some(command) = value.strip_prefix("!") {
					HeaderValueSource::Command(Str::new(command.trim()))
				} else if let Some(environment) = value.strip_prefix("$") {
					HeaderValueSource::Environment(Str::new(environment))
				} else {
					HeaderValueSource::Public(value.clone())
				};
				(name.clone(), source)
			})
			.collect()
	}
}

/// Model-level configuration facts mapped one-for-one onto the catalog model
/// fields. Typed overlay lowering is intentionally done by the catalog owner.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelConfig {
	/// Explicit normalized model id.
	pub id: Option<Str>,
	/// Picker display name.
	pub name: Option<Str>,
	/// Wire API selector.
	pub api: Option<Str>,
	/// Total context-window limit.
	pub context_window: Option<u64>,
	/// Maximum generated-token limit.
	pub max_tokens: Option<u64>,
	/// Tool-use support flag.
	pub supports_tools: Option<bool>,
	/// Streaming support flag.
	pub supports_streaming: Option<bool>,
	/// Reasoning-policy declaration.
	pub reasoning: Option<toml::Value>,
	/// Accepted input modalities.
	pub input: Option<toml::Value>,
	/// Price schedule.
	pub cost: Option<toml::Value>,
	/// Model-specific compatibility configuration.
	pub compat: Option<toml::Value>,
	/// Remote compaction contract.
	pub remote_compaction: Option<toml::Value>,
	/// Premium quota multiplier, written as text (`'0.25'`) or a number.
	#[serde(default, deserialize_with = "number_or_text")]
	pub premium_multiplier: Option<Str>,
	/// Compaction model reference.
	pub compaction_model: Option<ModelReference>,
	/// Preferred edit-tool contract revision.
	pub edit_revision: Option<Str>,
	/// Context-promotion target reference.
	pub context_promotion_target: Option<ModelReference>,
}

impl ModelConfig {
	/// The provider's own id for the model this entry declares: the explicit
	/// `id`, else the entry's table name.
	pub fn wire_id<'a>(&'a self, name: &'a str) -> &'a str {
		self.id.as_deref().unwrap_or(name)
	}
}

string_id!(/// A model named by a `models.toml` fact (`compactionModel`,
	/// `contextPromotionTarget`), written as the user wrote it: usually the
	/// declaring provider's own model id, else any catalog model selector.
	///
	/// A reference resolves inside its declaring provider first
	/// ([`ModelReference::within`]), then through the catalog-wide selection
	/// every other model selector uses ([`ModelReference::across`]); one that
	/// names nothing yet stays in the declaring provider's namespace, where
	/// runtime discovery adds models.
	ModelReference);

impl ModelReference<str> {
	/// The declaring provider's model this reference names: an entry of the
	/// provider's own `models.toml` table (by table name or `id`), else a
	/// catalog model or alias under `<provider>/<reference>`.
	pub fn within(
		&self,
		provider: &ProviderId<str>,
		declared: &ProviderConfig,
		catalog: &omp_catalog::Catalog,
	) -> Option<ModelKey> {
		let reference = self.as_str();
		if let Some(wire) = declared
			.configured_models()
			.find(|(name, model)| *name == reference || model.wire_id(name) == reference)
			.map(|(name, model)| model.wire_id(name))
		{
			return Some(ModelKey::provider_scoped(provider, wire));
		}
		let scoped = ModelKey::provider_scoped(provider, reference);
		if catalog.model(&scoped).is_some() {
			return Some(scoped);
		}
		catalog
			.resolve_alias(scoped.as_str())
			.filter(|model| {
				model.routes.iter().any(|route| {
					catalog
						.route(route)
						.is_some_and(|route| route.provider.as_str() == provider.as_str())
				})
			})
			.map(|model| model.key.clone())
	}

	/// The model the catalog-wide selector resolution names: exact keys,
	/// aliases, `provider/model` pairs, and bare ids, exactly as `/model`,
	/// role selectors, and `ai_model` resolve.
	pub fn across(&self, catalog: &omp_catalog::Catalog) -> Option<ModelKey> {
		omp_catalog::select_model(
			catalog.models(),
			catalog.routes(),
			catalog.aliases(),
			&[],
			&BTreeMap::new(),
			self.as_str(),
		)
		.ok()
		.map(|selected| selected.model)
	}

	/// The key an unresolved reference keeps: the declaring provider's
	/// namespace, where a later discovery listing adds the model.
	pub fn unresolved(&self, provider: &ProviderId<str>) -> ModelKey {
		ModelKey::provider_scoped(provider, self.as_str())
	}
}

/// A scalar written either as text or as a bare number.
#[derive(Deserialize)]
#[serde(untagged)]
enum NumberOrText {
	Text(Str),
	Integer(i64),
	Float(f64),
}

fn number_or_text<'de, D>(deserializer: D) -> Result<Option<Str>, D::Error>
where
	D: serde::Deserializer<'de>,
{
	Ok(Option::<NumberOrText>::deserialize(deserializer)?.map(|value| match value {
		NumberOrText::Text(text) => text,
		NumberOrText::Integer(number) => Str::from(number.to_string()),
		NumberOrText::Float(number) => Str::from(number.to_string()),
	}))
}

/// Decodes a native configured-model file.
pub fn load_models_config(path: &Path) -> Result<ModelsConfig, ModelsConfigError> {
	let source = fs::read_to_string(path)?;
	let config = toml::from_str(&source)?;
	validate_discovery(&config)?;
	Ok(config)
}

/// Source label for a native model configuration or one-time legacy import.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ModelsConfigSource {
	/// Canonical native TOML.
	NativeToml(PathBuf),
	/// Native TOML moved from a directory an earlier build used.
	MovedToml(PathBuf),
	/// Imported legacy JSON.
	LegacyJson(PathBuf),
	/// Imported legacy YAML.
	LegacyYaml(PathBuf),
}

/// Typed model config paired with its explicit provenance label.
#[derive(Clone, Debug)]
pub struct LoadedModelsConfig {
	/// Typed configuration.
	pub config: ModelsConfig,
	/// Decoder/import source.
	pub source: ModelsConfigSource,
}

/// Where `models.toml` lives and where earlier builds left model config.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelsConfigLocation {
	/// Directory holding `models.toml`: the profile's configuration root.
	pub config_dir:  PathBuf,
	/// Directories searched once, in order, when no `models.toml` exists yet:
	/// the data directory earlier omp2 builds used, then the v1 agent
	/// directory (`~/.omp/agent`).
	pub legacy_dirs: Vec<PathBuf>,
}

impl ModelsConfigLocation {
	/// Resolves the owner's profile configuration root (`~/.o2`, or
	/// `OMP_CONFIG_DIR`), with `data_dir` and `~/.omp/agent` as one-time
	/// import sources.
	///
	/// A `data_dir` other than the process default (a test's temporary state,
	/// an explicit state directory) isolates model configuration as well: it
	/// is read from that directory and nothing is imported.
	pub fn resolve(data_dir: &Path) -> Result<Self, ModelsConfigError> {
		if omp_core::dirs::data_dir(None).ok().as_deref() != Some(data_dir) {
			return Ok(Self { config_dir: data_dir.to_owned(), legacy_dirs: Vec::new() });
		}
		let config_dir = omp_core::dirs::user_config_root()?;
		let mut legacy_dirs = vec![data_dir.to_owned()];
		if let Some(home) = omp_core::dirs::home_dir() {
			legacy_dirs.push(home.join(".omp").join("agent"));
		}
		Ok(Self { config_dir, legacy_dirs })
	}

	fn native(&self) -> PathBuf {
		self.config_dir.join("models.toml")
	}

	/// The first legacy source file present, in search order.
	fn legacy_source(&self) -> Option<(PathBuf, LegacyFormat)> {
		const NAMES: [(&str, LegacyFormat); 4] = [
			("models.toml", LegacyFormat::Toml),
			("models.json", LegacyFormat::Json),
			("models.yml", LegacyFormat::Yaml),
			("models.yaml", LegacyFormat::Yaml),
		];
		self.legacy_dirs.iter().find_map(|directory| {
			NAMES
				.iter()
				.map(|&(name, format)| (directory.join(name), format))
				.find(|(path, _)| path.is_file())
		})
	}
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LegacyFormat {
	Toml,
	Json,
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
	fn into_native(self, provider: &str) -> Result<ProviderConfig, ModelsConfigError> {
		let models = match self.models {
			LegacyModels::Map(models) => models,
			LegacyModels::List(models) => {
				let mut keyed = BTreeMap::new();
				for model in models {
					let id =
						model
							.id
							.clone()
							.ok_or_else(|| ModelsConfigError::LegacyModelWithoutId {
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
) -> Result<LegacyModelsConfig, ModelsConfigError> {
	let text = fs::read_to_string(path)?;
	Ok(match format {
		LegacyFormat::Toml => toml::from_str(&text)?,
		LegacyFormat::Json => omp_core::slopjson::from_str(&text)?,
		LegacyFormat::Yaml => serde_yaml::from_str(&text)?,
	})
}

/// Loads `models.toml` from the configuration root, importing it once from
/// the first legacy source when none exists yet.
///
/// Legacy formats are never live fallback decoders, and legacy files are only
/// read, never changed: v1 may still be using them.
pub fn load_or_import_legacy(
	location: &ModelsConfigLocation,
) -> Result<Option<LoadedModelsConfig>, ModelsConfigError> {
	let native = location.native();
	if native.exists() {
		return Ok(Some(LoadedModelsConfig {
			config: load_models_config(&native)?,
			source: ModelsConfigSource::NativeToml(native),
		}));
	}
	let marker = location.config_dir.join(".models-migration-v1");
	if marker.exists() {
		return Ok(None);
	}
	fs::create_dir_all(&location.config_dir)?;
	let Some((path, format)) = location.legacy_source() else {
		atomic_replace(&marker, "revision = 1\n")?;
		return Ok(None);
	};
	let legacy = decode_legacy(&path, format)?;
	let mut config = ModelsConfig::default();
	for (provider, definition) in legacy.providers {
		let definition = definition.into_native(&provider)?;
		config.providers.insert(provider, definition);
	}
	validate_discovery(&config)?;
	atomic_replace(&native, &toml::to_string_pretty(&config)?)?;
	let source = match format {
		LegacyFormat::Toml => "moved-toml",
		LegacyFormat::Json => "legacy-json",
		LegacyFormat::Yaml => "legacy-yaml",
	};
	atomic_replace(&marker, &format!("revision = 1\nsource = \"{source}\"\n"))?;
	Ok(Some(LoadedModelsConfig {
		config,
		source: match format {
			LegacyFormat::Toml => ModelsConfigSource::MovedToml(path),
			LegacyFormat::Json => ModelsConfigSource::LegacyJson(path),
			LegacyFormat::Yaml => ModelsConfigSource::LegacyYaml(path),
		},
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

/// Moves literal v1 `apiKey` values into the encrypted credential store once.
///
/// The v1 value resolved as `!command`, then an environment variable of that
/// exact name, then the literal. Only a literal can be carried over: the
/// others are reported so the owner can set `OMP_<PROVIDER>_API_KEY`. A
/// provider that already has a stored account keeps it. Secrets never pass
/// through `models.toml`.
pub fn import_legacy_api_keys(
	location: &ModelsConfigLocation,
	control: &omp_ai::auth::AuthControlHandle,
) -> Result<Vec<LegacyApiKeyImport>, ModelsConfigError> {
	let marker = location.config_dir.join(".models-keys-migration-v1");
	if marker.exists() {
		return Ok(Vec::new());
	}
	let Some((path, format)) = location
		.legacy_source()
		.filter(|(_, format)| *format != LegacyFormat::Toml)
	else {
		if location.config_dir.is_dir() {
			atomic_replace(&marker, "revision = 1\n")?;
		}
		return Ok(Vec::new());
	};
	let legacy = decode_legacy(&path, format)?;
	let mut report = Vec::new();
	for (provider, definition) in legacy.providers {
		let Some(key) = definition.api_key else {
			continue;
		};
		if !control
			.accounts(Some(ProviderId::from_ref(provider.as_str())))
			.is_empty()
		{
			report.push(LegacyApiKeyImport::AlreadyLoggedIn { provider });
			continue;
		}
		let looks_like_variable = key.chars().all(|character| {
			character.is_ascii_uppercase() || character.is_ascii_digit() || character == '_'
		}) && key
			.starts_with(|character: char| character.is_ascii_uppercase());
		if key.starts_with('!') || looks_like_variable {
			report.push(LegacyApiKeyImport::NeedsEnvironment { provider });
			continue;
		}
		control.store(omp_ai::auth::CredentialControlWrite {
			provider:      ProviderId::from(provider.as_str()),
			principal:     omp_ai::PrincipalId::from("models-yml"),
			identity:      Some(Str::new_static("models-yml")),
			kind:          Str::new_static("api-key"),
			secret:        omp_core::Secret::from(key.as_bytes().to_vec()),
			expires_at_ms: None,
		})?;
		report.push(LegacyApiKeyImport::Stored { provider });
	}
	fs::create_dir_all(&location.config_dir)?;
	atomic_replace(&marker, "revision = 1\n")?;
	Ok(report)
}

/// Validates and lowers configured model facts into a secret-free immutable
/// overlay.
///
/// Omitted fields inherit bundled facts. Header and credential values remain
/// declarative in the configuration authority and are never copied into model
/// records.
pub fn lower_user_overlay(config: &ModelsConfig) -> Result<CatalogOverlay, ModelsConfigError> {
	validate_discovery(config)?;
	let source = ProvenanceSource {
		kind:           ProvenanceKind::Configured,
		origin:         "models.toml".into(),
		revision:       None,
		confidence:     EvidenceConfidence::Declared,
		observed_at_ms: None,
	};
	let mut builder = CatalogOverlayBuilder::new(source.clone());
	let catalog = omp_catalog::Catalog::embedded();
	let mut models = Vec::new();
	let mut references = Vec::new();
	for (provider, definition) in &config.providers {
		let provider_id = ProviderId::from_ref(provider.as_str());
		let configured_auth = definition
			.auth
			.as_deref()
			.map(|auth| configured_auth_spec(provider, auth))
			.transpose()?;
		if let Some(spec) = &configured_auth {
			builder = builder.with_auth_spec(spec.clone());
		}
		let base_provider = catalog.provider(ProviderId::from_ref(provider.as_str()));
		let configured_route = if let Some(base_provider) = base_provider {
			for route_id in &base_provider.routes {
				let Some(route) = catalog.route(route_id) else {
					continue;
				};
				let endpoint = definition
					.base_url
					.as_ref()
					.map(|base_url| omp_catalog::EndpointSpec {
						base_url:    base_url.clone(),
						region:      route.endpoint.region.clone(),
						api_version: route.endpoint.api_version.clone(),
					});
				builder = builder.with_route(RouteOverlay {
					route: route_id.clone(),
					added: None,
					patch: RoutePatch {
						endpoint,
						auth: configured_auth.as_ref().map(|spec| spec.id.clone()),
						discovery: None,
						disable_strict_tools: definition.disable_strict_tools,
						..RoutePatch::default()
					},
				});
			}
			None
		} else {
			let (template_provider, template_route) =
				configured_provider_template(catalog, definition);
			let route_id = RouteId::from(format!("{provider}-configured"));
			let mut added_provider: ProviderDef = template_provider.clone();
			added_provider.id = ProviderId::from(provider.as_str());
			added_provider.name = provider.clone();
			if let Some(auth) = &configured_auth {
				added_provider.auth = Box::new([auth.id.clone()]);
			}
			let mut added_route: RouteDef = template_route.clone();
			added_route.id = route_id.clone();
			added_route.provider = ProviderId::from(provider.as_str());
			if let Some((codec, transport)) = definition
				.api
				.as_deref()
				.or_else(|| {
					definition
						.models
						.values()
						.chain(definition.model_overrides.values())
						.find_map(|model| model.api.as_deref())
				})
				.and_then(omp_catalog::resolve_source_transport)
			{
				added_route.codec = codec;
				added_route.transport = transport;
			}
			if let Some(base_url) = &definition.base_url {
				added_route.endpoint.base_url = base_url.clone();
				if let Ok(url) = url::Url::parse(base_url)
					&& let Some(host) = url.host_str()
				{
					let mut origin = format!("{}://{host}", url.scheme());
					if let Some(port) = url.port() {
						origin.push(':');
						origin.push_str(&port.to_string());
					}
					added_route.trust_domain.origin = Str::from(origin);
					added_route.trust_domain.allow_plaintext = url.scheme() == "http";
				}
			}
			if let Some(auth) = &configured_auth {
				added_route.auth = auth.id.clone();
			}
			if definition.discovery.is_none() {
				added_route.discovery = None;
			}
			if let Some(disabled) = definition.disable_strict_tools {
				added_route.capability_limits.disable_strict_tools = disabled;
			}
			let alternate_route = definition
				.discovery
				.as_ref()
				.is_some_and(|discovery| discovery.kind == ProviderDiscoveryKind::Proxy)
				.then(|| {
					let alternate_api = if added_route.codec.as_str().contains("anthropic") {
						"openai-completions"
					} else {
						"anthropic-messages"
					};
					let (codec, transport) = omp_catalog::resolve_source_transport(alternate_api)
						.expect("built-in proxy transports exist");
					let alternate_id = RouteId::from(format!("{provider}-configured-alternate"));
					let mut alternate = added_route.clone();
					alternate.id = alternate_id.clone();
					alternate.codec = codec;
					alternate.transport = transport;
					(alternate_id, alternate)
				});
			added_provider.routes = match &alternate_route {
				Some((alternate, _)) => vec![route_id.clone(), alternate.clone()].into_boxed_slice(),
				None => Box::new([route_id.clone()]),
			};
			builder = builder
				.with_provider(added_provider)
				.with_route(RouteOverlay {
					route: route_id.clone(),
					added: Some(added_route),
					patch: RoutePatch::default(),
				});
			if let Some((alternate_id, alternate)) = alternate_route {
				builder = builder.with_route(RouteOverlay {
					route: alternate_id,
					added: Some(alternate),
					patch: RoutePatch::default(),
				});
			}
			Some(route_id)
		};
		for (name, model) in definition.configured_models() {
			let wire = model.wire_id(name);
			let key = ModelKey::provider_scoped(provider_id, wire);
			let limits =
				(model.context_window.is_some() || model.max_tokens.is_some()).then_some(ModelLimits {
					context_window:        model.context_window,
					maximum_input_tokens:  None,
					maximum_output_tokens: model.max_tokens,
					maximum_batch:         None,
				});
			let mut capabilities = None;
			if model.supports_tools.is_some()
				|| model.supports_streaming.is_some()
				|| model.input.is_some()
			{
				// The provider's own record, else the same model id anywhere
				// (a proxy reselling a bundled model inherits its evidence).
				let inherited = catalog
					.model(&key)
					.or_else(|| {
						catalog.models().iter().find(|candidate| {
							candidate
								.key
								.as_str()
								.split_once('/')
								.is_some_and(|(_, id)| id == wire)
						})
					})
					.map_or_else(omp_catalog::unknown_capabilities, |candidate| {
						candidate.capabilities.clone()
					});
				let mut updated = inherited;
				updated
					.operations
					.insert_kind(omp_catalog::OperationKind::Chat);
				let chat = updated
					.chat
					.get_or_insert_with(omp_catalog::unknown_chat_capabilities);
				if let Some(supports) = model.supports_tools {
					chat.tools = if supports {
						Availability::Native(omp_catalog::ToolCapabilities {
							features:      omp_catalog::ToolFeatureBits::empty(),
							maximum_tools: None,
						})
					} else {
						Availability::Unsupported
					};
				}
				if let Some(input) = &model.input {
					chat.input_modalities =
						Availability::Native(parse_modalities(input, provider, wire)?);
				}
				capabilities = Some(updated);
			}
			let thinking = match &model.reasoning {
				None => None,
				Some(toml::Value::Boolean(false)) => Some(None),
				Some(toml::Value::Boolean(true)) => None,
				Some(value) => {
					let policy = value.clone().try_into::<ThinkingPolicy>().map_err(|_| {
						ModelsConfigError::InvalidFact {
							provider: provider.clone(),
							model:    Str::new(wire),
							field:    "reasoning",
						}
					})?;
					policy
						.validate()
						.map_err(|_| ModelsConfigError::InvalidFact {
							provider: provider.clone(),
							model:    Str::new(wire),
							field:    "reasoning",
						})?;
					Some(Some(policy.content_id()))
				},
			};
			let pricing = model
				.cost
				.as_ref()
				.map(|value| {
					value
						.clone()
						.try_into::<Pricing>()
						.map_err(|_| ModelsConfigError::InvalidFact {
							provider: provider.clone(),
							model:    Str::new(wire),
							field:    "cost",
						})
				})
				.transpose()?;
			if let Some(pricing) = &pricing {
				pricing
					.validate()
					.map_err(|_| ModelsConfigError::InvalidFact {
						provider: provider.clone(),
						model:    Str::new(wire),
						field:    "cost",
					})?;
			}
			let premium_multiplier_millionths = model
				.premium_multiplier
				.as_deref()
				.map(|value| {
					parse_multiplier(value).ok_or_else(|| ModelsConfigError::InvalidFact {
						provider: provider.clone(),
						model:    Str::new(wire),
						field:    "premiumMultiplier",
					})
				})
				.transpose()?
				.map(|value| Some(PremiumMultiplier::from_millionths(value)));
			let existing = catalog.model(&key).filter(|candidate| {
				candidate.routes.iter().any(|route_id| {
					catalog
						.route(route_id)
						.is_some_and(|route| route.provider.as_str() == provider.as_str())
				})
			});
			let added = existing.is_none().then(|| {
				configured_model_record(
					catalog,
					key.clone(),
					wire,
					model,
					definition,
					configured_route
						.clone()
						.or_else(|| base_provider.and_then(|base| base.routes.first().cloned()))
						.expect("configured provider template always has a route"),
					&source,
				)
			});
			for (field, reference) in [
				(ReferenceField::CompactionModel, &model.compaction_model),
				(ReferenceField::ContextPromotionTarget, &model.context_promotion_target),
			] {
				if let Some(reference) = reference {
					references.push(PendingReference {
						model: models.len(),
						field,
						provider: provider_id,
						declared: definition,
						reference,
					});
				}
			}
			models.push(ModelOverlay {
				selector: omp_catalog::ExactSelector::new(provider.clone(), key),
				added,
				patch: ModelPatch {
					display_name: model.name.clone(),
					capabilities,
					limits,
					thinking,
					pricing,
					premium_multiplier_millionths,
					edit_revision: model.edit_revision.clone().map(Some),
					..ModelPatch::default()
				},
			});
		}
	}
	// References resolve once every configured key is known: inside the
	// declaring provider first, then catalog-wide against the configured
	// catalog, which is materialized only when some reference needs it.
	let mut global = Vec::new();
	for pending in references {
		match pending
			.reference
			.within(pending.provider, pending.declared, catalog)
		{
			Some(key) => pending.apply(&mut models, key),
			None => global.push(pending),
		}
	}
	if !global.is_empty() {
		let configured = catalog
			.with_overlay_stack(
				&OverlayStack::from_layers([(
					OverlaySource::UserConfig,
					models
						.iter()
						.cloned()
						.fold(builder.clone(), CatalogOverlayBuilder::with_model)
						.build(),
				)]),
				UnsafeTrustScope::ALL,
			)
			.map_err(|source| ModelsConfigError::ReferenceCatalog { source })?;
		for pending in global {
			let key = pending
				.reference
				.across(&configured)
				.unwrap_or_else(|| pending.reference.unresolved(pending.provider));
			pending.apply(&mut models, key);
		}
	}
	Ok(models
		.into_iter()
		.fold(builder, CatalogOverlayBuilder::with_model)
		.build())
}

/// The `models.toml` facts that name another model.
#[derive(Clone, Copy)]
enum ReferenceField {
	CompactionModel,
	ContextPromotionTarget,
}

/// One model reference awaiting resolution against every configured key.
struct PendingReference<'c> {
	/// Index of the declaring model's overlay.
	model:     usize,
	field:     ReferenceField,
	provider:  &'c ProviderId<str>,
	declared:  &'c ProviderConfig,
	reference: &'c ModelReference,
}

impl PendingReference<'_> {
	fn apply(&self, models: &mut [ModelOverlay], key: ModelKey) {
		let patch = &mut models[self.model].patch;
		match self.field {
			ReferenceField::CompactionModel => patch.compaction_model = Some(Some(key)),
			ReferenceField::ContextPromotionTarget => {
				patch.context_promotion_target = Some(Some(key));
			},
		}
	}
}

fn validate_discovery(config: &ModelsConfig) -> Result<(), ModelsConfigError> {
	for (provider, definition) in &config.providers {
		let Some(discovery) = &definition.discovery else {
			continue;
		};
		if discovery.timeout_ms == Some(0)
			|| (discovery.inject_v1.is_some()
				&& discovery.kind != ProviderDiscoveryKind::OpenAiModelsList)
		{
			return Err(ModelsConfigError::InvalidProviderDiscovery { provider: provider.clone() });
		}
	}
	Ok(())
}

fn configured_provider_template<'a>(
	catalog: &'a omp_catalog::Catalog,
	definition: &ProviderConfig,
) -> (&'a ProviderDef, &'a RouteDef) {
	let api = definition
		.api
		.as_deref()
		.or_else(|| {
			definition
				.models
				.values()
				.chain(definition.model_overrides.values())
				.find_map(|model| model.api.as_deref())
		})
		.unwrap_or_default();
	let preferred = if api.contains("anthropic") {
		"anthropic"
	} else if api.contains("google") || api.contains("gemini") {
		"google"
	} else {
		"openai"
	};
	let provider = catalog
		.provider(ProviderId::from_ref(preferred))
		.or_else(|| catalog.providers().first())
		.expect("embedded catalog has a provider template");
	let route = provider
		.routes
		.iter()
		.find_map(|route| catalog.route(route))
		.or_else(|| catalog.routes().first())
		.expect("embedded catalog has a route template");
	(provider, route)
}
/// Chat-capable capability record with otherwise unknown evidence.
///
/// Configured `models.toml` entries are chat models by contract; without the
/// declared chat operation the router rejects every turn with
/// `catalog-operation-unsupported`.
fn configured_chat_capabilities() -> omp_catalog::ModelCapabilities {
	let mut capabilities = omp_catalog::unknown_capabilities();
	capabilities
		.operations
		.insert_kind(omp_catalog::OperationKind::Chat);
	capabilities.chat = Some(omp_catalog::unknown_chat_capabilities());
	capabilities
}
/// Synthesizes the interned authentication spec named by a configured
/// provider's `auth` mode.
///
/// `none` fits keyless or header-authenticated endpoints; `api_key`,
/// `bearer`, and `optional_bearer` read `OMP_<PROVIDER>_API_KEY` (falling
/// back to stored credentials); `basic` reads `OMP_<PROVIDER>_USERNAME` and
/// `OMP_<PROVIDER>_PASSWORD`.
///
/// Modes accept snake_case, kebab-case, and legacy camelCase spellings
/// (`apiKey`, `optional-bearer`, `API_KEY` are all `api_key`-class inputs).
fn configured_auth_spec(provider: &str, auth: &str) -> Result<AuthSpec, ModelsConfigError> {
	let mut normalized = String::with_capacity(auth.len() + 4);
	let camel = auth.chars().any(|c| c.is_ascii_lowercase());
	for c in auth.chars() {
		if c == '-' {
			normalized.push('_');
		} else if c.is_ascii_uppercase() {
			if camel && !normalized.is_empty() && !normalized.ends_with('_') {
				normalized.push('_');
			}
			normalized.push(c.to_ascii_lowercase());
		} else {
			normalized.push(c);
		}
	}
	let kind = normalized
		.parse::<AuthSpecKind>()
		.ok()
		.filter(|kind| {
			matches!(
				kind,
				AuthSpecKind::None
					| AuthSpecKind::ApiKey
					| AuthSpecKind::Bearer
					| AuthSpecKind::OptionalBearer
					| AuthSpecKind::Basic
			)
		})
		.ok_or_else(|| ModelsConfigError::InvalidAuth { provider: Str::new(provider) })?;
	let env_base: String = provider
		.chars()
		.map(|c| {
			if c.is_ascii_alphanumeric() {
				c.to_ascii_uppercase()
			} else {
				'_'
			}
		})
		.collect();
	let bearer =
		matches!(kind, AuthSpecKind::ApiKey | AuthSpecKind::Bearer | AuthSpecKind::OptionalBearer);
	let credential_sources: Box<[CredentialSourceSpec]> = match kind {
		AuthSpecKind::None => Box::new([]),
		AuthSpecKind::Basic => Box::new([CredentialSourceSpec::BasicEnvironment {
			username_names: Box::new([Str::from(format!("OMP_{env_base}_USERNAME"))]),
			password_names: Box::new([Str::from(format!("OMP_{env_base}_PASSWORD"))]),
		}]),
		_ => Box::new([
			CredentialSourceSpec::Environment {
				ordered_names: Box::new([Str::from(format!("OMP_{env_base}_API_KEY"))]),
			},
			CredentialSourceSpec::Stored,
		]),
	};
	Ok(AuthSpec {
		id: omp_catalog::AuthSpecId::from(format!("{provider}-configured-auth")),
		kind,
		header_name: bearer.then(|| Str::new_static("authorization")),
		query_parameter: None,
		prefix: bearer.then(|| Str::new_static("Bearer ")),
		sealed_body: None,
		scopes: Box::new([]),
		audience: None,
		account_scope: AccountScope::Provider,
		credential_sources,
		oauth: None,
		signing: None,
	})
}

fn configured_model_record(
	catalog: &omp_catalog::Catalog,
	key: ModelKey,
	wire: &str,
	model: &ModelConfig,
	definition: &ProviderConfig,
	route: RouteId,
	source: &ProvenanceSource,
) -> ModelSpec {
	let (template_provider, _) = configured_provider_template(catalog, definition);
	let template = catalog
		.models()
		.iter()
		.find(|candidate| candidate.routes.contains(&route))
		.or_else(|| {
			catalog.models().iter().find(|candidate| {
				candidate
					.routes
					.iter()
					.any(|route| template_provider.routes.contains(route))
			})
		})
		.or_else(|| catalog.models().first())
		.expect("embedded catalog has a model template");
	ModelSpec {
		key,
		class: ClassId::from(wire),
		display_name: model.name.clone().unwrap_or_else(|| Str::new(wire)),
		wire_ids: Box::new([(route.clone(), WireModelId::from(wire))]),
		routes: Box::new([route]),
		capabilities: configured_chat_capabilities(),
		limits: ModelLimits::default(),
		thinking: None,
		thinking_routing: ThinkingRouting::default(),
		wire_policy: template.wire_policy.clone(),
		context: ContextStrategy::Replay,
		pricing: Pricing::default(),
		catalog_metrics: Default::default(),
		availability: ModelAvailability::Available,
		provenance: ModelProvenance {
			sources:          Box::new([source.clone()]),
			updated_at_ms:    None,
			blocked_until_ms: None,
			deprecated:       false,
		},
		context_promotion_target: None,
		compaction_model: None,
		edit_revision: None,
		remote_compaction: None,
		premium_multiplier_millionths: None,
	}
}

fn parse_modalities(
	value: &toml::Value,
	provider: &str,
	model: &str,
) -> Result<ModalityBits, ModelsConfigError> {
	let values = value
		.as_array()
		.ok_or_else(|| ModelsConfigError::InvalidFact {
			provider: Str::new(provider),
			model:    Str::new(model),
			field:    "input",
		})?;
	let mut modalities = ModalityBits::empty();
	for value in values {
		let modality = value
			.as_str()
			.ok_or_else(|| ModelsConfigError::InvalidFact {
				provider: Str::new(provider),
				model:    Str::new(model),
				field:    "input",
			})?;
		match modality {
			"text" => modalities.insert(ModalityBits::TEXT),
			"image" => modalities.insert(ModalityBits::IMAGE),
			"audio" => modalities.insert(ModalityBits::AUDIO),
			"video" => modalities.insert(ModalityBits::VIDEO),
			"document" => modalities.insert(ModalityBits::DOCUMENT),
			_ => {
				return Err(ModelsConfigError::InvalidFact {
					provider: Str::new(provider),
					model:    Str::new(model),
					field:    "input",
				});
			},
		}
	}
	Ok(modalities)
}

fn parse_multiplier(value: &str) -> Option<u64> {
	let value = value.trim();
	let (whole, fractional) = value.split_once('.').unwrap_or((value, ""));
	if whole.is_empty()
		|| fractional.len() > 6
		|| !whole.bytes().all(|byte| byte.is_ascii_digit())
		|| !fractional.bytes().all(|byte| byte.is_ascii_digit())
	{
		return None;
	}
	let whole = whole.parse::<u64>().ok()?;
	let fractional = if fractional.is_empty() {
		0
	} else {
		fractional
			.parse::<u64>()
			.ok()?
			.checked_mul(10_u64.pow(u32::try_from(6_usize.saturating_sub(fractional.len())).ok()?))?
	};
	whole
		.checked_mul(PremiumMultiplier::SCALE)?
		.checked_add(fractional)
}

/// Resolves implicit loopback and explicitly configured provider probes.
///
/// Explicit configuration replaces the implicit endpoint for the same provider.
/// Header values are resolved only here, never copied into catalog or cache
/// records.
pub fn discovery_probes(
	config: Option<&ModelsConfig>,
	catalog: &omp_catalog::Catalog,
) -> Result<Vec<omp_ai::discovery::DiscoveryProbe>, ModelsConfigError> {
	use omp_ai::discovery::{
		DiscoveryEndpointKind, DiscoveryProbe, ProxyDiscoveryRoutes,
		configured_endpoint_with_options, known_loopback_endpoints,
	};

	let mut probes = BTreeMap::<ProviderId, DiscoveryProbe>::new();
	for endpoint in known_loopback_endpoints() {
		let endpoint = environment_discovery_endpoint(endpoint)?;
		let provider = match endpoint.kind {
			DiscoveryEndpointKind::Ollama => ProviderId::from("ollama"),
			DiscoveryEndpointKind::LlamaCpp => ProviderId::from("llama.cpp"),
			DiscoveryEndpointKind::LmStudio => ProviderId::from("lm-studio"),
			DiscoveryEndpointKind::LiteLlm
			| DiscoveryEndpointKind::OpenAi
			| DiscoveryEndpointKind::Proxy => continue,
		};
		let Some(route) = catalog
			.provider(&provider)
			.and_then(|provider| provider.routes.first())
			.cloned()
		else {
			continue;
		};
		let headers = catalog_route_headers(catalog, &route);
		probes.insert(provider.clone(), DiscoveryProbe {
			provider,
			route,
			proxy_routes: None,
			headers,
			endpoint,
		});
	}
	let Some(config) = config else {
		return Ok(probes.into_values().collect());
	};
	validate_discovery(config)?;
	for (provider_name, definition) in &config.providers {
		let provider = ProviderId::from(provider_name.as_str());
		let implicit = probes.get(&provider);
		let kind = definition
			.discovery
			.as_ref()
			.map(|discovery| match discovery.kind {
				ProviderDiscoveryKind::Ollama => DiscoveryEndpointKind::Ollama,
				ProviderDiscoveryKind::LlamaCpp => DiscoveryEndpointKind::LlamaCpp,
				ProviderDiscoveryKind::LmStudio => DiscoveryEndpointKind::LmStudio,
				ProviderDiscoveryKind::LiteLlm => DiscoveryEndpointKind::LiteLlm,
				ProviderDiscoveryKind::OpenAiModelsList => DiscoveryEndpointKind::OpenAi,
				ProviderDiscoveryKind::Proxy => DiscoveryEndpointKind::Proxy,
			})
			.or_else(|| implicit.map(|probe| probe.endpoint.kind));
		let Some(kind) = kind else {
			continue;
		};
		if definition.discovery.is_none() && definition.base_url.is_none() {
			continue;
		}
		let base_url = definition
			.base_url
			.as_deref()
			.or_else(|| implicit.map(|probe| probe.endpoint.base_url.as_str()))
			.or_else(|| match kind {
				DiscoveryEndpointKind::Ollama => Some("http://127.0.0.1:11434"),
				DiscoveryEndpointKind::LlamaCpp => Some("http://127.0.0.1:8080"),
				DiscoveryEndpointKind::LmStudio => Some("http://127.0.0.1:1234"),
				DiscoveryEndpointKind::LiteLlm => Some("http://127.0.0.1:4000/v1"),
				DiscoveryEndpointKind::OpenAi | DiscoveryEndpointKind::Proxy => None,
			})
			.ok_or_else(|| ModelsConfigError::MissingDiscoveryBaseUrl {
				provider: provider_name.clone(),
			})?;
		let timeout_ms = definition
			.discovery
			.as_ref()
			.and_then(|discovery| discovery.timeout_ms);
		let inject_v1 = definition
			.discovery
			.as_ref()
			.and_then(|discovery| discovery.inject_v1);
		let endpoint = configured_endpoint_with_options(kind, base_url, timeout_ms, inject_v1)
			.map_err(|source| ModelsConfigError::DiscoveryEndpoint {
				provider: provider_name.clone(),
				source,
			})?;
		let provider_definition = catalog.provider(&provider).ok_or_else(|| {
			ModelsConfigError::DiscoveryProviderMissing { provider: provider_name.clone() }
		})?;
		let route = provider_definition.routes.first().cloned().ok_or_else(|| {
			ModelsConfigError::DiscoveryRouteMissing { provider: provider_name.clone() }
		})?;
		let proxy_routes = (kind == DiscoveryEndpointKind::Proxy).then(|| {
			let mut routes =
				ProxyDiscoveryRoutes { openai: route.clone(), anthropic: route.clone() };
			for route_id in &provider_definition.routes {
				let Some(candidate) = catalog.route(route_id) else {
					continue;
				};
				if candidate.codec.as_str().contains("anthropic") {
					routes.anthropic = route_id.clone();
				} else if candidate.codec.as_str().contains("openai") {
					routes.openai = route_id.clone();
				}
			}
			routes
		});
		let mut headers = catalog_route_headers(catalog, &route);
		headers.extend(discovery_headers(provider_name, definition)?);
		probes.insert(provider.clone(), DiscoveryProbe {
			provider,
			route,
			proxy_routes,
			headers,
			endpoint,
		});
	}
	Ok(probes.into_values().collect())
}

/// Authenticates probes with the provider's stored `/login` credential.
///
/// [`discovery_probes`] resolves only static, `$ENV`, and
/// `OMP_<PROVIDER>_API_KEY` headers, so a provider whose key lives in the
/// encrypted store probed its model list anonymously and failed. A probe
/// whose route authentication lists [`CredentialSourceSpec::Stored`] and that
/// carries no credential header yet leases the provider's most recently
/// updated stored account and applies it with the route's header placement.
/// Returns how many probes were authenticated.
pub async fn authenticate_probes_from_store(
	probes: &mut [DiscoveryProbe],
	catalog: &omp_catalog::Catalog,
	store: &Arc<CredentialStore>,
	now: SystemTime,
) -> usize {
	let Ok(mut accounts) = store.list_metadata() else {
		return 0;
	};
	accounts.sort_by_key(|account| Reverse(account.updated_at_ms));
	let source = StoredCredentialSource::new(store.clone());
	let mut authenticated = 0;
	for probe in probes {
		let Some(spec) = catalog
			.route(&probe.route)
			.and_then(|route| catalog.auth_spec(&route.auth))
		else {
			continue;
		};
		let Some(header) = spec.header_name.as_ref() else {
			continue;
		};
		if probe.headers.contains_key(header.as_str())
			|| !spec
				.credential_sources
				.iter()
				.any(|source| matches!(source, CredentialSourceSpec::Stored))
		{
			continue;
		}
		let placement = HeaderPlacement {
			name:   header.clone(),
			prefix: spec.prefix.clone().unwrap_or_default(),
		};
		let provider = probe.provider.as_str();
		for account in accounts.iter().filter(|account| {
			account
				.account_id
				.as_str()
				.strip_prefix(provider)
				.is_some_and(|rest| rest.starts_with(':'))
		}) {
			let need = CredentialNeed {
				spec:        spec.id.clone(),
				account:     Some(account.account_id.clone()),
				principal:   None,
				valid_after: now,
			};
			let Ok(lease) = source.lease(need).await else {
				continue;
			};
			if lease.apply_header(&placement, &mut probe.headers).is_ok() {
				authenticated += 1;
				break;
			}
		}
	}
	authenticated
}

/// Applies process-level runtime metadata overrides after native probes.
///
/// Ollama's `OLLAMA_CONTEXT_LENGTH` is the served context selected by its
/// Responses compatibility layer and therefore outranks `/api/show`.
pub fn apply_runtime_discovery_overrides(
	probe: &omp_ai::discovery::DiscoveryProbe,
	rows: &mut [omp_catalog::DiscoveredModel],
) {
	if probe.endpoint.kind != omp_ai::discovery::DiscoveryEndpointKind::Ollama {
		return;
	}
	let Some(context) = env::var("OLLAMA_CONTEXT_LENGTH")
		.ok()
		.and_then(|value| value.trim().parse::<u64>().ok())
		.filter(|value| *value > 0)
	else {
		return;
	};
	for row in rows {
		let limits = row.declared_limits.get_or_insert(ModelLimits {
			context_window:        None,
			maximum_input_tokens:  None,
			maximum_output_tokens: None,
			maximum_batch:         None,
		});
		limits.context_window = Some(context);
		limits.maximum_output_tokens =
			Some(limits.maximum_output_tokens.unwrap_or(32_768).min(context));
	}
}

fn environment_discovery_endpoint(
	endpoint: omp_ai::discovery::DiscoveryEndpoint,
) -> Result<omp_ai::discovery::DiscoveryEndpoint, ModelsConfigError> {
	use omp_ai::discovery::{DiscoveryEndpointKind, configured_endpoint_with_options};

	let base_url = match endpoint.kind {
		DiscoveryEndpointKind::Ollama => env::var("OLLAMA_BASE_URL")
			.ok()
			.filter(|value| !value.trim().is_empty())
			.or_else(|| {
				env::var("OLLAMA_HOST")
					.ok()
					.filter(|value| !value.trim().is_empty())
					.map(normalize_ollama_host)
			}),
		DiscoveryEndpointKind::LlamaCpp => env::var("LLAMA_CPP_BASE_URL")
			.ok()
			.filter(|value| !value.trim().is_empty()),
		DiscoveryEndpointKind::LmStudio => env::var("LM_STUDIO_BASE_URL")
			.ok()
			.filter(|value| !value.trim().is_empty()),
		DiscoveryEndpointKind::LiteLlm
		| DiscoveryEndpointKind::OpenAi
		| DiscoveryEndpointKind::Proxy => None,
	};
	let Some(base_url) = base_url else {
		return Ok(endpoint);
	};
	configured_endpoint_with_options(endpoint.kind, base_url.trim(), None, None).map_err(|source| {
		ModelsConfigError::DiscoveryEndpoint {
			provider: Str::new_static(<&'static str>::from(endpoint.kind)),
			source,
		}
	})
}

fn normalize_ollama_host(value: String) -> String {
	let value = value.trim();
	let candidate = if value.contains("://") {
		value.to_owned()
	} else if value.starts_with("//") {
		format!("http:{value}")
	} else if value.starts_with(':') {
		format!("http://127.0.0.1{value}")
	} else {
		format!("http://{value}")
	};
	let Ok(mut url) = url::Url::parse(&candidate) else {
		return candidate;
	};
	if url.scheme() == "http" && url.port().is_none() {
		let _ = url.set_port(Some(11434));
	}
	url.as_str().trim_end_matches('/').to_owned()
}

fn catalog_route_headers(catalog: &omp_catalog::Catalog, route: &RouteId<str>) -> http::HeaderMap {
	let mut headers = http::HeaderMap::new();
	let Some(route) = catalog.route(route) else {
		return headers;
	};
	let Some(auth) = catalog.auth_spec(&route.auth) else {
		return headers;
	};
	let credential = auth.credential_sources.iter().find_map(|source| {
		let CredentialSourceSpec::Environment { ordered_names } = source else {
			return None;
		};
		ordered_names.iter().find_map(|name| {
			env::var_os(name.as_str())
				.and_then(|value| value.into_string().ok())
				.filter(|value| !value.trim().is_empty())
		})
	});
	let Some(credential) = credential else {
		return headers;
	};
	let Some(header_name) = auth.header_name.as_deref() else {
		return headers;
	};
	let Ok(header_name) = http::HeaderName::from_bytes(header_name.as_bytes()) else {
		return headers;
	};
	let prefix = auth.prefix.as_deref().unwrap_or_default();
	let mut value = Vec::with_capacity(prefix.len() + credential.len());
	value.extend_from_slice(prefix.as_bytes());
	value.extend_from_slice(credential.as_bytes());
	if let Ok(value) = http::HeaderValue::from_bytes(&value) {
		headers.insert(header_name, value);
	}
	headers
}

fn discovery_headers(
	provider: &str,
	definition: &ProviderConfig,
) -> Result<http::HeaderMap, ModelsConfigError> {
	let mut headers = http::HeaderMap::new();
	for (name, source) in definition.header_sources() {
		let value = match source {
			HeaderValueSource::Public(value) => Some(value),
			HeaderValueSource::Environment(variable) => env::var_os(variable.as_str())
				.and_then(|value| value.into_string().ok())
				.map(Str::from),
			HeaderValueSource::Command(_) => None,
		};
		let Some(value) = value else {
			continue;
		};
		let name = http::HeaderName::from_bytes(name.as_bytes()).map_err(|source| {
			ModelsConfigError::DiscoveryHeaderName { provider: Str::new(provider), source }
		})?;
		let value = http::HeaderValue::from_bytes(value.as_bytes()).map_err(|source| {
			ModelsConfigError::DiscoveryHeaderValue {
				provider: Str::new(provider),
				header: Str::new(name.as_str()),
				source,
			}
		})?;
		headers.insert(name, value);
	}
	if definition
		.auth
		.as_deref()
		.is_some_and(|auth| !auth.eq_ignore_ascii_case("none"))
		&& !headers.contains_key(http::header::AUTHORIZATION)
	{
		let env_name = format!(
			"OMP_{}_API_KEY",
			provider
				.chars()
				.map(|character| {
					if character.is_ascii_alphanumeric() {
						character.to_ascii_uppercase()
					} else {
						'_'
					}
				})
				.collect::<String>()
		);
		if let Some(secret) = env::var_os(env_name).and_then(|value| value.into_string().ok()) {
			let mut value = Vec::with_capacity("Bearer ".len() + secret.len());
			value.extend_from_slice(b"Bearer ");
			value.extend_from_slice(secret.as_bytes());
			let value = http::HeaderValue::from_bytes(&value).map_err(|source| {
				ModelsConfigError::DiscoveryHeaderValue {
					provider: Str::new(provider),
					header: Str::new_static("authorization"),
					source,
				}
			})?;
			headers.insert(http::header::AUTHORIZATION, value);
		}
	}
	Ok(headers)
}

/// Publishes the complete native user-config generation atomically.
pub fn publish_user_overlay(
	store: &OverlayStore,
	config: &ModelsConfig,
) -> Result<(), ModelsConfigError> {
	store.replace(OverlaySource::UserConfig, lower_user_overlay(config)?);
	Ok(())
}
/// Native model-config decoding failures.
#[derive(Debug, thiserror::Error)]
pub enum ModelsConfigError {
	/// A configured provider/model fact is malformed or internally inconsistent.
	#[error("invalid `{field}` for configured model {provider}/{model}")]
	InvalidFact {
		/// Provider containing the invalid fact.
		provider: Str,
		/// Model containing the invalid fact.
		model:    Str,
		/// Stable field name.
		field:    &'static str,
	},
	/// A configured provider `auth` mode names no supported specification kind.
	#[error(
		"invalid `auth` for configured provider {provider}; expected one of none, api_key, bearer, \
		 optional_bearer, basic"
	)]
	InvalidAuth {
		/// Provider containing the invalid auth mode.
		provider: Str,
	},
	/// A configured provider discovery policy is invalid.
	#[error("invalid `discovery` for configured provider {provider}")]
	InvalidProviderDiscovery {
		/// Provider containing the invalid discovery policy.
		provider: Str,
	},
	/// An explicit discovery provider omitted its base URL.
	#[error("configured discovery provider {provider} requires `baseUrl`")]
	MissingDiscoveryBaseUrl {
		/// Provider missing the URL.
		provider: Str,
	},
	/// A configured discovery endpoint is invalid.
	#[error("invalid discovery endpoint for configured provider {provider}")]
	DiscoveryEndpoint {
		/// Provider containing the endpoint.
		provider: Str,
		/// Typed endpoint validation failure.
		#[source]
		source:   omp_ai::discovery::EndpointError,
	},
	/// The configured provider did not materialize in the catalog.
	#[error("configured discovery provider {provider} is missing from the catalog")]
	DiscoveryProviderMissing {
		/// Missing provider.
		provider: Str,
	},
	/// The configured provider has no route for discovered models.
	#[error("configured discovery provider {provider} has no route")]
	DiscoveryRouteMissing {
		/// Provider missing a route.
		provider: Str,
	},
	/// A configured discovery header name is invalid.
	#[error("configured discovery provider {provider} has an invalid header name")]
	DiscoveryHeaderName {
		/// Provider containing the header.
		provider: Str,
		/// Header parser failure.
		#[source]
		source:   http::header::InvalidHeaderName,
	},
	/// A configured discovery header value is invalid and has been redacted.
	#[error("configured discovery provider {provider} has an invalid value for header {header}")]
	DiscoveryHeaderValue {
		/// Provider containing the header.
		provider: Str,
		/// Non-secret header name.
		header:   Str,
		/// Header parser failure, which never contains the value.
		#[source]
		source:   http::header::InvalidHeaderValue,
	},
	/// Reading the configured source failed.
	#[error(transparent)]
	Io(#[from] io::Error),
	/// The TOML source was malformed.
	#[error(transparent)]
	Toml(#[from] de::Error),
	/// A legacy YAML source was malformed.
	#[error(transparent)]
	Yaml(#[from] serde_yaml::Error),
	/// A legacy JSON/JSONC source was malformed.
	#[error(transparent)]
	Json(#[from] omp_core::slopjson::ParseError),
	/// Native TOML encoding failed.
	#[error(transparent)]
	Encode(#[from] ser::Error),
	/// A v1 model list entry has no `id` to key it by.
	#[error("a model listed under provider {provider} in the v1 model config has no `id`")]
	LegacyModelWithoutId {
		/// Provider listing the model.
		provider: Str,
	},
	/// The configured catalog that model references resolve against did not
	/// materialize.
	#[error("the configured catalog for resolving model references is invalid")]
	ReferenceCatalog {
		/// Catalog validation failure.
		#[source]
		source: omp_catalog::snapshot::SnapshotError,
	},
	/// The configuration root could not be resolved.
	#[error(transparent)]
	ConfigRoot(#[from] omp_core::dirs::DataDirError),
	/// Saving an imported v1 key to the encrypted store failed.
	#[error(transparent)]
	CredentialStore(#[from] omp_ai::auth::StoreError),
}

#[cfg(test)]
mod tests {
	use super::*;

	/// A config root and a home holding a v1 `~/.omp/agent`, both temporary.
	fn location(root: &Path) -> ModelsConfigLocation {
		let data = root.join("data");
		let agent = root.join("home/.omp/agent");
		fs::create_dir_all(&data).expect("data dir");
		fs::create_dir_all(&agent).expect("agent dir");
		ModelsConfigLocation { config_dir: root.join("config"), legacy_dirs: vec![data, agent] }
	}

	const V1_MODELS_YML: &str = concat!(
		"providers:\n",
		"  easycliproxy:\n",
		"    baseUrl: https://proxy.example/v1\n",
		"    apiKey: sk-literal-key\n",
		"    discovery:\n",
		"      type: openai-models-list\n",
		"    models:\n",
		"      - id: claude-opus-5\n",
		"        name: Claude Opus 5\n",
		"        contextWindow: 1000000\n",
		"        premiumMultiplier: 0.5\n",
		"      - id: gpt-5.5\n",
		"  envkey:\n",
		"    baseUrl: https://env.example/v1\n",
		"    apiKey: EASY_PROXY_KEY\n",
		"  cmdkey:\n",
		"    baseUrl: https://cmd.example/v1\n",
		"    apiKey: '!security find-generic-password -w -s proxy'\n",
		"  keyless:\n",
		"    baseUrl: http://localhost:4000/v1\n",
		"    auth: none\n",
	);

	#[test]
	fn v1_models_yml_is_imported_into_the_config_root_once() {
		let root = tempfile::tempdir().expect("directory");
		let location = location(root.path());
		let v1 = location.legacy_dirs[1].join("models.yml");
		fs::write(&v1, V1_MODELS_YML).expect("v1 config");

		let imported = load_or_import_legacy(&location)
			.expect("import")
			.expect("config");
		assert_eq!(imported.source, ModelsConfigSource::LegacyYaml(v1.clone()));
		let proxy = &imported.config.providers["easycliproxy"];
		assert_eq!(proxy.models.keys().map(Str::as_str).collect::<Vec<_>>(), [
			"claude-opus-5",
			"gpt-5.5"
		]);
		let opus = &proxy.models["claude-opus-5"];
		assert_eq!(opus.context_window, Some(1_000_000));
		assert_eq!(opus.premium_multiplier.as_deref(), Some("0.5"));
		// v1 defaulted to `apiKey`; `auth: none` stays none.
		assert_eq!(proxy.auth.as_deref(), Some("apiKey"));
		assert_eq!(imported.config.providers["keyless"].auth.as_deref(), Some("none"));

		// The native file lives in the config root and never holds the key;
		// the v1 file is left exactly as it was.
		let native = fs::read_to_string(location.config_dir.join("models.toml")).expect("native");
		assert!(!native.contains("sk-literal-key"), "{native}");
		assert_eq!(fs::read_to_string(&v1).expect("v1"), V1_MODELS_YML);
		// The file keeps the provider's own ids; lowering scopes them.
		assert!(native.contains("[providers.easycliproxy.models.claude-opus-5]"), "{native}");
		let overlay = lower_user_overlay(&imported.config).expect("imported config lowers");
		let catalog = omp_catalog::Catalog::embedded()
			.with_overlay_stack(
				&OverlayStack::from_layers([(OverlaySource::UserConfig, overlay)]),
				UnsafeTrustScope::ALL,
			)
			.expect("imported config materializes");
		let opus = catalog
			.model(ModelKey::from_ref("easycliproxy/claude-opus-5"))
			.expect("imported model is provider-scoped");
		assert_eq!(opus.display_name, "Claude Opus 5");
		assert_eq!(opus.limits.context_window, Some(1_000_000));
		assert_eq!(opus.wire_ids[0].1.as_str(), "claude-opus-5");
		assert!(
			catalog
				.model(ModelKey::from_ref("easycliproxy/gpt-5.5"))
				.is_some()
		);

		let native = load_or_import_legacy(&location)
			.expect("native")
			.expect("config");
		assert!(matches!(native.source, ModelsConfigSource::NativeToml(_)));
	}

	#[test]
	fn an_earlier_omp2_models_toml_moves_before_v1_is_considered() {
		let root = tempfile::tempdir().expect("directory");
		let location = location(root.path());
		fs::write(
			location.legacy_dirs[0].join("models.toml"),
			"[providers.demo]\nbaseUrl='https://example.test/v1'\n[providers.demo.models.fast]\ncontextWindow=4096\n",
		)
		.expect("old native");
		fs::write(location.legacy_dirs[1].join("models.yml"), V1_MODELS_YML).expect("v1 config");

		let moved = load_or_import_legacy(&location)
			.expect("move")
			.expect("config");
		assert!(matches!(moved.source, ModelsConfigSource::MovedToml(_)));
		assert!(moved.config.providers.contains_key("demo"));
		assert!(!moved.config.providers.contains_key("easycliproxy"));
		assert!(location.config_dir.join("models.toml").is_file());
	}

	#[test]
	fn an_explicit_state_directory_isolates_model_config() {
		let state = tempfile::tempdir().expect("directory");
		assert_eq!(
			ModelsConfigLocation::resolve(state.path()).expect("location"),
			ModelsConfigLocation { config_dir: state.path().to_owned(), legacy_dirs: Vec::new() }
		);
	}

	#[test]
	fn nothing_to_import_is_remembered() {
		let root = tempfile::tempdir().expect("directory");
		let location = location(root.path());
		assert!(load_or_import_legacy(&location).expect("empty").is_none());
		// A v1 file that appears later is not picked up: the marker records
		// that the one-time import already ran.
		fs::write(location.legacy_dirs[1].join("models.yml"), V1_MODELS_YML).expect("v1 config");
		assert!(load_or_import_legacy(&location).expect("marker").is_none());
	}

	#[test]
	fn a_v1_model_list_entry_without_an_id_is_a_typed_error() {
		let root = tempfile::tempdir().expect("directory");
		let location = location(root.path());
		fs::write(
			location.legacy_dirs[1].join("models.yml"),
			"providers:\n  demo:\n    models:\n      - name: Nameless\n",
		)
		.expect("v1 config");
		assert!(matches!(
			load_or_import_legacy(&location),
			Err(ModelsConfigError::LegacyModelWithoutId { provider }) if provider == "demo"
		));
	}

	#[test]
	fn literal_v1_api_keys_move_into_the_encrypted_store_once() {
		use std::sync::Arc;

		use futures::{FutureExt as _, future::BoxFuture};
		use omp_ai::{
			AccountId,
			account::AccountPool,
			answer::AccountSummary,
			auth::{
				AuthManager, AuthRefreshEngine, CredentialBroker, CredentialBrokerEngines,
				CredentialStore, HeadlessKeySource, KeyId,
			},
		};

		#[derive(Clone, Copy)]
		struct UnusedLogin(omp_ai::call::AuthMethod);
		impl omp_ai::auth::AuthLoginEngine for UnusedLogin {
			fn method(&self) -> omp_ai::call::AuthMethod {
				self.0
			}

			fn supports(&self, _: &ProviderId<str>) -> bool {
				true
			}

			fn begin(
				&self,
				_: omp_ai::call::LoginRequest,
				_: omp_catalog::AuthSpecId,
			) -> BoxFuture<'_, Result<omp_ai::answer::AuthSession, omp_ai::Error>> {
				futures::future::pending().boxed()
			}
		}

		struct UnusedRefresh;
		impl AuthRefreshEngine for UnusedRefresh {
			fn refresh(&self, _: AccountId) -> BoxFuture<'_, Result<AccountSummary, omp_ai::Error>> {
				futures::future::pending().boxed()
			}
		}

		let root = tempfile::tempdir().expect("directory");
		let location = location(root.path());
		fs::write(location.legacy_dirs[1].join("models.yml"), V1_MODELS_YML).expect("v1 config");
		let config = load_or_import_legacy(&location)
			.expect("import")
			.expect("config");
		let overlay = lower_user_overlay(&config.config).expect("overlay");
		let catalog = Arc::new(
			omp_catalog::Catalog::embedded()
				.with_overlay_stack(
					&omp_catalog::OverlayStack::from_layers([(OverlaySource::UserConfig, overlay)]),
					omp_catalog::UnsafeTrustScope::ALL,
				)
				.expect("catalog"),
		);
		let store = Arc::new(
			CredentialStore::open(
				root.path().join("credentials.sqlite"),
				Arc::new(HeadlessKeySource::new(KeyId::new("legacy-key-import"), [0x33; 32])),
			)
			.expect("store"),
		);
		let broker =
			CredentialBroker::system(&catalog, CredentialBrokerEngines::default()).expect("broker");
		let manager = AuthManager::new(
			catalog,
			store.clone(),
			broker,
			AccountPool::new(),
			[
				omp_ai::call::AuthMethod::ApiKey,
				omp_ai::call::AuthMethod::OAuthPkce,
				omp_ai::call::AuthMethod::OAuthDevice,
				omp_ai::call::AuthMethod::ApplicationDefault,
				omp_ai::call::AuthMethod::AwsCredentialChain,
				omp_ai::call::AuthMethod::SessionToken,
			]
			.into_iter()
			.map(|method| Arc::new(UnusedLogin(method)) as Arc<dyn omp_ai::auth::AuthLoginEngine>)
			.collect(),
			Arc::new(UnusedRefresh),
		)
		.expect("manager");
		let control = manager.control_handle();

		let mut report = import_legacy_api_keys(&location, &control).expect("key import");
		report.sort_by(|left, right| format!("{left:?}").cmp(&format!("{right:?}")));
		assert_eq!(report, [
			LegacyApiKeyImport::NeedsEnvironment { provider: Str::new("cmdkey") },
			LegacyApiKeyImport::NeedsEnvironment { provider: Str::new("envkey") },
			LegacyApiKeyImport::Stored { provider: Str::new("easycliproxy") },
		]);
		let stored = store.list_metadata().expect("metadata");
		assert_eq!(
			stored
				.iter()
				.map(|row| (row.account_id.as_str(), row.kind.as_str()))
				.collect::<Vec<_>>(),
			[("easycliproxy:models-yml", "api-key")]
		);
		assert_eq!(
			control
				.accounts(Some(ProviderId::from_ref("easycliproxy")))
				.len(),
			1
		);
		// The marker makes the import one-shot.
		assert!(
			import_legacy_api_keys(&location, &control)
				.expect("second run")
				.is_empty()
		);
	}

	#[test]
	fn representative_models_toml_decodes_and_publishes() {
		let value: ModelsConfig = toml::from_str(
			"[providers.demo]\nbaseUrl='https://example.test/v1'\nauth='apiKey'\ndisableStrictTools=true\n[providers.demo.models.fast]\ncontextWindow=128000\nmaxTokens=8192\npremiumMultiplier='0.25'\ncontextPromotionTarget='large'\n",
		).expect("decode");
		let provider = &value.providers["demo"];
		assert_eq!(provider.base_url.as_deref(), Some("https://example.test/v1"));
		let model = &provider.models["fast"];
		assert_eq!(model.context_window, Some(128000));
		assert_eq!(model.max_tokens, Some(8192));
		let store = OverlayStore::default();
		publish_user_overlay(&store, &value).expect("publish");
		assert_eq!(store.load().sources(), &[OverlaySource::UserConfig]);
	}

	#[test]
	fn unknown_provider_lowers_complete_provider_route_and_model_records() {
		let value: ModelsConfig = toml::from_str(
			"[providers.demo]\nbaseUrl='https://example.test/v1'\nauth='apiKey'\n[providers.demo.models.fast]\napi='openai-completions'\ncontextWindow=128000\n",
		)
		.expect("decode");
		let overlay = lower_user_overlay(&value).expect("overlay");
		let stack = omp_catalog::OverlayStack::from_layers([(OverlaySource::UserConfig, overlay)]);
		let catalog = omp_catalog::Catalog::embedded()
			.with_overlay_stack(&stack, omp_catalog::UnsafeTrustScope::ALL)
			.expect("materialize configured provider");
		let provider = catalog
			.provider(ProviderId::from_ref("demo"))
			.expect("provider");
		assert_eq!(provider.routes.as_ref(), &[RouteId::from("demo-configured")]);
		let route = catalog.route(&provider.routes[0]).expect("route");
		assert_eq!(route.endpoint.base_url, "https://example.test/v1");
		assert_eq!(route.provider.as_str(), "demo");
		let model = catalog
			.models()
			.iter()
			.find(|model| model.key.as_str() == "demo/fast")
			.expect("model");
		assert_eq!(model.routes.as_ref(), provider.routes.as_ref());
		assert_eq!(model.limits.context_window, Some(128_000));
	}
	#[test]
	fn configured_provider_is_chat_capable_and_speaks_its_declared_api() {
		let value: ModelsConfig = toml::from_str(
			"[providers.demo]\nbaseUrl='http://127.0.0.1:9/v1'\nauth='none'\n[providers.demo.models.fast]\napi='openai-completions'\nsupportsTools=true\n",
		)
		.expect("decode");
		let overlay = lower_user_overlay(&value).expect("overlay");
		let stack = omp_catalog::OverlayStack::from_layers([(OverlaySource::UserConfig, overlay)]);
		let catalog = omp_catalog::Catalog::embedded()
			.with_overlay_stack(&stack, omp_catalog::UnsafeTrustScope::ALL)
			.expect("materialize configured provider");
		let model = catalog
			.models()
			.iter()
			.find(|model| model.key.as_str() == "demo/fast")
			.expect("model");
		assert!(
			model
				.capabilities
				.operations
				.contains_kind(omp_catalog::OperationKind::Chat),
			"configured models must admit the chat operation"
		);
		let chat = model.capabilities.chat.as_ref().expect("chat block");
		assert!(matches!(chat.tools, Availability::Native(_)));
		let route = catalog
			.route(&RouteId::from("demo-configured"))
			.expect("route");
		assert_eq!(route.codec.as_str(), "openai-chat");
		let auth = catalog.auth_spec(&route.auth).expect("interned auth spec");
		assert_eq!(auth.kind, AuthSpecKind::None);
		assert!(auth.credential_sources.is_empty());
	}

	#[test]
	fn configured_auth_modes_normalize_and_reject_unknown_kinds() {
		for (input, kind) in [
			("none", AuthSpecKind::None),
			("apiKey", AuthSpecKind::ApiKey),
			("api-key", AuthSpecKind::ApiKey),
			("API_KEY", AuthSpecKind::ApiKey),
			("optionalBearer", AuthSpecKind::OptionalBearer),
			("basic", AuthSpecKind::Basic),
		] {
			let spec = configured_auth_spec("demo", input).expect(input);
			assert_eq!(spec.kind, kind, "{input}");
		}
		let spec = configured_auth_spec("my-provider", "bearer").expect("bearer");
		assert!(matches!(
			&spec.credential_sources[0],
			CredentialSourceSpec::Environment { ordered_names }
				if ordered_names.as_ref() == ["OMP_MY_PROVIDER_API_KEY"]
		));
		assert!(matches!(
			configured_auth_spec("demo", "oauth"),
			Err(ModelsConfigError::InvalidAuth { .. })
		));
	}

	#[test]
	fn configured_discovery_overrides_implicit_endpoint_with_typed_policy() {
		let config: ModelsConfig = toml::from_str(
			"[providers.ollama]\nbaseUrl='http://192.168.1.20:11434'\ndiscovery={type='ollama',timeoutMs=2500}\n",
		)
		.expect("decode");
		let catalog = omp_catalog::Catalog::embedded();
		let probes = discovery_probes(Some(&config), catalog).expect("discovery probes");
		let ollama = probes
			.iter()
			.find(|probe| probe.provider.as_str() == "ollama")
			.expect("Ollama probe");
		assert_eq!(ollama.endpoint.base_url, "http://192.168.1.20:11434");
		assert_eq!(ollama.endpoint.deadline(), std::time::Duration::from_millis(2500));
		assert_eq!(
			probes
				.iter()
				.filter(|probe| probe.provider.as_str() == "ollama")
				.count(),
			1
		);
	}

	#[test]
	fn proxy_discovery_materializes_both_wire_routes() {
		let config: ModelsConfig = toml::from_str(
			"[providers.demo]\nbaseUrl='https://proxy.example/v1'\nauth='apiKey'\ndiscovery={type='proxy'}\n",
		)
		.expect("decode");
		let overlay = lower_user_overlay(&config).expect("overlay");
		let catalog = omp_catalog::Catalog::embedded()
			.with_overlay_stack(
				&omp_catalog::OverlayStack::from_layers([(OverlaySource::UserConfig, overlay)]),
				omp_catalog::UnsafeTrustScope::ALL,
			)
			.expect("catalog");
		let provider = catalog
			.provider(ProviderId::from_ref("demo"))
			.expect("provider");
		assert_eq!(provider.routes.len(), 2);
		let codecs = provider
			.routes
			.iter()
			.filter_map(|route| catalog.route(route))
			.map(|route| route.codec.as_str())
			.collect::<Vec<_>>();
		assert!(codecs.iter().any(|codec| codec.contains("openai")));
		assert!(codecs.iter().any(|codec| codec.contains("anthropic")));
		let probes = discovery_probes(Some(&config), &catalog).expect("probes");
		let proxy = probes
			.iter()
			.find(|probe| probe.provider.as_str() == "demo")
			.expect("proxy");
		let routes = proxy.proxy_routes.as_ref().expect("proxy routes");
		assert_ne!(routes.openai, routes.anthropic);
	}

	#[tokio::test]
	async fn stored_login_credential_authenticates_configured_discovery_probe() {
		use omp_ai::{
			AccountId, PrincipalId,
			auth::{CredentialOrigin, CredentialWrite, HeadlessKeySource, KeyId},
		};
		use omp_core::SecretBox;

		let config: ModelsConfig = toml::from_str(
			"[providers.demo]\nbaseUrl='https://proxy.example/v1'\nauth='apiKey'\ndiscovery={type='openai-models-list'}\n\
			 [providers.keyless]\nbaseUrl='https://keyless.example/v1'\nauth='apiKey'\ndiscovery={type='openai-models-list'}\n\
			 [providers.preset]\nbaseUrl='https://preset.example/v1'\nauth='apiKey'\ndiscovery={type='openai-models-list'}\nheaders={authorization='Bearer preset-header'}\n",
		)
		.expect("decode");
		let overlay = lower_user_overlay(&config).expect("overlay");
		let catalog = omp_catalog::Catalog::embedded()
			.with_overlay_stack(
				&omp_catalog::OverlayStack::from_layers([(OverlaySource::UserConfig, overlay)]),
				omp_catalog::UnsafeTrustScope::ALL,
			)
			.expect("catalog");
		let directory = tempfile::tempdir().expect("directory");
		let keys = Arc::new(HeadlessKeySource::new(KeyId::new("discovery-probe-key"), [0x5a; 32]));
		let store = Arc::new(
			CredentialStore::open(directory.path().join("credentials.sqlite"), keys).expect("store"),
		);
		let write = |account: &str, secret: &str, now_ms: u64| {
			store
				.put(CredentialWrite {
					account_id: AccountId::from_ref(account),
					principal_id: PrincipalId::from_ref("login"),
					kind: "api-key",
					secret: &SecretBox::new(Box::new(secret.as_bytes().to_vec())),
					expires_at_ms: None,
					origin: CredentialOrigin::Persistent,
					now_ms,
					expected_generation: None,
				})
				.expect("write credential");
		};
		// The newest login wins; a provider whose name merely starts with
		// `demo` never lends its key; `preset` keeps its explicit header.
		write("demo:old", "stale-key", 1_000);
		write("demo:work", "live-key", 2_000);
		write("demo-other:work", "foreign-key", 3_000);
		write("preset:work", "stored-key", 3_000);

		let mut probes = discovery_probes(Some(&config), &catalog).expect("probes");
		let demo = |probes: &[DiscoveryProbe]| {
			probes
				.iter()
				.find(|probe| probe.provider.as_str() == "demo")
				.is_some_and(|probe| probe.headers.contains_key(http::header::AUTHORIZATION))
		};
		assert!(!demo(&probes), "env-only resolution leaves the stored login unused");
		let authenticated =
			authenticate_probes_from_store(&mut probes, &catalog, &store, SystemTime::now()).await;
		assert_eq!(authenticated, 1);
		let authorization = |provider: &str| {
			probes
				.iter()
				.find(|probe| probe.provider.as_str() == provider)
				.expect("probe")
				.headers
				.get(http::header::AUTHORIZATION)
				.map(|value| value.to_str().expect("ascii").to_owned())
		};
		assert_eq!(authorization("demo").as_deref(), Some("Bearer live-key"));
		assert_eq!(authorization("keyless"), None);
		assert_eq!(authorization("preset").as_deref(), Some("Bearer preset-header"));
	}

	#[test]
	fn openai_v1_injection_is_rejected_for_other_discovery_protocols() {
		let config: ModelsConfig = toml::from_str(
			"[providers.demo]\nbaseUrl='http://localhost:9000'\ndiscovery={type='proxy',injectV1=false}\n",
		)
		.expect("decode");
		assert!(matches!(
			lower_user_overlay(&config),
			Err(ModelsConfigError::InvalidProviderDiscovery { .. })
		));
	}
}
