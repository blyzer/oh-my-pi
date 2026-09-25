//! Proves runtime-discovered models join the production catalog as an
//! additive, provider-scoped layer.
//!
//! Every bundled model is keyed `<provider>/<model>`; a discovered row must be
//! keyed the same way, so two providers listing the same id keep one model
//! each, and no listing can rewrite a bundled model's routes.

use std::{
	fs,
	path::Path,
	time::{Duration, SystemTime, UNIX_EPOCH},
};

use omp_ai::discovery::{DiscoveryCacheKey, DiscoveryProbe, DiscoveryStore};
use omp_catalog::{
	ClassificationInput, ClassificationPhase, DiscoveredModel, ModelKey, ModelLimits, ModelSpec,
	OperationBits, OperationKind, OverlaySource, OverlayStack, ProviderId, RouteId,
	UnsafeTrustScope, WireModelId, classify, snapshot::Catalog,
};
use omp_core::Str;
use omp_driver::{
	discovery::models::{discovery_probes, load_models_config, lower_user_overlay},
	registry::production_catalog,
};

const MODELS_TOML: &str = "[providers.alpha]\nbaseUrl='https://alpha.example/v1'\nauth='none'\n\
                           discovery={type='openai-models-list'}\n\
                           [providers.beta]\nbaseUrl='https://beta.example/v1'\nauth='none'\n\
                           discovery={type='openai-models-list'}\n";

fn now_ms() -> u64 {
	SystemTime::now()
		.duration_since(UNIX_EPOCH)
		.expect("clock after epoch")
		.as_millis()
		.try_into()
		.expect("millisecond clock")
}

fn row(provider: &ProviderId, route: &RouteId, wire: &str) -> DiscoveredModel {
	let mut operations = OperationBits::empty();
	operations.insert_kind(OperationKind::Chat);
	DiscoveredModel {
		provider:              provider.clone(),
		route:                 route.clone(),
		wire_model:            WireModelId::from(wire),
		aliases:               Box::new([]),
		display_name:          None,
		declared_class:        None,
		declared_operations:   operations,
		declared_capabilities: None,
		declared_limits:       None,
		declared_pricing:      Box::new([]),
		extended_context_mode: None,
		availability:          None,
		source:                Str::new_static("test-listing"),
		observed_at_ms:        Some(now_ms()),
		updated_at_ms:         None,
		deprecated:            None,
	}
}

/// Writes `models.toml` into `data_dir` and returns the probes the production
/// catalog reads cached listings for.
fn configured_probes(data_dir: &Path) -> Vec<DiscoveryProbe> {
	let path = data_dir.join("models.toml");
	fs::write(&path, MODELS_TOML).expect("models.toml");
	let config = load_models_config(&path).expect("config decodes");
	let configured = Catalog::embedded()
		.with_overlay_stack(
			&OverlayStack::from_layers([(
				OverlaySource::UserConfig,
				lower_user_overlay(&config).expect("config lowers"),
			)]),
			UnsafeTrustScope::ALL,
		)
		.expect("configured catalog");
	discovery_probes(Some(&config), &configured).expect("probes")
}

fn probe<'a>(probes: &'a [DiscoveryProbe], provider: &str) -> &'a DiscoveryProbe {
	probes
		.iter()
		.find(|probe| probe.provider.as_str() == provider)
		.expect("configured provider probe")
}

fn publish(store: &DiscoveryStore, key: &DiscoveryCacheKey, rows: &[DiscoveredModel]) {
	store
		.publish(key, rows, now_ms(), Duration::from_hours(1))
		.expect("publish cached listing");
}

fn scoped(provider: &str, model: &str) -> ModelKey {
	ModelKey::from(format!("{provider}/{model}"))
}

fn logical(provider: &str, wire: &str) -> Str {
	classify(ClassificationInput {
		phase: ClassificationPhase::DiscoveryNormalizer,
		provider,
		model: wire,
		observed_at_ms: None,
	})
	.logical_model
}

