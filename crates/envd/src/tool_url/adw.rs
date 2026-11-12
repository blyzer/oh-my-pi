//! Read-only `adw://` resolver over recorded ADW workflow run journals.
//!
//! Runs are written by `omp adw run` to `<data dir>/adw/runs/<run-id>.oms`
//! (ADR 0051). This resolver only reads them, through the lock-free
//! [`RunLog::read`], so it can inspect a run that is still executing.

use std::{
	fmt::Write as _,
	path::{Path, PathBuf},
};

use omp_core::{CowBytes, Str, dirs::DataDirError};
use omp_journal::runlog::{
	PhaseDispatch, PhaseSettle, PhaseUncertain, RunLog, RunRecord, RunStart, SettleStatus,
};
use omp_tools::read::{
	Fault,
	resolver::{
		LineOffsetCache, Resolve, ResourceCompletion, ResourceEntry, ResourceList, fuzzy_score,
	},
	selector::ParsedSelector,
};

/// File extension of a run journal.
const RUN_EXTENSION: &str = "oms";
/// Description attached to every run completion.
const RUN_DESCRIPTION: &str = "recorded ADW workflow run";

/// Why an `adw://` resource could not be served.
#[derive(Debug, thiserror::Error)]
pub(crate) enum AdwError {
	/// The data directory holding `adw/runs` could not be resolved.
	#[error("the ADW data directory is unavailable: {0}")]
	DataDir(String),
	/// A run id was empty, escaped the runs directory, or held odd characters.
	#[error(
		"invalid ADW run id '{0}': ids may contain only ASCII letters, digits, '-', '_' and '.', \
		 and cannot contain path separators or '..'"
	)]
	InvalidRunId(String),
	/// No journal exists for the run id.
	#[error("unknown ADW run: adw://{0}. Read adw://runs to list recorded runs.")]
	UnknownRun(String),
	/// The runs directory could not be listed.
	#[error("could not list ADW runs in {path}: {message}")]
	Io {
		/// Directory that failed.
		path:    String,
		/// Rendered OS error.
		message: String,
	},
	/// A run journal could not be read.
	#[error("ADW run {id} is unreadable: {reason}")]
	Log {
		/// Run id.
		id:     String,
		/// One-line cause.
		reason: String,
	},
}

impl From<AdwError> for Fault {
	fn from(error: AdwError) -> Self {
		let message = Str::new(error.to_string());
		match error {
			AdwError::InvalidRunId(_) => Self::Invalid { message },
			_ => Self::Source { message },
		}
	}
}

/// Read-only resolver for recorded ADW workflow runs.
#[derive(Debug)]
pub(crate) struct AdwResolver {
	/// `<data dir>/adw/runs`, or why it could not be resolved.
	runs: Result<PathBuf, DataDirError>,
}

impl AdwResolver {
	/// Resolver over an explicit runs directory.
	#[cfg(test)]
	pub(crate) fn new(runs_dir: PathBuf) -> Self {
		Self { runs: Ok(runs_dir) }
	}

	/// Resolver over `<data dir>/adw/runs`, the directory `omp adw run`
	/// writes to. A data directory that cannot be resolved is reported by
	/// every read as a typed fault instead of failing registration.
	pub(crate) fn from_data_dir() -> Self {
		Self { runs: omp_core::dirs::data_dir(None).map(|dir| dir.join("adw").join("runs")) }
	}

	fn runs_dir(&self) -> Result<&Path, AdwError> {
		self
			.runs
			.as_deref()
			.map_err(|error| AdwError::DataDir(error.to_string()))
	}

	/// Run ids with a journal on disk, newest first (ids are ULIDs).
	fn run_files(&self) -> Result<Vec<(String, PathBuf)>, AdwError> {
		let dir = self.runs_dir()?;
		let entries = match std::fs::read_dir(dir) {
			Ok(entries) => entries,
			Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
			Err(error) => {
				return Err(AdwError::Io {
					path:    dir.display().to_string(),
					message: error.to_string(),
				});
			},
		};
		let mut runs = entries
			.filter_map(Result::ok)
			.filter_map(|entry| {
				let path = entry.path();
				if path.extension().and_then(|ext| ext.to_str()) != Some(RUN_EXTENSION) {
					return None;
				}
				let id = path.file_stem()?.to_str()?.to_owned();
				validate_run_id(&id).ok()?;
				Some((id, path))
			})
			.collect::<Vec<_>>();
		runs.sort_unstable_by(|left, right| right.0.cmp(&left.0));
		Ok(runs)
	}

