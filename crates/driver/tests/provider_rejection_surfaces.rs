//! Proves a request the provider rejects reports the provider's answer when
//! the retry that answer asks for cannot start.
//!
//! A 402 asks the attempt layer to rotate to another account and a 401 to
//! refresh the credential. With one stored static key neither can start: no
//! other account exists, and an API key cannot be renewed. The caller must
//! see the provider's status, not the credential failure the retry ran into
//! (before the fix, an opaque `inference Authentication error during
//! Authentication`).

use std::{
	fs,
	sync::{
		Arc,
		atomic::{AtomicUsize, Ordering},
	},
};

use omp_ai::{
	CallAffinity, CallMeta, ChatRequest, ContentPart, ErrorKind, ExecutionBudget, Message,
	NegotiationPolicy, RequestId, Role, Sampling, Setting, Target,
};
use omp_catalog::ModelKey;
use omp_core::Str;
use omp_driver::registry::{
	InferenceSessionOverrides, PinnedRoutes, production_inference_for_session,
};
use tokio::{
	io::{AsyncReadExt as _, AsyncWriteExt as _},
	net::TcpListener,
};

const API_KEY: &str = "sk-fake-rejected-literal";
const LISTING: &str = r#"{"object":"list","data":[{"id":"fake-model","object":"model"}]}"#;

/// Lists `fake-model` to requests carrying the key and answers every other
/// request (the chat request) with `status` and an empty body, counting them.
async fn serve(listener: TcpListener, status: &'static str, chats: Arc<AtomicUsize>) {
	loop {
		let Ok((mut stream, _)) = listener.accept().await else {
			return;
		};
		let chats = Arc::clone(&chats);
		tokio::spawn(async move {
			let mut request = Vec::new();
			let mut buffer = [0_u8; 4096];
			let head_end = loop {
				if let Some(end) = request.windows(4).position(|window| window == b"\r\n\r\n") {
					break end + 4;
				}
				match stream.read(&mut buffer).await {
					Ok(0) | Err(_) => return,
					Ok(read) => request.extend_from_slice(&buffer[..read]),
				}
			};
			let head = String::from_utf8_lossy(&request[..head_end]).to_ascii_lowercase();
			// Read the whole body before answering, so closing the connection
			// never resets a request still being written.
			let length = head
				.lines()
				.find_map(|line| line.strip_prefix("content-length:"))
				.and_then(|value| value.trim().parse::<usize>().ok());
			let chunked = head.contains("transfer-encoding: chunked");
			while length.is_some_and(|length| request.len() < head_end + length)
				|| (chunked && !request.ends_with(b"0\r\n\r\n"))
			{
				match stream.read(&mut buffer).await {
					Ok(0) | Err(_) => return,
					Ok(read) => request.extend_from_slice(&buffer[..read]),
				}
			}
			let authorized = head
				.lines()
				.any(|line| line.trim() == format!("authorization: bearer {API_KEY}"));
			let response = if head.starts_with("get ") && authorized {
				format!(
					"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: \
					 {}\r\nconnection: close\r\n\r\n{LISTING}",
					LISTING.len()
				)
			} else if head.starts_with("get ") {
				"HTTP/1.1 401 Unauthorized\r\ncontent-length: 0\r\nconnection: close\r\n\r\n".to_owned()
			} else {
				chats.fetch_add(1, Ordering::SeqCst);
				format!("HTTP/1.1 {status}\r\ncontent-length: 0\r\nconnection: close\r\n\r\n")
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

/// Sends one chat request for a model of a provider whose only account is the
/// static key a v1 `models.yml` imported, to a provider that answers `status`.
/// Returns the caller's error and how many chat requests reached the wire.
async fn rejected_chat(status: &'static str) -> (omp_ai::Error, usize) {
	let root = tempfile::tempdir().expect("scratch root");
	let home = root.path().join("home");
	let agent = home.join(".omp").join("agent");
	fs::create_dir_all(&agent).expect("v1 agent dir");
	fs::create_dir_all(root.path().join("data")).expect("data dir");
	// SAFETY: nextest runs each test in its own process and this runs before
	// the composition spawns anything that reads the environment.
	unsafe {
		std::env::set_var("HOME", &home);
		std::env::set_var("OMP_DATA_DIR", root.path().join("data"));
		std::env::set_var("OMP_CONFIG_DIR", root.path().join("config"));
		std::env::set_var("OMP_STATE_DIR", root.path().join("state"));
		std::env::set_var("OMP_CACHE_DIR", root.path().join("cache"));
		std::env::set_var("OMP_LLM_KEY_SOURCE", "local-file");
		std::env::set_var("OMP_ANTIGRAVITY_VERSION", "1.0.0");
		std::env::remove_var("OMP_PROFILE");
		std::env::remove_var("OMP_EASYCLIPROXY_API_KEY");
		std::env::remove_var("OMP_DEFAULT_MODEL");
	}
	let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
	let port = listener.local_addr().expect("address").port();
	let chats = Arc::new(AtomicUsize::new(0));
	let server = tokio::spawn(serve(listener, status, Arc::clone(&chats)));
	fs::write(
		agent.join("models.yml"),
		format!(
			"providers:\n  easycliproxy:\n    baseUrl: http://127.0.0.1:{port}/v1\n    apiKey: \
			 {API_KEY}\n    discovery:\n      type: openai-models-list\n"
		),
	)
	.expect("v1 models.yml");
	let data_dir = omp_core::dirs::data_dir(None).expect("default data dir");

	let inference = production_inference_for_session(
		&data_dir,
		Arc::new(omp_tool::Registry::default()),
		None,
		InferenceSessionOverrides::default(),
	)
	.await
	.expect("production inference composes");
	let model = ModelKey::from("easycliproxy/fake-model");
	assert!(inference.catalog().model(&model).is_some(), "the imported key listed the model");
	let mut routes = PinnedRoutes::new(
		inference.registry.clone(),
		CallMeta {
			id:             RequestId::from("provider-rejection"),
			target:         Target::Model(model),
			deadline:       None,
			budget:         ExecutionBudget::default(),
			session:        None,
			debug_session:  None,
			response_hooks: Default::default(),
		},
		CallAffinity::none(),
	);
	let Err(error) = routes.client_mut().execute(chat()).await else {
		panic!("the provider rejects every chat request");
	};
	server.abort();
	(error, chats.load(Ordering::SeqCst))
}

/// A 402 asks for another account; with none, the caller sees the 402.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_payment_failure_with_no_other_account_reports_the_402() {
	let (error, chats) = rejected_chat("402 Payment Required").await;
	assert_eq!(chats, 1, "the rotation never reached the wire");
	assert_eq!(error.kind, ErrorKind::PaymentRequired, "{error}");
	assert_eq!(error.status, Some(402), "{error}");
}

/// A 401 asks for a refreshed credential; a static key cannot be renewed, so
/// the caller sees the 401.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_rejected_static_key_reports_the_401() {
	let (error, chats) = rejected_chat("401 Unauthorized").await;
	assert_eq!(chats, 1, "the refresh never reached the wire");
	assert_eq!(error.kind, ErrorKind::Authentication, "{error}");
	assert_eq!(error.status, Some(401), "{error}");
}
