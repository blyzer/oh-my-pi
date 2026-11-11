//! Standalone daemon startup and the embedded document authority.

use super::{DocumentAuthority, EnvServer, error::EnvdError, prelude::*};

#[cfg(unix)]
impl DocumentAuthority {
	pub(in crate::server) async fn finished_result(
		&mut self,
	) -> Option<Result<daemon::Result, JoinError>> {
		if !self.task.as_ref()?.is_finished() {
			return None;
		}
		Some(
			self
				.task
				.take()
				.expect("finished document authority task")
				.await,
		)
	}
}

impl Drop for DocumentAuthority {
	fn drop(&mut self) {
		self.shutdown.cancel();
	}
}

/// Assembles and runs the standalone environment daemon with the production
/// built-in registry.
#[cfg(unix)]
pub async fn run(
	args: EnvdConfig,
	con: Arc<Ctx>,
	bridges: RegistryBridges,
) -> Result<(), EnvdError> {
	run_with_registry(args, Registry::new(), con, bridges).await
}

/// Assembles production dispatch plus caller-provided tool revisions.
#[cfg(unix)]
#[tracing::instrument(
	name = "environment_daemon_run",
	level = "debug",
	skip_all,
	fields(
		root = %args.root.display(),
		idle_timeout_s = args.idle_timeout,
		py_eval = args.py_eval
	)
)]
pub async fn run_with_registry(
	args: EnvdConfig,
	registry: Registry,
	con: Arc<Ctx>,
	bridges: RegistryBridges,
) -> Result<(), EnvdError> {
	let workspace = WorkspaceHost::open(&args.root)?;
	let root = workspace.root().to_path_buf();
	let data_dir = omp_core::dirs::data_dir(None).map_err(io::Error::other)?;
	let interrupt_grace = crate::host_settings::SV_INTERRUPT_GRACE.get(&con);
	let state_dir = if let Some(path) = args.state_dir {
		path
	} else {
		omp_env::project_state::directory(&data_dir, &root)?
	};
	ensure_directory(&state_dir)?;
	let socket = match args.socket {
		Some(socket) => socket,
		None => omp_env::project_state::environment_socket(
			&state_dir,
			&crate::daemon_policy::from_con(&con, args.workspace_trust),
		)?,
	};
	let mut bridges = bridges;
	bridges.content.workspace_trust = args.workspace_trust;
	let require_document_ownership = args.docserver_socket.is_none();
	let docserver_socket = if let Some(socket) = args.docserver_socket {
		socket
	} else {
		let socket = omp_env::project_state::document_socket(&state_dir);
		socket
	};
	if require_document_ownership {
		ensure_document_socket_free(&root, &docserver_socket).await?;
	}
	let (principal_authority, session_id, session_generation) = authenticated_runtime_identity()?;
	let mut ext_host_config = ExtHostConfig::current(
		principal_authority.principal().clone(),
		session_id.clone(),
		session_generation,
	)?;
	ext_host_config.interrupt_grace = interrupt_grace;
	ext_host_config.py_eval = args.py_eval;
	let (env_connections, env_connection_rx) = watch::channel(0);
	let (doc_connections, doc_connection_rx) = watch::channel(0);
	let convars = Arc::new(crate::exthost::ConvarControlFactory::new(Arc::clone(&con)));
	let server = Arc::new(
		EnvServer::open_project(
			&root,
			&state_dir,
			&docserver_socket,
			registry,
			ext_host_config,
			Some(doc_connections),
			require_document_ownership,
			None,
			&con,
			convars,
			bridges,
		)
		.await?,
	);
	let process_shutdown = CancellationToken::new();
	let signal_shutdown = process_shutdown.clone();
	let signal_task = tokio::spawn(async move {
		let mut terminate = signal(SignalKind::terminate());
		match terminate.as_mut() {
			Ok(terminate) => {
				tokio::select! {
					_ = ctrl_c() => {},
					_ = terminate.recv() => {},
				}
			},
			Err(_) => {
				let _ = ctrl_c().await;
			},
		}
		signal_shutdown.cancel();
	});
	let listener_shutdown = CancellationToken::new();
	let serve_shutdown = listener_shutdown.clone();
	let serve_socket = socket.clone();
	let serve_server = Arc::clone(&server);
	let mut serve_task = tokio::spawn(async move {
		serve_server
			.serve_uds(&serve_socket, serve_shutdown, Some(env_connections))
			.await
	});
	let idle_timeout = Duration::from_secs(args.idle_timeout);
	let idle_state_dir = state_dir.clone();
	let idle_server_build = Str::from(omp_env::build_id::current());
	let idle_processes = server.process_host();
	let idle = async move {
		wait_idle(
			env_connection_rx,
			doc_connection_rx,
			1,
			idle_timeout,
			idle_state_dir,
			idle_server_build,
			idle_processes,
		)
		.await;
	};
	tokio::pin!(idle);
	let serve_result = async {
		tokio::select! {
			() = process_shutdown.cancelled() => {
				listener_shutdown.cancel();
				serve_task.await??;
			},
			() = &mut idle => {
				listener_shutdown.cancel();
				serve_task.await??;
			},
			result = &mut serve_task => {
				result??;
				tokio::select! {
					() = process_shutdown.cancelled() => {},
					() = &mut idle => {},
				}
			},
		}
		listener_shutdown.cancel();
		Ok::<(), EnvdError>(())
	}
	.await;
	listener_shutdown.cancel();
	let interrupt_grace = interrupt_grace.to_std().map_err(EnvdError::state)?;
	server.shutdown_managed(interrupt_grace).await;
	signal_task.abort();
	serve_result
}

