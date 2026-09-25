//! Proves models configured in `models.toml` join the catalog keyed
//! `<provider>/<model>`, like bundled and discovered models.
//!
//! The file names models by the provider's own id; lowering scopes them, and
//! references between them (`compactionModel`, `contextPromotionTarget`)
//! resolve inside the declaring provider before any catalog-wide selection.

use std::{
	fs,
	time::{Duration, SystemTime, UNIX_EPOCH},
};

use omp_ai::discovery::{DiscoveryCacheKey, DiscoveryStore};
use omp_catalog::{
	DiscoveredModel, ModelKey, ModelLimits, OperationBits, OperationKind, OverlaySource,
	OverlayStack, RouteId, UnsafeTrustScope, WireModelId, settings::ModelSettings,
	snapshot::Catalog,
};
use omp_core::Str;
use omp_driver::{
	discovery::{
		models::{ModelsConfig, discovery_probes, load_models_config, lower_user_overlay},
		roles::resolve_role_selector,
	},
	registry::production_catalog,
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
