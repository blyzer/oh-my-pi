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
//!     Step::Run { phase, correction, .. } => run.submit_agent_output(&phase.name, &model_turn)?,
//!     Step::Wait                         => wait_for_in_flight_work(),
//!     Step::Done { accepted }             => break,
//! } }
//! ```
//!
//! Steps and outcomes cross the napi boundary as structs (V8 accessors, no
//! JSON), and observability goes to the binary log in [`crate::trace`] — text
//! survives only where a model must read it: the correction prompt.

use std::{
	collections::HashMap,
	path::{Path, PathBuf},
};

use serde::Serialize;
use serde_json::Value;

use crate::{
	envelope::{Envelope, EnvelopeError},
	gate::{Check, Gate, GateCtx, GateReport},
	phase::{PhaseParams, PhaseRecord, PhaseStatus},
	trace::{EventKind, EventRecord, TraceReader, Tracer},
};

/// Accepted envelopes live beside the trace, one file per acceptance:
/// `<phase>.<version>.json`, where the version is the phase's 1-based
/// acceptance ordinal. A re-acceptance after a rewind or review revision
/// writes the next ordinal instead of overwriting — superseded evidence stays
/// on disk, and a consumer can always be told exactly which version it read.
const ENVELOPE_DIR: &str = "envelopes";

/// Phase names are user-authored; keep them from escaping the envelope dir.
fn sanitize(phase: &str) -> String {
	phase
		.chars()
		.map(|c| {
			if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
				c
			} else {
				'_'
			}
		})
		.collect()
}

fn load_envelope(
	trace_dir: &Path,
	phase: &str,
	version: u32,
) -> Result<Option<Envelope<Value>>, RunError> {
	let path = trace_dir
		.join(ENVELOPE_DIR)
		.join(format!("{}.{version}.json", sanitize(phase)));
	match std::fs::read(&path) {
		Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes).map_err(std::io::Error::other)?)),
		// A phase whose file was pruned: resumable, just without that handoff —
		// unless something consumes it, which resume checks after replay.
		Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
		Err(err) => Err(RunError::Trace(err)),
	}
}

const DEFAULT_MAX_ATTEMPTS: u32 = 3;

#[derive(Debug, Clone, Serialize)]
pub struct Workflow {
	pub name:   String,
	pub phases: Vec<PhaseParams>,
}

impl Workflow {
	/// Resolve implicit serial edges before stable topological normalization.
	/// Explicit empty dependency lists remain independent.
	pub fn new(name: impl Into<String>, mut phases: Vec<PhaseParams>) -> Self {
		let mut previous = None;
		for phase in &mut phases {
			if phase.depends_on.is_none() {
				phase.depends_on = Some(previous.iter().cloned().collect());
			}
			previous = Some(phase.name.clone());
		}
		let ordered = topological_order(&phases).unwrap_or_else(|| (0..phases.len()).collect());
		let mut slots: Vec<Option<PhaseParams>> = phases.into_iter().map(Some).collect();
		let phases = ordered
			.into_iter()
			.filter_map(|index| slots[index].take())
			.collect();
		Self { name: name.into(), phases }
	}
}

/// Indices in dependency order, or `None` when the graph cannot be ordered.
///
/// An unorderable graph — a cycle, or a name that does not exist — is left in
/// declaration order here and reported by the caller's validation, which knows
/// the file it came from. Failing silently into a *different* order would be
/// the one unacceptable outcome.
fn topological_order(phases: &[PhaseParams]) -> Option<Vec<usize>> {
	let index_of: HashMap<&str, usize> = phases
		.iter()
		.enumerate()
		.map(|(index, phase)| (phase.name.as_str(), index))
		.collect();
	let mut remaining: Vec<usize> = (0..phases.len()).collect();
	let mut done = vec![false; phases.len()];
	let mut order = Vec::with_capacity(phases.len());

	while !remaining.is_empty() {
		// Declaration order among everything currently ready: the tie-break that
		// makes this deterministic.
		let ready: Vec<usize> = remaining
			.iter()
			.copied()
			.filter(|&index| {
				phases[index]
					.depends_on
					.as_deref()
					.unwrap_or_default()
					.iter()
					.all(|name| index_of.get(name.as_str()).is_some_and(|&dep| done[dep]))
			})
			.collect();
		if ready.is_empty() {
			return None;
		}
		for index in &ready {
			done[*index] = true;
			order.push(*index);
		}
		remaining.retain(|index| !ready.contains(index));
	}
	Some(order)
}

/// One resolved input on a dispatch: which producer, which acceptance ordinal,
/// and the envelope that version holds. The version is what makes the step
/// auditable — the trace records the same number, so "which evidence did this
/// attempt see" has exactly one answer.
#[derive(Debug, Clone, PartialEq)]
pub struct SelectedInput {
	pub phase:    String,
	/// 1-based acceptance ordinal of the producer's envelope.
	pub version:  u32,
	pub envelope: Envelope<Value>,
}

/// What the caller must do next.
#[derive(Debug, Clone, PartialEq)]
pub enum Step {
	Run {
		phase:      PhaseParams,
		/// 1-based; `> 1` means the previous attempt was rejected.
		attempt:    u32,
		/// Verbatim feedback for the retry. The engine does not require the
		/// caller to reuse the agent's session — a driver that re-dispatches a
		/// fresh one must pass this through, because it is the only record of
		/// what was wrong.
		correction: Option<String>,
		/// The declared inputs, resolved to their current accepted versions at
		/// dispatch. Empty for a phase without declared inputs, which keeps the
		/// positional last-envelope handoff instead. A driver building the
		/// prompt for a phase WITH inputs uses these, never the handoff.
		inputs:     Vec<SelectedInput>,
	},
	/// Work remains in flight, but no pending phase is ready. No trace event.
	Wait,
	Done {
		accepted: bool,
	},
}

/// What the run decided about a submitted result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
	Advanced { phase: String },
	Retry { phase: String, attempt: u32, correction: String, invalidated: Vec<String> },
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
	pub adw_id:   String,
	pub workflow: String,
	pub accepted: bool,
	pub reason:   String,
	pub records:  Vec<PhaseRecord>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum DispatchStatus {
	#[default]
	Pending,
	Running,
	Passed,
	Failed,
}

#[derive(Default)]
struct PhaseState {
	status:     DispatchStatus,
	/// Completed attempts; an interrupted dispatch is retried at the same
	/// ordinal.
	attempts:   u32,
	correction: Option<String>,
	reports:    Vec<GateReport>,
	review:     Option<(bool, String)>,
	root:       Option<PathBuf>,
}

pub struct Run {
	adw_id:         String,
	root:           PathBuf,
	workflow:       Workflow,
	tracer:         Option<Tracer>,
	max_attempts:   u32,
	gates:          HashMap<String, Vec<Box<dyn Gate>>>,
	states:         Vec<PhaseState>,
	/// Cumulative for the entire run, never reset by another route.
	revisions:      HashMap<String, u32>,
	attempt_grants: HashMap<String, u32>,
	/// Total acceptances per phase — the next persisted version is this + 1.
	/// Never decremented: a superseded version's ordinal is history.
	accepted:       HashMap<String, u32>,
	/// Each phase's standing output: its acceptance ordinal and, when the
	/// envelope is at hand (always live; on resume only if its file read
	/// back), the envelope itself. Invalidation removes the entry — the file
	/// stays on disk, but the version is superseded and never selected again.
	current:        HashMap<String, (u32, Option<Envelope<Value>>)>,
	last_envelope:  Option<Envelope<Value>>,
	records:        Vec<PhaseRecord>,
	halted:         bool,
	started:        bool,
}

