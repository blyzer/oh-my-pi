//! Rewind-to-runtime lifecycle integration for the shared job primitive.

use std::sync::{
	Arc,
	atomic::{AtomicUsize, Ordering},
};

use omp_agent::{JobBoard, JobSettlement};
use omp_core::Str;
use omp_dom::{Op, PropKey, Txn, Value};
use omp_session::{
	ComponentRegistry, Session,
	components::jobs::{self, JobSpec},
};
use tempfile::tempdir;

#[tokio::test]
async fn jobs_rewind_removing_a_subagent_terminates_it() {
	let temp = tempdir().expect("temporary session directory");
	let path = temp.path().join("parent.oms");
	let mut session = Session::create(&path, ComponentRegistry::standard()).expect("create session");
	let before = session.head().expect("genesis head");
	let txn = jobs::insert(session.dom(), before, JobSpec {
		id:      Str::new_static("child-1"),
		kind:    Str::new_static("subagent"),
		owner:   Str::new_static("Main"),
		started: Str::new_static("1"),
		agent:   Some(Str::new_static("task")),
		lease:   None,
	})
	.expect("jobs root");
	session.patch(txn).expect("insert subagent");

	let handle = session
		.dom()
		.select("jobs subagent[id=child-1]")
		.expect("valid selector")
		.into_iter()
		.next()
		.expect("subagent element");
	let inserted = session.head().expect("insert head");
	let starts = Arc::new(AtomicUsize::new(0));
	let board = JobBoard::new();
	assert!(board.attach_restartable(session.dom(), handle, {
		let starts = Arc::clone(&starts);
		move |cancel| {
			starts.fetch_add(1, Ordering::SeqCst);
			tokio::spawn(async move {
				cancel.cancelled().await;
				JobSettlement {
					status:     Str::new_static("cancelled"),
					output:     None,
					error:      None,
					completion: None,
					execution:  None,
				}
			})
		}
	}));
	assert_eq!(starts.load(Ordering::SeqCst), 1);

	let work = session.rewind(before).expect("rewind before spawn");
	assert_eq!(work.terminate, vec![handle]);
	board.apply_lifecycle(&session, &work).await;
	assert!(board.list().is_empty());

	let work = session
		.rewind(inserted)
		.expect("rewind onto spawned branch");
	assert_eq!(work.spawn.len(), 1);
	board.apply_lifecycle(&session, &work).await;
	assert_eq!(starts.load(Ordering::SeqCst), 2);
	assert_eq!(board.list().len(), 1);
}

/// A `<job kind=tool>` re-derived without its execution unit (a forward
/// rewind over a detached call, or a restart) can never settle on its own:
/// the board journals it `failed` at the next poll instead of leaving
/// `hub wait` blocked on a phantom `running` job.
#[tokio::test]
async fn jobs_tool_job_without_an_execution_unit_settles_failed_at_poll() {
	let temp = tempdir().expect("temporary session directory");
	let path = temp.path().join("parent.oms");
	let mut session = Session::create(&path, ComponentRegistry::standard()).expect("create session");
	let head = session.head().expect("genesis head");
	let txn = jobs::insert(session.dom(), head, JobSpec {
		id:      Str::new_static("bash-timeout-1"),
		kind:    Str::new_static("tool"),
		owner:   Str::new_static("Main"),
		started: Str::new_static("1"),
		agent:   None,
		lease:   None,
	})
	.expect("jobs root");
	session.patch(txn).expect("insert detached tool job");

	let board = JobBoard::new();
	board.rebuild(&session);
	assert!(board.has_finished_units(), "an orphaned tool job wakes the settlement poll");
	let settled = board
		.wait(&mut session, Some(&[Str::new_static("bash-timeout-1")]))
		.await
		.expect("poll commits the orphan")
		.expect("the orphan settles rather than hanging the wait");
	assert_eq!(settled.status.as_str(), "failed");
	assert_eq!(settled.error.as_deref(), Some(omp_agent::ORPHANED_TOOL_JOB));
	assert!(!board.has_finished_units(), "the orphan is journaled exactly once");
}

/// A parent-process restart cannot retain the child execution unit. The stale
/// live node settles before explicit revival so neither `wait` nor revive is
/// permanently fenced by a phantom `running` status.
#[tokio::test]
async fn jobs_restart_settles_orphaned_subagent_and_replay_stays_revivable() {
	let temp = tempdir().expect("temporary session directory");
	let path = temp.path().join("subagent-restart.oms");
	let mut session = Session::create(&path, ComponentRegistry::standard()).expect("create session");
	let head = session.head().expect("genesis head");
	let txn = jobs::insert(session.dom(), head, JobSpec {
		id:      Str::new_static("child-restart"),
		kind:    Str::new_static("subagent"),
		owner:   Str::new_static("Main"),
		started: Str::new_static("1"),
		agent:   Some(Str::new_static("task")),
		lease:   None,
	})
	.expect("jobs root");
	session.patch(txn).expect("insert subagent");
	drop(session);

	let mut session = Session::open(&path, ComponentRegistry::standard()).expect("restart session");
	let board = JobBoard::new();
	board.rebuild(&session);
	assert!(board.has_finished_units(), "the missing child wakes settlement");
	let settled = board
		.wait(&mut session, Some(&[Str::new_static("child-restart")]))
		.await
		.expect("orphan settlement")
		.expect("subagent remains addressable");
	assert_eq!(settled.status.as_str(), "failed");
	assert_eq!(settled.error.as_deref(), Some(omp_agent::ORPHANED_SUBAGENT_JOB));
	drop(session);

	let replayed = Session::open(&path, ComponentRegistry::standard()).expect("replay settlement");
	let record = omp_agent::jobs::undelivered(replayed.dom())
		.into_iter()
		.find(|record| record.id == "child-restart")
		.expect("settled child remains deliverable");
	assert_eq!(record.status.as_str(), "failed");
	assert_eq!(record.error.as_deref(), Some(omp_agent::ORPHANED_SUBAGENT_JOB));
}