#[cfg(unix)]
pub(in crate::server) async fn wait_idle(
	mut env: watch::Receiver<usize>,
	mut docs: watch::Receiver<usize>,
	reserved_docs: usize,
	timeout: Duration,
	state_dir: PathBuf,
	server_build: Str,
	processes: ExecHost,
) {
	const BUILD_CHECK_INTERVAL: Duration = Duration::from_millis(50);

	let mut env_open = true;
	let mut docs_open = true;
	loop {
		while *env.borrow() != 0
			|| *docs.borrow() > reserved_docs
			|| processes.has_live_persistent_processes()
		{
			tokio::select! {
				result = env.changed(), if env_open => env_open = result.is_ok(),
				result = docs.changed(), if docs_open => docs_open = result.is_ok(),
				() = time::sleep(BUILD_CHECK_INTERVAL) => {
					if *env.borrow() == 0
						&& *docs.borrow() <= reserved_docs
						&& crate::launcher_build_is_stale(&state_dir, server_build.as_str())
					{
						return;
					}
				},
			}
		}
		if crate::launcher_build_is_stale(&state_dir, server_build.as_str()) {
			return;
		}
		let idle = async {
			if timeout.is_zero() {
				future::pending::<()>().await;
			} else {
				time::sleep(timeout).await;
			}
		};
		tokio::pin!(idle);
		loop {
			tokio::select! {
				() = &mut idle => return,
				result = env.changed(), if env_open => {
					env_open = result.is_ok();
					if *env.borrow() != 0 || *docs.borrow() > reserved_docs {
						break;
					}
				},
				result = docs.changed(), if docs_open => {
					docs_open = result.is_ok();
					if *env.borrow() != 0 || *docs.borrow() > reserved_docs {
						break;
					}
				},
				() = time::sleep(BUILD_CHECK_INTERVAL) => {
					if crate::launcher_build_is_stale(&state_dir, server_build.as_str()) {
						return;
					}
					if processes.has_live_persistent_processes() {
						break;
					}
				},
			}
		}
	}
}
/// Reports the transport limitation on platforms without a local IPC backend.
#[cfg(not(any(unix, windows)))]
pub async fn run(_args: EnvdConfig, _bridges: RegistryBridges) -> Result<(), EnvdError> {
	Err(
		io::Error::new(io::ErrorKind::Unsupported, "envd requires a Unix-domain socket in Phase 1")
			.into(),
	)
}

