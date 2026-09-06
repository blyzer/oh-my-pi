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

use napi::{Result, bindgen_prelude::*};
use napi_derive::napi;
use pi_tasks::{
	ArtifactsExist, Envelope, FilesNonEmpty, Gate, Outcome, PhaseKind, PhaseParams, PhaseStatus, Run, Step,
	TraceReader, Tracer, Workflow, trace,
};

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
	vec!["artifacts_exist".to_owned(), "files_non_empty".to_owned()]
}

fn build_gate(name: &str) -> Result<Box<dyn Gate>> {
	match name {
		"artifacts_exist" => Ok(Box::new(ArtifactsExist)),
		"files_non_empty" => Ok(Box::new(FilesNonEmpty)),
		other => Err(napi::Error::from_reason(format!(
			"unknown gate {other:?}: expected \"artifacts_exist\" or \"files_non_empty\""
		))),
	}
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
		for entry in options.gates.unwrap_or_default() {
			let gates = entry.gates.iter().map(|name| build_gate(name)).collect::<Result<Vec<_>>>()?;
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
		for entry in options.gates.unwrap_or_default() {
			let gates = entry.gates.iter().map(|name| build_gate(name)).collect::<Result<Vec<_>>>()?;
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
