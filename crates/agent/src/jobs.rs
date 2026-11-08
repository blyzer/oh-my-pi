//! Runtime supervision index rebuilt from the authoritative `<meta><jobs>`
//! tree.
//!
//! The board deliberately stores no durable job state.  Identities, kinds and
//! lifecycle status live in the session DOM; this module only connects those
//! elements to kill boundaries owned by the runtime.

use std::{
	collections::BTreeMap,
	sync::{
		Arc,
		atomic::{AtomicUsize, Ordering},
	},
	time::{SystemTime, UNIX_EPOCH},
};

use flume::Receiver;
use omp_core::{FastHashMap, Str};
use omp_dom::{Dom, Handle, KnownTag, Op, PropId, PropKey, Tag, Txn, Value};
use omp_exec::{
	Artifact, BackendId, Digest, ExecutionBackend, ExecutionError, ExecutionErrorKind,
	ExecutionEvent, ExecutionRequest, ExecutionResult, ExecutionRun, ExecutionState, FallbackCause,
	Logs, Outcome, ReattachHandle, Timing, TransferCounts,
};
use omp_journal::{
	EntryId,
	blob::{BlobRef, BlobStore},
	data::LaunchDaemonCompletion,
};
use omp_proto::toolhost::v1::HookEventId;
use omp_session::{LifecycleWork, Session};
use omp_tool::{CallOutcomeDetails, InvocationFeed, RegistryError, ToolIdentity};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
use strum::{Display, EnumString, IntoStaticStr};
use tokio::{task::JoinHandle, time};
use tokio_util::sync::CancellationToken;

use crate::{
	approvals::{ApprovalBook, ApprovalSource, TicketState},
	dispatch::{Committer, DispatchEvent, DispatchOptions, OutputStream},
};

/// The four execution shapes represented by the one job primitive.
#[derive(Clone, Copy, Debug, Deserialize, Display, EnumString, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
#[strum(serialize_all = "lowercase")]
pub enum JobKind {
	/// A detached ordinary tool call.
	Tool,
	/// A child agent kernel.
	Subagent,
	/// A supervised process or daemon.
	Process,
	/// A remote execution backend job with a durable reattach identity.
	Execution,
}

#[derive(Clone, Copy, Debug, IntoStaticStr)]
#[strum(serialize_all = "lowercase")]
enum DeliveryJobType {
	Bash,
	Task,
	Tool,
	Execution,
}

impl From<JobKind> for DeliveryJobType {
	fn from(kind: JobKind) -> Self {
		match kind {
			JobKind::Tool => Self::Tool,
			JobKind::Subagent => Self::Task,
			JobKind::Process => Self::Bash,
			JobKind::Execution => Self::Execution,
		}
	}
}

/// Journal shape of a settled [`ExecutionResult`]: the value of the
/// `execution_result` property.
///
/// The record is mirrored here so the stored format belongs to this module and
/// does not move when `omp-exec` changes a field. [`resolve_execution_result`]
/// reads it back.
#[derive(Debug, Deserialize, Serialize)]
struct DurableExecutionResult {
	id:             omp_exec::ExecutionId,
	backend:        BackendId,
	outcome:        Outcome,
	logs:           Option<Logs>,
	artifacts:      Arc<[Artifact]>,
	output_digests: BTreeMap<Str, Digest>,
	timing:         Timing,
	transfer:       TransferCounts,
	cache_hit:      bool,
	fallback:       Option<FallbackCause>,
}

impl From<&ExecutionResult> for DurableExecutionResult {
	fn from(result: &ExecutionResult) -> Self {
		Self {
			id:             result.id.clone(),
			backend:        result.backend.clone(),
			outcome:        result.outcome,
			logs:           result.logs,
			artifacts:      result.artifacts.clone(),
			output_digests: result.output_digests.clone(),
			timing:         result.timing,
			transfer:       result.transfer,
			cache_hit:      result.cache_hit,
			fallback:       result.fallback,
		}
	}
}

impl From<DurableExecutionResult> for ExecutionResult {
	fn from(durable: DurableExecutionResult) -> Self {
		Self {
			id:             durable.id,
			backend:        durable.backend,
			outcome:        durable.outcome,
			logs:           durable.logs,
			artifacts:      durable.artifacts,
			output_digests: durable.output_digests,
			timing:         durable.timing,
			transfer:       durable.transfer,
			cache_hit:      durable.cache_hit,
			fallback:       durable.fallback,
		}
	}
}

/// Typed execution state carried through the durable job settlement.
#[derive(Clone, Debug)]
pub struct ExecutionSettlement {
	/// Journal-derived execution state.
	pub state:  ExecutionState,
	/// Accepted backend identity, when one exists.
	pub handle: Option<ReattachHandle>,
	/// Terminal result references, when the backend settled successfully.
	pub result: Option<ExecutionResult>,
	/// Typed terminal failure, when the backend did not settle successfully.
	pub error:  Option<ExecutionError>,
}

/// Terminal result produced by one owned execution unit.
#[derive(Clone, Debug)]
pub struct JobSettlement {
	/// Durable terminal status (`completed`, `cancelled`, or `failed`).
	pub status:     Str,
	/// Bounded typed output, when the job completed with a value.
	pub output:     Option<Box<RawValue>>,
	/// Stable terminal diagnostic, when present.
	pub error:      Option<Str>,
	/// Typed one-shot completion for a supervised process.
	pub completion: Option<LaunchDaemonCompletion>,
	/// Execution-specific accepted handle, result, and typed error.
	pub execution:  Option<ExecutionSettlement>,
}

/// Proof that the host approved one execution for one destination.
///
/// Only [`AuthorizedExecution::from_session`] mints it, from a prompt the
/// session journaled, and [`JobBoard::start_execution`] consumes it. The
/// board refuses a ticket an earlier execution already submitted under, so one
/// approval starts at most one backend run, across restarts too.
#[derive(Debug)]
pub struct AuthorizedExecution {
	ticket:      Str,
	destination: Str,
}

impl AuthorizedExecution {
	/// Mints the proof from the journaled prompt `ticket_id`, answered for the
	/// `kind` requirement on `subject`, for the non-secret `destination` that
	/// subject names.
	///
	/// `ticket_id` is the journaled id, which [`ApprovalBook::decided_for`]
	/// finds from a call id; a route's own ticket id is not the journaled one.
	///
	/// # Errors
	///
	/// A prompt the session never journaled, a pending, withdrawn, denied,
	/// timed-out, or unreachable one (even when its default fails open), and a
	/// prompt with no `kind` requirement on `subject` authorize nothing.
	pub fn from_session(
		session: &Session,
		ticket_id: &str,
		kind: &str,
		subject: &str,
		destination: Str,
	) -> Result<Self, ExecutionApprovalError> {
		let ticket = ApprovalBook::new()
			.ticket(session, ticket_id)
			.ok_or_else(|| ExecutionApprovalError::UnknownTicket { ticket: Str::new(ticket_id) })?;
		let approved = ticket.state == TicketState::Decided
			&& ticket.decision.as_ref().is_some_and(|decision| {
				decision.approved
					&& !matches!(decision.source, ApprovalSource::Timeout | ApprovalSource::Unavailable)
			});
		if !approved {
			return Err(ExecutionApprovalError::NotApproved { ticket: ticket.ticket_id });
		}
		let covered = ticket
			.reasons
			.iter()
			.any(|reason| reason.kind.as_str() == kind && reason.subject.as_str() == subject);
		if !covered {
			return Err(ExecutionApprovalError::NotCovered {
				ticket:  ticket.ticket_id,
				kind:    Str::new(kind),
				subject: Str::new(subject),
			});
		}
		Ok(Self { ticket: ticket.ticket_id, destination })
	}
}

/// Why [`AuthorizedExecution::from_session`] minted nothing.
#[derive(Debug, Eq, PartialEq, thiserror::Error)]
pub enum ExecutionApprovalError {
	/// The session journals no prompt with this ticket id.
	#[error("approval ticket {ticket} is not journaled")]
	UnknownTicket {
		/// The ticket id the caller named.
		ticket: Str,
	},
	/// The prompt is unanswered, withdrawn, denied, or answered by a timeout or
	/// an unreachable route.
	#[error("approval ticket {ticket} did not approve an execution")]
	NotApproved {
		/// The unapproved ticket.
		ticket: Str,
	},
	/// The approved prompt names no requirement of this kind on this subject.
	#[error("approval ticket {ticket} does not cover {kind} access to {subject}")]
	NotCovered {
		/// The approved ticket.
		ticket:  Str,
		/// The requirement kind the execution needs.
		kind:    Str,
		/// The subject the execution reaches.
		subject: Str,
	},
}

/// One approved request for [`JobBoard::start_execution`].
#[derive(Debug)]
pub struct ExecutionSubmission {
	/// Runtime owner journaled on the job.
	pub owner:    Str,
	/// Serialized non-secret request identity journaled on the job.
	pub identity: Str,
	/// The request `start` receives. Its id becomes the job id.
	pub request:  ExecutionRequest,
}

/// Why [`JobBoard::start_execution`] started nothing.
#[derive(Debug, thiserror::Error)]
pub enum ExecutionStartError {
	/// The session already holds a job with this execution id.
	#[error("execution job {id} already exists")]
	Duplicate {
		/// The execution id already in use.
		id: Str,
	},
	/// An earlier execution already reached the backend under this approval.
	#[error("approval ticket {ticket} already started an execution")]
	ApprovalSpent {
		/// The spent approval ticket.
		ticket: Str,
	},
	/// The session has no jobs subtree to journal the request in.
	#[error("session has no jobs subtree")]
	NoJobs,
	/// Journaling the request or its submission marker failed.
	#[error(transparent)]
	Session(#[from] omp_session::SessionError),
}

/// Durable fields projected from one `<job>` or `<subagent>` element.
#[derive(Clone, Debug)]
pub struct JobRecord {
	/// Current DOM handle.
	pub handle:          Handle,
	/// Stable durable identity.
	pub id:              Str,
	/// Shared job kind.
	pub kind:            JobKind,
	/// Journal-derived lifecycle status.
	pub status:          Str,
	/// User-facing execution type (`bash`, `task`, `eval`, or a tool identity).
	pub job_type:        Str,
	/// Nonempty work label captured when the job started.
	pub label:           Str,
	/// Owning session or agent identity, when present.
	pub owner:           Option<Str>,
	/// Factory lease for a `JobKind::Subagent` job. Absent on journals written
	/// before the lease existed. The three facts are one value: a partial write
	/// folds as `None`.
	pub factory_lease:   Option<FactoryLease>,
	/// Start timestamp, when present.
	pub started:         Option<Str>,
	/// Exact elapsed wall time captured by the settlement patch.
	pub duration_ms:     Option<u64>,
	/// Bounded serialized output projected from the DOM.
	pub output:          Option<Box<RawValue>>,
	/// Terminal diagnostic projected from the DOM.
	pub error:           Option<Str>,
	/// Tool-call entry owned by this detached job.
	pub call:            Option<EntryId>,
	/// Initial detached `tool.result@1` entry which created this job.
	pub detached_at:     Option<EntryId>,
	/// Execution backend identity for a `JobKind::Execution` job.
	pub backend:         Option<Str>,
	/// Serialized reattach handle for a `JobKind::Execution` job.
	pub reattach_handle: Option<Str>,
	/// Serialized request identity for a `JobKind::Execution` job.
	pub request:         Option<Str>,
	/// Requested output mode for a `JobKind::Execution` job.
	pub output_mode:     Option<Str>,
	/// Fallback/error class recorded for a `JobKind::Execution` job.
	pub fallback:        Option<Str>,
	/// Explicit execution lifecycle state.
	pub execution_state: Option<ExecutionState>,

