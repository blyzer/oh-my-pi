//! Production checker execution and journal-first repair composition.

use std::{
	future::Future,
	path::{Path, PathBuf},
	pin::Pin,
	sync::Arc,
	time::Duration,
};

use omp_adw::{NetworkScope, Posture, Requirement, WriteScope};
use omp_core::{Str, sf};
use omp_dom::{NodeSpec, Op, Tag, Txn};
use omp_envd::{
	admission::{SandboxState, effective_approval_mode},
	exec_settings::{ExecSandboxMode, NetworkConfinement, SV_SANDBOX_MODE, network_confinement},
	tool_settings::{ApprovalMode, ToolSettings},
};
use omp_exec::{
	BackendId, Eligibility, EnvEntry, ExecutionBackend, ExecutionError, ExecutionEvent, ExecutionId,
	ExecutionPolicy, ExecutionRequest, ExecutionRun, FallbackPolicy, Intent, NeedSet, OutputSpec,
	Platform, PlatformRequirement, ResourceRequirements, ToolchainId, place,
};
use omp_exec_local::{Config as LocalConfig, LocalBackend};
use omp_session::{ComponentRegistry, Session};
use tokio::sync::OnceCell;
use tokio_util::sync::CancellationToken;

use super::{
	Assignment, BinaryResolver, Checker, CheckerEffect, CheckerRunner, CleanseHost,
	FilesystemResolver, ProcessOutput, RepairOutcome, Report, TargetChoice, assignment_prompt,
	discovery_schema, scan_project_files,
};
use crate::adw::remote::{AdwExecPolicy, SV_EXEC_POLICY};

/// Boxed presentation failure retained as a typed UI boundary.
pub type PresentationError = Box<dyn std::error::Error + Send + Sync + 'static>;

/// Presentation-owned target and free-form request collection.
pub trait CleansePresentation: Send + Sync + 'static {
	/// Selects one checker, all checkers, or a custom request.
	fn pick_target<'a>(
		&'a self,
		checkers: &'a [Checker],
		cancel: &'a CancellationToken,
	) -> Pin<Box<dyn Future<Output = Result<TargetChoice, PresentationError>> + 'a>>;
	/// Collects a custom request.
	fn prompt_request<'a>(
		&'a self,
		cancel: &'a CancellationToken,
	) -> Pin<Box<dyn Future<Output = Result<Option<Str>, PresentationError>> + 'a>>;
}

