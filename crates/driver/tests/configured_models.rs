//! Proves models configured in `models.toml` join the catalog keyed
//! `<provider>/<model>`, like bundled and discovered models.
//!
//! The file names models by the provider's own id; lowering scopes them, and
//! references between them (`compactionModel`, `contextPromotionTarget`)
//! resolve inside the declaring provider before any catalog-wide selection.
//! A configured `auth` replaces the routes' authentication, and with it the
//! kind a stored credential of the provider must have.

use std::{
	fs,
	path::Path,
	sync::Arc,
	time::{Duration, SystemTime, UNIX_EPOCH},
};

use omp_ai::{
	AuthEvent, AuthResponse,
	account::{AccountPool, AccountRecord, AccountStateStore},
	auth::{
		AuthControlHandle, AuthLoginEngine as _, CredentialBroker, CredentialBrokerEngines,
		CredentialControlWrite, CredentialEnvironment, CredentialError, CredentialKind,
		CredentialMetadata, CredentialNeed, CredentialOrigin, CredentialSource as _, CredentialStore,
		CredentialWrite, HeadlessKeySource, KeyId, SecretLoginEngine, StoredCredentialSource,
	},
	call::{AccountRoutingContext, AuthInput, AuthMethod, LoginRequest},
	discovery::{DiscoveryCacheKey, DiscoveryStore},
};
use omp_catalog::{
	AuthSpecId, DiscoveredModel, ModelKey, ModelLimits, OperationBits, OperationKind, OverlaySource,
	OverlayStack, ProviderId, RouteId, UnsafeTrustScope, WireModelId, settings::ModelSettings,
	snapshot::Catalog,
};
use omp_core::{SecretString, Str};
use omp_driver::{
	discovery::{
		models::{ModelsConfig, discovery_probes, load_models_config, lower_user_overlay},
		roles::resolve_role_selector,
	},
	registry::{production_catalog, production_registry},
};

/// Lowers `models_toml` over the embedded catalog, as the production registry
/// does before any discovery layer.
fn configured(models_toml: &str) -> Catalog {
	let config: ModelsConfig = toml::from_str(models_toml).expect("models.toml decodes");
	Catalog::embedded()
		.with_overlay_stack(
			&OverlayStack::from_layers([(
				OverlaySource::UserConfig,
				lower_user_overlay(&config).expect("config lowers"),
			)]),
			UnsafeTrustScope::ALL,
		)
		.expect("configured catalog")
}

fn key(text: &str) -> ModelKey {
	ModelKey::from(text)
}

const TWO_PROVIDERS: &str = "[providers.alpha]\nbaseUrl='https://alpha.example/v1'\nauth='none'\n\
                             api='openai-completions'\n\
                             [providers.alpha.models.shared]\nname='Alpha Shared'\ncontextWindow=1000\n\
                             [providers.beta]\nbaseUrl='https://beta.example/v1'\nauth='none'\n\
                             api='openai-completions'\n\
                             [providers.beta.models.shared]\nname='Beta Shared'\ncontextWindow=2000\n";

#[test]
fn two_providers_configuring_the_same_model_name_each_keep_their_own_model() {
	let catalog = configured(TWO_PROVIDERS);
	for (provider, name, window) in
		[("alpha", "Alpha Shared", 1_000), ("beta", "Beta Shared", 2_000)]
	{
		let scoped = key(&format!("{provider}/shared"));
		let model = catalog
			.model(&scoped)
			.unwrap_or_else(|| panic!("{scoped} survives beside the other provider's entry"));
		let route = RouteId::from(format!("{provider}-configured"));
		assert_eq!(model.routes.as_ref(), std::slice::from_ref(&route), "{scoped} routes");
		assert_eq!(
			model.wire_ids.as_ref(),
			[(route, WireModelId::from("shared"))],
			"{scoped} sends the provider's own id"
		);
		assert_eq!(model.display_name, name, "{scoped} name");
		assert_eq!(model.limits.context_window, Some(window), "{scoped} limits");
	}
	assert!(
		catalog.model(ModelKey::from_ref("shared")).is_none(),
		"no configured model is keyed without its provider"
	);
}