	/// Durable terminal execution result references.
	pub execution_result:  Option<Box<RawValue>>,
	/// Durable typed execution failure class.
	pub execution_failure: Option<ExecutionErrorKind>,
}

/// One factory lease. One custom prop, so a partial write cannot land.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FactoryLease {
	/// Worker holding the item.
	pub worker_id:   Str,
	/// Fencing token. A renew or release offering any other value writes
	/// nothing.
	pub lease_gen:   u64,
	/// Absent when placement has no TTL. Never the start instant.
	pub deadline_ms: Option<u64>,
}

/// Canonical text of one lease. An empty third field is no deadline.
pub fn factory_lease_prop(lease: &FactoryLease) -> Str {
	let deadline = lease.deadline_ms.map_or(String::new(), |ms| ms.to_string());
	Str::new(format!("{}|{}|{}", lease.worker_id, lease.lease_gen, deadline))
}

/// Folds `factory_lease`. A missing prop, or a value that is not three fields,
/// is `None`.
fn factory_lease(node: &omp_dom::Node) -> Option<FactoryLease> {
	let mut parts = custom(node, "factory_lease")?.split('|');
	let worker_id = parts.next().filter(|part| !part.is_empty())?;
	let lease_gen = parts.next()?.parse().ok()?;
	let deadline_ms = match parts.next()? {
		"" => None,
		text => Some(text.parse().ok()?),
	};
	if parts.next().is_some() {
		return None;
	}
	Some(FactoryLease { worker_id: Str::new(worker_id), lease_gen, deadline_ms })
}

/// A lease is past its deadline. A lease with no deadline is held until
/// release.
#[must_use]
pub fn lease_expired(lease: &FactoryLease, now_ms: u64) -> bool {
	lease.deadline_ms.is_some_and(|deadline| now_ms >= deadline)
}

/// Rejects a renew or release whose generation is stale or whose lease already
/// expired.
#[must_use]
pub fn accept_lease(current: Option<&FactoryLease>, offered_gen: u64, now_ms: u64) -> bool {
	current.is_some_and(|lease| lease.lease_gen == offered_gen && !lease_expired(lease, now_ms))
}

/// Writes the next generation of a held lease. `None` when the offered
/// generation is not the folded one, or the lease already expired.
#[must_use]
pub fn renew_lease(
	dom: &Dom,
	handle: Handle,
	cause: EntryId,
	offered_gen: u64,
	now_ms: u64,
	ttl_ms: Option<u64>,
) -> Option<Txn> {
	let node = dom.get(handle)?;
	let current = factory_lease(node)?;
	if !accept_lease(Some(&current), offered_gen, now_ms) {
		return None;
	}
	let next = FactoryLease {
		worker_id:   current.worker_id,
		lease_gen:   current.lease_gen.saturating_add(1),
		deadline_ms: ttl_ms.map(|ttl| now_ms.saturating_add(ttl)),
	};
	Some(lease_txn(handle, cause, factory_lease_prop(&next)))
}

/// Clears a held lease so the item is placeable again. `None` when the offered
/// generation is not the folded one.
#[must_use]
pub fn release_lease(dom: &Dom, handle: Handle, cause: EntryId, offered_gen: u64) -> Option<Txn> {
	let node = dom.get(handle)?;
	let current = factory_lease(node)?;
	if current.lease_gen != offered_gen {
		return None;
	}
	Some(lease_txn(handle, cause, Str::default()))
}

fn lease_txn(handle: Handle, cause: EntryId, value: Str) -> Txn {
	Txn {
		cause,
		label: Some(Str::new_static("jobs.lease")),
		ops: vec![Op::Set {
			h:     handle,
			prop:  PropKey::Custom(Str::new_static("factory_lease")),
			value: Value::Str(value),
		}],
	}
}

/// Live execution state retained after a tool call stops blocking its turn.
///
/// Durable identity and status remain in the DOM; this value owns only the
/// receiver, task and invocation feed needed to keep the execution unit alive.
pub(crate) struct DetachedCall {
	pub(crate) committer: Committer,
	pub(crate) identity:  ToolIdentity,
	pub(crate) call_id:   Str,
	pub(crate) call:      EntryId,
	pub(crate) options:   DispatchOptions,
	pub(crate) events:    Receiver<DispatchEvent>,
	pub(crate) task:      Option<JoinHandle<Result<(), RegistryError>>>,
	pub(crate) feed:      Option<InvocationFeed>,
	pub(crate) output:    OutputStream,
	pub(crate) closed:    bool,
}

type JobFactory = dyn Fn(CancellationToken) -> JoinHandle<JobSettlement> + Send + Sync + 'static;

struct RuntimeJob {
	record:             JobRecord,
	cancel:             CancellationToken,
	task:               Option<JoinHandle<JobSettlement>>,
	accepted:           Option<Receiver<ReattachHandle>>,
	accepted_handle:    Option<ReattachHandle>,
	accepted_committed: bool,
	_detached:          Option<DetachedCall>,
	/// A terminal call outcome recovered before its job settlement patch
	/// landed.
	recovered:          Option<RecoveredSettlement>,
	/// A running tool or subagent whose execution unit no longer exists
	/// (re-derived by a forward rewind or process restart): nothing can ever
	/// settle it, so [`JobBoard::poll`] journals it `failed`.
	orphaned:           bool,
}

impl RuntimeJob {
	fn capture_acceptance(&mut self) {
		if let Some(handle) = self
			.accepted
			.as_ref()
			.and_then(|receiver| receiver.try_recv().ok())
		{
			self.accepted_handle = Some(handle);
			self.accepted_committed = false;
		}
	}

	fn acceptance_pending(&self) -> bool {
		(!self.accepted_committed && self.accepted_handle.is_some())
			|| self
				.accepted
				.as_ref()
				.is_some_and(|receiver| !receiver.is_empty())
	}
}

#[derive(Clone)]
struct RecoveredSettlement {
	is_error: bool,
	status:   Option<Str>,
	artifact: Option<BlobRef>,
	output:   Option<Box<RawValue>>,
	error:    Option<Str>,
}

/// Custom job property carrying the journal-first workpool snapshot used for
/// restart adoption.
pub const WORKPOOL_STATE: &str = "workpool_state";

/// Stable diagnostic journaled on a detached tool call that lost its
/// execution unit.
pub const ORPHANED_TOOL_JOB: &str =
	"detached tool execution was lost across a rewind or restart and cannot settle";
/// Stable diagnostic journaled on a subagent whose execution unit disappeared.
///
/// Settling the stale `running` node is what makes the durable child eligible
/// for the ordinary explicit revive path; leaving it live would make both
/// `wait` and revive hang forever.
pub const ORPHANED_SUBAGENT_JOB: &str =
	"subagent execution was lost across a rewind or restart and can be revived";
/// Stable diagnostic journaled on an execution job whose backend connection was
/// lost.
pub const ORPHANED_EXECUTION_JOB: &str =
	"execution job was lost across a rewind or restart; reattach if a handle is available";
/// Stable diagnostic journaled when `start` fails before the backend returned
/// a handle. The backend may hold the request, so it is never started again.
pub const EXECUTION_START_FAILED: &str = "execution start failed; its submission is unknown";
/// Stable diagnostic journaled when an execution is cancelled before the
/// backend returned a handle.
pub const EXECUTION_CANCELLED_BEFORE_ACCEPTANCE: &str =
	"execution submission is unknown after cancellation";
/// Diagnostic journaled when an owned unit panicked or ended without returning
/// a settlement.
const UNSETTLED_UNIT: &str = "job execution unit ended without a settlement";

/// Host state outside the job board derived from the live session's journal,
/// which outlives that session because the kernel and its environment do.
///
/// Two transitions invalidate it. Every rewind of the live session
/// (`Session::rewind`, from any path: a host command, a checkpoint rewind, a
/// tool-tail retry) reaches the runtime through [`JobBoard::apply_lifecycle`],
/// which calls [`Self::rewound`]. A host that replaces the live session with
/// another one (new, resumed, forked or branched, handed off) tells the kernel
/// through [`crate::Kernel::session_switched`], which calls
/// [`Self::switched`]. Both hand the observer the session the kernel now
/// serves, so it can drop state that journal no longer justifies or re-derive
/// it from that journal; it must not block.
pub trait SessionObserver: Send + Sync {
	/// The live session was rewound; `session` is that session, already on the
	/// branch the rewind left live.
	fn rewound(&self, session: &Session);

	/// The host replaced the live session with `next`, whose journal never
	/// decided what this state was derived from.
	fn switched(&self, next: &Session);
}

/// A disposable runtime index over the authoritative jobs subtree.
///
/// Rebuilding preserves a live execution unit by durable `id`, remaps it to
/// the newly-derived handle, and cancels units absent from the new tree.
pub struct JobBoard {
	jobs:           Mutex<FastHashMap<Handle, RuntimeJob>>,
	factories:      Mutex<FastHashMap<Str, Arc<JobFactory>>>,
	hooks:          Mutex<Option<crate::LifecycleHooks>>,
	/// Largest settlement output published inline on a job element; larger
	/// outputs are spilled to the session blob store (ADR 0009: the DOM and
	/// every patch it emits stay bounded, the full result lives in the CAS).
	output_bound:   AtomicUsize,
	/// Dispatcher spill namespace. A detached artifact is copied into the
	/// session namespace before its durable job settlement references it.
	artifact_store: Mutex<Option<BlobStore>>,
	/// Host state told about every rewind and every session switch.
	observers:      Mutex<Vec<Arc<dyn SessionObserver>>>,
}

impl Default for JobBoard {
	fn default() -> Self {
		Self {
			jobs:           Mutex::default(),
			factories:      Mutex::default(),
			hooks:          Mutex::default(),
			output_bound:   AtomicUsize::new(crate::DispatchPolicy::DEFAULT_MAX_OUTPUT_BYTES),
			artifact_store: Mutex::default(),
			observers:      Mutex::default(),
		}
	}
}

impl JobBoard {
	/// Creates an empty runtime index.
	#[must_use]
	pub fn new() -> Self {
		Self::default()
	}

	/// Aligns the inline settlement bound with the dispatcher's central
	/// `max_output_bytes`.
	pub fn set_output_bound(&self, bytes: usize) {
		self.output_bound.store(bytes, Ordering::Relaxed);
	}

