//! The run: a driven state machine that owns sequencing, retries and
//! acceptance.
//!
//! Rust decides WHAT runs next and WHETHER it counts; the caller (the
//! TypeScript agent layer, which owns the model providers) executes the step
//! and reports back. That split is the whole point: an agent never decides it
//! is done, and a correction never costs a cold restart.
//!
//! ```text
//! loop { match run.next_step()? {
//!     Step::Run { phase, correction, .. } => run.submit_agent_output(&model_turn)?,
//!     Step::Done { accepted }             => break,
//! } }
//! ```
//!
//! Steps and outcomes cross the napi boundary as structs (V8 accessors, no
//! JSON), and observability goes to the binary log in [`crate::trace`] — text
//! survives only where a model must read it: the correction prompt.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::Value;

use crate::envelope::{Envelope, EnvelopeError};
use crate::gate::{Gate, GateCtx, GateReport};
use crate::phase::{PhaseParams, PhaseRecord, PhaseStatus};
use crate::trace::{EventKind, EventRecord, TraceReader, Tracer};

/// Accepted envelopes live beside the trace, one file per phase.
const ENVELOPE_DIR: &str = "envelopes";

/// Phase names are user-authored; keep them from escaping the envelope dir.
fn sanitize(phase: &str) -> String {
	phase.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect()
}

fn load_envelope(trace_dir: &Path, phase: &str) -> Result<Option<Envelope<Value>>, RunError> {
	let path = trace_dir.join(ENVELOPE_DIR).join(format!("{}.json", sanitize(phase)));
	match std::fs::read(&path) {
		Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes).map_err(std::io::Error::other)?)),
		// A trace written before envelopes were persisted, or a phase whose file
		// was pruned: resumable, just without that handoff.
		Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
		Err(err) => Err(RunError::Trace(err)),
	}
}

const DEFAULT_MAX_ATTEMPTS: u32 = 3;

#[derive(Debug, Clone, Serialize)]
pub struct Workflow {
	pub name: String,
	pub phases: Vec<PhaseParams>,
}

impl Workflow {
	pub fn new(name: impl Into<String>, phases: Vec<PhaseParams>) -> Self {
		Self { name: name.into(), phases }
	}
}

/// What the caller must do next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
	Run {
		phase: PhaseParams,
		/// 1-based; `> 1` means the previous attempt was rejected.
		attempt: u32,
		/// Verbatim feedback for the retry. The engine does not require the
		/// caller to reuse the agent's session — a driver that re-dispatches a
		/// fresh one must pass this through, because it is the only record of
		/// what was wrong.
		correction: Option<String>,
	},
	Done {
		accepted: bool,
	},
}

/// What the run decided about a submitted result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
	Advanced { phase: String },
	Retry { phase: String, attempt: u32, correction: String },
	Aborted { phase: String, reason: String },
}

#[derive(Debug, thiserror::Error)]
pub enum RunError {
	#[error("no active step: the run is finished or halted")]
	NoActiveStep,
	/// The trace does not describe the workflow it is being resumed against.
	#[error("cannot resume: {0}")]
	Mismatch(String),
	#[error("trace write failed: {0}")]
	Trace(#[from] std::io::Error),
}

#[derive(Debug, Clone, Serialize)]
pub struct RunSummary {
	pub adw_id: String,
	pub workflow: String,
	pub accepted: bool,
	pub reason: String,
	pub records: Vec<PhaseRecord>,
}

pub struct Run {
	adw_id: String,
	root: PathBuf,
	workflow: Workflow,
	tracer: Option<Tracer>,
	max_attempts: u32,
	gates: HashMap<String, Vec<Box<dyn Gate>>>,
	cursor: usize,
	attempts: u32,
	pending_correction: Option<String>,
	last_envelope: Option<Envelope<Value>>,
	records: Vec<PhaseRecord>,
	halted: bool,
	started: bool,
}

impl Run {
	pub fn new(adw_id: impl Into<String>, root: impl Into<PathBuf>, workflow: Workflow) -> Self {
		Self {
			adw_id: adw_id.into(),
			root: root.into(),
			workflow,
			tracer: None,
			max_attempts: DEFAULT_MAX_ATTEMPTS,
			gates: HashMap::new(),
			cursor: 0,
			attempts: 0,
			pending_correction: None,
			last_envelope: None,
			records: Vec::new(),
			halted: false,
			started: false,
		}
	}