/// Production cleanse failure.
#[derive(Debug, thiserror::Error)]
pub enum ProductionError {
	/// Project path or process I/O failed.
	#[error("cleanse I/O failed")]
	Io(#[from] std::io::Error),
	/// Presentation failed.
	#[error("cleanse presentation failed")]
	Presentation(#[source] PresentationError),
	/// Kernel composition failed.
	#[error("cleanse child composition failed")]
	Headless(#[from] crate::headless::HeadlessError),
	/// Kernel turn failed.
	#[error("cleanse child turn failed")]
	Kernel(#[from] omp_agent::KernelError),
	/// Durable remainder projection failed.
	#[error("cleanse journal failed")]
	Session(#[from] omp_session::SessionError),
	/// JSON projection failed.
	#[error("cleanse JSON failed")]
	Json(#[from] serde_json::Error),
	/// The project Environment that backs the local checker execution could not
	/// start.
	#[error("cleanse project environment could not start")]
	Environment(#[from] omp_envd::EnvdError),
	/// The execution backend failed.
	#[error("cleanse execution backend failed")]
	Execution(#[source] ExecutionError),
}

/// Production owner for one standalone cleanse run.
pub struct ProductionCleanseHost {
	root:         PathBuf,
	files:        Vec<PathBuf>,
	data_dir:     PathBuf,
	ctx:          Arc<omp_con::Ctx>,
	resolver:     FilesystemResolver,
	journal:      parking_lot::Mutex<Session>,
	presentation: Arc<dyn CleansePresentation>,
	/// The project Environment, started by the first routed checker.
	environment:  OnceCell<omp_envd::ProjectEnvironment>,
}

impl ProductionCleanseHost {
	/// Opens checker discovery and a journal-first parent session.
	pub fn open(
		root: PathBuf,
		data_dir: PathBuf,
		ctx: Arc<omp_con::Ctx>,
		presentation: Arc<dyn CleansePresentation>,
	) -> Result<Self, ProductionError> {
		let root = std::fs::canonicalize(root)?;
		let files = scan_project_files(&root).map_err(|source| std::io::Error::other(source))?;
		let state = omp_env::project_state::directory(&data_dir, &root)?;
		let sessions = state.join("sessions");
		std::fs::create_dir_all(&sessions)?;
		let path = sessions.join(format!("cleanse-{}.oms", omp_core::Ulid::generate()));
		let journal = Session::create(path, ComponentRegistry::standard())?;
		Ok(Self {
			root,
			files,
			data_dir,
			ctx,
			resolver: FilesystemResolver,
			journal: parking_lot::Mutex::new(journal),
			presentation,
			environment: OnceCell::new(),
		})
	}

	async fn run_child(
		&self,
		model: &str,
		prompt: Str,
		cancel: &CancellationToken,
	) -> Result<Str, ProductionError> {
		let ctx = Arc::clone(&self.ctx);
		let (mut kernel, mut session, _) = crate::headless::compose_kernel(
			&self.data_dir,
			&self.root,
			model,
			ctx,
			crate::headless::KernelOptions { ephemeral: true, ..Default::default() },
		)
		.await?;
		let outcome = kernel
			.run_turn(
				&mut session,
				omp_agent::TurnInput { text: prompt, attachments: Vec::new() },
				omp_agent::RunControl::new(cancel.clone(), None),
			)
			.await?;
		Ok(outcome.assistant_text)
	}

	async fn environment_owner(&self) -> Result<&omp_envd::ProjectEnvironment, ProductionError> {
		self
			.environment
			.get_or_try_init(|| async {
				let state_dir = omp_env::project_state::directory(&self.data_dir, &self.root)?;
				std::fs::create_dir_all(&state_dir)?;
				omp_envd::ProjectEnvironment::attach(&self.root, &state_dir, omp_envd::AttachOptions {
					py_eval:            false,
					approval_mode:      None,
					trusted_extensions: Vec::new(),
					contributed_values: Vec::new(),
					con:                Arc::clone(&self.ctx),
					bridges:            omp_envd::RegistryBridges::default(),
					spawn_idle_timeout: None,
					spawn_policy:       None,
				})
				.await
				.map_err(ProductionError::from)
			})
			.await
	}

	fn posture(&self) -> Posture {
		let sandbox = SandboxState::probe(&self.ctx, &self.root);
		let mode = SV_SANDBOX_MODE.get(&self.ctx);
		let unconfined = mode != ExecSandboxMode::Off && !sandbox.confines();
		Posture {
			write:    match mode {
				_ if unconfined => WriteScope::Unconfined,
				ExecSandboxMode::ReadOnly => WriteScope::ReadOnly,
				ExecSandboxMode::WorkspaceWrite => WriteScope::WorkspaceWrite,
				ExecSandboxMode::Off => WriteScope::Unconfined,
			},
			network:  match network_confinement(&self.ctx, sandbox) {
				NetworkConfinement::Unconfined => NetworkScope::Unrestricted,
				NetworkConfinement::Disabled => NetworkScope::Disabled,
				NetworkConfinement::Scoped => NetworkScope::Scoped,
			},
			approval: match effective_approval_mode(
				ToolSettings::from_con(&self.ctx).configured_approval(),
				sandbox,
			) {
				ApprovalMode::AlwaysAsk => omp_adw::ApprovalScope::AlwaysAsk,
				ApprovalMode::Write => omp_adw::ApprovalScope::Write,
				ApprovalMode::Yolo => omp_adw::ApprovalScope::Yolo,
			},
		}
	}

	fn exec_policy(&self) -> ExecutionPolicy {
		let fallback = match SV_EXEC_POLICY.get(&self.ctx) {
			AdwExecPolicy::LocalOnly => FallbackPolicy::LocalOnly,
			AdwExecPolicy::LocalPreferred => FallbackPolicy::LocalPreferred,
			AdwExecPolicy::RemotePreferred => FallbackPolicy::RemotePreferred,
			AdwExecPolicy::RemoteOnly => FallbackPolicy::RemoteOnly,
		};
		ExecutionPolicy { fallback, cache: omp_exec::CachePolicy::Off }
	}
}

impl BinaryResolver for ProductionCleanseHost {
	fn resolve(&self, names: &[&str]) -> Option<PathBuf> {
		self.resolver.resolve(names)
	}
}

fn local_platform() -> Platform {
	#[cfg(target_arch = "aarch64")]
	let arch = omp_exec::Arch::Aarch64;
	#[cfg(not(target_arch = "aarch64"))]
	let arch = omp_exec::Arch::X86_64;
	#[cfg(target_os = "macos")]
	let os = omp_exec::Os::Macos;
	#[cfg(not(target_os = "macos"))]
	let os = omp_exec::Os::Linux;
	Platform { os, arch }
}

fn checker_eligibility(checker: &Checker) -> Eligibility {
	match checker.effect {
		// A mutating checker (e.g. `cargo fix`, formatters) writes the workspace
		// and cannot run remotely: a remote action cannot apply those writes back.
		CheckerEffect::Mutating => Eligibility::LocalRequired,
		// A read-only checker is a candidate for remote offload.
		CheckerEffect::ReadOnly => Eligibility::RemotePreferred,
	}
}

fn checker_intent(checker: &Checker) -> Intent {
	if checker.test {
		Intent::Test
	} else {
		Intent::Check
	}
}

impl CheckerRunner for ProductionCleanseHost {
	type Error = ProductionError;

	fn run_checker(
		&self,
		checker: &Checker,
		cancel: &CancellationToken,
		partials: Option<flume::Sender<ProcessOutput>>,
	) -> impl Future<Output = Result<ProcessOutput, Self::Error>> + Send {
		let checker = checker.clone();
		let cancel = cancel.clone();
		let policy = self.exec_policy();
		async move {
			let environment = self.environment_owner().await?;
			let platform = local_platform();
			let backend = LocalBackend::new(LocalConfig {
				id: BackendId::new_static("local"),
				root: self.root.clone(),
				host: environment.exec_host(),
				platform,
				enforced: self.posture(),
			});

			// Build argv: the resolved binary as argv[0] plus all args.
			let mut argv: Vec<Str> = Vec::with_capacity(checker.args.len() + 1);
			argv.push(Str::new(checker.binary.to_string_lossy()));
			argv.extend(checker.args.iter().cloned());

			// cwd: make relative to root for the request, then LocalBackend
			// resolves it back to absolute.
			let cwd = checker
				.cwd
				.strip_prefix(&self.root)
				.map(|rel| Str::new(rel.to_string_lossy()))
				.unwrap_or_else(|_| Str::empty());

			let eligibility = checker_eligibility(&checker);
			let request = ExecutionRequest {
				id: ExecutionId::new(Str::from(omp_core::Ulid::generate().to_string())),
				intent: checker_intent(&checker),
				argv: Arc::from(argv),
				cwd,
				env: Arc::<[EnvEntry]>::from([]),
				inputs: None,
				outputs: OutputSpec::Logs,
				toolchain: ToolchainId::new_static("local"),
				caches: Arc::from([]),
				platform: PlatformRequirement {
					os:            platform.os,
					arch:          platform.arch,
					needs_network: false,
					needs:         NeedSet::new(),
				},
				requirement: Requirement::default(),
				resources: ResourceRequirements::default(),
				eligibility,
				timeout: Duration::from_secs(30 * 60),
			};

			// place() decides local vs remote; with no remote backend offered
			// here, remote-eligible requests go local under local-only/local-preferred,
			// and are unplaceable under remote-only with no backend configured.
			// For Slice 2, the local backend handles every admitted request.
			let _placement = place(&request, &[], &policy)
				.map_err(|e| ProductionError::Execution(ExecutionError::Unplaceable { need: e }))?;

			let mut run = match backend.start(request).await {
				Ok(run) => run,
				Err(error) if matches!(&error, ExecutionError::Infrastructure { .. }) => {
					return Ok(ProcessOutput {
						exit_code: Some(1),
						stdout:    Str::empty(),
						stderr:    sf!("command refused by the workspace sandbox"),
					});
				},
				Err(error) => return Err(ProductionError::Execution(error)),
			};

			let mut stdout = Vec::new();
			let mut stderr = Vec::new();
			loop {
				let event = tokio::select! {
					event = run.next_event() => event,
					() = cancel.cancelled() => {
						let _ = run.cancel().await;
						return Ok(ProcessOutput { exit_code: None, stdout: sf!(""), stderr: sf!("cancelled") });
					},
				};
				match event {
					Some(ExecutionEvent::Output(frame)) => match frame.stream {
						omp_exec::Stream::Stdout | omp_exec::Stream::Combined => {
							stdout.extend_from_slice(&frame.bytes);
						},
						omp_exec::Stream::Stderr => {
							stderr.extend_from_slice(&frame.bytes);
						},
					},
					Some(ExecutionEvent::Settled(Ok(result))) => {
						let output = ProcessOutput {
							exit_code: match result.outcome {
								omp_exec::Outcome::Exited { code } => Some(code),
								omp_exec::Outcome::Signalled { .. } => None,
							},
							stdout:    Str::new(String::from_utf8_lossy(&stdout)),
							stderr:    Str::new(String::from_utf8_lossy(&stderr)),
						};
						if let Some(tx) = partials {
							let _ = tx.send(output.clone());
						}
						return Ok(output);
					},
					Some(ExecutionEvent::Settled(Err(error))) => {
						return Err(ProductionError::Execution(error));
					},
					Some(_) => {},
					None => {
						return Err(ProductionError::Execution(ExecutionError::Infrastructure {
							source: Arc::new(std::io::Error::other(
								"local execution ended without a result",
							)),
						}));
					},
				}
			}
		}
	}
}

impl CleanseHost for ProductionCleanseHost {
	fn project_root(&self) -> &Path {
		&self.root
	}

	fn project_files(&self) -> &[PathBuf] {
		&self.files
	}

	fn pick_target(
		&self,
		checkers: &[Checker],
		cancel: &CancellationToken,
	) -> impl Future<Output = Result<TargetChoice, Self::Error>> {
		let presentation = Arc::clone(&self.presentation);
		let checkers = checkers.to_vec();
		let cancel = cancel.clone();
		async move {
			presentation
				.pick_target(&checkers, &cancel)
				.await
				.map_err(ProductionError::Presentation)
		}
	}

	fn prompt_request(
		&self,
		cancel: &CancellationToken,
	) -> impl Future<Output = Result<Option<Str>, Self::Error>> {
		let presentation = Arc::clone(&self.presentation);
		let cancel = cancel.clone();
		async move {
			presentation
				.prompt_request(&cancel)
				.await
				.map_err(ProductionError::Presentation)
		}
	}

	fn discover_custom(
		&self,
		request: &str,
		model: &str,
		cancel: &CancellationToken,
	) -> impl Future<Output = Result<Str, Self::Error>> + Send {
		let request = Str::new(request);
		let model = Str::new(model);
		async move {
			let prompt = Str::new(format!(
				"Discover exact checker argv for this project request: {request}. Return only JSON \
				 matching this schema: {}",
				discovery_schema()
			));
			self.run_child(model.as_str(), prompt, cancel).await
		}
	}

	fn repair_worker(
		&self,
		assignment: Assignment,
		worker: usize,
		peers: Vec<Assignment>,
		model: &str,
		followups: flume::Receiver<Vec<super::Diagnostic>>,
		cancel: &CancellationToken,
	) -> impl Future<Output = Result<RepairOutcome, Self::Error>> + Send {
		let model = Str::new(model);
		async move {
			let mut prompt = assignment_prompt(&assignment, worker, &peers).to_string();
			for batch in followups.try_iter() {
				prompt.push_str("\n");
				prompt.push_str(&format!("Additional diagnostics: {batch:?}"));
			}
			let output = self
				.run_child(model.as_str(), Str::new(prompt), cancel)
				.await?;
			Ok(RepairOutcome {
				name: Str::new(format!("CleanseA{worker}")),
				success: !output.is_empty(),
				output,
			})
		}
	}

	fn journal_remainder(&self, report: &Report) -> Result<(), Self::Error> {
		let mut session = self.journal.lock();
		let cause = session
			.head()
			.ok_or(omp_session::SessionError::NoActiveTurn)?;
		let data = serde_json::value::to_raw_value(&format!("{report:?}"))?;
		let parent = session.dom().meta();
		let after = session.dom().children(parent).last().copied();
		session.patch(Txn {
			cause,
			label: Some(Str::new_static("cleanse.remainder")),
			ops: vec![Op::Ins {
				parent,
				after,
				node: NodeSpec::new(Tag::Custom(Str::new_static("cleanse-remainder"))).with_prop(
					omp_dom::PropKey::Custom(Str::new_static("data")),
					omp_dom::Value::Json(data),
				),
			}],
		})?;
		Ok(())
	}
}
