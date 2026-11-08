//! `omp jobs`: inspect journaled jobs and resume watching accepted executions.

use std::{future::Future, path::Path, sync::Arc};

use miette::{IntoDiagnostic as _, miette};
use omp_agent::{JobBoard, JobRecord, jobs::ExecutionResume};
use omp_driver::exec_composer::{ExecutionSettings, ResumeConnection};
use omp_journal::JournalError;
use omp_session::{ComponentRegistry, Session, SessionError};
use tokio_util::sync::CancellationToken;

use crate::cli::{JobsArgs, JobsCommand};

/// Ctrl-C detached an execution watch.
#[derive(Debug, thiserror::Error, miette::Diagnostic)]
#[error("execution watch interrupted")]
pub struct JobsInterrupted;

impl JobsInterrupted {
	/// Conventional shell status for Ctrl-C.
	#[must_use]
	pub const fn exit_code(&self) -> u8 {
		130
	}
}

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
	let settings_ref = &settings;
	let ctx_ref = &ctx;
	let root_ref = &root;
	let (resume, connection) =
		connect_after_validation(session, board, id, &settings, |resume| async move {
			let data_dir = omp_core::dirs::data_dir(None).into_diagnostic()?;
			let connection = ResumeConnection::open(
				settings_ref,
				resume.handle(),
				Arc::clone(ctx_ref),
				root_ref,
				&data_dir,
				&CancellationToken::new(),
			)
			.await
			.into_diagnostic()?;
			Ok((resume, connection))
		})
		.await?;
	let record = board
		.reattach_execution(session, resume, connection.backend().clone())
		.await
		.into_diagnostic()?;
	println!("watching execution job {id}; Ctrl-C detaches and leaves its backend work running");
	let ids = [record.id.clone()];
	let settled = tokio::select! {
		settled = board.wait(session, Some(ids.as_slice())) => settled,
		signal = tokio::signal::ctrl_c() => {
			let detached = board.detach(record.handle);
			signal.into_diagnostic()?;
			// Keep the supervisor alive until it has detached and poll has journaled its result.
			let settled = board.wait(session, Some(ids.as_slice())).await.into_diagnostic()?;
			let Some(settled) = settled else {
				return Err(miette!("execution job {id} left the session before it settled"));
			};
			return report_interrupted(session, &settled, detached, journal);
		},
	};
	let settled = match settled {
		Ok(settled) => settled,
		Err(error) => {
			// Keep the backend work running while the journal failure surfaces.
			let _ = board.detach(record.handle);
			return Err(error).into_diagnostic();
		},
	};
	let Some(settled) = settled else {
		return Err(miette!("execution job {id} left the session before it settled"));
	};
	report(session, &settled)
}


fn report_interrupted(
	session: &Session,
	record: &JobRecord,
	detached: bool,
	journal: &Path,
) -> miette::Result<()> {
	let state: Option<&'static str> = record.execution_state.map(Into::into);
	if detached
		&& state == Some("effects-unknown")
		&& !matches!(record.status.as_str(), "completed" | "cancelled")
	{
		let id = &record.id;
		println!("detached from execution job {id}; resume with `omp jobs --journal {} attach {id}`", journal.display());
		return Err(JobsInterrupted.into());
	}
	report(session, record)
}
async fn connect_after_validation<T, F, Fut>(
	session: &Session,
	board: &JobBoard,
	id: &str,
	settings: &ExecutionSettings,
	connect: F,
) -> miette::Result<T>
where
	F: FnOnce(ExecutionResume) -> Fut,
	Fut: Future<Output = miette::Result<T>>,
{
	let resume = board
		.prepare_reattach(session, id, &settings.destination_identity())
		.into_diagnostic()?;
	connect(resume).await
}

fn summary(session: &Session, record: &JobRecord) -> miette::Result<String> {
	let state: &'static str = record.execution_state.map_or("unknown", Into::into);
	let mut text = format!("execution job {}: {} ({state})", record.id, record.status);
	if let Some(raw) = record.execution_result.as_deref() {
		let result = omp_agent::jobs::resolve_execution_result(session, raw)
			.into_diagnostic()?
			.ok_or_else(|| miette!("execution result is unavailable"))?;
		text.push_str(&format!(", outcome: {:?}", result.outcome));
	} else if let Some(failure) = record.execution_failure {
		text.push_str(&format!(", execution failure: {failure}"));
	}
	Ok(text)
}

/// Prints how a resumed execution job settled; anything but `completed` fails.
fn report(session: &Session, record: &JobRecord) -> miette::Result<()> {
	let text = summary(session, record)?;
	println!("{text}");
	if record.status == "completed" {
		return Ok(());
	}
	if let Some(error) = record.error.as_deref() {
		Err(miette!("{text}: {error}"))
	} else if record.execution_result.is_some() || record.execution_failure.is_some() {
		Err(miette!("{text}"))
	} else {
		Err(miette!("{text}: no diagnostic was journaled"))
	}
}

#[cfg(test)]
mod tests {
	use omp_core::Str;
	use omp_session::{ComponentRegistry, Session};