	/// Supplies the dispatcher namespace from which detached output is pinned
	/// into the session blob store before the settlement patch is appended.
	pub fn set_artifact_store(&self, store: BlobStore) {
		*self.artifact_store.lock() = Some(store);
	}

	/// Installs the extension observer for `job_registered`/`job_settled`.
	pub fn set_lifecycle_hooks(&self, hooks: crate::LifecycleHooks) {
		*self.hooks.lock() = Some(hooks);
	}

	/// Registers host state that every later rewind of the live session, and
	/// every switch to another session, invalidates.
	pub fn observe_sessions(&self, observer: Arc<dyn SessionObserver>) {
		self.observers.lock().push(observer);
	}

	/// Tells every [`SessionObserver`] that the host replaced the live session
	/// with `next`.
	pub fn session_switched(&self, next: &Session) {
		for observer in self.observers.lock().iter() {
			observer.switched(next);
		}
	}

	fn notify_registered(&self, record: &JobRecord) {
		let hooks = self.hooks.lock();
		let Some(hooks) = hooks.as_ref() else {
			return;
		};
		let _ = hooks.notify(
			HookEventId::HookEventJobRegistered,
			serde_json::json!({
				"job_id": record.id,
				"owner": record.owner.as_deref().unwrap_or("kernel"),
				"call_id": serde_json::Value::Null,
				"lifetime": "session",
				"expected_artifact": serde_json::Value::Null,
			}),
		);
	}

	fn settled_event(
		&self,
		record: &JobRecord,
		settlement: &JobSettlement,
	) -> Option<(crate::LifecycleHooks, serde_json::Value)> {
		let hooks = self.hooks.lock().clone()?;
		let artifact = settlement.output.as_deref().and_then(|output| {
			serde_json::from_str::<serde_json::Value>(output.get())
				.ok()?
				.get("artifact")?
				.as_str()
				.map(str::to_owned)
		});
		Some((
			hooks,
			serde_json::json!({
				"job_id": record.id,
				"owner": record.owner.as_deref().unwrap_or("kernel"),
				"artifact": artifact,
				"failed": settlement.status.as_str() != "completed",
				"duration": format!("{}s", elapsed_ms(record.started.as_deref(), now_ms()) / 1_000),
			}),
		))
	}

	/// Attaches the runtime kill boundary for a job already present in the DOM.
	/// Returns false when `handle` is not a lifecycle-bearing job element.
	pub fn attach(&self, dom: &Dom, handle: Handle, cancel: CancellationToken) -> bool {
		let Some(record) = record(dom, handle) else {
			return false;
		};
		self.notify_registered(&record);
		self.jobs.lock().insert(handle, RuntimeJob {
			record,
			cancel,
			task: None,
			accepted: None,
			accepted_handle: None,
			accepted_committed: false,
			_detached: None,
			recovered: None,
			orphaned: false,
		});
		true
	}

	/// Adopts a timed-out tool execution already represented by a `<job>`
	/// element. Retaining its live receiver and task keeps the returned job
	/// reference observable and cancellable after the foreground turn resumes.
	pub(crate) fn adopt_tool_job(
		&self,
		session: &Session,
		id: &Str,
		cancel: CancellationToken,
		detached: DetachedCall,
	) -> bool {
		let Some(record) = records(session.dom())
			.into_iter()
			.find(|record| &record.id == id)
		else {
			return false;
		};
		self.notify_registered(&record);
		self.jobs.lock().insert(record.handle, RuntimeJob {
			record,
			cancel,
			task: None,
			accepted: None,
			accepted_handle: None,
			accepted_committed: false,
			_detached: Some(detached),
			recovered: None,
			orphaned: false,
		});
		true
	}

	/// Attaches an owned execution task to a DOM job.
	pub fn attach_task(
		&self,
		dom: &Dom,
		handle: Handle,
		cancel: CancellationToken,
		task: JoinHandle<JobSettlement>,
	) -> bool {
		let Some(record) = record(dom, handle) else {
			return false;
		};
		self.notify_registered(&record);
		self.jobs.lock().insert(handle, RuntimeJob {
			record,
			cancel,
			task: Some(task),
			accepted: None,
			accepted_handle: None,
			accepted_committed: false,
			_detached: None,
			recovered: None,
			orphaned: false,
		});
		true
	}

	/// Attaches a restartable execution factory for rewind/resume lifecycle.
	pub fn attach_restartable<F>(&self, dom: &Dom, handle: Handle, factory: F) -> bool
	where
		F: Fn(CancellationToken) -> JoinHandle<JobSettlement> + Send + Sync + 'static,
	{
		let Some(record) = record(dom, handle) else {
			return false;
		};
		let factory: Arc<JobFactory> = Arc::new(factory);
		let cancel = CancellationToken::new();
		let task = factory(cancel.clone());
		self.notify_registered(&record);
		self.factories.lock().insert(record.id.clone(), factory);
		self.jobs.lock().insert(handle, RuntimeJob {
			record,
			cancel,
			task: Some(task),
			accepted: None,
			accepted_handle: None,
			accepted_committed: false,
			_detached: None,
			recovered: None,
			orphaned: false,
		});
		true
	}

	/// Attaches a backend-owned execution task to a journaled
	/// `JobKind::Execution` node.
	///
	/// The caller must append the node through
	/// `omp_session::components::jobs::insert_execution` and commit
	/// `mark_execution_submitting` first. Without that marker this refuses and
	/// returns false: a unit lost before acceptance would otherwise restart as
	/// never submitted, although it may have reached the backend. This method
	/// only installs the disposable cancellation and settlement boundary.
	pub fn attach_execution_task(
		&self,
		dom: &Dom,
		handle: Handle,
		cancel: CancellationToken,
		task: JoinHandle<JobSettlement>,
	) -> bool {
		self.attach_execution_task_with_acceptance(dom, handle, cancel, task, None)
	}

	fn attach_execution_task_with_acceptance(
		&self,
		dom: &Dom,
		handle: Handle,
		cancel: CancellationToken,
		task: JoinHandle<JobSettlement>,
		accepted: Option<Receiver<ReattachHandle>>,
	) -> bool {
		let Some(record) = submitted_execution(dom, handle) else {
			return false;
		};
		self.install_execution(record, cancel, task, accepted);
		true
	}

	fn install_execution(
		&self,
		record: JobRecord,
		cancel: CancellationToken,
		task: JoinHandle<JobSettlement>,
		accepted: Option<Receiver<ReattachHandle>>,
	) {
		self.notify_registered(&record);
		self.jobs.lock().insert(record.handle, RuntimeJob {
			record,
			cancel,
			task: Some(task),
			accepted,
			accepted_handle: None,
			accepted_committed: false,
			_detached: None,
			recovered: None,
			orphaned: false,
		});
	}

	/// Attaches a live backend run, supervising it until settlement.
	///
	/// Maps terminal [`omp_exec::ExecutionEvent`]s to one durable
	/// `jobs.settle` patch through [`Self::poll`]. Like
	/// [`Self::attach_execution_task`] it refuses a node without the submission
	/// marker; the refused `run` is dropped, which cancels it.
	pub fn attach_execution_run<R>(
		&self,
		dom: &Dom,
		handle: Handle,
		cancel: CancellationToken,
		run: R,
	) -> bool
	where
		R: ExecutionRun + 'static,
	{
		let Some(record) = submitted_execution(dom, handle) else {
			return false;
		};
		let (accepted_tx, accepted_rx) = flume::bounded(1);
		let task = tokio::spawn(supervise_execution(run, cancel.clone(), Some(accepted_tx), None));
		self.install_execution(record, cancel, task, Some(accepted_rx));
		true
	}

	/// Journals one approved execution and starts it on `backend`: the producer
	/// for agent-initiated runs.
	///
	/// The `requested` node and then its submission marker are committed before
	/// anything reaches the backend. A spawned starting owner then calls `start`
	/// exactly once and supervises the run, so acceptance and settlement reach
	/// the journal through [`Self::poll`]. Nothing is retried, and nothing falls
	/// back to another backend.
	///
	/// # Errors
	///
	/// Refuses a duplicate execution id, a ticket an earlier execution already
	/// submitted under, or a session without a jobs subtree, and returns the
	/// journal failure when either commit fails. `start` never runs in any of
	/// these cases. When only the marker fails to commit, the `requested` node
	/// stays indexed, so the next [`Self::poll`] settles it `NotSubmitted`.
	pub fn start_execution<B: ExecutionBackend>(
		&self,
		session: &mut Session,
		authorized: AuthorizedExecution,
		backend: B,
		submission: ExecutionSubmission,
	) -> Result<JobRecord, ExecutionStartError> {
		use omp_session::components::jobs::{
			ExecutionSpec, insert_execution, mark_execution_submitting,
		};

		let ExecutionSubmission { owner, identity, request } = submission;
		let id = request.id.as_inner().clone();
		let existing = records(session.dom());
		if existing.iter().any(|record| record.id == id) {
			return Err(ExecutionStartError::Duplicate { id });
		}
		let ticket = authorized.ticket.as_str();
		if existing
			.iter()
			.any(|record| submitted_under(session.dom(), record, ticket))
		{
			return Err(ExecutionStartError::ApprovalSpent { ticket: authorized.ticket });
		}
		let started = now_ms();
		let timeout = u64::try_from(request.timeout.as_millis()).unwrap_or(u64::MAX);
		let output_mode =
			serde_json::to_string(&request.outputs).map_err(omp_session::SessionError::from)?;
		let cause = session
			.head()
			.ok_or(omp_session::SessionError::NoActiveTurn)?;
		let insert = insert_execution(session.dom(), cause, ExecutionSpec {
			id: id.clone(),
			owner,
			started: Str::new(started.to_string()),
			backend: Str::new(backend.id().as_str()),
			request: identity,
			output_mode: Str::new(output_mode),
			destination: authorized.destination,
			deadline: Str::new(started.saturating_add(timeout).to_string()),
			approval: authorized.ticket,
		})
		.ok_or(ExecutionStartError::NoJobs)?;
		let inserted = session.patch(insert)?;
		let handle = records(session.dom())
			.into_iter()
			.find(|record| record.id == id)
			.ok_or(ExecutionStartError::NoJobs)?
			.handle;
		if let Err(error) = session.patch(mark_execution_submitting(inserted, handle)) {
			// The `requested` node landed without its marker: index it so the next
			// poll settles it `NotSubmitted` instead of leaving it running.
			self.rebuild(session);
			return Err(error.into());
		}
		let record = submitted_execution(session.dom(), handle).ok_or(ExecutionStartError::NoJobs)?;
		let cancel = CancellationToken::new();
		let (accepted_tx, accepted_rx) = flume::bounded(1);
		let task = tokio::spawn(start_execution_run(backend, request, cancel.clone(), accepted_tx));
		self.install_execution(record.clone(), cancel, task, Some(accepted_rx));
		Ok(record)
	}