	fn load(&self, id: &str) -> Result<Vec<RunRecord>, AdwError> {
		validate_run_id(id)?;
		let path = self.runs_dir()?.join(format!("{id}.{RUN_EXTENSION}"));
		if !path.is_file() {
			return Err(AdwError::UnknownRun(id.to_owned()));
		}
		read_records(id, &path)
	}

	fn render_runs(&self) -> Result<String, AdwError> {
		let dir = self.runs_dir()?;
		let mut rows = Vec::new();
		let mut skipped = Vec::new();
		for (id, path) in self.run_files()? {
			match read_records(&id, &path) {
				Ok(records) => {
					if let Some(start) = start_of(&records) {
						rows.push((
							start.created.to_string(),
							id,
							start.workflow.to_string(),
							outcome(&records),
						));
					}
				},
				Err(error) => skipped.push(one_line(&error.to_string())),
			}
		}
		rows.sort_unstable_by(|left, right| right.0.cmp(&left.0).then_with(|| right.1.cmp(&left.1)));
		let mut out = String::from("# ADW runs\n\n");
		let _ = writeln!(
			out,
			"Recorded under {}, newest first. Read adw://<run-id> for phases.\n",
			dir.display()
		);
		if rows.is_empty() {
			out.push_str("No ADW runs recorded.\n");
		}
		for (started, id, workflow, outcome) in &rows {
			let _ = write!(out, "- adw://{id} | workflow: {workflow}");
			if !started.is_empty() {
				let _ = write!(out, " | started: {started}");
			}
			let _ = writeln!(out, " | outcome: {outcome}");
		}
		if !skipped.is_empty() {
			let _ = writeln!(out, "\nSkipped {} unreadable run log(s):", skipped.len());
			for note in skipped {
				let _ = writeln!(out, "- {note}");
			}
		}
		Ok(out)
	}

	fn render_run(&self, id: &str) -> Result<String, AdwError> {
		let records = self.load(id)?;
		let start = start_of(&records).ok_or_else(|| AdwError::Log {
			id:     id.to_owned(),
			reason: "no start record".to_owned(),
		})?;
		let mut out = format!("# ADW run {id}\n\n");
		let _ = writeln!(out, "- Workflow: {}", start.workflow);
		let _ = writeln!(out, "- Project: {}", start.workspace_root);
		if let Some(head) = &start.workspace_head {
			let _ = writeln!(out, "- Project head: {head}");
		}
		if !start.created.is_empty() {
			let _ = writeln!(out, "- Started: {}", start.created);
		}
		let _ = writeln!(
			out,
			"- Exec policy: {}{}",
			start.exec_policy,
			if start.exec_fallback_local {
				" (local fallback allowed)"
			} else {
				""
			}
		);
		let _ = writeln!(out, "- Workspace trusted: {}", yes_no(start.workspace_trusted));
		let _ = writeln!(
			out,
			"- Model: {}{}",
			start.model,
			if start.model_explicit {
				" (explicit)"
			} else {
				""
			}
		);
		let _ = writeln!(out, "- Max attempts per phase: {}", start.max_attempts);
		let _ = writeln!(out, "- Outcome: {}", outcome(&records));
		out.push_str("\n## Phases\n");
		let phases = phases_of(&records);
		if phases.is_empty() {
			out.push_str("\nNo phase has been dispatched.\n");
		}
		for phase in &phases {
			let _ = writeln!(out, "\n### {}", phase.name);
			for attempt in &phase.attempts {
				let _ = writeln!(out, "{}", attempt_line(attempt));
			}
		}
		out.push_str("\n## Result\n\n");
		let _ = writeln!(out, "{}", result_line(&records));
		Ok(out)
	}
}

/// Rejects ids that could leave the runs directory or name no ULID-like file.
fn validate_run_id(id: &str) -> Result<&str, AdwError> {
	let valid = !id.is_empty()
		&& !id.starts_with('.')
		&& !id.contains("..")
		&& id
			.chars()
			.all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
	if valid {
		Ok(id)
	} else {
		Err(AdwError::InvalidRunId(id.to_owned()))
	}
}

/// Splits a resource into its target: the run listing (`None`) or one run id.
fn target(resource: &str) -> Option<&str> {
	let resource = resource.strip_suffix('/').unwrap_or(resource);
	(!resource.is_empty() && resource != "runs").then_some(resource)
}

fn read_records(id: &str, path: &Path) -> Result<Vec<RunRecord>, AdwError> {
	let log_error =
		|reason: String| AdwError::Log { id: id.to_owned(), reason: one_line(&reason) };
	let records = RunLog::read(path).map_err(|error| {
		let mut reason = error.to_string();
		let mut source = std::error::Error::source(&error);
		while let Some(cause) = source {
			let _ = write!(reason, ": {cause}");
			source = cause.source();
		}
		log_error(reason)
	})?;
	if start_of(&records).is_none() {
		return Err(log_error("a run journal must begin with a start record".to_owned()));
	}
	Ok(records)
}

