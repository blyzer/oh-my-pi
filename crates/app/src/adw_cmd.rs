//! Terminal presentation adapter for the driver-owned workflow host.

use std::{
	error::Error as StdError,
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
	exec_policy: Option<Str>,
) -> miette::Result<()> {
	let definition = definition::load(project, workflow).into_diagnostic()?;
	let data_dir = omp_core::dirs::data_dir(None).into_diagnostic()?;
	let model_override = model.is_some();
	let model = model.unwrap_or_else(|| Str::new_static("@smol"));
	let mut host = open_host(project, data_dir, model, model_override)?;
	if let Some(policy) = exec_policy {
		let policy: AdwExecPolicy = policy.parse().into_diagnostic()?;
		host = host.with_exec_policy(policy);
	}
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
			record.phase, record.attempt, record.status, record.decision
		);
	}
	match exit.status {
		RunStatus::Accepted => println!("Workflow `{workflow}` accepted every phase."),
		RunStatus::Halted => println!(
			"Workflow `{workflow}` halted{}.",
			exit
				.reason
				.as_deref()
				.map_or_else(String::new, |reason| format!(": {reason}"))
		),
		RunStatus::Blocked => println!(
			"Workflow `{workflow}` blocked{}.",
			exit
				.reason
				.as_deref()
				.map_or_else(String::new, |reason| format!(": {reason}"))
		),
		RunStatus::Failed(failure) => {
			println!(
				"Workflow `{workflow}` failed in phase `{}` attempt {}.",
				failure.phase, failure.attempt
			);
			print_error_chain(&failure.source);
		},
		RunStatus::Cancelled => println!("Workflow `{workflow}` cancelled."),
	}
	if exit.code == 0 {
		Ok(())
	} else {
		Err(miette::miette!("workflow exited with status {}", exit.code))
	}
}

/// Renders a typed host failure's cause chain at the terminal boundary.
fn print_error_chain(error: &(dyn StdError + 'static)) {
	println!("  caused by: {error}");
	let mut source = error.source();
	while let Some(error) = source {
		println!("  caused by: {error}");
		source = error.source();
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