	/// Rebuilds the index after open or rewind. The DOM is the source of truth.
	pub fn rebuild(&self, session: &Session) {
		let records = records(session.dom());
		let mut jobs = self.jobs.lock();
		let mut by_id: FastHashMap<Str, RuntimeJob> = std::mem::take(&mut *jobs)
			.into_values()
			.map(|job| (job.record.id.clone(), job))
			.collect();
		for record in records {
			let mut job = by_id.remove(&record.id).unwrap_or_else(|| {
				let recovered = recovered_settlement(session.dom(), &record);
				RuntimeJob {
					// Terminal detached-tool artifacts and drained workpool
					// snapshots are adopted into their job nodes. A genuinely
					// running tool or child without an execution unit is
					// orphaned so wait/revive cannot hang.
					orphaned: restart_orphan(&record, recovered.as_ref()),
					recovered,
					record: record.clone(),
					cancel: CancellationToken::new(),
					task: None,
					accepted: None,
					accepted_handle: None,
					accepted_committed: false,
					_detached: None,
				}
			});
			let retained_live_owner =
				is_live_status(job.record.status.as_str()) && !job.orphaned && job.recovered.is_none();
			job.record = record.clone();
			if !is_live_status(record.status.as_str()) {
				job.recovered = None;
				job.orphaned = false;
			} else if !retained_live_owner && job.task.is_none() && job._detached.is_none() {
				job.recovered = recovered_settlement(session.dom(), &record);
				job.orphaned = restart_orphan(&record, job.recovered.as_ref());
			}
			jobs.insert(record.handle, job);
		}
		for job in by_id.into_values() {
			job.cancel.cancel();
		}
	}

	/// Commits every already-settled owned task to the authoritative job node.
	pub fn poll(&self, session: &mut Session) -> Result<Vec<JobRecord>, omp_session::SessionError> {
		let accepted = {
			let mut jobs = self.jobs.lock();
			jobs
				.iter_mut()
				.filter_map(|(handle, job)| {
					job.capture_acceptance();
					if job.accepted_committed {
						None
					} else {
						job.accepted_handle
							.clone()
							.map(|accepted| (*handle, accepted))
					}
				})
				.collect::<Vec<_>>()
		};
		for (handle, accepted) in accepted {
			commit_settlement(
				session,
				handle,
				JobSettlement {
					status:     Str::new_static("running"),
					output:     None,
					error:      None,
					completion: None,
					execution:  Some(ExecutionSettlement {
						state:  ExecutionState::Accepted,
						handle: Some(accepted.clone()),
						result: None,
						error:  None,
					}),
				},
				self.output_bound.load(Ordering::Relaxed),
			)?;
			if let Some(job) = self.jobs.lock().get_mut(&handle)
				&& job.accepted_handle.as_ref() == Some(&accepted)
			{
				job.accepted_committed = true;
			}
		}

		let recovered = {
			let jobs = self.jobs.lock();
			jobs
				.iter()
				.filter_map(|(handle, job)| {
					job.recovered
						.clone()
						.map(|settlement| (*handle, settlement))
				})
				.collect::<Vec<_>>()
		};
		for (handle, settlement) in recovered {
			let settlement = self.materialize_recovered(session, settlement)?;
			self.commit(session, handle, settlement)?;
			if let Some(job) = self.jobs.lock().get_mut(&handle) {
				job.recovered = None;
			}
		}
		let orphaned = {
			let jobs = self.jobs.lock();
			jobs
				.iter()
				.filter(|(_, job)| job.orphaned)
				.map(|(handle, job)| (*handle, job.record.kind))
				.collect::<Vec<_>>()
		};
		for (handle, kind) in orphaned {
			let (error, execution) = match kind {
				JobKind::Subagent => (ORPHANED_SUBAGENT_JOB, None),
				JobKind::Tool | JobKind::Process => (ORPHANED_TOOL_JOB, None),
				JobKind::Execution => {
					(ORPHANED_EXECUTION_JOB, Some(orphaned_execution(session.dom(), handle)))
				},
			};
			self.commit(session, handle, JobSettlement {
				status: Str::new_static("failed"),
				output: None,
				error: Some(Str::new_static(error)),
				completion: None,
				execution,
			})?;
			if let Some(job) = self.jobs.lock().get_mut(&handle) {
				job.orphaned = false;
			}
		}
		let detached = {
			let mut jobs = self.jobs.lock();
			jobs
				.iter_mut()
				.filter_map(|(handle, job)| {
					let detached = job._detached.as_mut()?;
					match detached.poll(session) {
						Ok(Some(report)) => {
							job._detached = None;
							let settlement = recovered_settlement(session.dom(), &job.record).unwrap_or(
								RecoveredSettlement {
									is_error: report.is_error,
									status:   None,
									artifact: report.spilled,
									output:   None,
									error:    None,
								},
							);
							job.recovered = Some(settlement.clone());
							Some((*handle, settlement))
						},
						Ok(None) => None,
						Err(error) => {
							tracing::warn!(?error, "detached tool settlement failed");
							job._detached = None;
							let settlement = RecoveredSettlement {
								is_error: true,
								status:   None,
								artifact: None,
								output:   None,
								error:    Some(Str::new_static("detached tool settlement failed")),
							};
							job.recovered = Some(settlement.clone());
							Some((*handle, settlement))
						},
					}
				})
				.collect::<Vec<_>>()
		};
		for (handle, settlement) in detached {
			let settlement = self.materialize_recovered(session, settlement)?;
			self.commit(session, handle, settlement)?;
			if let Some(job) = self.jobs.lock().get_mut(&handle) {
				job.recovered = None;
			}
		}
		let finished = {
			let mut jobs = self.jobs.lock();
			jobs
				.iter_mut()
				.filter(|(_, job)| job.task.as_ref().is_some_and(JoinHandle::is_finished))
				.filter_map(|(handle, job)| {
					let task = job.task.take()?;
					job.capture_acceptance();
					let execution =
						(job.record.kind == JobKind::Execution).then(|| job.accepted_handle.clone());
					Some((*handle, task, execution))
				})
				.collect::<Vec<_>>()
		};
		for (handle, mut task, execution) in finished {
			// A panicked execution unit may still have reached the backend.
			let settlement = futures::FutureExt::now_or_never(&mut task)
				.and_then(Result::ok)
				.unwrap_or_else(|| match execution {
					Some(accepted) => {
						unsettled_execution("failed", accepted.as_ref(), UNSETTLED_UNIT, UNSETTLED_UNIT)
					},
					None => JobSettlement {
						status:     Str::new_static("failed"),
						output:     None,
						error:      Some(Str::new_static(UNSETTLED_UNIT)),
						completion: None,
						execution:  None,
					},
				});
			self.commit(session, handle, settlement)?;
		}
		self.rebuild(session);
		Ok(self.list())
	}

	/// Whether the named execution unit has finished but has not yet been
	/// committed to its DOM node.
	#[must_use]
	pub fn has_finished(&self, id: &str) -> bool {
		self.jobs.lock().values().any(|job| {
			job.record.id == id
				&& (job.orphaned
					|| job.recovered.is_some()
					|| job.acceptance_pending()
					|| job.task.as_ref().is_some_and(JoinHandle::is_finished)
					|| job._detached.as_ref().is_some_and(|detached| {
						detached.closed
							|| !detached.events.is_empty()
							|| detached.task.as_ref().is_some_and(JoinHandle::is_finished)
					}))
		})
	}

	/// Whether any owned execution unit has finished but not yet been
	/// committed to its DOM node (a cheap check the turn loop makes before
	/// deciding whether to poll).
	#[must_use]
	pub fn has_finished_units(&self) -> bool {
		self.jobs.lock().values().any(|job| {
			job.orphaned
				|| job.recovered.is_some()
				|| job.acceptance_pending()
				|| job.task.as_ref().is_some_and(JoinHandle::is_finished)
				|| job._detached.as_ref().is_some_and(|detached| {
					detached.closed
						|| !detached.events.is_empty()
						|| detached.task.as_ref().is_some_and(JoinHandle::is_finished)
				})
		})
	}

	/// Resolves once any owned execution unit finishes (never when none is
	/// live); used by hosts idling between turns.
	pub async fn any_finished(&self) {
		loop {
			if self.has_finished_units() {
				return;
			}
			let live = self.jobs.lock().values().any(|job| {
				job.task.is_some()
					|| job
						._detached
						.as_ref()
						.is_some_and(|detached| !detached.closed)
			});
			if !live {
				std::future::pending::<()>().await;
			}
			time::sleep(std::time::Duration::from_millis(25)).await;
		}
	}

	fn materialize_recovered(
		&self,
		session: &Session,
		settlement: RecoveredSettlement,
	) -> Result<JobSettlement, omp_session::SessionError> {
		let output = match settlement.output {
			Some(output) => Some(output),
			None => settlement
				.artifact
				.map(|artifact| self.pin_artifact(session, artifact))
				.transpose()?
				.map(|artifact| {
					serde_json::value::to_raw_value(&SpilledOutput {
						artifact: Str::new(format!("artifact://sha256/{}", artifact.to_hex())),
						byte_len: artifact.size,
						text:     None,
					})
				})
				.transpose()?,
		};
		Ok(JobSettlement {
			status: settlement.status.unwrap_or_else(|| {
				Str::new_static(if settlement.is_error {
					"failed"
				} else {
					"completed"
				})
			}),
			output,
			error: settlement.error,
			completion: None,
			execution: None,
		})
	}

	fn pin_artifact(
		&self,
		session: &Session,
		reference: BlobRef,
	) -> Result<BlobRef, omp_session::SessionError> {
		if session.blobs().has(&reference) {
			if session.blobs().verify(&reference)? {
				return Ok(reference);
			}
			let bytes = session.blobs().get(&reference)?;
			return Err(
				omp_journal::blob::Error::DigestMismatch {
					expected: reference.hash,
					actual:   omp_core::Hash32::sum(&bytes),
				}
				.into(),
			);
		}
		let source = self
			.artifact_store
			.lock()
			.clone()
			.ok_or(omp_journal::blob::Error::NotFound)?;
		let bytes = source.get(&reference)?;
		let actual = omp_core::Hash32::sum(&bytes);
		if actual != reference.hash {
			return Err(
				omp_journal::blob::Error::DigestMismatch { expected: reference.hash, actual }.into(),
			);
		}
		let pinned = session.blobs().put(&bytes)?;
		Ok(pinned)
	}

	fn commit(
		&self,
		session: &mut Session,
		handle: Handle,
		settlement: JobSettlement,
	) -> Result<(), omp_session::SessionError> {
		let event =
			record(session.dom(), handle).and_then(|record| self.settled_event(&record, &settlement));
		commit_settlement(session, handle, settlement, self.output_bound.load(Ordering::Relaxed))?;
		// JobSettled reports durable settlements only, so a failed append that a
		// later retry commits fires the hook once.
		if let Some((hooks, payload)) = event {
			let _ = hooks.notify(HookEventId::HookEventJobSettled, payload);
		}
		Ok(())
	}