const fn start_of(records: &[RunRecord]) -> Option<&RunStart> {
	match records.first() {
		Some(RunRecord::Start(start)) => Some(start),
		_ => None,
	}
}

fn one_line(text: &str) -> String {
	text.split_whitespace().collect::<Vec<_>>().join(" ")
}

const fn yes_no(value: bool) -> &'static str {
	if value { "yes" } else { "no" }
}

/// Where a run stands, judged from its last record. A halted run may be
/// resumed, so only `Done` is final.
fn outcome(records: &[RunRecord]) -> String {
	match records.last() {
		Some(RunRecord::Done(done)) if done.accepted => "done".to_owned(),
		Some(RunRecord::Done(_)) => "done (not accepted)".to_owned(),
		Some(RunRecord::Halt(halt)) => format!("halted: {}", one_line(&halt.reason)),
		Some(RunRecord::Uncertain(uncertain)) => {
			format!("uncertain: phase {} attempt {}", uncertain.phase, uncertain.attempt)
		},
		_ => "in progress".to_owned(),
	}
}

fn result_line(records: &[RunRecord]) -> String {
	match records.last() {
		Some(RunRecord::Done(done)) if done.accepted => "Done: every phase was accepted.".to_owned(),
		Some(RunRecord::Done(_)) => "Done: the run finished without being accepted.".to_owned(),
		Some(RunRecord::Halt(halt)) => format!("Halted: {}", halt.reason),
		Some(RunRecord::Uncertain(uncertain)) => format!(
			"Uncertain: phase {} attempt {} could not be decided ({}).",
			uncertain.phase, uncertain.attempt, uncertain.reason
		),
		_ => "In progress: no halt or done record yet.".to_owned(),
	}
}

#[derive(Default)]
struct Attempt {
	number:    u32,
	dispatch:  Option<PhaseDispatch>,
	settle:    Option<PhaseSettle>,
	uncertain: Option<PhaseUncertain>,
}

struct PhaseView {
	name:     String,
	attempts: Vec<Attempt>,
}

/// Groups records by phase, then attempt, in first-seen order.
fn phases_of(records: &[RunRecord]) -> Vec<PhaseView> {
	fn attempt<'a>(phases: &'a mut Vec<PhaseView>, phase: &str, number: u32) -> &'a mut Attempt {
		let index = phases
			.iter()
			.position(|view| view.name == phase)
			.unwrap_or_else(|| {
				phases.push(PhaseView { name: phase.to_owned(), attempts: Vec::new() });
				phases.len() - 1
			});
		let attempts = &mut phases[index].attempts;
		let slot = attempts
			.iter()
			.position(|candidate| candidate.number == number)
			.unwrap_or_else(|| {
				attempts.push(Attempt { number, ..Attempt::default() });
				attempts.len() - 1
			});
		&mut attempts[slot]
	}
	let mut phases = Vec::new();
	for record in records {
		match record {
			RunRecord::Dispatch(dispatch) => {
				attempt(&mut phases, &dispatch.phase, dispatch.attempt).dispatch =
					Some(dispatch.clone());
			},
			RunRecord::Settle(settle) => {
				attempt(&mut phases, &settle.phase, settle.attempt).settle = Some(settle.clone());
			},
			RunRecord::Uncertain(uncertain) => {
				attempt(&mut phases, &uncertain.phase, uncertain.attempt).uncertain =
					Some(uncertain.clone());
			},
			RunRecord::Start(_) | RunRecord::Halt(_) | RunRecord::Done(_) => {},
		}
	}
	phases
}

fn attempt_line(attempt: &Attempt) -> String {
	let mut line = format!("- attempt {}: ", attempt.number);
	match &attempt.dispatch {
		Some(dispatch) => {
			let _ = write!(line, "dispatched to {}", dispatch.executor);
			if !dispatch.posture.is_empty() {
				let _ = write!(line, " (posture {})", dispatch.posture);
			}
			if !dispatch.inputs.is_empty() {
				let inputs = dispatch
					.inputs
					.iter()
					.map(|input| format!("{} v{}", input.phase, input.version))
					.collect::<Vec<_>>()
					.join(", ");
				let _ = write!(line, "; inputs {inputs}");
			}
		},
		None => line.push_str("no dispatch record"),
	}
	line.push_str(" -> ");
	if let Some(settle) = &attempt.settle {
		line.push_str(match settle.status {
			SettleStatus::Accepted => "accepted",
			SettleStatus::Rejected => "rejected",
			SettleStatus::Failed => "failed",
		});
		if !settle.feedback.is_empty() {
			let _ = write!(line, ": {}", one_line(&settle.feedback));
		}
	} else if let Some(uncertain) = &attempt.uncertain {
		let _ = write!(line, "uncertain: {}", one_line(&uncertain.reason));
	} else {
		line.push_str("not settled");
	}
	line
}

