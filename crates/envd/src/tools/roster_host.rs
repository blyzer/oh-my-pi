//! Host adapters for the built-in construction table.
//!
//! `omp-tools` decides which row runs. This module supplies the authorities
//! those rows close over. It does not name a built-in in order to decide
//! whether that built-in is registered.

use std::{env, mem, path::Path, sync::Arc, time};

use omp_cache::github_cache::GithubCache;
use omp_con::Ctx;
use omp_core::{Str, sf};
use omp_memory::Capabilities;
use omp_tool::{Registry, Rev, Tool, ToolSpec, ToolsPolicy};
#[cfg(feature = "cap-media")]
use omp_tools::ask::AskVocalizer;
use omp_tools::{
	ask::AskPresenter,
	edit::observer::EditRepairClient,
	eval::{EvalSessionControl, EvalTool, PyEvalTool, TaskDescriptionSnapshot},
	grep::SearchPolicy,
	read::{ReadPolicy, conflicts::ConflictRegistry, resolver::ResolverTable},
	roster::{
		BuiltinRosterEntry, EnvironmentDeclare, EnvironmentInstall, RosterExtension, SessionInstall,
		ToolGate,
	},
	staging::StagedProposalRegistry,
};

use super::{
	bridges::{
		ContentResolver, GoalAuthority, HostResources, SearchInference, TelemetryUpload,
		environment_registry, invocation_edit_model, invocation_edit_repair, register_instrumented,
	},
	registry::{
		AgentCheckpointControl, EnvironmentDeclaration, EnvironmentDeclarationInputs,
		GoalControlAdapter, build_prelude_table, compose_eval_executor,
		configured_model_edit_revision, configured_model_identity, production_read_policy,
		production_search_policy, shell_timeout_bounds, state_error,
	},
};
#[cfg(feature = "cap-browser")]
use crate::browser_daemon::{BrowserDaemon, BrowserSettings};
#[cfg(feature = "cap-computer")]
use crate::computer::ComputerSessionHost;
#[cfg(feature = "cap-media")]
use crate::media_devices;
#[cfg(feature = "cap-media")]
use crate::media_tts::SpeechConfig;
use crate::{
	EnvdError,
	blobs::BlobHost,
	docs::{DocumentHost, ResourceMutationServices},
	document_cache,
	eval::{PreludeTable, ProcessEvalExec, SessionBridgeHost},
	exec::ExecHost,
	exec_settings::{SandboxSettings, ShellSettings},
	github::GithubService,
	github_url::GithubCredentialBridge,
	managed_skills::ManagedSkills,
	mcp::McpService,
	memory::ReflectionBridgeHost,
	report_issue::{self, ReportIssue},
	search_backend::SearchBridgeHost,
	security_scan::SecurityScanService,
	tool_ast_grep::AstSearchAuthority,
	tool_debug::DocumentDebugControl,
	tool_document::SessionReadBlobs,
	tool_lsp::DocumentLspControl,
	tool_read_sources::ReadSourceAdapter,
	tool_search::WorkspaceSearchAdapter,
	tool_settings::ToolSettings,
	tool_shell::ShellExecHost,
	tool_url::{UrlResolver, production_url_resolvers},
	vault::{VaultPaths, VaultService},
	worker::ExtHostSupervisor,
	workspace::WorkspaceHost,
};

pub(super) struct SessionOutput {
	pub search_bridge:      Arc<SearchBridgeHost>,
	pub github_credentials: Arc<GithubCredentialBridge>,
	pub ask_presenter:      omp_tools::ask::PresenterSlot,
	pub checkpoint_control: AgentCheckpointControl,
}

pub(super) struct SessionHost<'a> {
	registry:           &'a mut Registry,
	policy:             ToolsPolicy,
	tool_settings:      &'a ToolSettings,
	search_bridge:      Arc<SearchBridgeHost>,
	github_credentials: Arc<GithubCredentialBridge>,
	ask_presenter:      omp_tools::ask::PresenterSlot,
	goal_control:       Option<Arc<dyn GoalAuthority>>,
	telemetry_upload:   Option<Arc<dyn TelemetryUpload>>,
	telemetry:          &'a Arc<omp_cache::telemetry_cache::TelemetryIndex>,
	blobs:              &'a BlobHost,
	project_root:       &'a Path,
	state_dir:          &'a Path,
	github_cache:       Arc<GithubCache>,
	#[cfg(feature = "cap-media")]
	image_config:       media_devices::ImageConfig,
	#[cfg(feature = "cap-media")]
	speech_config:      SpeechConfig,
	checkpoint_control: AgentCheckpointControl,
	github:             Option<Arc<GithubService>>,
}