	/// Waits for the first selected job to settle while committing completions.
	pub async fn wait(
		&self,
		session: &mut Session,
		ids: Option<&[Str]>,
	) -> Result<Option<JobRecord>, omp_session::SessionError> {
		loop {
			let records = self.poll(session)?;
			if let Some(record) = records.into_iter().find(|record| {
				!is_live_status(record.status.as_str())
					&& ids.is_none_or(|selected| {
						selected.is_empty() || selected.iter().any(|id| id == &record.id)
					})
			}) {
				return Ok(Some(record));
			}
			let selected_live = self.list().into_iter().any(|record| {
				is_live_status(record.status.as_str())
					&& ids.is_none_or(|selected| {
						selected.is_empty() || selected.iter().any(|id| id == &record.id)
					})
			});
			if !selected_live {
				return Ok(None);
			}
			time::sleep(std::time::Duration::from_millis(10)).await;
		}
	}

	/// Applies lifecycle work returned by `Session::rewind`, then remaps
	/// retained handles. Removed executions are cooperatively cancelled and
	/// their owned tasks are force-aborted after a bounded grace. Added
	/// records are re-derived from `session` rather than left invisible.
	/// Every [`SessionObserver`] is told first.
	pub fn apply_lifecycle(
		&self,
		session: &Session,
		work: &LifecycleWork,
	) -> impl Future<Output = ()> + Send + 'static {
		for observer in self.observers.lock().iter() {
			observer.rewound(session);
		}
		let mut terminated = Vec::new();
		{
			let mut jobs = self.jobs.lock();
			for handle in &work.terminate {
				if let Some(job) = jobs.remove(handle) {
					job.cancel.cancel();
					terminated.push(job);
				}
			}
			for (old, new) in &work.retained {
				if let Some(mut job) = jobs.remove(old) {
					job.record.handle = *new;
					jobs.insert(*new, job);
				}
			}
		}
		self.rebuild(session);
		for handle in &work.spawn {
			let Some(record) = record(session.dom(), *handle) else {
				continue;
			};
			let Some(factory) = self.factories.lock().get(&record.id).cloned() else {
				continue;
			};
			let cancel = CancellationToken::new();
			let task = factory(cancel.clone());
			self.jobs.lock().insert(*handle, RuntimeJob {
				record,
				cancel,
				task: Some(task),
				accepted: None,
				accepted_handle: None,
				accepted_committed: false,
				_detached: None,
				recovered: None,
				orphaned: false,
			});
		}
		async move {
			for mut job in terminated {
				let _ = terminate_runtime(&mut job).await;
			}
		}
	}

	/// Terminates one execution unit and journals `cancelled` on its DOM node.
	///
	/// A settled execution keeps its committed result and returns `false`.
	/// When the journal append fails, the computed settlement stays on the
	/// board as a finished unit, so a later poll or terminate commits it.
	pub async fn terminate(
		&self,
		session: &mut Session,
		handle: Handle,
	) -> Result<bool, omp_session::SessionError> {
		if record(session.dom(), handle).is_some_and(|record| {
			record.kind == JobKind::Execution && !is_live_status(record.status.as_str())
		}) {
			return Ok(false);
		}
		let recovered = self
			.jobs
			.lock()
			.get(&handle)
			.and_then(|job| job.recovered.clone());
		if let Some(recovered) = recovered {
			let settlement = self.materialize_recovered(session, recovered)?;
			self.commit(session, handle, settlement)?;
			self.rebuild(session);
			return Ok(true);
		}
		let job = {
			let mut jobs = self.jobs.lock();
			if let Some(job) = jobs.get_mut(&handle) {
				job.capture_acceptance();
			}
			jobs.remove(&handle)
		};
		let Some(mut job) = job else {
			return Ok(false);
		};
		let settlement = terminate_runtime(&mut job).await;
		if let Err(error) = self.commit(session, handle, settlement.clone()) {
			job.task = Some(tokio::spawn(std::future::ready(settlement)));
			// The re-armed settlement is the one terminal commit left for this job.
			job.orphaned = false;
			self.jobs.lock().insert(handle, job);
			return Err(error);
		}
		self.rebuild(session);
		Ok(true)
	}

	/// Returns the current DOM-derived roster.
	#[must_use]
	pub fn list(&self) -> Vec<JobRecord> {
		let mut records = self
			.jobs
			.lock()
			.values()
			.map(|job| job.record.clone())
			.collect::<Vec<_>>();
		records.sort_by(|left, right| left.id.cmp(&right.id));
		records
	}
}

async fn terminate_runtime(job: &mut RuntimeJob) -> JobSettlement {
	job.capture_acceptance();
	job.cancel.cancel();
	let mut settlement = None;
	if let Some(mut task) = job.task.take() {
		match time::timeout(std::time::Duration::from_secs(1), &mut task).await {
			Ok(Ok(result)) if job.record.kind == JobKind::Execution => settlement = Some(result),
			Ok(_) => {},
			Err(_) => {
				task.abort();
				let _ = task.await;
			},
		}
	}
	job.capture_acceptance();
	if let Some(mut detached) = job._detached.take() {
		if let Some(feed) = &detached.feed {
			let _ = feed.interrupt(omp_tool::Interrupt {
				class:  Str::new_static(omp_tool::Interrupt::ESCAPE),
				reason: Str::new_static("job removed by session lifecycle diff"),
			});
		}
		if let Some(mut task) = detached.task.take()
			&& time::timeout(std::time::Duration::from_secs(1), &mut task)
				.await
				.is_err()
		{
			task.abort();
			let _ = task.await;
		}
	}
	if let Some(settlement) = settlement {
		return settlement;
	}
	if job.record.kind == JobKind::Execution
		&& (job.accepted_handle.is_some() || job.accepted.is_some())
	{
		// A supervised run means `start` ran, so the backend may hold the request.
		return unsettled_execution(
			"cancelled",
			job.accepted_handle.as_ref(),
			"execution effects are unknown after cancellation",
			EXECUTION_CANCELLED_BEFORE_ACCEPTANCE,
		);
	}
	JobSettlement {
		status:     Str::new_static("cancelled"),
		output:     None,
		error:      None,
		completion: None,
		execution:  None,
	}
}

/// Starting owner of a produced execution: calls `start` exactly once, then
/// supervises the run it returns.
///
/// Nothing before a handle is retried. The backend may hold the request, so a
/// failed or cancelled start settles `SubmissionUnknown` and never invents a
/// handle.
async fn start_execution_run<B: ExecutionBackend>(
	backend: B,
	request: ExecutionRequest,
	cancel: CancellationToken,
	accepted_tx: flume::Sender<ReattachHandle>,
) -> JobSettlement {
	let started = tokio::select! {
		started = backend.start(request) => started,
		() = cancel.cancelled() => {
			return submission_unknown(
				"cancelled",
				EXECUTION_CANCELLED_BEFORE_ACCEPTANCE,
				ExecutionError::SubmissionUnknown,
			);
		},
	};
	match started {
		Ok(run) => supervise_execution(run, cancel, Some(accepted_tx), None).await,
		Err(error) => submission_unknown("failed", EXECUTION_START_FAILED, error),
	}
}

/// Settlement of an execution that may have reached the backend before any
/// handle existed: never retried, and never given an invented handle.
const fn submission_unknown(
	status: &'static str,
	message: &'static str,
	error: ExecutionError,
) -> JobSettlement {
	JobSettlement {
		status:     Str::new_static(status),
		output:     None,
		error:      Some(Str::new_static(message)),
		completion: None,
		execution:  Some(ExecutionSettlement {
			state:  ExecutionState::SubmissionUnknown,
			handle: None,
			result: None,
			error:  Some(error),
		}),
	}
}

/// Settlement of an execution unit that ended without settling itself. An
/// accepted run keeps its handle as `EffectsUnknown`; without a handle the
/// backend may still hold the request, so it is `SubmissionUnknown`.
fn unsettled_execution(
	status: &'static str,
	accepted: Option<&ReattachHandle>,
	effects_unknown: &'static str,
	submission: &'static str,
) -> JobSettlement {
	let Some(handle) = accepted else {
		return submission_unknown(status, submission, ExecutionError::SubmissionUnknown);
	};
	JobSettlement {
		status:     Str::new_static(status),
		output:     None,
		error:      Some(Str::new_static(effects_unknown)),
		completion: None,
		execution:  Some(ExecutionSettlement {
			state:  ExecutionState::EffectsUnknown,
			handle: Some(handle.clone()),
			result: None,
			error:  Some(ExecutionError::EffectsUnknown { handle: handle.clone() }),
		}),
	}
}

/// The record of an execution job whose submission marker is journaled. Only
/// such a job may own a live unit.
fn submitted_execution(dom: &Dom, handle: Handle) -> Option<JobRecord> {
	let marker =
		PropKey::Custom(Str::new_static(omp_session::components::jobs::EXECUTION_SUBMISSION_STARTED));
	dom.get(handle)?.prop(&marker)?;
	record(dom, handle)
}

/// Whether `record` is an execution that already reached the backend under
/// `ticket`. Only the submission marker spends an approval: a `requested`
/// node whose marker never landed started nothing.
fn submitted_under(dom: &Dom, record: &JobRecord, ticket: &str) -> bool {
	use omp_session::components::jobs::{EXECUTION_APPROVAL, EXECUTION_SUBMISSION_STARTED};

	record.kind == JobKind::Execution
		&& dom.get(record.handle).is_some_and(|node| {
			custom(node, EXECUTION_APPROVAL) == Some(ticket)
				&& node
					.prop(&PropKey::Custom(Str::new_static(EXECUTION_SUBMISSION_STARTED)))
					.is_some()
		})
}