/// Configuration inputs a started document authority discovers its LSP and
/// DAP rosters from.
#[derive(Clone, Default)]
pub(in crate::server) struct DocumentConfigInputs {
	/// User configuration root probed for `lsp.json` and `dap.json`.
	pub(in crate::server) user_config_root: Option<PathBuf>,
	/// Installed, enabled Claude-format marketplace plugins whose LSP and DAP
	/// declarations join discovery.
	pub(in crate::server) claude_plugins:   Arc<[omp_ext::claude_plugin::ClaudePlugin]>,
	/// The workspace trust a started authority loads the project's LSP/DAP
	/// configuration and local bins under, and that a running one must report
	/// to be joined.
	pub(in crate::server) workspace_trust:  WorkspaceTrust,
}

/// The user configuration root the document authority probes for `lsp.json`
/// and `dap.json` overrides (`<root>` and `<root>/agent`).
///
/// User configuration lives in `~/.o2` ([`omp_core::dirs::user_config_root`],
/// profile-aware), never under the data directory: a language-server
/// declaration in `~/.o2/lsp.json` must reach the native LSP supervisor.
///
/// # Errors
///
/// Returns [`omp_core::dirs::DataDirError::HomeUnset`] when no home directory
/// is set.
pub fn document_user_config_root() -> Result<PathBuf, omp_core::dirs::DataDirError> {
	omp_core::dirs::user_config_root()
}

#[cfg(any(unix, windows))]
pub(in crate::server) async fn rehost_document_authority(
	root: &Path,
	state_dir: &Path,
	socket: &Path,
	connections: Option<watch::Sender<usize>>,
	lsp: crate::docserver::NativeLspOptions,
	config: DocumentConfigInputs,
	server_build: Str,
) -> Result<Option<DocumentAuthority>, EnvdError> {
	if crate::launcher_build_is_stale(state_dir, server_build.as_str()) {
		tracing::debug!(
			build = %server_build,
			"stale-build environment daemon declined to rehost document authority"
		);
		return Ok(None);
	}
	let (_connection, authority) = connect_or_start_docserver(
		root,
		socket,
		connections,
		false,
		lsp,
		config,
		server_build.clone(),
	)
	.await?;
	if crate::launcher_build_is_stale(state_dir, server_build.as_str()) {
		drop(authority);
		return Ok(None);
	}
	Ok(authority)
}

