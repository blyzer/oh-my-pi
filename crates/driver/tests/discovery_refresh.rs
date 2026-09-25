//! Proves a mid-session model-discovery refresh publishes live: a login
//! re-probes its provider even while the cache is fresh, the refreshed
//! registry is what new routing plans against, and a registry snapshot an
//! in-flight turn already holds keeps routing exactly as before.

use std::{
	fs,
	sync::Arc,
	time::{Duration, SystemTime},
};

use omp_ai::{
	CallAffinity, CallMeta, ChatRequest, ContentPart, ExecutionBudget, Message, NegotiationPolicy,
	RequestId, Role, Sampling, Setting, Target,
};
use omp_catalog::{ModelKey, ProviderId};
use omp_core::Str;
use omp_driver::{
	headless::kernel::{LaunchModelPolicy, settle_launch_model},
	registry::{
		DiscoveryRefresh, InferenceSessionOverrides, PinnedRoutes, production_catalog,
		production_inference_for_session,
	},
};
use parking_lot::Mutex;
use tokio::{
	io::{AsyncReadExt as _, AsyncWriteExt as _},
	net::TcpListener,
};

const API_KEY: &str = "sk-refresh-literal";

fn listing(ids: &[&str]) -> String {
	let data = ids
		.iter()
		.map(|id| format!(r#"{{"id":"{id}","object":"model"}}"#))
		.collect::<Vec<_>>()
		.join(",");
	format!(r#"{{"object":"list","data":[{data}]}}"#)
}

/// Serves the current listing to requests carrying the key, like the proxy.
async fn serve_listing(listener: TcpListener, current: Arc<Mutex<String>>) {
	loop {
		let Ok((mut stream, _)) = listener.accept().await else {
			return;
		};
		let current = Arc::clone(&current);
		tokio::spawn(async move {
			let mut request = Vec::new();
			let mut buffer = [0_u8; 4096];
			while !request.windows(4).any(|window| window == b"\r\n\r\n") {
				match stream.read(&mut buffer).await {
					Ok(0) | Err(_) => return,
					Ok(read) => request.extend_from_slice(&buffer[..read]),
				}
			}
			let head = String::from_utf8_lossy(&request).to_ascii_lowercase();
			let authorized = head
				.lines()
				.any(|line| line.trim() == format!("authorization: bearer {API_KEY}"));
			let body = current.lock().clone();
			let response = if authorized && head.starts_with("get ") {
				format!(
					"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: \
					 {}\r\nconnection: close\r\n\r\n{body}",
					body.len()
				)
			} else {
				"HTTP/1.1 401 Unauthorized\r\ncontent-length: 0\r\nconnection: close\r\n\r\n".to_owned()
			};
			let _ = stream.write_all(response.as_bytes()).await;
			let _ = stream.shutdown().await;
		});
	}
}

fn chat() -> ChatRequest {
	ChatRequest {
		messages:          Arc::from([Message {
			role:    Role::User,
			content: Arc::from([ContentPart::Text { text: Str::new_static("hi"), proof: None }]),
			name:    None,
		}]),
		tools:             Arc::from([]),
		hosted_tools:      Arc::from([]),
		tool_choice:       Setting::Unset,
		output:            Setting::Unset,
		reasoning:         Setting::Unset,
		verbosity:         Setting::Unset,
		cache_retention:   Setting::Unset,
		service_tier:      Setting::Unset,
		sampling:          Sampling::default(),
		max_output_tokens: Some(16),
		top_logprobs:      None,
		safety:            Arc::from([]),
		negotiation:       NegotiationPolicy::default(),
		forced_call:       None,
	}
}

fn meta(model: &ModelKey) -> CallMeta {
	CallMeta {
		id:             RequestId::from("discovery-refresh"),
		target:         Target::Model(model.clone()),
		deadline:       None,
		budget:         ExecutionBudget::default(),
		session:        None,
		debug_session:  None,
		response_hooks: Default::default(),
	}
}

/// Points the process at a scratch home whose v1 `models.yml` configures the
/// discovery proxy on `port`; returns the data directory.
fn scratch_home(root: &std::path::Path, port: u16) -> std::path::PathBuf {
	let home = root.join("home");
	let data_dir = root.join("data");
	let agent = home.join(".omp").join("agent");
	fs::create_dir_all(&agent).expect("v1 agent dir");
	fs::create_dir_all(&data_dir).expect("data dir");
	// SAFETY: nextest runs each test in its own process and this runs before
	// the composition spawns anything that reads the environment.
	unsafe {
		std::env::set_var("HOME", &home);
		std::env::set_var("OMP_DATA_DIR", &data_dir);
		std::env::set_var("OMP_CONFIG_DIR", root.join("config"));
		std::env::set_var("OMP_STATE_DIR", root.join("state"));
		std::env::set_var("OMP_CACHE_DIR", root.join("cache"));
		std::env::set_var("OMP_LLM_KEY_SOURCE", "local-file");
		std::env::set_var("OMP_ANTIGRAVITY_VERSION", "1.0.0");
		std::env::remove_var("OMP_PROFILE");
		std::env::remove_var("OMP_EASYCLIPROXY_API_KEY");
		std::env::remove_var("OMP_DEFAULT_MODEL");
	}
	fs::write(
		agent.join("models.yml"),
		format!(
			"providers:\n  easycliproxy:\n    baseUrl: http://127.0.0.1:{port}/v1\n    apiKey: \
			 {API_KEY}\n    discovery:\n      type: openai-models-list\n"
		),
	)
	.expect("v1 models.yml");
	omp_core::dirs::data_dir(None).expect("default data dir")
}

/// The remembered default is a discovered model and the discovery cache is
/// empty (expired) at launch: settled against the pre-discovery snapshot it
/// is missing, settled against the catalog composition refreshed it resolves
/// — and a default discovery still does not list is reported, not replaced
/// silently.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_launch_default_settles_against_the_refreshed_catalog() {
	let root = tempfile::tempdir().expect("scratch root");
	let current = Arc::new(Mutex::new(listing(&["claude-opus-5"])));
	let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
	let port = listener.local_addr().expect("address").port();
	let server = tokio::spawn(serve_listing(listener, Arc::clone(&current)));
	let data_dir = scratch_home(root.path(), port);
	let project = root.path().join("project");
	fs::create_dir_all(&project).expect("project");
	let ctx = omp_con::Ctx::new();
	ctx.exec(
		"ai_model_roles {default easycliproxy/claude-opus-5}",
		omp_con::Source::Config(Str::new_static("config.cfg")),
	)
	.expect("remembered default role");
	let fallback = "openai/gpt-5";

	let launch = production_catalog(&data_dir).expect("launch snapshot");
	let early = settle_launch_model(
		launch.as_ref(),
		&ctx,
		&project,
		fallback,
		LaunchModelPolicy::RememberedDefault,
	)
	.expect("settles on the fallback");
	assert_eq!(early.model.as_str(), fallback, "the snapshot cannot resolve the discovered default");
	assert_eq!(early.missing_default.as_deref(), Some("easycliproxy/claude-opus-5"));

	let inference = production_inference_for_session(
		&data_dir,
		Arc::new(omp_tool::Registry::default()),
		None,
		InferenceSessionOverrides::default(),
	)
	.await
	.expect("production inference composes");
	server.abort();
	let settled = settle_launch_model(
		inference.catalog().as_ref(),
		&ctx,
		&project,
		fallback,
		LaunchModelPolicy::RememberedDefault,
	)
	.expect("settles on the remembered default");
	assert_eq!(settled.model.as_str(), "easycliproxy/claude-opus-5");
	assert_eq!(settled.missing_default, None);

	ctx.exec(
		"ai_model_roles {default easycliproxy/never-listed}",
		omp_con::Source::Config(Str::new_static("config.cfg")),
	)
	.expect("unlisted default role");
	let missing = settle_launch_model(
		inference.catalog().as_ref(),
		&ctx,
		&project,
		fallback,
		LaunchModelPolicy::RememberedDefault,
	)
	.expect("falls back");
	assert_eq!(missing.model.as_str(), fallback);
	assert_eq!(missing.missing_default.as_deref(), Some("easycliproxy/never-listed"));
	let selected = settle_launch_model(
		inference.catalog().as_ref(),
		&ctx,
		&project,
		fallback,
		LaunchModelPolicy::Selected,
	)
	.expect("an explicit selector is final");
	assert_eq!((selected.model.as_str(), selected.missing_default), (fallback, None));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_login_refresh_reaches_new_turns_and_spares_the_in_flight_one() {
	let root = tempfile::tempdir().expect("scratch root");
	let current = Arc::new(Mutex::new(listing(&["claude-opus-5"])));
	let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
	let port = listener.local_addr().expect("address").port();
	let server = tokio::spawn(serve_listing(listener, Arc::clone(&current)));
	let data_dir = scratch_home(root.path(), port);

	let inference = production_inference_for_session(
		&data_dir,
		Arc::new(omp_tool::Registry::default()),
		None,
		InferenceSessionOverrides::default(),
	)
	.await
	.expect("production inference composes");
	let opus = ModelKey::from("easycliproxy/claude-opus-5");
	let added = ModelKey::from("easycliproxy/gpt-5.5");
	assert!(inference.catalog().model(&opus).is_some(), "the launch probe listed opus");
	assert!(inference.catalog().model(&added).is_none(), "gpt-5.5 is not listed yet");

	let refresher = inference
		.discovery
		.clone()
		.expect("a composed catalog refreshes");
	let expiry = refresher
		.next_expiry()
		.expect("cache readable")
		.expect("the launch probe cached a generation");
	let ttl = expiry
		.duration_since(SystemTime::now())
		.expect("the generation is fresh");
	assert!(ttl > Duration::from_secs(110 * 60), "the deadline is the 2 h TTL: {ttl:?}");

	// The turn in flight holds the launch registry; the next turn's routes
	// are pinned the same way until they adopt a publication.
	let in_flight = inference.registry.load();
	let mut routes =
		PinnedRoutes::new(inference.registry.clone(), meta(&added), CallAffinity::none());

	// The provider now lists a new model, but its cache is fresh: an
	// expiry-driven pass has nothing stale to probe and publishes nothing.
	*current.lock() = listing(&["claude-opus-5", "gpt-5.5"]);
	assert!(
		refresher
			.refresh(&DiscoveryRefresh::Expired)
			.await
			.expect("expiry pass")
			.is_none(),
		"a fresh cache is not re-probed"
	);
	assert!(inference.registry.load().same_publication(&in_flight));

	// A login re-probes that provider regardless and publishes live.
	let refreshed = refresher
		.refresh(&DiscoveryRefresh::LoggedIn(ProviderId::from("easycliproxy")))
		.await
		.expect("login pass")
		.expect("the login pass published");
	server.abort();
	assert!(refreshed.model(&added).is_some(), "the refresh lists gpt-5.5");
	let published = inference.registry.load();
	assert!(!published.same_publication(&in_flight), "a new registry was published");
	assert!(published.generation() > in_flight.generation());
	assert!(published.catalog().model(&added).is_some(), "new routing sees gpt-5.5");
	assert!(inference.catalog().model(&added).is_some(), "pickers read the publication");
	assert!(
		in_flight.catalog().model(&added).is_none(),
		"the in-flight snapshot keeps routing through the registry it loaded"
	);

	// Within the turn the pinned client still plans against the launch
	// registry; the next turn adopts the publication and routes the model.
	assert!(routes.client().plan(&chat()).is_err(), "mid-turn routing is unchanged");
	assert!(routes.adopt_published(), "the next turn adopts the refreshed registry");
	assert!(routes.registry().same_publication(&published));
	let plan = routes
		.client()
		.plan(&chat())
		.expect("the next turn routes the discovered model");
	assert_eq!(plan.execution_plan().model.as_ref(), Some(&added));
	assert!(!routes.adopt_published(), "an unchanged publication is not re-adopted");
}
