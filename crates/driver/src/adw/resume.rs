//! Resumes a recorded run for `omp adw resume` and the chat `/adw resume`.

use std::{
	error::Error as StdError,
	path::{Path, PathBuf},
};

use omp_adw::ApprovalScope;
use omp_core::Str;
use omp_envd::tool_url::{InvalidRunId, RUN_EXTENSION, validate_run_id};
use omp_ext::workspace_trust::WorkspaceTrust;
use omp_journal::runlog::{RunLog, RunLogError, RunRecord, RunStart};
use tokio_util::sync::CancellationToken;

use super::{
	AdwHost, DefinitionError, Error, RunExit, RunStatus, definition, had_no_effect, resume_from_log,
};

#[derive(Clone, Copy, Debug)]
/// The journal of the run to resume.
pub enum RunRef<'a> {
	/// A run id under `runs_dir`, validated before a path is built from it.
	Id {
		/// The runs directory, `<data dir>/adw/runs`.
		runs_dir: &'a Path,
		/// The run id.
		id:       &'a str,
	},
	/// A journal file named directly.
	Journal(&'a Path),
}

#[derive(Clone, Copy, Debug)]
/// What a resume may do.
pub struct ResumeRequest<'a> {
	/// The run to resume.
	pub run:              RunRef<'a>,
	/// The project root the run's workflow definition loads from.
	pub project:          &'a Path,
	/// Refuses an untrusted workspace before the log is opened or a phase
	/// dispatches.
	pub require_trusted:  bool,
	/// The loosest approval scope the run may resolve to; `None` sets no
	/// ceiling.
	pub approval_ceiling: Option<ApprovalScope>,
}

/// A host opened for one resume, with the workspace trust it runs under.
pub struct OpenedHost<H> {
	/// The host the run continues under.
	pub host:  H,
	/// The trust of the workspace the host gates code phases on.
	pub trust: WorkspaceTrust,
}

#[derive(Debug)]
/// How a resume that was not refused ended.
pub enum ResumeOutcome<E: StdError + 'static> {
	/// The run continued; `exit` is how it stopped this time.
	Resumed {
		/// The workflow the run executes.
		workflow:  Str,
		/// The journal the run continued from.
		journal:   PathBuf,
		/// How the continued run stopped.
		exit:      RunExit<E>,
		/// Set when the run was cancelled: where that resume stopped, read from
		/// its journal.
		cancelled: Option<CancelledAt>,
	},
	/// The journal already ended in `Done`; nothing was dispatched or appended.
	Finished {
		/// The workflow the run executed.
		workflow: Str,
		/// The journal that already ended.
		journal:  PathBuf,
		/// Whether the finished run was accepted.
		accepted: bool,
	},
}