#[cfg(unix)]
pub(in crate::server) async fn connect_or_start_docserver(
	root: &Path,
	socket: &Path,
	connections: Option<watch::Sender<usize>>,
	require_ownership: bool,
	lsp: crate::docserver::NativeLspOptions,
	config: DocumentConfigInputs,
	server_build: Str,
) -> Result<(DocumentHost, Option<DocumentAuthority>), EnvdError> {
	const HANDOFF_TIMEOUT: Duration = Duration::from_secs(5);
	const RETRY_STEP: Duration = Duration::from_millis(50);
	const STARTUP_TIMEOUT: Duration = Duration::from_secs(10);

	let handoff_deadline = Instant::now() + HANDOFF_TIMEOUT;
	'connect_or_serve: loop {
		if let Ok(stream) = UnixStream::connect(socket).await {
			if require_ownership {
				return Err(document_authority_held(root));
			}
			match DocumentHost::connect_uds_stream(socket, stream).await {
				Ok(documents) => {
					validate_document_authority(root, documents.hello(), config.workspace_trust)?;
					if !omp_env::build_id::is_stale(
						server_build.as_str(),
						documents.hello().server_build.as_str(),
					) {
						return Ok((documents, None));
					}
					tracing::warn!(
						socket = %socket.display(),
						owner_build = %documents.hello().server_build,
						launcher_build = %server_build,
						"waiting for stale-build document daemon to drain"
					);
					drop(documents);
				},
				Err(error) if Instant::now() >= handoff_deadline => return Err(error.into()),
				Err(_) => {},
			}
			if Instant::now() >= handoff_deadline {
				return Err(document_authority_held(root));
			}
			time::sleep(RETRY_STEP).await;
			continue;
		}
		if let Some(parent) = socket.parent() {
			fs::create_dir_all(parent)?;
		}

		let shutdown = CancellationToken::new();
		let task_shutdown = shutdown.clone();
		let task_root = root.to_path_buf();
		let task_socket = socket.to_path_buf();
		let task_lsp = lsp.clone();
		let task_config = config.clone();
		let task_server_build = server_build.clone();
		let task_connections = connections.clone();
		let task = tokio::spawn(async move {
			crate::docserver::daemon::serve(
				task_root,
				daemon::Transport::Socket(task_socket),
				crate::docserver::daemon::ServeOptions {
					lsp_config_paths: Vec::new(),
					lsp:              task_lsp,
					user_config_root: task_config.user_config_root,
					claude_plugins:   task_config.claude_plugins,
					workspace_trust:  task_config.workspace_trust,
					shutdown:         Some(task_shutdown),
					server_build:     task_server_build,
					connections:      task_connections,
				},
			)
			.await
		});
		let mut authority = DocumentAuthority { shutdown, task: Some(task) };
		let startup_deadline = Instant::now() + STARTUP_TIMEOUT;
		loop {
			if let Some(result) = authority.finished_result().await {
				match result? {
					Ok(()) => return Err(EnvdError::DocserverExited),
					Err(error) if document_daemon_authority_held(&error) => {
						if Instant::now() >= handoff_deadline {
							return Err(document_authority_held(root));
						}
						time::sleep(RETRY_STEP).await;
						continue 'connect_or_serve;
					},
					Err(error) => return Err(EnvdError::document(error)),
				}
			}
			if let Ok(stream) = UnixStream::connect(socket).await {
				match DocumentHost::connect_uds_stream(socket, stream).await {
					Ok(documents) => {
						validate_document_authority(root, documents.hello(), config.workspace_trust)?;
						if omp_env::build_id::is_stale(
							server_build.as_str(),
							documents.hello().server_build.as_str(),
						) {
							drop(documents);
							drop(authority);
							if Instant::now() >= handoff_deadline {
								return Err(document_authority_held(root));
							}
							time::sleep(RETRY_STEP).await;
							continue 'connect_or_serve;
						}
						return Ok((documents, Some(authority)));
					},
					Err(_) => {},
				}
			}
			if Instant::now() >= startup_deadline {
				return Err(
					io::Error::new(io::ErrorKind::TimedOut, "document-server hello timed out").into(),
				);
			}
			time::sleep(RETRY_STEP).await;
		}
	}
}

#[cfg(unix)]
pub(in crate::server) fn document_daemon_authority_held(error: &daemon::Error) -> bool {
	match error {
		daemon::Error::AcquireAuthorityLock { .. } => true,
		daemon::Error::Document(crate::docserver::Error::Io { source, .. }) => {
			source.kind() == io::ErrorKind::WouldBlock
		},
		_ => false,
	}
}

pub(in crate::server) fn document_authority_held(path: &Path) -> EnvdError {
	EnvdError::DocumentAuthorityHeldBy { path: path.to_path_buf() }
}