impl<'a> SessionHost<'a> {
	#[allow(
		clippy::too_many_arguments,
		reason = "session tool composition carries independent typed authorities"
	)]
	pub(super) fn new(
		registry: &'a mut Registry,
		policy: ToolsPolicy,
		tool_settings: &'a ToolSettings,
		search: Option<Arc<dyn SearchInference>>,
		ask_presenter: Option<Arc<dyn AskPresenter>>,
		goal_control: Option<Arc<dyn GoalAuthority>>,
		telemetry_upload: Option<Arc<dyn TelemetryUpload>>,
		blobs: &'a BlobHost,
		project_root: &'a Path,
		state_dir: &'a Path,
		telemetry: &'a Arc<omp_cache::telemetry_cache::TelemetryIndex>,
		github_cache: Arc<GithubCache>,
		#[cfg(feature = "cap-media")] image_config: media_devices::ImageConfig,
		#[cfg(feature = "cap-media")] speech_config: SpeechConfig,
	) -> Self {
		let search_bridge = Arc::new(SearchBridgeHost::new(search));
		let github_credentials = Arc::new(GithubCredentialBridge::new());
		let ask_presenter = omp_tools::ask::PresenterSlot::new(
			ask_presenter.unwrap_or_else(|| Arc::new(omp_tools::ask::HeadlessPresenter)),
		);
		Self {
			registry,
			policy,
			tool_settings,
			search_bridge,
			github_credentials,
			ask_presenter,
			goal_control,
			telemetry_upload,
			telemetry,
			blobs,
			project_root,
			state_dir,
			github_cache,
			#[cfg(feature = "cap-media")]
			image_config,
			#[cfg(feature = "cap-media")]
			speech_config,
			checkpoint_control: AgentCheckpointControl::default(),
			github: None,
		}
	}

	fn ensure_github(&mut self) -> Arc<GithubService> {
		if self.github.is_none() {
			if let Some(upload) = self.telemetry_upload.clone() {
				upload.start(Arc::clone(self.telemetry), Arc::clone(&self.github_credentials));
			}
			self.github = Some(GithubService::new(
				self.project_root.to_path_buf(),
				self.state_dir,
				Arc::clone(&self.github_credentials),
				Arc::clone(&self.github_cache),
				self.blobs.clone(),
			));
		}
		Arc::clone(self.github.as_ref().expect("github service"))
	}

	pub(super) fn output(&self) -> SessionOutput {
		SessionOutput {
			search_bridge:      Arc::clone(&self.search_bridge),
			github_credentials: Arc::clone(&self.github_credentials),
			ask_presenter:      self.ask_presenter.clone(),
			checkpoint_control: self.checkpoint_control.clone(),
		}
	}
}

impl ToolGate for SessionHost<'_> {
	fn tool_enabled(&self, key: &str) -> bool {
		self.tool_settings.enabled(key)
	}

	fn policy(&self) -> ToolsPolicy {
		self.policy
	}
}

impl SessionInstall for SessionHost<'_> {
	type Checkpoint = AgentCheckpointControl;
	type CiRouter = omp_ci_delegated::Delegated<
		crate::github_adapter::GithubCiAdapter,
		omp_ci_delegated::PermitAll,
	>;
	type Error = EnvdError;
	type ForgeRouter = omp_forge_delegated::Delegated<
		crate::github_adapter::GithubForgeAdapter,
		omp_forge_delegated::PermitAll,
	>;
	type Goal = GoalControlAdapter;
	#[cfg(feature = "cap-media")]
	type Image = media_devices::MediaDevice;
	type Report = ReportIssue;
	type Search = SearchBridgeHost;
	#[cfg(feature = "cap-media")]
	type Speech = media_devices::MediaDevice;

	fn register_session<T: Tool>(
		&mut self,
		tool: T,
		row: &BuiltinRosterEntry,
	) -> Result<(), Self::Error> {
		let (presentation, claims) = omp_tools::roster::registration(row, self.policy);
		register_instrumented(self.registry, tool, presentation, claims)?;
		Ok(())
	}

	fn register_extension(&mut self, extension: RosterExtension) -> Result<(), Self::Error> {
		extension(self.registry)?;
		Ok(())
	}

	#[cfg(feature = "cap-media")]
	fn image_tool(&mut self) -> Self::Image {
		media_devices::image_gen(
			Arc::clone(&self.search_bridge),
			self.image_config.clone(),
			self.blobs.clone(),
			self.project_root.to_path_buf(),
		)
	}

	#[cfg(feature = "cap-media")]
	fn speech_tool(&mut self) -> Self::Speech {
		media_devices::tts_with_config(
			Arc::clone(&self.search_bridge),
			Arc::clone(&self.github_credentials),
			self.speech_config.clone(),
			self.blobs.clone(),
			self.project_root.to_path_buf(),
		)
	}

	fn report_tool(&mut self) -> Self::Report {
		report_issue::tool(Arc::clone(self.telemetry))
	}

	fn search_backend(&self) -> Arc<Self::Search> {
		Arc::clone(&self.search_bridge)
	}

	fn forge_router(&mut self) -> Self::ForgeRouter {
		crate::github_adapter::forge_router(self.ensure_github())
	}

	fn ci_router(&mut self) -> Self::CiRouter {
		crate::github_adapter::ci_router(self.ensure_github())
	}

	fn current_repo(
		&self,
	) -> Arc<dyn Fn() -> Result<omp_forge::RepoRef, omp_forge::ForgeError> + Send + Sync> {
		let root = self.project_root.to_path_buf();
		Arc::new(move || checkout_repo(&root))
	}

	fn ask_presenter(&self) -> Arc<dyn AskPresenter> {
		Arc::new(self.ask_presenter.clone())
	}

	#[cfg(feature = "cap-media")]
	fn ask_vocalizer(&self) -> Arc<dyn AskVocalizer> {
		media_devices::ask_vocalizer(Arc::clone(&self.search_bridge))
	}

	fn goal_control(&self) -> Option<Self::Goal> {
		self.goal_control.clone().map(GoalControlAdapter)
	}

	fn checkpoint_control(&self) -> Self::Checkpoint {
		self.checkpoint_control.clone()
	}
}

