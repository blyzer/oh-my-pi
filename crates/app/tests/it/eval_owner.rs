//! The eval bridge binds one session identity for the bind and the lookup, on
//! the production composition path.
//!
//! A real `compose_kernel` over an embedded project environment and a scripted
//! inference gateway runs one `dyn eval` cell through `bash`, exactly as a
//! model reaches it under the default tools policy. The cell can only run when
//! the owner the kernel's dispatch stamps on the call names the parent session
//! the composition bound.

use std::{env, fs, path::Path, sync::Arc};

use bytes::Bytes;
use omp_agent::{RunControl, TurnInput};
use omp_ai::{
	BlockKind, ChatEvent, Completion, ExecutionReceipt, FinishReason, Registry, ToolCall, Usage,
	call::OpaqueJson,
	layer::{LayerCall, stack::RouteProviderService},
	provider::fake::{FakeProvider, FakeScript},
	receipt::ReasonId,
	registry::RouteUnavailable,
	session::ConversationSessionPlanner,
};
use omp_catalog::{
	ManagementCapabilities, OperationBits, OperationKind,
	snapshot::{Catalog, SnapshotProvenance},
};
use omp_core::{Str, sf};
use omp_driver::headless::kernel::{KernelOptions, compose_kernel};
use omp_envd::{
	exec_settings::{ExecSandboxMode, SV_SANDBOX_MODE},
	tool_settings::SV_TOOLS_APPROVAL,
};
use omp_proto::inference::v1::inference_server::InferenceServer;
use omp_serve::inference::InferenceRpc;
use tokio_util::sync::CancellationToken;
use tonic::transport::{Channel, Server};

/// The cell's result is spelled by concatenation, so the marker appears in the
/// journal only when the cell actually ran.
const CELL: &str = r#"dyn eval --json '{"i":"Running an eval cell","language":"py","code":"\"eval-owner-\" + \"ok\""}'"#;

/// The tool registry the gateway lowers calls against: the `bash` declaration
/// the production environment advertises.
fn bash_declaration() -> omp_tool::Registry {
	let mut registry = omp_tool::Registry::new();
	registry
		.register_worker(
			omp_tools::shell::spec(&omp_tools::shell::ShellPromptSnapshot {
				sibling_tools:       Arc::from([]),
				platform:            sf!("test"),
				command_prefix:      false,
				embedded_builtins:   true,
				devices:             true,
				interceptor_enabled: false,
				interceptor_rules:   Arc::from([]),
			}),
			omp_tool::Presentation::Slot,
			omp_tool::Claims {
				precedence: omp_tool::Precedence::CORE,
				claimant:   sf!("omp/core"),
				replaces:   None,
			},
		)
		.expect("bash declaration registers");
	registry
}

/// An inference gateway served in this process whose one chat route answers
/// from scripts.
struct Gateway {
	channel:  Channel,
	model:    Str,
	fake:     FakeProvider,
	_serving: tokio::task::JoinHandle<()>,
	_inbox:   flume::Receiver<omp_ai::event::WorkflowResponse>,
}

impl Gateway {
	async fn start(scratch: &Path) -> Self {
		let mut compiled = Catalog::embedded().compiled().clone();
		for provider in &mut compiled.providers {
			provider.management = ManagementCapabilities {
				operations:        OperationBits::empty(),
				multiple_accounts: false,
				refresh:           false,
				principal_quota:   false,
			};
		}
		let artifacts = Catalog::encode(compiled, SnapshotProvenance { source_digest: [0; 32] })
			.expect("catalog snapshot");
		let catalog = Arc::new(Catalog::decode(&artifacts.postcard).expect("catalog decode"));
		let model = catalog
			.models()
			.iter()
			.find(|model| {
				model
					.capabilities
					.operations
					.contains_kind(OperationKind::Chat)
			})
			.expect("chat model");
		let route_id = model.routes.first().expect("chat route").clone();
		let model_key = Str::new(model.key.as_str());
		let route = catalog.route(&route_id).expect("route");
		let fake = FakeProvider::new(route.provider.clone(), route_id.clone());
		let service = {
			let fake = fake.clone();
			RouteProviderService::new(tower::service_fn(move |call: LayerCall<omp_ai::Call>| {
				tower::ServiceExt::oneshot(fake.clone(), call.payload)
			}))
		};
		let mut builder = Registry::builder(Arc::clone(&catalog));
		for candidate in catalog.routes() {
			builder = if candidate.id == route_id {
				builder.register_route(candidate.id.clone(), service.clone())
			} else {
				builder.register_unavailable(RouteUnavailable::new(
					candidate.id.clone(),
					ReasonId(sf!("eval-owner-scripted-route-only")),
					None,
				))
			}
			.expect("route registers");
		}
		let sessions = ConversationSessionPlanner::open(
			scratch.join("gateway-sessions.db"),
			Arc::clone(&catalog),
		)
		.expect("conversation store");
		let (responses, inbox) = flume::bounded(32);
		let rpc = InferenceRpc::new_for_test(
			builder.build().expect("registry"),
			sessions,
			Arc::new(bash_declaration()),
			responses,
		);
		let socket = scratch.join("gateway.sock");
		let listener = omp_rpc::uds::listen(&socket).await.expect("gateway socket");
		let serving = tokio::spawn(async move {
			let _ = Server::builder()
				.add_service(InferenceServer::new(rpc))
				.serve_with_incoming(listener)
				.await;
		});
		let channel = omp_rpc::uds::connect(&socket)
			.await
			.expect("gateway channel");
		Self { channel, model: model_key, fake, _serving: serving, _inbox: inbox }
	}

