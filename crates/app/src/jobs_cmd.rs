//! `omp jobs` — inspect and reattach journalled execution jobs.

use miette::miette;
use omp_agent::{JobBoard, JobKind, reattach_execution};
use omp_session::{ComponentRegistry, Session};

use crate::cli::{JobsArgs, JobsCommand};

/// Runs the `omp jobs` command tree.
pub async fn run(args: JobsArgs) -> miette::Result<()> {
	let journal = args.journal.ok_or_else(|| {
		miette!(
			"a session journal path is required; pass --journal <path> pointing to an .oms file"
		)
	})?;

	let session =
		Session::open(&journal, ComponentRegistry::standard()).map_err(|e| miette!("{e}"))?;
	let board = JobBoard::new();
	board.rebuild(&session);

	match args.command {
		None | Some(JobsCommand::Ls) => ls(&board),
		Some(JobsCommand::Attach { id }) => attach(&board, &id),
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
		println!(
			"{:<32}  {:<10}  {:<12}  {}",
			r.id,
			r.kind,
			r.status,
			r.label,
		);
	}
	Ok(())
}

/// Prints the reattach handle for a single execution job, or a diagnostic if
/// the job cannot be reattached.
fn attach(board: &JobBoard, id: &str) -> miette::Result<()> {
	let records = board.list();
	let Some(record) = records.iter().find(|r| r.id.as_str() == id) else {
		return Err(miette!("job {id:?} not found"));
	};
	if record.kind != JobKind::Execution {
		println!("job {id:?} is kind {:?} — not reattachable", record.kind);
		return Ok(());
	}
	match reattach_execution(board, id) {
		Some((_rec, handle)) => {
			println!("reattach handle: {handle}");
		},
		None => {
			println!("job {id:?} has no reattach handle recorded");
		},
	}
	Ok(())
}