fn checkout_repo(root: &Path) -> Result<omp_forge::RepoRef, omp_forge::ForgeError> {
	let repo = crate::github_url::infer_repo(root)
		.map_err(|_| omp_forge::ForgeError::Unscoped { op: omp_forge::ForgeOp::RepoView })?;
	let mut parts = repo.slug().split('/');
	let owner = parts.next().unwrap_or_default();
	let name = parts.next().unwrap_or_default();
	omp_forge::RepoRef::try_new(repo.host(), owner, name).map_err(omp_forge::ForgeError::from)
}

pub(super) struct DeclarationHost<'a> {
	tool_settings:   &'a ToolSettings,
	#[cfg(feature = "cap-browser")]
	browser_enabled: bool,
	inputs:          &'a EnvironmentDeclarationInputs,
	py_eval:         bool,
	policy:          ToolsPolicy,
	out:             Vec<EnvironmentDeclaration>,
}

impl<'a> DeclarationHost<'a> {
	pub(super) fn collect(
		tool_settings: &'a ToolSettings,
		#[cfg(feature = "cap-browser")] browser_enabled: bool,
		inputs: &'a EnvironmentDeclarationInputs,
		py_eval: bool,
		policy: ToolsPolicy,
		roster: &omp_tools::roster::SelectedRoster,
	) -> Vec<EnvironmentDeclaration> {
		let mut host = Self {
			tool_settings,
			#[cfg(feature = "cap-browser")]
			browser_enabled,
			inputs,
			py_eval,
			policy,
			out: Vec::new(),
		};
		omp_tools::roster::declare_environment(&mut host, roster);
		host.out
	}
}

impl ToolGate for DeclarationHost<'_> {
	fn tool_enabled(&self, key: &str) -> bool {
		self.tool_settings.enabled(key)
	}

	fn policy(&self) -> ToolsPolicy {
		self.policy
	}
}

impl EnvironmentDeclare for DeclarationHost<'_> {
	fn declare(&mut self, spec: ToolSpec, row: &BuiltinRosterEntry) {
		let (presentation, claims) = omp_tools::roster::declaration(row, self.policy);
		self
			.out
			.push(EnvironmentDeclaration { spec, presentation, claims });
	}

	#[cfg(feature = "cap-browser")]
	fn browser_enabled(&self) -> bool {
		self.browser_enabled
	}

	fn memory_capabilities(&self) -> Capabilities {
		self.inputs.memory
	}

	fn managed_skills(&self) -> bool {
		self.inputs.managed_skills
	}

	fn py_eval_enabled(&self) -> bool {
		self.py_eval
	}

	fn eval_description(&self) -> Option<Str> {
		self.inputs.eval_description.clone()
	}

	fn shell_snapshot(&self) -> Option<omp_tools::shell::ShellPromptSnapshot> {
		self.inputs.shell_snapshot.clone()
	}

	fn read_policy(&self) -> ReadPolicy {
		self.inputs.read_policy
	}

	fn search_policy(&self) -> SearchPolicy {
		production_search_policy(self.tool_settings)
	}

	fn selected_edit(&self) -> Rev {
		self.inputs.selected_edit.clone()
	}
}

pub(super) struct EnvironmentParts {
	pub reflection:   Arc<ReflectionBridgeHost>,
	pub eval_host:    Arc<SessionBridgeHost>,
	pub eval_control: EvalSessionControl,
	pub previews:     StagedProposalRegistry,
	pub resolvers:    Arc<ResolverTable<UrlResolver>>,
	pub prelude:      Arc<PreludeTable>,
}