#[derive(Debug, thiserror::Error)]
/// Why a resume was refused or failed. Every variant except `Run` is returned
/// before the run log is opened for writing.
pub enum ResumeError<E: StdError + 'static> {
	#[error(transparent)]
	/// The run id is empty, escapes the runs directory, or holds other
	/// characters.
	InvalidRunId(#[from] InvalidRunId),
	/// No journal is recorded for the run id.
	#[error("no run is recorded as `{id}` under {}", .runs_dir.display())]
	UnknownRun {
		/// The run id.
		id:       String,
		/// The runs directory that was searched.
		runs_dir: PathBuf,
	},
	/// The journal cannot be read.
	#[error("run log {} could not be read", .path.display())]
	Journal {
		/// The journal file.
		path:   PathBuf,
		/// The journal failure.
		#[source]
		source: RunLogError,
	},
	/// The journal has no start record.
	#[error("run log {} carries no start record", .path.display())]
	NoStartRecord {
		/// The journal file.
		path: PathBuf,
	},
	#[error(transparent)]
	/// The workflow definition does not load.
	Definition(#[from] DefinitionError),
	/// The workflow definition changed since the run started.
	#[error("workflow definition `{workflow}` changed since the run started (digest mismatch)")]
	DigestMismatch {
		/// The workflow name the run recorded.
		workflow: String,
	},
	/// The workflow host could not be opened.
	#[error("workflow host could not be opened")]
	Open(#[source] E),
	#[error("workspace is not trusted; resume refused before any phase was dispatched")]
	/// The workspace is not trusted and the request requires trust.
	UntrustedWorkspace,
	/// The host reports more approval than the request allows.
	#[error("host approval {host:?} is above the {ceiling:?} ceiling")]
	ApprovalExceedsCeiling {
		/// The approval scope the host reports.
		host:    ApprovalScope,
		/// The loosest scope the request allows.
		ceiling: ApprovalScope,
	},
	/// The run could not proceed: posture, replay, journal or transition.
	#[error(transparent)]
	Run(#[from] Error<E>),
}

/// Resumes the run `request` names. Checks run in order; nothing dispatches
/// before the last one passes.
pub async fn resume<H, F>(
	request: ResumeRequest<'_>,
	open_host: F,
	cancel: &CancellationToken,
) -> Result<ResumeOutcome<H::Error>, ResumeError<H::Error>>
where
	H: AdwHost,
	F: FnOnce(&RunStart) -> Result<OpenedHost<H>, H::Error>,
{
	let journal = locate::<H::Error>(request.run)?;
	let records = RunLog::read(&journal)
		.map_err(|source| ResumeError::Journal { path: journal.clone(), source })?;
	let Some(RunRecord::Start(start)) = records.first() else {
		return Err(ResumeError::NoStartRecord { path: journal });
	};
	if let Some(RunRecord::Done(done)) = records.last() {
		return Ok(ResumeOutcome::Finished {
			workflow: start.workflow.clone(),
			journal,
			accepted: done.accepted,
		});
	}

	let definition = definition::load(request.project, &start.workflow)?;
	if definition.digest() != start.workflow_digest.as_str() {
		return Err(ResumeError::DigestMismatch { workflow: start.workflow.to_string() });
	}

	let OpenedHost { host, trust } = open_host(start).map_err(ResumeError::Open)?;
	if request.require_trusted && !matches!(trust, WorkspaceTrust::Trusted) {
		return Err(ResumeError::UntrustedWorkspace);
	}
	if let Some(ceiling) = request.approval_ceiling {
		let host_approval = host.posture().map_err(Error::Posture)?.approval;
		if looseness(host_approval) > looseness(ceiling) {
			return Err(ResumeError::ApprovalExceedsCeiling { host: host_approval, ceiling });
		}
	}

	let (log, locked) = RunLog::open(&journal, &start.workflow_digest)
		.map_err(|source| ResumeError::Journal { path: journal.clone(), source })?;
	let exit = resume_from_log(&definition, start.max_attempts, &host, cancel, log, &locked).await?;
	let cancelled = if matches!(exit.status, RunStatus::Cancelled) {
		let after = RunLog::read(&journal)
			.map_err(|source| ResumeError::Journal { path: journal.clone(), source })?;
		Some(classify_cancel(after.get(locked.len()..).unwrap_or_default()))
	} else {
		None
	};
	Ok(ResumeOutcome::Resumed { workflow: start.workflow.clone(), journal, exit, cancelled })
}

/// Where a cancelled resume stopped, read from what that resume journaled.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CancelledAt {
	/// No phase was dispatched by this resume.
	BeforeAnyPhase,
	/// Phases settled, and the cancel landed before the next dispatch.
	BetweenPhases,
	/// `phase` attempt `attempt` was dispatched and cut off before it settled.
	/// When `may_have_effects` is set, resuming reports the attempt as uncertain
	/// instead of re-running it.
	Interrupted {
		/// The interrupted phase.
		phase:            Str,
		/// The interrupted attempt.
		attempt:          u32,
		/// Whether the recorded posture allows effects.
		may_have_effects: bool,
	},
}

/// Classifies what one resume appended to the journal, excluding the records
/// that were already there when it started.
fn classify_cancel(appended: &[RunRecord]) -> CancelledAt {
	let last_dispatch = appended
		.iter()
		.enumerate()
		.rev()
		.find_map(|(index, record)| match record {
			RunRecord::Dispatch(dispatch) => Some((index, dispatch)),
			_ => None,
		});
	let Some((index, dispatch)) = last_dispatch else {
		let settled = appended
			.iter()
			.any(|record| matches!(record, RunRecord::Settle(_)));
		return if settled {
			CancelledAt::BetweenPhases
		} else {
			CancelledAt::BeforeAnyPhase
		};
	};
	let settled = appended[index + 1..].iter().any(|record| match record {
		RunRecord::Settle(settle) => {
			settle.phase.as_str() == dispatch.phase.as_str() && settle.attempt == dispatch.attempt
		},
		_ => false,
	});
	if settled {
		return CancelledAt::BetweenPhases;
	}
	CancelledAt::Interrupted {
		phase:            dispatch.phase.clone(),
		attempt:          dispatch.attempt,
		may_have_effects: !had_no_effect(dispatch),
	}
}

fn locate<E: StdError + 'static>(run: RunRef<'_>) -> Result<PathBuf, ResumeError<E>> {
	match run {
		RunRef::Journal(path) => Ok(path.to_path_buf()),
		RunRef::Id { runs_dir, id } => {
			validate_run_id(id)?;
			let journal = runs_dir.join(format!("{id}.{RUN_EXTENSION}"));
			if journal.is_file() {
				Ok(journal)
			} else {
				Err(ResumeError::UnknownRun {
					id:       id.to_owned(),
					runs_dir: runs_dir.to_path_buf(),
				})
			}
		},
	}
}

const fn looseness(scope: ApprovalScope) -> u8 {
	match scope {
		ApprovalScope::AlwaysAsk => 0,
		ApprovalScope::Write => 1,
		ApprovalScope::Yolo => 2,
	}
}