	fn completed(reason: FinishReason, blocks: u32) -> Result<ChatEvent, omp_ai::Error> {
		Ok(ChatEvent::Completed(Completion {
			reason,
			blocks,
			usage: Usage::default(),
			receipt: ExecutionReceipt::default().into(),
		}))
	}

	/// Scripts a `bash` call running `dyn eval`, then the final answer.
	fn script_eval_turn(&self) {
		let arguments = serde_json::json!({ "command": CELL, "i": "Running an eval cell" });
		let call = ToolCall {
			id:        "eval-owner-regression".into(),
			name:      sf!("bash"),
			arguments: OpaqueJson::new(arguments.clone()),
		};
		self.fake.push(FakeScript::chat(vec![
			Ok(ChatEvent::ToolCallStarted {
				index: 0,
				id:    call.id.clone(),
				name:  call.name.clone(),
			}),
			Ok(ChatEvent::ToolArgumentsDelta { index: 0, bytes: Bytes::from(arguments.to_string()) }),
			Ok(ChatEvent::ToolCallReady { index: 0, call }),
			Self::completed(FinishReason::ToolCalls, 1),
		]));
		self.fake.push(FakeScript::chat(vec![
			Ok(ChatEvent::BlockStarted { index: 0, kind: BlockKind::Text }),
			Ok(ChatEvent::TextDelta { index: 0, text: Str::new_static("Done.") }),
			Self::completed(FinishReason::Stop, 1),
		]));
	}
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_composed_kernel_runs_an_eval_cell_for_its_own_session() {
	let scratch = tempfile::tempdir().expect("scratch");
	let root = fs::canonicalize(scratch.path()).expect("canonical scratch");
	let project = root.join("project");
	let data = root.join("data");
	fs::create_dir_all(&project).expect("project");
	fs::create_dir_all(&data).expect("data");
	// SAFETY: nextest runs each test in its own process, and nothing else in
	// this one reads the environment concurrently.
	unsafe {
		env::set_var("HOME", root.join("home"));
		env::set_var("OMP_CONFIG_DIR", root.join("home").join(omp_core::dirs::CONFIG_DIR_NAME));
		env::set_var("OMP_DATA_DIR", &data);
		env::set_var("OMP_STATE_DIR", root.join("state"));
		env::set_var("OMP_CACHE_DIR", root.join("cache"));
		for name in ["OMP_PROFILE", "PI_PROFILE", "OMP_CONFIG_FILES"] {
			env::remove_var(name);
		}
	}
	// `bash` and `eval` are the vehicle of the proof, so no approval prompt
	// may park them.
	let con = omp_con::Ctx::new();
	SV_SANDBOX_MODE
		.set(&con, ExecSandboxMode::Off)
		.expect("sandbox off");
	SV_TOOLS_APPROVAL
		.set(
			&con,
			omp_con::Kv(vec![
				(sf!("bash"), omp_con::Value::Str(sf!("allow"))),
				(sf!("eval"), omp_con::Value::Str(sf!("allow"))),
			]),
		)
		.expect("allow bash and eval");
	let gateway = Gateway::start(&root).await;
	let journal = root.join("sessions/eval-owner.oms");
	let options = KernelOptions {
		session: Some(journal.clone()),
		sessions_dir: journal.parent().map(Path::to_path_buf),
		// The scripted gateway resolves only the `bash` declaration. `eval` is
		// a `dyn` device under the default tools policy, so the model reaches
		// it through `bash`.
		tools: Some(vec![sf!("bash")]),
		gateway: Some(gateway.channel.clone()),
		..KernelOptions::default()
	};
	let (mut kernel, mut session, _) =
		Box::pin(compose_kernel(&data, &project, gateway.model.as_str(), Arc::new(con), options))
			.await
			.expect("composition");

	gateway.script_eval_turn();
	let turn = kernel
		.run_turn(
			&mut session,
			TurnInput { text: Str::new_static("run an eval cell"), attachments: Vec::new() },
			RunControl::new(CancellationToken::new(), None),
		)
		.await;

	let journal = fs::read_to_string(&journal).expect("read composed session journal");
	assert!(
		!journal.contains("not bound for this owner"),
		"the eval bridge refused the kernel's own session (turn: {turn:?}): {journal}"
	);
	assert!(
		journal.contains("eval-owner-ok"),
		"the eval cell did not return its result (turn: {turn:?}): {journal}"
	);
	turn.expect("eval turn");
}