impl Resolve for AdwResolver {
	async fn read<'a>(
		&'a self,
		resource: &'a str,
		selector: &'a ParsedSelector,
	) -> Result<CowBytes<'static>, Fault> {
		let text = match target(resource) {
			None => self.render_runs()?,
			Some(id) => self.render_run(id)?,
		};
		// Run logs grow while a run executes, so line offsets are never reused.
		super::select_bytes(
			&LineOffsetCache::default(),
			resource,
			CowBytes::from(text.into_bytes()),
			selector,
		)
	}

	async fn list(
		&self,
		resource: &str,
		max_entries: usize,
		max_bytes: usize,
	) -> Result<ResourceList, Fault> {
		if let Some(id) = target(resource) {
			validate_run_id(id)?;
			return Err(Fault::Invalid {
				message: Str::new_static("ADW runs cannot be listed; read the run to see its phases."),
			});
		}
		let mut entries = Vec::new();
		let mut bytes = 0usize;
		let mut truncated = false;
		for (id, path) in self.run_files()? {
			let entry_bytes = id.len().saturating_mul(2).saturating_add(6);
			if entries.len() == max_entries || bytes.saturating_add(entry_bytes) > max_bytes {
				truncated = true;
				break;
			}
			bytes += entry_bytes;
			let size = std::fs::metadata(&path).map_or(0, |meta| meta.len());
			entries.push(ResourceEntry {
				uri: Str::new(format!("adw://{id}")),
				name: Str::new(id),
				directory: false,
				size,
			});
		}
		Ok(ResourceList { entries, truncated })
	}

	async fn complete(
		&self,
		query: &str,
		max_results: usize,
	) -> Result<Vec<ResourceCompletion>, Fault> {
		let query = query.trim();
		let query = query
			.strip_prefix("adw://")
			.unwrap_or(query)
			.trim_start_matches('/');
		let mut candidates =
			vec![("runs".to_owned(), Str::new_static("recorded ADW workflow runs index"))];
		candidates.extend(
			self
				.run_files()?
				.into_iter()
				.map(|(id, _)| (id, Str::new_static(RUN_DESCRIPTION))),
		);
		let mut matches = candidates
			.into_iter()
			.filter_map(|(path, description)| {
				Some(ResourceCompletion {
					value: Str::new(format!("adw://{path}")),
					description,
					score: fuzzy_score(query, &path)?,
				})
			})
			.collect::<Vec<_>>();
		matches.sort_unstable_by(|left, right| {
			right
				.score
				.cmp(&left.score)
				.then_with(|| left.value.cmp(&right.value))
		});
		matches.truncate(max_results);
		Ok(matches)
	}
}

#[cfg(test)]
mod tests {
	use omp_journal::runlog::{EffectKey, InputVersion, RunDone, RunHalt};

	use super::*;

	fn start(run: &str, workflow: &str, created: &str) -> RunStart {
		RunStart {
			run:                 Str::new(run),
			workflow:            Str::new(workflow),
			workflow_digest:     Str::new("digest"),
			input_digest:        Str::default(),
			max_attempts:        3,
			model:               Str::new("test-model"),
			model_explicit:      false,
			exec_policy:         Str::new("sandboxed"),
			exec_fallback_local: false,
			workspace_trusted:   true,
			workspace_root:      Str::new("/work/project"),
			workspace_head:      None,
			created:             Str::new(created),
		}
	}

	fn dispatch(phase: &str, attempt: u32) -> RunRecord {
		RunRecord::Dispatch(PhaseDispatch {
			phase: Str::new(phase),
			attempt,
			inputs: Vec::<InputVersion>::new(),
			executor: Str::new("native"),
			posture: Str::new("workspace-write"),
			effects: Vec::<EffectKey>::new(),
		})
	}

	fn settle(phase: &str, attempt: u32, status: SettleStatus, feedback: &str) -> RunRecord {
		RunRecord::Settle(PhaseSettle {
			phase: Str::new(phase),
			attempt,
			status,
			feedback: Str::new(feedback),
		})
	}

