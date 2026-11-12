//! Terminal presentation adapter for the driver-owned workflow host.

use std::{
	path::{Path, PathBuf},
	sync::Arc,
};

use miette::IntoDiagnostic as _;
use omp_core::Str;
use omp_driver::{
	adw::{RunStatus, definition, production::ProductionAdwHost, remote::AdwExecPolicy},
	workspace_trust::evaluate_workspace,
};
use omp_ext::workspace_trust::inventory::WORKFLOWS_DIR;
use tokio_util::sync::CancellationToken;

use crate::{cli::AdwCommand, process_ctx_with};

/// Runs one workflow verb.
pub async fn run(command: AdwCommand) -> miette::Result<()> {
	match command {
		AdwCommand::List { project } => list(&project),
		AdwCommand::Run { workflow, project, model, max_attempts, exec_policy } => {
			execute(&workflow, &project, model, max_attempts, exec_policy).await
		},
		AdwCommand::Resume { journal, project, exec_policy } => {
			resume(&journal, &project, exec_policy).await
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
	model: Option<Str>,
	max_attempts: u32,
	exec_policy: Option<AdwExecPolicy>,
) -> miette::Result<()> {
	let definition = definition::load(project, workflow).into_diagnostic()?;
	let data_dir = omp_core::dirs::data_dir(None).into_diagnostic()?;
	let explicit = model.is_some();
	let model = model.unwrap_or_else(|| Str::new_static("default"));
	let host = open_host(project, data_dir.clone(), model.clone(), explicit)?;
	let host = match exec_policy {
		Some(policy) => host.with_exec_policy(policy),
		None => host,
	};
	let (mut log, journal_path) =
		create_run_log(&definition, project, &data_dir, max_attempts, &model, explicit, exec_policy)?;
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
	if exit.code == 0 {
		Ok(())
	} else {
		Err(miette::miette!("workflow exited with status {}", exit.code))
	}
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
	exec_policy: Option<AdwExecPolicy>,
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
	let trusted = matches!(
		omp_driver::workspace_trust::evaluate_workspace(data_dir, project),
		omp_ext::workspace_trust::TrustDecision::Trusted { .. }
	);
	let start = omp_journal::runlog::RunStart {
		run: Str::from(run),
		workflow: Str::from(definition.workflow().name().as_str()),
		workflow_digest: Str::from(definition.digest()),
		input_digest: Str::default(),
		max_attempts,
		model: model.clone(),
		model_explicit,
		exec_policy: Str::new_static(<&'static str>::from(exec_policy.unwrap_or_default())),
		exec_fallback_local: false,
		workspace_trusted: trusted,
		workspace_root: Str::from(root.display().to_string()),
		workspace_head: head.map(Str::from),
		created: Str::from(jiff::Timestamp::now().to_string()),
	};
	let log = omp_journal::runlog::RunLog::create(&path, start)
		.map_err(|error| miette::miette!("could not create run log: {error}"))?;
	Ok((log, path))
}

async fn resume(
	journal_path: &Path,
	project: &Path,
	exec_policy: Option<AdwExecPolicy>,
) -> miette::Result<()> {
	let (records, start) = match omp_journal::runlog::RunLog::read(journal_path) {
		Ok(records) => {
			let start = match records.first() {
				Some(omp_journal::runlog::RunRecord::Start(start)) => start.clone(),
				_ => return Err(miette::miette!("run log carries no start record")),
			};
			(records, start)
		},
		Err(error) => return Err(miette::miette!("could not read run log: {error}")),
	};

	let definition = definition::load(project, &start.workflow).into_diagnostic()?;
	if definition.digest() != start.workflow_digest.as_str() {
		return Err(miette::miette!(
			"workflow definition `{}` changed since the run started (digest mismatch)",
			start.workflow
		));
	}

	let data_dir = omp_core::dirs::data_dir(None).into_diagnostic()?;
	let host = open_host(project, data_dir, start.model.clone(), start.model_explicit)?;
	let host = match exec_policy {
		Some(policy) => host.with_exec_policy(policy),
		None => host,
	};

	// The records `open` returns were read under the log's lock; the earlier
	// lock-free read only located the start record and its digest.
	let (log, records) = omp_journal::runlog::RunLog::open(journal_path, &start.workflow_digest)
		.map_err(|error| miette::miette!("could not reopen run log: {error}"))?;

	let cancel = CancellationToken::new();
	let exit = omp_driver::adw::resume_from_log(
		&definition,
		start.max_attempts,
		&host,
		&cancel,
		log,
		&records,
	)
	.await
	.into_diagnostic()?;

	println!("Resumed workflow `{}` from `{}`.", start.workflow, journal_path.display());
	if exit.code == 0 {
		Ok(())
	} else {
		Err(miette::miette!("resumed workflow exited with status {}", exit.code))
	}
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
