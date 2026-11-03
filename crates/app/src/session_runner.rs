//! Application-owned composition and lifetime of live session controllers.

use std::{
	path::{Path, PathBuf},
	sync::Arc,
};

use miette::{IntoDiagnostic, miette};
use omp_chat::{
	HostCommand,
	overlays::{Services, services::Mutations},
};
use omp_core::FastHashMap;
use omp_driver::{
	collab::session::{CollabCommandHandle, CollabSessionAuthority},
	headless::kernel::{ComposedInference, KernelOptions, SessionHome},
	session_wire::OpenHost,
	sessions::{SessionId, SessionRegistry},
};
use omp_serve::session::SessionControlError;
use parking_lot::{Mutex, RwLock};

use crate::{
	chat_cmd::{Launch, LaunchEnv},
	chat_services::{AppServices, ServiceState, StackHandles, trace::TraceLog},
	cli::ChatArgs,
};

pub(crate) fn services(
	launch: &Launch,
	kernel: &omp_agent::Kernel<ComposedInference>,
	session: &omp_session::Session,
	collab: &CollabCommandHandle,
	live_journal: &Arc<RwLock<PathBuf>>,
	discovery: Option<omp_driver::registry::DiscoveryRefreshSender>,
	trace: Arc<TraceLog>,
) -> miette::Result<(Arc<dyn Services>, Arc<dyn Mutations>)> {
	let composed = kernel.inference();
	let environment = composed.environment();
	let state_dir =
		omp_env::project_state::directory(&launch.data_dir, &launch.project).into_diagnostic()?;
	let services = Arc::new(AppServices::new(ServiceState {
		data_dir: launch.data_dir.clone(),
		project: launch.project.clone(),
		approval_override: launch.options.approval_mode,
		plugin_dirs: launch.options.extensions.native_roots.clone(),
		workspace_trust: composed.workspace_trust().trust(),
		sessions_dir: launch
			.sessions_dir
			.clone()
			.unwrap_or_else(|| state_dir.join("sessions")),
		state_dir,
		journal: session.journal_path().to_path_buf(),
		live_journal: Arc::clone(live_journal),
		model: launch.model.clone(),
		catalog: composed.live_catalog(),
		discovery,
		registry: Arc::clone(kernel.tool_registry()),
		con: Arc::clone(&launch.ctx),
		sessions: Arc::clone(&launch.live_sessions),
		collab: collab.clone(),
		env: composed.environment_client().clone(),
		mcp: environment.mcp_inspector(),
		reload: environment.extension_reload_handle(),
		memory: environment.memory_runtime(),
		stack: composed.production_stack().map(StackHandles::from_stack),
		trace,
		theme_catalog: Arc::clone(&launch.theme_catalog),
		runtime: tokio::runtime::Handle::current(),
	}));
	Ok((Arc::clone(&services) as Arc<dyn Services>, services as Arc<dyn Mutations>))
}

struct RunningSession {
	commands: flume::Sender<HostCommand>,
	task:     tokio::task::JoinHandle<()>,
}

struct Registration {
	sessions: Arc<SessionRegistry>,
	id:       SessionId,
}

impl Drop for Registration {
	fn drop(&mut self) {
		self.sessions.remove(&self.id);
	}
}

pub(crate) struct AppOpenHost {
	env:      LaunchEnv,
	args:     ChatArgs,
	sessions: Arc<SessionRegistry>,
	running:  Mutex<FastHashMap<SessionId, RunningSession>>,
}

impl AppOpenHost {
	pub(crate) fn new(mut env: LaunchEnv, args: ChatArgs, sessions: Arc<SessionRegistry>) -> Self {
		env.workspace_trust = None;
		Self { env, args, sessions, running: Mutex::new(FastHashMap::default()) }
	}

	pub(crate) fn commands(&self, id: &SessionId) -> Option<flume::Sender<HostCommand>> {
		self
			.running
			.lock()
			.get(id)
			.map(|running| running.commands.clone())
	}