	use super::{JobsInterrupted, connect_after_validation, report, report_interrupted, run, summary};
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

	#[tokio::test]
	async fn journal_refusal_never_calls_connector() {
		use std::sync::atomic::{AtomicUsize, Ordering};

		use omp_driver::exec_composer::{
			ExecutionSettings, SV_EXEC_REMOTE_ENDPOINT, SV_EXEC_REMOTE_IMAGE,
		};

		let temp = tempfile::tempdir().expect("temporary session directory");
		let session = Session::create(&temp.path().join("empty.oms"), ComponentRegistry::standard())
			.expect("create session");
		let board = omp_agent::JobBoard::new();
		board.rebuild(&session);
		let ctx = omp_con::Ctx::new();
		SV_EXEC_REMOTE_ENDPOINT
			.set(&ctx, Str::new_static("grpcs://example.com"))
			.unwrap();
		SV_EXEC_REMOTE_IMAGE
			.set(&ctx, Str::new_static("image@sha256:abc"))
			.unwrap();
		let settings = ExecutionSettings::generic(&ctx).unwrap();
		let calls = AtomicUsize::new(0);
		let head = session.head();
		let result: miette::Result<()> =
			connect_after_validation(&session, &board, "missing", &settings, |_| async {
				calls.fetch_add(1, Ordering::SeqCst);
				Ok(())
			})
			.await;
		assert!(result.is_err());
		assert_eq!(calls.load(Ordering::SeqCst), 0);
		assert_eq!(session.head(), head);
	}

	#[test]
	fn report_shows_terminal_outcome_or_execution_failure() {
		let temp = tempfile::tempdir().expect("temporary session directory");
		let session = Session::create(&temp.path().join("report.oms"), ComponentRegistry::standard())
			.expect("create session");
		let mut record = omp_agent::JobRecord {
			handle:            omp_dom::Handle::new(1).unwrap(),
			id:                Str::new_static("exec-42"),
			kind:              omp_agent::jobs::JobKind::Execution,
			status:            Str::new_static("failed"),
			job_type:          Str::new_static("execution"),
			label:             Str::new_static("test"),
			owner:             None,
			factory_lease:     None,
			started:           None,
			duration_ms:       None,
			output:            None,
			error:             None,
			call:              None,
			detached_at:       None,
			backend:           None,
			reattach_handle:   None,
			request:           None,
			output_mode:       None,
			fallback:          None,
			execution_state:   None,
			execution_result:  None,
			execution_failure: Some("effects-unknown".parse().unwrap()),
		};
		assert_eq!(
			summary(&session, &record).unwrap(),
			"execution job exec-42: failed (unknown), execution failure: effects-unknown"
		);
		assert_eq!(
			report(&session, &record).unwrap_err().to_string(),
			"execution job exec-42: failed (unknown), execution failure: effects-unknown"
		);
		record.execution_failure = None;
		record.execution_result = Some(serde_json::value::to_raw_value(&serde_json::json!({
			"id": "exec-42", "backend": "remote", "outcome": {"Exited": {"code": 42}},
			"logs": null, "artifacts": [], "output_digests": {},
			"timing": {"queued": 0, "upload": 0, "execution": 0, "download": 0, "total": 0},
			"transfer": {"blobs_total": 0, "cache_hits": 0, "cache_misses": 0, "blobs_uploaded": 0, "bytes_uploaded": 0, "bytes_uploaded_compressed": 0, "bytes_downloaded": 0},
			"cache_hit": false, "fallback": null
		})).unwrap());
		assert_eq!(
			summary(&session, &record).unwrap(),
			"execution job exec-42: failed (unknown), outcome: Exited { code: 42 }"
		);
		assert_eq!(
			report(&session, &record).unwrap_err().to_string(),
			"execution job exec-42: failed (unknown), outcome: Exited { code: 42 }"
		);
		let journal = temp.path().join("report.oms");
		record.execution_state = Some("settled".parse().unwrap());
		for detached in [false, true] {
			assert_eq!(
				report_interrupted(&session, &record, detached, &journal).unwrap_err().to_string(),
				"execution job exec-42: failed (settled), outcome: Exited { code: 42 }"
			);
		}
		record.status = Str::new_static("completed");
		assert!(report_interrupted(&session, &record, true, &journal).is_ok());
		record.status = Str::new_static("failed");
		record.execution_result = None;
		record.execution_state = Some("effects-unknown".parse().unwrap());
		let error = report_interrupted(&session, &record, true, &journal).unwrap_err();
		assert!(error.downcast_ref::<JobsInterrupted>().is_some());
		assert_eq!(
			report_interrupted(&session, &record, false, &journal).unwrap_err().to_string(),
			"execution job exec-42: failed (effects-unknown): no diagnostic was journaled"
		);
		record.error = Some(Str::new_static("backend failed"));
		assert_eq!(
			report(&session, &record).unwrap_err().to_string(),
			"execution job exec-42: failed (effects-unknown): backend failed"
		);
	}

	#[test]
	fn detached_interrupt_has_sigint_exit_code() {
		assert_eq!(JobsInterrupted.exit_code(), 130);
	}
}