impl Run {
	pub fn new(adw_id: impl Into<String>, root: impl Into<PathBuf>, workflow: Workflow) -> Self {
		let workflow = Workflow::new(workflow.name, workflow.phases);
		Self {
			adw_id: adw_id.into(),
			root: root.into(),
			states: (0..workflow.phases.len())
				.map(|_| PhaseState::default())
				.collect(),
			workflow,
			tracer: None,
			max_attempts: DEFAULT_MAX_ATTEMPTS,
			gates: HashMap::new(),
			revisions: HashMap::new(),
			attempt_grants: HashMap::new(),
			accepted: HashMap::new(),
			current: HashMap::new(),
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
	/// Replay uses phase names, never completion order. Each unfinished dispatch
	/// returns to Pending exactly once, retaining its attempt and correction.
	pub fn resume(
		adw_id: impl Into<String>,
		root: impl Into<PathBuf>,
		workflow: Workflow,
		trace_dir: impl AsRef<Path>,
	) -> Result<Self, RunError> {
		let dir = trace_dir.as_ref();
		let mut reader = TraceReader::open(dir)?;
		let events = reader.read_from(0)?;

		if let Some(started) = events
			.iter()
			.find(|event| event.kind == EventKind::RunStarted)
		{
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
		if events
			.iter()
			.any(|event| event.kind == EventKind::RunFinished && event.ok)
		{
			return Err(RunError::Mismatch("this run was already accepted".to_owned()));
		}

		let mut run = Self::new(adw_id, root, workflow);
		if let Some(started) = events
			.iter()
			.find(|event| event.kind == EventKind::RunStarted)
		{
			run.max_attempts = started.value.max(1);
		}
		run.validate_review_routes()?;
		run.validate_inputs()?;
		run.started = true;
		let mut rejected = HashMap::new();
		for event in &events {
			let named = matches!(
				event.kind,
				EventKind::PhaseStarted
					| EventKind::PhaseRetry
					| EventKind::PhaseFinished
					| EventKind::PhaseRejected
					| EventKind::CorrectionPending
					| EventKind::GateCheck
					| EventKind::PhaseRewound
					| EventKind::ReviewRevision
					| EventKind::ReviewExhausted
					| EventKind::PhaseInvalidated
					| EventKind::InputSelected
					| EventKind::PhaseTokens
					| EventKind::PanelOpinion
			);
			if !named {
				continue;
			}
			let name = reader.text(event.phase).to_owned();
			let index = run.phase_index(&name)?;
			match event.kind {
				EventKind::PhaseStarted | EventKind::PhaseRetry => {
					if run.states[index].status != DispatchStatus::Pending || event.attempt == 0 {
						return Err(RunError::Mismatch(format!(
							"duplicate or invalid dispatch of {name:?}"
						)));
					}
					run.states[index].status = DispatchStatus::Running;
					run.states[index].attempts = event.attempt - 1;
					run.states[index].reports.clear();
				},
				EventKind::GateCheck => {
					let gate = reader.text(event.gate);
					let reports = &mut run.states[index].reports;
					let position = reports.iter().position(|report| report.gate == gate);
					let report = match position {
						Some(position) => &mut reports[position],
						None => {
							reports.push(GateReport::new(gate));
							reports.last_mut().unwrap()
						},
					};
					report.checks.push(Check {
						item: reader.text(event.detail).to_owned(),
						ok:   event.ok,
						note: reader.text(event.owner).to_owned(),
					});
				},
				EventKind::PhaseFinished => {
					let phase = &run.workflow.phases[index];
					let gates = std::mem::take(&mut run.states[index].reports);
					let summary = reader.text(event.detail).to_owned();
					let violations = if event.ok {
						Vec::new()
					} else {
						let mut violations: Vec<_> =
							gates.iter().flat_map(GateReport::violations).collect();
						if violations.is_empty() {
							violations.push(summary.clone());
						}
						violations
					};
					run.records.push(PhaseRecord {
						name: name.clone(),
						kind: phase.kind,
						owner: phase.owner.clone(),
						status: if event.ok {
							PhaseStatus::Passed
						} else {
							PhaseStatus::Failed
						},
						invalidated: false,
						attempts: event.attempt,
						summary,
						gates,
						violations,
					});
					run.states[index].attempts = event.attempt;
					run.states[index].correction = None;
					run.states[index].status = if event.ok {
						DispatchStatus::Passed
					} else {
						DispatchStatus::Failed
					};
					if !event.ok {
						run.halted = true;
						continue;
					}
					let version = run.accepted.get(&name).copied().unwrap_or(0) + 1;
					let envelope = load_envelope(dir, &name, version).map_err(|err| {
						match run.consumer_of(&name) {
							Some(consumer) => RunError::Mismatch(format!(
								"phase {consumer:?} consumes {name:?} version {version}, whose envelope \
								 cannot be read: {err}"
							)),
							None => err,
						}
					})?;
					if let Some(env) = &envelope {
						run.last_envelope = Some(env.clone());
					}
					run.accepted.insert(name.clone(), version);
					run.current.insert(name, (version, envelope));
				},
				EventKind::PhaseRejected => {
					run.states[index].attempts = event.attempt;
					run.states[index].status = DispatchStatus::Pending;
					rejected.insert(
						name,
						(
							event.attempt,
							reader.text(event.detail).to_owned(),
							std::mem::take(&mut run.states[index].reports),
						),
					);
				},
				EventKind::CorrectionPending => {
					run.states[index].correction = Some(reader.text(event.detail).to_owned());
				},
				EventKind::PhaseInvalidated => {
					// Empty owner is a crash reset, not supersession of prior evidence.
					if event.owner != crate::trace::NO_STRING {
						run.current.remove(&name);
						for record in &mut run.records {
							if record.name == name {
								record.invalidated = true;
							}
						}
					}
					run.states[index].status = DispatchStatus::Pending;
					run.states[index].attempts = event.attempt;
					run.states[index].reports.clear();
				},
				EventKind::PhaseRewound | EventKind::ReviewRevision => {
					let source = reader.text(event.owner).to_owned();
					let source_index = run.phase_index(&source)?;
					if !run.depends_transitively(source_index, index) {
						return Err(RunError::Mismatch(format!(
							"trace routes {source:?} to non-dependency {name:?}"
						)));
					}
					if event.kind == EventKind::ReviewRevision {
						let route = run.workflow.phases[source_index]
							.on_reject
							.as_ref()
							.ok_or_else(|| {
								RunError::Mismatch(format!("review route for {source:?} is missing"))
							})?;
						if route.to != name
							|| event.value > u32::from(route.max_revisions)
							|| event.value != run.revisions.get(&source).copied().unwrap_or(0) + 1
						{
							return Err(RunError::Mismatch(format!(
								"review route for {source:?} changed"
							)));
						}
						run.grant_attempt(&name)?;
						run.revisions.insert(source.clone(), event.value);
					} else {
						if run.workflow.phases[source_index].rewind_to.as_deref() != Some(name.as_str()) {
							return Err(RunError::Mismatch(format!(
								"rewind route for {source:?} changed"
							)));
						}
						if let Some((attempts, summary, gates)) = rejected.remove(&source) {
							let phase = &run.workflow.phases[source_index];
							let violations = gates.iter().flat_map(GateReport::violations).collect();
							run.records.push(PhaseRecord {
								name: source.clone(),
								kind: phase.kind,
								owner: phase.owner.clone(),
								status: PhaseStatus::Failed,
								invalidated: false,
								attempts,
								summary,
								gates,
								violations,
							});
						}
					}
					// Also supports v6 serial traces predating explicit invalidation events.
					run.invalidate_dependents(index, source_index)?;
					run.states[index].attempts = event.attempt;
					run.states[index].correction = Some(reader.text(event.detail).to_owned());
					run.last_envelope = run.handoff_before(index)?;
				},
				EventKind::ReviewExhausted => {
					run.halted = true;
					run.states[index].status = DispatchStatus::Failed;
				},
				_ => {},
			}
		}

		// The store just rebuilt is what the next dispatch will select from.
		// A consumed version the trace says was accepted but whose file is gone
		// must refuse resume here, naming the parties — not silently hand the
		// consumer nothing. Superseded versions are exempt: they are never
		// selected again, so pruning them costs nothing.
		for consumer in &run.workflow.phases {
			for producer in &consumer.inputs {
				if let Some((version, envelope)) = run.current.get(producer) {
					if envelope.is_none() {
						return Err(RunError::Mismatch(format!(
							"phase {:?} consumes {producer:?} version {version}, but its envelope file \
							 is missing",
							consumer.name
						)));
					}
				}
			}
		}

		// Resume owns the trace it continues: the marker tells a reader that the
		// records below belong to a continuation, so the terminal record above
		// them is not the end.
		let tracer = Tracer::create(dir)?;
		// Reset every interrupted flight once. The reset is persisted before the
		// continuation marker, so another crash cannot duplicate a start.
		for (index, state) in run.states.iter_mut().enumerate() {
			if state.status == DispatchStatus::Running {
				tracer.emit(
					EventRecord::new(EventKind::PhaseInvalidated)
						.phase(tracer.intern(&run.workflow.phases[index].name)?)
						.attempt(state.attempts),
				)?;
				state.status = DispatchStatus::Pending;
				state.reports.clear();
			}
		}
		let workflow_id = tracer.intern(&run.workflow.name)?;
		tracer.emit(
			EventRecord::new(EventKind::RunResumed)
				.detail(workflow_id)
				.value(
					run.states
						.iter()
						.filter(|state| state.status == DispatchStatus::Passed)
						.count() as u32,
				),
		)?;
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

	/// Base attempt budget. Review grants add to individual target limits.
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
			self.validate_review_routes()?;
			self.validate_inputs()?;
			self.started = true;
			let workflow = self.intern(&self.workflow.name)?;
			self.trace(
				EventRecord::new(EventKind::RunStarted)
					.detail(workflow)
					.value(self.max_attempts),
			)?;
		}
		let active = self
			.states
			.iter()
			.any(|state| state.status == DispatchStatus::Running);
		if self.halted {
			return Ok(if active {
				Step::Wait
			} else {
				Step::Done { accepted: false }
			});
		}
		let ready = self
			.workflow
			.phases
			.iter()
			.enumerate()
			.position(|(index, phase)| {
				self.states[index].status == DispatchStatus::Pending
					&& phase
						.depends_on
						.as_deref()
						.unwrap_or_default()
						.iter()
						.all(|name| {
							self
								.workflow
								.phases
								.iter()
								.position(|dep| &dep.name == name)
								.is_some_and(|dep| self.states[dep].status == DispatchStatus::Passed)
						})
			});
		let Some(index) = ready else {
			if active {
				return Ok(Step::Wait);
			}
			if self
				.states
				.iter()
				.all(|state| state.status == DispatchStatus::Passed)
			{
				return Ok(Step::Done { accepted: true });
			}
			return Err(RunError::Mismatch(
				"pending phases have no satisfied dependency path".to_owned(),
			));
		};
		let phase = self.workflow.phases[index].clone();
		let attempt = self.next_attempt(index)?;
		// Resolved before anything is traced: a phase whose evidence cannot be
		// produced was never dispatched, so its trace must not say it started.
		let inputs = self.select_inputs(&phase)?;
		let kind = if attempt == 1 {
			EventKind::PhaseStarted
		} else {
			EventKind::PhaseRetry
		};
		let phase_id = self.intern(&phase.name)?;
		let owner_id = self.intern(&phase.owner)?;
		self.trace(
			EventRecord::new(kind)
				.phase(phase_id)
				.owner(owner_id)
				.attempt(attempt),
		)?;
		for input in &inputs {
			let producer_id = self.intern(&input.phase)?;
			self.trace(
				EventRecord::new(EventKind::InputSelected)
					.phase(phase_id)
					.owner(producer_id)
					.attempt(attempt)
					.value(input.version),
			)?;
		}
		self.states[index].status = DispatchStatus::Running;
		Ok(Step::Run { phase, attempt, correction: self.states[index].correction.clone(), inputs })
	}

	/// Submit an agent's raw final turn. Output that does not parse is a
	/// correction, not a crash: the agent is asked again in the same session.
	pub fn submit_agent_output(&mut self, name: &str, text: &str) -> Result<Outcome, RunError> {
		let index = self.running_index(name)?;
		match Envelope::from_agent_text(text) {
			Ok(envelope) => self.submit_envelope(name, envelope),
			Err(err) => {
				let violation = match err {
					EnvelopeError::NoJson => "envelope: the turn contained no JSON object".to_owned(),
					EnvelopeError::Invalid(e) => format!("envelope: {e}"),
				};
				// Nothing the caller validated survives an unparseable turn: there
				// was no payload to check, and carrying a report forward would
				// fail the next attempt for this one.
				self.states[index].reports.clear();
				self.states[index].review = None;
				self.reject(index, vec![violation], Vec::new(), "unparseable envelope".to_owned())
			},
		}
	}

	/// Submit a result directly — deterministic `Code` phases and human
	/// `Engineer` phases report through the same door, so their failures reach
	/// the next agent as an envelope like any other.
	pub fn submit_envelope(
		&mut self,
		name: &str,
		envelope: Envelope<Value>,
	) -> Result<Outcome, RunError> {
		let index = self.running_index(name)?;
		let phase = self.workflow.phases[index].clone();

		let mut reports: Vec<GateReport> = {
			let ctx = GateCtx { root: self.states[index].root.as_deref().unwrap_or(&self.root) };
			self
				.gates
				.get(&phase.name)
				.map(|gates| gates.iter().map(|g| g.run(&envelope, &ctx)).collect())
				.unwrap_or_default()
		};
		// Drained, never carried: a report belongs to the attempt that produced
		// it, and a stale one would fail the next attempt for the last one's sin.
		reports.append(&mut self.states[index].reports);
		let review = self.states[index].review.take();

		let phase_id = self.intern(&phase.name)?;
		for report in &reports {
			let gate_id = self.intern(&report.gate)?;
			for check in &report.checks {
				let item_id = self.intern(&check.item)?;
				self.trace(
					EventRecord::new(EventKind::GateCheck)
						.owner(self.intern(&check.note)?)
						.attempt(self.next_attempt(index)?)
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
		if phase.on_reject.is_some() && review.is_none() {
			violations.push("review: missing caller-validated review decision".to_owned());
		}

		if violations.is_empty() {
			// Stateful gates commit only now. A gate that mutated shared state
			// during `run` would let a rejected attempt's declarations authorize
			// later evaluations; `accept` fires exclusively on the accepted path,
			// including an acceptance a negative review immediately supersedes —
			// that attempt was still accepted and its evidence stands.
			if let Some(gates) = self.gates.get(&phase.name) {
				let ctx = GateCtx { root: self.states[index].root.as_deref().unwrap_or(&self.root) };
				for gate in gates {
					gate.accept(&envelope, &ctx);
				}
			}
			let attempts = self.next_attempt(index)?;
			self.states[index].attempts = attempts;
			let summary = envelope.summary.clone();
			let summary_id = self.intern(&summary)?;
			self.records.push(PhaseRecord {
				name: phase.name.clone(),
				kind: phase.kind,
				owner: phase.owner.clone(),
				status: PhaseStatus::Passed,
				invalidated: false,
				attempts,
				summary,
				gates: reports,
				violations: Vec::new(),
			});
			self.trace(
				EventRecord::new(EventKind::PhaseFinished)
					.phase(phase_id)
					.attempt(attempts)
					.detail(summary_id)
					.ok(true),
			)?;
			// The one thing the trace cannot reconstruct. Every other resume input
			// is derivable from the event stream; the handoff the next phase's
			// prompt is built from exists only here. Each acceptance takes the
			// phase's next ordinal and becomes the standing version — a negative
			// review below immediately supersedes it, but it was still accepted
			// and its evidence stays addressable.
			let version = self.accepted.get(&phase.name).copied().unwrap_or(0) + 1;
			self.persist_envelope(&phase.name, version, &envelope)?;
			self.accepted.insert(phase.name.clone(), version);
			self
				.current
				.insert(phase.name.clone(), (version, Some(envelope.clone())));
			self.states[index].status = DispatchStatus::Passed;
			if let (Some(route), Some((false, reason))) = (&phase.on_reject, &review) {
				return self.revise(&phase, route, reason, &envelope);
			}
			self.last_envelope = Some(envelope);
			self.states[index].correction = None;
			self.states[index].root = None;
			return Ok(Outcome::Advanced { phase: phase.name });
		}

		self.reject(index, violations, reports, envelope.summary)
	}

	/// Records one panel member's answer inside the active phase.
	///
	/// A fusion phase is still ONE phase to the engine — it settles on the
	/// fuser's envelope — but the members that fed it are real work with real
	/// cost, and `value` (tokens) is what makes two models comparable.
	pub fn note_panel_opinion(
		&self,
		name: &str,
		owner: &str,
		ok: bool,
		value: u32,
		model: Option<&str>,
	) -> Result<(), RunError> {
		let index = self.running_index(name)?;
		let phase = &self.workflow.phases[index];
		let phase_id = self.intern(&phase.name)?;
		let owner_id = self.intern(owner)?;
		self.trace(
			EventRecord::new(EventKind::PanelOpinion)
				.detail(self.intern(model.unwrap_or_default())?)
				.phase(phase_id)
				.owner(owner_id)
				.ok(ok)
				.value(value),
		)
	}

	/// Records what the attempt in flight cost, reported by the caller — only it
	/// talks to a model.
	///
	/// Charged per attempt, not per phase: a rejected attempt spent real tokens,
	/// and a phase that needed three tries is the one worth seeing in a cost
	/// report. Owner is recorded too, so a fusion phase's fuser is separable
	/// from the panel members already traced by `note_panel_opinion`.
	///
	/// The `ok` flag means *accounted for*. A model turn cannot cost zero
	/// tokens, so a zero is a provider that reported no usage — `omniroute/auto`
	/// returns an all-zero usage record — and a reader must be able to tell that
	/// apart from a phase that was genuinely free. Silently summing zeros would
	/// present a confident total that is wrong.
	pub fn note_phase_tokens(
		&self,
		name: &str,
		owner: &str,
		tokens: u32,
		model: Option<&str>,
	) -> Result<(), RunError> {
		let index = self.running_index(name)?;
		let phase = &self.workflow.phases[index];
		let phase_id = self.intern(&phase.name)?;
		let owner_id = self.intern(owner)?;
		self.trace(
			EventRecord::new(EventKind::PhaseTokens)
				.detail(self.intern(model.unwrap_or_default())?)
				.phase(phase_id)
				.owner(owner_id)
				.attempt(self.next_attempt(index)?)
				.ok(tokens > 0)
				.value(tokens),
		)
	}

	/// Record a gate the caller ran itself, to be judged with the engine's own
	/// on the next submission.
	///
	/// The escape hatch for a check the engine cannot perform — schema
	/// validation needs the TypeScript type system, and putting a JSON Schema
	/// validator in here would cost the crate its three dependencies. Same
	/// bargain `Code` phases already make: the caller executes, the engine
	/// judges, and the result is a `gate_check` in the trace like any other.
	pub fn note_gate_report(
		&mut self,
		name: &str,
		gate: impl Into<String>,
		checks: Vec<Check>,
	) -> Result<(), RunError> {
		let index = self.running_index(name)?;
		let mut report = GateReport::new(gate);
		report.checks = checks;
		self.states[index].reports.push(report);
		Ok(())
	}

	/// Supply a coherent decision for this active attempt, not a gate bypass.
	pub fn note_review_decision(
		&mut self,
		name: &str,
		approved: bool,
		reason: impl Into<String>,
	) -> Result<(), RunError> {
		let index = self.running_index(name)?;
		self.states[index].review = Some((approved, reason.into()));
		Ok(())
	}

	/// Evaluate this flight's gates in its writer workspace. Never persisted:
	/// resumed dispatches must provide their newly created workspace again.
	pub fn set_phase_root(&mut self, name: &str, root: impl Into<PathBuf>) -> Result<(), RunError> {
		let index = self.running_index(name)?;
		self.states[index].root = Some(root.into());
		Ok(())
	}

	/// Settles the run's second question: phases passing is not the same as the
	/// result being acceptable.
	pub fn finish(
		&mut self,
		accepted: bool,
		reason: impl Into<String>,
	) -> Result<RunSummary, RunError> {
		let accepted = accepted
			&& !self.halted
			&& self
				.states
				.iter()
				.all(|state| state.status == DispatchStatus::Passed);
		let reason = reason.into();
		let reason_id = self.intern(&reason)?;
		self.trace(
			EventRecord::new(EventKind::RunFinished)
				.detail(reason_id)
				.ok(accepted),
		)?;
		Ok(RunSummary {
			adw_id: self.adw_id.clone(),
			workflow: self.workflow.name.clone(),
			accepted,
			reason,
			records: self.records.clone(),
		})
	}

	/// Writes the accepted envelope beside the trace, keyed by phase name and
	/// acceptance ordinal. Never overwrites: a later acceptance takes the next
	/// ordinal, so superseded evidence stays addressable.
	fn persist_envelope(
		&self,
		phase: &str,
		version: u32,
		envelope: &Envelope<Value>,
	) -> Result<(), RunError> {
		let Some(tracer) = &self.tracer else {
			return Ok(());
		};
		let dir = tracer.dir().join(ENVELOPE_DIR);
		std::fs::create_dir_all(&dir)?;
		let json = serde_json::to_vec_pretty(envelope).map_err(std::io::Error::other)?;
		std::fs::write(dir.join(format!("{}.{version}.json", sanitize(phase))), json)?;
		Ok(())
	}

	/// The declared inputs of `phase`, resolved to their producers' current
	/// accepted versions.
	///
	/// A producer without a standing version is a decided failure naming both
	/// parties, never a silent fall-through to the positional handoff: a phase
	/// that declared what it consumes must not run on something else.
	fn select_inputs(&self, phase: &PhaseParams) -> Result<Vec<SelectedInput>, RunError> {
		let mut selected = Vec::with_capacity(phase.inputs.len());
		for producer in &phase.inputs {
			match self.current.get(producer) {
				Some((version, Some(envelope))) => selected.push(SelectedInput {
					phase:    producer.clone(),
					version:  *version,
					envelope: envelope.clone(),
				}),
				Some((version, None)) => {
					return Err(RunError::Mismatch(format!(
						"phase {:?} consumes {producer:?} version {version}, but its envelope file is \
						 missing",
						phase.name
					)));
				},
				None => {
					let accepted = self.accepted.get(producer).copied().unwrap_or(0);
					return Err(RunError::Mismatch(if accepted == 0 {
						format!(
							"phase {:?} consumes {producer:?}, which has no accepted output",
							phase.name
						)
					} else {
						format!(
							"phase {:?} consumes {producer:?}, whose accepted output (version \
							 {accepted}) was invalidated and not re-accepted",
							phase.name
						)
					}));
				},
			}
		}
		Ok(selected)
	}

	/// The first phase that declares `producer` as an input, if any — the party
	/// to name when the producer's persisted output cannot be served.
	fn consumer_of(&self, producer: &str) -> Option<&str> {
		self
			.workflow
			.phases
			.iter()
			.find(|phase| phase.inputs.iter().any(|input| input == producer))
			.map(|phase| phase.name.as_str())
	}

	/// Engine-side guard on declared inputs, mirroring how
	/// [`Self::validate_review_routes`] refuses a caller that bypassed file
	/// validation: every input must name a known, distinct phase that executes
	/// earlier, exactly once — and when the consumer declares `depends_on`,
	/// every input must be inside its transitive dependency closure, so the
	/// ordering the author wrote and the data edge cannot disagree.
	fn validate_inputs(&self) -> Result<(), RunError> {
		for (position, phase) in self.workflow.phases.iter().enumerate() {
			if phase.inputs.is_empty() {
				continue;
			}
			for (index, input) in phase.inputs.iter().enumerate() {
				if input == &phase.name {
					return Err(RunError::Mismatch(format!(
						"phase {:?} cannot consume itself",
						phase.name
					)));
				}
				if phase.inputs[..index].contains(input) {
					return Err(RunError::Mismatch(format!(
						"phase {:?} declares input {input:?} more than once",
						phase.name
					)));
				}
				let Some(producer) = self
					.workflow
					.phases
					.iter()
					.position(|candidate| candidate.name == *input)
				else {
					return Err(RunError::Mismatch(format!(
						"phase {:?} consumes unknown phase {input:?}",
						phase.name
					)));
				};
				if producer >= position {
					return Err(RunError::Mismatch(format!(
						"phase {:?} consumes {input:?}, which does not execute earlier",
						phase.name
					)));
				}
			}
			{
				let mut visited = vec![false; self.workflow.phases.len()];
				let mut pending: Vec<&str> = phase
					.depends_on
					.as_deref()
					.unwrap_or_default()
					.iter()
					.map(String::as_str)
					.collect();
				while let Some(name) = pending.pop() {
					if let Some((index, dependency)) = self
						.workflow
						.phases
						.iter()
						.enumerate()
						.find(|(_, candidate)| candidate.name == name)
					{
						if !visited[index] {
							visited[index] = true;
							pending.extend(
								dependency
									.depends_on
									.as_deref()
									.unwrap_or_default()
									.iter()
									.map(String::as_str),
							);
						}
					}
				}
				for input in &phase.inputs {
					let reachable = self
						.workflow
						.phases
						.iter()
						.enumerate()
						.any(|(index, candidate)| visited[index] && candidate.name == *input);
					if !reachable {
						return Err(RunError::Mismatch(format!(
							"input {input:?} is not a transitive dependency of {:?}",
							phase.name
						)));
					}
				}
			}
		}
		Ok(())
	}

	fn phase_index(&self, name: &str) -> Result<usize, RunError> {
		self
			.workflow
			.phases
			.iter()
			.position(|phase| phase.name == name)
			.ok_or_else(|| RunError::Mismatch(format!("unknown phase {name:?}")))
	}

	fn running_index(&self, name: &str) -> Result<usize, RunError> {
		let index = self.phase_index(name)?;
		if self.states[index].status != DispatchStatus::Running {
			return Err(RunError::NoActiveStep);
		}
		Ok(index)
	}

	fn depends_transitively(&self, source: usize, target: usize) -> bool {
		let mut seen = vec![false; self.workflow.phases.len()];
		let mut pending = vec![source];
		while let Some(index) = pending.pop() {
			if seen[index] {
				continue;
			}
			seen[index] = true;
			for name in self.workflow.phases[index]
				.depends_on
				.as_deref()
				.unwrap_or_default()
			{
				if let Ok(dependency) = self.phase_index(name) {
					if dependency == target {
						return true;
					}
					pending.push(dependency);
				}
			}
		}
		false
	}

	fn reject(
		&mut self,
		index: usize,
		violations: Vec<String>,
		reports: Vec<GateReport>,
		summary: String,
	) -> Result<Outcome, RunError> {
		let phase = self.workflow.phases[index].clone();

		// Checked before the in-place budget: re-running a deterministic command
		// cannot change its own verdict, so a phase that names a target must not
		// spend attempts proving that twice.
		if phase.on_reject.is_none() {
			if let Some(target) = phase.rewind_to.clone() {
				return self.rewind(&phase, &target, violations, reports, summary);
			}
		}

		let attempts = self.next_attempt(index)?;
		self.states[index].attempts = attempts;
		self.states[index].root = None;
		let limit = self.attempt_limit(&phase.name)?;
		let phase_id = self.intern(&phase.name)?;
		let summary_id = self.intern(&summary)?;

		if attempts >= limit {
			let reason = violations.join("; ");
			let count = violations.len() as u32;
			self.records.push(PhaseRecord {
				name: phase.name.clone(),
				kind: phase.kind,
				owner: phase.owner.clone(),
				status: PhaseStatus::Failed,
				invalidated: false,
				attempts,
				summary,
				gates: reports,
				violations,
			});
			self.halted = true;
			self.states[index].status = DispatchStatus::Failed;
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
		let correction = correction_text(&phase.name, attempts + 1, limit, &violations);
		let correction_id = self.intern(&correction)?;
		// The full correction, beside the summary-carrying rejection above:
		// without it a resumed retry after an in-place rejection would dispatch
		// with no diagnostic — only rewinds and revisions traced theirs.
		self.trace(
			EventRecord::new(EventKind::CorrectionPending)
				.phase(phase_id)
				.attempt(attempts + 1)
				.detail(correction_id),
		)?;
		self.states[index].status = DispatchStatus::Pending;
		self.states[index].correction = Some(correction.clone());
		Ok(Outcome::Retry {
			phase: phase.name,
			attempt: attempts + 1,
			correction,
			invalidated: Vec::new(),
		})
	}

	/// Send the run back to an earlier phase, carrying this failure as its
	/// correction.
	///
	/// The budget belongs to the **target**, not to the phase that failed: only
	/// the target can change the outcome, and charging it is what terminates
	/// the loop. A target that has already spent its attempts halts the run
	/// exactly like any other exhausted phase.
	fn rewind(
		&mut self,
		from: &PhaseParams,
		target: &str,
		violations: Vec<String>,
		reports: Vec<GateReport>,
		summary: String,
	) -> Result<Outcome, RunError> {
		let Some(index) = self
			.workflow
			.phases
			.iter()
			.position(|phase| phase.name == target)
		else {
			return Err(RunError::Mismatch(format!(
				"phase {:?} rewinds to unknown phase {target:?}",
				from.name
			)));
		};
		let source = self.phase_index(&from.name)?;
		if !self.depends_transitively(source, index) {
			// Forward is not a rewind. Allowing it would let a workflow skip
			// phases, or loop on itself with no budget to exhaust.
			return Err(RunError::Mismatch(format!(
				"phase {:?} rewinds to {target:?}, which has not run yet",
				from.name
			)));
		}

		let spent = self
			.records
			.iter()
			.rev()
			.find(|record| record.name == target)
			.map_or(0, |record| record.attempts);
		let phase_id = self.intern(&from.name)?;
		let target_id = self.intern(target)?;
		let summary_id = self.intern(&summary)?;
		let count = violations.len() as u32;

		// Recorded either way: the run's history has to show what sent it back,
		// or a report shows a phase running twice for no visible reason.
		self.records.push(PhaseRecord {
			name: from.name.clone(),
			kind: from.kind,
			owner: from.owner.clone(),
			status: PhaseStatus::Failed,
			invalidated: false,
			attempts: self.next_attempt(source)?,
			summary,
			gates: reports,
			violations: violations.clone(),
		});

		self.states[source].attempts = self.next_attempt(source)?;
		self.states[source].status = DispatchStatus::Failed;
		let limit = self.attempt_limit(target)?;
		if spent >= limit {
			let reason = violations.join("; ");
			self.halted = true;
			self.trace(
				EventRecord::new(EventKind::PhaseFinished)
					.phase(phase_id)
					.attempt(self.states[source].attempts)
					.detail(summary_id)
					.value(count),
			)?;
			return Ok(Outcome::Aborted { phase: from.name.clone(), reason });
		}

		self.trace(
			EventRecord::new(EventKind::PhaseRejected)
				.phase(phase_id)
				.attempt(self.states[source].attempts)
				.detail(summary_id)
				.value(count),
		)?;
		let correction = rewind_text(&from.name, target, spent + 1, limit, &violations);
		let correction_id = self.intern(&correction)?;
		// `owner` carries the phase that failed: the target alone does not say
		// why the run came back.
		self.trace(
			EventRecord::new(EventKind::PhaseRewound)
				.phase(target_id)
				.owner(phase_id)
				.attempt(spent)
				.detail(correction_id)
				.value(count),
		)?;

		let invalidated = self.invalidate_dependents(index, source)?;

		// The target is handed the envelope it originally received, not the one
		// from the phase that just failed downstream of it.
		self.last_envelope = self.handoff_before(index)?;
		self.states[index].attempts = spent;
		// Spent target attempts survive every route; only review grants add budget.
		self.states[index].correction = Some(correction.clone());
		Ok(Outcome::Retry { phase: target.to_owned(), attempt: spent + 1, correction, invalidated })
	}

	fn validate_review_routes(&self) -> Result<(), RunError> {
		let mut names = std::collections::HashSet::new();
		if self
			.workflow
			.phases
			.iter()
			.any(|phase| !names.insert(&phase.name))
		{
			return Err(RunError::Mismatch("duplicate phase names".to_owned()));
		}
		if topological_order(&self.workflow.phases).is_none() {
			return Err(RunError::Mismatch(
				"workflow requires an acyclic dependency graph with known phases".to_owned(),
			));
		}
		for (source, phase) in self.workflow.phases.iter().enumerate() {
			if let Some(target) = &phase.rewind_to {
				let target_index = self.phase_index(target)?;
				if !self.depends_transitively(source, target_index) {
					return Err(RunError::Mismatch(format!(
						"rewind target {target:?} is not a dependency of {:?}",
						phase.name
					)));
				}
			}
			let Some(route) = &phase.on_reject else {
				continue;
			};
			let target = self.phase_index(&route.to)?;
			if !self.depends_transitively(source, target)
				|| self.workflow.phases[target].kind != crate::phase::PhaseKind::Agent
				|| phase.kind != crate::phase::PhaseKind::Agent
				|| route.max_revisions == 0
			{
				return Err(RunError::Mismatch(format!(
					"invalid review route from {:?} to {:?}",
					phase.name, route.to
				)));
			}
		}
		Ok(())
	}

	fn next_attempt(&self, index: usize) -> Result<u32, RunError> {
		let phase = &self.workflow.phases[index];
		if self.states[index].attempts >= self.attempt_limit(&phase.name)? {
			return Err(RunError::Mismatch(format!(
				"attempt budget exhausted for phase {:?}",
				phase.name
			)));
		}
		self.states[index]
			.attempts
			.checked_add(1)
			.ok_or_else(|| RunError::Mismatch("attempt counter overflow".to_owned()))
	}

	/// Keep cumulative attempt accounting for every possible rewind target,
	/// including a target revisited serially after an earlier phase rewinds.
	fn entry_attempts(&self, index: usize) -> u32 {
		let Some(phase) = self.workflow.phases.get(index) else {
			return 0;
		};
		let is_target = self.workflow.phases.iter().any(|source| {
			source.rewind_to.as_deref() == Some(phase.name.as_str())
				|| source
					.on_reject
					.as_ref()
					.is_some_and(|route| route.to == phase.name)
		});
		if is_target {
			self
				.records
				.iter()
				.rev()
				.find(|record| record.name == phase.name)
				.map_or(0, |record| record.attempts)
		} else {
			0
		}
	}

	fn attempt_limit(&self, phase: &str) -> Result<u32, RunError> {
		self
			.max_attempts
			.checked_add(self.attempt_grants.get(phase).copied().unwrap_or(0))
			.ok_or_else(|| RunError::Mismatch(format!("attempt budget overflow for phase {phase:?}")))
	}

	fn grant_attempt(&mut self, target: &str) -> Result<(), RunError> {
		let grants = self
			.attempt_grants
			.get(target)
			.copied()
			.unwrap_or(0)
			.checked_add(1)
			.ok_or_else(|| {
				RunError::Mismatch(format!("revision grant overflow for phase {target:?}"))
			})?;
		self.max_attempts.checked_add(grants).ok_or_else(|| {
			RunError::Mismatch(format!("attempt budget overflow for phase {target:?}"))
		})?;
		self.attempt_grants.insert(target.to_owned(), grants);
		Ok(())
	}

	fn invalidate_dependents(
		&mut self,
		target: usize,
		source: usize,
	) -> Result<Vec<String>, RunError> {
		let indices: Vec<_> = (0..self.workflow.phases.len())
			.filter(|&index| index == target || self.depends_transitively(index, target))
			.collect();
		let source_id = self.intern(&self.workflow.phases[source].name)?;
		let mut invalidated = Vec::with_capacity(indices.len());
		for index in indices {
			let name = self.workflow.phases[index].name.clone();
			let attempts = self.entry_attempts(index);
			self.trace(
				EventRecord::new(EventKind::PhaseInvalidated)
					.phase(self.intern(&name)?)
					.owner(source_id)
					.attempt(attempts),
			)?;
			self.states[index] = PhaseState { attempts, ..PhaseState::default() };
			self.current.remove(&name);
			for record in &mut self.records {
				if record.name == name {
					record.invalidated = true;
				}
			}
			invalidated.push(name);
		}
		Ok(invalidated)
	}

	fn revise(
		&mut self,
		from: &PhaseParams,
		route: &crate::phase::ReviewRoute,
		reason: &str,
		envelope: &Envelope<Value>,
	) -> Result<Outcome, RunError> {
		let Some(index) = self
			.workflow
			.phases
			.iter()
			.position(|phase| phase.name == route.to)
		else {
			return Err(RunError::Mismatch(format!(
				"review {:?} targets unknown phase {:?}",
				from.name, route.to
			)));
		};
		let source = self.phase_index(&from.name)?;
		if !self.depends_transitively(source, index)
			|| from.kind != crate::phase::PhaseKind::Agent
			|| self.workflow.phases[index].kind != crate::phase::PhaseKind::Agent
			|| route.max_revisions == 0
		{
			return Err(RunError::Mismatch(format!(
				"invalid review route from {:?} to {:?}",
				from.name, route.to
			)));
		}
		let consumed = self.revisions.get(&from.name).copied().unwrap_or(0);
		let source_id = self.intern(&from.name)?;
		if consumed >= u32::from(route.max_revisions) {
			let reason = format!(
				"{reason}; review revision budget exhausted ({consumed}/{})",
				route.max_revisions
			);
			return self.halt_review(&from.name, reason, consumed);
		}
		let spent = self.entry_attempts(index);
		if let Err(error) = self.grant_attempt(&route.to) {
			return self.halt_review(&from.name, format!("{reason}; {error}"), consumed);
		}
		let revision = consumed + 1;
		let limit = self.attempt_limit(&route.to)?;
		let report = serde_json::to_string_pretty(envelope).map_err(std::io::Error::other)?;
		let correction = format!(
			"Review `{}` requested revision {revision}/{} of `{}` (attempt {} of \
			 {limit}).\n\n{reason}\n\nFull review report:\n{report}",
			from.name,
			route.max_revisions,
			route.to,
			spent + 1,
		);
		let target_id = self.intern(&route.to)?;
		let correction_id = self.intern(&correction)?;
		self.trace(
			EventRecord::new(EventKind::ReviewRevision)
				.phase(target_id)
				.owner(source_id)
				.attempt(spent)
				.detail(correction_id)
				.value(revision),
		)?;
		let invalidated = self.invalidate_dependents(index, source)?;
		self.revisions.insert(from.name.clone(), revision);
		self.last_envelope = self.handoff_before(index)?;
		self.states[index].attempts = spent;
		self.states[index].correction = Some(correction.clone());
		Ok(Outcome::Retry { phase: route.to.clone(), attempt: spent + 1, correction, invalidated })
	}

	fn halt_review(
		&mut self,
		phase: &str,
		reason: String,
		revisions: u32,
	) -> Result<Outcome, RunError> {
		let phase_id = self.intern(phase)?;
		let detail = self.intern(&reason)?;
		self.trace(
			EventRecord::new(EventKind::ReviewExhausted)
				.phase(phase_id)
				.detail(detail)
				.value(revisions),
		)?;
		self.halted = true;
		let index = self.phase_index(phase)?;
		self.states[index].status = DispatchStatus::Failed;
		self.states[index].correction = None;
		Ok(Outcome::Aborted { phase: phase.to_owned(), reason })
	}

	/// The envelope the phase at `index` was most recently handed — its
	/// predecessor's standing version, straight from the store. Versioned files
	/// are never overwritten, so this cannot read a later phase's output the
	/// way rereading a shared file could.
	fn handoff_before(&self, index: usize) -> Result<Option<Envelope<Value>>, RunError> {
		let Some(previous) = index
			.checked_sub(1)
			.and_then(|i| self.workflow.phases.get(i))
		else {
			return Ok(None);
		};
		Ok(self
			.current
			.get(&previous.name)
			.and_then(|(_, envelope)| envelope.clone()))
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

fn correction_text(
	phase: &str,
	next_attempt: u32,
	max_attempts: u32,
	violations: &[String],
) -> String {
	let mut text = format!(
		"Your result for phase `{phase}` was rejected (attempt {next_attempt} of \
		 {max_attempts}).\n\nViolations:\n"
	);
	for violation in violations {
		text.push_str("- ");
		text.push_str(violation);
		text.push('\n');
	}
	text.push_str(
		"\nFix exactly these and return a corrected final JSON envelope. Your prior work is intact \
		 — do not start over.",
	);
	text
}

/// The correction a rewound phase receives.
///
/// Names the phase that failed, because the agent being asked to fix it never
/// saw that phase run: without the attribution the request reads as an
/// unexplained demand to redo accepted work.
fn rewind_text(
	from: &str,
	target: &str,
	next_attempt: u32,
	max_attempts: u32,
	violations: &[String],
) -> String {
	let mut text = format!(
		"Phase `{from}` failed after `{target}` was accepted, so the run came back to you (attempt \
		 {next_attempt} of {max_attempts}).\n\nWhat `{from}` reported:\n"
	);
	for violation in violations {
		text.push_str("- ");
		text.push_str(violation);
		text.push('\n');
	}
	text.push_str(
		"\nYour earlier work is on disk and intact. Change what made `{from}` fail, then return a \
		 corrected final JSON envelope."
			.replace("{from}", from)
			.as_str(),
	);
	text
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::{
		envelope::EnvelopeStatus, gate::ArtifactsExist, phase::PhaseKind, test_support::TempDir,
		trace::TraceReader,
	};

	fn workflow() -> Workflow {
		Workflow::new("plan_build", vec![
			PhaseParams::new("plan", PhaseKind::Agent, "planner"),
			PhaseParams::new("build", PhaseKind::Agent, "builder"),
		])
	}

	fn run_in(dir: &TempDir) -> Run {
		Run::new("adw-test", dir.path(), workflow())
	}

	fn ok_envelope(summary: &str) -> Envelope<Value> {
		Envelope {
			status:               EnvelopeStatus::Success,
			summary:              summary.to_owned(),
			artifacts:            Vec::new(),
			notes_for_next_agent: String::new(),
			payload:              Value::Object(serde_json::Map::new()),
		}
	}

	fn review_workflow(max_revisions: u16) -> Workflow {
		Workflow::new("review_loop", vec![
			PhaseParams::new("build", PhaseKind::Agent, "builder"),
			PhaseParams::new("checks", PhaseKind::Code, "test").rewinding_to("build"),
			PhaseParams::new("review", PhaseKind::Agent, "reviewer").on_reject("build", max_revisions),
			PhaseParams::new("deliver", PhaseKind::Code, "git"),
		])
	}

	fn dispatch_serial(run: &mut Run, name: &str) {
		match run.next_step().expect("dispatch") {
			Step::Run { phase, .. } => assert_eq!(phase.name, name),
			// Some scenarios inspect the dispatch before entering this helper.
			Step::Wait => {},
			other => panic!("expected {name}, got {other:?}"),
		}
	}

	fn reach_review(run: &mut Run) {
		dispatch_serial(run, "build");
		assert!(matches!(run.submit_envelope("build", ok_envelope("built")).expect("build"),
			Outcome::Advanced { phase } if phase == "build"));
		dispatch_serial(run, "checks");
		assert!(
			matches!(run.submit_envelope("checks", Envelope::code(true, "checks passed")).expect("checks"),
			Outcome::Advanced { phase } if phase == "checks")
		);
	}

	fn rejected_review() -> Envelope<Value> {
		Envelope {
			payload: serde_json::json!({
				"approved": false,
				"blockers": ["missing authorization"],
				"findings": [{"path": "src/auth.rs", "line": 42, "severity": "critical"}],
				"evidence": ["unauthenticated request returned 200"],
			}),
			..ok_envelope("authorization is incomplete")
		}
	}

	fn reject_review(run: &mut Run, name: &str) -> Outcome {
		dispatch_serial(run, name);
		run.note_review_decision(
			name,
			false,
			"missing authorization: unauthenticated request returned 200",
		)
		.expect("review decision");
		run.submit_envelope(name, rejected_review())
			.expect("review")
	}

	#[test]
	fn review_format_gate_status_and_missing_decisions_correct_the_reviewer() {
		for failure in ["parse", "gate", "status", "missing"] {
			let dir = TempDir::new("review-ordinary-correction");
			let mut run = Run::new("adw", dir.path(), review_workflow(1));
			run.next_step().expect("start");
			reach_review(&mut run);
			dispatch_serial(&mut run, "review");
			if failure != "missing" {
				run.note_review_decision("review", false, "this decision must be drained")
					.expect("decision");
			}
			let outcome = match failure {
				"parse" => run.submit_agent_output("review", "not an envelope"),
				"gate" => {
					run.note_gate_report("review", "verdict_consistent", vec![Check {
						item: "approved".into(),
						ok:   false,
						note: "contradictory verdict".into(),
					}])
					.expect("gate report");
					run.submit_envelope("review", rejected_review())
				},
				"status" => run.submit_envelope("review", Envelope {
					status: EnvelopeStatus::Fail,
					..rejected_review()
				}),
				_ => run.submit_envelope("review", rejected_review()),
			}
			.expect("submission");
			assert!(
				matches!(outcome, Outcome::Retry { phase, attempt: 2, .. } if phase == "review"),
				"{failure}"
			);
			assert!(run.records().iter().all(|record| !record.invalidated));
			// The prior decision must not authorize this attempt.
			dispatch_serial(&mut run, "review");
			assert!(
				matches!(run.submit_envelope("review", ok_envelope("still no decision")).expect("missing"),
				Outcome::Retry { phase, attempt: 3, .. } if phase == "review")
			);
			assert!(matches!(reject_review(&mut run, "review"),
				Outcome::Retry { phase, attempt: 2, .. } if phase == "build"));
			let review = run.records().last().expect("review record");
			assert_eq!(review.status, PhaseStatus::Passed);
			assert_eq!(review.attempts, 3);
			assert!(review.violations.is_empty());
		}
	}

	#[test]
	fn negative_decisions_without_an_explicit_route_do_not_infer_a_builder() {
		let dir = TempDir::new("review-no-route");
		let mut workflow = review_workflow(1);
		workflow.phases[2].on_reject = None;
		let mut run = Run::new("adw", dir.path(), workflow);
		reach_review(&mut run);
		assert!(
			matches!(reject_review(&mut run, "review"), Outcome::Advanced { phase } if phase == "review")
		);
		let Step::Run { phase, .. } = run.next_step().expect("delivery") else {
			panic!("delivery")
		};
		assert_eq!(phase.name, "deliver");
	}

	#[test]
	fn max_attempts_one_allows_only_the_two_explicit_review_revisions() {
		let dir = TempDir::new("review-budget");
		let mut run = Run::new("adw", dir.path(), review_workflow(2)).with_max_attempts(1);
		run.next_step().expect("start");
		for build_attempt in 1..=3 {
			reach_review(&mut run);
			let outcome = reject_review(&mut run, "review");
			if build_attempt < 3 {
				let Outcome::Retry { phase, attempt, correction, .. } = outcome else {
					panic!("revision")
				};
				assert_eq!(phase, "build");
				assert_eq!(attempt, build_attempt + 1);
				assert!(correction.contains("src/auth.rs"));
				assert!(correction.contains("unauthenticated request returned 200"));
				assert!(run.records().iter().all(|record| record.invalidated));
			} else {
				let Outcome::Aborted { phase, reason } = outcome else {
					panic!("exhaustion")
				};
				assert_eq!(phase, "review");
				assert!(reason.contains("missing authorization"));
				assert!(reason.contains("budget exhausted (2/2)"));
			}
		}
		assert_eq!(run.next_step().expect("halt"), Step::Done { accepted: false });
		let reviews: Vec<_> = run
			.records()
			.iter()
			.filter(|record| record.name == "review")
			.collect();
		assert_eq!(reviews.len(), 3);
		assert!(
			reviews
				.iter()
				.all(|record| record.status == PhaseStatus::Passed && record.violations.is_empty())
		);
		assert!(!reviews[2].invalidated);
		assert!(run.records().iter().all(|record| record.name != "deliver"));
	}

	#[test]
	fn a_revision_grants_one_attempt_without_replenishing_spent_builder_attempts() {
		let dir = TempDir::new("review-spent-builder");
		let mut run = Run::new("adw", dir.path(), review_workflow(2)).with_max_attempts(2);
		dispatch_serial(&mut run, "build");
		assert!(matches!(
			run.submit_agent_output("build", "malformed first build")
				.expect("correction"),
			Outcome::Retry { attempt: 2, .. }
		));
		reach_review(&mut run);
		assert!(
			matches!(reject_review(&mut run, "review"), Outcome::Retry { phase, attempt: 3, .. } if phase == "build")
		);
		dispatch_serial(&mut run, "build");
		assert!(
			matches!(run.submit_agent_output("build", "malformed revised build").expect("exhaustion"),
			Outcome::Aborted { phase, .. } if phase == "build")
		);
		assert_eq!(run.records().last().expect("failed build").attempts, 3);
		assert_eq!(run.next_step().expect("halt"), Step::Done { accepted: false });
	}

	#[test]
	fn revised_failing_checks_cannot_reuse_an_invalidated_passing_check() {
		let dir = TempDir::new("review-stale-check");
		let mut run = Run::new("adw", dir.path(), review_workflow(1)).with_max_attempts(1);
		reach_review(&mut run);
		reject_review(&mut run, "review");
		dispatch_serial(&mut run, "build");
		run.submit_envelope("build", ok_envelope("revised build"))
			.expect("build");
		dispatch_serial(&mut run, "checks");
		assert!(
			matches!(run.submit_envelope("checks", Envelope::code(false, "authorization regression")).expect("checks"),
			Outcome::Aborted { phase, .. } if phase == "checks")
		);
		assert_eq!(run.next_step().expect("halt"), Step::Done { accepted: false });
		assert!(run.records().iter().any(|record| record.name == "checks"
			&& record.status == PhaseStatus::Passed
			&& record.invalidated));
		assert!(!run.records().iter().any(|record| !record.invalidated
			&& (record.name == "checks" || record.name == "review")
			&& record.status == PhaseStatus::Passed));
	}

	#[test]
	fn code_rewinds_invalidate_earlier_approvals_too() {
		let dir = TempDir::new("code-stale-approval");
		let mut workflow = review_workflow(1);
		workflow
			.phases
			.insert(3, PhaseParams::new("late_checks", PhaseKind::Code, "test").rewinding_to("build"));
		let mut run = Run::new("adw", dir.path(), workflow).with_max_attempts(2);
		reach_review(&mut run);
		dispatch_serial(&mut run, "review");
		run.note_review_decision("review", true, "approved")
			.expect("decision");
		run.submit_envelope("review", ok_envelope("approved"))
			.expect("review");
		dispatch_serial(&mut run, "late_checks");
		assert!(
			matches!(run.submit_envelope("late_checks", Envelope::code(false, "late failure")).expect("late checks"),
			Outcome::Retry { phase, .. } if phase == "build")
		);
		assert!(run.records().iter().all(|record| record.invalidated));
		dispatch_serial(&mut run, "build");
		run.submit_envelope("build", ok_envelope("revised"))
			.expect("build");
		dispatch_serial(&mut run, "checks");
		assert!(matches!(
			run.submit_envelope("checks", Envelope::code(false, "new regression"))
				.expect("checks"),
			Outcome::Aborted { .. }
		));
		assert!(
			!run
				.records()
				.iter()
				.any(|record| record.name == "review" && !record.invalidated)
		);
		assert_eq!(run.next_step().expect("halt"), Step::Done { accepted: false });
	}

	#[test]
	fn another_review_route_does_not_reset_a_sources_revision_budget() {
		let dir = TempDir::new("review-cumulative-source");
		let mut workflow = review_workflow(1);
		workflow.phases.insert(
			3,
			PhaseParams::new("second_review", PhaseKind::Agent, "reviewer").on_reject("build", 1),
		);
		let mut run = Run::new("adw", dir.path(), workflow).with_max_attempts(1);
		reach_review(&mut run);
		reject_review(&mut run, "review");
		reach_review(&mut run);
		dispatch_serial(&mut run, "review");
		run.note_review_decision("review", true, "approved")
			.expect("decision");
		run.submit_envelope("review", ok_envelope("approved"))
			.expect("review");
		assert!(
			matches!(reject_review(&mut run, "second_review"), Outcome::Retry { phase, attempt: 3, .. } if phase == "build")
		);
		reach_review(&mut run);
		assert!(matches!(reject_review(&mut run, "review"), Outcome::Aborted { phase, reason }
			if phase == "review" && reason.contains("budget exhausted (1/1)")));
	}

	#[test]
	fn replay_restores_full_revision_correction_invalidation_and_cumulative_budget() {
		let dir = TempDir::new("review-replay");
		let trace_dir = dir.path().join("trace");
		let correction;
		{
			let mut run = Run::new("adw", dir.path(), review_workflow(2))
				.with_max_attempts(1)
				.with_tracer(Tracer::create(&trace_dir).expect("tracer"));
			run.next_step().expect("start");
			reach_review(&mut run);
			dispatch_serial(&mut run, "review");
			run.note_review_decision("review", false, "first review requires authorization")
				.expect("decision");
			run.submit_envelope("review", Envelope {
				summary: "first rejection".into(),
				..rejected_review()
			})
			.expect("review");
			reach_review(&mut run);
			let Outcome::Retry { correction: pending, .. } = reject_review(&mut run, "review") else {
				panic!("revision")
			};
			correction = pending;
		}
		let mut resumed =
			Run::resume("adw", dir.path(), review_workflow(2), &trace_dir).expect("resume");
		let Step::Run { phase, attempt, correction: pending, .. } =
			resumed.next_step().expect("step")
		else {
			panic!("builder")
		};
		assert_eq!(phase.name, "build");
		assert_eq!(attempt, 3);
		assert_eq!(pending.as_deref(), Some(correction.as_str()));
		assert!(resumed.records().iter().all(|record| record.invalidated));
		assert_eq!(
			resumed
				.records()
				.iter()
				.filter(|record| record.name == "review")
				.map(|record| record.summary.as_str())
				.collect::<Vec<_>>(),
			vec!["first rejection", "authorization is incomplete"]
		);
		reach_review(&mut resumed);
		assert!(matches!(reject_review(&mut resumed, "review"), Outcome::Aborted { phase, reason }
			if phase == "review" && reason.contains("budget exhausted (2/2)")));
	}

	#[test]
	fn replay_does_not_grant_an_extra_retry_to_a_malformed_revision() {
		let dir = TempDir::new("review-replay-builder-budget");
		let trace_dir = dir.path().join("trace");
		{
			let mut run = Run::new("adw", dir.path(), review_workflow(1))
				.with_max_attempts(1)
				.with_tracer(Tracer::create(&trace_dir).expect("tracer"));
			run.next_step().expect("start");
			reach_review(&mut run);
			reject_review(&mut run, "review");
		}
		let mut resumed =
			Run::resume("adw", dir.path(), review_workflow(1), &trace_dir).expect("resume");
		dispatch_serial(&mut resumed, "build");
		assert!(
			matches!(resumed.submit_agent_output("build", "malformed revision").expect("exhausted"),
			Outcome::Aborted { phase, .. } if phase == "build")
		);
		assert_eq!(resumed.records().last().expect("build").attempts, 2);
	}

	#[test]
	fn a_revision_cannot_wrap_the_builder_budget() {
		let dir = TempDir::new("review-budget-overflow");
		let trace_dir = dir.path().join("trace");
		{
			let mut run = Run::new("adw", dir.path(), review_workflow(1))
				.with_max_attempts(u32::MAX)
				.with_tracer(Tracer::create(&trace_dir).expect("tracer"));
			run.next_step().expect("start");
			reach_review(&mut run);
			assert!(matches!(reject_review(&mut run, "review"), Outcome::Aborted { phase, reason }
				if phase == "review" && reason.contains("missing authorization") && reason.contains("overflow")));
			assert_eq!(run.next_step().expect("halt"), Step::Done { accepted: false });
			assert_eq!(run.records().last().expect("review").status, PhaseStatus::Passed);
		}
		let mut resumed =
			Run::resume("adw", dir.path(), review_workflow(1), &trace_dir).expect("resume");
		assert_eq!(resumed.next_step().expect("halt"), Step::Done { accepted: false });
	}

	#[test]
	fn dependencies_reorder_declaration_order() {
		// Declared last but needed first: the author should not have to hand-sort.
		let workflow = Workflow::new("dag", vec![
			PhaseParams::new("docs", PhaseKind::Agent, "sonic").after(["api"]),
			PhaseParams::new("api", PhaseKind::Agent, "task").after(Vec::<String>::new()),
		]);
		assert_eq!(
			workflow
				.phases
				.iter()
				.map(|p| p.name.as_str())
				.collect::<Vec<_>>(),
			vec!["api", "docs"]
		);
	}

	#[test]
	fn independent_phases_keep_declaration_order() {
		// The tie-break. Anything else and two runs of the same file could
		// execute in different orders, which would make every trace unreplayable.
		let phases = vec![
			PhaseParams::new("api", PhaseKind::Agent, "task"),
			PhaseParams::new("tests", PhaseKind::Agent, "task").after(["api"]),
			PhaseParams::new("docs", PhaseKind::Agent, "sonic").after(["api"]),
			PhaseParams::new("changelog", PhaseKind::Agent, "sonic").after(["tests", "docs"]),
		];
		let first = Workflow::new("dag", phases.clone());
		let again = Workflow::new("dag", phases);
		let names = |w: &Workflow| w.phases.iter().map(|p| p.name.clone()).collect::<Vec<_>>();
		assert_eq!(names(&first), vec!["api", "tests", "docs", "changelog"]);
		assert_eq!(names(&first), names(&again), "the order must be a pure function of the file");
	}

	#[test]
	fn an_unorderable_graph_keeps_declaration_order_instead_of_inventing_one() {
		// A cycle is a config error the caller reports with its file name. What
		// this must never do is silently pick some other order.
		let workflow = Workflow::new("cycle", vec![
			PhaseParams::new("a", PhaseKind::Agent, "task").after(["b"]),
			PhaseParams::new("b", PhaseKind::Agent, "task").after(["a"]),
		]);
		assert_eq!(
			workflow
				.phases
				.iter()
				.map(|p| p.name.as_str())
				.collect::<Vec<_>>(),
			vec!["a", "b"]
		);
	}

	#[test]
	fn a_dependency_run_resumes_against_the_same_order_it_ran() {
		// The sort happens at construction, so a resumed run must derive the
		// identical order or `resume` would match trace names to wrong positions.
		let dir = TempDir::new("run-dag-resume");
		let trace_dir = dir.path().join("trace");
		let build = || {
			Workflow::new("dag", vec![
				PhaseParams::new("second", PhaseKind::Agent, "task").after(["first"]),
				PhaseParams::new("first", PhaseKind::Agent, "task").after(Vec::<String>::new()),
			])
		};
		{
			let mut run = Run::new("adw-dag", dir.path(), build())
				.with_tracer(Tracer::create(&trace_dir).expect("tracer"));
			let step = run.next_step().expect("step");
			match step {
				Step::Run { phase, .. } => {
					assert_eq!(phase.name, "first", "dependency order, not declaration order")
				},
				other => panic!("{other:?}"),
			}
			run.submit_envelope("first", ok_envelope("first done"))
				.expect("submit");
		}

		let mut resumed = Run::resume("adw-dag", dir.path(), build(), &trace_dir).expect("resume");
		match resumed.next_step().expect("step") {
			Step::Run { phase, .. } => assert_eq!(phase.name, "second"),
			other => panic!("{other:?}"),
		}
	}

	#[test]
	fn a_caller_run_gate_blocks_acceptance_and_lands_in_the_trace() {
		// The escape hatch for checks the engine cannot perform. If the caller's
		// verdict did not count, the feature would be decoration.
		let dir = TempDir::new("run-caller-gate");
		let trace_dir = dir.path().join("trace");
		let mut run = run_in(&dir).with_tracer(Tracer::create(&trace_dir).expect("tracer"));

		run.next_step().expect("step");
		run.note_gate_report("plan", "payload_matches_schema", vec![Check {
			item: "approved".to_owned(),
			ok:   false,
			note: "expected boolean, got string".to_owned(),
		}])
		.expect("gate report");
		let outcome = run
			.submit_envelope("plan", ok_envelope("done"))
			.expect("submit");

		assert!(
			matches!(outcome, Outcome::Retry { .. }),
			"a red caller gate must reject: {outcome:?}"
		);
		let mut reader = TraceReader::open(&trace_dir).expect("open");
		let events = reader.read_from(0).expect("read");
		let check = events
			.iter()
			.find(|e| {
				e.kind == EventKind::GateCheck && reader.text(e.gate) == "payload_matches_schema"
			})
			.expect("the caller's gate is traced like any other");
		assert!(!check.ok);
		assert_eq!(reader.text(check.detail), "approved", "the failing field has to be named");
	}

	#[test]
	fn a_caller_run_gate_is_not_carried_into_the_next_attempt() {
		// A report belongs to the attempt that produced it. Carrying one would
		// fail a corrected attempt for the previous one's sin.
		let dir = TempDir::new("run-caller-gate-drain");
		let mut run = run_in(&dir);

		run.next_step().expect("step");
		run.note_gate_report("plan", "payload_matches_schema", vec![Check {
			item: "approved".to_owned(),
			ok:   false,
			note: "wrong type".to_owned(),
		}])
		.expect("gate report");
		run.submit_envelope("plan", ok_envelope("first"))
			.expect("rejected");

		run.next_step().expect("retry");
		let outcome = run
			.submit_envelope("plan", ok_envelope("second"))
			.expect("submit");
		assert!(
			matches!(outcome, Outcome::Advanced { .. }),
			"the stale report must not still be judging: {outcome:?}"
		);
	}

	/// `build` writes, `verify` checks it, and `verify` sends failures back to
	/// `build` instead of re-running its own command.
	fn rewinding_workflow() -> Workflow {
		Workflow::new("build_verify", vec![
			PhaseParams::new("build", PhaseKind::Agent, "builder"),
			PhaseParams::new("verify", PhaseKind::Code, "sh").rewinding_to("build"),
		])
	}

	#[test]
	fn a_failing_code_phase_returns_to_the_phase_that_can_fix_it() {
		let dir = TempDir::new("run-rewind");
		let trace_dir = dir.path().join("trace");
		let mut run = Run::new("adw-rewind", dir.path(), rewinding_workflow())
			.with_tracer(Tracer::create(&trace_dir).expect("tracer"));

		run.next_step().expect("build");
		run.submit_envelope("build", ok_envelope("built"))
			.expect("build passes");
		run.next_step().expect("verify");
		let outcome = run
			.submit_envelope("verify", Envelope::code(false, "2 tests failed"))
			.expect("verify fails");

		// Not a retry of `verify`: re-running the same command cannot change it.
		match outcome {
			Outcome::Retry { phase, attempt, correction, .. } => {
				assert_eq!(
					phase, "build",
					"the run goes back to the phase that can change the outcome"
				);
				assert_eq!(attempt, 2, "the target pays the attempt, so the loop terminates");
				assert!(
					correction.contains("verify"),
					"the agent never saw `verify` run: {correction}"
				);
				assert!(
					correction.contains("2 tests failed"),
					"the failure itself has to reach it: {correction}"
				);
			},
			other => panic!("expected a rewind to build, got {other:?}"),
		}

		let step = run.next_step().expect("step after rewind");
		match step {
			Step::Run { phase, attempt, .. } => {
				assert_eq!(phase.name, "build");
				assert_eq!(attempt, 2);
			},
			other => panic!("expected build again, got {other:?}"),
		}

		let mut reader = TraceReader::open(&trace_dir).expect("open");
		let events = reader.read_from(0).expect("read");
		let rewound = events
			.iter()
			.find(|e| e.kind == EventKind::PhaseRewound)
			.expect("a rewind record");
		assert_eq!(reader.text(rewound.phase), "build", "the record names the target");
		assert_eq!(reader.text(rewound.owner), "verify", "and the phase that sent it back");
	}

	#[test]
	fn a_rewind_loop_halts_on_the_target_budget() {
		let dir = TempDir::new("run-rewind-budget");
		let mut run = Run::new("adw-budget", dir.path(), rewinding_workflow()).with_max_attempts(2);

		// `verify` never passes, so the only thing that can stop this is the
		// budget of the phase being rewound to.
		let mut rewinds = 0;
		let outcome = loop {
			let step = run.next_step().expect("step");
			if let Step::Done { accepted } = step {
				panic!("run settled with accepted={accepted} instead of halting");
			}
			let phase = match &step {
				Step::Run { phase, .. } => phase.name.clone(),
				Step::Done { .. } | Step::Wait => unreachable!(),
			};
			let outcome = if phase == "build" {
				run.submit_envelope("build", ok_envelope("built"))
					.expect("build")
			} else {
				run.submit_envelope("verify", Envelope::code(false, "still red"))
					.expect("verify")
			};
			if let Outcome::Retry { phase: ref target, .. } = outcome {
				if target == "build" {
					rewinds += 1;
				}
			}
			if let Outcome::Aborted { .. } = outcome {
				break outcome;
			}
			assert!(rewinds < 10, "a rewind loop with no budget would never terminate");
		};

		assert!(matches!(outcome, Outcome::Aborted { .. }), "{outcome:?}");
		// `build` spent attempt 1 before the first rewind and attempt 2 after it,
		// so a budget of 2 buys exactly one rewind. The rewind is not free: it
		// costs the target, which is what makes the loop finite.
		assert_eq!(rewinds, 1);
	}

	#[test]
	fn a_run_resumed_after_a_rewind_continues_at_the_target() {
		let dir = TempDir::new("run-rewind-resume");
		let trace_dir = dir.path().join("trace");
		{
			let mut run = Run::new("adw-rr", dir.path(), rewinding_workflow())
				.with_tracer(Tracer::create(&trace_dir).expect("tracer"));
			run.next_step().expect("build");
			run.submit_envelope("build", ok_envelope("built"))
				.expect("build passes");
			run.next_step().expect("verify");
			run.submit_envelope("verify", Envelope::code(false, "red"))
				.expect("verify fails");
			// Process dies here, mid-rewind.
		}

		let resumed =
			Run::resume("adw-rr", dir.path(), rewinding_workflow(), &trace_dir).expect("resume");
		let mut resumed = resumed;
		match resumed.next_step().expect("step") {
			Step::Run { phase, attempt, .. } => {
				assert_eq!(
					phase.name, "build",
					"counting passed phases would have resumed at `verify`"
				);
				assert_eq!(attempt, 2, "and with the budget the target had already spent");
			},
			other => panic!("expected build, got {other:?}"),
		}
	}

	#[test]
	fn a_provider_that_reports_no_usage_is_marked_unaccounted() {
		let dir = TempDir::new("run-unaccounted");
		let trace_dir = dir.path().join("trace");
		let mut run = run_in(&dir).with_tracer(Tracer::create(&trace_dir).expect("tracer"));

		run.next_step().expect("step");
		// `omniroute/auto` returns an all-zero usage record. A model turn cannot
		// cost nothing, so this must not read as a free phase.
		run.note_phase_tokens("plan", "sonic", 0, None)
			.expect("note");
		run.submit_envelope("plan", ok_envelope("done"))
			.expect("submit");

		let mut reader = TraceReader::open(&trace_dir).expect("open");
		let events = reader.read_from(0).expect("read");
		let charge = events
			.iter()
			.find(|e| e.kind == EventKind::PhaseTokens)
			.expect("a charge record");
		assert_eq!(charge.value, 0);
		assert!(!charge.ok, "zero tokens means unaccounted, and a reader has to be able to see that");
	}

	#[test]
	fn phases_run_in_order_and_the_run_is_accepted() {
		let dir = TempDir::new("run-happy");
		let mut run = run_in(&dir);

		let Step::Run { phase, attempt, correction, .. } = run.next_step().expect("step") else {
			panic!("expected a step");
		};
		assert_eq!(phase.name, "plan");
		assert_eq!(attempt, 1);
		assert!(correction.is_none());

		assert_eq!(
			run.submit_envelope("plan", ok_envelope("planned"))
				.expect("submit"),
			Outcome::Advanced { phase: "plan".into() }
		);

		let Step::Run { phase, .. } = run.next_step().expect("step") else {
			panic!("expected a step")
		};
		assert_eq!(phase.name, "build");
		run.submit_envelope("build", ok_envelope("built"))
			.expect("submit");

		assert_eq!(run.next_step().expect("step"), Step::Done { accepted: true });
		assert_eq!(run.records().len(), 2);
		assert!(
			run.records()
				.iter()
				.all(|r| r.status == PhaseStatus::Passed)
		);
	}

	#[test]
	fn a_failed_gate_re_runs_the_same_phase_with_the_violation_named() {
		let dir = TempDir::new("run-gate");
		let mut run = run_in(&dir);
		run.register_gates("plan", vec![Box::new(ArtifactsExist)]);

		run.next_step().expect("step");
		let claimed = Envelope { artifacts: vec!["specs/plan.md".into()], ..ok_envelope("planned") };
		let outcome = run.submit_envelope("plan", claimed).expect("submit");

		let Outcome::Retry { attempt, correction, .. } = outcome else {
			panic!("expected a retry, got {outcome:?}");
		};
		assert_eq!(attempt, 2);
		assert!(correction.contains("specs/plan.md"), "{correction}");
		assert!(correction.contains("artifacts_exist"), "{correction}");

		// Same phase, next attempt, feedback carried into the same session.
		let Step::Run { phase, attempt, correction, .. } = run.next_step().expect("step") else {
			panic!("expected a step");
		};
		assert_eq!(phase.name, "plan");
		assert_eq!(attempt, 2);
		assert!(
			correction
				.expect("correction carried")
				.contains("specs/plan.md")
		);

		// The agent writes the file it claimed; the same claim now passes.
		dir.write("specs/plan.md", "# plan");
		let claimed = Envelope { artifacts: vec!["specs/plan.md".into()], ..ok_envelope("planned") };
		assert_eq!(run.submit_envelope("plan", claimed).expect("submit"), Outcome::Advanced {
			phase: "plan".into(),
		});
		assert_eq!(run.records()[0].attempts, 2, "both attempts are recorded");
	}

	#[test]
	fn gate_accept_fires_only_for_accepted_attempts() {
		// A stateful gate stages evidence during `run` and commits in `accept`.
		// If `accept` fired for a rejected attempt, a rejected envelope's
		// declarations would authorize later evaluations — the exact leak the
		// hook exists to close.
		use std::sync::{
			Arc,
			atomic::{AtomicUsize, Ordering},
		};
		struct StagingGate {
			accepts: Arc<AtomicUsize>,
		}
		impl Gate for StagingGate {
			fn name(&self) -> &'static str {
				"staging"
			}

			fn run(&self, _envelope: &Envelope<Value>, _ctx: &GateCtx<'_>) -> GateReport {
				let mut report = GateReport::new(self.name());
				report.push(".", true, "staged");
				report
			}

			fn accept(&self, _envelope: &Envelope<Value>, _ctx: &GateCtx<'_>) {
				self.accepts.fetch_add(1, Ordering::SeqCst);
			}
		}

		let dir = TempDir::new("run-gate-accept");
		let mut run = run_in(&dir);
		let accepts = Arc::new(AtomicUsize::new(0));
		run.register_gates("plan", vec![
			Box::new(ArtifactsExist),
			Box::new(StagingGate { accepts: Arc::clone(&accepts) }),
		]);

		run.next_step().expect("step");
		let claimed = Envelope { artifacts: vec!["missing.md".into()], ..ok_envelope("planned") };
		let outcome = run.submit_envelope("plan", claimed).expect("submit");
		assert!(matches!(outcome, Outcome::Retry { .. }), "sibling gate must reject: {outcome:?}");
		assert_eq!(accepts.load(Ordering::SeqCst), 0, "a rejected attempt must not commit");

		run.next_step().expect("step");
		dir.write("plan.md", "# plan");
		let accepted = Envelope { artifacts: vec!["plan.md".into()], ..ok_envelope("planned") };
		assert_eq!(run.submit_envelope("plan", accepted).expect("submit"), Outcome::Advanced {
			phase: "plan".into(),
		});
		assert_eq!(accepts.load(Ordering::SeqCst), 1, "the accepted attempt commits exactly once");
	}

	#[test]
	fn unparseable_output_is_a_correction_not_a_crash() {
		let dir = TempDir::new("run-parse");
		let mut run = run_in(&dir);
		run.next_step().expect("step");

		let outcome = run
			.submit_agent_output("plan", "I finished the plan, trust me.")
			.expect("submit");
		let Outcome::Retry { correction, .. } = outcome else {
			panic!("expected retry, got {outcome:?}")
		};
		assert!(correction.contains("no JSON object"), "{correction}");
	}

	#[test]
	fn a_self_reported_failure_is_rejected_even_with_green_gates() {
		let dir = TempDir::new("run-selffail");
		let mut run = run_in(&dir);
		run.next_step().expect("step");

		let outcome = run
			.submit_envelope("plan", Envelope {
				status: EnvelopeStatus::Fail,
				..ok_envelope("could not read the spec")
			})
			.expect("submit");
		let Outcome::Retry { correction, .. } = outcome else {
			panic!("expected retry, got {outcome:?}")
		};
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
			run.submit_envelope("plan", env).expect("submit");
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
			resumed
				.previous_envelope()
				.expect("handoff")
				.notes_for_next_agent,
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
			run.submit_envelope("plan", claim).expect("submit");
		}

		let mut resumed =
			Run::resume("adw-test", dir.path(), workflow(), &trace_dir).expect("resume");
		let Step::Run { phase, attempt, .. } = resumed.next_step().expect("step") else {
			panic!("expected a step");
		};
		// A resumed run must not hand back a fresh budget; that would let a
		// crash-loop retry forever.
		assert_eq!(phase.name, "plan");
		assert_eq!(attempt, 2);
	}

	#[test]
	fn a_resumed_retry_carries_the_same_in_place_correction_the_live_run_had() {
		// Regression: only rewinds and review revisions traced their diagnostic,
		// so a resumed retry after a plain gate rejection dispatched with no
		// correction — the agent was asked again without being told what failed.
		let dir = TempDir::new("run-resume-correction");
		let trace_dir = dir.path().join("trace");
		let live_correction;
		{
			let mut run = run_in(&dir).with_tracer(Tracer::create(&trace_dir).expect("tracer"));
			run.register_gates("plan", vec![Box::new(ArtifactsExist)]);
			run.next_step().expect("step");
			let claim = Envelope { artifacts: vec!["missing.md".into()], ..ok_envelope("planned") };
			let Outcome::Retry { correction, .. } =
				run.submit_envelope("plan", claim).expect("submit")
			else {
				panic!("expected a retry");
			};
			live_correction = correction;
			// The process dies here, before the retry dispatches.
		}

		let mut resumed =
			Run::resume("adw-test", dir.path(), workflow(), &trace_dir).expect("resume");
		let Step::Run { phase, attempt, correction, .. } = resumed.next_step().expect("step") else {
			panic!("expected a step");
		};
		assert_eq!(phase.name, "plan");
		assert_eq!(attempt, 2);
		assert_eq!(
			correction.as_deref(),
			Some(live_correction.as_str()),
			"the retry must carry the exact diagnostic the live run produced"
		);
	}

	#[test]
	fn an_accepted_retry_leaves_no_stale_correction_after_resume() {
		let dir = TempDir::new("run-resume-correction-cleared");
		let trace_dir = dir.path().join("trace");
		{
			let mut run = run_in(&dir).with_tracer(Tracer::create(&trace_dir).expect("tracer"));
			run.register_gates("plan", vec![Box::new(ArtifactsExist)]);
			run.next_step().expect("step");
			let claim = Envelope { artifacts: vec!["plan.md".into()], ..ok_envelope("planned") };
			run.submit_envelope("plan", claim).expect("rejected");
			run.next_step().expect("retry");
			dir.write("plan.md", "# plan");
			let claim = Envelope { artifacts: vec!["plan.md".into()], ..ok_envelope("planned") };
			run.submit_envelope("plan", claim).expect("accepted");
			// Dies mid-`build`.
		}

		let mut resumed =
			Run::resume("adw-test", dir.path(), workflow(), &trace_dir).expect("resume");
		let Step::Run { phase, correction, .. } = resumed.next_step().expect("step") else {
			panic!("expected a step");
		};
		assert_eq!(phase.name, "build");
		assert!(
			correction.is_none(),
			"the plan correction was consumed by its accepted retry: {correction:?}"
		);
	}

	#[test]
	fn resuming_a_trace_from_another_workflow_is_refused() {
		let dir = TempDir::new("run-resume-mismatch");
		let trace_dir = dir.path().join("trace");
		{
			let mut run = run_in(&dir).with_tracer(Tracer::create(&trace_dir).expect("tracer"));
			run.next_step().expect("step");
		}
		let other = Workflow::new("something_else", vec![PhaseParams::new(
			"plan",
			PhaseKind::Agent,
			"planner",
		)]);
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
			run.submit_envelope("plan", ok_envelope("planned"))
				.expect("submit");
			run.next_step().expect("step");
			run.submit_envelope("build", ok_envelope("built"))
				.expect("submit");
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
		run.note_panel_opinion("plan", "scout", true, 1_200, None)
			.expect("note");
		run.note_panel_opinion("plan", "reviewer", false, 0, None)
			.expect("note");
		run.submit_envelope("plan", ok_envelope("fused"))
			.expect("submit");

		let mut reader = TraceReader::open(&trace_dir).expect("open");
		let events = reader.read_from(0).expect("read");
		let panel: Vec<_> = events
			.iter()
			.filter(|e| e.kind == EventKind::PanelOpinion)
			.collect();
		assert_eq!(panel.len(), 2);
		assert_eq!(reader.text(panel[0].owner), "scout");
		assert!(panel[0].ok);
		assert_eq!(panel[0].value, 1_200, "tokens make two models comparable");
		assert_eq!(reader.text(panel[1].owner), "reviewer");
		assert!(!panel[1].ok);
		// Still one phase to the engine: it settles on the fuser's envelope.
		assert_eq!(run.records().len(), 1);
		assert_eq!(
			events
				.iter()
				.filter(|e| e.kind == EventKind::PhaseFinished)
				.count(),
			1
		);
	}

	#[test]
	fn rejected_attempts_are_charged_for_the_tokens_they_spent() {
		let dir = TempDir::new("run-tokens");
		let trace_dir = dir.path().join("trace");
		let mut run = run_in(&dir).with_tracer(Tracer::create(&trace_dir).expect("tracer"));

		// A phase that needed two tries. The first one still cost money.
		run.next_step().expect("step");
		run.note_phase_tokens("plan", "task", 900, None)
			.expect("note");
		run.submit_agent_output("plan", "no envelope here")
			.expect("submit");
		run.next_step().expect("retry");
		run.note_phase_tokens("plan", "task", 1_500, None)
			.expect("note");
		run.submit_envelope("plan", ok_envelope("done"))
			.expect("submit");

		let mut reader = TraceReader::open(&trace_dir).expect("open");
		let events = reader.read_from(0).expect("read");
		let charges: Vec<_> = events
			.iter()
			.filter(|e| e.kind == EventKind::PhaseTokens)
			.collect();

		assert_eq!(
			charges.len(),
			2,
			"the rejected attempt must be charged, not just the accepted one"
		);
		assert_eq!(charges[0].value, 900);
		assert_eq!(charges[0].attempt, 1);
		assert_eq!(charges[1].value, 1_500);
		assert_eq!(
			charges[1].attempt, 2,
			"charges are per attempt, so a retry is visible as a second cost"
		);
		assert_eq!(reader.text(charges[0].owner), "task");
		let total: u32 = charges.iter().map(|e| e.value).sum();
		assert_eq!(total, 2_400);
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
		let outcome = run
			.submit_envelope("test", Envelope::code(false, "1 test failed"))
			.expect("submit");
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
		assert!(matches!(
			run.submit_envelope("plan", claim()).expect("submit"),
			Outcome::Retry { .. }
		));
		run.next_step().expect("step");
		assert!(matches!(
			run.submit_envelope("plan", claim()).expect("submit"),
			Outcome::Aborted { .. }
		));

		assert_eq!(run.next_step().expect("step"), Step::Done { accepted: false });
		assert!(matches!(
			run.submit_envelope("plan", ok_envelope("late")),
			Err(RunError::NoActiveStep)
		));
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
		run.submit_envelope("plan", env).expect("submit");

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

		let outcome = run
			.submit_envelope("test", Envelope::code(false, "2 tests failed"))
			.expect("submit");
		let Outcome::Retry { correction, .. } = outcome else {
			panic!("expected retry, got {outcome:?}")
		};
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
		run.submit_envelope("plan", claimed).expect("submit");
		run.finish(true, "").expect("finish");

		let mut reader = TraceReader::open(&trace_dir).expect("open");
		let events = reader.read_from(0).expect("read");
		let kinds: Vec<EventKind> = events.iter().map(|e| e.kind).collect();
		assert_eq!(kinds, [
			EventKind::RunStarted,
			EventKind::PhaseStarted,
			EventKind::GateCheck,
			EventKind::PhaseFinished,
			EventKind::RunFinished,
		]);

		let gate_check = events
			.iter()
			.find(|e| e.kind == EventKind::GateCheck)
			.expect("gate check");
		assert!(gate_check.ok);
		assert_eq!(reader.text(gate_check.phase), "plan");
		assert_eq!(reader.text(gate_check.gate), "artifacts_exist");
		assert_eq!(reader.text(gate_check.detail), "specs/plan.md");
		assert!(!events.last().expect("last").ok, "unfinished build cannot be accepted");
	}

	#[test]
	fn a_rejected_attempt_is_traced_with_its_violation_count() {
		let dir = TempDir::new("run-trace-reject");
		let trace_dir = dir.path().join("trace");
		let mut run = run_in(&dir).with_tracer(Tracer::create(&trace_dir).expect("tracer"));
		run.register_gates("plan", vec![Box::new(ArtifactsExist)]);

		run.next_step().expect("step");
		let claimed =
			Envelope { artifacts: vec!["a.md".into(), "b.md".into()], ..ok_envelope("planned") };
		run.submit_envelope("plan", claimed).expect("submit");

		let mut reader = TraceReader::open(&trace_dir).expect("open");
		let events = reader.read_from(0).expect("read");
		let failed_checks = events
			.iter()
			.filter(|e| e.kind == EventKind::GateCheck && !e.ok)
			.count();
		assert_eq!(failed_checks, 2, "one record per examined item");

		let rejected = events
			.iter()
			.find(|e| e.kind == EventKind::PhaseRejected)
			.expect("rejection");
		assert_eq!(rejected.value, 2, "violation count");
		assert_eq!(rejected.attempt, 1);
	}

	/// plan → fetch (code) → build, where build declares what it consumes.
	fn consuming_workflow() -> Workflow {
		Workflow::new("io", vec![
			PhaseParams::new("plan", PhaseKind::Agent, "planner"),
			PhaseParams::new("fetch", PhaseKind::Code, "git"),
			PhaseParams::new("build", PhaseKind::Agent, "builder").consuming(["plan"]),
		])
	}

	#[test]
	fn a_declared_input_survives_an_intervening_acceptance() {
		// Positional handoff would give `build` the fetch envelope. The declared
		// input must pin the plan's, and the trace must say which version.
		let dir = TempDir::new("run-input-pin");
		let trace_dir = dir.path().join("trace");
		let mut run = Run::new("adw", dir.path(), consuming_workflow())
			.with_tracer(Tracer::create(&trace_dir).expect("tracer"));

		run.next_step().expect("plan");
		let mut plan = ok_envelope("planned");
		plan.notes_for_next_agent = "theme tokens live in src/theme.ts".into();
		run.submit_envelope("plan", plan).expect("plan passes");
		let Step::Run { phase, inputs, .. } = run.next_step().expect("fetch") else {
			panic!("fetch step")
		};
		assert_eq!(phase.name, "fetch");
		assert!(inputs.is_empty(), "no declared inputs, no selection");
		run.submit_envelope("fetch", Envelope::code(true, "fetched"))
			.expect("fetch passes");

		let Step::Run { phase, inputs, .. } = run.next_step().expect("build") else {
			panic!("build step")
		};
		assert_eq!(phase.name, "build");
		assert_eq!(inputs.len(), 1);
		assert_eq!(inputs[0].phase, "plan");
		assert_eq!(inputs[0].version, 1);
		assert_eq!(inputs[0].envelope.notes_for_next_agent, "theme tokens live in src/theme.ts");
		// The positional handoff still tracks the last acceptance — a driver for
		// a consuming phase reads the step's inputs, not the handoff.
		assert_eq!(run.previous_envelope().expect("handoff").summary, "fetched");

		let mut reader = TraceReader::open(&trace_dir).expect("open");
		let events = reader.read_from(0).expect("read");
		let selected = events
			.iter()
			.find(|e| e.kind == EventKind::InputSelected)
			.expect("a selection record");
		assert_eq!(reader.text(selected.phase), "build");
		assert_eq!(reader.text(selected.owner), "plan");
		assert_eq!(selected.attempt, 1);
		assert_eq!(selected.value, 1, "version 1");
	}

	#[test]
	fn a_join_receives_every_declared_output_by_name_and_version() {
		let dir = TempDir::new("run-input-join");
		let mut run = Run::new(
			"adw",
			dir.path(),
			Workflow::new("join", vec![
				PhaseParams::new("api", PhaseKind::Agent, "task"),
				PhaseParams::new("docs", PhaseKind::Agent, "sonic"),
				PhaseParams::new("release", PhaseKind::Agent, "task").consuming(["api", "docs"]),
			]),
		);
		run.next_step().expect("api");
		run.submit_envelope("api", ok_envelope("api done"))
			.expect("api");
		run.next_step().expect("docs");
		run.submit_envelope("docs", ok_envelope("docs done"))
			.expect("docs");

		let Step::Run { phase, inputs, .. } = run.next_step().expect("release") else {
			panic!("release step")
		};
		assert_eq!(phase.name, "release");
		assert_eq!(
			inputs
				.iter()
				.map(|input| (input.phase.as_str(), input.version, input.envelope.summary.as_str()))
				.collect::<Vec<_>>(),
			vec![("api", 1, "api done"), ("docs", 1, "docs done")],
			"declared order, each producer's own accepted output"
		);
	}

	/// The review workflow with the reviewer declaring what it reads.
	fn consuming_review_workflow(max_revisions: u16) -> Workflow {
		let mut workflow = review_workflow(max_revisions);
		workflow.phases[2].inputs = vec!["build".into()];
		workflow
	}

	#[test]
	fn a_revision_supersedes_the_old_version_and_reselects_the_new_one() {
		let dir = TempDir::new("run-input-revision");
		let trace_dir = dir.path().join("trace");
		let mut run = Run::new("adw", dir.path(), consuming_review_workflow(1))
			.with_tracer(Tracer::create(&trace_dir).expect("tracer"));
		run.next_step().expect("start");
		reach_review(&mut run);
		let Step::Run { phase, inputs, .. } = run.next_step().expect("review") else {
			panic!("review step")
		};
		assert_eq!(phase.name, "review");
		assert_eq!((inputs[0].phase.as_str(), inputs[0].version), ("build", 1));

		assert!(matches!(reject_review(&mut run, "review"),
			Outcome::Retry { phase, .. } if phase == "build"));
		run.next_step().expect("build retry");
		run.submit_envelope("build", ok_envelope("revised build"))
			.expect("build v2");
		run.next_step().expect("checks");
		run.submit_envelope("checks", Envelope::code(true, "checks passed"))
			.expect("checks");

		let Step::Run { phase, inputs, .. } = run.next_step().expect("review again") else {
			panic!("review step")
		};
		assert_eq!(phase.name, "review");
		assert_eq!(inputs[0].version, 2, "the revision's output, not the superseded one");
		assert_eq!(inputs[0].envelope.summary, "revised build");
		assert!(
			run.records()
				.iter()
				.any(|record| record.name == "build" && record.invalidated),
			"the first acceptance is superseded evidence"
		);

		let mut reader = TraceReader::open(&trace_dir).expect("open");
		let versions: Vec<u32> = reader
			.read_from(0)
			.expect("read")
			.iter()
			.filter(|e| e.kind == EventKind::InputSelected)
			.map(|e| e.value)
			.collect();
		assert_eq!(versions, vec![1, 2], "both selections are in the trace");
	}

	#[test]
	fn an_invalidated_producer_without_a_successor_is_refused_not_substituted() {
		// Unreachable through honest driving — the engine re-runs the producer
		// before its consumer dispatches — so this guards the caller-bypass path
		// the same way `revise` re-validates an already-validated route.
		let dir = TempDir::new("run-input-invalidated");
		let mut run = Run::new("adw", dir.path(), consuming_review_workflow(1));
		run.next_step().expect("start");
		reach_review(&mut run);
		run.next_step().expect("review");
		assert!(matches!(reject_review(&mut run, "review"),
			Outcome::Retry { phase, .. } if phase == "build"));

		let review = run.workflow.phases[2].clone();
		let Err(RunError::Mismatch(reason)) = run.select_inputs(&review) else {
			panic!("selection must refuse an invalidated producer");
		};
		assert!(reason.contains("\"review\""), "{reason}");
		assert!(reason.contains("\"build\""), "{reason}");
		assert!(reason.contains("version 1"), "{reason}");
		assert!(reason.contains("invalidated"), "{reason}");
	}

	fn resumable_consuming_workflow() -> Workflow {
		Workflow::new("io", vec![
			PhaseParams::new("plan", PhaseKind::Agent, "planner"),
			PhaseParams::new("build", PhaseKind::Agent, "builder").consuming(["plan"]),
		])
	}

	#[test]
	fn resume_rebuilds_the_same_selection_and_refuses_lost_evidence() {
		let dir = TempDir::new("run-input-resume");
		let trace_dir = dir.path().join("trace");
		{
			let mut run = Run::new("adw", dir.path(), resumable_consuming_workflow())
				.with_tracer(Tracer::create(&trace_dir).expect("tracer"));
			run.next_step().expect("plan");
			let mut plan = ok_envelope("planned");
			plan.notes_for_next_agent = "read specs/plan.md first".into();
			run.submit_envelope("plan", plan).expect("plan passes");
			run.next_step().expect("build dispatched");
			// The process dies here, mid-`build`.
		}

		let mut resumed = Run::resume("adw", dir.path(), resumable_consuming_workflow(), &trace_dir)
			.expect("resume");
		let Step::Run { phase, inputs, .. } = resumed.next_step().expect("step") else {
			panic!("build step")
		};
		assert_eq!(phase.name, "build");
		assert_eq!((inputs[0].phase.as_str(), inputs[0].version), ("plan", 1));
		assert_eq!(inputs[0].envelope.notes_for_next_agent, "read specs/plan.md first");

		let envelope_file = trace_dir.join(ENVELOPE_DIR).join("plan.1.json");
		std::fs::write(&envelope_file, "{ not json").expect("corrupt");
		let Err(RunError::Mismatch(reason)) =
			Run::resume("adw", dir.path(), resumable_consuming_workflow(), &trace_dir)
		else {
			panic!("a corrupted consumed envelope must refuse resume");
		};
		assert!(reason.contains("\"build\""), "{reason}");
		assert!(reason.contains("\"plan\""), "{reason}");
		assert!(reason.contains("version 1"), "{reason}");

		std::fs::remove_file(&envelope_file).expect("delete");
		let Err(RunError::Mismatch(reason)) =
			Run::resume("adw", dir.path(), resumable_consuming_workflow(), &trace_dir)
		else {
			panic!("a deleted consumed envelope must refuse resume");
		};
		assert!(reason.contains("\"build\""), "{reason}");
		assert!(reason.contains("\"plan\""), "{reason}");
		assert!(reason.contains("version 1"), "{reason}");
		assert!(reason.contains("missing"), "{reason}");
	}

	#[test]
	fn declared_inputs_are_validated_before_anything_runs() {
		// Mirrors validate_review_routes: the caller's file validation can be
		// bypassed, so the engine refuses the same shapes itself.
		let cases: Vec<(&str, Workflow)> = vec![
			(
				"unknown producer",
				Workflow::new("bad", vec![
					PhaseParams::new("build", PhaseKind::Agent, "task").consuming(["ghost"]),
				]),
			),
			(
				"self input",
				Workflow::new("bad", vec![
					PhaseParams::new("build", PhaseKind::Agent, "task").consuming(["build"]),
				]),
			),
			(
				"duplicate input",
				Workflow::new("bad", vec![
					PhaseParams::new("plan", PhaseKind::Agent, "task"),
					PhaseParams::new("build", PhaseKind::Agent, "task").consuming(["plan", "plan"]),
				]),
			),
			(
				"producer does not execute earlier",
				Workflow::new("bad", vec![
					PhaseParams::new("plan", PhaseKind::Agent, "task").consuming(["build"]),
					PhaseParams::new("build", PhaseKind::Agent, "task"),
				]),
			),
			(
				"input outside the dependency closure",
				Workflow::new("bad", vec![
					PhaseParams::new("api", PhaseKind::Agent, "task"),
					PhaseParams::new("docs", PhaseKind::Agent, "task").after(Vec::<String>::new()),
					PhaseParams::new("release", PhaseKind::Agent, "task")
						.after(["docs"])
						.consuming(["api"]),
				]),
			),
		];
		for (case, workflow) in cases {
			let dir = TempDir::new("run-input-validate");
			let mut run = Run::new("adw", dir.path(), workflow);
			assert!(
				matches!(run.next_step(), Err(RunError::Mismatch(_))),
				"{case} must be refused at start"
			);
		}
	}

	/// Two independent producers feeding a join that names both as inputs.
	fn fork_workflow() -> Workflow {
		Workflow::new("fork", vec![
			PhaseParams::new("a", PhaseKind::Agent, "task"),
			PhaseParams::new("b", PhaseKind::Agent, "task").after(Vec::<String>::new()),
			PhaseParams::new("join", PhaseKind::Agent, "task")
				.after(["a", "b"])
				.consuming(["a", "b"]),
		])
	}

	#[test]
	fn independent_phases_dispatch_before_either_submits_and_the_join_waits_for_both() {
		let dir = TempDir::new("run-dag-fork");
		let mut run = Run::new("adw", dir.path(), fork_workflow());

		let Step::Run { phase, .. } = run.next_step().expect("first dispatch") else {
			panic!("expected a")
		};
		assert_eq!(phase.name, "a");
		// `b` does not depend on `a`: it must dispatch while `a` is in flight,
		// or the DAG is a serial chain with extra syntax.
		let Step::Run { phase, .. } = run.next_step().expect("second dispatch") else {
			panic!("expected b")
		};
		assert_eq!(phase.name, "b");
		// Both flights active, the join blocked: wait, never error or settle.
		assert_eq!(run.next_step().expect("gap"), Step::Wait);

		run.submit_envelope("a", ok_envelope("a done")).expect("a");
		// One accepted producer is not enough for the join.
		assert_eq!(run.next_step().expect("still gated"), Step::Wait);

		run.submit_envelope("b", ok_envelope("b done")).expect("b");
		let Step::Run { phase, inputs, .. } = run.next_step().expect("join") else {
			panic!("join step")
		};
		assert_eq!(phase.name, "join");
		assert_eq!(
			inputs
				.iter()
				.map(|input| (input.phase.as_str(), input.version, input.envelope.summary.as_str()))
				.collect::<Vec<_>>(),
			vec![("a", 1, "a done"), ("b", 1, "b done")],
			"the join dispatches with both named producers' accepted outputs resolved"
		);
		run.submit_envelope("join", ok_envelope("joined"))
			.expect("join");
		assert_eq!(run.next_step().expect("settled"), Step::Done { accepted: true });
	}

	#[test]
	fn submissions_are_refused_for_any_phase_that_is_not_running() {
		let dir = TempDir::new("run-dag-not-running");
		let mut run = Run::new("adw", dir.path(), fork_workflow());
		dispatch_serial(&mut run, "a");

		// Pending, never dispatched: nothing is in flight to report against.
		assert!(matches!(
			run.submit_envelope("join", ok_envelope("early")),
			Err(RunError::NoActiveStep)
		));
		assert!(matches!(run.note_phase_tokens("b", "task", 10, None), Err(RunError::NoActiveStep)));
		// A name outside the workflow is a caller bug, named as such.
		assert!(matches!(
			run.submit_envelope("ghost", ok_envelope("noise")),
			Err(RunError::Mismatch(_))
		));

		run.submit_envelope("a", ok_envelope("a done")).expect("a");
		// Passed is not running either: a second submission must not double-accept.
		assert!(matches!(
			run.submit_envelope("a", ok_envelope("again")),
			Err(RunError::NoActiveStep)
		));
	}

	#[test]
	fn a_revision_invalidates_the_dependent_closure_but_not_an_unrelated_branch() {
		let dir = TempDir::new("run-dag-closure");
		let mut run = Run::new(
			"adw",
			dir.path(),
			Workflow::new("closure", vec![
				PhaseParams::new("build", PhaseKind::Agent, "builder"),
				PhaseParams::new("docs", PhaseKind::Agent, "sonic").after(Vec::<String>::new()),
				PhaseParams::new("review", PhaseKind::Agent, "reviewer")
					.after(["build"])
					.on_reject("build", 1),
			]),
		);
		dispatch_serial(&mut run, "build");
		dispatch_serial(&mut run, "docs");
		run.submit_envelope("build", ok_envelope("built"))
			.expect("build");
		run.submit_envelope("docs", ok_envelope("documented"))
			.expect("docs");

		let Outcome::Retry { phase, invalidated, .. } = reject_review(&mut run, "review") else {
			panic!("expected a revision")
		};
		assert_eq!(phase, "build");
		assert_eq!(
			invalidated,
			["build", "review"],
			"the outcome names the target and its transitive dependents, nothing else"
		);
		assert!(
			run.records().iter().any(|record| record.name == "docs"
				&& record.status == PhaseStatus::Passed
				&& !record.invalidated),
			"an unrelated passed branch keeps its record"
		);
		assert!(
			run.records()
				.iter()
				.filter(|record| record.name != "docs")
				.all(|record| record.invalidated)
		);
	}

	#[test]
	fn a_completed_independent_flight_still_submits_after_a_sibling_exhausts() {
		let dir = TempDir::new("run-dag-late-submit");
		let trace_dir = dir.path().join("trace");
		let mut run = Run::new(
			"adw",
			dir.path(),
			Workflow::new("late", vec![
				PhaseParams::new("a", PhaseKind::Agent, "task"),
				PhaseParams::new("b", PhaseKind::Code, "bun").after(Vec::<String>::new()),
			]),
		)
		.with_max_attempts(1)
		.with_tracer(Tracer::create(&trace_dir).expect("tracer"));
		dispatch_serial(&mut run, "a");
		dispatch_serial(&mut run, "b");

		assert!(matches!(
			run.submit_envelope("b", Envelope::code(false, "1 test failed")).expect("b"),
			Outcome::Aborted { phase, .. } if phase == "b"
		));
		// Halted, but `a` is still in flight: wait for it instead of settling —
		// discarding a finished agent's work because a sibling died is waste.
		assert_eq!(run.next_step().expect("halted gap"), Step::Wait);

		assert!(matches!(
			run.submit_envelope("a", ok_envelope("a done")).expect("late submission"),
			Outcome::Advanced { phase } if phase == "a"
		));
		// The evidence persists like any acceptance, even though the run is lost.
		assert!(trace_dir.join(ENVELOPE_DIR).join("a.1.json").exists());
		assert_eq!(run.next_step().expect("settled"), Step::Done { accepted: false });
		let summary = run.finish(true, "driver claims success").expect("finish");
		assert!(!summary.accepted, "a halted run cannot be accepted");
		assert!(summary.records.iter().any(|record| record.name == "a"
			&& record.status == PhaseStatus::Passed
			&& !record.invalidated));
	}

	#[test]
	fn replay_restores_two_in_flight_dispatches_as_pending_exactly_once() {
		let dir = TempDir::new("run-dag-replay-two");
		let trace_dir = dir.path().join("trace");
		{
			let mut run = Run::new("adw", dir.path(), fork_workflow())
				.with_tracer(Tracer::create(&trace_dir).expect("tracer"));
			dispatch_serial(&mut run, "a");
			assert!(matches!(
				run.submit_agent_output("a", "not an envelope")
					.expect("reject"),
				Outcome::Retry { attempt: 2, .. }
			));
			dispatch_serial(&mut run, "a");
			dispatch_serial(&mut run, "b");
			// Dies with both flights active.
		}
		// A second crash between the reset and any dispatch: the persisted
		// resets replay, so resuming again must not stack another one.
		drop(Run::resume("adw", dir.path(), fork_workflow(), &trace_dir).expect("resume"));

		let mut resumed =
			Run::resume("adw", dir.path(), fork_workflow(), &trace_dir).expect("re-resume");
		let Step::Run { phase, attempt, correction, .. } = resumed.next_step().expect("first") else {
			panic!("expected a")
		};
		assert_eq!(
			(phase.name.as_str(), attempt),
			("a", 2),
			"the interrupted dispatch keeps its spent budget, not a fresh one"
		);
		assert!(
			correction
				.as_deref()
				.is_some_and(|c| c.contains("no JSON object")),
			"the pending correction survives the reset: {correction:?}"
		);
		let Step::Run { phase, attempt, .. } = resumed.next_step().expect("second") else {
			panic!("expected b")
		};
		assert_eq!((phase.name.as_str(), attempt), ("b", 1));
		assert_eq!(resumed.next_step().expect("gap"), Step::Wait);

		let mut reader = TraceReader::open(&trace_dir).expect("open");
		let events = reader.read_from(0).expect("read");
		let resets: Vec<_> = events
			.iter()
			.filter(|event| event.kind == EventKind::PhaseInvalidated)
			.map(|event| (reader.text(event.phase).to_owned(), event.attempt, event.owner))
			.collect();
		assert_eq!(
			resets,
			vec![
				("a".to_owned(), 1, crate::trace::NO_STRING),
				("b".to_owned(), 0, crate::trace::NO_STRING),
			],
			"each flight is reset exactly once, owner-empty, retaining its spent attempts"
		);
	}

	#[test]
	fn replay_keeps_crash_reset_evidence_but_clears_superseded_evidence() {
		let dir = TempDir::new("run-dag-invalidation-kinds");
		let trace_dir = dir.path().join("trace");
		let build = || {
			Workflow::new("kinds", vec![
				PhaseParams::new("build", PhaseKind::Agent, "builder"),
				PhaseParams::new("review", PhaseKind::Agent, "reviewer")
					.on_reject("build", 1)
					.consuming(["build"]),
			])
		};
		{
			let mut run = Run::new("adw", dir.path(), build())
				.with_tracer(Tracer::create(&trace_dir).expect("tracer"));
			dispatch_serial(&mut run, "build");
			run.submit_envelope("build", ok_envelope("built"))
				.expect("build");
			run.next_step().expect("review dispatched");
			// Dies mid-review: an owner-empty crash reset, not a supersession.
		}
		let mut resumed = Run::resume("adw", dir.path(), build(), &trace_dir).expect("resume");
		let Step::Run { phase, inputs, .. } = resumed.next_step().expect("review again") else {
			panic!("review step")
		};
		assert_eq!(phase.name, "review");
		assert_eq!(
			(inputs[0].phase.as_str(), inputs[0].version),
			("build", 1),
			"a crash reset must not cost the producer's accepted evidence"
		);
		assert!(resumed.records().iter().all(|record| !record.invalidated));

		resumed
			.note_review_decision("review", false, "missing authorization")
			.expect("decision");
		assert!(matches!(
			resumed.submit_envelope("review", rejected_review()).expect("review"),
			Outcome::Retry { phase, invalidated, .. }
				if phase == "build" && invalidated == ["build", "review"]
		));
		drop(resumed);
		// Dies again, now after a supersession.

		let mut resumed = Run::resume("adw", dir.path(), build(), &trace_dir).expect("re-resume");
		let review = resumed.workflow.phases[1].clone();
		let Err(RunError::Mismatch(reason)) = resumed.select_inputs(&review) else {
			panic!("superseded evidence must not be served after replay");
		};
		assert!(reason.contains("invalidated"), "{reason}");
		let Step::Run { phase, attempt, correction, .. } = resumed.next_step().expect("builder")
		else {
			panic!("build step")
		};
		assert_eq!((phase.name.as_str(), attempt), ("build", 2));
		assert!(
			correction
				.as_deref()
				.is_some_and(|c| c.contains("missing authorization")),
			"the revision correction survives the second crash: {correction:?}"
		);
		assert!(resumed.records().iter().all(|record| record.invalidated));
	}
}
