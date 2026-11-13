//! Terminal presentation adapter for the driver-owned workflow host.

use std::{
	path::{Path, PathBuf},
	sync::Arc,
};

use miette::IntoDiagnostic as _;
use omp_core::{Str, sf};
use omp_driver::{
	adw::{
		RunExit, RunStatus, definition,
		production::{ProductionAdwHost, ProductionError},
		remote::AdwExecPolicy,
		resume::{
			OpenedHost, ResumeError, ResumeOutcome, ResumeRequest, RunRef, resume as shared_resume,
		},
	},
	workspace_trust::evaluate_workspace,
};
use omp_ext::workspace_trust::{WorkspaceTrust, inventory::WORKFLOWS_DIR};
use tokio_util::sync::CancellationToken;

use crate::{cli::AdwCommand, process_ctx_with};

/// Runs one workflow verb.
pub async fn run(command: AdwCommand) -> miette::Result<()> {
	match command {
		AdwCommand::List { project } => list(&project),
		AdwCommand::Run { workflow, project, model, max_attempts, exec_policy } => {
			let data_dir = omp_core::dirs::data_dir(None).into_diagnostic()?;
			let (_, code) =
				execute(&workflow, &project, data_dir, model, max_attempts, exec_policy).await?;
			if code == 0 {
				Ok(())
			} else {
				Err(miette::miette!("workflow exited with status {code}"))
			}
		},
		AdwCommand::Resume { journal, project, exec_policy } => {
			let data_dir = omp_core::dirs::data_dir(None).into_diagnostic()?;
			resume(&journal, &project, data_dir, exec_policy).await
		},
	}
}

fn list(project: &Path) -> miette::Result<()> {
	let names = definition::available(project);
	if names.is_empty() {
		println!("No workflows declared. Add `{WORKFLOWS_DIR}/<name>.toml` under the project root.");
		return Ok(());
	}
	for name in names {
		match definition::load(project, name.as_str()) {
			Ok(loaded) => {
				let phases = loaded.workflow().phases();
				println!("{name} ({} phases)", phases.len());
				for phase in phases {
					println!("  - {} [{}]", phase.name.as_str(), phase.kind);
				}
			},
			// A broken definition is reported in place rather than aborting the
			// listing: the operator asked what exists, and one invalid file does
			// not make the others unknowable.
			Err(error) => println!("{name} (invalid: {error})"),
		}
	}
	Ok(())
}

async fn execute(
	workflow: &str,
	project: &Path,
	data_dir: PathBuf,
	model: Option<Str>,
	max_attempts: u32,
	exec_policy: Option<AdwExecPolicy>,
) -> miette::Result<(PathBuf, u8)> {
	let definition = definition::load(project, workflow).into_diagnostic()?;
	let explicit = model.is_some();
	let model = model.unwrap_or_else(|| Str::new_static("default"));
	let host = open_host(project, data_dir.clone(), model.clone(), explicit)?;
	let host = match exec_policy {
		Some(policy) => host.with_exec_policy(policy),
		None => host,
	};
	let (mut log, journal_path) =
		create_run_log(&definition, project, &data_dir, max_attempts, &model, explicit, &host)?;
	println!("Recording run to `{}`.", journal_path.display());
	let cancel = CancellationToken::new();
	let exit = {
		let run =
			omp_driver::adw::run_with_log(&definition, max_attempts, &host, &cancel, Some(&mut log));
		tokio::pin!(run);
		tokio::select! {
			exit = &mut run => exit,
			signal = tokio::signal::ctrl_c() => {
				signal.into_diagnostic()?;
				// The run settles as cancelled: the token stops the active
				// phase (a code phase's process tree, a child turn) and the
				// loop dispatches nothing further.
				cancel.cancel();
				run.await
			},
		}
	}
	.into_diagnostic()?;

	for record in &exit.records {
		println!(
			"{} attempt {}: {} — {}",
			record.phase,
			record.attempt,
			if record.status == omp_driver::adw::PhaseStatus::Accepted {
				"accepted"
			} else {
				"rejected"
			},
			record.decision
		);
		if record.status != omp_driver::adw::PhaseStatus::Accepted && !record.feedback.is_empty() {
			println!("{}", record.feedback);
		}
	}
	match exit.status {
		RunStatus::Accepted => println!("Workflow `{workflow}` accepted every phase."),
		RunStatus::Halted | RunStatus::Blocked => println!(
			"Workflow `{workflow}` {}{}.",
			format!("{:?}", exit.status),
			exit
				.reason
				.as_deref()
				.map_or_else(String::new, |reason| format!(": {reason}"))
		),
		RunStatus::Failed(failure) => println!("Workflow `{workflow}` failed: {failure:?}."),
		RunStatus::Cancelled => println!("Workflow `{workflow}` cancelled."),
	}
	Ok((journal_path, exit.code))
}

