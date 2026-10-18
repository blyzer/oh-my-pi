//! Terminal presentation adapter for the driver-owned workflow host.

use std::{
	path::{Path, PathBuf},
	sync::Arc,
};

use miette::IntoDiagnostic as _;
use omp_core::Str;
use omp_driver::{
	adw::{RunStatus, definition, production::ProductionAdwHost},
	workspace_trust::evaluate_workspace,
};
use omp_ext::workspace_trust::inventory::WORKFLOWS_DIR;
use tokio_util::sync::CancellationToken;

use crate::{cli::AdwCommand, process_ctx_with};

/// Runs one workflow verb.
pub async fn run(command: AdwCommand) -> miette::Result<()> {
	match command {
		AdwCommand::List { project } => list(&project),
		AdwCommand::Run { workflow, project, model, max_attempts } => {
			execute(
				&workflow,
				&project,
				model.unwrap_or_else(|| Str::new_static("default")),
				max_attempts,
			)
			.await
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
	model: Str,
	max_attempts: u32,
) -> miette::Result<()> {
	let definition = definition::load(project, workflow).into_diagnostic()?;
	let data_dir = omp_core::dirs::data_dir(None).into_diagnostic()?;
	let host = open_host(project, data_dir, model)?;
	let cancel = CancellationToken::new();
	let exit = {
		let run = omp_driver::adw::run(&definition, max_attempts, &host, &cancel);
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

/// Opens the workflow host under the operator's configuration: user
/// `config.cfg` and the project overlay decide the sandbox, network and
/// approval posture its phases run under and report, and the workspace's
/// trust decision, evaluated here once for the run (failing closed), decides
/// whether its code phases run at all.
pub fn open_host(
	project: &Path,
	data_dir: PathBuf,
	model: Str,
) -> miette::Result<ProductionAdwHost> {
	let ctx = Arc::new(process_ctx_with(project, omp_con::Ctx::builder())?);
	let workspace_trust = evaluate_workspace(&data_dir, project);
	ProductionAdwHost::open(project.to_path_buf(), data_dir, model, false, ctx, workspace_trust)
		.into_diagnostic()
}