async fn supervise_execution<R: ExecutionRun>(
	mut run: R,
	cancel: CancellationToken,
	accepted_tx: Option<flume::Sender<ReattachHandle>>,
	mut accepted: Option<ReattachHandle>,
) -> JobSettlement {
	loop {
		tokio::select! {
			 event = run.next_event() => {
				  match event {
						Some(ExecutionEvent::Accepted { handle }) => {
							if let Some(sender) = accepted_tx.as_ref() {
								let _ = sender.send(handle.clone());
							}
							accepted = Some(handle);
						},
						Some(ExecutionEvent::Settled(Ok(result))) => {
							 let status = if result.outcome.is_success() {
								  Str::new_static("completed")
							 } else {
								  Str::new_static("failed")
							 };
							 return JobSettlement {
								  status,
								  output: None,
								  error: None,
								  completion: None,
								  execution: Some(ExecutionSettlement {
										state: ExecutionState::Settled,
										handle: accepted,
										result: Some(result),
										error: None,
								  }),
							 };
						},
						Some(ExecutionEvent::Settled(Err(error))) => {
						 let (state, handle) = match &error {
							 ExecutionError::EffectsUnknown { handle } => {
								 (ExecutionState::EffectsUnknown, Some(handle.clone()))
							 },
							 _ if accepted.is_some() => (ExecutionState::Settled, accepted),
							 // No handle yet: the backend may hold the request.
							 _ => (ExecutionState::SubmissionUnknown, None),
						 };
							 return JobSettlement {
								  status: Str::new_static("failed"),
								  output: None,
								  error: Some(Str::new_static("execution failed")),
								  completion: None,
								  execution: Some(ExecutionSettlement {
										state,
										handle,
										result: None,
										error: Some(error),
								  }),
							 };
						},
						Some(_) => continue,
						None => {
							 let (state, handle, error, message) = match accepted {
								  Some(handle) => {
										let error = ExecutionError::EffectsUnknown { handle: handle.clone() };
										(
											 ExecutionState::EffectsUnknown,
											 Some(handle),
											 Some(error),
											 Str::new_static("execution effects are unknown after acceptance"),
										)
								  },
								  None => (
										ExecutionState::SubmissionUnknown,
										None,
										Some(ExecutionError::SubmissionUnknown),
										Str::new_static("execution run ended without a result"),
								  ),
							 };
							 return JobSettlement {
								  status: Str::new_static("failed"),
								  output: None,
								  error: Some(message),
								  completion: None,
								  execution: Some(ExecutionSettlement {
										state,
										handle,
										result: None,
										error,
								  }),
							 };
						},
				  }
			 },
			 _ = cancel.cancelled() => {
				  let _ = run.cancel().await;
				  let Some(handle) = accepted else {
						return submission_unknown(
							 "cancelled",
							 EXECUTION_CANCELLED_BEFORE_ACCEPTANCE,
							 ExecutionError::SubmissionUnknown,
						);
				  };
				  let error = ExecutionError::EffectsUnknown { handle: handle.clone() };
				  return JobSettlement {
						status: Str::new_static("cancelled"),
						output: None,
						error: Some(Str::new_static("execution effects are unknown after cancellation")),
						completion: None,
						execution: Some(ExecutionSettlement {
							 state: ExecutionState::EffectsUnknown,
							 handle: Some(handle),
							 result: None,
							 error: Some(error),
						}),
				  };
			 },
		}
	}
}

/// Returns a cloned [`JobRecord`] and its opaque reattach handle for the given
/// execution job id, or `None` if the job does not exist, is not of kind
/// `Execution`, or has no reattach handle recorded.
///
/// The reattach handle is the raw serialized string the backend wrote when it
/// accepted the job. Pass it to the backend's reattach API to reconnect.
///
/// # TODO
/// Wire a backend registry here and call `backend.reattach(handle)` once the
/// `ExecutionBackend` trait objects land.
#[must_use]
pub fn reattach_execution(board: &JobBoard, id: &str) -> Option<(JobRecord, Str)> {
	let records = board.list();
	let record = records.into_iter().find(|r| r.id.as_str() == id)?;
	if record.kind != JobKind::Execution {
		return None;
	}
	let handle = record.reattach_handle.clone()?;
	// TODO: call backend.reattach(&handle) to reconnect the live run.
	Some((record, handle))
}

fn recovered_settlement(dom: &Dom, record: &JobRecord) -> Option<RecoveredSettlement> {
	recovered_workpool_settlement(dom, record).or_else(|| recovered_tool_settlement(dom, record))
}

fn recovered_workpool_settlement(dom: &Dom, record: &JobRecord) -> Option<RecoveredSettlement> {
	if record.kind != JobKind::Tool || !is_live_status(record.status.as_str()) {
		return None;
	}
	let node = dom.get(record.handle)?;
	let Value::Json(raw) = node.prop(&PropKey::Custom(Str::new_static(WORKPOOL_STATE)))? else {
		return None;
	};
	let state = serde_json::from_str::<serde_json::Value>(raw.get()).ok()?;
	if state.get("closed").and_then(serde_json::Value::as_bool) != Some(true) {
		return None;
	}
	let summary = state.get("summary").and_then(serde_json::Value::as_str)?;
	let status = state
		.get("terminal_status")
		.and_then(serde_json::Value::as_str)
		.filter(|status| matches!(*status, "completed" | "cancelled" | "failed"))
		.map_or_else(|| Str::new_static("completed"), Str::new);
	let is_error = status != "completed";
	let error = match status.as_str() {
		"cancelled" => Some(Str::new_static("workpool was cancelled")),
		"failed" => Some(Str::new_static("workpool failed before settlement delivery")),
		_ => None,
	};
	let output = serde_json::value::to_raw_value(&serde_json::json!({ "text": summary })).ok()?;
	Some(RecoveredSettlement {
		is_error,
		status: Some(status),
		artifact: None,
		output: Some(output),
		error,
	})
}

fn restart_orphan(record: &JobRecord, recovered: Option<&RecoveredSettlement>) -> bool {
	is_live_status(record.status.as_str())
		&& match record.kind {
			JobKind::Tool => recovered.is_none(),
			JobKind::Subagent => true,
			// The environment host owns durable process generations and may
			// reconnect them independently of this disposable board.
			JobKind::Process => false,
			// An execution job without a live run is settled from its own journal by
			// `orphaned_execution`; the board never starts or resumes a backend run.
			JobKind::Execution => recovered.is_none(),
		}
}

/// Settlement for an execution job that restarted without its run.
///
/// The journal is the only evidence. A stored handle means the backend accepted
/// the work, so its effects are unknown. The submission marker without a handle
/// means `start` may have reached the backend. Neither means `start` never ran.
/// No branch contacts a backend or starts work.
fn orphaned_execution(dom: &Dom, handle: Handle) -> ExecutionSettlement {
	use omp_session::components::jobs::{EXECUTION_HANDLE, EXECUTION_SUBMISSION_STARTED};

	let node = dom.get(handle);
	let prop =
		|key: &'static str| node.and_then(|node| node.prop(&PropKey::Custom(Str::new_static(key))));
	let (state, accepted, error) = match prop(EXECUTION_HANDLE) {
		Some(stored) => {
			match serde_json::from_str::<ReattachHandle>(stored.as_str().unwrap_or_default()) {
				Ok(decoded) => (
					ExecutionState::EffectsUnknown,
					Some(decoded.clone()),
					ExecutionError::EffectsUnknown { handle: decoded },
				),
				// A handle was stored, so the work was accepted: never "not started".
				Err(source) => (ExecutionState::EffectsUnknown, None, ExecutionError::InvalidHandle {
					source: Arc::new(source),
				}),
			}
		},
		None if prop(EXECUTION_SUBMISSION_STARTED).is_some() => {
			(ExecutionState::SubmissionUnknown, None, ExecutionError::SubmissionUnknown)
		},
		None => (ExecutionState::NotSubmitted, None, ExecutionError::NotSubmitted),
	};
	ExecutionSettlement { state, handle: accepted, result: None, error: Some(error) }
}

fn recovered_tool_settlement(dom: &Dom, record: &JobRecord) -> Option<RecoveredSettlement> {
	if record.kind != JobKind::Tool || !is_live_status(record.status.as_str()) {
		return None;
	}
	let call = record.call?;
	let detached_at = record.detached_at?;
	let handle = find_call(dom, dom.body(), call)?;
	let node = dom.get(handle)?;
	let latest = custom(node, "result_entry")?.parse::<EntryId>().ok()?;
	if latest == detached_at {
		return None;
	}
	let is_error = match prop(node, PropId::Status)? {
		"ok" => false,
		"error" => true,
		_ => return None,
	};
	let artifact = call_artifact(dom, handle);
	let error = is_error.then(|| {
		dom.children(handle)
			.iter()
			.rev()
			.filter_map(|handle| dom.get(*handle))
			.find(|child| {
				child.tag == Tag::Known(KnownTag::Diag)
					&& prop(child, PropId::Severity) == Some("error")
			})
			.and_then(|child| prop(child, PropId::Text))
			.map_or_else(|| Str::new_static("detached tool failed"), Str::new)
	});
	Some(RecoveredSettlement { is_error, status: None, artifact, output: None, error })
}

fn find_call(dom: &Dom, parent: Handle, call: EntryId) -> Option<Handle> {
	let call = call.to_string();
	find_call_by_cause(dom, parent, &call)
}

fn find_call_by_cause(dom: &Dom, parent: Handle, call: &str) -> Option<Handle> {
	for handle in dom.children(parent) {
		let node = dom.get(*handle)?;
		if matches!(node.tag, Tag::Custom(_)) && prop(node, PropId::Cause) == Some(call) {
			return Some(*handle);
		}
		if let Some(found) = find_call_by_cause(dom, *handle, call) {
			return Some(found);
		}
	}
	None
}

fn call_artifact(dom: &Dom, call: Handle) -> Option<BlobRef> {
	let node = dom.get(call)?;
	if let (Some(address), Some(size)) =
		(prop(node, PropId::Blob), custom_int(node, "size").and_then(|size| u64::try_from(size).ok()))
		&& let Some(hash) = address.strip_prefix("artifact://sha256/")
	{
		return BlobRef::parse_hex(hash, size).ok();
	}
	let details = dom.children(call).iter().rev().find_map(|handle| {
		let child = dom.get(*handle)?;
		match child.tag {
			Tag::Known(KnownTag::Result) => match child.prop(&PropKey::from(PropId::Outcome)) {
				Some(Value::Json(raw)) => Some(raw),
				_ => None,
			},
			Tag::Known(KnownTag::Diag) => match child.prop(&PropKey::from(PropId::Fault)) {
				Some(Value::Json(raw)) => Some(raw),
				_ => None,
			},
			_ => None,
		}
	})?;
	let CallOutcomeDetails::Spilled { blob, byte_len } =
		serde_json::from_str::<CallOutcomeDetails>(details.get()).ok()?
	else {
		return None;
	};
	BlobRef::parse_hex(blob.hash.as_str(), byte_len).ok()
}

/// Inline shape of a settlement output that exceeded the board's bound: the
/// complete JSON is in the CAS at `artifact`, `text` keeps a bounded head of
/// the child's final text for actors and the async-result notice.
#[derive(Debug, Deserialize, Serialize)]
pub struct SpilledOutput {
	/// `artifact://sha256/<hex>` of the full settlement JSON.
	pub artifact: Str,
	/// Size of the full settlement JSON.
	pub byte_len: u64,
	/// Bounded head of the output's `text`, when it carried one.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub text:     Option<Str>,
}

/// Bounds one settlement output for the DOM: inline when it fits, else the
/// full JSON is spilled and a [`SpilledOutput`] stands in for it.
fn bounded_output(
	session: &Session,
	output: Box<RawValue>,
	bound: usize,
) -> Result<Box<RawValue>, omp_session::SessionError> {
	let raw = output.get();
	if raw.len() <= bound {
		return Ok(output);
	}
	let blob = session.blobs().put(raw.as_bytes())?;
	let text = serde_json::from_str::<serde_json::Value>(raw)
		.ok()
		.and_then(|value| {
			value
				.get("text")
				.and_then(serde_json::Value::as_str)
				.map(|text| Str::new(crate::dispatch::utf8_prefix(text, bound / 4)))
		});
	Ok(serde_json::value::to_raw_value(&SpilledOutput {
		artifact: Str::new(format!("artifact://sha256/{}", blob.to_hex())),
		byte_len: u64::try_from(raw.len()).unwrap_or(u64::MAX),
		text,
	})?)
}

