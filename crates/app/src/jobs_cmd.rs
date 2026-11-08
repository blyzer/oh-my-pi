//! `omp jobs`: inspect journaled jobs and resume watching accepted executions.

use std::{path::Path, sync::Arc};

use miette::{IntoDiagnostic as _, miette};
use omp_agent::{JobBoard, JobRecord};
use omp_driver::exec_composer::{ExecutionSettings, ResumeConnection};
use omp_journal::JournalError;
use omp_session::{ComponentRegistry, Session, SessionError};
use tokio_util::sync::CancellationToken;

use crate::cli::{JobsArgs, JobsCommand};

/// Runs the `omp jobs` command tree.
pub async fn run(args: JobsArgs) -> miette::Result<()> {
	let journal = args.journal.ok_or_else(|| {
		miette!("a session journal path is required; pass --journal <path> pointing to an .oms file")
	})?;
	let mut session = open(&journal)?;
	let board = JobBoard::new();
	board.rebuild(&session);

	match args.command {
		None | Some(JobsCommand::Ls) => ls(&board),
		Some(JobsCommand::Attach { id }) => attach(&journal, &mut session, &board, &id).await,
	}
}

/// Opens the journal as its one writer, so no other process settles its jobs
/// while this command runs.
fn open(journal: &Path) -> miette::Result<Session> {
	match Session::open(journal, ComponentRegistry::standard()) {
		Ok(session) => Ok(session),
		Err(SessionError::Journal(JournalError::Locked { path })) => Err(miette!(
			"session journal {} is open in another omp process; exit that session, then run `omp \
			 jobs` again",
			path.display()
		)),
		Err(error) => Err(error).into_diagnostic(),
	}
}

/// Prints a table of all jobs in the session.
fn ls(board: &JobBoard) -> miette::Result<()> {
	let records = board.list();
	if records.is_empty() {
		println!("no jobs");
		return Ok(());
	}
	println!("{:<32}  {:<10}  {:<12}  {}", "ID", "KIND", "STATUS", "LABEL");
	for r in &records {
		println!("{:<32}  {:<10}  {:<12}  {}", r.id, r.kind, r.status, r.label,);
	}
	Ok(())
}

/// Resumes watching the accepted execution job `id` until it settles.
///
/// Every refusal comes before the destination is contacted. Ctrl-C detaches:
/// the backend work keeps running, and the job stays reattachable.
async fn attach(
	journal: &Path,
	session: &mut Session,
	board: &JobBoard,
	id: &str,
) -> miette::Result<()> {
	// A job whose watcher was lost settles `EffectsUnknown` before it resumes.
	board.poll(session).into_diagnostic()?;
	let root = std::env::current_dir().into_diagnostic()?;
	let ctx = Arc::new(crate::process_ctx_with(&root, omp_con::Ctx::builder())?);
	let settings = ExecutionSettings::generic(&ctx).into_diagnostic()?;
	let resume = board
		.prepare_reattach(session, id, &settings.destination_identity())
		.into_diagnostic()?;
	let data_dir = omp_core::dirs::data_dir(None).into_diagnostic()?;
	let connection = ResumeConnection::open(
		&settings,
		resume.handle(),
		Arc::clone(&ctx),
		&root,
		&data_dir,
		&CancellationToken::new(),
	)
	.await
	.into_diagnostic()?;
	let record = board
		.reattach_execution(session, resume, connection.backend().clone())
		.await
		.into_diagnostic()?;
	println!("watching execution job {id}; Ctrl-C detaches and leaves its backend work running");
	let ids = [record.id.clone()];
	let settled = tokio::select! {
		settled = board.wait(session, Some(ids.as_slice())) => settled,
		signal = tokio::signal::ctrl_c() => {
			// Detach whatever the signal reported: a dropped run would cancel.
			let detached = board.detach(session, record.handle).await;
			signal.into_diagnostic()?;
			detached.into_diagnostic()?;
			println!(
				"detached from execution job {id}; resume with `omp jobs --journal {} attach {id}`",
				journal.display()
			);
			return Ok(());
		},
	};
	let settled = match settled {
		Ok(settled) => settled,
		Err(error) => {
			// Keep the backend work running while the journal failure surfaces.
			let _ = board.detach(session, record.handle).await;
			return Err(error).into_diagnostic();
		},
	};
	let Some(settled) = settled else {
		return Err(miette!("execution job {id} left the session before it settled"));
	};
	report(&settled)
}

/// Prints how a resumed execution job settled; anything but `completed` fails.
fn report(record: &JobRecord) -> miette::Result<()> {
	let state: &'static str = record.execution_state.map_or("unknown", Into::into);
	println!("execution job {}: {} ({state})", record.id, record.status);
	if record.status == "completed" {
		return Ok(());
	}
	Err(miette!(
		"execution job {} {}: {}",
		record.id,
		record.status,
		record
			.error
			.as_deref()
			.unwrap_or("no diagnostic was journaled")
	))
}

#[cfg(test)]
mod tests {
	use omp_core::Str;
	use omp_session::{ComponentRegistry, Session};

	use super::run;
	use crate::cli::{JobsArgs, JobsCommand};

	#[tokio::test]
	async fn attach_refuses_a_journal_another_writer_holds() {
		let temp = tempfile::tempdir().expect("temporary session directory");
		let path = temp.path().join("held.oms");
		let _writer = Session::create(&path, ComponentRegistry::standard()).expect("create session");
		let error = run(JobsArgs {
			journal: Some(path.clone()),
			command: Some(JobsCommand::Attach { id: Str::new_static("exec-42") }),
		})
		.await
		.expect_err("a second writer is refused");
		assert_eq!(
			error.to_string(),
			format!(
				"session journal {} is open in another omp process; exit that session, then run `omp \
				 jobs` again",
				path.display()
			)
		);
	}
}
