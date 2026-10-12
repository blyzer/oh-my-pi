//! Production posture resolution and phase dispatch.
//!
//! Agent and review phases run as restricted child kernels; code phases run
//! through the project Environment's exec authority, so the sandbox the
//! reported posture describes is the one that confines them. Both report a
//! [`PhaseResult`] the domain turns into a transition.
//!
//! A code phase runs a command the workspace's workflow declares, so it runs
//! only in a [`WorkspaceTrust::Trusted`] workspace: an untrusted one refuses
//! it ([`ProductionError::UntrustedWorkspace`]). Model phases' child kernels
//! run under the run's own decision.

use std::{
	fs, io,
	path::{Path, PathBuf},
	sync::Arc,
};

use omp_adw::{ApprovalScope, NetworkScope, Phase, PhaseKind, Posture, SelectedInput, WriteScope};
use omp_core::{EnvPath, Str, StrMut, sf};
use omp_env::{EnvClient, ExecEvent};
use omp_envd::{
	admission::{SandboxState, effective_approval_mode},
	exec_settings::{ExecSandboxMode, NetworkConfinement, network_confinement},
	tool_settings::{ApprovalMode, ToolSettings},
};
use omp_ext::workspace_trust::{TrustDecision, WorkspaceTrust};
use omp_proto::env::v1::{
	CloseSessionRequest, ExecOutcome, ExecRequest, OpenSessionRequest, OutputChannel, Script,
};
use tokio::sync::OnceCell;
use tokio_util::sync::CancellationToken;

use super::{AdwHost, PhaseResult, PhaseSpec};
use crate::{
	cfg::CfgFiles,
	cleanse::checkers::{BinaryResolver as _, FilesystemResolver},
	headless::{HeadlessError, KernelOptions, compose_kernel, kernel::LaunchExtensionPolicy},
	workspace_trust::WorkspaceTrustSource,
};

