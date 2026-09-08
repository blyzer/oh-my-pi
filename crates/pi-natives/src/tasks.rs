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
//!   if (step.kind === TaskStepKind.Wait) { await drainInFlight(); continue; }
//!   const turn = await agent(step.phase.owner, prompt, step.correction);
//!   run.submitAgentOutput(step.phase.name, turn);
//! }
//! ```
//!
//! Nothing here serializes state to JSON: objects cross as N-API structs, and
//! the trace crosses as raw [`Buffer`] records that JS indexes with the offsets
//! from [`task_trace_layout`]. The only strings on the wire are the ones a
//! model must read — its own turn, and the correction fed back to it.

use std::{
	collections::HashSet,
	sync::{Arc, Mutex},
};

use globset::{Glob, GlobSet, GlobSetBuilder};
use napi::{Result, bindgen_prelude::*};
use napi_derive::napi;
use pi_tasks::{
	ArtifactsExist, Check, Envelope, FilesNonEmpty, Gate, GateCtx, GateReport, JsonParses, Outcome,
	PhaseKind, PhaseParams, PhaseStatus, Run, Step, TraceReader, Tracer, Workflow, trace,
};
use pi_vcs::types::{DiffOptions, StatusOptions, UntrackedMode};
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
	/// everything earlier ACCEPTED attempts claimed. Shared across phases, so
	/// phase 2 is never blamed for the files phase 1 legitimately wrote — but a
	/// rejected attempt's claims never enter here; `accept` is the only writer.
	allowed: Arc<Mutex<HashSet<String>>>,
	/// Paths a build legitimately rewrites without any phase claiming them.
	/// Compiled once at run construction, so a bad pattern fails before a
	/// token is spent rather than when the gate first runs.
	ignore:  GlobSet,
	/// Commit the working copy was on when the run started.
	///
	/// Without it the gate only sees the working tree, and a phase that commits
	/// its own changes hides them: `git status` goes clean and the gate reports
	/// that every change was declared. Measured — a phase running
	/// `git add -A && git commit` passed a gate that rejected the identical
	/// command without the commit.
	base:    Option<String>,
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
		let options = StatusOptions {
			untracked:      UntrackedMode::All,
			pathspecs:      Vec::new(),
			nul_terminated: true,
		};
		let porcelain = match repo.status_porcelain(&options) {
			Ok(text) => text,
			Err(err) => {
				report.push(".", false, format!("status failed: {err}"));
				return report;
			},
		};

		// Untracked files never appear in a diff, and tracked changes stop
		// appearing in `status` the moment a phase commits them. Neither source
		// alone sees every change, so the gate reads both.
		let mut changed: Vec<String> = changed_paths(&porcelain);
		if let Some(base) = &self.base {
			let diff = DiffOptions { base: Some(base.clone()), ..DiffOptions::default() };
			match repo.changed_files(&diff) {
				Ok(paths) => changed.extend(paths.iter().map(|path| normalize(path))),
				Err(err) => {
					report.push(".", false, format!("diff against {base} failed: {err}"));
					return report;
				},
			}
		}
		changed.sort_unstable();
		changed.dedup();

		let Ok(allowed) = self.allowed.lock() else {
			report.push(".", false, "gate state poisoned");
			return report;
		};
		// This attempt's claims cover its own changes for THIS evaluation only.
		// They are committed to the shared set by `accept`, once the engine
		// accepts the attempt — a rejected attempt's declarations must never
		// authorize a later evaluation.
		let claimed: HashSet<String> = envelope.artifacts.iter().map(|a| normalize(a)).collect();

		let mut undeclared = 0usize;
		for path in changed {
			if allowed.contains(&path) || claimed.contains(&path) || self.ignore.is_match(&path) {
				continue;
			}
			undeclared += 1;
			report.push(path, false, "changed but not declared in artifacts");
		}
		if undeclared == 0 {
			let covered = allowed.len()
				+ claimed
					.iter()
					.filter(|path| !allowed.contains(*path))
					.count();
			// Names what it examined, so a clean gate is evidence and not silence.
			report.push(".", true, format!("{covered} declared path(s) cover every change"));
		}
		report
	}

	/// Commits this attempt's declarations into the run-wide allowed set. The
	/// engine calls this only when the attempt is accepted, so claims from a
	/// rejected attempt never survive into later evaluations.
	fn accept(&self, envelope: &Envelope<Value>, _ctx: &GateCtx<'_>) {
		if let Ok(mut allowed) = self.allowed.lock() {
			allowed.extend(envelope.artifacts.iter().map(|a| normalize(a)));
		}
	}
}

/// Repo-relative paths from `git status --porcelain -z`.
///
/// Each record is `XY <path>`; a rename adds a second NUL-separated record
/// holding the source, which is a real change to a real path and is returned
/// too — a file moved out from under a claim is exactly what this gate is for.
pub(crate) fn changed_paths(porcelain: &str) -> Vec<String> {
	let mut paths = Vec::new();
	for record in porcelain.split('\0') {
		if record.is_empty() {
			continue;
		}
		// A rename's source record carries no status prefix.
		let path = if record.len() > 3 && record.as_bytes()[2] == b' ' {
			&record[3..]
		} else {
			record
		};
		if !path.is_empty() {
			paths.push(normalize(path));
		}
	}
	paths
}