pub(super) struct EnvironmentHost<'a> {
	registry:            &'a mut Registry,
	policy:              ToolsPolicy,
	tool_settings:       &'a ToolSettings,
	#[cfg(feature = "cap-browser")]
	browser_settings:    &'a BrowserSettings,
	shell_settings:      &'a ShellSettings,
	sandbox_settings:    &'a SandboxSettings,
	con:                 &'a Ctx,
	documents:           &'a DocumentHost,
	blobs:               &'a BlobHost,
	exec:                &'a ExecHost,
	workspace:           &'a WorkspaceHost,
	state_dir:           &'a Path,
	session_id:          &'a str,
	root_uri:            &'a Str,
	memory:              &'a Arc<omp_memory::MemoryRuntime>,
	memory_capabilities: Capabilities,
	workers:             &'a ExtHostSupervisor,
	interrupt_grace:     omp_core::Duration,
	py_eval:             bool,
	skills:              Option<Arc<ManagedSkills>>,
	github_credentials:  Arc<GithubCredentialBridge>,
	github_cache:        Arc<GithubCache>,
	url_resolvers:       Vec<Arc<dyn ContentResolver>>,
	host_resources:      Option<Arc<dyn HostResources>>,
	session_authority:   Option<Arc<dyn omp_contract::SessionAuthority>>,
	mcp:                 Arc<McpService>,
	edit_model:          Option<Str>,
	edit_repair:         Option<EditRepairClient>,
	workspace_trust:     omp_ext::workspace_trust::WorkspaceTrust,
	previews:            StagedProposalRegistry,
	reflection:          Arc<ReflectionBridgeHost>,
	security:            Option<SecurityScanService>,
	read_sources:        Option<ReadSourceAdapter>,
	read_blobs:          Option<SessionReadBlobs>,
	conflicts:           Option<Arc<ConflictRegistry>>,
	resolvers:           Option<Arc<ResolverTable<UrlResolver>>>,
	selected_edit:       Option<Rev>,
	read_policy:         Option<ReadPolicy>,
	search_policy:       Option<SearchPolicy>,
	search:              Option<WorkspaceSearchAdapter>,
	eval_host:           Option<Arc<SessionBridgeHost>>,
	prelude:             Option<Arc<PreludeTable>>,
	eval_control:        EvalSessionControl,
	eval_tool:           Option<EvalTool<ProcessEvalExec>>,
	py_tool:             Option<PyEvalTool<ProcessEvalExec>>,
	eval_prepared:       bool,
	dyn_installed:       bool,
}