/// Resolves a job's settlement output, reading a spilled one back from the
/// session blob store.
pub fn resolve_output(
	session: &Session,
	output: &RawValue,
) -> Result<Option<Box<RawValue>>, omp_session::SessionError> {
	let Ok(spilled) = serde_json::from_str::<SpilledOutput>(output.get()) else {
		return Ok(Some(RawValue::from_string(output.get().to_owned())?));
	};
	let Some(hex) = spilled.artifact.as_str().strip_prefix("artifact://sha256/") else {
		return Ok(None);
	};
	let reference = omp_journal::blob::BlobRef::parse_hex(hex, spilled.byte_len)?;
	let bytes = session.blobs().get(&reference)?;
	let json = std::str::from_utf8(&bytes)
		.map_err(|source| omp_session::SessionError::JobOutputUtf8 { source })?;
	Ok(Some(RawValue::from_string(json.to_owned())?))
}

/// Reads a job's `execution_result` back as the typed record, following a
/// spilled stand-in into the session blob store.
pub fn resolve_execution_result(
	session: &Session,
	output: &RawValue,
) -> Result<Option<ExecutionResult>, omp_session::SessionError> {
	let Some(raw) = resolve_output(session, output)? else {
		return Ok(None);
	};
	Ok(Some(serde_json::from_str::<DurableExecutionResult>(raw.get())?.into()))
}

fn commit_settlement(
	session: &mut Session,
	handle: Handle,
	settlement: JobSettlement,
	bound: usize,
) -> Result<(), omp_session::SessionError> {
	let cause = session
		.head()
		.ok_or(omp_session::SessionError::NoActiveTurn)?;
	let settled_at_ms = now_ms();
	let duration_ms = settlement.completion.as_ref().map_or_else(
		|| {
			record(session.dom(), handle)
				.map_or(0, |record| elapsed_ms(record.started.as_deref(), settled_at_ms))
		},
		|completion| completion.duration_ms,
	);
	let mut ops = vec![
		Op::Set {
			h:     handle,
			prop:  PropId::Status.into(),
			value: Value::Str(settlement.status.clone()),
		},
		Op::Set {
			h:     handle,
			prop:  PropId::DurationMs.into(),
			value: Value::Int(i64::try_from(duration_ms).unwrap_or(i64::MAX)),
		},
	];
	if let Some(output) = settlement.output {
		let output = bounded_output(session, output, bound)?;
		ops.push(Op::Set { h: handle, prop: PropId::Data.into(), value: Value::Json(output) });
	}
	if let Some(error) = settlement.error {
		ops.push(Op::Set {
			h:     handle,
			prop:  PropKey::Custom(Str::new_static("error")),
			value: Value::Str(error),
		});
	}
	if let Some(execution) = settlement.execution {
		let state: &'static str = execution.state.into();
		ops.push(Op::Set {
			h:     handle,
			prop:  PropKey::Custom(Str::new_static("execution_state")),
			value: Value::Str(Str::new_static(state)),
		});
		if let Some(accepted) = execution.handle {
			ops.extend(omp_session::components::jobs::execution_handle_ops(handle, &accepted)?);
		}
		if let Some(result) = execution.result {
			let result = bounded_output(
				session,
				serde_json::value::to_raw_value(&DurableExecutionResult::from(&result))?,
				bound,
			)?;
			ops.push(Op::Set {
				h:     handle,
				prop:  PropKey::Custom(Str::new_static("execution_result")),
				value: Value::Json(result),
			});
		}
		if let Some(error) = execution.error {
			let kind: &'static str = ExecutionErrorKind::from(&error).into();
			ops.push(Op::Set {
				h:     handle,
				prop:  PropKey::Custom(Str::new_static("execution_failure")),
				value: Value::Str(Str::new_static(kind)),
			});
		}
	}

	if let Some(completion) = settlement.completion {
		ops.extend(crate::launch_completion_ops(session, handle, &completion)?);
	}
	session.patch(Txn { cause, label: Some(Str::new_static("jobs.settle")), ops })?;
	Ok(())
}

/// Whether a job or subagent owned by this session is still running: its
/// settlement will re-wake the loop with an async-result follow-up, so a
/// candidate yield is a scheduling pause.
#[must_use]
pub fn pending_wake(dom: &Dom) -> bool {
	records(dom)
		.iter()
		.any(|record| is_live_status(record.status.as_str()))
}

/// Settled jobs whose result has not yet been delivered to the model
/// (no `delivered` prop on the element), oldest first.
#[must_use]
pub fn undelivered(dom: &Dom) -> Vec<JobRecord> {
	let mut out = records(dom)
		.into_iter()
		.filter(|record| {
			!is_live_status(record.status.as_str())
				&& !dom
					.get(record.handle)
					.and_then(|node| node.prop(&PropKey::Custom(Str::new_static(DELIVERED))))
					.is_some_and(|value| matches!(value, Value::Bool(true)))
		})
		.collect::<Vec<_>>();
	out.sort_by(|left, right| {
		started_ms(left)
			.cmp(&started_ms(right))
			.then(left.id.cmp(&right.id))
	});
	out
}

/// Prop set on a settled job once its result reached the model.
pub const DELIVERED: &str = "delivered";

fn now_ms() -> u64 {
	SystemTime::now()
		.duration_since(UNIX_EPOCH)
		.map_or(0, |elapsed| u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX))
}

fn elapsed_ms(started: Option<&str>, settled_at_ms: u64) -> u64 {
	started
		.and_then(|started| started.parse::<u64>().ok())
		.map_or(0, |started_at_ms| settled_at_ms.saturating_sub(started_at_ms))
}

fn started_ms(record: &JobRecord) -> Option<u64> {
	record
		.started
		.as_deref()
		.and_then(|started| started.parse().ok())
}

fn is_live_status(status: &str) -> bool {
	matches!(status, "running" | "starting")
}

fn records(dom: &Dom) -> Vec<JobRecord> {
	let Some(jobs) = dom.children(dom.meta()).iter().copied().find(|handle| {
		dom.get(*handle)
			.is_some_and(|node| node.tag == Tag::Known(KnownTag::Jobs))
	}) else {
		return Vec::new();
	};
	let mut out = Vec::new();
	collect(dom, jobs, &mut out);
	out
}

fn collect(dom: &Dom, parent: Handle, out: &mut Vec<JobRecord>) {
	for handle in dom.children(parent) {
		if let Some(record) = record(dom, *handle) {
			out.push(record);
		}
		collect(dom, *handle, out);
	}
}

fn record(dom: &Dom, handle: Handle) -> Option<JobRecord> {
	let node = dom.get(handle)?;
	let kind = match node.tag {
		Tag::Known(KnownTag::Job) => prop(node, PropId::Kind)
			.and_then(|value| value.parse().ok())
			.unwrap_or(JobKind::Tool),
		Tag::Known(KnownTag::Subagent) => JobKind::Subagent,
		_ => return None,
	};
	Some(JobRecord {
		handle,
		id: prop(node, PropId::Id).map_or_else(|| Str::new(handle.to_string()), Str::new),
		kind,
		status: prop(node, PropId::Status).map_or_else(|| Str::new_static("running"), Str::new),
		job_type: prop(node, PropId::Name)
			.filter(|value| !value.is_empty())
			.map_or_else(|| fallback_job_type(kind), Str::new),
		label: prop(node, PropId::Label)
			.filter(|value| !value.is_empty())
			.map_or_else(
				|| prop(node, PropId::Id).map_or_else(|| Str::new(handle.to_string()), Str::new),
				Str::new,
			),
		owner: custom(node, "owner").map(Str::new),
		factory_lease: factory_lease(node),
		started: custom(node, "started").map(Str::new),
		duration_ms: prop_int(node, PropId::DurationMs).and_then(|value| u64::try_from(value).ok()),
		output: node
			.prop(&PropKey::from(PropId::Data))
			.and_then(|value| match value {
				Value::Json(raw) => RawValue::from_string(raw.get().to_owned()).ok(),
				_ => None,
			}),
		error: custom(node, "error").map(Str::new),
		call: custom(node, "call").and_then(|value| value.parse().ok()),
		detached_at: prop(node, PropId::Cause).and_then(|value| value.parse().ok()),
		backend: custom(node, "backend").map(Str::new),
		reattach_handle: custom(node, "reattach_handle").map(Str::new),
		request: custom(node, "request").map(Str::new),
		output_mode: custom(node, omp_session::components::jobs::EXECUTION_OUTPUT_MODE).map(Str::new),
		execution_state: custom(node, "execution_state").and_then(|value| value.parse().ok()),
		execution_result: node
			.prop(&PropKey::Custom(Str::new_static("execution_result")))
			.and_then(|value| match value {
				Value::Json(raw) => RawValue::from_string(raw.get().to_owned()).ok(),
				_ => None,
			}),
		execution_failure: custom(node, "execution_failure").and_then(|value| value.parse().ok()),

		fallback: custom(node, "fallback").map(Str::new),
	})
}

fn prop(node: &omp_dom::Node, id: PropId) -> Option<&str> {
	node.prop(&PropKey::from(id)).and_then(Value::as_str)
}

fn custom<'a>(node: &'a omp_dom::Node, key: &'static str) -> Option<&'a str> {
	node
		.prop(&PropKey::Custom(Str::new_static(key)))
		.and_then(Value::as_str)
}

fn prop_int(node: &omp_dom::Node, id: PropId) -> Option<i64> {
	match node.prop(&PropKey::from(id)) {
		Some(Value::Int(value)) => Some(*value),
		_ => None,
	}
}

fn custom_int(node: &omp_dom::Node, key: &'static str) -> Option<i64> {
	match node.prop(&PropKey::Custom(Str::new_static(key))) {
		Some(Value::Int(value)) => Some(*value),
		_ => None,
	}
}

fn fallback_job_type(kind: JobKind) -> Str {
	let job_type: &'static str = DeliveryJobType::from(kind).into();
	Str::new_static(job_type)
}

#[cfg(test)]
mod tests {
	use std::{
		collections::VecDeque,
		future::{Future, ready},
	};

	use omp_core::Str;
	use omp_exec::{
		BackendId, CancelOutcome, ExecutionError, ExecutionEvent, ExecutionId, ExecutionRun,
		ExecutionState, ReattachHandle,
	};
	use tokio_util::sync::CancellationToken;

	use super::supervise_execution;

	struct TestRun {
		events: VecDeque<Option<ExecutionEvent>>,
	}

	impl ExecutionRun for TestRun {
		fn next_event(&mut self) -> impl Future<Output = Option<ExecutionEvent>> + Send + '_ {
			ready(self.events.pop_front().flatten())
		}