#[test]
fn providers_listing_the_same_model_id_each_keep_a_provider_scoped_model() {
	let data_dir = tempfile::tempdir().expect("scratch data dir");
	let probes = configured_probes(data_dir.path());
	let store = DiscoveryStore::open(&data_dir.path().join("models.db")).expect("cache");
	for provider in ["alpha", "beta"] {
		let probe = probe(&probes, provider);
		publish(&store, &DiscoveryCacheKey::endpoint(probe.provider.clone(), &probe.endpoint), &[
			row(&probe.provider, &probe.route, "shared-model"),
		]);
	}

	let catalog = production_catalog(data_dir.path()).expect("production catalog");
	for provider in ["alpha", "beta"] {
		let probe = probe(&probes, provider);
		let key = scoped(provider, "shared-model");
		let model = catalog
			.model(&key)
			.unwrap_or_else(|| panic!("{key} survives beside the other provider's listing"));
		assert_eq!(model.routes.as_ref(), std::slice::from_ref(&probe.route), "{key} routes");
		assert_eq!(
			model.wire_ids.as_ref(),
			[(probe.route.clone(), WireModelId::from("shared-model"))],
			"{key} keeps its own wire id"
		);
	}
	assert!(
		catalog.model(ModelKey::from_ref("shared-model")).is_none(),
		"no discovered model is keyed without its provider"
	);
}

/// A bundled model whose key a listing could collide with: a namespaced id a
/// proxy resells verbatim (`anthropic/…`), classified to itself.
fn resold_bundled_model(catalog: &Catalog) -> &ModelSpec {
	catalog
		.models()
		.iter()
		.find(|model| {
			model.key.as_str().contains('/')
				&& !model.routes.is_empty()
				&& logical("alpha", model.key.as_str()).as_str() == model.key.as_str()
		})
		.expect("a bundled model a proxy could resell under its own key")
}

/// A bundled model served on a discovery route whose own listing of it
/// normalizes to the bundled key.
fn relisted_bundled_model(catalog: &Catalog) -> (&ModelSpec, RouteId, WireModelId) {
	catalog
		.models()
		.iter()
		.find_map(|model| {
			let (route, wire) = model.wire_ids.first()?;
			let definition = catalog.route(route)?;
			definition.discovery.as_ref()?;
			let provider = definition.provider.as_str();
			(scoped(provider, logical(provider, wire.as_str()).as_str()) == model.key)
				.then(|| (model, route.clone(), wire.clone()))
		})
		.expect("a bundled model on a discovery route")
}

#[test]
fn a_discovered_row_never_replaces_a_bundled_models_routes() {
	let data_dir = tempfile::tempdir().expect("scratch data dir");
	let probes = configured_probes(data_dir.path());
	let store = DiscoveryStore::open(&data_dir.path().join("models.db")).expect("cache");
	let embedded = Catalog::embedded();

	// A proxy reselling a bundled model under the bundled key.
	let resold = resold_bundled_model(embedded);
	let alpha = probe(&probes, "alpha");
	publish(&store, &DiscoveryCacheKey::endpoint(alpha.provider.clone(), &alpha.endpoint), &[row(
		&alpha.provider,
		&alpha.route,
		resold.key.as_str(),
	)]);

	// The owning provider relisting a bundled model under different facts.
	let (relisted, route, wire) = relisted_bundled_model(embedded);
	let owner = embedded.route(&route).expect("route").provider.clone();
	let mut impostor = row(&owner, &route, wire.as_str());
	impostor.display_name = Some(Str::new_static("Relisted Impostor"));
	impostor.declared_limits = Some(ModelLimits {
		context_window:        Some(1_024),
		maximum_input_tokens:  None,
		maximum_output_tokens: Some(256),
		maximum_batch:         None,
	});
	publish(&store, &DiscoveryCacheKey::provider(owner), &[impostor]);

	let catalog = production_catalog(data_dir.path()).expect("production catalog");
	for bundled in [resold, relisted] {
		let live = catalog.model(&bundled.key).expect("bundled model stays");
		assert_eq!(live.routes, bundled.routes, "{} routes", bundled.key);
		assert_eq!(live.wire_ids, bundled.wire_ids, "{} wire ids", bundled.key);
		assert_eq!(live.display_name, bundled.display_name, "{} name", bundled.key);
		assert_eq!(live.limits, bundled.limits, "{} limits", bundled.key);
	}
	let resale = catalog
		.model(&scoped("alpha", resold.key.as_str()))
		.expect("the proxy's listing is added under its own provider");
	assert_eq!(resale.routes.as_ref(), std::slice::from_ref(&alpha.route));
}
