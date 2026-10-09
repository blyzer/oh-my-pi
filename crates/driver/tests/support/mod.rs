//! Shared fixtures for the driver's integration tests.

use std::{path::Path, sync::Arc, time::Duration};

use omp_core::{Principal, sf};
use omp_envd::{EnvServer, RegistryBridges, exthost::ConvarControlFactory, worker::ExtHostConfig};
use omp_tool::Registry;
use tokio::{net::UnixStream, task::JoinHandle};
use tokio_util::sync::CancellationToken;

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
