//! N-API surface for the deterministic phase engine (`pi_tasks`).
//!
//! # Overview
//! One [`TaskRun`] per AI developer workflow. Rust owns sequencing, retries and
//! acceptance; TypeScript owns the models. The loop is:
//!
//! ```ignore
//! for (;;) {
//!   const step = run.nextStep();
//!   if (step.kind === TaskStepKind.Done) break;
//!   const turn = await agent(step.phase.owner, prompt, step.correction);
//!   run.submitAgentOutput(turn);
//! }
//! ```
//!
//! Nothing here serializes state to JSON: objects cross as N-API structs, and
//! the trace crosses as raw [`Buffer`] records that JS indexes with the offsets
//! from [`task_trace_layout`]. The only strings on the wire are the ones a
//! model must read — its own turn, and the correction fed back to it.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use napi::{Result, bindgen_prelude::*};
use napi_derive::napi;
use pi_tasks::{
	ArtifactsExist, Envelope, FilesNonEmpty, Gate, GateCtx, GateReport, Outcome, PhaseKind, PhaseParams, PhaseStatus,
	Run, Step, TraceReader, Tracer, Workflow, trace,
};
use pi_vcs::types::{StatusOptions, UntrackedMode};
use serde_json::Value;

/// Every path the working tree changed must be one the envelope declared.
///
/// `artifacts_exist` catches a claim with no file. This catches the opposite —
/// a file with no claim — which is the failure that actually hurts: an agent
/// that edited three files and confessed one leaves two changes nobody
/// reviewed. Existence checks cannot see them, because nothing was claimed.
///
/// It lives here rather than in `pi_tasks` because it needs git, and the engine
/// crate stays free of I/O beyond the filesystem.
struct DiffMatchesClaims {
	/// Paths that are already accounted for: dirty before the run started, plus
	/// everything earlier phases claimed. Shared across phases, so phase 2 is
	/// never blamed for the files phase 1 legitimately wrote.
	allowed: Arc<Mutex<HashSet<String>>>,
}

impl Gate for DiffMatchesClaims {
	fn name(&self) -> &'static str {
		"diff_matches_claims"
	}

	fn run(&self, envelope: &Envelope<Value>, ctx: &GateCtx<'_>) -> GateReport {
		let mut report = GateReport::new(self.name());

		// A gate that cannot gather evidence must not report "passed". Silence
		// here would read as approval of a diff nobody looked at.
		let repo = match pi_vcs::detect(ctx.root) {
			Ok(Some(repo)) => repo,
			Ok(None) => {
				report.push(".", false, "not a repository: the working tree cannot be compared");
				return report;
			},
			Err(err) => {
				report.push(".", false, format!("repository unreadable: {err}"));
				return report;
			},
		};

		// Untracked files individually: a brand-new file nobody declared is the
		// common case, and `git status` would otherwise collapse it into its
		// directory.
		let options = StatusOptions { untracked: UntrackedMode::All, pathspecs: Vec::new(), nul_terminated: true };
		let porcelain = match repo.status_porcelain(&options) {
			Ok(text) => text,
			Err(err) => {
				report.push(".", false, format!("status failed: {err}"));
				return report;
			},
		};

		let Ok(mut allowed) = self.allowed.lock() else {
			report.push(".", false, "gate state poisoned");
			return report;
		};
		allowed.extend(envelope.artifacts.iter().map(|a| normalize(a)));

		let mut undeclared = 0usize;
		for path in changed_paths(&porcelain) {
			if allowed.contains(&path) {
				continue;
			}
			undeclared += 1;
			report.push(path, false, "changed but not declared in artifacts");
		}
		if undeclared == 0 {
			// Names what it examined, so a clean gate is evidence and not silence.
			report.push(".", true, format!("{} declared path(s) cover every change", allowed.len()));
		}
		report
	}
}