/// A phase that could not be executed.
#[derive(Debug, thiserror::Error)]
pub enum ProductionError {
	/// The project root could not be canonicalized, or a process failed.
	#[error("workflow host filesystem or process operation failed")]
	Io(#[from] io::Error),
	/// The project Environment that runs code phases could not start.
	#[error("workflow project environment could not start")]
	Environment(#[from] omp_envd::EnvdError),
	/// The Environment refused or lost a code phase's command.
	#[error("workflow project environment rejected a command")]
	Exec(#[from] omp_env::ClientError),
	/// A child kernel could not be composed.
	#[error("workflow child session composition failed")]
	Session(#[from] HeadlessError),
	/// A child turn failed.
	#[error("workflow child turn failed")]
	Turn(#[from] omp_agent::KernelError),
	/// A code phase named an executable `$PATH` does not hold.
	#[error("workflow phase executable `{binary}` was not found on PATH")]
	UnresolvedBinary {
		/// The unresolved executable name.
		binary: Str,
	},
	/// A code phase's working directory resolves outside the project, through
	/// a symbolic link the lexical definition check cannot see.
	#[error("workflow phase working directory `{cwd}` resolves outside the project")]
	CwdEscapes {
		/// The declared working directory.
		cwd: Str,
	},
	/// A path could not be expressed as an Environment location.
	#[error("workflow phase path is not a valid environment location")]
	Location(#[source] omp_core::LocationError),
	/// A code phase was dispatched in a workspace that is not trusted.
	#[error("workspace is not trusted; adw code phases are withheld")]
	UntrustedWorkspace,
}

/// Production owner for one workflow run.
pub struct ProductionAdwHost {
	root:            PathBuf,
	data_dir:        PathBuf,
	model:           Str,
	ctx:             Arc<omp_con::Ctx>,
	resolver:        FilesystemResolver,
	/// The workspace trust decision the run executes under: code phases run
	/// only when it trusts, and model phases' child kernels inherit it.
	workspace_trust: TrustDecision,
	/// The project Environment, started by the first code phase: a workflow
	/// of model phases alone never needs one of its own.
	environment:     OnceCell<omp_envd::ProjectEnvironment>,
}

impl ProductionAdwHost {
	/// Opens a host rooted at a canonical project directory, running under
	/// `workspace_trust`, the workspace's trust decision.
	///
	/// # Errors
	///
	/// Returns [`ProductionError`] when the project root cannot be resolved.
	pub fn open(
		root: PathBuf,
		data_dir: PathBuf,
		model: Str,
		ctx: Arc<omp_con::Ctx>,
		workspace_trust: TrustDecision,
	) -> Result<Self, ProductionError> {
		Ok(Self {
			root: fs::canonicalize(root)?,
			data_dir,
			model,
			ctx,
			resolver: FilesystemResolver,
			workspace_trust,
			environment: OnceCell::new(),
		})
	}

	/// The Environment client code phases run through, attaching the project
	/// Environment under this run's control context on first use.
	async fn environment(&self) -> Result<&EnvClient, ProductionError> {
		let environment = self
			.environment
			.get_or_try_init(|| async {
				let state_dir = omp_env::project_state::directory(&self.data_dir, &self.root)?;
				fs::create_dir_all(&state_dir)?;
				omp_envd::ProjectEnvironment::attach(&self.root, &state_dir, omp_envd::AttachOptions {
					py_eval:            false,
					approval_mode:      None,
					trusted_extensions: Vec::new(),
					contributed_values: Vec::new(),
					con:                Arc::clone(&self.ctx),
					bridges:            omp_envd::RegistryBridges {
						content: omp_envd::ActiveContentInputs {
							workspace_trust: self.workspace_trust.trust(),
							..omp_envd::ActiveContentInputs::default()
						},
						..omp_envd::RegistryBridges::default()
					},
					spawn_idle_timeout: None,
					spawn_policy:       CfgFiles::new(Some(&self.root))
						.ok()
						.and_then(|files| files.daemon_policy(self.workspace_trust.trust()).ok()),
				})
				.await
				.map_err(ProductionError::from)
			})
			.await?;
		Ok(environment.client())
	}

	async fn run_child(
		&self,
		prompt: Str,
		cancel: &CancellationToken,
	) -> Result<Str, ProductionError> {
		let (mut kernel, mut session, _) = compose_kernel(
			&self.data_dir,
			&self.root,
			self.model.as_str(),
			Arc::clone(&self.ctx),
			child_kernel_options(&self.workspace_trust),
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

	async fn run_command(
		&self,
		binary: &str,
		args: &[Str],
		cwd: Option<&str>,
		cancel: &CancellationToken,
	) -> Result<PhaseResult, ProductionError> {
		let directory = match cwd {
			Some(cwd) => {
				let directory = fs::canonicalize(self.root.join(cwd))?;
				if !directory.starts_with(&self.root) {
					return Err(ProductionError::CwdEscapes { cwd: Str::new(cwd) });
				}
				directory
			},
			None => self.root.clone(),
		};
		let resolved = self
			.resolver
			.resolve(&[binary])
			.ok_or_else(|| ProductionError::UnresolvedBinary { binary: Str::new(binary) })?;
		let client = self.environment().await?;
		let cwd =
			EnvPath::new(Str::new(directory.to_string_lossy())).map_err(ProductionError::Location)?;
		let opened = client
			.open_session(&cwd, OpenSessionRequest::default())
			.await?;
		let session = opened.session.clone();
		let finished =
			run_script(client, opened.session, command_script(&resolved, args), cancel).await;
		let _ = client
			.close_session(CloseSessionRequest { session, ..CloseSessionRequest::default() })
			.await;
		let Some(finished) = finished? else {
			return Ok(PhaseResult { accepted: false, feedback: sf!("cancelled") });
		};
		if finished.outcome == ExecOutcome::Exited && finished.code == Some(0) {
			return Ok(PhaseResult { accepted: true, feedback: Str::empty() });
		}
		// The rejection feedback is the next attempt's only record of what was
		// wrong, so it carries the diagnostics rather than just the status. A
		// sandbox denial has no exit code; its outcome says why.
		let mut feedback = StrMut::new(match finished.code {
			Some(code) => format!("`{binary}` exited with status {code}\n"),
			None => format!("`{binary}` ended {}\n", finished.outcome.as_str_name()),
		});
		feedback.push_str(&String::from_utf8_lossy(&finished.stdout));
		feedback.push_str(&String::from_utf8_lossy(&finished.stderr));
		Ok(PhaseResult { accepted: false, feedback: feedback.freeze() })
	}
}

/// Options for one model phase's child kernel, which inherits the run's
/// workspace trust decision.
///
/// Workspace-owned native extensions (`.omp/extensions`) are excluded: the
/// workflow is project input already, and a project must not also choose
/// the code its phases' kernels load.
fn child_kernel_options(workspace_trust: &TrustDecision) -> KernelOptions {
	KernelOptions {
		ephemeral: true,
		extensions: LaunchExtensionPolicy {
			include_workspace: false,
			..LaunchExtensionPolicy::default()
		},
		workspace_trust: WorkspaceTrustSource::Fixed(workspace_trust.clone()),
		..KernelOptions::default()
	}
}

/// The in-process shell script that runs `binary` with `args`, each word
/// single-quoted so no argument is reinterpreted.
fn command_script(binary: &Path, args: &[Str]) -> String {
	let mut script = String::new();
	push_quoted(&mut script, &binary.to_string_lossy());
	for argument in args {
		script.push(' ');
		push_quoted(&mut script, argument.as_str());
	}
	script
}

fn push_quoted(script: &mut String, word: &str) {
	script.push('\'');
	for (index, part) in word.split('\'').enumerate() {
		if index > 0 {
			script.push_str("'\\''");
		}
		script.push_str(part);
	}
	script.push('\'');
}

/// What one code phase's command produced.
struct Finished {
	outcome: ExecOutcome,
	code:    Option<i32>,
	stdout:  Vec<u8>,
	stderr:  Vec<u8>,
}

/// Runs `script` in `session` to its exit; `None` when `cancel` fired first.
///
/// Dropping the run on cancellation tears its process groups down
/// TERM-then-KILL in the Environment.
async fn run_script(
	client: &EnvClient,
	session: bytes::Bytes,
	script: String,
	cancel: &CancellationToken,
) -> Result<Option<Finished>, ProductionError> {
	let mut run = client
		.exec(ExecRequest {
			session,
			source: Some(Script { text: script, ..Script::default() }),
			..ExecRequest::default()
		})
		.await?;
	let mut stdout = Vec::new();
	let mut stderr = Vec::new();
	loop {
		let event = tokio::select! {
			event = run.next_event() => event?,
			() = cancel.cancelled() => return Ok(None),
		};
		match event {
			Some(ExecEvent::Output(frame)) => {
				if frame.channel == OutputChannel::Stderr as i32 {
					stderr.extend_from_slice(&frame.data);
				} else {
					stdout.extend_from_slice(&frame.data);
				}
			},
			Some(ExecEvent::Started(_)) => {},
			Some(ExecEvent::Exit(exit)) => {
				let status = exit.status.unwrap_or_default();
				let outcome = ExecOutcome::try_from(status.outcome).unwrap_or(ExecOutcome::Unspecified);
				let code = matches!(outcome, ExecOutcome::Exited | ExecOutcome::Failed)
					.then_some(status.exit_code)
					.flatten();
				return Ok(Some(Finished { outcome, code, stdout, stderr }));
			},
			None => return Ok(None),
		}
	}
}

impl AdwHost for ProductionAdwHost {
	type Error = ProductionError;

	fn project_root(&self) -> &Path {
		&self.root
	}

	fn posture(&self) -> Result<Posture, Self::Error> {
		let sandbox = SandboxState::probe(&self.ctx, &self.root);
		let mode = omp_envd::exec_settings::SV_SANDBOX_MODE.get(&self.ctx);
		// A requested sandbox that was not constructed confines nothing, so the
		// posture is the host's, not the convar's.
		let unconfined = mode != ExecSandboxMode::Off && !sandbox.confines();
		Ok(Posture {
			write:    match mode {
				_ if unconfined => WriteScope::Unconfined,
				ExecSandboxMode::ReadOnly => WriteScope::ReadOnly,
				ExecSandboxMode::WorkspaceWrite => WriteScope::WorkspaceWrite,
				ExecSandboxMode::Off => WriteScope::Unconfined,
			},
			// The confinement shell sessions are configured for: a defaulted
			// network mode does not sandbox an explicit `off`, and an unconstructed
			// sandbox confines nothing. A session whose broker could not start, and
			// eval cells and detached processes, run `disabled`.
			network:  match network_confinement(&self.ctx, sandbox) {
				NetworkConfinement::Unconfined => NetworkScope::Unrestricted,
				NetworkConfinement::Disabled => NetworkScope::Disabled,
				NetworkConfinement::Scoped => NetworkScope::Scoped,
			},
			// The session's posture, reported as the loosest scope: a defaulted
			// `yolo` kept by an active sandbox covers only the tools that sandbox
			// confines (`bash`, `hub`); every other tool follows `write` per call
			// (`omp_envd::admission::resolve_approval`).
			approval: match effective_approval_mode(
				ToolSettings::from_con(&self.ctx)
					.with_workspace_trust(self.workspace_trust.trust())
					.configured_approval(),
				sandbox,
			) {
				ApprovalMode::AlwaysAsk => ApprovalScope::AlwaysAsk,
				ApprovalMode::Write => ApprovalScope::Write,
				ApprovalMode::Yolo => ApprovalScope::Yolo,
			},
		})
	}

	async fn dispatch(
		&self,
		phase: &Phase,
		spec: &PhaseSpec,
		attempt: u32,
		correction: Option<&Str>,
		inputs: &[SelectedInput],
		cancel: &CancellationToken,
	) -> Result<PhaseResult, Self::Error> {
		match spec {
			PhaseSpec::Command { binary, args, cwd } => {
				if self.workspace_trust.trust() != WorkspaceTrust::Trusted {
					return Err(ProductionError::UntrustedWorkspace);
				}
				self
					.run_command(binary.as_str(), args, cwd.as_deref(), cancel)
					.await
			},
			PhaseSpec::Prompt { text } => {
				let prompt = phase_prompt(phase, text, attempt, correction, inputs);
				let answer = self.run_child(prompt, cancel).await?;
				Ok(judge(phase.kind, answer))
			},
		}
	}
}

/// Composes the brief one model phase receives.
///
/// The attempt ordinal, the rejection that caused the retry, and the producer
/// versions consumed are all facts the child cannot otherwise observe: the
/// engine does not require a retry to reuse the rejected attempt's session.
fn phase_prompt(
	phase: &Phase,
	instruction: &Str,
	attempt: u32,
	correction: Option<&Str>,
	inputs: &[SelectedInput],
) -> Str {
	let mut text = StrMut::new(format!("Workflow phase `{}`.\n", phase.name.as_str()));
	if attempt > 1 {
		text.push_str(&format!("This is attempt {attempt}; a prior attempt was rejected.\n"));
	}
	if !inputs.is_empty() {
		text.push_str("Consuming: ");
		for (index, input) in inputs.iter().enumerate() {
			if index > 0 {
				text.push_str(", ");
			}
			text.push_str(&format!("`{}` v{}", input.phase.as_str(), input.version));
		}
		text.push('\n');
	}
	text.push('\n');
	text.push_str(instruction.as_str());
	if let Some(correction) = correction {
		text.push_str("\n\nCorrect this rejection:\n");
		text.push_str(correction.as_str());
	}
	if phase.kind == PhaseKind::Review {
		text.push_str(
			"\n\nAnswer with ACCEPT on its own first line when the work meets the bar, or REJECT \
			 followed by exactly what must change.",
		);
	}
	text.freeze()
}

/// Reads a model phase's verdict.
///
/// A review phase judges, so its answer is the verdict and an unparseable
/// answer is a rejection — a reviewer that did not say ACCEPT has not
/// accepted. A producing phase reports completion, so any answer stands.
fn judge(kind: PhaseKind, answer: Str) -> PhaseResult {
	if kind != PhaseKind::Review {
		return PhaseResult { accepted: true, feedback: Str::empty() };
	}
	if answer
		.as_str()
		.split_whitespace()
		.next()
		.is_some_and(|word| word.eq_ignore_ascii_case("accept"))
	{
		PhaseResult { accepted: true, feedback: Str::empty() }
	} else {
		PhaseResult { accepted: false, feedback: answer }
	}
}

#[cfg(test)]
mod tests {
	use std::{env, iter, os::unix::fs::PermissionsExt as _};

	use omp_adw::{PhaseName, PostureError};
	use omp_envd::exec_settings::{SV_SANDBOX_MODE, SV_SANDBOX_NETWORK_MODE, SandboxNetworkMode};
	use omp_ext::trust::GrantDuration;

	use super::{super::definition, *};

	const TRUSTED: TrustDecision = TrustDecision::Trusted { duration: GrantDuration::Persistent };

	fn host(project: &Path, ctx: omp_con::Ctx) -> ProductionAdwHost {
		host_under(project, ctx, TRUSTED)
	}

	fn host_under(project: &Path, ctx: omp_con::Ctx, trust: TrustDecision) -> ProductionAdwHost {
		ProductionAdwHost::open(
			project.to_path_buf(),
			project.join("data"),
			sf!("unused-model"),
			Arc::new(ctx),
			trust,
		)
		.expect("workflow host")
	}

	/// A user workflow phase that declares `requires.network = "disabled"`.
	fn offline_phase(project: &Path) -> Arc<Phase> {
		let directory = project.join(".omp/workflows");
		fs::create_dir_all(&directory).expect("workflow directory");
		fs::write(
			directory.join("offline.toml"),
			r#"
[[phase]]
name = "offline"
kind = "code"
command = ["true"]
requires = { network = "disabled" }
"#,
		)
		.expect("workflow file");
		let definition = definition::load(project, "offline").expect("definition");
		let phase = Arc::clone(&definition.workflow().phases()[0]);
		assert_eq!(phase.requires.network, Some(NetworkScope::Disabled));
		phase
	}

	#[test]
	fn posture_reports_the_network_commands_actually_get_when_the_sandbox_is_off() {
		let project = tempfile::tempdir().expect("project");
		// The probe never compiles mode `off`, so these hold on every host.
		let off = omp_con::Ctx::new();
		SV_SANDBOX_MODE
			.set(&off, ExecSandboxMode::Off)
			.expect("sandbox off");
		let posture = host(project.path(), off).posture().expect("posture");
		assert_eq!(posture.write, WriteScope::Unconfined);
		assert_eq!(
			posture.network,
			NetworkScope::Unrestricted,
			"the shipped scoped network does not sandbox an explicit off"
		);

		let off_disabled = omp_con::Ctx::new();
		SV_SANDBOX_MODE
			.set(&off_disabled, ExecSandboxMode::Off)
			.expect("sandbox off");
		SV_SANDBOX_NETWORK_MODE
			.set(&off_disabled, SandboxNetworkMode::Disabled)
			.expect("network disabled");
		assert_eq!(
			host(project.path(), off_disabled)
				.posture()
				.expect("posture")
				.network,
			NetworkScope::Unrestricted,
			"nothing enforces a disabled network while the sandbox is off"
		);

		let off_scoped = omp_con::Ctx::new();
		SV_SANDBOX_MODE
			.set(&off_scoped, ExecSandboxMode::Off)
			.expect("sandbox off");
		SV_SANDBOX_NETWORK_MODE
			.set(&off_scoped, SandboxNetworkMode::Scoped)
			.expect("network scoped");
		assert_eq!(
			host(project.path(), off_scoped)
				.posture()
				.expect("posture")
				.network,
			NetworkScope::Scoped,
			"an explicit scoped network keeps its network-only sandbox"
		);
	}

	#[test]
	fn a_phase_requiring_no_network_is_refused_under_the_default_posture() {
		let project = tempfile::tempdir().expect("project");
		let phase = offline_phase(project.path());

		// Scoped where the default sandbox is constructed, unrestricted where it
		// is not: looser than `disabled` on every host.
		let posture = host(project.path(), omp_con::Ctx::new())
			.posture()
			.expect("posture");
		assert_ne!(posture.network, NetworkScope::Disabled);
		assert!(matches!(
			phase.requires.check(&phase.name, &posture),
			Err(PostureError::Network { required: NetworkScope::Disabled, .. })
		));

		let off = omp_con::Ctx::new();
		SV_SANDBOX_MODE
			.set(&off, ExecSandboxMode::Off)
			.expect("sandbox off");
		let posture = host(project.path(), off).posture().expect("posture");
		assert!(matches!(
			phase.requires.check(&phase.name, &posture),
			Err(PostureError::Network {
				required: NetworkScope::Disabled,
				resolved: NetworkScope::Unrestricted,
				..
			})
		));
	}

	#[test]
	fn a_reviewer_that_did_not_say_accept_has_not_accepted() {
		assert!(judge(PhaseKind::Review, sf!("ACCEPT")).accepted);
		assert!(judge(PhaseKind::Review, sf!("  accept\nlooks good")).accepted);
		let rejected = judge(PhaseKind::Review, sf!("REJECT the tests are missing"));
		assert!(!rejected.accepted);
		assert_eq!(rejected.feedback, sf!("REJECT the tests are missing"));
		// An empty or off-script answer is not an acceptance.
		assert!(!judge(PhaseKind::Review, Str::empty()).accepted);
		assert!(!judge(PhaseKind::Review, sf!("acceptable with changes")).accepted);
		// A producing phase reports completion rather than a verdict.
		assert!(judge(PhaseKind::Agent, sf!("done")).accepted);
	}

	#[test]
	fn a_retry_brief_carries_the_rejection_and_consumed_versions() {
		let phase = Phase::new(PhaseName::new("review"), PhaseKind::Review);
		let inputs = [SelectedInput { phase: PhaseName::new("build"), version: 2 }];
		let prompt = phase_prompt(&phase, &sf!("Judge it."), 3, Some(&sf!("tests missing")), &inputs);
		let text = prompt.as_str();
		assert!(text.contains("attempt 3"));
		assert!(text.contains("`build` v2"));
		assert!(text.contains("Judge it."));
		assert!(text.contains("tests missing"));
		assert!(text.contains("ACCEPT"));

		let first = phase_prompt(&phase, &sf!("Judge it."), 1, None, &[]);
		assert!(!first.as_str().contains("attempt"));
		assert!(!first.as_str().contains("Consuming"));
	}

	/// Loads the single code phase of a workflow declaring `command`.
	fn code_phase(project: &Path, command: &[&str]) -> (Arc<Phase>, PhaseSpec) {
		let directory = project.join(".omp/workflows");
		fs::create_dir_all(&directory).expect("workflow directory");
		let argv = command
			.iter()
			.map(|word| format!("{word:?}"))
			.collect::<Vec<_>>()
			.join(", ");
		fs::write(
			directory.join("code.toml"),
			format!("[[phase]]\nname = \"code\"\nkind = \"code\"\ncommand = [{argv}]\n"),
		)
		.expect("workflow file");
		let definition = definition::load(project, "code").expect("definition");
		let phase = Arc::clone(&definition.workflow().phases()[0]);
		let spec = definition.spec(&phase.name).expect("spec").clone();
		(phase, spec)
	}

	fn script(path: &Path, body: &str) {
		fs::write(path, format!("#!/bin/sh\n{body}\n")).expect("script");
		fs::set_permissions(path, fs::Permissions::from_mode(0o755)).expect("mode");
	}

	async fn dispatch(host: &ProductionAdwHost, phase: &Phase, spec: &PhaseSpec) -> PhaseResult {
		host
			.dispatch(phase, spec, 1, None, &[], &CancellationToken::new())
			.await
			.expect("dispatch")
	}

	/// A repository that commits an executable named `cargo` at its root (or
	/// in a package-manager bin) cannot shadow the operator's `cargo`.
	#[tokio::test]
	async fn a_repo_root_executable_does_not_shadow_path() {
		let project = tempfile::tempdir().expect("project");
		let project_root = project.path().canonicalize().expect("canonical project");
		let tools = tempfile::tempdir().expect("tools");
		script(&project_root.join("cargo"), "echo repo > shadowed");
		fs::create_dir_all(project_root.join("node_modules/.bin")).expect("bin");
		script(&project_root.join("node_modules/.bin/cargo"), "echo repo > shadowed");
		script(&tools.path().join("cargo"), "echo \"$1\" > path-cargo");
		let path = env::join_paths(
			iter::once(tools.path().to_path_buf())
				.chain(env::split_paths(&env::var_os("PATH").unwrap_or_default())),
		)
		.expect("PATH");
		// SAFETY: nextest runs each test in its own process; nothing else reads
		// the variable concurrently.
		unsafe { env::set_var("PATH", path) };
		let off = omp_con::Ctx::new();
		SV_SANDBOX_MODE
			.set(&off, ExecSandboxMode::Off)
			.expect("sandbox off");
		let host = host(&project_root, off);
		let (phase, spec) = code_phase(&project_root, &["cargo", "it's"]);

		let result = dispatch(&host, &phase, &spec).await;
		assert!(result.accepted, "{}", result.feedback);
		assert!(!project_root.join("shadowed").exists(), "the repository's cargo ran");
		assert_eq!(
			fs::read_to_string(project_root.join("path-cargo")).expect("PATH cargo ran"),
			"it's\n",
			"arguments reach the program verbatim"
		);
	}

	/// A phase that declares `write = "workspace-write"` runs under the
	/// sandbox that posture reports: a write outside the workspace is
	/// refused, one inside it lands.
	#[tokio::test]
	async fn a_workspace_write_phase_cannot_write_outside_the_workspace() {
		let project = tempfile::tempdir().expect("project");
		let project_root = project.path().canonicalize().expect("canonical project");
		let outside = tempfile::tempdir().expect("outside");
		let escaped = outside
			.path()
			.canonicalize()
			.expect("outside")
			.join("escaped");
		let ctx = omp_con::Ctx::new();
		SV_SANDBOX_MODE
			.set(&ctx, ExecSandboxMode::WorkspaceWrite)
			.expect("workspace-write");
		// The scratch directory lives under the temporary directories the
		// default grant makes writable.
		ctx.exec(
			"sv_sandbox_exclude_tmpdir 1\nsv_sandbox_exclude_slash_tmp 1\n",
			omp_con::Source::Config(sf!("test")),
		)
		.expect("temporary directories excluded");
		if !SandboxState::probe(&ctx, &project_root).confines() {
			// No native sandbox on this host: posture reports `unconfined`, and
			// the domain refuses the phase before dispatch.
			return;
		}
		let host = host(&project_root, ctx);
		assert_eq!(host.posture().expect("posture").write, WriteScope::WorkspaceWrite);

		let directory = project_root.join(".omp/workflows");
		fs::create_dir_all(&directory).expect("workflow directory");
		fs::write(
			directory.join("contained.toml"),
			format!(
				"[[phase]]\nname = \"outside\"\nkind = \"code\"\ncommand = [\"touch\", \
				 {:?}]\nrequires = {{ write = \"workspace-write\" }}\n",
				escaped.to_string_lossy()
			),
		)
		.expect("workflow file");
		let definition = definition::load(&project_root, "contained").expect("definition");
		let phase = Arc::clone(&definition.workflow().phases()[0]);
		assert_eq!(phase.requires.write, Some(WriteScope::WorkspaceWrite));
		let spec = definition.spec(&phase.name).expect("spec");
		let refused = dispatch(&host, &phase, spec).await;
		assert!(!refused.accepted, "the write outside the workspace was accepted");
		assert!(!escaped.exists(), "the phase wrote outside the workspace");

		let (inside, spec) = code_phase(&project_root, &["touch", "inside"]);
		let landed = dispatch(&host, &inside, &spec).await;
		assert!(landed.accepted, "{}", landed.feedback);
		assert!(project_root.join("inside").exists());
	}

	/// A code phase runs a command the workspace declares: an untrusted
	/// workspace (any decision but `Trusted`) refuses it before resolving or
	/// running anything, and a trusted one runs the same phase.
	#[tokio::test]
	async fn an_untrusted_workspace_refuses_code_phases() {
		let project = tempfile::tempdir().expect("project");
		let project_root = project.path().canonicalize().expect("canonical project");
		let (phase, spec) = code_phase(&project_root, &["touch", "ran"]);
		let off = || {
			let ctx = omp_con::Ctx::new();
			SV_SANDBOX_MODE
				.set(&ctx, ExecSandboxMode::Off)
				.expect("sandbox off");
			ctx
		};

		for decision in [TrustDecision::Untrusted, TrustDecision::Denied] {
			let host = host_under(&project_root, off(), decision);
			let refused =
				Box::pin(host.dispatch(&phase, &spec, 1, None, &[], &CancellationToken::new()))
					.await
					.expect_err("an untrusted workspace ran a code phase");
			assert!(matches!(refused, ProductionError::UntrustedWorkspace), "{refused:?}");
			assert!(!project_root.join("ran").exists(), "the refused command ran");
		}

		let ran = Box::pin(dispatch(&host(&project_root, off()), &phase, &spec)).await;
		assert!(ran.accepted, "{}", ran.feedback);
		assert!(project_root.join("ran").exists());
	}

	#[test]
	fn model_phase_kernels_exclude_workspace_extensions_and_inherit_trust() {
		let options = child_kernel_options(&TrustDecision::Denied);
		assert!(!options.extensions.include_workspace);
		assert!(options.ephemeral);
		assert_eq!(options.workspace_trust, WorkspaceTrustSource::Fixed(TrustDecision::Denied));
	}
}