#[test]
fn bare_references_resolve_within_the_declaring_provider_first() {
	let bundled = Catalog::embedded()
		.models()
		.iter()
		.find(|model| !model.routes.is_empty() && model.key.as_str().split('/').count() == 2)
		.expect("a bundled provider/model key");
	let catalog = configured(&format!(
		"[providers.alpha]\nbaseUrl='https://alpha.example/v1'\nauth='none'\n\
		 api='openai-completions'\n\
		 [providers.alpha.models.fast]\ncontextPromotionTarget='large'\ncompactionModel='fast'\n\
		 [providers.alpha.models.large]\n\
		 [providers.beta]\nbaseUrl='https://beta.example/v1'\nauth='none'\n\
		 api='openai-completions'\n\
		 [providers.beta.models.large]\ncompactionModel='alpha/fast'\n\
		 contextPromotionTarget='later'\n\
		 [providers.beta.models.pinned]\ncompactionModel='{}'\n",
		bundled.key
	));
	let model = |text: &str| catalog.model(&key(text)).expect("configured model");

	let fast = model("alpha/fast");
	assert_eq!(
		fast.context_promotion_target,
		Some(key("alpha/large")),
		"a bare target names the declaring provider's model, not beta's `large`"
	);
	assert_eq!(fast.compaction_model, Some(key("alpha/fast")));

	let large = model("beta/large");
	assert_eq!(
		large.compaction_model,
		Some(key("alpha/fast")),
		"a qualified reference resolves catalog-wide"
	);
	assert_eq!(
		large.context_promotion_target,
		Some(key("beta/later")),
		"a reference naming nothing yet stays in the provider discovery fills"
	);

	assert_eq!(model("beta/pinned").compaction_model, Some(bundled.key.clone()));
}

#[test]
fn configured_models_resolve_by_scoped_key_and_by_bare_id() {
	let catalog = configured(
		"[providers.easycliproxy]\nbaseUrl='https://proxy.example/v1'\nauth='none'\n\
		 api='openai-completions'\n\
		 [providers.easycliproxy.models.claude-opus-5]\n\
		 [providers.easycliproxy.models.proxy-only-fast]\n",
	);
	let settings = ModelSettings::default();
	let resolve = |selector: &str| {
		resolve_role_selector(&catalog, &settings, selector)
			.unwrap_or_else(|error| panic!("{selector} resolves: {error}"))
			.model
	};
	assert_eq!(resolve("easycliproxy/claude-opus-5"), key("easycliproxy/claude-opus-5"));
	assert_eq!(resolve("proxy-only-fast"), key("easycliproxy/proxy-only-fast"));
	let bare = resolve("claude-opus-5");
	assert!(
		bare.as_str().ends_with("/claude-opus-5") && catalog.model(&bare).is_some(),
		"a bare id selects a provider-scoped model, got {bare}"
	);
}

fn now_ms() -> u64 {
	SystemTime::now()
		.duration_since(UNIX_EPOCH)
		.expect("clock after epoch")
		.as_millis()
		.try_into()
		.expect("millisecond clock")
}

