//! The legacy model-config decoders behind the `models` and `models-keys`
//! steps, over temporary config roots and v1 homes.

use std::{fs, path::Path};

use omp_catalog::{ModelKey, OverlaySource, OverlayStack, ProviderId, UnsafeTrustScope};
use omp_core::Str;

use super::*;
use crate::discovery::models::lower_user_overlay;

/// A config root and a home holding a v1 `~/.omp/agent`, both temporary,
/// with the v1 profile found by the locator.
fn location(root: &Path) -> ModelsConfigLocation {
	let data = root.join("data");
	let home = root.join("home");
	fs::create_dir_all(&data).expect("data dir");
	fs::create_dir_all(home.join(".omp/agent")).expect("agent dir");
	let v1 =
		crate::v1_import::V1Source::new(crate::v1_import::V1Inputs { home, ..Default::default() })
			.layout(None);
	ModelsConfigLocation {
		config_dir:  root.join("config"),
		legacy_dirs: vec![data],
		v1:          Some(v1),
	}
}

/// What one applied import converted, and from where.
struct Imported {
	source: ModelsConfigSource,
	config: ModelsConfig,
}

/// Applies the one-time import; the written config when it ran.
fn apply(location: &ModelsConfigLocation) -> Result<Option<Imported>, ModelsImportError> {
	Ok(match import_legacy_models(location, ImportMode::Apply)? {
		LegacyModelsImport::Imported(source) => Some(Imported {
			source,
			config: crate::discovery::models::load_models_config(&location.native())?,
		}),
		LegacyModelsImport::NativePresent(_)
		| LegacyModelsImport::AlreadyImported
		| LegacyModelsImport::NothingToImport => None,
	})
}

/// The located v1 agent directory.
fn v1_agent(location: &ModelsConfigLocation) -> &Path {
	location.v1.as_ref().expect("v1 layout").agent_dir()
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
	let v1 = v1_agent(&location).join("models.yml");
	fs::write(&v1, V1_MODELS_YML).expect("v1 config");

	let imported = apply(&location).expect("import").expect("config");
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

	assert!(matches!(
		import_legacy_models(&location, ImportMode::Apply).expect("native"),
		LegacyModelsImport::NativePresent(_)
	));
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
	fs::write(v1_agent(&location).join("models.yml"), V1_MODELS_YML).expect("v1 config");

	let moved = apply(&location).expect("move").expect("config");
	assert!(matches!(moved.source, ModelsConfigSource::MovedToml(_)));
	assert!(moved.config.providers.contains_key("demo"));
	assert!(!moved.config.providers.contains_key("easycliproxy"));
	assert!(location.config_dir.join("models.toml").is_file());
}

#[test]
fn nothing_to_import_is_remembered() {
	let root = tempfile::tempdir().expect("directory");
	let location = location(root.path());
	assert!(apply(&location).expect("empty").is_none());
	// A v1 file that appears later is not picked up: the marker records
	// that the one-time import already ran.
	fs::write(v1_agent(&location).join("models.yml"), V1_MODELS_YML).expect("v1 config");
	assert!(apply(&location).expect("marker").is_none());
}

#[test]
fn a_v1_model_list_entry_without_an_id_is_a_typed_error() {
	let root = tempfile::tempdir().expect("directory");
	let location = location(root.path());
	fs::write(
		v1_agent(&location).join("models.yml"),
		"providers:\n  demo:\n    models:\n      - name: Nameless\n",
	)
	.expect("v1 config");
	assert!(matches!(
		apply(&location),
		Err(ModelsImportError::LegacyModelWithoutId { provider }) if provider == "demo"
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
	fs::write(v1_agent(&location).join("models.yml"), V1_MODELS_YML).expect("v1 config");
	let config = apply(&location).expect("import").expect("config");
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

	let mut report =
		import_legacy_api_keys(&location, LegacyKeyTarget::Store(&control)).expect("key import");
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
		import_legacy_api_keys(&location, LegacyKeyTarget::Store(&control))
			.expect("second run")
			.is_empty()
	);
}