impl<'a> EnvironmentHost<'a> {
	#[allow(
		clippy::too_many_arguments,
		reason = "environment tool composition carries independent typed authorities"
	)]
	pub(super) fn new(
		registry: &'a mut Registry,
		policy: ToolsPolicy,
		tool_settings: &'a ToolSettings,
		#[cfg(feature = "cap-browser")] browser_settings: &'a BrowserSettings,
		shell_settings: &'a ShellSettings,
		sandbox_settings: &'a SandboxSettings,
		con: &'a Ctx,
		documents: &'a DocumentHost,
		blobs: &'a BlobHost,
		exec: &'a ExecHost,
		workspace: &'a WorkspaceHost,
		state_dir: &'a Path,
		session_id: &'a str,
		root_uri: &'a Str,
		memory: &'a Arc<omp_memory::MemoryRuntime>,
		workers: &'a ExtHostSupervisor,
		interrupt_grace: omp_core::Duration,
		py_eval: bool,
		skills: Option<Arc<ManagedSkills>>,
		github_credentials: Arc<GithubCredentialBridge>,
		github_cache: Arc<GithubCache>,
		url_resolvers: Vec<Arc<dyn ContentResolver>>,
		host_resources: Option<Arc<dyn HostResources>>,
		session_authority: Option<Arc<dyn omp_contract::SessionAuthority>>,
		mcp: Arc<McpService>,
		edit_model: Option<Str>,
		edit_repair: Option<EditRepairClient>,
		workspace_trust: omp_ext::workspace_trust::WorkspaceTrust,
		previews: StagedProposalRegistry,
	) -> Self {
		Self {
			registry,
			policy,
			tool_settings,
			#[cfg(feature = "cap-browser")]
			browser_settings,
			shell_settings,
			sandbox_settings,
			con,
			documents,
			blobs,
			exec,
			workspace,
			state_dir,
			session_id,
			root_uri,
			memory,
			memory_capabilities: memory.capabilities(),
			workers,
			interrupt_grace,
			py_eval,
			skills,
			github_credentials,
			github_cache,
			url_resolvers,
			host_resources,
			session_authority,
			mcp,
			edit_model,
			edit_repair,
			workspace_trust,
			previews,
			reflection: Arc::new(ReflectionBridgeHost::new()),
			security: None,
			read_sources: None,
			read_blobs: None,
			conflicts: None,
			resolvers: None,
			selected_edit: None,
			read_policy: None,
			search_policy: None,
			search: None,
			eval_host: None,
			prelude: None,
			eval_control: EvalSessionControl::default(),
			eval_tool: None,
			py_tool: None,
			eval_prepared: false,
			dyn_installed: false,
		}
	}

	pub(super) fn ensure_resources(&mut self) -> Result<(), EnvdError> {
		let user_config_root = omp_core::dirs::user_config_root()?;
		#[cfg(feature = "cap-ssh")]
		let ssh = omp_ssh::SshService::new(
			omp_ssh::HostStore::load_layered(
				&omp_ssh::HostPaths::new(&user_config_root, self.workspace.root()),
				self.workspace_trust,
			)
			.map_err(state_error)?,
		);
		let vault = VaultService::load_layered(
			&VaultPaths::new(&user_config_root, self.workspace.root()),
			self.workspace_trust,
		)
		.map_err(state_error)?
		.with_obsidian_enabled(omp_tools::settings::SV_VAULT_ENABLED.get(self.con));
		self
			.documents
			.set_resource_mutations(ResourceMutationServices {
				#[cfg(feature = "cap-ssh")]
				ssh: ssh.clone(),
				vault: vault.clone(),
				local_sessions: self.state_dir.join("sessions"),
			});
		self.read_sources = Some(ReadSourceAdapter::new(
			self.documents.clone(),
			self.workspace.clone(),
			document_cache::project_document_cache(self.state_dir),
		));
		self.read_blobs = Some(
			SessionReadBlobs::open(self.blobs.clone(), self.session_id).map_err(EnvdError::State)?,
		);
		let conflicts = Arc::new(ConflictRegistry::default());
		self.conflicts = Some(Arc::clone(&conflicts));
		self.ensure_security_service();
		let security = self.security.clone().expect("security service");
		let github = GithubService::new(
			self.workspace.root().to_path_buf(),
			self.state_dir,
			Arc::clone(&self.github_credentials),
			Arc::clone(&self.github_cache),
			self.blobs.clone(),
		);
		let forge = crate::github_adapter::forge_router(github);
		self.resolvers = Some(production_url_resolvers(
			conflicts,
			self.blobs.store().clone(),
			self.session_id,
			self.state_dir.join("sessions"),
			self.workspace.root().to_path_buf(),
			Arc::clone(&self.github_cache),
			Arc::clone(&self.github_credentials),
			forge,
			mem::take(&mut self.url_resolvers),
			self.host_resources.clone(),
			self.session_authority.clone(),
			Arc::clone(&self.mcp),
			#[cfg(feature = "cap-ssh")]
			ssh,
			security,
			vault,
		));
		let environment_edit_dialect = env::var("OMP_EDIT_DIALECT").ok();
		let force_hashline = env::var_os("OMP_STRICT_EDIT_MODE").is_some();
		let model_edit_revision = configured_model_edit_revision(self.con)?;
		let selected_edit =
			omp_tools::edit::resolve_edit_revision(omp_tools::edit::EditRevisionCandidates {
				environment: environment_edit_dialect.as_deref(),
				model_rule: model_edit_revision.as_ref(),
				setting: self.tool_settings.edit_dialect.as_deref(),
				force_hashline,
				..omp_tools::edit::EditRevisionCandidates::default()
			})
			.map_err(EnvdError::EditDialect)?
			.revision;
		self.read_policy = Some(production_read_policy(self.tool_settings, &selected_edit));
		self.search_policy = Some(production_search_policy(self.tool_settings));
		self.selected_edit = Some(selected_edit);
		self.prelude = Some(Arc::new(build_prelude_table(self.workers)?));
		self.eval_host = Some(Arc::new(SessionBridgeHost::new()));
		Ok(())
	}

	pub(super) fn resolvers(&self) -> Arc<ResolverTable<UrlResolver>> {
		Arc::clone(self.resolvers.as_ref().expect("url resolvers"))
	}

	pub(super) fn previews(&self) -> StagedProposalRegistry {
		self.previews.clone()
	}

	pub(super) fn set_dyn_installed(&mut self, installed: bool) {
		self.dyn_installed = installed;
	}

	pub(super) fn finish(self) -> EnvironmentParts {
		EnvironmentParts {
			reflection:   self.reflection,
			eval_host:    self.eval_host.expect("eval host"),
			eval_control: self.eval_control,
			previews:     self.previews,
			resolvers:    self.resolvers.expect("url resolvers"),
			prelude:      self.prelude.expect("eval prelude"),
		}
	}

	fn register<T: Tool>(&mut self, tool: T, row: &BuiltinRosterEntry) -> Result<(), EnvdError> {
		let (presentation, claims) = omp_tools::roster::registration(row, self.policy);
		environment_registry(self.registry, tool, presentation, claims)?;
		Ok(())
	}

	fn workspace_search(&mut self) -> WorkspaceSearchAdapter {
		if self.search.is_none() {
			self.search = Some(WorkspaceSearchAdapter::new(
				self.workspace.clone(),
				self.documents.clone(),
				self.read_sources.clone().expect("read sources"),
				Arc::clone(self.resolvers.as_ref().expect("url resolvers")),
			));
		}
		self.search.clone().expect("workspace search")
	}

	fn ensure_eval(&mut self) -> Result<(), EnvdError> {
		if self.eval_prepared {
			return Ok(());
		}
		self.eval_prepared = true;
		let eval_host = Arc::clone(self.eval_host.as_ref().expect("eval host"));
		let interpreter = self
			.tool_settings
			.eval_interpreters
			.get("py")
			.map(|path| std::path::PathBuf::from(path.as_str()));
		let eval_exec = compose_eval_executor(
			ProcessEvalExec::production(
				self.exec.clone(),
				eval_host,
				self.interrupt_grace,
				self.blobs.clone(),
				interpreter,
			),
			self.py_eval,
		)?;
		let Some(eval_exec) = eval_exec else {
			return Ok(());
		};
		let prelude = self.prelude.as_ref().expect("eval prelude");
		let helper_docs = prelude
			.helpers()
			.map(|helper| omp_tools::eval::PreludeHelperDescription {
				signature: helper.signature.as_str(),
				summary:   helper.summary.as_str(),
			})
			.collect::<Vec<_>>();
		let mut task_snapshot =
			TaskDescriptionSnapshot { helpers: &helper_docs, ..TaskDescriptionSnapshot::standard() };
		let task = omp_tools::roster::entry(omp_tools::roster::Family::Task)
			.expect("task row")
			.name;
		if !self.tool_settings.enabled(task) {
			task_snapshot.agents = &[];
		}
		let (eval_tool, control) =
			omp_tools::eval::eval_controlled_with_task_snapshot(eval_exec.clone(), task_snapshot);
		self.eval_control = control;
		self.eval_tool = Some(eval_tool);
		if self.py_eval {
			self.py_tool = Some(omp_tools::eval::py_eval(eval_exec));
		}
		Ok(())
	}
}