#[test]
fn a_configured_model_wins_over_its_providers_discovered_row() {
	const MODELS_TOML: &str = "[providers.alpha]\nbaseUrl='https://alpha.example/v1'\nauth='none'\n\
	                           discovery={type='openai-models-list'}\n\
	                           [providers.alpha.models.fast]\nid='shared-model'\n\
	                           name='Configured Fast'\ncontextWindow=4242\n";
	let data_dir = tempfile::tempdir().expect("scratch data dir");
	let path = data_dir.path().join("models.toml");
	fs::write(&path, MODELS_TOML).expect("models.toml");
	let config = load_models_config(&path).expect("config decodes");
	let probes = discovery_probes(Some(&config), &configured(MODELS_TOML)).expect("probes");
	let probe = probes
		.iter()
		.find(|probe| probe.provider.as_str() == "alpha")
		.expect("configured provider probe");

	let mut operations = OperationBits::empty();
	operations.insert_kind(OperationKind::Chat);
	let listed = DiscoveredModel {
		provider:              probe.provider.clone(),
		route:                 probe.route.clone(),
		wire_model:            WireModelId::from("shared-model"),
		aliases:               Box::new([]),
		display_name:          Some(Str::new_static("Listed Impostor")),
		declared_class:        None,
		declared_operations:   operations,
		declared_capabilities: None,
		declared_limits:       Some(ModelLimits {
			context_window:        Some(1_024),
			maximum_input_tokens:  None,
			maximum_output_tokens: None,
			maximum_batch:         None,
		}),
		declared_pricing:      Box::new([]),
		extended_context_mode: None,
		availability:          None,
		source:                Str::new_static("test-listing"),
		observed_at_ms:        Some(now_ms()),
		updated_at_ms:         None,
		deprecated:            None,
	};
	DiscoveryStore::open(&data_dir.path().join("models.db"))
		.expect("cache")
		.publish(
			&DiscoveryCacheKey::endpoint(probe.provider.clone(), &probe.endpoint),
			&[listed],
			now_ms(),
			Duration::from_hours(1),
		)
		.expect("publish cached listing");

	let catalog = production_catalog(data_dir.path()).expect("production catalog");
	let model = catalog
		.model(&key("alpha/shared-model"))
		.expect("the configured model keeps its provider-scoped key");
	assert_eq!(model.display_name, "Configured Fast");
	assert_eq!(model.limits.context_window, Some(4_242));
	assert!(
		catalog.model(ModelKey::from_ref("shared-model")).is_none(),
		"no configured model is keyed without its provider"
	);
	assert_eq!(
		catalog
			.models()
			.iter()
			.filter(|model| model.key.as_str().ends_with("/shared-model"))
			.count(),
		1,
		"the listing adds no second record for the configured model"
	);
}

/// Hugging Face declares a bearer token, but `auth = 'apiKey'` (what the v1
/// importer writes for a keyed `models.yml` provider) makes its routes lease
/// an API key. The base URL is a closed local port, so a composition that
/// refreshes discovery never leaves the machine.
const HUGGINGFACE_API_KEY_ROUTES: &str =
	"[providers.huggingface]\nbaseUrl='http://127.0.0.1:9/v1'\nauth='apiKey'\n";

/// The authentication `HUGGINGFACE_API_KEY_ROUTES` gives every Hugging Face
/// route.
fn configured_auth() -> AuthSpecId {
	AuthSpecId::from("huggingface-configured-auth")
}

/// The credential store and account pool of `data_dir`.
fn stores(data_dir: &Path) -> (Arc<CredentialStore>, AccountPool) {
	let database = data_dir.join("credentials.db");
	let store = Arc::new(
		CredentialStore::open(
			&database,
			Arc::new(HeadlessKeySource::new(KeyId::new("configured-kind"), [5; 32])),
		)
		.expect("credential store"),
	);
	let accounts =
		AccountPool::with_store(Arc::new(AccountStateStore::open(&database).expect("state")))
			.expect("account pool");
	(store, accounts)
}

/// Stores `secret` as the `bearer` row `/login` wrote for Hugging Face from
/// its declared authentication before it followed the routes' kind, and
/// registers its account.
fn earlier_bearer_login(
	store: &CredentialStore,
	accounts: &AccountPool,
	secret: &[u8],
) -> CredentialMetadata {
	let account = omp_ai::AccountId::from("huggingface:api-key");
	let principal = omp_ai::PrincipalId::from("api-key");
	let metadata = store
		.put(CredentialWrite {
			account_id:          &account,
			principal_id:        &principal,
			kind:                "bearer",
			secret:              &omp_core::SecretBox::new(Box::new(secret.to_vec())),
			expires_at_ms:       None,
			origin:              CredentialOrigin::Persistent,
			now_ms:              1,
			expected_generation: None,
		})
		.expect("earlier login");
	accounts
		.upsert(AccountRecord {
			account,
			principal,
			provider: ProviderId::from("huggingface"),
			routes: std::collections::BTreeSet::new(),
			enabled: true,
			credential_generation: metadata.generation,
			routing: AccountRoutingContext::default(),
		})
		.expect("earlier account");
	metadata
}