	async fn start(&self, project_root: &Path) -> miette::Result<SessionId> {
		let project = std::fs::canonicalize(project_root).into_diagnostic()?;
		let ctx = Arc::new(crate::process_ctx_with(
			&project,
			omp_chat::HostMailbox::new().attach(omp_con::Ctx::builder()),
		)?);
		let mut args = self.args.clone();
		args.project = project;
		args.continue_session = false;
		args.resume = None;
		args.fork = None;
		args.from_claude = false;
		args.from_codex = false;
		args.no_session = false;
		args.trust_workspace = false;
		args.provider_session = None;
		args.prompt.clear();
		let mut launch = Launch::prepare_headless(args, ctx, self.env.clone()).await?;
		launch.live_sessions = Arc::clone(&self.sessions);
		launch.options.sessions = Some(Arc::clone(&self.sessions));
		let (mut kernel, session) = launch.compose().await?;
		let id = session
			.journal_path()
			.file_stem()
			.and_then(|name| name.to_str())
			.map(SessionId::new)
			.ok_or_else(|| miette!("new session journal has no UTF-8 identifier"))?;
		let registration =
			Registration { sessions: Arc::clone(&self.sessions), id: id.clone() };
		if self.sessions.lookup(&id).is_none() {
			return Err(miette!("new session kernel was not registered"));
		}
		let live_journal = Arc::new(RwLock::new(session.journal_path().to_path_buf()));
		let (collab_authority, collab) = CollabSessionAuthority::new();
		let collab_owner = omp_driver::collab::session::spawn_session_owner(collab_authority);
		let trace = TraceLog::record(kernel.subscribe(), &tokio::runtime::Handle::current());
		let (services, mutations) =
			services(&launch, &kernel, &session, &collab, &live_journal, None, trace)?;
		let up = kernel.mailbox();
		let home = SessionHome::new(
			&launch.data_dir,
			&launch.project,
			&KernelOptions {
				sessions_dir: launch.sessions_dir.clone(),
				sessions: Some(Arc::clone(&self.sessions)),
				..KernelOptions::default()
			},
			launch.model.clone(),
			up,
		)
		.into_diagnostic()?
		.with_facts_of(&session)
		.with_rules(Arc::clone(kernel.inference().rule_scope()))
		.with_workspace_trust(kernel.inference().workspace_trust().clone());
		let live_catalog = kernel
			.inference()
			.live_catalog()
			.unwrap_or_else(|| omp_driver::registry::LiveCatalog::Fixed(Arc::clone(&launch.catalog)));
		let live_auth = kernel
			.inference()
			.production_stack()
			.map(|stack| stack.auth_manager.clone());
		let env = kernel.inference().environment_client().clone();
		let ask = omp_driver::headless::AskRoute::new();
		let ui = Arc::new(crate::chat_services::extension_ui::ChatUiOwner::new(
			Arc::clone(&launch.ctx),
			ask.clone(),
			Some(collab.clone()),
		));
		kernel
			.inference()
			.environment()
			.bind_ask_presenter(Arc::clone(&ui) as Arc<dyn omp_tools::ask::AskPresenter>);
		let extension_ui = kernel
			.inference()
			.environment()
			.bind_domain_control_factories(omp_envd::exthost::ExternalDomainControlFactories {
				ui: Some(ui.factory()),
				..omp_envd::exthost::ExternalDomainControlFactories::default()
			});
		let (relay, _events) = flume::unbounded();
		let (controller, _snapshot) = crate::chat_control::Controller::new(
			kernel,
			session,
			home,
			relay,
			Arc::clone(&launch.ctx),
			mutations,
			services,
			collab,
			Some(live_catalog),
			env,
			live_journal,
			launch.data_dir.clone(),
			live_auth,
			None,
			ask,
		);
		let (commands, command_rx) = flume::unbounded();
		let task = tokio::spawn(async move {
			let _registration = registration;
			let _collab_owner = collab_owner;
			let _extension_ui = extension_ui;
			if let Err(error) = controller.run(command_rx).await {
				tracing::warn!(%error, "opened session controller stopped");
			}
		});
		let mut running = self.running.lock();
		running.retain(|_, session| !session.task.is_finished());
		running.insert(id.clone(), RunningSession { commands, task });
		Ok(id)
	}
}

impl Drop for AppOpenHost {
	fn drop(&mut self) {
		for session in self.running.get_mut().values() {
			let _ = session.commands.send(HostCommand::Quit);
		}
	}
}

#[tonic::async_trait]
impl OpenHost for AppOpenHost {
	async fn open_new(&self, project_root: &Path) -> Result<SessionId, SessionControlError> {
		self
			.start(project_root)
			.await
			.map_err(|error| SessionControlError::Backend(error.into()))
	}
}