/// Repo-relative paths from `git status --porcelain -z`.
///
/// Each record is `XY <path>`; a rename adds a second NUL-separated record
/// holding the source, which is a real change to a real path and is returned
/// too — a file moved out from under a claim is exactly what this gate is for.
fn changed_paths(porcelain: &str) -> Vec<String> {
	let mut paths = Vec::new();
	for record in porcelain.split('\0') {
		if record.is_empty() {
			continue;
		}
		// A rename's source record carries no status prefix.
		let path = if record.len() > 3 && record.as_bytes()[2] == b' ' { &record[3..] } else { record };
		if !path.is_empty() {
			paths.push(normalize(path));
		}
	}
	paths
}

/// `./docs/x.md` and `docs/x.md` are the same claim.
fn normalize(path: &str) -> String {
	path.trim_start_matches("./").trim_end_matches('/').to_owned()
}

fn fail(err: impl std::fmt::Display) -> napi::Error {
	napi::Error::from_reason(err.to_string())
}

/// Three lanes: only `Agent` costs tokens; `Code` and `Engineer` are executed
/// by the caller and reported back through the same door.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[napi]
pub enum TaskPhaseKind {
	Engineer = 0,
	Agent    = 1,
	Code     = 2,
}

const fn to_core_kind(kind: TaskPhaseKind) -> PhaseKind {
	match kind {
		TaskPhaseKind::Engineer => PhaseKind::Engineer,
		TaskPhaseKind::Agent => PhaseKind::Agent,
		TaskPhaseKind::Code => PhaseKind::Code,
	}
}

const fn to_napi_kind(kind: PhaseKind) -> TaskPhaseKind {
	match kind {
		PhaseKind::Engineer => TaskPhaseKind::Engineer,
		PhaseKind::Agent => TaskPhaseKind::Agent,
		PhaseKind::Code => TaskPhaseKind::Code,
	}
}

/// One phase of a workflow. `owner` names an agent from the roster or a
/// subsystem (`git`, `bun`) — never a model id.
#[napi(object)]
pub struct TaskPhaseSpec {
	pub name:        String,
	pub kind:        TaskPhaseKind,
	pub owner:       String,
	pub description: Option<String>,
}

/// Acceptance gates for one phase, named so the workflow stays declarative.
#[napi(object)]
pub struct TaskPhaseGates {
	pub phase: String,
	/// `"artifacts_exist"` · `"files_non_empty"`.
	pub gates: Vec<String>,
}