	pub fn with_tracer(mut self, tracer: Tracer) -> Self {
		self.tracer = Some(tracer);
		self
	}

	/// Attempts per phase before the run halts. Clamped to at least one.
	pub fn with_max_attempts(mut self, attempts: u32) -> Self {
		self.max_attempts = attempts.max(1);
		self
	}

	/// Rebuild a run from its own trace.
	///
	/// Everything but the handoff is derived from the event stream: the cursor is
	/// the count of passed phases, the attempt tally is the rejections recorded
	/// against the phase that was in flight. The workflow must be the one that
	/// produced the trace — resuming a plan against a different pipeline would
	/// replay decisions that were never made about these phases.
	pub fn resume(
		adw_id: impl Into<String>,
		root: impl Into<PathBuf>,
		workflow: Workflow,
		trace_dir: impl AsRef<Path>,
	) -> Result<Self, RunError> {
		let dir = trace_dir.as_ref();
		let mut reader = TraceReader::open(dir)?;
		let events = reader.read_from(0)?;

		if let Some(started) = events.iter().find(|event| event.kind == EventKind::RunStarted) {
			let traced = reader.text(started.detail);
			if traced != workflow.name {
				return Err(RunError::Mismatch(format!(
					"trace belongs to workflow {traced:?}, not {:?}",
					workflow.name
				)));
			}
		}
		// A crash writes a terminal record too — the driver closes the trace so a
		// reader can tell a dead run from a running one. Only an ACCEPTED run has
		// nothing left to continue.
		if events.iter().any(|event| event.kind == EventKind::RunFinished && event.ok) {
			return Err(RunError::Mismatch("this run was already accepted".to_owned()));
		}

		let mut run = Self::new(adw_id, root, workflow);
		run.started = true;
		for event in &events {
			match event.kind {
				EventKind::PhaseFinished if event.ok => {
					let name = reader.text(event.phase).to_owned();
					let Some(phase) = run.workflow.phases.get(run.cursor) else {
						return Err(RunError::Mismatch(format!("trace has more phases than workflow {:?}", run.workflow.name)));
					};
					if phase.name != name {
						return Err(RunError::Mismatch(format!(
							"trace expects phase {name:?} at position {}, workflow has {:?}",
							run.cursor, phase.name
						)));
					}
					let envelope = load_envelope(dir, &name)?;
					run.records.push(PhaseRecord {
						name: name.clone(),
						kind: phase.kind,
						owner: phase.owner.clone(),
						status: PhaseStatus::Passed,
						attempts: u32::from(event.attempt),
						summary: envelope.as_ref().map_or_else(String::new, |env| env.summary.clone()),
						// The gate evidence stays in the trace; re-synthesizing it
						// here would invent checks nobody ran.
						gates: Vec::new(),
						violations: Vec::new(),
					});
					if let Some(env) = envelope {
						run.last_envelope = Some(env);
					}
					run.cursor += 1;
					run.attempts = 0;
				}
				// A phase that exhausted its budget halted the run; the trace ends
				// there and there is nothing left to resume into.
				EventKind::PhaseFinished => run.halted = true,
				EventKind::PhaseRejected => run.attempts = u32::from(event.attempt),
				_ => {}
			}
		}

		// Resume owns the trace it continues: the marker tells a reader that the
		// records below belong to a continuation, so the terminal record above
		// them is not the end.
		let tracer = Tracer::create(dir)?;
		let workflow_id = tracer.intern(&run.workflow.name)?;
		tracer.emit(EventRecord::new(EventKind::RunResumed).detail(workflow_id).value(run.cursor as u32))?;
		run.tracer = Some(tracer);
		Ok(run)
	}

	pub fn register_gates(&mut self, phase: &str, gates: Vec<Box<dyn Gate>>) {
		self.gates.insert(phase.to_owned(), gates);
	}