impl ToolGate for EnvironmentHost<'_> {
	fn tool_enabled(&self, key: &str) -> bool {
		self.tool_settings.enabled(key)
	}

	fn policy(&self) -> ToolsPolicy {
		self.policy
	}
}

impl EnvironmentInstall for EnvironmentHost<'_> {
	type Error = EnvdError;

	fn ensure_security_service(&mut self) {
		if self.security.is_none() {
			self.security = Some(
				SecurityScanService::new(self.workspace.root().to_path_buf(), self.state_dir)
					.with_credentials(Arc::clone(&self.github_credentials)),
			);
		}
	}

	#[cfg(feature = "cap-browser")]
	fn browser_enabled(&self) -> bool {
		self.browser_settings.enabled
	}

	fn memory_capabilities(&self) -> Capabilities {
		self.memory_capabilities
	}

	fn managed_skills(&self) -> bool {
		self.skills.is_some()
	}

	fn py_eval_enabled(&self) -> bool {
		self.py_eval
	}

	fn shell_enabled(&self) -> bool {
		self.shell_settings.enabled
	}

	#[cfg(feature = "cap-browser")]
	fn install_browser(&mut self, row: &BuiltinRosterEntry) -> Result<(), Self::Error> {
		let daemon = BrowserDaemon::start(self.blobs.clone(), self.browser_settings.clone());
		self.register(omp_tools::browser::tool(daemon), row)
	}

	#[cfg(feature = "cap-computer")]
	fn install_computer(&mut self, row: &BuiltinRosterEntry) -> Result<(), Self::Error> {
		let computer = ComputerSessionHost::new(self.blobs.clone(), self.con);
		self.register(omp_tools::computer::tool(computer), row)
	}

	fn install_security_scan(&mut self, row: &BuiltinRosterEntry) -> Result<(), Self::Error> {
		let service = self.security.clone().expect("security service");
		self.register(omp_tools::security_scan::tool(service), row)
	}

	fn install_retain(&mut self, row: &BuiltinRosterEntry) -> Result<(), Self::Error> {
		self.register(omp_tools::memory::retain_tool(Arc::clone(self.memory)), row)
	}

	fn install_recall(&mut self, row: &BuiltinRosterEntry) -> Result<(), Self::Error> {
		self.register(omp_tools::memory::recall_tool(Arc::clone(self.memory)), row)
	}

	fn install_reflect(&mut self, row: &BuiltinRosterEntry) -> Result<(), Self::Error> {
		self.register(
			omp_tools::memory::reflect_tool(Arc::clone(self.memory), Arc::clone(&self.reflection)),
			row,
		)
	}

	fn install_memory_edit(&mut self, row: &BuiltinRosterEntry) -> Result<(), Self::Error> {
		self.register(omp_tools::memory_edit::tool(Arc::clone(self.memory)), row)
	}

	fn install_manage_skill(&mut self, row: &BuiltinRosterEntry) -> Result<(), Self::Error> {
		let Some(authority) = self.skills.clone() else {
			return Ok(());
		};
		self.register(omp_tools::manage_skill::tool(authority), row)
	}

	fn install_learn(&mut self, row: &BuiltinRosterEntry) -> Result<(), Self::Error> {
		let Some(authority) = self.skills.clone() else {
			return Ok(());
		};
		self.register(omp_tools::learn::tool(Arc::clone(self.memory), authority), row)
	}

	fn install_read(&mut self, row: &BuiltinRosterEntry) -> Result<(), Self::Error> {
		let read = omp_tools::read::tool_with_policy(
			self.read_sources.clone().expect("read sources"),
			self.read_blobs.clone().expect("read blobs"),
			Arc::clone(self.resolvers.as_ref().expect("url resolvers")),
			Arc::clone(self.conflicts.as_ref().expect("conflicts")),
			self.read_policy.expect("read policy"),
		);
		self.register(read, row)
	}

	fn install_edit(&mut self, row: &BuiltinRosterEntry) -> Result<(), Self::Error> {
		let selected_edit = self.selected_edit.clone().expect("selected edit");
		let edit_repair = self.tool_settings.edit_auto_repair.then(|| {
			self
				.edit_repair
				.clone()
				.unwrap_or_else(|| EditRepairClient::from_completion(invocation_edit_repair))
				.with_model_identity(invocation_edit_model)
		});
		let edit_observer = omp_tools::edit::observer::EditObserver::new(
			omp_tools::edit::observer::EditBlackboxConfig {
				path: self.tool_settings.edit_blackbox_path.as_ref().map(|path| {
					if path.is_absolute() {
						path.clone()
					} else {
						self.workspace.root().join(path)
					}
				}),
				model: self
					.edit_model
					.clone()
					.or_else(|| configured_model_identity(self.con))
					.unwrap_or_else(|| sf!("unknown")),
				..omp_tools::edit::observer::EditBlackboxConfig::default()
			},
			edit_repair,
		);
		let mut hashline_edit = Some(omp_tools::edit::tool_with_observer(
			self.documents.clone(),
			self.blobs.clone(),
			self.tool_settings.format_policy,
			edit_observer.clone(),
			self.tool_settings.edit_guard_generated,
		));
		let mut legacy_replace_edit = Some(
			omp_tools::edit::legacy_replace_tool_with_observer(
				self.documents.clone(),
				self.tool_settings.format_policy,
				edit_observer.clone(),
				self.tool_settings.edit_guard_generated,
				self.tool_settings.edit_fuzzy,
				self.tool_settings.edit_require_seen,
			)
			.with_fuzzy_threshold(self.tool_settings.edit_fuzzy_threshold),
		);
		let mut replace_edit = Some(
			omp_tools::edit::replace_tool_with_observer(
				self.documents.clone(),
				self.tool_settings.format_policy,
				edit_observer.clone(),
				self.tool_settings.edit_guard_generated,
				self.tool_settings.edit_fuzzy,
				self.tool_settings.edit_require_seen,
			)
			.with_fuzzy_threshold(self.tool_settings.edit_fuzzy_threshold),
		);
		let mut legacy_patch_edit = Some(omp_tools::edit::legacy_patch_tool_with_observer(
			self.documents.clone(),
			self.tool_settings.format_policy,
			edit_observer.clone(),
			self.tool_settings.edit_guard_generated,
			self.tool_settings.edit_require_seen,
		));
		let mut patch_edit = Some(omp_tools::edit::patch_tool_with_observer(
			self.documents.clone(),
			self.tool_settings.format_policy,
			edit_observer.clone(),
			self.tool_settings.edit_guard_generated,
			self.tool_settings.edit_require_seen,
		));
		let mut apply_patch_edit = Some(omp_tools::edit::apply_patch_tool_with_observer(
			self.documents.clone(),
			self.tool_settings.format_policy,
			edit_observer.clone(),
			self.tool_settings.edit_guard_generated,
			self.tool_settings.edit_require_seen,
		));
		let mut sloppy_edit = Some(omp_tools::edit::sloppy_tool_with_observer(
			self.documents.clone(),
			self.tool_settings.format_policy,
			edit_observer,
			self.tool_settings.edit_guard_generated,
			self.tool_settings.edit_require_seen,
		));
		let mut edits = [
			(
				legacy_replace_edit
					.as_ref()
					.expect("constructed")
					.spec()
					.identity(),
				0_u8,
			),
			(
				legacy_patch_edit
					.as_ref()
					.expect("constructed")
					.spec()
					.identity(),
				1,
			),
			(
				hashline_edit
					.as_ref()
					.expect("constructed")
					.spec()
					.identity(),
				2,
			),
			(
				replace_edit
					.as_ref()
					.expect("constructed")
					.spec()
					.identity(),
				3,
			),
			(patch_edit.as_ref().expect("constructed").spec().identity(), 4),
			(
				apply_patch_edit
					.as_ref()
					.expect("constructed")
					.spec()
					.identity(),
				5,
			),
			(sloppy_edit.as_ref().expect("constructed").spec().identity(), 6),
		];
		edits.sort_by_key(|(identity, _)| identity.rev == selected_edit);
		for (_, index) in edits {
			match index {
				0 => self.register(legacy_replace_edit.take().expect("once"), row)?,
				1 => self.register(legacy_patch_edit.take().expect("once"), row)?,
				2 => self.register(hashline_edit.take().expect("once"), row)?,
				3 => self.register(replace_edit.take().expect("once"), row)?,
				4 => self.register(patch_edit.take().expect("once"), row)?,
				5 => self.register(apply_patch_edit.take().expect("once"), row)?,
				6 => self.register(sloppy_edit.take().expect("once"), row)?,
				_ => unreachable!("edit dialect index"),
			}
		}
		Ok(())
	}

	fn install_write(&mut self, row: &BuiltinRosterEntry) -> Result<(), Self::Error> {
		let write = omp_tools::write::tool_with_policy_and_conflicts(
			self.documents.clone(),
			Arc::clone(self.conflicts.as_ref().expect("conflicts")),
			self.tool_settings.format_policy,
			self.tool_settings.edit_guard_generated,
		);
		self.register(write, row)
	}

	fn install_lsp(&mut self, row: &BuiltinRosterEntry) -> Result<(), Self::Error> {
		let maximum = self
			.tool_settings
			.max_timeout
			.and_then(|duration| duration.to_std().ok())
			.unwrap_or_else(|| time::Duration::from_secs(300));
		self.register(
			omp_tools::lsp::tool(
				DocumentLspControl::new(self.documents.clone(), self.exec.clone()),
				maximum,
			),
			row,
		)
	}

	fn install_debug(&mut self, row: &BuiltinRosterEntry) -> Result<(), Self::Error> {
		let maximum = self
			.tool_settings
			.max_timeout
			.and_then(|duration| duration.to_std().ok())
			.unwrap_or_else(|| time::Duration::from_secs(300));
		self.register(
			omp_tools::debug::tool(DocumentDebugControl::new(self.documents.clone()), maximum),
			row,
		)
	}

	fn install_grep(&mut self, row: &BuiltinRosterEntry) -> Result<(), Self::Error> {
		let policy = self.search_policy.expect("search policy");
		let grep = omp_tools::grep::tool(
			self.workspace_search(),
			u32::from(self.tool_settings.grep_context_before),
			u32::from(self.tool_settings.grep_context_after),
			policy,
		);
		self.register(grep, row)
	}

	fn install_glob(&mut self, row: &BuiltinRosterEntry) -> Result<(), Self::Error> {
		let policy = self.search_policy.expect("search policy");
		let search = self.workspace_search();
		self.register(omp_tools::glob::tool(search, policy), row)
	}

	fn install_ast_grep(&mut self, row: &BuiltinRosterEntry) -> Result<(), Self::Error> {
		let policy = self.search_policy.expect("search policy");
		let ast_search = AstSearchAuthority::new(
			self.workspace.clone(),
			self.read_sources.clone().expect("read sources"),
			Arc::clone(self.resolvers.as_ref().expect("url resolvers")),
			self.state_dir,
		);
		self.register(omp_tools::ast_grep::tool(ast_search, policy), row)
	}

	fn install_ast_edit(&mut self, row: &BuiltinRosterEntry) -> Result<(), Self::Error> {
		self.register(
			omp_tools::ast_edit::tool(self.workspace.root().to_path_buf(), self.previews.clone()),
			row,
		)
	}

	fn install_eval(&mut self, row: &BuiltinRosterEntry) -> Result<(), Self::Error> {
		self.ensure_eval()?;
		if let Some(tool) = self.eval_tool.take() {
			self.register(tool, row)?;
		}
		Ok(())
	}

	fn install_py_eval(&mut self, row: &BuiltinRosterEntry) -> Result<(), Self::Error> {
		self.ensure_eval()?;
		if let Some(tool) = self.py_tool.take() {
			self.register(tool, row)?;
		}
		Ok(())
	}

	fn install_bash(&mut self, row: &BuiltinRosterEntry) -> Result<(), Self::Error> {
		let bash_name = omp_tools::roster::entry(omp_tools::roster::Family::Bash)
			.expect("bash row")
			.name;
		let sibling_tools = self
			.registry
			.live_identities()
			.filter_map(|(name, _)| {
				(name.as_str() != bash_name
					&& self.registry.presentation(name).ok() == Some(omp_tool::Presentation::Slot))
				.then(|| name.clone())
			})
			.collect::<Arc<[_]>>();
		let snapshot = omp_tools::shell::ShellPromptSnapshot {
			sibling_tools,
			platform: Str::new(env::consts::OS),
			devices: self.dyn_installed,
			embedded_builtins: self.shell_settings.embedded_builtins,
			interceptor_enabled: self.shell_settings.interceptor.enabled,
			interceptor_rules: self
				.shell_settings
				.interceptor
				.patterns
				.iter()
				.map(|rule| omp_tools::shell_intercept::Rule {
					pattern: rule.pattern.clone(),
					tool:    rule.tool.clone(),
					message: rule.message.clone(),
				})
				.collect(),
			command_prefix: self.shell_settings.command_prefix.is_some(),
		};
		let shell = omp_tools::shell::shell_with_snapshot_and_timeout_bounds(
			ShellExecHost::new(
				self.exec.clone(),
				self.blobs.clone(),
				self.root_uri.clone(),
				Arc::clone(self.resolvers.as_ref().expect("url resolvers")),
				self.shell_settings.clone(),
				self.sandbox_settings,
			),
			shell_timeout_bounds(self.tool_settings),
			&snapshot,
		)
		.with_auto_background(
			self.shell_settings.auto_background.enabled,
			time::Duration::from_millis(self.shell_settings.auto_background.threshold_ms),
		);
		self.register(shell, row)
	}
}