#[napi(object)]
pub struct TaskRunOptions {
	pub adw_id:       String,
	/// Repository root that gates resolve claimed paths against.
	pub root:         String,
	pub workflow:     String,
	pub phases:       Vec<TaskPhaseSpec>,
	/// Directory for `events.bin` / `strings.bin`. Omit to run untraced.
	pub trace_dir:    Option<String>,
	/// Attempts per phase before the run halts. Default 3, minimum 1.
	pub max_attempts: Option<u32>,
	pub gates:        Option<Vec<TaskPhaseGates>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[napi]
pub enum TaskStepKind {
	Run  = 0,
	Done = 1,
}

/// What the caller must do next. `Run` carries the phase and, from attempt 2
/// on, the correction explaining why the previous attempt was rejected; `Done`
/// carries the verdict. Reusing the agent's session across attempts is a
/// driver's choice, not a requirement — but the correction must reach the
/// retry either way, since it is the only record of what was wrong.
#[napi(object)]
pub struct TaskStep {
	pub kind:       TaskStepKind,
	pub phase:      Option<TaskPhaseSpec>,
	/// 1-based; `> 1` means the previous attempt was rejected.
	pub attempt:    u32,
	pub correction: Option<String>,
	pub accepted:   bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[napi]
pub enum TaskOutcomeKind {
	Advanced = 0,
	Retry    = 1,
	Aborted  = 2,
}

#[napi(object)]
pub struct TaskOutcome {
	pub kind:       TaskOutcomeKind,
	pub phase:      String,
	/// The attempt to run next when `kind` is `Retry`.
	pub attempt:    u32,
	pub correction: Option<String>,
	/// Why the run halted when `kind` is `Aborted`.
	pub reason:     Option<String>,
}

/// The last accepted envelope — what the next phase's prompt is built from.
/// Context crosses phases here, in code, not in conversation.
#[napi(object)]
pub struct TaskHandoff {
	pub summary:              String,
	pub artifacts:            Vec<String>,
	pub notes_for_next_agent: String,
	/// Phase-specific fields the agent returned alongside the contract
	/// (`changed_files`, `commit_message`, …), as JSON text.
	pub payload_json:         String,
}

#[napi(object)]
pub struct TaskPhaseResult {
	pub name:          String,
	pub kind:          TaskPhaseKind,
	pub owner:         String,
	pub passed:        bool,
	pub attempts:      u32,
	pub summary:       String,
	/// Why the phase was rejected — gate violations plus non-gate ones such as
	/// a self-reported `fail`. Empty when it passed.
	pub violations: Vec<String>,
}

#[napi(object)]
pub struct TaskRunSummary {
	pub adw_id:   String,
	pub workflow: String,
	pub accepted: bool,
	pub reason:   String,
	pub phases:   Vec<TaskPhaseResult>,
}

/// Gate names this engine can build. The single source of truth for a caller
/// that validates a workflow file before starting a run.
#[napi]
pub fn task_gate_names() -> Vec<String> {
	vec!["artifacts_exist".to_owned(), "files_non_empty".to_owned(), "diff_matches_claims".to_owned()]
}

/// `allowed` is the run-wide set every `diff_matches_claims` instance shares:
/// one gate per phase, but a change declared by phase 1 must not be undeclared
/// for phase 2.
fn build_gate(name: &str, allowed: &Arc<Mutex<HashSet<String>>>) -> Result<Box<dyn Gate>> {
	match name {
		"artifacts_exist" => Ok(Box::new(ArtifactsExist)),
		"files_non_empty" => Ok(Box::new(FilesNonEmpty)),
		"diff_matches_claims" => Ok(Box::new(DiffMatchesClaims { allowed: Arc::clone(allowed) })),
		other => Err(napi::Error::from_reason(format!(
			"unknown gate {other:?}: expected one of {}",
			task_gate_names().join(", ")
		))),
	}
}

/// The working tree's dirt before the run touches anything.
///
/// Whatever is already changed belongs to the operator, not to the agent, and
/// must not be blamed on whichever phase happens to carry the gate. On resume
/// this naturally re-admits the work earlier phases already landed.
///
/// An unreadable or non-git tree yields an empty set rather than a failure:
/// the gate reports that condition itself when it runs, where it belongs, so a
/// workflow without this gate is never blocked by a missing repository.
fn initial_allowed(root: &str) -> Arc<Mutex<HashSet<String>>> {
	let options = StatusOptions { untracked: UntrackedMode::All, pathspecs: Vec::new(), nul_terminated: true };
	let dirty = pi_vcs::detect(std::path::Path::new(root))
		.ok()
		.flatten()
		.and_then(|repo| repo.status_porcelain(&options).ok())
		.map(|text| changed_paths(&text).into_iter().collect::<HashSet<_>>())
		.unwrap_or_default();
	Arc::new(Mutex::new(dirty))
}

fn to_napi_step(step: Step) -> TaskStep {
	match step {
		Step::Run { phase, attempt, correction } => TaskStep {
			kind: TaskStepKind::Run,
			phase: Some(TaskPhaseSpec {
				name:        phase.name,
				kind:        to_napi_kind(phase.kind),
				owner:       phase.owner,
				description: Some(phase.description).filter(|d| !d.is_empty()),
			}),
			attempt,
			correction,
			accepted: false,
		},
		Step::Done { accepted } => TaskStep {
			kind: TaskStepKind::Done,
			phase: None,
			attempt: 0,
			correction: None,
			accepted,
		},
	}
}

fn to_napi_outcome(outcome: Outcome) -> TaskOutcome {
	match outcome {
		Outcome::Advanced { phase } => TaskOutcome {
			kind:       TaskOutcomeKind::Advanced,
			phase,
			attempt:    0,
			correction: None,
			reason:     None,
		},
		Outcome::Retry { phase, attempt, correction } => TaskOutcome {
			kind:       TaskOutcomeKind::Retry,
			phase,
			attempt,
			correction: Some(correction),
			reason:     None,
		},
		Outcome::Aborted { phase, reason } => TaskOutcome {
			kind:       TaskOutcomeKind::Aborted,
			phase,
			attempt:    0,
			correction: None,
			reason:     Some(reason),
		},
	}
}

/// A driven workflow run. Every method is synchronous and cheap: the engine
/// decides, it never waits on a model.
#[napi]
pub struct TaskRun {
	inner:     Run,
	trace_dir: Option<String>,
}

fn to_core_phases(specs: Vec<TaskPhaseSpec>) -> Vec<PhaseParams> {
	specs
		.into_iter()
		.map(|spec| {
			let params = PhaseParams::new(spec.name, to_core_kind(spec.kind), spec.owner);
			match spec.description {
				Some(text) => params.describe(text),
				None => params,
			}
		})
		.collect()
}

#[napi]
impl TaskRun {
	/// Rebuild a run that died mid-flight from its own trace, continuing at the
	/// phase that was in flight with the attempts it had already spent.
	/// Requires `traceDir`; refuses a trace from another workflow or a run that
	/// already finished.
	#[napi(factory)]
	pub fn resume(options: TaskRunOptions) -> Result<Self> {
		let Some(trace_dir) = options.trace_dir.clone() else {
			return Err(napi::Error::from_reason("resume requires traceDir"));
		};
		let phases = to_core_phases(options.phases);
		let workflow = Workflow::new(options.workflow.clone(), phases);
		// `Run::resume` opens the trace itself so it can mark the continuation.
		let mut run = Run::resume(options.adw_id.clone(), &options.root, workflow, &trace_dir).map_err(fail)?;
		if let Some(attempts) = options.max_attempts {
			run = run.with_max_attempts(attempts);
		}
		let allowed = initial_allowed(&options.root);
		for entry in options.gates.unwrap_or_default() {
			let gates = entry.gates.iter().map(|name| build_gate(name, &allowed)).collect::<Result<Vec<_>>>()?;
			run.register_gates(&entry.phase, gates);
		}
		Ok(Self { inner: run, trace_dir: Some(trace_dir) })
	}