	fn write_run(dir: &Path, run: &str, workflow: &str, created: &str, records: &[RunRecord]) {
		let mut log =
			RunLog::create(dir.join(format!("{run}.oms")), start(run, workflow, created)).unwrap();
		for record in records {
			log.append(record).unwrap();
		}
	}

	async fn read(resolver: &AdwResolver, resource: &str) -> Result<String, Fault> {
		resolver
			.read(resource, &ParsedSelector::None)
			.await
			.map(|bytes| String::from_utf8(bytes.to_vec()).unwrap())
	}

	#[tokio::test]
	async fn listing_shows_outcomes_newest_first_and_detail_shows_phases() {
		let temp = tempfile::tempdir().unwrap();
		write_run(temp.path(), "01AAAAAAAAAAAAAAAAAAAAAAAA", "ship", "2026-01-01T00:00:00Z", &[
			dispatch("build", 1),
			settle("build", 1, SettleStatus::Accepted, ""),
			RunRecord::Done(RunDone { accepted: true }),
		]);
		write_run(temp.path(), "01BBBBBBBBBBBBBBBBBBBBBBBB", "ship", "2026-01-02T00:00:00Z", &[
			dispatch("build", 1),
			settle("build", 1, SettleStatus::Rejected, "tests fail"),
			dispatch("build", 2),
			RunRecord::Halt(RunHalt { reason: Str::new("operator cancelled") }),
		]);
		write_run(temp.path(), "01CCCCCCCCCCCCCCCCCCCCCCCC", "other", "2026-01-03T00:00:00Z", &[
			dispatch("scan", 1),
		]);
		std::fs::write(temp.path().join("01DDDDDDDDDDDDDDDDDDDDDDDD.oms"), b"not a journal").unwrap();
		let resolver = AdwResolver::new(temp.path().to_path_buf());

		let listing = read(&resolver, "runs").await.unwrap();
		assert_eq!(listing, read(&resolver, "").await.unwrap());
		assert!(listing.contains("outcome: done\n"), "{listing}");
		assert!(listing.contains("outcome: halted: operator cancelled"), "{listing}");
		assert!(listing.contains("outcome: in progress"), "{listing}");
		assert!(listing.contains("Skipped 1 unreadable run log(s)"), "{listing}");
		let newest = listing.find("01CCCC").unwrap();
		let middle = listing.find("01BBBB").unwrap();
		let oldest = listing.find("01AAAA").unwrap();
		assert!(newest < middle && middle < oldest, "{listing}");

		let detail = read(&resolver, "01BBBBBBBBBBBBBBBBBBBBBBBB").await.unwrap();
		assert!(detail.contains("- Workflow: ship"), "{detail}");
		assert!(detail.contains("- Project: /work/project"), "{detail}");
		assert!(detail.contains("- Exec policy: sandboxed"), "{detail}");
		assert!(detail.contains("### build"), "{detail}");
		assert!(
			detail.contains(
				"attempt 1: dispatched to native (posture workspace-write) -> rejected: tests fail"
			),
			"{detail}"
		);
		assert!(
			detail
				.contains("attempt 2: dispatched to native (posture workspace-write) -> not settled"),
			"{detail}"
		);
		assert!(detail.contains("Halted: operator cancelled"), "{detail}");

		let entries = resolver.list("runs", 10, 4096).await.unwrap();
		assert_eq!(entries.entries.len(), 4);
		let completions = resolver.complete("01BB", 5).await.unwrap();
		assert_eq!(completions[0].value.as_str(), "adw://01BBBBBBBBBBBBBBBBBBBBBBBB");
	}

	#[tokio::test]
	async fn run_ids_that_leave_the_runs_directory_are_rejected() {
		let temp = tempfile::tempdir().unwrap();
		let resolver = AdwResolver::new(temp.path().to_path_buf());
		for resource in ["../x", "..", "a/b", "a\\b", ".hidden", "x..y"] {
			let fault = read(&resolver, resource).await.unwrap_err();
			assert!(matches!(fault, Fault::Invalid { .. }), "{resource}: {fault:?}");
			assert!(resolver.list(resource, 10, 1024).await.is_err(), "{resource}");
		}
		let missing = read(&resolver, "01ZZZZZZZZZZZZZZZZZZZZZZZZ")
			.await
			.unwrap_err();
		assert!(missing.message().contains("unknown ADW run"), "{missing:?}");
	}

	#[tokio::test]
	async fn missing_runs_directory_lists_as_empty() {
		let temp = tempfile::tempdir().unwrap();
		let resolver = AdwResolver::new(temp.path().join("absent"));
		assert!(
			read(&resolver, "runs")
				.await
				.unwrap()
				.contains("No ADW runs recorded.")
		);
	}
}