	pub fn adw_id(&self) -> &str {
		&self.adw_id
	}

	pub fn root(&self) -> &Path {
		&self.root
	}

	pub fn records(&self) -> &[PhaseRecord] {
		&self.records
	}

	/// The effective per-phase attempt budget, after clamping. A driver that
	/// bounds its own loop must read it here rather than re-hardcoding the
	/// default, or the two disagree the moment this one changes.
	pub const fn max_attempts(&self) -> u32 {
		self.max_attempts
	}

	/// The last accepted envelope — the handoff the next phase's prompt is
	/// built from. Context crosses phases here, in code, not in conversation.
	pub fn previous_envelope(&self) -> Option<&Envelope<Value>> {
		self.last_envelope.as_ref()
	}

	pub fn next_step(&mut self) -> Result<Step, RunError> {
		if !self.started {
			self.started = true;
			let workflow = self.intern(&self.workflow.name)?;
			self.trace(EventRecord::new(EventKind::RunStarted).detail(workflow))?;
		}
		let Some(phase) = self.active_phase().cloned() else {
			return Ok(Step::Done { accepted: !self.halted });
		};
		let attempt = self.attempts + 1;
		let kind = if attempt == 1 { EventKind::PhaseStarted } else { EventKind::PhaseRetry };
		let phase_id = self.intern(&phase.name)?;
		let owner_id = self.intern(&phase.owner)?;
		self.trace(EventRecord::new(kind).phase(phase_id).owner(owner_id).attempt(attempt))?;
		Ok(Step::Run { phase, attempt, correction: self.pending_correction.clone() })
	}

	/// Submit an agent's raw final turn. Output that does not parse is a
	/// correction, not a crash: the agent is asked again in the same session.
	pub fn submit_agent_output(&mut self, text: &str) -> Result<Outcome, RunError> {
		if self.active_phase().is_none() {
			return Err(RunError::NoActiveStep);
		}
		match Envelope::from_agent_text(text) {
			Ok(envelope) => self.submit_envelope(envelope),
			Err(err) => {
				let violation = match err {
					EnvelopeError::NoJson => "envelope: the turn contained no JSON object".to_owned(),
					EnvelopeError::Invalid(e) => format!("envelope: {e}"),
				};
				self.reject(vec![violation], Vec::new(), "unparseable envelope".to_owned())
			}
		}
	}

	/// Submit a result directly — deterministic `Code` phases and human
	/// `Engineer` phases report through the same door, so their failures reach
	/// the next agent as an envelope like any other.
	pub fn submit_envelope(&mut self, envelope: Envelope<Value>) -> Result<Outcome, RunError> {
		let Some(phase) = self.active_phase().cloned() else {
			return Err(RunError::NoActiveStep);
		};

		let reports: Vec<GateReport> = {
			let ctx = GateCtx { root: &self.root };
			self.gates
				.get(&phase.name)
				.map(|gates| gates.iter().map(|g| g.run(&envelope, &ctx)).collect())
				.unwrap_or_default()
		};

		let phase_id = self.intern(&phase.name)?;
		for report in &reports {
			let gate_id = self.intern(&report.gate)?;
			for check in &report.checks {
				let item_id = self.intern(&check.item)?;
				self.trace(
					EventRecord::new(EventKind::GateCheck)
						.phase(phase_id)
						.gate(gate_id)
						.detail(item_id)
						.ok(check.ok),
				)?;
			}
		}

		let mut violations: Vec<String> = reports.iter().flat_map(GateReport::violations).collect();
		if !envelope.is_success() {
			// Not "the agent": a code phase reports through this same path and has
			// no agent to blame.
			violations.push(format!("status: reported fail — {}", envelope.summary));
		}

		if violations.is_empty() {
			self.attempts += 1;
			let attempts = self.attempts;
			let summary = envelope.summary.clone();
			self.records.push(PhaseRecord {
				name: phase.name.clone(),
				kind: phase.kind,
				owner: phase.owner.clone(),
				status: PhaseStatus::Passed,
				attempts,
				summary,
				gates: reports,
				violations: Vec::new(),
			});
			self.trace(
				EventRecord::new(EventKind::PhaseFinished).phase(phase_id).attempt(attempts).ok(true),
			)?;
			// The one thing the trace cannot reconstruct. Every other resume input
			// is derivable from the event stream; the handoff the next phase's
			// prompt is built from exists only here.
			self.persist_envelope(&phase.name, &envelope)?;
			self.last_envelope = Some(envelope);
			self.cursor += 1;
			self.attempts = 0;
			self.pending_correction = None;
			return Ok(Outcome::Advanced { phase: phase.name });
		}

		self.reject(violations, reports, envelope.summary)
	}

