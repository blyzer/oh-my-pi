//! Characterization tests pinning the complete probe output for realistic
//! provider payloads.
//!
//! Each case replays recorded or documented response shapes through the public
//! probe API and snapshots the full normalized rows (or the stable error code)
//! plus the request sequence. The snapshots were recorded against the
//! `serde_json::Value`-walking decoder and pin its observable behavior for the
//! typed wire decoder that replaced it.

use std::{fmt::Write as _, sync::Arc};

use parking_lot::Mutex;

use super::*;
use crate::discovery::endpoints::configured_endpoint;

/// One scripted response: request URL, optional exact request body, body.
type Route = (&'static str, Option<&'static str>, &'static str);

#[derive(Clone)]
struct Recorded {
	routes: Arc<[Route]>,
	log:    Arc<Mutex<Vec<String>>>,
}

impl DiscoveryHttpClient for Recorded {
	fn request(&self, request: ProbeHttpRequest, _: CancellationToken) -> ProbeHttpFuture {
		let body = std::str::from_utf8(&request.body).unwrap_or("<binary>");
		let content_type = request
			.headers
			.get(http::header::CONTENT_TYPE)
			.and_then(|value| value.to_str().ok())
			.unwrap_or("-");
		self
			.log
			.lock()
			.push(format!("{} {} [{content_type}] {body}", request.method, request.url));
		let response = self
			.routes
			.iter()
			.find(|(url, expected_body, _)| {
				*url == request.url.as_str() && expected_body.is_none_or(|expected| expected == body)
			})
			.map(|(_, _, response)| Bytes::from_static(response.as_bytes()));
		Box::pin(async move { response.ok_or(ProbeError::HttpStatus { status: 404 }) })
	}
}

fn probe_for(kind: DiscoveryEndpointKind, base_url: &str) -> DiscoveryProbe {
	let provider = <&'static str>::from(kind);
	DiscoveryProbe {
		provider:     ProviderId::from(provider),
		route:        RouteId::new(format!("{provider}/primary")),
		proxy_routes: (kind == DiscoveryEndpointKind::Proxy).then(|| ProxyDiscoveryRoutes {
			openai:    RouteId::from("proxy/openai"),
			anthropic: RouteId::from("proxy/anthropic"),
		}),
		headers:      http::HeaderMap::new(),
		endpoint:     configured_endpoint(kind, base_url).expect("endpoint"),
	}
}

fn render(result: Result<Vec<DiscoveredModel>, ProbeError>, log: &Mutex<Vec<String>>) -> String {
	let mut requests = log.lock().clone();
	requests.sort();
	let mut out = String::new();
	for request in requests {
		let _ = writeln!(out, "> {request}");
	}
	match result {
		Ok(rows) if rows.is_empty() => out.push_str("(no rows)"),
		Ok(rows) => {
			for row in rows {
				let _ = writeln!(out, "{}", serde_json::to_string(&row).expect("row JSON"));
			}
		},
		Err(error) => {
			let _ = write!(out, "error: {}", <&'static str>::from(&error));
		},
	}
	out
}

async fn run(kind: DiscoveryEndpointKind, base_url: &str, routes: &[Route]) -> String {
	let client = Recorded { routes: routes.into(), log: Arc::default() };
	let result = probe_for(kind, base_url)
		.probe(&client, CancellationToken::new())
		.await;
	render(result, &client.log)
}

async fn run_model(
	kind: DiscoveryEndpointKind,
	base_url: &str,
	routes: &[Route],
	model: &str,
) -> String {
	let client = Recorded { routes: routes.into(), log: Arc::default() };
	let result = probe_for(kind, base_url)
		.probe_model(WireModelId::from_ref(model), &client, CancellationToken::new())
		.await
		.map(|row| row.into_iter().collect());
	render(result, &client.log)
}

/// Runs one single-endpoint listing per labeled body and joins the outputs.
async fn run_bodies(
	kind: DiscoveryEndpointKind,
	base_url: &str,
	url: &'static str,
	bodies: &[(&str, &'static str)],
) -> String {
	let mut out = String::new();
	for (label, body) in bodies {
		let output = run(kind, base_url, &[(url, None, body)]).await;
		let _ = writeln!(out, "## {label}\n{output}\n");
	}
	out
}

const OLLAMA: &str = "http://127.0.0.1:11434";
const OLLAMA_TAGS: &str = "http://127.0.0.1:11434/api/tags";
const OLLAMA_SHOW: &str = "http://127.0.0.1:11434/api/show";

const OLLAMA_TAGS_BODY: &str = r#"{"models":[
	{"name":"deepseek-r1:latest","model":"deepseek-r1:latest","modified_at":"2025-05-10T08:06:48.639712648-07:00","size":4683075271,"digest":"0a8c266910232fd3291e71e5ba1e058cc5af9d411192cf88b6d30e92b6e73163","details":{"parent_model":"","format":"gguf","family":"qwen2","families":["qwen2"],"parameter_size":"7.6B","quantization_level":"Q4_K_M"}},
	{"name":"llama3.2-vision:11b","model":"llama3.2-vision:11b","size":7901829417,"details":{"format":"gguf","family":"mllama","families":["mllama"],"parameter_size":"10.7B","quantization_level":"Q4_K_M"}},
	{"name":"Qwen 3 Coder","model":"qwen3-coder:30b","details":{"family":"qwen3moe"}},
	{"name":"nomic-embed-text:latest","model":"nomic-embed-text:latest","details":{"family":"nomic-bert","families":["nomic-bert"],"parameter_size":"137M","quantization_level":"F16"}},
	{"name":"missing-show:latest","model":"missing-show:latest"},
	{"name":"broken-show:latest","model":"broken-show:latest"},
	{"name":"odd-info:latest","model":"odd-info:latest"},
	{"name":"null-show:latest","model":"null-show:latest","context_length":4096}
]}"#;

const OLLAMA_SHOW_DEEPSEEK: &str = r##"{"modelfile":"# Modelfile generated by \"ollama show\"\nFROM /models/blobs/sha256-96c4\nTEMPLATE \"\"\"{{ .System }}\"\"\"\nPARAMETER stop \"<｜begin▁of▁sentence｜>\"","parameters":"stop                           \"<｜begin▁of▁sentence｜>\"\nnum_ctx                        24576\ntemperature 0.6","template":"{{- if .System }}{{ .System }}{{ end }}","details":{"parent_model":"","format":"gguf","family":"qwen2","families":["qwen2"],"parameter_size":"7.6B","quantization_level":"Q4_K_M"},"model_info":{"general.architecture":"qwen2","general.file_type":15,"general.parameter_count":7615616512,"qwen2.attention.layer_norm_rms_epsilon":0.000001,"qwen2.context_length":131072,"qwen2.rope.freq_base":10000,"tokenizer.ggml.tokens":null},"capabilities":["completion","thinking"],"modified_at":"2025-05-10T08:06:48.639712648-07:00"}"##;

const OLLAMA_SHOW_VISION: &str = r#"{"license":"LLAMA 3.2 COMMUNITY LICENSE","parameters":"temperature 0.6\ntop_p 0.9","details":{"family":"mllama"},"model_info":{"general.architecture":"mllama","mllama.context_length":131072,"mllama.vision.image_size":560},"projector_info":{"mllama.vision.block_count":32},"capabilities":["completion","vision","tools"]}"#;

const OLLAMA_SHOW_CODER: &str = r#"{"parameters":"","model_info":{"general.architecture":"qwen3moe","qwen3moe.context_length":"262144"},"capabilities":["Completion","TOOLS","Insert"]}"#;

const OLLAMA_SHOW_EMBED: &str = r#"{"model_info":{"general.architecture":"nomic-bert","nomic-bert.context_length":2048},"capabilities":["embedding"]}"#;

const OLLAMA_SHOW_ODD: &str = r#"{"parameters":"num_ctx notanumber\nnum_ctx 0","model_info":{"general.architecture":"x","x.context_length":"garbage","context_length":9999},"context_length":"7777","capabilities":{"vision":true,"thinking":false},"supports_reasoning":null}"#;

fn ollama_routes() -> Vec<Route> {
	vec![
		(OLLAMA_TAGS, None, OLLAMA_TAGS_BODY),
		(OLLAMA_SHOW, Some(r#"{"model":"deepseek-r1:latest"}"#), OLLAMA_SHOW_DEEPSEEK),
		(OLLAMA_SHOW, Some(r#"{"model":"llama3.2-vision:11b"}"#), OLLAMA_SHOW_VISION),
		(OLLAMA_SHOW, Some(r#"{"model":"qwen3-coder:30b"}"#), OLLAMA_SHOW_CODER),
		(OLLAMA_SHOW, Some(r#"{"model":"nomic-embed-text:latest"}"#), OLLAMA_SHOW_EMBED),
		(OLLAMA_SHOW, Some(r#"{"model":"broken-show:latest"}"#), r#"{"parameters":"num_ctx 1024""#),
		(OLLAMA_SHOW, Some(r#"{"model":"odd-info:latest"}"#), OLLAMA_SHOW_ODD),
		(OLLAMA_SHOW, Some(r#"{"model":"null-show:latest"}"#), "null"),
	]
}

#[tokio::test]
async fn ollama_tags_with_show_metadata() {
	let output = run(DiscoveryEndpointKind::Ollama, OLLAMA, &ollama_routes()).await;
	insta::assert_snapshot!("ollama_tags_with_show_metadata", output);
}

#[tokio::test]
async fn ollama_selected_model_reprobe() {
	let routes = ollama_routes();
	let mut out = String::new();
	for model in ["deepseek-r1:latest", "llama3.2-vision:11b", "broken-show:latest", "absent"] {
		let output = run_model(DiscoveryEndpointKind::Ollama, OLLAMA, &routes, model).await;
		let _ = writeln!(out, "## {model}\n{output}\n");
	}
	insta::assert_snapshot!("ollama_selected_model_reprobe", out);
}

#[tokio::test]
async fn ollama_tags_edge_cases() {
	let output = run_bodies(DiscoveryEndpointKind::Ollama, OLLAMA, OLLAMA_TAGS, &[
		("empty list", r#"{"models":[]}"#),
		("name only", r#"{"models":[{"name":"named:latest","size":1}]}"#),
		("model preferred over name", r#"{"models":[{"model":"wire:7b","name":"Display"}]}"#),
		("null model does not fall back to name", r#"{"models":[{"model":null,"name":"x"}]}"#),
		("numeric model", r#"{"models":[{"model":7,"name":"x"}]}"#),
		("blank id", r#"{"models":[{"model":"  "}]}"#),
		("missing id fails listing", r#"{"models":[{"model":"ok"},{"details":{}}]}"#),
		("non-object entry", r#"{"models":["qwen"]}"#),
		("missing models key", r#"{"object":"list"}"#),
		("models is null", r#"{"models":null}"#),
		("bare array", r#"[{"model":"bare"}]"#),
		("invalid JSON", r#"{"models":["#),
		(
			"duplicate tag rows merge",
			r#"{"models":[{"model":"dup"},{"model":"dup","name":"Dup","context_length":4096}]}"#,
		),
	])
	.await;
	insta::assert_snapshot!("ollama_tags_edge_cases", output);
}

const LLAMA: &str = "http://127.0.0.1:8080/v1";
const LLAMA_MODELS: &str = "http://127.0.0.1:8080/models";
const LLAMA_PROPS: &str = "http://127.0.0.1:8080/props";

const LLAMA_MODELS_BODY: &str = r#"{"models":[{"name":"gemma-3-4b-it-Q4_K_M.gguf","model":"gemma-3-4b-it-Q4_K_M.gguf","modified_at":"","size":"","digest":"","type":"model","description":"","tags":[""],"capabilities":["completion","multimodal"],"parameters":"","details":{"parent_model":"","format":"gguf","family":"","families":[""],"parameter_size":"","quantization_level":""}}],"object":"list","data":[{"id":"gemma-3-4b-it-Q4_K_M.gguf","object":"model","created":1754000000,"owned_by":"llamacpp","meta":{"vocab_type":1,"n_vocab":262144,"n_ctx_train":131072,"n_embd":2560,"n_params":3880263168,"size":2489757856}}]}"#;

const LLAMA_PROPS_BODY: &str = r#"{"default_generation_settings":{"id":0,"id_task":-1,"n_ctx":8192,"speculative":false,"is_processing":false,"params":{"n_predict":-1,"seed":4294967295,"temperature":0.800000011920929,"samplers":["top_k","top_p"]}},"total_slots":1,"model_path":"/models/gemma-3-4b-it-Q4_K_M.gguf","chat_template":"{{ bos_token }}","modalities":{"vision":true,"audio":false},"build_info":"b6000-abcdef"}"#;

const LLAMA_ROUTER_BODY: &str = r#"{"object":"list","data":[
	{"id":"qwen","object":"model","owned_by":"llamacpp","status":{"value":"loaded","args":["llama-server","-m","/m/qwen.gguf","--ctx-size","16384","--port","8081"]}},
	{"id":"gemma","object":"model","status":{"value":"unloaded","preset":"[gemma]\nmodel = /m/gemma.gguf\nctx-size = 32768\n"}},
	{"id":"phi","status":{"value":"loaded","args":["-c=8192"]}},
	{"id":"mistral","status":{"value":"loaded","args":["--ctx-size","abc","-c",4096]}},
	{"id":"trailing","status":{"value":"loaded","args":["--ctx-size"],"preset":"ctx-size=2048"}},
	{"id":"runtime","meta":{"n_ctx":"12288","n_ctx_train":65536},"status":{"args":["-c","999"]}},
	{"id":"bare","status":"loaded"},
	{"id":"plain"}
]}"#;

#[tokio::test]
async fn llama_cpp_models_and_props() {
	let mut out = String::new();
	for (label, routes) in [
		("single model with props", vec![
			(LLAMA_MODELS, None, LLAMA_MODELS_BODY),
			(LLAMA_PROPS, None, LLAMA_PROPS_BODY),
		]),
		("props unavailable", vec![(LLAMA_MODELS, None, LLAMA_MODELS_BODY)]),
		("router mode with bounded props", vec![
			(LLAMA_MODELS, None, LLAMA_ROUTER_BODY),
			(
				LLAMA_PROPS,
				None,
				r#"{"default_generation_settings":{"n_ctx":"4096","params":{"n_predict":512}}}"#,
			),
		]),
		("router mode with top-level unlimited props", vec![
			(LLAMA_MODELS, None, LLAMA_ROUTER_BODY),
			(LLAMA_PROPS, None, r#"{"n_ctx":2048,"max_tokens":" -1 ","supports_vision":true}"#),
		]),
		("props malformed", vec![
			(LLAMA_MODELS, None, r#"{"data":[{"id":"plain"}]}"#),
			(LLAMA_PROPS, None, r#"{"n_ctx":"#),
		]),
		("props training context only", vec![
			(LLAMA_MODELS, None, r#"{"data":[{"id":"plain","max_output_tokens":1000000}]}"#),
			(LLAMA_PROPS, None, r#"{"n_ctx":0,"n_ctx_train":"65536","n_predict":-1.0}"#),
		]),
		("props not an object", vec![
			(LLAMA_MODELS, None, r#"{"data":[{"id":"plain","capabilities":["vision"]}]}"#),
			(LLAMA_PROPS, None, "[1]"),
		]),
		("models malformed", vec![
			(LLAMA_MODELS, None, "not json"),
			(LLAMA_PROPS, None, LLAMA_PROPS_BODY),
		]),
	] {
		let output = run(DiscoveryEndpointKind::LlamaCpp, LLAMA, &routes).await;
		let _ = writeln!(out, "## {label}\n{output}\n");
	}
	insta::assert_snapshot!("llama_cpp_models_and_props", out);
}

const LM_STUDIO: &str = "http://127.0.0.1:1234";
const LM_STUDIO_MODELS: &str = "http://127.0.0.1:1234/api/v0/models";

const LM_STUDIO_BODY: &str = r#"{"object":"list","data":[
	{"id":"qwen2-vl-7b-instruct","object":"model","type":"vlm","publisher":"mlx-community","arch":"qwen2_vl","compatibility_type":"mlx","quantization":"4bit","state":"not-loaded","max_context_length":32768},
	{"id":"meta-llama-3.1-8b-instruct","object":"model","type":"llm","publisher":"lmstudio-community","arch":"llama","compatibility_type":"gguf","quantization":"Q4_K_M","state":"loaded","max_context_length":131072,"loaded_context_length":4096,"capabilities":["tool_use"]},
	{"id":"text-embedding-nomic-embed-text-v1.5","object":"model","type":"embeddings","publisher":"nomic-ai","arch":"nomic-bert","compatibility_type":"gguf","quantization":"Q4_0","state":"not-loaded","max_context_length":2048},
	{"id":"deepseek-r1-distill","type":"llm","state":"loaded","loaded_context_length":null,"max_context_length":"65536","capabilities":["tool_use","reasoning"]},
	{"id":"gemma-3-12b","type":"vlm","state":"loaded","loaded_context_length":0,"context_length":8192,"capabilities":{"vision":true,"trained_for_tool_use":true}},
	{"id":"unloaded-context-length","state":"not-loaded","context_length":"16384","max_context_length":32768},
	{"id":"stateless"}
]}"#;

#[tokio::test]
async fn lm_studio_models() {
	let output = run_bodies(DiscoveryEndpointKind::LmStudio, LM_STUDIO, LM_STUDIO_MODELS, &[
		("documented listing", LM_STUDIO_BODY),
		("empty", r#"{"object":"list","data":[]}"#),
	])
	.await;
	insta::assert_snapshot!("lm_studio_models", output);
}

const LITELLM: &str = "http://primary:4000/v1";
const LITELLM_GROUP: &str = "http://primary:4000/model_group/info";
const LITELLM_V2: &str = "http://primary:4000/v2/model/info";
const LITELLM_INFO: &str = "http://primary:4000/model/info";
const LITELLM_V1_INFO: &str = "http://primary:4000/v1/model/info";
const LITELLM_V1_MODELS: &str = "http://primary:4000/v1/models";

const LITELLM_GROUP_COMPLETE: &str = r#"{"data":[
	{"model_group":"gpt-4o","providers":["openai"],"max_input_tokens":128000,"max_output_tokens":16384,"input_cost_per_token":0.0000025,"output_cost_per_token":0.00001,"cache_read_input_token_cost":0.00000125,"cache_creation_input_token_cost":0.0000025,"mode":"chat","tpm":null,"rpm":null,"supports_parallel_function_calling":true,"supports_vision":true,"supports_function_calling":true,"supported_openai_params":["temperature","max_tokens"],"configurable_clientside_auth_params":null},
	{"model_group":"claude-sonnet","providers":["anthropic"],"max_input_tokens":200000,"max_output_tokens":64000,"input_cost_per_token":0.000003,"output_cost_per_token":0.000015,"cache_read_input_token_cost":3e-7,"cache_creation_input_token_cost":0.00000375,"supports_vision":true,"supports_reasoning":true}
]}"#;

const LITELLM_GROUP_PARTIAL: &str = r#"{"data":[
	{"model_group":"gpt-4o","providers":["openai"],"max_input_tokens":128000,"input_cost_per_token":"0.0000025","supports_vision":null},
	{"model_group":"local-llama","providers":[" ",""],"supports_reasoning":"yes"},
	{"model_group":7,"model_name":"shadowed"},
	{"model_name":"  "},
	"junk",
	["also","junk"],
	{"id":"embed-large","mode":"embedding","input_cost_per_token":-1,"output_cost_per_token":"abc","model_info":{"output_cost_per_token":0.5}},
	{"name":"named-only","base_model":"azure/gpt-4o"},
	{"litellm_params":{"model":"  openai/gpt-4.1  "}}
]}"#;

const LITELLM_V2_BODY: &str = r#"{"data":[
	{"model_name":"gpt-4o","litellm_params":{"model":"openai/gpt-4o","api_base":"https://api.openai.com"},"model_info":{"id":"abc","db_model":false,"max_output_tokens":"16384","output_cost_per_token":0.00001,"cache_read_input_token_cost":null,"base_model":null}},
	{"model_name":"local-llama","litellm_params":{"model":"ollama/llama3","api_base":"http://localhost:11434"},"model_info":{"max_input_tokens":"8192","max_output_tokens":null,"input_cost_per_token":0,"output_cost_per_token":"0","supports_vision":false}},
	{"model_name":"embed-large","litellm_params":{"custom_llm_provider":" OpenAI ","model":"azure/x"}},
	{"model_name":"named-only","model_info":{"base_model":"openai/gpt-4o"}},
	{"model_name":"late-newcomer","providers":["openai"]}
]}"#;

const LITELLM_INFO_BODY: &str = r#"{"data":[
	{"model_name":"gpt-4o","model_info":{"cache_read_input_token_cost":"0.00000125","cache_creation_input_token_cost":0.0000025}},
	{"model_name":"local-llama","model_info":{"input_cost_per_token":0.0000001,"output_cost_per_token":0.0000002,"cache_read_input_token_cost":0.00000001,"cache_creation_input_token_cost":0.00000002}},
	{"model_name":"embed-large","input_cost_per_token":0.00000013,"output_cost_per_token":0,"cache_read_input_token_cost":1e-9,"cache_creation_input_token_cost":"2e-9"},
	{"model_name":"named-only","input_cost_per_token":1,"output_cost_per_token":2,"cache_read_input_token_cost":3,"cache_creation_input_token_cost":4},
	{"model_name":"gpt-4.1","input_cost_per_token":1,"output_cost_per_token":2,"cache_read_input_token_cost":3,"cache_creation_input_token_cost":4}
]}"#;

const LITELLM_V1_MODELS_BODY: &str = r#"{"object":"list","data":[
	{"id":"gpt-4o","object":"model","created":1677610602,"owned_by":"openai"},
	{"id":"openai/gpt-4.1-mini","object":"model","owned_by":"openai"},
	{"id":"my-local","object":"model","created":1677610602,"owned_by":"openai","context_length":"32768"},
	{"object":"model"},
	"junk",
	{"id":"   "},
	{"id":"gpt-4o","display_name":"GPT-4o"}
]}"#;

#[tokio::test]
async fn litellm_info_endpoints() {
	let mut out = String::new();
	for (label, routes) in [
		("complete first endpoint stops early", vec![(LITELLM_GROUP, None, LITELLM_GROUP_COMPLETE)]),
		("partial evidence walks every info endpoint", vec![
			(LITELLM_GROUP, None, LITELLM_GROUP_PARTIAL),
			(LITELLM_V2, None, LITELLM_V2_BODY),
			(LITELLM_INFO, None, LITELLM_INFO_BODY),
			(LITELLM_V1_INFO, None, r#"{"data":[{"model_name":"gpt-4o","supports_vision":true}]}"#),
		]),
		("unavailable group endpoint seeds from v2", vec![(LITELLM_V2, None, LITELLM_V2_BODY)]),
		("malformed info endpoints fall through", vec![
			(LITELLM_GROUP, None, "<html>proxy error</html>"),
			(LITELLM_V2, None, r#"{"detail":"Not Found"}"#),
			(LITELLM_INFO, None, r#"{"data":[]}"#),
			(LITELLM_V1_INFO, None, r#"{"data":["junk"]}"#),
			(LITELLM_V1_MODELS, None, LITELLM_V1_MODELS_BODY),
		]),
		("models fallback only", vec![(LITELLM_V1_MODELS, None, LITELLM_V1_MODELS_BODY)]),
		("models fallback malformed", vec![(LITELLM_V1_MODELS, None, r#"{"data":{}}"#)]),
		("models fallback invalid JSON", vec![(LITELLM_V1_MODELS, None, "{")]),
		("models fallback empty", vec![(LITELLM_V1_MODELS, None, r#"{"data":[]}"#)]),
		("nothing reachable", vec![]),
	] {
		let output = run(DiscoveryEndpointKind::LiteLlm, LITELLM, &routes).await;
		let _ = writeln!(out, "## {label}\n{output}\n");
	}
	insta::assert_snapshot!("litellm_info_endpoints", out);
}

const OPENAI: &str = "https://models.example/v1";
const OPENAI_MODELS: &str = "https://models.example/v1/models";

const VLLM_BODY: &str = r#"{"object":"list","data":[{"id":"meta-llama/Llama-3.1-8B-Instruct","object":"model","created":1726000000,"owned_by":"vllm","root":"meta-llama/Llama-3.1-8B-Instruct","parent":null,"max_model_len":32768,"permission":[{"id":"modelperm-1","object":"model_permission","allow_sampling":true}]}]}"#;

const OPENROUTER_STYLE_BODY: &str = r#"{"data":[
	{"id":"vendor/vision","name":"Vendor: Vision","context_length":"200000","architecture":{"modality":"text+image->text","input_modalities":["text","image"],"output_modalities":["text"]},"top_provider":{"max_completion_tokens":64000},"pricing":{"prompt":"0.000003"}},
	{"id":"vendor/text","name":"vendor/text","context_length":65536,"architecture":{"input_modalities":["text"]},"supported_parameters":["tools","reasoning"]}
]}"#;

const OPENAI_EDGE_BODY: &str = r#"{"data":[
	{"id":"caps-object","capabilities":{"image":"yes","vision":true,"thinking":true}},
	{"id":"caps-object-fallback","capabilities":{"vision":true,"reasoning":true}},
	{"id":"caps-array","capabilities":["Vision","Reasoning","tools"]},
	{"id":"caps-other","capabilities":"vision"},
	{"id":"vision-null","supports_vision":null},
	{"id":"vision-string","supports_vision":"true","supports_reasoning":false},
	{"id":"empty-modalities","input_modalities":[]},
	{"id":"input-mixed-case","input":["TEXT","Image",7]},
	{"id":"input-not-array","input":"image","input_modalities":{"image":true}},
	{"id":"float-context","context_length":8192.0,"contextWindow":"4096"},
	{"id":"negative-context","context_length":-1,"max_input_tokens":1000,"max_output_tokens":"99999"},
	{"id":"nested-info","model_info":{"max_input_tokens":"65536","max_output_tokens":4096}},
	{"id":"nested-info-not-object","model_info":"n/a","maxTokens":"2048"},
	{"id":"max-model-len","max_model_len":" 40960 ","max_output_tokens":0,"maxTokens":512},
	{"id":"plus-sign","context_length":"+4096"},
	{"id":"huge","context_length":18446744073709551615},
	{"id":"overflow","context_length":18446744073709551616},
	{"id":"display-null","display_name":null,"name":"Shadowed"},
	{"id":"display-camel","displayName":"Camel","name":"ignored"},
	{"id":"same-name","name":"same-name"},
	{"id":"escaped-é","name":"Esc\"aped \\ name\n"},
	{"id":"numeric-display","display_name":5,"name":"Ignored"},
	{"id":" padded ","extra":{"nested":[1,2,{"deep":null}]}}
]}"#;

#[tokio::test]
async fn openai_compatible_listings() {
	let output = run_bodies(DiscoveryEndpointKind::OpenAi, OPENAI, OPENAI_MODELS, &[
		("vllm", VLLM_BODY),
		("openrouter style", OPENROUTER_STYLE_BODY),
		("field edge cases", OPENAI_EDGE_BODY),
	])
	.await;
	insta::assert_snapshot!("openai_compatible_listings", output);
}

#[tokio::test]
async fn openai_compatible_envelopes_and_failures() {
	let output = run_bodies(DiscoveryEndpointKind::OpenAi, OPENAI, OPENAI_MODELS, &[
		("empty data", r#"{"object":"list","data":[]}"#),
		("bare array", r#"[{"id":"bare"}]"#),
		("models key", r#"{"models":[{"name":"from-name"}]}"#),
		("result.items nesting", r#"{"result":{"items":[{"model":"nested"}]}}"#),
		("data object with models", r#"{"data":{"models":[{"model_group":"grouped"}]}}"#),
		("null data falls through to models", r#"{"data":null,"models":[{"model_name":"named"}]}"#),
		("string data falls through to items", r#"{"data":"x","items":[{"id":"item"}]}"#),
		("data wins over models", r#"{"models":[{"id":"second"}],"data":[{"id":"first"}]}"#),
		("duplicate data key keeps last", r#"{"data":[{"id":"first"}],"data":[{"id":"last"}]}"#),
		(
			"id precedence",
			r#"{"data":[{"model_name":"e","model_group":"d","model":"c","name":"b","id":"a"}]}"#,
		),
		("non-string first id fails", r#"{"data":[{"id":5,"name":"x"}]}"#),
		("missing id fails", r#"{"data":[{"id":"ok"},{"object":"model"}]}"#),
		("blank id fails", r#"{"data":[{"id":""}]}"#),
		("string entry fails", r#"{"data":["gpt"]}"#),
		("array entry fails", r#"{"data":[["x"]]}"#),
		("no envelope", r#"{"object":"list"}"#),
		("scalar body", r#""models""#),
		("invalid JSON", "{\"data\":["),
		("trailing garbage", r#"{"data":[{"id":"ok"}]} trailing"#),
	])
	.await;
	insta::assert_snapshot!("openai_compatible_envelopes_and_failures", output);
}

#[tokio::test]
async fn proxy_routes_and_output_defaults() {
	let output = run_bodies(
		DiscoveryEndpointKind::Proxy,
		"https://proxy.example/v1",
		"https://proxy.example/v1/models",
		&[(
			"mixed endpoint evidence",
			r#"{"data":[
				{"id":"claude","supported_endpoint_types":["Anthropic","openai"],"context_length":200000},
				{"id":"claude-small","supported_endpoint_types":["anthropic"],"context_length":4096},
				{"id":"gpt","supported_endpoint_types":[1,"OPENAI"]},
				{"id":"string-types","supported_endpoint_types":"anthropic"},
				{"id":"other-types","supported_endpoint_types":["gemini"]},
				{"id":"opaque"},
				{"id":"claude","supported_endpoint_types":["openai"],"display_name":"Claude via OpenAI"}
			]}"#,
		)],
	)
	.await;
	insta::assert_snapshot!("proxy_routes_and_output_defaults", output);
}