/// A process environment with no credential variable set.
struct NoEnvironment;

impl CredentialEnvironment for NoEnvironment {
	fn read(&self, _: &str) -> Result<Option<SecretString>, CredentialError> {
		Ok(None)
	}
}

/// Leases `account` from `store` on the configured Hugging Face
/// authentication.
async fn lease_configured(
	catalog: &Catalog,
	store: &Arc<CredentialStore>,
	account: &str,
) -> Result<omp_ai::auth::CredentialLease, CredentialError> {
	let broker =
		CredentialBroker::from_catalog(catalog, Arc::new(NoEnvironment), CredentialBrokerEngines {
			stored: Some(Arc::new(StoredCredentialSource::new(Arc::clone(store)))),
			..CredentialBrokerEngines::default()
		})
		.expect("broker");
	broker
		.lease(CredentialNeed {
			spec:        configured_auth(),
			account:     Some(omp_ai::AccountId::from(account)),
			principal:   None,
			valid_after: SystemTime::now(),
		})
		.await
}

/// The `authorization` header `lease` puts on a request under the configured
/// authentication.
fn authorization(catalog: &Catalog, lease: &omp_ai::auth::CredentialLease) -> String {
	let spec = catalog
		.auth_spec(&configured_auth())
		.expect("configured auth");
	let runtime = omp_ai::auth::AuthSpec::from_catalog(spec, None, None).expect("runtime auth");
	let mut request = http::Request::builder()
		.uri("http://127.0.0.1:9/v1/chat/completions")
		.body(bytes::Bytes::new())
		.expect("request");
	lease
		.prepare(&runtime, SystemTime::now())
		.expect("the lease prepares")
		.finalize_buffered(&mut request)
		.expect("the lease applies");
	request.headers()["authorization"]
		.to_str()
		.expect("ASCII header")
		.to_owned()
}