		fn cancel(&self) -> impl Future<Output = CancelOutcome> + Send + '_ {
			ready(CancelOutcome::Requested)
		}

		fn detach(self) -> ReattachHandle {
			ReattachHandle::new(
				BackendId::new_static("test"),
				Str::new_static("operation"),
				ExecutionId::new_static("execution"),
			)
		}
	}

	#[tokio::test]
	async fn accepted_run_without_terminal_event_is_effects_unknown() {
		let handle = ReattachHandle::new(
			BackendId::new_static("test"),
			Str::new_static("operation"),
			ExecutionId::new_static("execution"),
		);
		let run = TestRun {
			events: VecDeque::from([Some(ExecutionEvent::Accepted { handle: handle.clone() }), None]),
		};
		let settlement = supervise_execution(run, CancellationToken::new(), None, None).await;
		let execution = settlement.execution.expect("execution settlement");
		assert_eq!(execution.state, ExecutionState::EffectsUnknown);
		assert_eq!(execution.handle, Some(handle.clone()));
		assert!(
			matches!(execution.error, Some(ExecutionError::EffectsUnknown { handle: actual }) if actual == handle)
		);
	}

	#[tokio::test]
	async fn seeded_execution_without_terminal_event_is_effects_unknown() {
		let handle = ReattachHandle::new(
			BackendId::new_static("test"),
			Str::new_static("operation"),
			ExecutionId::new_static("execution"),
		);
		let run = TestRun { events: VecDeque::from([None]) };
		let settlement =
			supervise_execution(run, CancellationToken::new(), None, Some(handle.clone())).await;
		let execution = settlement.execution.expect("execution settlement");
		assert_eq!(execution.state, ExecutionState::EffectsUnknown);
		assert_eq!(execution.handle, Some(handle.clone()));
		assert!(
			matches!(execution.error, Some(ExecutionError::EffectsUnknown { handle: actual }) if actual == handle)
		);
	}

	#[tokio::test]
	async fn accepted_typed_backend_failure_stays_settled() {
		let handle = ReattachHandle::new(
			BackendId::new_static("test"),
			Str::new_static("operation"),
			ExecutionId::new_static("execution"),
		);
		let error = ExecutionError::ResumeUnsupported { backend: BackendId::new_static("test") };
		let run = TestRun { events: VecDeque::from([Some(ExecutionEvent::Settled(Err(error)))]) };
		let settlement =
			supervise_execution(run, CancellationToken::new(), None, Some(handle.clone())).await;
		let execution = settlement.execution.expect("execution settlement");
		assert_eq!(execution.state, ExecutionState::Settled);
		assert_eq!(execution.handle, Some(handle));
		assert!(matches!(execution.error, Some(ExecutionError::ResumeUnsupported { .. })));
	}

	#[tokio::test]
	async fn outcomes_before_acceptance_are_submission_unknown() {
		let lost = TestRun { events: VecDeque::from([None]) };
		let failed = TestRun {
			events: VecDeque::from([Some(ExecutionEvent::Settled(Err(ExecutionError::Capacity {
				backend: BackendId::new_static("test"),
			})))]),
		};
		for (run, kind) in [
			(lost, omp_exec::ExecutionErrorKind::SubmissionUnknown),
			(failed, omp_exec::ExecutionErrorKind::Capacity),
		] {
			let settlement = supervise_execution(run, CancellationToken::new(), None, None).await;
			assert_eq!(settlement.status, "failed");
			let execution = settlement.execution.expect("execution settlement");
			assert_eq!(execution.state, ExecutionState::SubmissionUnknown);
			assert_eq!(execution.handle, None);
			assert_eq!(
				execution
					.error
					.as_ref()
					.map(omp_exec::ExecutionErrorKind::from),
				Some(kind)
			);
		}
	}

	#[cfg(target_os = "linux")]
	#[tokio::test]
	async fn failed_acceptance_commit_retains_private_state_until_retry() {
		const CHILD: &str = "OMP_EXECUTION_T3_UNIT_CHILD";
		if std::env::var_os(CHILD).is_none() {
			let output = std::process::Command::new("sh")
				.args(["-c", "trap '' XFSZ; exec \"$@\"", "sh"])
				.arg(std::env::current_exe().expect("test executable"))
				.args([
					"--exact",
					"jobs::tests::failed_acceptance_commit_retains_private_state_until_retry",
					"--nocapture",
				])
				.env(CHILD, "1")
				.output()
				.expect("run isolated append-failure test");
			assert!(
				output.status.success(),
				"child failed: {}\n{}",
				String::from_utf8_lossy(&output.stdout),
				String::from_utf8_lossy(&output.stderr)
			);
			let stdout = String::from_utf8_lossy(&output.stdout);
			assert!(
				stdout.contains("running 1 test") && stdout.contains("1 passed"),
				"child did not execute test: {stdout}"
			);
			return;
		}
		use omp_session::{
			ComponentRegistry, Session,
			components::jobs::{self, ExecutionSpec},
		};
		let temp = tempfile::tempdir().expect("temporary session directory");
		let path = temp.path().join("accept.oms");
		let mut session =
			Session::create(&path, ComponentRegistry::standard()).expect("create session");
		let before = session.head().expect("genesis head");
		let txn = jobs::insert_execution(session.dom(), before, ExecutionSpec {
			id:          Str::new_static("execution"),
			owner:       Str::new_static("Main"),
			started:     Str::new_static("1"),
			backend:     Str::new_static("test"),
			request:     Str::new_static("cargo test"),
			output_mode: Str::new_static(r#""logs""#),
			destination: Str::new_static("test"),
			deadline:    Str::new_static("2"),
			approval:    Str::new_static("approval-1"),
		})
		.expect("jobs root");
		session.patch(txn).expect("insert execution");
		let handle = session
			.dom()
			.select("jobs job[id=execution]")
			.expect("selector")
			.into_iter()
			.next()
			.expect("execution node");
		let cause = session.head().expect("insert head");
		session
			.patch(jobs::mark_execution_submitting(cause, handle))
			.expect("mark submission");
		let inserted = session.head().expect("marker head");
		let accepted = ReattachHandle::new(
			BackendId::new_static("test"),
			Str::new_static("operation"),
			ExecutionId::new_static("execution"),
		);
		let (sender, receiver) = flume::bounded(1);
		let task = tokio::spawn(std::future::pending::<super::JobSettlement>());
		let board = super::JobBoard::new();
		assert!(board.attach_execution_task_with_acceptance(
			session.dom(),
			handle,
			CancellationToken::new(),
			task,
			Some(receiver)
		));
		sender.send(accepted.clone()).expect("send acceptance");
		let journal_size = std::fs::metadata(&path).expect("journal metadata").len();
		let limit = format!("--fsize={journal_size}:unlimited");
		assert!(
			std::process::Command::new("prlimit")
				.args(["--pid", &std::process::id().to_string(), &limit])
				.status()
				.expect("set file-size limit")
				.success()
		);
		let failure = board.poll(&mut session);
		assert!(
			std::process::Command::new("prlimit")
				.args(["--pid", &std::process::id().to_string(), "--fsize=unlimited:unlimited"])
				.status()
				.expect("restore file-size limit")
				.success()
		);
		assert!(matches!(
			failure,
			Err(omp_session::SessionError::Journal(omp_journal::JournalError::Io(_)))
		));
		assert_eq!(session.head(), Some(inserted));
		{
			let jobs = board.jobs.lock();
			let job = jobs.get(&handle).expect("runtime job retained");
			assert_eq!(job.accepted_handle.as_ref(), Some(&accepted));
			assert!(!job.accepted_committed);
		}
		assert!(board.has_finished_units());
		board.poll(&mut session).expect("retry acceptance");
		{
			let jobs = board.jobs.lock();
			let job = jobs.get(&handle).expect("runtime job retained");
			assert_eq!(job.accepted_handle.as_ref(), Some(&accepted));
			assert!(job.accepted_committed);
		}
		let records = board.list();
		assert_eq!(records[0].execution_state, Some(ExecutionState::Accepted));
		assert_eq!(
			records[0]
				.reattach_handle
				.as_deref()
				.map(serde_json::from_str::<ReattachHandle>)
				.transpose()
				.expect("decode handle"),
			Some(accepted)
		);
		board
			.terminate(&mut session, handle)
			.await
			.expect("stop test task");
	}

	#[test]
	fn a_subagent_lease_folds_or_stays_absent() {
		use omp_dom::{Dom, Handle, KnownTag, NodeSpec, Op, PropId, PropKey, Txn, Value};
		let mut dom = Dom::new();
		let jobs = dom.high_water() + 1;
		dom.apply(&Txn {
			cause: omp_journal::EntryId::default(),
			label: None,
			ops:   vec![
				Op::Ins { parent: dom.meta(), after: None, node: NodeSpec::new(KnownTag::Jobs) },
				Op::Ins {
					parent: Handle::new(jobs).expect("jobs handle"),
					after:  None,
					node:   NodeSpec::new(KnownTag::Subagent)
						.with_prop(PropId::Id, Value::Str(Str::new_static("child")))
						.with_prop(
							PropKey::Custom(Str::new_static("factory_lease")),
							Value::Str(Str::new_static("w1|2|50")),
						),
				},
				Op::Ins {
					parent: Handle::new(jobs).expect("jobs handle"),
					after:  None,
					node:   NodeSpec::new(KnownTag::Subagent)
						.with_prop(PropId::Id, Value::Str(Str::new_static("old"))),
				},
			],
		})
		.expect("the fixture inserts");
		let leased = super::record(&dom, Handle::new(jobs + 1).expect("leased"))
			.expect("record")
			.factory_lease
			.expect("lease");
		assert_eq!(leased.worker_id.as_str(), "w1");
		assert_eq!(leased.lease_gen, 2);
		assert_eq!(leased.deadline_ms, Some(50));
		assert!(super::accept_lease(Some(&leased), 2, 10));
		assert!(!super::accept_lease(Some(&leased), 1, 10), "a stale generation is refused");
		assert!(!super::accept_lease(Some(&leased), 2, 50), "an expired lease is refused");
		let old = super::record(&dom, Handle::new(jobs + 2).expect("old")).expect("old record");
		assert!(old.factory_lease.is_none());

		let leased_handle = Handle::new(jobs + 1).expect("leased");
		assert!(
			super::renew_lease(&dom, leased_handle, omp_journal::EntryId::default(), 1, 10, Some(100))
				.is_none(),
			"a stale generation writes nothing"
		);
		assert!(
			super::renew_lease(&dom, leased_handle, omp_journal::EntryId::default(), 2, 50, Some(100))
				.is_none(),
			"an expired lease writes nothing"
		);
		let renew =
			super::renew_lease(&dom, leased_handle, omp_journal::EntryId::default(), 2, 10, Some(100))
				.expect("the held generation renews");
		dom.apply(&renew).expect("the renewal applies");
		let renewed = super::record(&dom, leased_handle)
			.expect("record")
			.factory_lease
			.expect("renewed lease");
		assert_eq!(renewed.lease_gen, 3);
		assert_eq!(renewed.deadline_ms, Some(110));

		assert!(
			super::release_lease(&dom, leased_handle, omp_journal::EntryId::default(), 2).is_none(),
			"a stale release writes nothing"
		);
		let release = super::release_lease(&dom, leased_handle, omp_journal::EntryId::default(), 3)
			.expect("the held generation releases");
		dom.apply(&release).expect("the release applies");
		assert!(
			super::record(&dom, leased_handle)
				.expect("record")
				.factory_lease
				.is_none(),
			"a released item folds as unleased"
		);
	}
}
