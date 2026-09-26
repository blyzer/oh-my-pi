//! Proves a first run with only a v1 `models.yml` lists the configured
//! provider's models in that same session.
//!
//! The v1 `apiKey` is what authenticates the provider's `/v1/models`. The
//! production composition must import it before probing, and must hand the
//! catalog that probe produced to model selection and the chat picker
//! (`ProductionInference::catalog`) instead of the snapshot read at launch.

use std::{fs, sync::Arc};

use omp_catalog::ModelKey;
use omp_driver::registry::{
	InferenceSessionOverrides, production_catalog, production_inference_for_session,
};
use tokio::{
	io::{AsyncReadExt as _, AsyncWriteExt as _},
	net::TcpListener,
};

const API_KEY: &str = "sk-first-run-literal";
const LISTING: &str = r#"{"object":"list","data":[{"id":"claude-opus-5","object":"model"},{"id":"gpt-5.5","object":"model"}]}"#;

/// Serves `/…/models` to requests carrying the v1 key and answers 401 to
/// anything else, like the configured proxy.
async fn serve_listing(listener: TcpListener) {
	loop {
		let Ok((mut stream, _)) = listener.accept().await else {
			return;
		};
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
			let listing = head.starts_with("get ")
				&& head.lines().next().is_some_and(|line| {
					line
						.split_whitespace()
						.nth(1)
						.is_some_and(|path| path.ends_with("/models"))
				});
			let response = if authorized && listing {
				format!(
					"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: \
					 {}\r\nconnection: close\r\n\r\n{LISTING}",
					LISTING.len()
				)
			} else {
				"HTTP/1.1 401 Unauthorized\r\ncontent-length: 0\r\nconnection: close\r\n\r\n".to_owned()
			};
			let _ = stream.write_all(response.as_bytes()).await;
			let _ = stream.shutdown().await;
		});
	}
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_v1_api_key_authenticates_discovery_in_the_first_session() {
	let root = tempfile::tempdir().expect("scratch root");
	let home = root.path().join("home");
	let data_dir = root.path().join("data");
	let agent = home.join(".omp").join("agent");
	fs::create_dir_all(&agent).expect("v1 agent dir");
	fs::create_dir_all(&data_dir).expect("data dir");
	// SAFETY: nextest runs each test in its own process and this runs before
	// the composition spawns anything that reads the environment.
	unsafe {
		std::env::set_var("HOME", &home);
		std::env::set_var("OMP_DATA_DIR", &data_dir);
		std::env::set_var("OMP_CONFIG_DIR", root.path().join("config"));
		std::env::set_var("OMP_STATE_DIR", root.path().join("state"));
		std::env::set_var("OMP_CACHE_DIR", root.path().join("cache"));
		std::env::set_var("OMP_LLM_KEY_SOURCE", "local-file");
		std::env::set_var("OMP_ANTIGRAVITY_VERSION", "1.0.0");
		std::env::remove_var("OMP_PROFILE");
		std::env::remove_var("OMP_EASYCLIPROXY_API_KEY");
	}
	let data_dir = omp_core::dirs::data_dir(None).expect("default data dir");

	let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
	let port = listener.local_addr().expect("address").port();
	let server = tokio::spawn(serve_listing(listener));
	fs::write(
		agent.join("models.yml"),
		format!(
			"providers:\n  easycliproxy:\n    baseUrl: http://127.0.0.1:{port}/v1\n    apiKey: \
			 {API_KEY}\n    discovery:\n      type: openai-models-list\n"
		),
	)
	.expect("v1 models.yml");

	// What `omp` reads before composing: nothing has been probed yet.
	let launch = production_catalog(&data_dir).expect("launch snapshot");
	let opus = ModelKey::from("easycliproxy/claude-opus-5");
	assert!(launch.model(&opus).is_none(), "nothing is discovered before composition");

	let inference = production_inference_for_session(
		&data_dir,
		Arc::new(omp_tool::Registry::default()),
		None,
		InferenceSessionOverrides::default(),
	)
	.await
	.expect("production inference composes");
	server.abort();

	for (key, wire) in [(opus, "claude-opus-5"), (ModelKey::from("easycliproxy/gpt-5.5"), "gpt-5.5")]
	{
		let model = inference
			.catalog()
			.model(&key)
			.cloned()
			.unwrap_or_else(|| panic!("{key} is listed in the session that imported the key"));
		assert!(
			model
				.wire_ids
				.iter()
				.any(|(_, candidate)| candidate.as_str() == wire),
			"{key} is sent to the proxy as {wire}"
		);
		assert!(inference.registry.load().catalog().model(&key).is_some(), "{key} is routable");
	}
}