	#[napi(constructor)]
	pub fn new(options: TaskRunOptions) -> Result<Self> {
		let phases = to_core_phases(options.phases);

		let mut run = Run::new(options.adw_id, &options.root, Workflow::new(options.workflow, phases));
		if let Some(dir) = &options.trace_dir {
			run = run.with_tracer(Tracer::create(dir).map_err(fail)?);
		}
		if let Some(attempts) = options.max_attempts {
			run = run.with_max_attempts(attempts);
		}
		let allowed = initial_allowed(&options.root);
		for entry in options.gates.unwrap_or_default() {
			let gates = entry.gates.iter().map(|name| build_gate(name, &allowed)).collect::<Result<Vec<_>>>()?;
			run.register_gates(&entry.phase, gates);
		}
		Ok(Self { inner: run, trace_dir: options.trace_dir })
	}

	/// What to run next, and the correction to carry if the last attempt was
	/// rejected.
	#[napi]
	pub fn next_step(&mut self) -> Result<TaskStep> {
		self.inner.next_step().map(to_napi_step).map_err(fail)
	}

	/// Hand back an agent's raw final turn. Output that does not parse is a
	/// correction, not a throw: the same session is asked again.
	#[napi]
	pub fn submit_agent_output(&mut self, text: String) -> Result<TaskOutcome> {
		self.inner.submit_agent_output(&text).map(to_napi_outcome).map_err(fail)
	}