	/// Records one panel member's answer inside the active phase.
	///
	/// A fusion phase is still ONE phase to the engine — it settles on the
	/// fuser's envelope — but the members that fed it are real work with real
	/// cost, and `value` (tokens) is what makes two models comparable.
	pub fn note_panel_opinion(&self, owner: &str, ok: bool, value: u32) -> Result<(), RunError> {
		let phase = self.active_phase().ok_or(RunError::NoActiveStep)?;
		let phase_id = self.intern(&phase.name)?;
		let owner_id = self.intern(owner)?;
		self.trace(
			EventRecord::new(EventKind::PanelOpinion).phase(phase_id).owner(owner_id).ok(ok).value(value),
		)
	}

	/// Settles the run's second question: phases passing is not the same as the
	/// result being acceptable.
	pub fn finish(&mut self, accepted: bool, reason: impl Into<String>) -> Result<RunSummary, RunError> {
		let reason = reason.into();
		let reason_id = self.intern(&reason)?;
		self.trace(EventRecord::new(EventKind::RunFinished).detail(reason_id).ok(accepted))?;
		Ok(RunSummary {
			adw_id: self.adw_id.clone(),
			workflow: self.workflow.name.clone(),
			accepted,
			reason,
			records: self.records.clone(),
		})
	}

	/// Writes the accepted envelope beside the trace, keyed by phase name.
	fn persist_envelope(&self, phase: &str, envelope: &Envelope<Value>) -> Result<(), RunError> {
		let Some(tracer) = &self.tracer else { return Ok(()) };
		let dir = tracer.dir().join(ENVELOPE_DIR);
		std::fs::create_dir_all(&dir)?;
		let json = serde_json::to_vec_pretty(envelope).map_err(std::io::Error::other)?;
		std::fs::write(dir.join(format!("{}.json", sanitize(phase))), json)?;
		Ok(())
	}

	fn active_phase(&self) -> Option<&PhaseParams> {
		if self.halted { None } else { self.workflow.phases.get(self.cursor) }
	}

	fn reject(
		&mut self,
		violations: Vec<String>,
		reports: Vec<GateReport>,
		summary: String,
	) -> Result<Outcome, RunError> {
		let phase = self.active_phase().cloned().ok_or(RunError::NoActiveStep)?;
		self.attempts += 1;
		let attempts = self.attempts;
		let phase_id = self.intern(&phase.name)?;
		let summary_id = self.intern(&summary)?;

		if attempts >= self.max_attempts {
			let reason = violations.join("; ");
			let count = violations.len() as u32;
			self.records.push(PhaseRecord {
				name: phase.name.clone(),
				kind: phase.kind,
				owner: phase.owner.clone(),
				status: PhaseStatus::Failed,
				attempts,
				summary,
				gates: reports,
				violations,
			});
			self.halted = true;
			self.trace(
				EventRecord::new(EventKind::PhaseFinished)
					.phase(phase_id)
					.attempt(attempts)
					.detail(summary_id)
					.value(count),
			)?;
			return Ok(Outcome::Aborted { phase: phase.name, reason });
		}

		self.trace(
			EventRecord::new(EventKind::PhaseRejected)
				.phase(phase_id)
				.attempt(attempts)
				.detail(summary_id)
				.value(violations.len() as u32),
		)?;
		let correction = correction_text(&phase.name, attempts + 1, self.max_attempts, &violations);
		self.pending_correction = Some(correction.clone());
		Ok(Outcome::Retry { phase: phase.name, attempt: attempts + 1, correction })
	}