/// Explicit revival clears stale terminal presentation and re-arms exactly one
/// delivery for the new execution generation.
#[tokio::test]
async fn jobs_revive_rearms_settlement_delivery() {
	let temp = tempdir().expect("temporary session directory");
	let mut session =
		Session::create(temp.path().join("revive-delivery.oms"), ComponentRegistry::standard())
			.expect("create session");
	let head = session.head().expect("genesis head");
	session
		.patch(
			jobs::insert(session.dom(), head, JobSpec {
				id:      Str::new_static("child-revive"),
				kind:    Str::new_static("subagent"),
				owner:   Str::new_static("Main"),
				started: Str::new_static("1"),
				agent:   Some(Str::new_static("task")),
				lease:   None,
			})
			.expect("jobs root"),
		)
		.expect("insert child");
	let handle = session
		.dom()
		.select("jobs subagent[id=child-revive]")
		.expect("selector")
		.into_iter()
		.next()
		.expect("child handle");
	let delivered = session.head().expect("insert head");
	session
		.patch(Txn {
			cause: delivered,
			label: Some(Str::new_static("test.delivered")),
			ops:   vec![Op::Set {
				h:     handle,
				prop:  PropKey::Custom(Str::new_static(omp_agent::jobs::DELIVERED)),
				value: Value::Bool(true),
			}],
		})
		.expect("old delivery marker");
	let restart = session.head().expect("delivered head");
	session
		.patch(jobs::restart(restart, handle, Str::new_static("2")))
		.expect("restart generation");
	let board = JobBoard::new();
	assert!(board.attach_task(
		session.dom(),
		handle,
		tokio_util::sync::CancellationToken::new(),
		tokio::spawn(async {
			JobSettlement {
				status:     Str::new_static("completed"),
				output:     Some(
					serde_json::value::to_raw_value(&serde_json::json!({"text":"revived"}))
						.expect("output"),
				),
				error:      None,
				completion: None,
				execution:  None,
			}
		})
	));
	let settled = board
		.wait(&mut session, Some(&[Str::new_static("child-revive")]))
		.await
		.expect("wait")
		.expect("revived result");
	assert_eq!(settled.status, "completed");
	let pending = omp_agent::jobs::undelivered(session.dom());
	assert_eq!(pending.len(), 1);
	assert_eq!(pending[0].id, "child-revive");
	assert_eq!(
		pending[0]
			.output
			.as_deref()
			.map(serde_json::value::RawValue::get),
		Some(r#"{"text":"revived"}"#)
	);
}

/// Progress after detachment is not a terminal outcome. Restart reconciliation
/// keys off the durable result-entry marker rather than the call's general
/// order marker, so streamed updates cannot be mistaken for completion.
#[tokio::test]
async fn jobs_restart_does_not_adopt_progress_as_a_terminal() {
	let temp = tempdir().expect("temporary session directory");
	let path = temp.path().join("progress.oms");
	let mut session = Session::create(&path, ComponentRegistry::standard()).expect("create session");
	session.begin_turn().expect("turn");
	let call = session
		.call(
			"bash",
			1,
			"call-progress",
			None,
			Some(serde_json::value::to_raw_value(&serde_json::json!({})).expect("args")),
			None,
		)
		.expect("call");
	session
		.settle(
			call,
			serde_json::value::to_raw_value(&serde_json::json!({
				"kind": "detached",
				"id": "job-progress"
			}))
			.expect("detached outcome"),
		)
		.expect("detach");
	session
		.call_update(
			call,
			serde_json::value::to_raw_value(&serde_json::json!({"progress":"still running"}))
				.expect("update"),
		)
		.expect("progress");
	drop(session);

	let mut session = Session::open(&path, ComponentRegistry::standard()).expect("restart session");
	let board = JobBoard::new();
	board.rebuild(&session);
	let jobs = board.poll(&mut session).expect("orphan settlement");
	assert_eq!(jobs.len(), 1);
	assert_eq!(jobs[0].status.as_str(), "failed");
	assert_eq!(jobs[0].error.as_deref(), Some(omp_agent::ORPHANED_TOOL_JOB));
}

/// A supervised-process settlement journals its typed completion and delivery
/// marker in the same patch, so replay shows exactly one launch row and generic
/// async delivery cannot duplicate it after a crash.
#[tokio::test]
async fn process_settlement_journals_one_atomic_replayable_completion() {
	use omp_journal::data::{LaunchCompletion, LaunchDaemonCompletion, LaunchDaemonStatus, Patch};

	let temp = tempdir().expect("temporary session directory");
	let path = temp.path().join("process.oms");
	let mut session = Session::create(&path, ComponentRegistry::standard()).expect("create session");
	session.begin_turn().expect("turn");
	let head = session.head().expect("turn head");
	let txn = jobs::insert(session.dom(), head, JobSpec {
		id:      Str::new_static("web"),
		kind:    Str::new_static("process"),
		owner:   Str::new_static("Main"),
		started: Str::new_static("1"),
		agent:   None,
		lease:   None,
	})
	.expect("jobs root");
	session.patch(txn).expect("insert process job");
	let handle = session
		.dom()
		.select("jobs job[id=web]")
		.expect("valid selector")
		.into_iter()
		.next()
		.expect("process job");

	let board = JobBoard::new();
	assert!(board.attach_task(
		session.dom(),
		handle,
		tokio_util::sync::CancellationToken::new(),
		tokio::spawn(async move {
			JobSettlement {
				status:     Str::new_static("completed"),
				output:     None,
				error:      None,
				completion: Some(LaunchDaemonCompletion {
					name:        Str::new_static("web"),
					status:      LaunchDaemonStatus::Completed,
					exit_code:   Some(0),
					duration_ms: 2_500,
					fault:       None,
				}),
				execution:  None,
			}
		}),
	));
	board
		.wait(&mut session, Some(&[Str::new_static("web")]))
		.await
		.expect("poll")
		.expect("process settles");
	assert_eq!(
		session
			.dom()
			.get(handle)
			.and_then(|node| node.prop(&omp_dom::PropKey::Custom(Str::new_static("delivered")))),
		Some(&omp_dom::Value::Bool(true))
	);
	drop(session);

	let restored = Session::open(&path, ComponentRegistry::standard()).expect("completion replays");
	let rows = restored
		.dom()
		.select("body turn user[launch_completion=true]")
		.expect("valid selector")
		.collect::<Vec<_>>();
	assert_eq!(rows.len(), 1, "exactly one launch row replays");
	let data = restored
		.dom()
		.get(rows[0])
		.and_then(|node| node.prop(&omp_dom::PropId::Data.into()))
		.and_then(|value| match value {
			omp_dom::Value::Json(raw) => Some(raw),
			_ => None,
		})
		.expect("typed completion data");
	let completion: LaunchCompletion = serde_json::from_str(data.get()).expect("completion decodes");
	assert_eq!(completion.daemons[0].name, "web");
	assert_eq!(completion.daemons[0].duration_ms, 2_500);
	drop(restored);

	let (_, entries) = omp_journal::Journal::open(&path).expect("journal opens");
	let settlement = entries
		.iter()
		.filter(|entry| entry.label.as_deref() == Some("jobs.settle"))
		.collect::<Vec<_>>();
	assert_eq!(settlement.len(), 1, "settlement is one journal entry");
	let patch: Patch = serde_json::from_str(settlement[0].data.as_str()).expect("patch payload");
	assert!(patch.ops.get().contains("\"custom:launch_completion\""));
	assert!(patch.ops.get().contains("\"custom:delivered\""));
}

/// ADR 0009: a settlement larger than the central inline bound never lands
/// on the `<subagent>` element verbatim; the full JSON goes to the session
/// CAS and the element carries the artifact address plus a bounded head of
/// the child's text, which `resolve_output` reads back whole.
#[tokio::test]
async fn jobs_oversized_settlement_is_spilled_to_the_cas_and_resolvable() {
	let temp = tempdir().expect("temporary session directory");
	let path = temp.path().join("parent.oms");
	let mut session = Session::create(&path, ComponentRegistry::standard()).expect("create session");
	let head = session.head().expect("genesis head");
	let txn = jobs::insert(session.dom(), head, JobSpec {
		id:      Str::new_static("child-big"),
		kind:    Str::new_static("subagent"),
		owner:   Str::new_static("Main"),
		started: Str::new_static("1"),
		agent:   Some(Str::new_static("task")),
		lease:   None,
	})
	.expect("jobs root");
	session.patch(txn).expect("insert subagent");
	let handle = session
		.dom()
		.select("jobs subagent[id=child-big]")
		.expect("valid selector")
		.into_iter()
		.next()
		.expect("subagent element");

	let text = "x".repeat(4_096);
	let full = serde_json::json!({"id": "child-big", "text": text, "error": null});
	let board = JobBoard::new();
	board.set_output_bound(512);
	assert!(board.attach_task(
		session.dom(),
		handle,
		tokio_util::sync::CancellationToken::new(),
		tokio::spawn({
			let full = full.clone();
			async move {
				JobSettlement {
					status:     Str::new_static("completed"),
					output:     serde_json::value::to_raw_value(&full).ok(),
					error:      None,
					completion: None,
					execution:  None,
				}
			}
		}),
	));
	let settled = board
		.wait(&mut session, Some(&[Str::new_static("child-big")]))
		.await
		.expect("poll")
		.expect("settles");
	assert_eq!(settled.status.as_str(), "completed");
	let inline = settled.output.as_deref().expect("inline output");
	assert!(inline.get().len() <= 512, "the element carries a bounded stand-in");
	let spilled: omp_agent::SpilledOutput =
		serde_json::from_str(inline.get()).expect("spilled shape");
	assert!(spilled.artifact.starts_with("artifact://sha256/"));
	assert_eq!(spilled.byte_len, serde_json::to_string(&full).expect("json").len() as u64);
	assert_eq!(spilled.text.as_deref(), Some("x".repeat(128).as_str()));
	let resolved = omp_agent::resolve_output(&session, inline)
		.expect("blob read")
		.expect("addressable");
	assert_eq!(serde_json::from_str::<serde_json::Value>(resolved.get()).expect("json"), full);
}

/// A restart between the terminal `tool.result@1` and `jobs.settle` adopts the
/// call's complete-output artifact instead of failing the job as an orphan.
/// The artifact is copied from the runtime spill namespace into the session
/// namespace before the one durable settlement patch names it.
#[tokio::test]
async fn jobs_restart_adopts_terminal_artifact_and_settles_exactly_once() {
	let temp = tempdir().expect("temporary session directory");
	let path = temp.path().join("restart.oms");
	let runtime =
		omp_journal::blob::BlobStore::open(temp.path().join("runtime")).expect("runtime CAS");
	let mut session = Session::create(&path, ComponentRegistry::standard()).expect("create session");
	session.begin_turn().expect("turn");
	let call = session
		.call(
			"bash",
			1,
			"call-1",
			Some(Str::new_static("run a long command")),
			Some(serde_json::value::to_raw_value(&serde_json::json!({})).expect("args")),
			None,
		)
		.expect("call");
	session
		.settle(
			call,
			serde_json::value::to_raw_value(&serde_json::json!({
				"kind": "detached",
				"id": "job-1"
			}))
			.expect("detached outcome"),
		)
		.expect("detach");
	let full =
		serde_json::value::to_raw_value(&serde_json::json!({"kind":"ok","value":{"text":"done"}}))
			.expect("full outcome");
	let artifact = runtime
		.put(full.get().as_bytes())
		.expect("runtime artifact");
	session
		.settle_projected(
			call,
			serde_json::value::to_raw_value(&serde_json::json!({
				"storage": "spilled",
				"blob": {
					"hash": artifact.to_hex().as_str(),
					"media_type": "application/json",
					"byte_len": artifact.size
				},
				"byte_len": artifact.size
			}))
			.expect("spilled details"),
			serde_json::value::to_raw_value(&serde_json::json!([
				{"kind":"text","text":"artifact-backed completion"}
			]))
			.expect("parts"),
		)
		.expect("terminal result");
	drop(session);

	let mut session = Session::open(&path, ComponentRegistry::standard()).expect("restart session");
	assert!(
		!session.blobs().has(&artifact),
		"runtime and session stores are distinct before adoption"
	);
	let board = JobBoard::new();
	board.set_artifact_store(runtime);
	board.rebuild(&session);
	let jobs = board.poll(&mut session).expect("reconcile");
	assert_eq!(jobs.len(), 1);
	assert_eq!(jobs[0].status.as_str(), "completed");
	assert!(session.blobs().has(&artifact), "settlement pins the artifact in the session CAS");
	let output = jobs[0].output.as_deref().expect("artifact-backed output");
	let spilled: omp_agent::SpilledOutput =
		serde_json::from_str(output.get()).expect("spilled output");
	let address = format!("artifact://sha256/{}", artifact.to_hex());
	assert_eq!(spilled.artifact.as_str(), address.as_str());
	let resolved = omp_agent::resolve_output(&session, output)
		.expect("resolve")
		.expect("full output");
	assert_eq!(resolved.get(), full.get());

	board.poll(&mut session).expect("idempotent reconcile");
	drop(session);
	let (_, entries) = omp_journal::Journal::open(&path).expect("journal");
	assert_eq!(
		entries
			.iter()
			.filter(|entry| entry.label.as_deref() == Some("jobs.settle"))
			.count(),
		1,
		"the recovered terminal is journaled exactly once"
	);
}

/// Counts the rewinds and the session switches it is told about, and keeps
/// the head of the session each one handed it.
#[derive(Default)]
struct Transitions {
	rewinds:  AtomicUsize,
	switches: AtomicUsize,
	heads:    parking_lot::Mutex<Vec<Option<omp_journal::EntryId>>>,
}

impl Transitions {
	fn seen(&self) -> (usize, usize) {
		(self.rewinds.load(Ordering::SeqCst), self.switches.load(Ordering::SeqCst))
	}

	fn heads(&self) -> Vec<Option<omp_journal::EntryId>> {
		self.heads.lock().clone()
	}
}

impl omp_agent::SessionObserver for Transitions {
	fn rewound(&self, session: &Session) {
		self.rewinds.fetch_add(1, Ordering::SeqCst);
		self.heads.lock().push(session.head());
	}

	fn switched(&self, next: &Session) {
		self.switches.fetch_add(1, Ordering::SeqCst);
		self.heads.lock().push(next.head());
	}
}

/// Every rewind applied to the board tells each registered session observer
/// once, whatever the rewind changed, handing it the session already on the
/// branch the rewind left live; every session switch tells each one once as a
/// switch, handing it the next session.
#[tokio::test]
async fn every_rewind_and_switch_tells_the_session_observers() {
	let temp = tempdir().expect("temporary session directory");
	let mut session = Session::create(temp.path().join("rewind.oms"), ComponentRegistry::standard())
		.expect("create session");
	let genesis = session.head().expect("genesis head");
	let txn = jobs::insert(session.dom(), genesis, JobSpec {
		id:      Str::new_static("job-1"),
		kind:    Str::new_static("tool"),
		owner:   Str::new_static("Main"),
		started: Str::new_static("1"),
		agent:   None,
		lease:   None,
	})
	.expect("jobs root");
	session.patch(txn).expect("insert job");
	let board = JobBoard::new();
	let (first, second) = (Arc::new(Transitions::default()), Arc::new(Transitions::default()));
	board.observe_sessions(Arc::clone(&first) as Arc<dyn omp_agent::SessionObserver>);
	board.observe_sessions(Arc::clone(&second) as Arc<dyn omp_agent::SessionObserver>);

	let work = session.rewind(genesis).expect("rewind to genesis");
	board.apply_lifecycle(&session, &work).await;
	let empty = session
		.rewind(genesis)
		.expect("rewind that changes nothing");
	board.apply_lifecycle(&session, &empty).await;
	assert_eq!(first.seen(), (2, 0));
	assert_eq!(second.seen(), (2, 0));

	let next = Session::create(temp.path().join("next.oms"), ComponentRegistry::standard())
		.expect("create the next session");
	board.session_switched(&next);
	assert_eq!(first.seen(), (2, 1), "a switch is told as a switch, never as a rewind");
	assert_eq!(second.seen(), (2, 1));
	let told = [Some(genesis), Some(genesis), next.head()];
	assert_eq!(first.heads(), told, "each transition hands over the session now served");
	assert_eq!(second.heads(), told);
}

#[derive(Clone)]
struct ExecutionTestBackend {
	id:           omp_exec::BackendId,
	capabilities: omp_exec::BackendCapabilities,
	events:       flume::Receiver<omp_exec::ExecutionEvent>,
	counts:       Arc<ExecutionTestCounts>,
	stall_cancel: bool,
}

#[derive(Default)]
struct ExecutionTestCounts {
	cancels: AtomicUsize,
}

struct ExecutionTestRun {
	handle:       omp_exec::ReattachHandle,
	events:       flume::Receiver<omp_exec::ExecutionEvent>,
	counts:       Arc<ExecutionTestCounts>,
	stall_cancel: bool,
}

fn execution_test_handle() -> omp_exec::ReattachHandle {
	omp_exec::ReattachHandle::new(
		omp_exec::BackendId::new_static("fake@build.example:443/test"),
		Str::new_static("operations/42"),
		omp_exec::ExecutionId::new_static("exec-42"),
	)
}

fn execution_test_backend(
	stall_cancel: bool,
) -> (ExecutionTestBackend, flume::Sender<omp_exec::ExecutionEvent>) {
	let (sender, events) = flume::bounded(8);
	let capabilities = omp_exec::BackendCapabilities {
		platforms:      [omp_exec::Platform::LINUX_X86_64].into(),
		needs:          Default::default(),
		max_timeout:    None,
		action_cache:   omp_exec::CacheSupport::None,
		cancel:         omp_exec::CancelSupport::BestEffort,
		resume:         omp_exec::ResumeSupport::Reattach,
		max_blob_bytes: None,
		warm_caches:    false,
		enforced:       serde_json::from_str(
			r#"{"write":"read-only","network":"disabled","approval":"always-ask"}"#,
		)
		.expect("posture"),
	};
	(
		ExecutionTestBackend {
			id: omp_exec::BackendId::new_static("fake@build.example:443/test"),
			capabilities,
			events,
			counts: Arc::default(),
			stall_cancel,
		},
		sender,
	)
}

impl omp_exec::ExecutionBackend for ExecutionTestBackend {
	type Run = ExecutionTestRun;

	fn id(&self) -> &omp_exec::BackendId<str> {
		&self.id
	}

	fn capabilities(&self) -> &omp_exec::BackendCapabilities {
		&self.capabilities
	}

	async fn start(
		&self,
		request: omp_exec::ExecutionRequest,
	) -> Result<Self::Run, omp_exec::ExecutionError> {
		assert_eq!(request.id, omp_exec::ExecutionId::new_static("exec-42"));
		assert_eq!(request.outputs, omp_exec::OutputSpec::Logs);
		Ok(ExecutionTestRun {
			handle:       execution_test_handle(),
			events:       self.events.clone(),
			counts:       Arc::clone(&self.counts),
			stall_cancel: self.stall_cancel,
		})
	}

	async fn reattach(
		&self,
		_handle: omp_exec::ReattachHandle,
		_context: omp_exec::ReattachContext,
	) -> Result<Self::Run, omp_exec::ExecutionError> {
		Err(omp_exec::ExecutionError::ResumeUnsupported { backend: self.id.clone() })
	}
}

impl omp_exec::ExecutionRun for ExecutionTestRun {
	async fn next_event(&mut self) -> Option<omp_exec::ExecutionEvent> {
		self.events.recv_async().await.ok()
	}

	async fn cancel(&self) -> omp_exec::CancelOutcome {
		self.counts.cancels.fetch_add(1, Ordering::SeqCst);
		if self.stall_cancel {
			std::future::pending::<()>().await;
		}
		omp_exec::CancelOutcome::Requested
	}

	fn detach(self) -> omp_exec::ReattachHandle {
		self.handle
	}
}

fn execution_test_request() -> omp_exec::ExecutionRequest {
	omp_exec::ExecutionRequest {
		id:          omp_exec::ExecutionId::new_static("exec-42"),
		intent:      omp_exec::Intent::Test,
		argv:        [Str::new_static("cargo"), Str::new_static("test")].into(),
		cwd:         Str::new_static(""),
		env:         [].into(),
		inputs:      None,
		outputs:     omp_exec::OutputSpec::Logs,
		toolchain:   omp_exec::ToolchainId::new_static("test-toolchain"),
		caches:      [].into(),
		platform:    omp_exec::PlatformRequirement {
			os:            omp_exec::Os::Linux,
			arch:          omp_exec::Arch::X86_64,
			needs_network: false,
			needs:         Default::default(),
		},
		requirement: Default::default(),
		resources:   Default::default(),
		eligibility: omp_exec::Eligibility::RemoteRequired,
		timeout:     std::time::Duration::from_secs(60),
	}
}

fn insert_test_execution(session: &mut Session) -> omp_dom::Handle {
	let head = session.head().expect("genesis head");
	let txn = jobs::insert_execution(session.dom(), head, jobs::ExecutionSpec {
		id:          Str::new_static("exec-42"),
		owner:       Str::new_static("Main"),
		started:     Str::new_static("1"),
		backend:     Str::new_static("fake@build.example:443/test"),
		request:     Str::new_static(r#"{"recipe":"cargo test"}"#),
		output_mode: Str::new_static("logs"),
	})
	.expect("jobs root");
	session.patch(txn).expect("insert execution");
	session
		.dom()
		.select("jobs job[id=exec-42]")
		.expect("selector")
		.into_iter()
		.next()
		.expect("execution handle")
}

async fn attach_test_execution(
	session: &Session,
	board: &JobBoard,
	handle: omp_dom::Handle,
	backend: &ExecutionTestBackend,
) {
	use omp_exec::ExecutionBackend;
	let run = backend
		.start(execution_test_request())
		.await
		.expect("start execution");
	assert!(board.attach_execution_run(
		session.dom(),
		handle,
		tokio_util::sync::CancellationToken::new(),
		run
	));
}

async fn wait_for_test_acceptance(
	board: &JobBoard,
	sender: &flume::Sender<omp_exec::ExecutionEvent>,
) {
	sender
		.send(omp_exec::ExecutionEvent::Accepted { handle: execution_test_handle() })
		.expect("send acceptance");
	tokio::time::timeout(std::time::Duration::from_secs(5), board.any_finished())
		.await
		.expect("acceptance wakes board");
	assert!(board.has_finished_units());
	assert!(board.has_finished("exec-42"));
}

fn assert_test_identity(session: &Session, handle: omp_dom::Handle) {
	let node = session.dom().get(handle).expect("execution node");
	let custom = |key: &'static str| {
		node
			.prop(&PropKey::Custom(Str::new_static(key)))
			.and_then(Value::as_str)
	};
	assert_eq!(
		node
			.prop(&omp_dom::PropId::Id.into())
			.and_then(Value::as_str),
		Some("exec-42")
	);
	assert_eq!(
		node
			.prop(&omp_dom::PropId::Kind.into())
			.and_then(Value::as_str),
		Some("execution")
	);
	assert_eq!(
		node
			.prop(&omp_dom::PropId::Name.into())
			.and_then(Value::as_str),
		Some("execution")
	);
	assert_eq!(
		node
			.prop(&omp_dom::PropId::Label.into())
			.and_then(Value::as_str),
		Some("exec-42")
	);
	assert_eq!(custom("owner"), Some("Main"));
	assert_eq!(custom("started"), Some("1"));
	assert_eq!(custom(jobs::EXECUTION_BACKEND), Some("fake@build.example:443/test"));
	assert_eq!(custom(jobs::EXECUTION_REQUEST), Some(r#"{"recipe":"cargo test"}"#));
	assert_eq!(custom(jobs::EXECUTION_OUTPUT_MODE), Some("logs"));
}

fn assert_test_handle(record: &omp_agent::jobs::JobRecord) {
	assert_eq!(record.id, "exec-42");
	let stored = record.reattach_handle.as_deref().expect("reattach handle");
	assert_eq!(
		stored,
		r#"{"schema":"V1","backend":"fake@build.example:443/test","operation":"operations/42","id":"exec-42"}"#
	);
	assert_eq!(
		serde_json::from_str::<omp_exec::ReattachHandle>(stored).expect("decode handle"),
		execution_test_handle()
	);
}

#[tokio::test]
async fn execution_t2_acceptance_wakes_board_and_survives_replay() {
	let temp = tempdir().expect("temporary session directory");
	let path = temp.path().join("accepted.oms");
	let mut session = Session::create(&path, ComponentRegistry::standard()).expect("create session");
	let handle = insert_test_execution(&mut session);
	let board = JobBoard::new();
	let (backend, events) = execution_test_backend(false);
	attach_test_execution(&session, &board, handle, &backend).await;
	assert!(!board.has_finished_units());
	wait_for_test_acceptance(&board, &events).await;
	let records = board.poll(&mut session).expect("commit acceptance");
	assert_eq!(records.len(), 1);
	assert_eq!(records[0].status, "running");
	assert_eq!(records[0].execution_state, Some(omp_exec::ExecutionState::Accepted));
	assert_test_handle(&records[0]);
	assert!(!board.has_finished_units());
	drop(session);
	let mut session =
		Session::open(&path, ComponentRegistry::standard()).expect("reopen accepted session");
	board.rebuild(&session);
	let records = board.list();
	assert_eq!(records.len(), 1);
	assert_eq!(records[0].execution_state, Some(omp_exec::ExecutionState::Accepted));
	assert_eq!(records[0].owner.as_deref(), Some("Main"));
	assert_eq!(records[0].backend.as_deref(), Some("fake@build.example:443/test"));
	assert_eq!(records[0].request.as_deref(), Some(r#"{"recipe":"cargo test"}"#));
	assert_test_handle(&records[0]);
	assert_test_identity(&session, records[0].handle);
	board
		.terminate(&mut session, records[0].handle)
		.await
		.expect("stop test run");
}

#[tokio::test]
async fn execution_t6_backend_effects_unknown_is_not_settled() {
	let temp = tempdir().expect("temporary session directory");
	let path = temp.path().join("unknown.oms");
	let mut session = Session::create(&path, ComponentRegistry::standard()).expect("create session");
	let handle = insert_test_execution(&mut session);
	let board = JobBoard::new();
	let (backend, events) = execution_test_backend(false);
	attach_test_execution(&session, &board, handle, &backend).await;
	wait_for_test_acceptance(&board, &events).await;
	board.poll(&mut session).expect("commit acceptance");
	events
		.send(omp_exec::ExecutionEvent::Settled(Err(omp_exec::ExecutionError::EffectsUnknown {
			handle: execution_test_handle(),
		})))
		.expect("send observation loss");
	let record = tokio::time::timeout(
		std::time::Duration::from_secs(5),
		board.wait(&mut session, Some(&[Str::new_static("exec-42")])),
	)
	.await
	.expect("settlement deadline")
	.expect("settlement commit")
	.expect("settled job");
	assert_eq!(record.status, "failed");
	assert_eq!(record.execution_state, Some(omp_exec::ExecutionState::EffectsUnknown));
	assert_eq!(record.execution_failure, Some(omp_exec::ExecutionErrorKind::EffectsUnknown));
	assert_test_handle(&record);
	assert_test_identity(&session, handle);
	drop(session);
	let session = Session::open(&path, ComponentRegistry::standard()).expect("reopen settlement");
	board.rebuild(&session);
	let record = board.list().into_iter().next().expect("replayed job");
	assert_eq!(record.execution_state, Some(omp_exec::ExecutionState::EffectsUnknown));
	assert_eq!(record.execution_failure, Some(omp_exec::ExecutionErrorKind::EffectsUnknown));
	assert_test_handle(&record);
}

#[tokio::test]
async fn execution_t8_cancel_preserves_accepted_handle_before_and_after_poll_and_abort() {
	for (poll_first, stall_cancel) in [(true, false), (false, false), (false, true)] {
		let temp = tempdir().expect("temporary session directory");
		let path = temp.path().join("cancelled.oms");
		let mut session =
			Session::create(&path, ComponentRegistry::standard()).expect("create session");
		let handle = insert_test_execution(&mut session);
		let board = JobBoard::new();
		let (backend, events) = execution_test_backend(stall_cancel);
		attach_test_execution(&session, &board, handle, &backend).await;
		wait_for_test_acceptance(&board, &events).await;
		if poll_first {
			board.poll(&mut session).expect("commit acceptance");
		}
		assert!(
			tokio::time::timeout(
				std::time::Duration::from_secs(5),
				board.terminate(&mut session, handle)
			)
			.await
			.expect("termination deadline")
			.expect("commit cancellation")
		);
		assert_eq!(backend.counts.cancels.load(Ordering::SeqCst), 1);
		let record = board.list().into_iter().next().expect("cancelled job");
		assert_eq!(record.status, "cancelled");
		assert_eq!(record.execution_state, Some(omp_exec::ExecutionState::EffectsUnknown));
		assert_eq!(record.execution_failure, Some(omp_exec::ExecutionErrorKind::EffectsUnknown));
		assert_test_handle(&record);
		assert_test_identity(&session, handle);
		drop(session);
		let session =
			Session::open(&path, ComponentRegistry::standard()).expect("reopen cancellation");
		board.rebuild(&session);
		let record = board.list().into_iter().next().expect("replayed job");
		assert_eq!(record.execution_state, Some(omp_exec::ExecutionState::EffectsUnknown));
		assert_eq!(record.execution_failure, Some(omp_exec::ExecutionErrorKind::EffectsUnknown));
		assert_test_handle(&record);
	}
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn execution_t3_failed_acceptance_append_commits_once_on_retry() {
	const CHILD: &str = "OMP_EXECUTION_T3_CHILD";
	if std::env::var_os(CHILD).is_none() {
		let output = std::process::Command::new("sh")
			.args(["-c", "trap '' XFSZ; exec \"$@\"", "sh"])
			.arg(std::env::current_exe().expect("test executable"))
			.args([
				"--exact",
				"execution_t3_failed_acceptance_append_commits_once_on_retry",
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
			"child did not execute T3: {stdout}"
		);
		return;
	}
	let temp = tempdir().expect("temporary session directory");
	let path = temp.path().join("append-failure.oms");
	let mut session = Session::create(&path, ComponentRegistry::standard()).expect("create session");
	let handle = insert_test_execution(&mut session);
	let board = JobBoard::new();
	let (backend, events) = execution_test_backend(false);
	attach_test_execution(&session, &board, handle, &backend).await;
	wait_for_test_acceptance(&board, &events).await;
	let prior_head = session.head();
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
	assert!(
		matches!(failure, Err(omp_session::SessionError::Journal(omp_journal::JournalError::Io(_)))),
		"the journal append must fail"
	);
	assert_eq!(session.head(), prior_head);
	assert!(board.has_finished_units(), "uncommitted acceptance remains ready");
	assert_eq!(board.list()[0].execution_state, Some(omp_exec::ExecutionState::Requested));
	let node = session.dom().get(handle).expect("execution node");
	assert!(
		node
			.prop(&PropKey::Custom(Str::new_static(jobs::EXECUTION_HANDLE)))
			.is_none()
	);
	let records = board.poll(&mut session).expect("retry acceptance");
	assert_eq!(records[0].execution_state, Some(omp_exec::ExecutionState::Accepted));
	assert_test_handle(&records[0]);
	assert_test_identity(&session, handle);
	assert_eq!(
		session
			.entries()
			.filter(|entry| entry.label.as_deref() == Some("jobs.settle"))
			.count(),
		1
	);
	board.poll(&mut session).expect("no duplicate acceptance");
	assert_eq!(
		session
			.entries()
			.filter(|entry| entry.label.as_deref() == Some("jobs.settle"))
			.count(),
		1
	);
	board
		.terminate(&mut session, handle)
		.await
		.expect("stop test run");
}

#[tokio::test]
async fn execution_t8_preserves_completed_supervisor_settlement() {
	let temp = tempdir().expect("temporary session directory");
	let mut session =
		Session::create(temp.path().join("completed.oms"), ComponentRegistry::standard())
			.expect("create session");
	let handle = insert_test_execution(&mut session);
	let board = JobBoard::new();
	let cancel = tokio_util::sync::CancellationToken::new();
	let child = cancel.clone();
	let task = tokio::spawn(async move {
		child.cancelled().await;
		JobSettlement {
			status:     Str::new_static("failed"),
			output:     None,
			error:      Some(Str::new_static("terminal backend error")),
			completion: None,
			execution:  Some(omp_agent::jobs::ExecutionSettlement {
				state:  omp_exec::ExecutionState::Settled,
				handle: Some(execution_test_handle()),
				result: None,
				error:  Some(omp_exec::ExecutionError::ResumeUnsupported {
					backend: omp_exec::BackendId::new_static("fake@build.example:443/test"),
				}),
			}),
		}
	});
	assert!(board.attach_execution_task(session.dom(), handle, cancel, task));
	assert!(
		board
			.terminate(&mut session, handle)
			.await
			.expect("commit supervisor settlement")
	);
	let record = board.list().into_iter().next().expect("settled job");
	assert_eq!(record.status, "failed");
	assert_eq!(record.error.as_deref(), Some("terminal backend error"));
	assert_eq!(record.execution_state, Some(omp_exec::ExecutionState::Settled));
	assert_eq!(record.execution_failure, Some(omp_exec::ExecutionErrorKind::ResumeUnsupported));
	assert_test_handle(&record);
}

#[tokio::test]
async fn execution_terminate_keeps_a_settled_result() {
	let temp = tempdir().expect("temporary session directory");
	let mut session =
		Session::create(temp.path().join("settled.oms"), ComponentRegistry::standard())
			.expect("create session");
	let handle = insert_test_execution(&mut session);
	let board = JobBoard::new();
	let (backend, events) = execution_test_backend(false);
	attach_test_execution(&session, &board, handle, &backend).await;
	wait_for_test_acceptance(&board, &events).await;
	events
		.send(omp_exec::ExecutionEvent::Settled(Err(omp_exec::ExecutionError::ResumeUnsupported {
			backend: omp_exec::BackendId::new_static("fake@build.example:443/test"),
		})))
		.expect("send typed failure");
	tokio::time::timeout(
		std::time::Duration::from_secs(5),
		board.wait(&mut session, Some(&[Str::new_static("exec-42")])),
	)
	.await
	.expect("settlement deadline")
	.expect("settlement commit")
	.expect("settled job");
	assert!(
		!board
			.terminate(&mut session, handle)
			.await
			.expect("terminate settled job")
	);
	let record = board.list().into_iter().next().expect("settled job");
	assert_eq!(record.status, "failed");
	assert_eq!(record.error.as_deref(), Some("execution failed"));
	assert_eq!(record.execution_state, Some(omp_exec::ExecutionState::Settled));
	assert_eq!(record.execution_failure, Some(omp_exec::ExecutionErrorKind::ResumeUnsupported));
	assert_test_handle(&record);
	assert_eq!(
		session
			.entries()
			.filter(|entry| entry.label.as_deref() == Some("jobs.settle"))
			.count(),
		2,
		"acceptance and settlement only; terminate journals nothing"
	);
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn execution_failed_cancellation_append_retries_with_one_settlement() {
	const CHILD: &str = "OMP_EXECUTION_CANCEL_APPEND_CHILD";
	if std::env::var_os(CHILD).is_none() {
		let output = std::process::Command::new("sh")
			.args(["-c", "trap '' XFSZ; exec \"$@\"", "sh"])
			.arg(std::env::current_exe().expect("test executable"))
			.args([
				"--exact",
				"execution_failed_cancellation_append_retries_with_one_settlement",
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
			"child did not execute the cancellation test: {stdout}"
		);
		return;
	}
	let temp = tempdir().expect("temporary session directory");
	let path = temp.path().join("cancel-append-failure.oms");
	let mut session = Session::create(&path, ComponentRegistry::standard()).expect("create session");
	let handle = insert_test_execution(&mut session);
	let board = JobBoard::new();
	let (backend, events) = execution_test_backend(false);
	attach_test_execution(&session, &board, handle, &backend).await;
	wait_for_test_acceptance(&board, &events).await;
	let prior_head = session.head();
	let journal_size = std::fs::metadata(&path).expect("journal metadata").len();
	let limit = format!("--fsize={journal_size}:unlimited");
	assert!(
		std::process::Command::new("prlimit")
			.args(["--pid", &std::process::id().to_string(), &limit])
			.status()
			.expect("set file-size limit")
			.success()
	);
	let failure = board.terminate(&mut session, handle).await;
	assert!(
		std::process::Command::new("prlimit")
			.args(["--pid", &std::process::id().to_string(), "--fsize=unlimited:unlimited"])
			.status()
			.expect("restore file-size limit")
			.success()
	);
	assert!(
		matches!(failure, Err(omp_session::SessionError::Journal(omp_journal::JournalError::Io(_)))),
		"the journal append must fail"
	);
	assert_eq!(session.head(), prior_head);
	let node = session.dom().get(handle).expect("execution node");
	assert!(
		node
			.prop(&PropKey::Custom(Str::new_static(jobs::EXECUTION_HANDLE)))
			.is_none()
	);
	assert!(
		board
			.terminate(&mut session, handle)
			.await
			.expect("retry cancellation")
	);
	let record = board.list().into_iter().next().expect("cancelled job");
	assert_eq!(record.status, "cancelled");
	assert_eq!(record.error.as_deref(), Some("execution effects are unknown after cancellation"));
	assert_eq!(record.execution_state, Some(omp_exec::ExecutionState::EffectsUnknown));
	assert_eq!(record.execution_failure, Some(omp_exec::ExecutionErrorKind::EffectsUnknown));
	assert_test_handle(&record);
	assert_eq!(
		session
			.entries()
			.filter(|entry| entry.label.as_deref() == Some("jobs.settle"))
			.count(),
		1
	);
}