	/// Report a deterministic `Code` or human `Engineer` phase. A red test
	/// suite reaches the next agent as an envelope like any other.
	#[napi]
	pub fn submit_code_result(&mut self, ok: bool, summary: String) -> Result<TaskOutcome> {
		self.inner.submit_envelope(Envelope::code(ok, summary)).map(to_napi_outcome).map_err(fail)
	}

	/// Records one fusion-panel member's answer against the active phase.
	/// `tokens` is what makes two models comparable in the trace.
	#[napi]
	pub fn note_panel_opinion(&self, owner: String, ok: bool, tokens: u32) -> Result<()> {
		self.inner.note_panel_opinion(&owner, ok, tokens).map_err(fail)
	}

	/// Records what the attempt in flight cost. Charged per attempt: a rejected
	/// try spent real tokens, and a phase that needed three of them is the one
	/// a cost report has to show.
	#[napi]
	pub fn note_phase_tokens(&self, owner: String, tokens: u32) -> Result<()> {
		self.inner.note_phase_tokens(&owner, tokens).map_err(fail)
	}

	/// The last accepted envelope, for building the next phase's prompt.
	#[napi]
	pub fn handoff(&self) -> Option<TaskHandoff> {
		self.inner.previous_envelope().map(|env| TaskHandoff {
			summary:              env.summary.clone(),
			artifacts:            env.artifacts.clone(),
			notes_for_next_agent: env.notes_for_next_agent.clone(),
			payload_json:         env.payload.to_string(),
		})
	}

	/// Settles the second question: phases passing is not the same as the
	/// result being acceptable.
	#[napi]
	pub fn finish(&mut self, accepted: bool, reason: Option<String>) -> Result<TaskRunSummary> {
		let summary = self.inner.finish(accepted, reason.unwrap_or_default()).map_err(fail)?;
		Ok(TaskRunSummary {
			adw_id:   summary.adw_id,
			workflow: summary.workflow,
			accepted: summary.accepted,
			reason:   summary.reason,
			phases:   summary
				.records
				.into_iter()
				.map(|record| TaskPhaseResult {
					name:          record.name,
					kind:          to_napi_kind(record.kind),
					owner:         record.owner,
					passed:        record.status == PhaseStatus::Passed,
					attempts:      record.attempts,
					summary:       record.summary,
					violations:    record.violations,
				})
				.collect(),
		})
	}

	/// The effective per-phase attempt budget after clamping. A driver bounding
	/// its own loop reads this instead of re-hardcoding the default.
	#[napi(getter)]
	pub fn max_attempts(&self) -> u32 {
		self.inner.max_attempts()
	}

	/// Where this run's binary trace lives, if it is traced.
	#[napi(getter)]
	pub fn trace_dir(&self) -> Option<String> {
		self.trace_dir.clone()
	}
}

/// Byte layout of one trace record. A JS reader indexes [`TaskTraceReader::read_raw`]
/// with these instead of hardcoding them, so a format bump cannot go unnoticed.
#[napi(object)]
pub struct TaskTraceLayout {
	pub header_len:     u32,
	pub record_len:     u32,
	pub off_ts:         u32,
	pub off_kind:       u32,
	pub off_flags:      u32,
	pub off_attempt:    u32,
	pub off_phase:      u32,
	pub off_owner:      u32,
	pub off_gate:       u32,
	pub off_detail:     u32,
	pub off_value:      u32,
	/// Mask of the `ok` bit inside the flags byte.
	pub flag_ok:        u32,
	pub events_magic:   u32,
	pub strings_magic:  u32,
	pub format_version: u32,
	/// Event kind names indexed by the record's kind byte; index 0 is unused.
	pub kind_names:     Vec<String>,
}

#[napi]
pub fn task_trace_layout() -> TaskTraceLayout {
	TaskTraceLayout {
		header_len:     trace::HEADER_LEN as u32,
		record_len:     trace::RECORD_LEN as u32,
		off_ts:         trace::OFF_TS as u32,
		off_kind:       trace::OFF_KIND as u32,
		off_flags:      trace::OFF_FLAGS as u32,
		off_attempt:    trace::OFF_ATTEMPT as u32,
		off_phase:      trace::OFF_PHASE as u32,
		off_owner:      trace::OFF_OWNER as u32,
		off_gate:       trace::OFF_GATE as u32,
		off_detail:     trace::OFF_DETAIL as u32,
		off_value:      trace::OFF_VALUE as u32,
		flag_ok:        1,
		events_magic:   trace::EVENTS_MAGIC,
		strings_magic:  trace::STRINGS_MAGIC,
		format_version: u32::from(trace::FORMAT_VERSION),
		kind_names:     [
			"",
			"run_started",
			"phase_started",
			"phase_retry",
			"gate_check",
			"phase_rejected",
			"phase_finished",
			"run_finished",
			"panel_opinion",
			"run_resumed",
			"phase_tokens",
		]
		.map(String::from)
		.to_vec(),
	}
}

/// Tails a run's binary trace. Records cross as raw bytes — Rust builds no
/// per-event object and JS parses no text.
#[napi]
pub struct TaskTraceReader {
	inner: TraceReader,
}

#[napi]
impl TaskTraceReader {
	#[napi(constructor)]
	pub fn new(dir: String) -> Result<Self> {
		Ok(Self { inner: TraceReader::open(dir).map_err(fail)? })
	}