	fn intern(&self, text: &str) -> Result<u32, RunError> {
		match &self.tracer {
			Some(tracer) => Ok(tracer.intern(text)?),
			None => Ok(crate::trace::NO_STRING),
		}
	}

	fn trace(&self, record: EventRecord) -> Result<(), RunError> {
		if let Some(tracer) = &self.tracer {
			tracer.emit(record)?;
		}
		Ok(())
	}
}

fn correction_text(phase: &str, next_attempt: u32, max_attempts: u32, violations: &[String]) -> String {
	let mut text = format!(
		"Your result for phase `{phase}` was rejected (attempt {next_attempt} of {max_attempts}).\n\nViolations:\n"
	);
	for violation in violations {
		text.push_str("- ");
		text.push_str(violation);
		text.push('\n');
	}
	text.push_str(
		"\nFix exactly these and return a corrected final JSON envelope. Your prior work is intact — do not start over.",
	);
	text
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::envelope::EnvelopeStatus;
	use crate::gate::ArtifactsExist;
	use crate::phase::PhaseKind;
	use crate::test_support::TempDir;
	use crate::trace::TraceReader;

	fn workflow() -> Workflow {
		Workflow::new(
			"plan_build",
			vec![
				PhaseParams::new("plan", PhaseKind::Agent, "planner"),
				PhaseParams::new("build", PhaseKind::Agent, "builder"),
			],
		)
	}

	fn run_in(dir: &TempDir) -> Run {
		Run::new("adw-test", dir.path(), workflow())
	}

	fn ok_envelope(summary: &str) -> Envelope<Value> {
		Envelope {
			status: EnvelopeStatus::Success,
			summary: summary.to_owned(),
			artifacts: Vec::new(),
			notes_for_next_agent: String::new(),
			payload: Value::Object(serde_json::Map::new()),
		}
	}

	#[test]
	fn phases_run_in_order_and_the_run_is_accepted() {
		let dir = TempDir::new("run-happy");
		let mut run = run_in(&dir);

		let Step::Run { phase, attempt, correction } = run.next_step().expect("step") else {
			panic!("expected a step");
		};
		assert_eq!(phase.name, "plan");
		assert_eq!(attempt, 1);
		assert!(correction.is_none());

		assert_eq!(
			run.submit_envelope(ok_envelope("planned")).expect("submit"),
			Outcome::Advanced { phase: "plan".into() }
		);

		let Step::Run { phase, .. } = run.next_step().expect("step") else { panic!("expected a step") };
		assert_eq!(phase.name, "build");
		run.submit_envelope(ok_envelope("built")).expect("submit");

		assert_eq!(run.next_step().expect("step"), Step::Done { accepted: true });
		assert_eq!(run.records().len(), 2);
		assert!(run.records().iter().all(|r| r.status == PhaseStatus::Passed));
	}

	#[test]
	fn a_failed_gate_re_runs_the_same_phase_with_the_violation_named() {
		let dir = TempDir::new("run-gate");
		let mut run = run_in(&dir);
		run.register_gates("plan", vec![Box::new(ArtifactsExist)]);

		run.next_step().expect("step");
		let claimed = Envelope { artifacts: vec!["specs/plan.md".into()], ..ok_envelope("planned") };
		let outcome = run.submit_envelope(claimed).expect("submit");

		let Outcome::Retry { attempt, correction, .. } = outcome else {
			panic!("expected a retry, got {outcome:?}");
		};
		assert_eq!(attempt, 2);
		assert!(correction.contains("specs/plan.md"), "{correction}");
		assert!(correction.contains("artifacts_exist"), "{correction}");

		// Same phase, next attempt, feedback carried into the same session.
		let Step::Run { phase, attempt, correction } = run.next_step().expect("step") else {
			panic!("expected a step");
		};
		assert_eq!(phase.name, "plan");
		assert_eq!(attempt, 2);
		assert!(correction.expect("correction carried").contains("specs/plan.md"));

		// The agent writes the file it claimed; the same claim now passes.
		dir.write("specs/plan.md", "# plan");
		let claimed = Envelope { artifacts: vec!["specs/plan.md".into()], ..ok_envelope("planned") };
		assert_eq!(
			run.submit_envelope(claimed).expect("submit"),
			Outcome::Advanced { phase: "plan".into() }
		);
		assert_eq!(run.records()[0].attempts, 2, "both attempts are recorded");
	}

	#[test]
	fn unparseable_output_is_a_correction_not_a_crash() {
		let dir = TempDir::new("run-parse");
		let mut run = run_in(&dir);
		run.next_step().expect("step");

		let outcome = run.submit_agent_output("I finished the plan, trust me.").expect("submit");
		let Outcome::Retry { correction, .. } = outcome else { panic!("expected retry, got {outcome:?}") };
		assert!(correction.contains("no JSON object"), "{correction}");
	}

	#[test]
	fn a_self_reported_failure_is_rejected_even_with_green_gates() {
		let dir = TempDir::new("run-selffail");
		let mut run = run_in(&dir);
		run.next_step().expect("step");

		let outcome = run
			.submit_envelope(Envelope { status: EnvelopeStatus::Fail, ..ok_envelope("could not read the spec") })
			.expect("submit");
		let Outcome::Retry { correction, .. } = outcome else { panic!("expected retry, got {outcome:?}") };
		assert!(correction.contains("could not read the spec"), "{correction}");
	}

	#[test]
	fn a_resumed_run_continues_at_the_phase_that_was_in_flight() {
		let dir = TempDir::new("run-resume");
		let trace_dir = dir.path().join("trace");
		{
			let mut run = run_in(&dir).with_tracer(Tracer::create(&trace_dir).expect("tracer"));
			run.next_step().expect("step");
			let mut env = ok_envelope("planned");
			env.notes_for_next_agent = "theme tokens live in src/theme.ts".into();
			run.submit_envelope(env).expect("submit");
			// The process dies here, mid-`build`.
			run.next_step().expect("step");
		}

		let mut resumed =
			Run::resume("adw-test", dir.path(), workflow(), &trace_dir).expect("resume");
		let Step::Run { phase, attempt, .. } = resumed.next_step().expect("step") else {
			panic!("expected the unfinished phase");
		};
		assert_eq!(phase.name, "build", "plan already passed; it must not run twice");
		assert_eq!(attempt, 1);
		// The handoff survives the crash — it is the one thing the events cannot
		// reconstruct, so it is persisted beside them.
		assert_eq!(
			resumed.previous_envelope().expect("handoff").notes_for_next_agent,
			"theme tokens live in src/theme.ts"
		);
		assert_eq!(resumed.records().len(), 1);
		assert_eq!(resumed.records()[0].name, "plan");
	}

	#[test]
	fn a_resumed_run_keeps_the_attempts_already_spent() {
		let dir = TempDir::new("run-resume-attempts");
		let trace_dir = dir.path().join("trace");
		{
			let mut run = run_in(&dir).with_tracer(Tracer::create(&trace_dir).expect("tracer"));
			run.register_gates("plan", vec![Box::new(ArtifactsExist)]);
			run.next_step().expect("step");
			let claim = Envelope { artifacts: vec!["missing.md".into()], ..ok_envelope("planned") };
			run.submit_envelope(claim).expect("submit");
		}

		let mut resumed = Run::resume("adw-test", dir.path(), workflow(), &trace_dir).expect("resume");
		let Step::Run { phase, attempt, .. } = resumed.next_step().expect("step") else {
			panic!("expected a step");
		};
		// A resumed run must not hand back a fresh budget; that would let a
		// crash-loop retry forever.
		assert_eq!(phase.name, "plan");
		assert_eq!(attempt, 2);
	}

	#[test]
	fn resuming_a_trace_from_another_workflow_is_refused() {
		let dir = TempDir::new("run-resume-mismatch");
		let trace_dir = dir.path().join("trace");
		{
			let mut run = run_in(&dir).with_tracer(Tracer::create(&trace_dir).expect("tracer"));
			run.next_step().expect("step");
		}
		let other = Workflow::new("something_else", vec![PhaseParams::new("plan", PhaseKind::Agent, "planner")]);
		let Err(err) = Run::resume("adw-test", dir.path(), other, &trace_dir) else {
			panic!("must refuse a foreign trace");
		};
		assert!(matches!(err, RunError::Mismatch(_)), "{err:?}");
	}

	#[test]
	fn resuming_a_finished_run_is_refused() {
		let dir = TempDir::new("run-resume-done");
		let trace_dir = dir.path().join("trace");
		{
			let mut run = run_in(&dir).with_tracer(Tracer::create(&trace_dir).expect("tracer"));
			run.next_step().expect("step");
			run.submit_envelope(ok_envelope("planned")).expect("submit");
			run.next_step().expect("step");
			run.submit_envelope(ok_envelope("built")).expect("submit");
			run.finish(true, "").expect("finish");
		}
		let Err(err) = Run::resume("adw-test", dir.path(), workflow(), &trace_dir) else {
			panic!("must refuse a finished run");
		};
		assert!(matches!(err, RunError::Mismatch(_)), "{err:?}");
	}

	#[test]
	fn panel_opinions_are_traced_inside_the_phase_they_fed() {
		let dir = TempDir::new("run-panel");
		let trace_dir = dir.path().join("trace");
		let mut run = run_in(&dir).with_tracer(Tracer::create(&trace_dir).expect("tracer"));

		run.next_step().expect("step");
		run.note_panel_opinion("scout", true, 1_200).expect("note");
		run.note_panel_opinion("reviewer", false, 0).expect("note");
		run.submit_envelope(ok_envelope("fused")).expect("submit");

		let mut reader = TraceReader::open(&trace_dir).expect("open");
		let events = reader.read_from(0).expect("read");
		let panel: Vec<_> = events.iter().filter(|e| e.kind == EventKind::PanelOpinion).collect();
		assert_eq!(panel.len(), 2);
		assert_eq!(reader.text(panel[0].owner), "scout");
		assert!(panel[0].ok);
		assert_eq!(panel[0].value, 1_200, "tokens make two models comparable");
		assert_eq!(reader.text(panel[1].owner), "reviewer");
		assert!(!panel[1].ok);
		// Still one phase to the engine: it settles on the fuser's envelope.
		assert_eq!(run.records().len(), 1);
		assert_eq!(events.iter().filter(|e| e.kind == EventKind::PhaseFinished).count(), 1);
	}

	#[test]
	fn a_gateless_phase_still_records_why_it_failed() {
		// A red test suite has no gate to blame. Without the record's own
		// violations a failed phase would report no reason at all.
		let dir = TempDir::new("run-why");
		let mut run = Run::new(
			"adw-why",
			dir.path(),
			Workflow::new("test_only", vec![PhaseParams::new("test", PhaseKind::Code, "bun")]),
		)
		.with_max_attempts(1);

		run.next_step().expect("step");
		let outcome = run.submit_envelope(Envelope::code(false, "1 test failed")).expect("submit");
		assert!(matches!(outcome, Outcome::Aborted { .. }), "{outcome:?}");

		let record = run.records().last().expect("record");
		assert_eq!(record.status, PhaseStatus::Failed);
		assert!(record.gates.is_empty(), "no gates were registered");
		assert_eq!(record.violations.len(), 1);
		assert!(record.violations[0].contains("1 test failed"), "{:?}", record.violations);
	}

	#[test]
	fn exhausting_attempts_halts_the_run() {
		let dir = TempDir::new("run-exhaust");
		let mut run = run_in(&dir).with_max_attempts(2);
		run.register_gates("plan", vec![Box::new(ArtifactsExist)]);
		let claim = || Envelope { artifacts: vec!["missing.md".into()], ..ok_envelope("planned") };

		run.next_step().expect("step");
		assert!(matches!(run.submit_envelope(claim()).expect("submit"), Outcome::Retry { .. }));
		run.next_step().expect("step");
		assert!(matches!(run.submit_envelope(claim()).expect("submit"), Outcome::Aborted { .. }));

		assert_eq!(run.next_step().expect("step"), Step::Done { accepted: false });
		assert!(matches!(run.submit_envelope(ok_envelope("late")), Err(RunError::NoActiveStep)));
		assert_eq!(run.records().last().expect("record").status, PhaseStatus::Failed);
	}

	#[test]
	fn the_accepted_envelope_is_the_handoff_to_the_next_phase() {
		let dir = TempDir::new("run-handoff");
		let mut run = run_in(&dir);
		run.next_step().expect("step");
		assert!(run.previous_envelope().is_none());

		let mut env = ok_envelope("planned");
		env.notes_for_next_agent = "theme tokens live in src/theme.ts".into();
		run.submit_envelope(env).expect("submit");

		let handoff = run.previous_envelope().expect("handoff");
		assert_eq!(handoff.notes_for_next_agent, "theme tokens live in src/theme.ts");
	}

	#[test]
	fn a_failing_code_phase_reports_through_the_same_door() {
		let dir = TempDir::new("run-code");
		let mut run = Run::new(
			"adw-code",
			dir.path(),
			Workflow::new("test_only", vec![PhaseParams::new("test", PhaseKind::Code, "bun")]),
		);
		run.next_step().expect("step");

		let outcome = run.submit_envelope(Envelope::code(false, "2 tests failed")).expect("submit");
		let Outcome::Retry { correction, .. } = outcome else { panic!("expected retry, got {outcome:?}") };
		assert!(correction.contains("2 tests failed"), "{correction}");
	}

	#[test]
	fn the_run_writes_a_binary_trace_a_reader_can_tail() {
		let dir = TempDir::new("run-trace");
		let trace_dir = dir.path().join("trace");
		let mut run = run_in(&dir).with_tracer(Tracer::create(&trace_dir).expect("tracer"));
		run.register_gates("plan", vec![Box::new(ArtifactsExist)]);

		run.next_step().expect("step");
		dir.write("specs/plan.md", "# plan");
		let claimed = Envelope { artifacts: vec!["specs/plan.md".into()], ..ok_envelope("planned") };
		run.submit_envelope(claimed).expect("submit");
		run.finish(true, "").expect("finish");

		let mut reader = TraceReader::open(&trace_dir).expect("open");
		let events = reader.read_from(0).expect("read");
		let kinds: Vec<EventKind> = events.iter().map(|e| e.kind).collect();
		assert_eq!(
			kinds,
			[
				EventKind::RunStarted,
				EventKind::PhaseStarted,
				EventKind::GateCheck,
				EventKind::PhaseFinished,
				EventKind::RunFinished,
			]
		);

		let gate_check = events.iter().find(|e| e.kind == EventKind::GateCheck).expect("gate check");
		assert!(gate_check.ok);
		assert_eq!(reader.text(gate_check.phase), "plan");
		assert_eq!(reader.text(gate_check.gate), "artifacts_exist");
		assert_eq!(reader.text(gate_check.detail), "specs/plan.md");
		assert!(events.last().expect("last").ok, "run accepted");
	}

	#[test]
	fn a_rejected_attempt_is_traced_with_its_violation_count() {
		let dir = TempDir::new("run-trace-reject");
		let trace_dir = dir.path().join("trace");
		let mut run = run_in(&dir).with_tracer(Tracer::create(&trace_dir).expect("tracer"));
		run.register_gates("plan", vec![Box::new(ArtifactsExist)]);

		run.next_step().expect("step");
		let claimed = Envelope { artifacts: vec!["a.md".into(), "b.md".into()], ..ok_envelope("planned") };
		run.submit_envelope(claimed).expect("submit");

		let mut reader = TraceReader::open(&trace_dir).expect("open");
		let events = reader.read_from(0).expect("read");
		let failed_checks =
			events.iter().filter(|e| e.kind == EventKind::GateCheck && !e.ok).count();
		assert_eq!(failed_checks, 2, "one record per examined item");

		let rejected = events.iter().find(|e| e.kind == EventKind::PhaseRejected).expect("rejection");
		assert_eq!(rejected.value, 2, "violation count");
		assert_eq!(rejected.attempt, 1);
	}
}