/// Creates the run's journal at `<data dir>/adw/runs/<run-id>.oms` (ADR 0051
/// S2) and commits its start record.
fn create_run_log(
	definition: &omp_driver::adw::definition::WorkflowDefinition,
	project: &Path,
	data_dir: &Path,
	max_attempts: u32,
	model: &Str,
	model_explicit: bool,
	host: &ProductionAdwHost,
) -> miette::Result<(omp_journal::runlog::RunLog, PathBuf)> {
	let run = omp_core::ulid::Ulid::generate().to_string();
	let path = data_dir.join("adw").join("runs").join(format!("{run}.oms"));
	if let Some(parent) = path.parent() {
		std::fs::create_dir_all(parent).into_diagnostic()?;
	}
	let root = std::fs::canonicalize(project).unwrap_or_else(|_| project.to_path_buf());
	let head = omp_vcs::detect(&root)
		.ok()
		.flatten()
		.and_then(|repo| repo.head_id().ok().flatten());
	let start = omp_journal::runlog::RunStart {
		run: Str::from(run),
		workflow: Str::from(definition.workflow().name().as_str()),
		workflow_digest: Str::from(definition.digest()),
		// `omp adw run` takes no run inputs yet, so there is nothing to digest.
		input_digest: Str::default(),
		max_attempts,
		model: model.clone(),
		model_explicit,
		// What the host actually runs under, not what the command line said.
		exec_policy: Str::new_static(<&'static str>::from(host.exec_policy())),
		exec_fallback_local: host.exec_fallback_local(),
		workspace_trusted: host.workspace_trusted(),
		workspace_root: Str::from(root.display().to_string()),
		workspace_head: head.map(Str::from),
		created: Str::from(jiff::Timestamp::now().to_string()),
	};
	let log = omp_journal::runlog::RunLog::create(&path, start)
		.map_err(|error| miette::miette!("could not create run log: {error}"))?;
	Ok((log, path))
}

/// The execution policy a run journal recorded, when it names one.
fn recorded_exec_policy(start: &omp_journal::runlog::RunStart) -> Option<AdwExecPolicy> {
	start.exec_policy.as_str().parse().ok()
}

async fn resume(
	journal_path: &Path,
	project: &Path,
	data_dir: PathBuf,
	exec_policy: Option<AdwExecPolicy>,
) -> miette::Result<()> {
	let ctx = Arc::new(process_ctx_with(project, omp_con::Ctx::builder())?);
	let request = ResumeRequest {
		run: RunRef::Journal(journal_path),
		project,
		require_trusted: false,
		approval_ceiling: None,
	};
	let outcome = resume_recorded(request, &data_dir, ctx, exec_policy, &CancellationToken::new())
		.await
		.into_diagnostic()?;
	match outcome {
		ResumeOutcome::Resumed { workflow, journal, exit } => {
			println!("Resumed workflow `{workflow}` from `{}`.", journal.display());
			exit_status(exit.code)
		},
		ResumeOutcome::Finished { workflow, journal, accepted } => {
			println!(
				"Workflow `{workflow}` already finished in `{}`; nothing to resume.",
				journal.display()
			);
			exit_status(if accepted { 0 } else { 1 })
		},
	}
}

fn exit_status(code: u8) -> miette::Result<()> {
	if code == 0 {
		Ok(())
	} else {
		Err(miette::miette!("resumed workflow exited with status {code}"))
	}
}

pub(crate) async fn resume_recorded(
	request: ResumeRequest<'_>,
	data_dir: &Path,
	ctx: Arc<omp_con::Ctx>,
	exec_policy: Option<AdwExecPolicy>,
	cancel: &CancellationToken,
) -> Result<ResumeOutcome<ProductionError>, ResumeError<ProductionError>> {
	let project = request.project;
	shared_resume(
		request,
		|start: &omp_journal::runlog::RunStart| {
			let host = ProductionAdwHost::open(
				project.to_path_buf(),
				data_dir.to_path_buf(),
				start.model.clone(),
				start.model_explicit,
				Arc::clone(&ctx),
				evaluate_workspace(data_dir, project),
			)?;
			let host = match exec_policy.or_else(|| recorded_exec_policy(start)) {
				Some(policy) => host.with_exec_policy(policy),
				None => host,
			};
			let trust = if host.workspace_trusted() {
				WorkspaceTrust::Trusted
			} else {
				WorkspaceTrust::Untrusted
			};
			Ok(OpenedHost { host, trust })
		},
		cancel,
	)
	.await
}

pub(crate) fn outcome_line(outcome: &ResumeOutcome<ProductionError>) -> Str {
	match outcome {
		ResumeOutcome::Resumed { workflow, journal, exit } => {
			sf!("Resumed workflow `{workflow}` from `{}`: {}.", journal.display(), exit_summary(exit))
		},
		ResumeOutcome::Finished { workflow, accepted, .. } => sf!(
			"Workflow `{workflow}` already finished ({}); nothing to resume.",
			if *accepted { "accepted" } else { "rejected" }
		),
	}
}

fn exit_summary(exit: &RunExit<ProductionError>) -> String {
	let state = match exit.status {
		RunStatus::Accepted => "accepted every phase",
		RunStatus::Halted => "halted",
		RunStatus::Blocked => "blocked",
		RunStatus::Failed(_) => "failed",
		RunStatus::Cancelled => "cancelled",
	};
	match exit.reason.as_deref() {
		Some(reason) => format!("{state} ({reason})"),
		None => state.to_owned(),
	}
}

pub(crate) fn recorded_run_ids() -> std::io::Result<Vec<String>> {
	let data_dir = omp_core::dirs::data_dir(None).map_err(std::io::Error::other)?;
	omp_envd::tool_url::recorded_run_ids(&omp_envd::tool_url::runs_dir(&data_dir))
}

/// Opens the workflow host under the operator's configuration: user
/// `config.cfg` and the project overlay decide the sandbox, network and
/// approval posture its phases run under and report, and the workspace's
/// trust decision, evaluated here once for the run (failing closed), decides
/// whether its code phases run at all.
pub fn open_host(
	project: &Path,
	data_dir: PathBuf,
	model: Str,
	model_override: bool,
) -> miette::Result<ProductionAdwHost> {
	let ctx = Arc::new(process_ctx_with(project, omp_con::Ctx::builder())?);
	let workspace_trust = evaluate_workspace(&data_dir, project);
	ProductionAdwHost::open(
		project.to_path_buf(),
		data_dir,
		model,
		model_override,
		ctx,
		workspace_trust,
	)
	.into_diagnostic()
}