#[cfg(windows)]
pub(in crate::server) async fn connect_or_start_docserver(
	root: &Path,
	socket: &Path,
	connections: Option<watch::Sender<usize>>,
	require_ownership: bool,
	lsp: crate::docserver::NativeLspOptions,
	config: DocumentConfigInputs,
	server_build: Str,
) -> Result<(DocumentHost, Option<DocumentAuthority>), EnvdError> {
	if let Ok(stream) = crate::docserver::windows::connect_owner_pipe(socket) {
		if require_ownership {
			return Err(document_authority_held(root));
		}
		let documents = DocumentHost::connect_pipe_stream(socket, stream).await?;
		validate_document_authority(root, documents.hello(), config.workspace_trust)?;
		if omp_env::build_id::is_stale(server_build.as_str(), documents.hello().server_build.as_str())
		{
			return Err(document_authority_held(root));
		}
		return Ok((documents, None));
	}
	let listener = OwnerPipeListener::bind(socket)?;
	let workspace_trust = config.workspace_trust;
	let server_config = crate::docserver::ServerConfig::new(root)
		.map_err(EnvdError::document)?
		.with_server_build(server_build)
		.with_project_binaries(lsp.project_binaries)
		.with_workspace_trust(workspace_trust);
	let environment =
		crate::docserver::Environment::new(server_config).map_err(EnvdError::document)?;
	if lsp.enabled {
		match crate::docserver::NativeLspSupervisor::discover(
			&environment,
			config.user_config_root.as_deref(),
			Arc::clone(&config.claude_plugins),
		) {
			Ok(supervisor) => {
				environment.install_lsp_supervisor(supervisor.clone());
				if !lsp.lazy {
					supervisor.warm_all();
				}
			},
			Err(error) => {
				tracing::warn!(%error, "native LSP discovery failed; continuing without servers");
			},
		}
	}
	let shutdown = CancellationToken::new();
	let task_shutdown = shutdown.clone();
	let task = tokio::spawn(async move {
		crate::docserver::windows::serve_owner_pipe(
			environment,
			listener,
			ConnectionConfig::default(),
			task_shutdown,
			connections,
		)
		.await
	});
	let authority = DocumentAuthority { shutdown, task: Some(task) };
	let stream = crate::docserver::windows::connect_owner_pipe(socket)?;
	let documents = DocumentHost::connect_pipe_stream(socket, stream).await?;
	validate_document_authority(root, documents.hello(), workspace_trust)?;
	Ok((documents, Some(authority)))
}

/// Admits the document authority that answered `hello` only when it serves
/// `root` and loads the project's LSP/DAP configuration under the session's
/// own workspace `trust`, so a trusted and an untrusted session never share
/// one language- and debug-server roster.
pub(in crate::server) fn validate_document_authority(
	root: &Path,
	hello: &DocumentHello,
	trust: WorkspaceTrust,
) -> Result<(), EnvdError> {
	let authority_root = Url::parse(&hello.root_uri)
		.ok()
		.and_then(|uri| uri.to_file_path().ok())
		.and_then(|path| fs::canonicalize(path).ok());
	let expected_root = fs::canonicalize(root)?;
	if authority_root.as_deref() != Some(expected_root.as_path()) {
		return Err(EnvdError::Document(sf!(
			"document authority root does not match environment root"
		)));
	}
	let served = hello.workspace_trust;
	if served.is(trust) {
		Ok(())
	} else {
		Err(EnvdError::DocumentAuthorityTrustMismatch {
			path: expected_root,
			expected: trust,
			served,
		})
	}
}

/// Refuses standalone-daemon startup while another process serves the project
/// document authority.
///
/// A daemon must own its document authority: joining a foreign authority as a
/// client would chain daemon lifetimes across builds, keeping a draining
/// generation alive forever through the successor's own connection.
#[cfg(unix)]
pub(in crate::server) async fn ensure_document_socket_free(
	root: &Path,
	socket: &Path,
) -> Result<(), EnvdError> {
	match UnixStream::connect(socket).await {
		Ok(_) => Err(document_authority_held(root)),
		Err(_) => Ok(()),
	}
}

#[cfg(unix)]
pub(in crate::server) fn ensure_directory(path: &Path) -> io::Result<()> {
	use std::os::unix::fs::PermissionsExt as _;

	// A pre-existing parent (e.g. `/tmp`) is not ours to re-mode; chmod only
	// directories this call created.
	if fs::metadata(path).is_ok_and(|metadata| metadata.is_dir()) {
		return Ok(());
	}
	fs::create_dir_all(path)?;
	fs::set_permissions(path, fs::Permissions::from_mode(0o700))
}
