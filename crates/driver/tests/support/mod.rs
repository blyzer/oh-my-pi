//! Shared fixtures for the driver's integration tests.

use std::{path::Path, sync::Arc, time::Duration};

use omp_agent::Kernel;
use omp_core::{Principal, Str, sf};
use omp_driver::headless::kernel::{
	EnvToolExecutor, SettingsAdmission, bind_environment_approvals, install_tool_authority,
};
use omp_envd::{
	EnvServer, ProjectEnvironment, RegistryBridges, exthost::ConvarControlFactory,
	tool_settings::ApprovalMode, worker::ExtHostConfig,
};
use omp_session::Session;
use omp_tool::Registry;
use tokio::{net::UnixStream, task::JoinHandle};
use tokio_util::sync::CancellationToken;

/// Installs the session's tool authority on `kernel`, which serves `session`
/// first and runs `environment`'s tools, as `compose_kernel` installs it: the
/// environment's approvals bound to the kernel, the environment executor, and
/// the kernel's admission under `con` and the invocation's `mode` (`None`: the
/// shipped default), keying fetches on the environment's hosts. Both share the
/// session's one approval-posture report.
#[allow(dead_code, reason = "only some test crates sharing this module compose a kernel")]
pub fn install_authority<C>(
	kernel: Kernel<C>,
	environment: &ProjectEnvironment,
	session: &Session,
	con: &omp_con::Ctx,
	mode: Option<ApprovalMode>,
	root: &Path,
) -> Kernel<C> {
	let approvals = bind_environment_approvals(&kernel, environment);
	install_tool_authority(
		kernel,
		session,
		EnvToolExecutor::new(environment.client().clone(), approvals),
		SettingsAdmission::new(con, mode, root).with_fetch_hosts(environment.fetch_hosts()),
	)
}

/// The typed payload of every `approval-posture` notice the session
/// journaled under its turns.
#[allow(dead_code, reason = "only some test crates sharing this module read the posture")]
pub fn posture_notices(session: &Session) -> Vec<serde_json::Value> {
	let dom = session.dom();
	dom.select("body turn notice")
		.expect("selector")
		.filter_map(|handle| dom.get(handle))
		.filter(|node| {
			node
				.prop(&omp_dom::PropKey::Custom(Str::new_static("name")))
				.and_then(omp_dom::Value::as_str)
				== Some(omp_envd::admission::APPROVAL_POSTURE_NOTICE)
		})
		.map(|node| match node.prop(&omp_dom::PropId::Data.into()) {
			Some(omp_dom::Value::Json(data)) => serde_json::from_str(data.get()).expect("payload"),
			_ => panic!("the notice carries its typed payload"),
		})
		.collect()
}

/// The posture payload of a defaulted `yolo` that no sandbox keeps: `write`
/// is in force.
#[allow(dead_code, reason = "only some test crates sharing this module read the posture")]
pub fn downgraded_default_yolo() -> serde_json::Value {
	serde_json::json!({
		"configured": "yolo",
		"provenance": "default",
		"effective": "write",
		"sandbox": { "state": "off" },
	})
}

/// Bounded wait for the daemon's listener.
const LISTEN_WAIT: Duration = Duration::from_secs(30);

/// A project daemon this test process serves on the production socket of the
/// project's state directory, opened by `EnvServer::open_project` as
/// `omp envd` opens it; like it, its host binds no approval route.
///
/// `ProjectEnvironment::attach` finds it there and, because it runs the same
/// executable and so has the same build id, joins it as a peer instead of
/// spawning a daemon or falling back to an embedded environment, provided the
/// session's control context resolves the daemon's sandbox and approval policy:
/// the socket is keyed by that policy. Dropping it stops serving.
pub struct InProcessDaemon {
	shutdown: CancellationToken,
	serving:  JoinHandle<Result<(), omp_envd::EnvdError>>,
}

impl InProcessDaemon {
	/// Serves the daemon for the project at `root` with state under `state`,
	/// under the daemon's own control context `con`, on the socket its policy
	/// keys.
	#[allow(dead_code, reason = "only some test crates sharing this module serve a daemon")]
	pub async fn serve(root: &Path, state: &Path, con: Arc<omp_con::Ctx>) -> Self {
		let socket = omp_env::project_state::environment_socket(
			state,
			&omp_envd::daemon_policy::from_con(&con),
		)
		.expect("environment socket");
		Self::serve_at(root, state, con, socket).await
	}

	/// Serves the daemon like [`Self::serve`], but on `socket` whatever policy
	/// it keys: a daemon a session reaches though it enforces another policy.
	#[allow(dead_code, reason = "only some test crates sharing this module misplace a daemon")]
	pub async fn serve_at(
		root: &Path,
		state: &Path,
		con: Arc<omp_con::Ctx>,
		socket: std::path::PathBuf,
	) -> Self {
		let convars = Arc::new(ConvarControlFactory::new(Arc::clone(&con)));
		let server = EnvServer::open_project(
			root,
			state,
			&omp_env::project_state::document_socket(state),
			Registry::new(),
			ExtHostConfig::current(
				Principal::new(sf!("daemon-tester"), sf!("Daemon Tester")),
				sf!("daemon-session"),
				1,
			)
			.expect("daemon host configuration"),
			None,
			false,
			None,
			&con,
			convars,
			RegistryBridges::default(),
		)
		.await
		.expect("project daemon");
		let shutdown = CancellationToken::new();
		let serving = tokio::spawn({
			let server = Arc::new(server);
			let socket = socket.clone();
			let shutdown = shutdown.clone();
			async move { server.serve_uds(&socket, shutdown, None).await }
		});
		tokio::time::timeout(LISTEN_WAIT, async {
			while UnixStream::connect(&socket).await.is_err() {
				tokio::time::sleep(Duration::from_millis(10)).await;
			}
		})
		.await
		.expect("the project daemon never listened");
		Self { shutdown, serving }
	}
}

impl Drop for InProcessDaemon {
	fn drop(&mut self) {
		self.shutdown.cancel();
		self.serving.abort();
	}
}