/// `./docs/x.md` and `docs/x.md` are the same claim.
pub(crate) fn normalize(path: &str) -> String {
	path
		.trim_start_matches("./")
		.trim_end_matches('/')
		.to_owned()
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

/// Routing for a caller-validated coherent negative review.
#[napi(object)]
pub struct TaskReviewRoute {
	pub to:            String,
	pub max_revisions: f64,
}

/// One phase of a workflow. `owner` names an agent from the roster or a
/// subsystem (`git`, `bun`) — never a model id.
#[napi(object)]
pub struct TaskPhaseSpec {
	pub name:        String,
	pub kind:        TaskPhaseKind,
	pub owner:       String,
	pub description: Option<String>,
	/// Phase this one's failure returns to, instead of retrying in place. The
	/// caller resolves it: the engine obeys a name and holds no policy about
	/// which phase can fix a failure.
	pub rewind_to:   Option<String>,
	pub on_reject:   Option<TaskReviewRoute>,
	/// Phases that must pass before this one runs. The engine sorts on these at
	/// construction, so a resumed run derives the same order it ran.
	pub depends_on:  Option<Vec<String>>,
	/// Accepted outputs this phase consumes, named by producer phase. Resolved
	/// to their current accepted versions at dispatch and returned on the step
	/// as `inputs`; a phase without them keeps the positional handoff.
	pub inputs:      Option<Vec<String>>,
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
	pub adw_id:            String,
	/// Repository root that gates resolve claimed paths against.
	pub root:              String,
	pub workflow:          String,
	pub phases:            Vec<TaskPhaseSpec>,
	/// Directory for `events.bin` / `strings.bin`. Omit to run untraced.
	pub trace_dir:         Option<String>,
	/// Attempts per phase before the run halts. Default 3, minimum 1.
	pub max_attempts:      Option<u32>,
	pub gates:             Option<Vec<TaskPhaseGates>>,
	/// Globs `diff_matches_claims` treats as always accounted for.
	///
	/// Empty by default and deliberately so: a wide default makes the gate
	/// noisy, and an operator who cannot tell which changes it will forgive
	/// stops trusting it. Declare the paths a build legitimately rewrites
	/// (`bun.lock`, `*.generated.ts`) and nothing more.
	pub undeclared_ignore: Option<Vec<String>>,
	/// Where the run-wide `diff_matches_claims` allowed set is persisted.
	///
	/// Format: a JSON array of normalized repo-relative paths, sorted, written
	/// atomically (`<file>.tmp` + rename). A fresh run writes the initial dirt
	/// capture there and every accepted claim rewrites it; a resume that finds
	/// the file loads it as THE set instead of recapturing dirt, so a crashed
	/// attempt's undeclared edits are not silently admitted as operator dirt.
	/// A resume that does not find it falls back to recapture (legacy traces).
	pub claims_file:       Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[napi]
pub enum TaskStepKind {
	Run  = 0,
	Done = 1,
	Wait = 2,
}

/// One resolved input on a dispatched step: which producer phase, which
/// acceptance ordinal (1-based version), and that version's envelope. The
/// trace's `input_selected` records carry the same version, so the evidence an
/// attempt saw is auditable after the fact.
#[napi(object)]
pub struct TaskPhaseInput {
	pub phase:                String,
	pub version:              u32,
	pub summary:              String,
	pub artifacts:            Vec<String>,
	pub notes_for_next_agent: String,
	/// Phase-specific fields beyond the contract, as JSON text.
	pub payload_json:         String,
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
	/// Declared inputs resolved at dispatch, in declaration order. Present only
	/// for a `Run` step whose phase declares `inputs`; a driver building that
	/// phase's prompt uses these, never the positional handoff.
	pub inputs:     Option<Vec<TaskPhaseInput>>,
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
	pub kind:        TaskOutcomeKind,
	pub phase:       String,
	/// The attempt to run next when `kind` is `Retry`.
	pub attempt:     u32,
	pub correction:  Option<String>,
	/// Why the run halted when `kind` is `Aborted`.
	pub reason:      Option<String>,
	/// Target and transitive dependents superseded by a rewind or revision.
	pub invalidated: Vec<String>,
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
	pub name:        String,
	pub kind:        TaskPhaseKind,
	pub owner:       String,
	pub passed:      bool,
	/// Historical evidence superseded by a code or review rewind.
	pub invalidated: bool,
	pub attempts:    u32,
	pub summary:     String,
	/// Why the phase was rejected — gate violations plus non-gate ones such as
	/// a self-reported `fail`. Empty when it passed.
	pub violations:  Vec<String>,
}

/// One finding from a gate the caller ran itself.
#[napi(object)]
pub struct TaskGateCheck {
	/// What was examined — a path, a field name, a symbol. Named, because a
	/// violation that does not say which thing failed is not actionable.
	pub item: String,
	pub ok:   bool,
	pub note: String,
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
/// The envelope text inside an agent turn: the last complete top-level JSON
/// object, or `null` when there is none.
///
/// Exposed so a caller validating the payload before submission uses the
/// engine's own extraction rule instead of reimplementing it and drifting.
#[napi]
pub fn task_envelope_text(turn: String) -> Option<String> {
	pi_tasks::envelope_text(&turn).map(str::to_owned)
}

#[napi]
pub fn task_gate_names() -> Vec<String> {
	vec![
		"artifacts_exist".to_owned(),
		"files_non_empty".to_owned(),
		"json_parses".to_owned(),
		"diff_matches_claims".to_owned(),
	]
}

/// `allowed` is the run-wide set every `diff_matches_claims` instance shares:
/// one gate per phase, but a change declared by phase 1 must not be undeclared
/// for phase 2.
fn build_gate(
	name: &str,
	allowed: &Arc<Mutex<HashSet<String>>>,
	ignore: &GlobSet,
	base: Option<&str>,
) -> Result<Box<dyn Gate>> {
	match name {
		"artifacts_exist" => Ok(Box::new(ArtifactsExist)),
		"files_non_empty" => Ok(Box::new(FilesNonEmpty)),
		"json_parses" => Ok(Box::new(JsonParses)),
		"diff_matches_claims" => Ok(Box::new(DiffMatchesClaims {
			allowed: Arc::clone(allowed),
			ignore:  ignore.clone(),
			base:    base.map(str::to_owned),
		})),
		other => Err(napi::Error::from_reason(format!(
			"unknown gate {other:?}: expected one of {}",
			task_gate_names().join(", ")
		))),
	}
}

/// Compile the ignore globs, naming the pattern that is wrong.
///
/// Run construction is the right place to fail: a bad pattern discovered when
/// the gate first runs would have already spent a phase's tokens, and an
/// operator who wrote `**.lock` instead of `**/*.lock` must learn it now
/// rather than watch a gate quietly forgive nothing.
fn compile_ignore(patterns: Option<Vec<String>>) -> Result<GlobSet> {
	let mut builder = GlobSetBuilder::new();
	for pattern in patterns.unwrap_or_default() {
		let glob = Glob::new(&pattern)
			.map_err(|err| napi::Error::from_reason(format!("undeclaredIgnore {pattern:?}: {err}")))?;
		builder.add(glob);
	}
	builder.build().map_err(fail)
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
	let options = StatusOptions {
		untracked:      UntrackedMode::All,
		pathspecs:      Vec::new(),
		nul_terminated: true,
	};
	let dirty = pi_vcs::detect(std::path::Path::new(root))
		.ok()
		.flatten()
		.and_then(|repo| repo.status_porcelain(&options).ok())
		.map(|text| changed_paths(&text).into_iter().collect::<HashSet<_>>())
		.unwrap_or_default();
	Arc::new(Mutex::new(dirty))
}

/// Persist the run-wide allowed set to `path`: a sorted JSON array of
/// normalized repo-relative paths (see [`TaskRunOptions::claims_file`]).
///
/// Atomic — the full serialized set goes to `<path>.tmp`, then renames over
/// the target — so a crash mid-write can never leave a truncated file to be
/// loaded later as a smaller authorization set.
fn persist_claims(path: &str, allowed: &Arc<Mutex<HashSet<String>>>) -> Result<()> {
	let mut paths: Vec<String> = allowed
		.lock()
		.unwrap_or_else(std::sync::PoisonError::into_inner)
		.iter()
		.cloned()
		.collect();
	paths.sort_unstable();
	let json = serde_json::to_vec_pretty(&paths).map_err(fail)?;
	let tmp = format!("{path}.tmp");
	std::fs::write(&tmp, json)
		.and_then(|()| std::fs::rename(&tmp, path))
		.map_err(|err| napi::Error::from_reason(format!("claimsFile {path}: {err}")))
}

/// Load a persisted claims set. The file IS the set — nothing is merged in,
/// because the point of loading is to NOT admit whatever a crashed attempt
/// left in the working tree as initial dirt. An unreadable or malformed file
/// is an error naming it, never a silent fall-through to recapture.
fn load_claims(path: &str) -> Result<Arc<Mutex<HashSet<String>>>> {
	let bytes = std::fs::read(path)
		.map_err(|err| napi::Error::from_reason(format!("claimsFile {path}: {err}")))?;
	let paths: Vec<String> = serde_json::from_slice(&bytes)
		.map_err(|err| napi::Error::from_reason(format!("claimsFile {path}: {err}")))?;
	Ok(Arc::new(Mutex::new(paths.iter().map(|p| normalize(p)).collect())))
}

/// The commit the working copy is on when the run starts.
///
/// This is the fixed point every later comparison is made against, which is
/// what stops a phase from hiding its work by committing it. `None` when the
/// tree is not a repository or has no commits yet — the gate then sees only
/// the working tree and says so through the same report it always writes.
fn initial_base(root: &str) -> Option<String> {
	pi_vcs::detect(std::path::Path::new(root))
		.ok()
		.flatten()
		.and_then(|repo| repo.head_id().ok().flatten())
}

fn to_napi_step(step: Step) -> TaskStep {
	match step {
		Step::Run { phase, attempt, correction, inputs } => TaskStep {
			kind: TaskStepKind::Run,
			phase: Some(TaskPhaseSpec {
				name:        phase.name,
				kind:        to_napi_kind(phase.kind),
				owner:       phase.owner,
				description: Some(phase.description).filter(|d| !d.is_empty()),
				rewind_to:   phase.rewind_to,
				on_reject:   phase.on_reject.map(|route| TaskReviewRoute {
					to:            route.to,
					max_revisions: f64::from(route.max_revisions),
				}),
				depends_on:  phase.depends_on,
				inputs:      Some(phase.inputs).filter(|i| !i.is_empty()),
			}),
			attempt,
			correction,
			inputs: Some(inputs).filter(|i| !i.is_empty()).map(|inputs| {
				inputs
					.into_iter()
					.map(|input| TaskPhaseInput {
						phase:                input.phase,
						version:              input.version,
						summary:              input.envelope.summary,
						artifacts:            input.envelope.artifacts,
						notes_for_next_agent: input.envelope.notes_for_next_agent,
						payload_json:         input.envelope.payload.to_string(),
					})
					.collect()
			}),
			accepted: false,
		},
		Step::Wait => TaskStep {
			kind:       TaskStepKind::Wait,
			phase:      None,
			attempt:    0,
			correction: None,
			inputs:     None,
			accepted:   false,
		},
		Step::Done { accepted } => TaskStep {
			kind: TaskStepKind::Done,
			phase: None,
			attempt: 0,
			correction: None,
			inputs: None,
			accepted,
		},
	}
}

fn to_napi_outcome(outcome: Outcome) -> TaskOutcome {
	match outcome {
		Outcome::Advanced { phase } => TaskOutcome {
			kind: TaskOutcomeKind::Advanced,
			phase,
			attempt: 0,
			correction: None,
			reason: None,
			invalidated: Vec::new(),
		},
		Outcome::Retry { phase, attempt, correction, invalidated } => TaskOutcome {
			kind: TaskOutcomeKind::Retry,
			phase,
			attempt,
			correction: Some(correction),
			reason: None,
			invalidated,
		},
		Outcome::Aborted { phase, reason } => TaskOutcome {
			kind: TaskOutcomeKind::Aborted,
			phase,
			attempt: 0,
			correction: None,
			reason: Some(reason),
			invalidated: Vec::new(),
		},
	}
}

/// A driven workflow run. Every method is synchronous and cheap: the engine
/// decides, it never waits on a model.
#[napi]
pub struct TaskRun {
	inner:       Run,
	trace_dir:   Option<String>,
	/// The run-wide accepted-claims set every `diff_matches_claims` instance
	/// shares. Held here so ANY accepted phase commits its declarations —
	/// `Gate::accept` only fires for phases that carry the gate, and a plan
	/// phase without it would otherwise leave its own PLAN.md to be blamed on
	/// whichever later phase does carry it. With a claims file the set also
	/// survives a crash; without one, resume re-admits earlier work through
	/// the dirt recapture instead.
	allowed:     Arc<Mutex<HashSet<String>>>,
	/// Where `allowed` is persisted after construction and every acceptance.
	claims_file: Option<String>,
}

fn to_core_phases(specs: Vec<TaskPhaseSpec>) -> Result<Vec<PhaseParams>> {
	specs
		.into_iter()
		.map(|spec| {
			let params = PhaseParams::new(spec.name, to_core_kind(spec.kind), spec.owner);
			let params = match spec.rewind_to {
				Some(target) => params.rewinding_to(target),
				None => params,
			};
			let params = match spec.on_reject {
				Some(route) => {
					if !(1.0..=65_535.0).contains(&route.max_revisions)
						|| route.max_revisions.fract() != 0.0
					{
						return Err(fail(
							"onReject.maxRevisions must be an integer from 1 through 65535",
						));
					}
					let max_revisions = route.max_revisions as u16;
					if spec.depends_on.as_ref().is_some_and(Vec::is_empty) {
						return Err(fail(
							"onReject target must be a transitive dependency when dependsOn is declared",
						));
					}
					params.on_reject(route.to, max_revisions)
				},
				None => params,
			};
			let params = match spec.depends_on {
				Some(names) => params.after(names),
				_ => params,
			};
			let params = match spec.inputs {
				Some(names) if !names.is_empty() => params.consuming(names),
				_ => params,
			};
			Ok(match spec.description {
				Some(text) => params.describe(text),
				None => params,
			})
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
		let phases = to_core_phases(options.phases)?;
		let workflow = Workflow::new(options.workflow.clone(), phases);
		// `Run::resume` opens the trace itself so it can mark the continuation.
		let mut run =
			Run::resume(options.adw_id.clone(), &options.root, workflow, &trace_dir).map_err(fail)?;
		// The traced base is the budget this run actually ran with — RunStarted
		// recorded it, and every spent attempt was judged against it. A
		// different number here would silently substitute the budget mid-run,
		// so it is refused instead, naming both.
		if let Some(attempts) = options.max_attempts {
			let traced = run.max_attempts();
			if attempts != traced {
				return Err(napi::Error::from_reason(format!(
					"maxAttempts {attempts} does not match this trace's recorded budget {traced}; \
					 resume with the original configuration"
				)));
			}
		}
		let allowed = match &options.claims_file {
			// The file IS the set: loading, never recapturing, is what keeps a
			// crashed attempt's undeclared edits out of the initial dirt.
			Some(path) if std::path::Path::new(path).exists() => load_claims(path)?,
			// No persisted set (legacy trace, or first traced run before this
			// option existed): recapture dirt, re-admitting the work earlier
			// phases already landed.
			_ => initial_allowed(&options.root),
		};
		let ignore = compile_ignore(options.undeclared_ignore)?;
		// Captured once: the fixed point later diffs are made against.
		let base = initial_base(&options.root);
		for entry in options.gates.unwrap_or_default() {
			let gates = entry
				.gates
				.iter()
				.map(|name| build_gate(name, &allowed, &ignore, base.as_deref()))
				.collect::<Result<Vec<_>>>()?;
			run.register_gates(&entry.phase, gates);
		}
		Ok(Self { inner: run, trace_dir: Some(trace_dir), allowed, claims_file: options.claims_file })
	}

	#[napi(constructor)]
	pub fn new(options: TaskRunOptions) -> Result<Self> {
		let phases = to_core_phases(options.phases)?;

		let mut run =
			Run::new(options.adw_id, &options.root, Workflow::new(options.workflow, phases));
		if let Some(dir) = &options.trace_dir {
			run = run.with_tracer(Tracer::create(dir).map_err(fail)?);
		}
		if let Some(attempts) = options.max_attempts {
			run = run.with_max_attempts(attempts);
		}
		let allowed = initial_allowed(&options.root);
		let ignore = compile_ignore(options.undeclared_ignore)?;
		// Captured once: the fixed point later diffs are made against.
		let base = initial_base(&options.root);
		for entry in options.gates.unwrap_or_default() {
			let gates = entry
				.gates
				.iter()
				.map(|name| build_gate(name, &allowed, &ignore, base.as_deref()))
				.collect::<Result<Vec<_>>>()?;
			run.register_gates(&entry.phase, gates);
		}
		// The initial dirt is authorization too: persist it now, so a resume
		// that loads the file sees exactly what this run admitted at start.
		if let Some(path) = &options.claims_file {
			persist_claims(path, &allowed)?;
		}
		Ok(Self {
			inner: run,
			trace_dir: options.trace_dir,
			allowed,
			claims_file: options.claims_file,
		})
	}

	/// Commit an accepted envelope's declarations to the shared claims set,
	/// then persist the set when the run carries a claims file — authorization
	/// living only in memory would vanish in exactly the crash resume exists
	/// for. Only acceptance authorizes: rejected attempts never reach this.
	fn commit_accepted_claims(&self, outcome: &Outcome) -> Result<()> {
		if !matches!(outcome, Outcome::Advanced { .. }) {
			return Ok(());
		}
		let Some(envelope) = self.inner.previous_envelope() else {
			return Ok(());
		};
		if let Ok(mut allowed) = self.allowed.lock() {
			allowed.extend(envelope.artifacts.iter().map(|a| normalize(a)));
		}
		match &self.claims_file {
			Some(path) => persist_claims(path, &self.allowed),
			None => Ok(()),
		}
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
	pub fn submit_agent_output(&mut self, phase: String, text: String) -> Result<TaskOutcome> {
		let outcome = self
			.inner
			.submit_agent_output(&phase, &text)
			.map_err(fail)?;
		self.commit_accepted_claims(&outcome)?;
		Ok(to_napi_outcome(outcome))
	}

	/// Report a deterministic `Code` or human `Engineer` phase. A red test
	/// suite reaches the next agent as an envelope like any other.
	#[napi]
	pub fn submit_code_result(
		&mut self,
		phase: String,
		ok: bool,
		summary: String,
	) -> Result<TaskOutcome> {
		let outcome = self
			.inner
			.submit_envelope(&phase, Envelope::code(ok, summary))
			.map_err(fail)?;
		self.commit_accepted_claims(&outcome)?;
		Ok(to_napi_outcome(outcome))
	}

	/// Records one fusion-panel member's answer against the active phase.
	/// `tokens` is what makes two models comparable in the trace.
	#[napi]
	pub fn note_panel_opinion(
		&self,
		phase: String,
		owner: String,
		ok: bool,
		tokens: u32,
		model: Option<String>,
	) -> Result<()> {
		self
			.inner
			.note_panel_opinion(&phase, &owner, ok, tokens, model.as_deref())
			.map_err(fail)
	}

	/// Records what the attempt in flight cost. Charged per attempt: a rejected
	/// try spent real tokens, and a phase that needed three of them is the one
	/// a cost report has to show.
	#[napi]
	pub fn note_phase_tokens(
		&self,
		phase: String,
		owner: String,
		tokens: u32,
		model: Option<String>,
	) -> Result<()> {
		self
			.inner
			.note_phase_tokens(&phase, &owner, tokens, model.as_deref())
			.map_err(fail)
	}

	/// Record a gate the caller ran itself, judged with the engine's own on the
	/// next submission.
	///
	/// For checks the engine cannot perform — schema validation needs the
	/// TypeScript type system, and a JSON Schema validator in `pi-tasks` would
	/// cost that crate its three dependencies. The result is a `gate_check` in
	/// the trace and blocks acceptance exactly like a native gate.
	#[napi]
	pub fn note_gate_report(
		&mut self,
		phase: String,
		gate: String,
		checks: Vec<TaskGateCheck>,
	) -> Result<()> {
		self
			.inner
			.note_gate_report(
				&phase,
				gate,
				checks
					.into_iter()
					.map(|c| Check { item: c.item, ok: c.ok, note: c.note })
					.collect(),
			)
			.map_err(fail)
	}

	/// A coherent verdict for the next submission. Gates and envelope status
	/// still run first; this decision is drained even if the attempt fails.
	#[napi]
	pub fn note_review_decision(
		&mut self,
		phase: String,
		approved: bool,
		reason: String,
	) -> Result<()> {
		self
			.inner
			.note_review_decision(&phase, approved, reason)
			.map_err(fail)
	}

	/// Evaluate an active phase's gates in its ephemeral writer workspace.
	#[napi]
	pub fn set_phase_root(&mut self, phase: String, root: String) -> Result<()> {
		self.inner.set_phase_root(&phase, root).map_err(fail)
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
		let summary = self
			.inner
			.finish(accepted, reason.unwrap_or_default())
			.map_err(fail)?;
		Ok(TaskRunSummary {
			adw_id:   summary.adw_id,
			workflow: summary.workflow,
			accepted: summary.accepted,
			reason:   summary.reason,
			phases:   summary
				.records
				.into_iter()
				.map(|record| TaskPhaseResult {
					name:        record.name,
					kind:        to_napi_kind(record.kind),
					owner:       record.owner,
					passed:      record.status == PhaseStatus::Passed,
					invalidated: record.invalidated,
					attempts:    record.attempts,
					summary:     record.summary,
					violations:  record.violations,
				})
				.collect(),
		})
	}

	/// Base attempt budget after clamping. Review revisions grant individual
	/// targets additional attempts; the engine owns transition bounds.
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

/// Byte layout of one trace record. A JS reader indexes
/// [`TaskTraceReader::read_raw`] with these instead of hardcoding them, so a
/// format bump cannot go unnoticed.
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
			"phase_rewound",
			"review_revision",
			"review_exhausted",
			"input_selected",
			"correction_pending",
			"phase_invalidated",
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
		self
			.inner
			.read_raw(u64::from(from_seq))
			.map(Buffer::from)
			.map_err(fail)
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
	fn review_revision_limits_reject_fractional_and_out_of_range_numbers() {
		for max_revisions in [0.0, 65_536.0, 1.5, f64::NAN] {
			let phase = TaskPhaseSpec {
				name:        "review".into(),
				kind:        TaskPhaseKind::Agent,
				owner:       "reviewer".into(),
				description: None,
				rewind_to:   None,
				on_reject:   Some(TaskReviewRoute { to: "build".into(), max_revisions }),
				depends_on:  None,
				inputs:      None,
			};
			assert!(to_core_phases(vec![phase]).is_err(), "must reject {max_revisions}");
		}
	}

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
		let gate = DiffMatchesClaims {
			allowed: Arc::new(Mutex::new(HashSet::new())),
			ignore:  compile_ignore(None).expect("compiles"),
			base:    None,
		};
		let envelope = Envelope::code(true, "done");
		let report = gate.run(&envelope, &GateCtx { root: &dir });
		let _ = std::fs::remove_dir_all(&dir);

		assert!(!report.ok(), "a gate that cannot read a diff must not report passed");
		assert!(
			report.violations().any(|v| v.contains("not a repository")),
			"the violation has to name why it could not check, not just fail"
		);
	}

	#[test]
	fn an_ignored_glob_is_forgiven_but_a_sibling_is_not() {
		let ignore = compile_ignore(Some(vec!["**/*.lock".to_owned(), "bun.lock".to_owned()]))
			.expect("compiles");
		assert!(ignore.is_match("bun.lock"));
		assert!(ignore.is_match("packages/x/pnpm.lock"));
		// Narrow on purpose: a pattern that forgave src/index.ts would make the
		// gate worthless, and an operator cannot audit what it silently allows.
		assert!(!ignore.is_match("src/index.ts"));
		assert!(!ignore.is_match("lock"));
	}

	#[test]
	fn no_patterns_forgives_nothing() {
		let ignore = compile_ignore(None).expect("compiles");
		assert!(!ignore.is_match("bun.lock"), "an empty default must not quietly allow anything");
	}

	#[test]
	fn a_malformed_glob_fails_at_construction_naming_the_pattern() {
		let err = compile_ignore(Some(vec!["src/**/[".to_owned()])).expect_err("must refuse");
		assert!(
			err.reason.contains("src/**/["),
			"the operator has to be told which pattern is wrong: {err}"
		);
	}

	#[test]
	fn rejected_attempt_claims_never_authorize_later_runs_but_accepted_claims_do() {
		// Regression: `run` used to insert an envelope's declarations into the
		// shared allowed set the moment the gate RAN, so a rejected attempt's
		// claims kept authorizing every later evaluation.
		let dir = std::env::temp_dir().join(format!(
			"pi-natives-claims-{}-{:?}",
			std::process::id(),
			std::thread::current().id()
		));
		std::fs::create_dir_all(&dir).expect("temp dir");
		let ok = std::process::Command::new("git")
			.args(["init", "-q"])
			.current_dir(&dir)
			.status()
			.expect("git runs")
			.success();
		assert!(ok, "git init");
		std::fs::write(dir.join("y.ts"), "y").expect("write y");
		std::fs::write(dir.join("z.ts"), "z").expect("write z");

		let gate = DiffMatchesClaims {
			allowed: Arc::new(Mutex::new(HashSet::new())),
			ignore:  compile_ignore(None).expect("compiles"),
			base:    None,
		};
		let ctx = GateCtx { root: &dir };
		let claims = |paths: &[&str]| Envelope {
			artifacts: paths.iter().map(|p| (*p).to_owned()).collect(),
			..Envelope::code(true, "claimed")
		};

		// Rejected attempt: claims y.ts, but z.ts changed too.
		let report = gate.run(&claims(&["y.ts"]), &ctx);
		assert!(!report.ok(), "z.ts is undeclared");

		// The next attempt claims only z.ts. The rejected attempt's y.ts claim
		// must NOT cover y.ts — under the old behavior this passed.
		let report = gate.run(&claims(&["z.ts"]), &ctx);
		assert!(
			report.violations().any(|v| v.contains("y.ts")),
			"a rejected attempt's claim must not authorize a later run"
		);

		// Once an attempt claiming y.ts is ACCEPTED, its claim is committed and
		// the very same z.ts envelope now passes.
		gate.accept(&claims(&["y.ts"]), &ctx);
		let report = gate.run(&claims(&["z.ts"]), &ctx);
		let _ = std::fs::remove_dir_all(&dir);
		assert!(
			report.ok(),
			"accepted claims stay committed: {:?}",
			report.violations().collect::<Vec<_>>()
		);
	}

	#[test]
	fn an_accepted_phase_without_the_gate_still_commits_its_claims() {
		// Regression from a live sdlc run: `plan` carried no diff gate, so its
		// accepted PLAN.md never entered the shared allowed set and `build` —
		// the first phase that DID carry the gate — was blamed for it.
		let dir = std::env::temp_dir().join(format!(
			"pi-natives-gateless-claims-{}-{:?}",
			std::process::id(),
			std::thread::current().id()
		));
		std::fs::create_dir_all(&dir).expect("temp dir");
		let ok = std::process::Command::new("git")
			.args(["init", "-q"])
			.current_dir(&dir)
			.status()
			.expect("git runs")
			.success();
		assert!(ok, "git init");

		let phase = |name: &str| TaskPhaseSpec {
			name:        name.into(),
			kind:        TaskPhaseKind::Agent,
			owner:       "task".into(),
			description: None,
			rewind_to:   None,
			on_reject:   None,
			depends_on:  None,
			inputs:      None,
		};
		let mut run = TaskRun::new(TaskRunOptions {
			adw_id:            "adw-test".into(),
			root:              dir.to_string_lossy().into_owned(),
			workflow:          "gateless".into(),
			phases:            vec![phase("plan"), phase("build")],
			trace_dir:         None,
			max_attempts:      Some(1),
			gates:             Some(vec![TaskPhaseGates {
				phase: "build".into(),
				gates: vec!["diff_matches_claims".into()],
			}]),
			undeclared_ignore: None,
			claims_file:       None,
		})
		.expect("run builds");

		run.next_step().expect("plan step");
		std::fs::write(dir.join("PLAN.md"), "# plan").expect("write plan");
		let planned = serde_json::to_string(&Envelope {
			artifacts: vec!["PLAN.md".into()],
			..Envelope::code(true, "planned")
		})
		.expect("serialize");
		let outcome = run
			.submit_agent_output("plan".into(), planned)
			.expect("submit plan");
		assert_eq!(outcome.kind, TaskOutcomeKind::Advanced, "plan accepts: {:?}", outcome.reason);

		// Build changes only its own file; PLAN.md is already on disk from the
		// accepted plan and must not be blamed on build.
		run.next_step().expect("build step");
		std::fs::write(dir.join("src.ts"), "code").expect("write src");
		let built = serde_json::to_string(&Envelope {
			artifacts: vec!["src.ts".into()],
			..Envelope::code(true, "built")
		})
		.expect("serialize");
		let outcome = run
			.submit_agent_output("build".into(), built)
			.expect("submit build");
		let _ = std::fs::remove_dir_all(&dir);
		assert_eq!(
			outcome.kind,
			TaskOutcomeKind::Advanced,
			"an accepted gate-less phase's claim covers its file: {:?}",
			outcome.correction
		);
	}

	#[test]
	fn resume_refuses_a_substituted_attempt_budget_but_keeps_a_matching_one() {
		// `resume` used to apply `maxAttempts` AFTER the trace restored the
		// recorded base, silently substituting the budget every spent attempt
		// was judged against.
		let dir = std::env::temp_dir().join(format!(
			"pi-natives-budget-{}-{:?}",
			std::process::id(),
			std::thread::current().id()
		));
		let _ = std::fs::remove_dir_all(&dir);
		std::fs::create_dir_all(&dir).expect("temp dir");
		let trace_dir = dir.join("trace");
		let options = |max_attempts: Option<u32>| TaskRunOptions {
			adw_id: "adw-budget".into(),
			root: dir.to_string_lossy().into_owned(),
			workflow: "budget".into(),
			phases: vec![TaskPhaseSpec {
				name:        "plan".into(),
				kind:        TaskPhaseKind::Agent,
				owner:       "planner".into(),
				description: None,
				rewind_to:   None,
				on_reject:   None,
				depends_on:  None,
				inputs:      None,
			}],
			trace_dir: Some(trace_dir.to_string_lossy().into_owned()),
			max_attempts,
			gates: None,
			undeclared_ignore: None,
			claims_file: None,
		};

		{
			let mut run = TaskRun::new(options(Some(4))).expect("run builds");
			run.next_step().expect("step");
			// Dies mid-`plan`.
		}

		let Err(err) = TaskRun::resume(options(Some(2))) else {
			panic!("a substituted budget must refuse")
		};
		assert!(
			err.reason.contains("maxAttempts 2") && err.reason.contains("budget 4"),
			"the operator must see both numbers: {}",
			err.reason
		);
		let resumed = TaskRun::resume(options(Some(4))).expect("the traced budget resumes");
		assert_eq!(resumed.max_attempts(), 4);
		drop(resumed);
		let resumed = TaskRun::resume(options(None)).expect("an omitted override resumes");
		assert_eq!(resumed.max_attempts(), 4, "the traced base survives an omitted override");
		let _ = std::fs::remove_dir_all(&dir);
	}

	#[test]
	fn a_claims_file_round_trips_resume_and_refuses_a_crashed_attempts_edits() {
		// The implicit-authorization leak: without persistence, resume recaptures
		// the working tree as "operator dirt" — including whatever a crashed
		// attempt edited without declaring. Loading the persisted set instead
		// keeps exactly the initial dirt and the ACCEPTED claims admitted.
		let dir = std::env::temp_dir().join(format!(
			"pi-natives-claims-file-{}-{:?}",
			std::process::id(),
			std::thread::current().id()
		));
		let _ = std::fs::remove_dir_all(&dir);
		let repo = dir.join("repo");
		let state = dir.join("state");
		std::fs::create_dir_all(&repo).expect("repo dir");
		std::fs::create_dir_all(&state).expect("state dir");
		let ok = std::process::Command::new("git")
			.args(["init", "-q"])
			.current_dir(&repo)
			.status()
			.expect("git runs")
			.success();
		assert!(ok, "git init");
		// Operator dirt, present before the run starts.
		std::fs::write(repo.join("dirty.ts"), "operator dirt").expect("write dirt");

		let claims_file = state.join("claims.json");
		let phase = |name: &str| TaskPhaseSpec {
			name:        name.into(),
			kind:        TaskPhaseKind::Agent,
			owner:       "task".into(),
			description: None,
			rewind_to:   None,
			on_reject:   None,
			depends_on:  None,
			inputs:      None,
		};
		let options = |claims: &std::path::Path| TaskRunOptions {
			adw_id:            "adw-claims".into(),
			root:              repo.to_string_lossy().into_owned(),
			workflow:          "claims".into(),
			phases:            vec![phase("plan"), phase("build")],
			trace_dir:         Some(state.join("trace").to_string_lossy().into_owned()),
			max_attempts:      None,
			gates:             Some(vec![TaskPhaseGates {
				phase: "build".into(),
				gates: vec!["diff_matches_claims".into()],
			}]),
			undeclared_ignore: None,
			claims_file:       Some(claims.to_string_lossy().into_owned()),
		};
		let envelope = |artifacts: &[&str], summary: &str| {
			serde_json::to_string(&Envelope {
				artifacts: artifacts.iter().map(|p| (*p).to_owned()).collect(),
				..Envelope::code(true, summary)
			})
			.expect("serialize")
		};

		{
			let mut run = TaskRun::new(options(&claims_file)).expect("run builds");
			run.next_step().expect("plan step");
			std::fs::write(repo.join("a.ts"), "a").expect("write a");
			let outcome = run
				.submit_agent_output("plan".into(), envelope(&["a.ts"], "planned"))
				.expect("submit plan");
			assert_eq!(outcome.kind, TaskOutcomeKind::Advanced, "{:?}", outcome.reason);
			// The process dies here, before `build` dispatches.
		}
		let persisted: Vec<String> =
			serde_json::from_slice(&std::fs::read(&claims_file).expect("claims file"))
				.expect("claims parse");
		assert_eq!(persisted, ["a.ts", "dirty.ts"], "initial dirt plus the accepted claim, sorted");

		// The crashed attempt's undeclared edit, made between the runs.
		std::fs::write(repo.join("sneaky.ts"), "undeclared").expect("write sneaky");

		{
			let mut run = TaskRun::resume(options(&claims_file)).expect("resume loads the file");
			run.next_step().expect("build step");
			std::fs::write(repo.join("b.ts"), "b").expect("write b");
			let outcome = run
				.submit_agent_output("build".into(), envelope(&["b.ts"], "built"))
				.expect("submit build");
			assert_eq!(outcome.kind, TaskOutcomeKind::Retry, "sneaky.ts is undeclared");
			let correction = outcome.correction.expect("correction");
			assert!(correction.contains("sneaky.ts"), "the leak is flagged: {correction}");
			assert!(
				!correction.contains("dirty.ts") && !correction.contains("a.ts"),
				"original dirt and the accepted claim stay admitted: {correction}"
			);
		}

		// A missing file is a legacy trace: recapture admits the tree as dirt.
		let mut run = TaskRun::resume(options(&state.join("absent.json")))
			.expect("resume falls back to recapture");
		run.next_step().expect("build retry");
		let outcome = run
			.submit_agent_output("build".into(), envelope(&["b.ts"], "built"))
			.expect("submit build");
		let _ = std::fs::remove_dir_all(&dir);
		assert_eq!(
			outcome.kind,
			TaskOutcomeKind::Advanced,
			"recaptured dirt covers sneaky.ts: {:?}",
			outcome.correction
		);
	}
}