/// A configured auth decides the kind a Hugging Face credential is stored
/// under: a `bearer` row stored before is re-stored as `api-key`, and an
/// extension's bearer token and a `/login` key (which picks the provider's
/// declared bearer authentication) are stored as `api-key`. Each then leases
/// on the configured authentication.
#[tokio::test]
async fn a_configured_auth_decides_the_stored_credential_kind() {
	let catalog = Arc::new(configured(HUGGINGFACE_API_KEY_ROUTES));
	let data_dir = tempfile::tempdir().expect("scratch data dir");
	let (store, accounts) = stores(data_dir.path());
	let control =
		AuthControlHandle::offline(Arc::clone(&catalog), Arc::clone(&store), accounts.clone())
			.expect("control");

	let earlier = earlier_bearer_login(&store, &accounts, b"hf-fake-earlier");
	assert_eq!(
		lease_configured(&catalog, &store, "huggingface:api-key")
			.await
			.expect_err("a bearer row cannot lease on the configured auth"),
		CredentialError::KindMismatch {
			expected: CredentialKind::ApiKey,
			actual:   CredentialKind::Bearer,
		}
	);
	let repairs = control.repair_static_secret_kinds().expect("repair");
	assert_eq!(
		repairs
			.iter()
			.map(|repair| (repair.account.as_str(), repair.stored.as_str(), repair.repaired))
			.collect::<Vec<_>>(),
		[("huggingface:api-key", "bearer", CredentialKind::ApiKey)]
	);
	let repaired = store
		.metadata(&earlier.account_id)
		.expect("metadata")
		.expect("row");
	assert_eq!(repaired.kind.as_str(), "api-key");
	assert_eq!(repaired.generation, earlier.generation + 1);
	let lease = lease_configured(&catalog, &store, "huggingface:api-key")
		.await
		.expect("the re-stored row leases");
	assert_eq!(authorization(&catalog, &lease), "Bearer hf-fake-earlier");
	assert!(
		control
			.repair_static_secret_kinds()
			.expect("again")
			.is_empty()
	);

	let (extension, _) = control
		.store(CredentialControlWrite {
			provider:      ProviderId::from("huggingface"),
			principal:     omp_ai::PrincipalId::from("extension"),
			identity:      Some(Str::new_static("extension")),
			kind:          Str::new_static("bearer"),
			secret:        omp_core::Secret::from(b"hf-fake-extension".to_vec()),
			expires_at_ms: None,
		})
		.expect("extension store");
	assert_eq!(extension.kind.as_str(), "api-key");

	let declared = catalog
		.provider(ProviderId::from_ref("huggingface"))
		.expect("huggingface")
		.auth[0]
		.clone();
	assert_eq!(
		catalog.auth_spec(&declared).expect("declared auth").kind,
		omp_catalog::provider::AuthSpecKind::Bearer
	);
	let engine = SecretLoginEngine::new(
		AuthMethod::ApiKey,
		Str::new_static("login"),
		Arc::clone(&catalog),
		Arc::clone(&store),
		accounts,
	)
	.expect("API-key login engine");
	let session = engine
		.begin(LoginRequest { provider: ProviderId::from("huggingface"), method: None }, declared)
		.await
		.expect("login starts");
	loop {
		let event = tokio::time::timeout(Duration::from_secs(5), session.events.recv_async())
			.await
			.expect("login event in time")
			.expect("login channel")
			.expect("login event");
		match event {
			AuthEvent::Prompt(_) => session
				.responses
				.send_async(AuthResponse {
					session: session.id.clone(),
					input:   AuthInput::ApiKey(SecretString::from("hf-fake-login".to_owned())),
				})
				.await
				.expect("answer the prompt"),
			AuthEvent::Complete(account) => {
				assert_eq!(account.account.as_str(), "huggingface:login");
				break;
			},
			AuthEvent::OpenUrl { .. } | AuthEvent::ShowDeviceCode { .. } | AuthEvent::Waiting => {},
		}
	}
	let login = lease_configured(&catalog, &store, "huggingface:login")
		.await
		.expect("the login leases on the configured auth");
	assert_eq!(login.kind(), CredentialKind::ApiKey);
	assert_eq!(authorization(&catalog, &login), "Bearer hf-fake-login");
}

/// The production composition re-stores a row stored under a kind its
/// provider's routes do not lease before any request leases it, and a second
/// composition finds nothing left to re-store.
#[tokio::test]
async fn composition_restores_a_row_its_routes_do_not_lease() {
	// SAFETY: nextest runs each test in its own process, and this runs before
	// the composition spawns anything that reads the environment.
	unsafe { std::env::set_var("OMP_ANTIGRAVITY_VERSION", "1.0.0") };
	let data_dir = tempfile::tempdir().expect("scratch data dir");
	fs::write(data_dir.path().join("models.toml"), HUGGINGFACE_API_KEY_ROUTES).expect("models.toml");
	let (store, accounts) = stores(data_dir.path());
	let earlier = earlier_bearer_login(&store, &accounts, b"hf-fake-composed");

	production_registry(data_dir.path(), Arc::clone(&store))
		.await
		.expect("production composition");

	let repaired = store
		.metadata(&earlier.account_id)
		.expect("metadata")
		.expect("row");
	assert_eq!(repaired.kind.as_str(), "api-key");
	assert_eq!(repaired.generation, earlier.generation + 1);
	let catalog = production_catalog(data_dir.path()).expect("production catalog");
	let lease = lease_configured(&catalog, &store, "huggingface:api-key")
		.await
		.expect("the re-stored row leases");
	assert_eq!(authorization(&catalog, &lease), "Bearer hf-fake-composed");

	production_registry(data_dir.path(), Arc::clone(&store))
		.await
		.expect("second composition");
	assert_eq!(store.metadata(&earlier.account_id).expect("metadata"), Some(repaired));
}