	/// Complete records currently on disk — the exclusive upper bound of the
	/// next `readRaw` cursor.
	#[napi]
	pub fn count(&self) -> Result<u32> {
		self.inner.count().map(|count| count as u32).map_err(fail)
	}

	/// Every complete record from `fromSeq` onward, packed at
	/// [`TaskTraceLayout::record_len`] stride. A partially written trailing
	/// record is left for the next call.
	#[napi]
	pub fn read_raw(&mut self, from_seq: u32) -> Result<Buffer> {
		self.inner.read_raw(u64::from(from_seq)).map(Buffer::from).map_err(fail)
	}

	/// The interned string table in id order: `strings()[id - 1]`, id 0 means
	/// absent. Call after `readRaw`, which refreshes it.
	#[napi]
	pub fn strings(&self) -> Vec<String> {
		self.inner.strings().to_vec()
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn porcelain_records_yield_repo_relative_paths() {
		// `-z` output: status prefix, path, NUL. A rename adds a second record
		// carrying the source with no prefix.
		let porcelain = " M src/edited.ts\0?? src/brand-new.ts\0R  docs/new.md\0docs/old.md\0";
		assert_eq!(
			changed_paths(porcelain),
			vec!["src/edited.ts", "src/brand-new.ts", "docs/new.md", "docs/old.md"],
			"an untracked file and a rename's source are both real changes"
		);
	}

	#[test]
	fn trailing_separator_does_not_produce_an_empty_path() {
		// An empty path would be allowed by any claim set and silently swallow
		// a real change.
		assert!(changed_paths("\0\0").is_empty());
		assert_eq!(changed_paths(" M a.ts\0").len(), 1);
	}

	#[test]
	fn a_claim_is_matched_regardless_of_dot_slash_prefix() {
		assert_eq!(normalize("./docs/x.md"), normalize("docs/x.md"));
	}

	#[test]
	fn a_gate_with_no_repository_fails_instead_of_passing_quietly() {
		let dir = std::env::temp_dir().join(format!("pi-natives-gate-{}", std::process::id()));
		std::fs::create_dir_all(&dir).expect("temp dir");
		let gate = DiffMatchesClaims { allowed: Arc::new(Mutex::new(HashSet::new())) };
		let envelope = Envelope::code(true, "done");
		let report = gate.run(&envelope, &GateCtx { root: &dir });
		let _ = std::fs::remove_dir_all(&dir);

		assert!(!report.ok(), "a gate that cannot read a diff must not report passed");
		assert!(
			report.violations().any(|v| v.contains("not a repository")),
			"the violation has to name why it could not check, not just fail"
		);
	}
}
