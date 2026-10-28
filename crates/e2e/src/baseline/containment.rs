//! W8: the containment correctness gate.
//!
//! Attached, the proof kills the real daemon with SIGKILL while remote work is
//! in flight and holds the client to the ADR 0045 containment contract: every
//! remote call reaches a typed terminal error within the interrupt grace, no
//! call is left pending, the local host keeps serving the work that was in
//! flight on it, the daemon leaves no process behind, and the router ends
//! only because its client closed. Embedded, the equivalent is cancellation:
//! cancelling one command terminates it with a typed outcome within the
//! interrupt grace, spares an unrelated command, and leaves no child process.
//!
//! Unlike every other workload a failed check here fails the recording: a
//! wrong containment or cleanup result is not a slow sample.

use std::{
	collections::BTreeSet,
	path::Path,
	sync::{
		Arc,
		atomic::{AtomicU64, Ordering},
	},
	time::{Duration, Instant},
};

use nix::{
	sys::signal::{self, Signal},
	unistd::Pid,
};
use omp_core::EnvPath;
use omp_env::{ClientError, EnvClient, ExecEvent, ExecRun, frame};
use serde_json::{Value, json};
use tokio::time;

use super::{
	DaemonState, Transport,
	census::{Census, kill_all, quiesce},
	topology::{RouterExit, Staging, Target},
	workloads::{
		Plan, STEP_LIMIT, Sample, close_session, drain, echo, ensure_exited, ensure_read_fixture,
		exec_request, finish_call, open_session, read_target, read_whole, run_script, start_call,
	},
};
use crate::{
	Context as _, Result, error,
	support::{wait_process_group_dead, within},
};

/// A command that holds a session until the proof ends it.
const REMOTE_SLEEP: &str = "sleep 30";
/// An in-process command the embedded proof cancels: nothing but the
/// interpreter runs it.
const BUILTIN_SLEEP: &str = "sleep 30";
/// An external program the embedded proof cancels, so cancellation must stop
/// and reap a real child process.
const EXTERNAL_SLEEP: &str = "/bin/sleep 30";
/// The exec host's kill boundary for a command with a child process: TERM, a
/// 250 ms courtesy interval slept in full, KILL, then the reap that precedes
/// the exit event. Four courtesy intervals leave room for a loaded host.
const EXEC_KILL_BOUND: Duration = Duration::from_secs(1);
/// An unrelated command that must outlive the other commands' cancellation.
const SURVIVOR: &str = "sleep 0.6; echo survivor";
/// Time the local `p8_hold` call spends on the extension host, long enough to
/// still be running when the daemon dies.
const HOLD_MILLIS: u64 = 600;
/// Local calls that must succeed after the daemon died.
const LOCAL_CALLS_AFTER_KILL: u64 = 20;
const WARMUP_OPERATIONS: u64 = 20;
/// How long cleanup may take to show a process gone.
const SETTLE: Duration = Duration::from_secs(5);

fn interrupt_grace() -> Result<Duration> {
	omp_tool::DEFAULT_INTERRUPT_GRACE
		.to_std()
		.context("converting the interrupt grace")
}

/// Records `count` repetitions of the W8 gate.
pub(super) async fn repetitions(
	transport: Transport,
	state: DaemonState,
	plan: &Plan,
	count: usize,
) -> Result<Vec<Sample>> {
	let mut samples = Vec::with_capacity(count);
	match transport {
		Transport::Attached => {
			for _ in 0..count {
				quiesce().await;
				samples.push(daemon_kill(state, plan).await?);
			}
		},
		Transport::Embedded => match state {
			DaemonState::Cold => {
				for _ in 0..count {
					quiesce().await;
					let target = Staging::new(plan).await?.start(transport).await?;
					let sample = cancellation(&target).await;
					let closed = target.close().await;
					samples.push(sample?);
					closed?;
				}
			},
			DaemonState::Warm => {
				let target = Staging::new(plan).await?.start(transport).await?;
				let result: Result<()> = async {
					cancellation(&target).await?;
					for _ in 0..count {
						quiesce().await;
						samples.push(cancellation(&target).await?);
					}
					Ok(())
				}
				.await;
				let closed = target.close().await;
				result?;
				closed?;
			},
		},
	}
	Ok(samples)
}

async fn start_running(
	client: &EnvClient,
	session: &bytes::Bytes,
	script: &str,
) -> Result<ExecRun> {
	let mut run =
		within("command start", STEP_LIMIT, client.exec(exec_request(session, script))).await??;
	match within("command started event", STEP_LIMIT, run.next_event()).await?? {
		Some(ExecEvent::Started(_)) => Ok(run),
		other => Err(error(format!("expected {script:?} to start, got {other:?}"))),
	}
}

async fn children_beyond(recorder: u32, baseline: &BTreeSet<u32>) -> Result<BTreeSet<u32>> {
	let census = Census::take().await?;
	Ok(census
		.descendants(recorder)
		.difference(baseline)
		.copied()
		.collect())
}

/// Waits until the recorder has a child process it did not have at
/// `baseline`, so a leak check cannot pass for want of a child.
async fn await_children(recorder: u32, baseline: &BTreeSet<u32>) -> Result<BTreeSet<u32>> {
	within("a child process to appear", STEP_LIMIT, async {
		loop {
			let children = children_beyond(recorder, baseline).await?;
			if !children.is_empty() {
				return Ok::<_, crate::Error>(children);
			}
			time::sleep(Duration::from_millis(10)).await;
		}
	})
	.await?
}

/// Waits until every child the recorder gained since `baseline` is gone, and
/// kills and reports whatever outlives the bound.
async fn settle_children(recorder: u32, baseline: &BTreeSet<u32>, what: &str) -> Result<()> {
	let deadline = Instant::now() + SETTLE;
	loop {
		let census = Census::take().await?;
		let leaked: BTreeSet<u32> = census
			.descendants(recorder)
			.difference(baseline)
			.copied()
			.collect();
		if leaked.is_empty() {
			return Ok(());
		}
		if Instant::now() >= deadline {
			let description = census.describe(&leaked);
			kill_all(&leaked);
			return Err(error(format!("{what} left child processes running: {description}")));
		}
		time::sleep(Duration::from_millis(25)).await;
	}
}

fn processes_gone(pids: &BTreeSet<u32>) -> bool {
	pids.iter().all(|pid| {
		i32::try_from(*pid)
			.is_ok_and(|raw| signal::kill(Pid::from_raw(raw), None) == Err(nix::errno::Errno::ESRCH))
	})
}

/// Whether none of `pids` is a live process; a zombie awaiting its reap does
/// not count as live.
async fn children_gone(pids: &BTreeSet<u32>) -> Result<bool> {
	if processes_gone(pids) {
		return Ok(true);
	}
	Ok(Census::take().await?.alive(pids).is_empty())
}

async fn cancelled_terminal(run: &mut ExecRun, since: Instant, label: &str) -> Result<Duration> {
	let done = within("cancelled command terminal", STEP_LIMIT, drain(run, false, since)).await??;
	let after = since.elapsed();
	if done.status.outcome != frame::ExecOutcome::Cancelled as i32 {
		return Err(error(format!("{label} did not end as cancelled: {:?}", done.status)));
	}
	Ok(after)
}

async fn cancellation(target: &Target) -> Result<Sample> {
	let client = target.client();
	let project = target.project();
	let grace = interrupt_grace()?;
	let recorder = std::process::id();
	let baseline = Census::take().await?.descendants(recorder);

	let builtin_session = open_session(client, project).await?;
	let external_session = open_session(client, project).await?;
	let survivor_session = open_session(client, project).await?;
	let mut builtin = start_running(client, &builtin_session, BUILTIN_SLEEP).await?;
	let mut external = start_running(client, &external_session, EXTERNAL_SLEEP).await?;
	let mut survivor = start_running(client, &survivor_session, SURVIVOR).await?;
	let children = await_children(recorder, &baseline).await?;

	let cancelled = Instant::now();
	builtin.guard().cancel();
	external.guard().cancel();
	let builtin_terminal =
		cancelled_terminal(&mut builtin, cancelled, "the in-process command").await?;
	let external_terminal =
		cancelled_terminal(&mut external, cancelled, "the external command").await?;
	if builtin_terminal > grace {
		return Err(error(format!(
			"the cancelled in-process command was still pending {builtin_terminal:?} after \
			 cancellation, past the {grace:?} interrupt grace"
		)));
	}
	if external_terminal > EXEC_KILL_BOUND {
		return Err(error(format!(
			"the cancelled external command was still pending {external_terminal:?} after \
			 cancellation, past the {EXEC_KILL_BOUND:?} kill boundary"
		)));
	}
	if !children_gone(&children).await? {
		return Err(error(
			"the cancelled external command reported its end while its child process still lived",
		));
	}
	let survivor_done =
		within("surviving command exit", STEP_LIMIT, drain(&mut survivor, true, cancelled)).await??;
	let survivor_total = cancelled.elapsed();
	ensure_exited(&survivor_done.status, SURVIVOR)?;
	if survivor_done.output != b"survivor\n" {
		return Err(error(format!(
			"the unrelated command was disturbed by the cancellations: it printed {:?}",
			String::from_utf8_lossy(&survivor_done.output)
		)));
	}
	close_session(client, builtin_session).await?;
	close_session(client, external_session).await?;
	close_session(client, survivor_session).await?;
	settle_children(recorder, &baseline, "cancellation").await?;
	Ok(Sample {
		metrics: vec![
			("builtin_cancel_to_terminal_ns", builtin_terminal.as_nanos() as f64),
			("external_cancel_to_terminal_ns", external_terminal.as_nanos() as f64),
			("survivor_exit_ns", survivor_total.as_nanos() as f64),
			("external_children_count", children.len() as f64),
		],
		facts:   vec![("pending_after_grace", 0), ("leaked_children", 0), ("survivor_completed", 1)],
	})
}

async fn warm_up(client: &EnvClient, project: &Path) -> Result<()> {
	let path = read_target(project)?;
	for _ in 0..WARMUP_OPERATIONS {
		ensure_read_fixture(&read_whole(client, &path).await?)?;
	}
	let session = open_session(client, project).await?;
	for _ in 0..WARMUP_OPERATIONS {
		run_script(client, &session, "true", false).await?;
	}
	close_session(client, session).await?;
	for value in 0..WARMUP_OPERATIONS {
		echo(client, value).await?;
	}
	Ok(())
}

/// Reads the fixture until the first failure and returns it with the number
/// of reads that succeeded.
async fn read_until_failure(
	client: EnvClient,
	path: EnvPath,
	completed: Arc<AtomicU64>,
) -> Result<ClientError> {
	loop {
		match read_whole(&client, &path).await {
			Ok(content) => {
				ensure_read_fixture(&content)?;
				completed.fetch_add(1, Ordering::Release);
			},
			Err(failure) => return Ok(failure),
		}
	}
}

fn is_typed_terminal(failure: &ClientError) -> bool {
	matches!(failure, ClientError::Protocol(_) | ClientError::StreamLost(_))
}

async fn typed_terminal(run: &mut ExecRun, killed: Instant, label: &str) -> Result<Duration> {
	let end = within("remote command terminal", STEP_LIMIT, async {
		loop {
			match run.next_event().await {
				Ok(Some(ExecEvent::Started(_) | ExecEvent::Output(_))) => {},
				other => return other,
			}
		}
	})
	.await?;
	let after = killed.elapsed();
	match end {
		Err(failure) if is_typed_terminal(&failure) => Ok(after),
		other => Err(error(format!("{label} ended without a typed terminal error: {other:?}"))),
	}
}

struct Observation {
	terminal:   Duration,
	held:       Duration,
	family:     usize,
	read_error: ClientError,
}

async fn kill_during_work(
	target: &Target,
	state: DaemonState,
	grace: Duration,
) -> Result<Observation> {
	let client = target.client().clone();
	let project = target.project().to_path_buf();
	let daemon = target.daemon_pid()?;
	if state == DaemonState::Warm {
		warm_up(&client, &project).await?;
	}

	let session_a = open_session(&client, &project).await?;
	let session_b = open_session(&client, &project).await?;
	let mut sleeper_a = start_running(&client, &session_a, REMOTE_SLEEP).await?;
	let mut sleeper_b = start_running(&client, &session_b, REMOTE_SLEEP).await?;
	let completed = Arc::new(AtomicU64::new(0));
	let reader = tokio::spawn(read_until_failure(
		client.clone(),
		read_target(&project)?,
		Arc::clone(&completed),
	));
	let held = start_call(&client, "p8_hold", &json!({ "millis": HOLD_MILLIS })).await?;
	let held = tokio::spawn(finish_call(held, STEP_LIMIT));
	within("a document read to complete", STEP_LIMIT, async {
		while completed.load(Ordering::Acquire) == 0 {
			time::sleep(Duration::from_millis(1)).await;
		}
	})
	.await?;

	let census = Census::take().await?;
	let family: BTreeSet<u32> = census
		.descendants(daemon)
		.into_iter()
		.chain(census.group(daemon))
		.filter(|pid| *pid != daemon)
		.collect();

	let killed = Instant::now();
	signal::kill(Pid::from_raw(daemon.cast_signed()), Signal::SIGKILL)
		.context("SIGKILL project daemon")?;

	let terminal_a = typed_terminal(&mut sleeper_a, killed, "the first remote command").await?;
	let terminal_b = typed_terminal(&mut sleeper_b, killed, "the second remote command").await?;
	let read_error = within("document reader termination", STEP_LIMIT, reader)
		.await?
		.context("joining the document reader")??;
	let terminal_reader = killed.elapsed();
	if !is_typed_terminal(&read_error) {
		return Err(error(format!(
			"the document reader ended with an untyped error: {read_error:?}"
		)));
	}
	let terminal = terminal_a.max(terminal_b).max(terminal_reader);
	if terminal > grace {
		return Err(error(format!(
			"remote calls were still pending {terminal:?} after the daemon died, past the {grace:?} \
			 interrupt grace"
		)));
	}

	let cwd = super::workloads::directory_path(&project)?;
	match within(
		"request after the daemon died",
		STEP_LIMIT,
		client.open_session(&cwd, frame::OpenSessionRequest::default()),
	)
	.await?
	{
		Err(failure) if is_typed_terminal(&failure) => {},
		other => {
			return Err(error(format!(
				"a request after the daemon died did not fail with a typed error: {other:?}"
			)));
		},
	}

	let held_payload = within("the held local call", STEP_LIMIT, held)
		.await?
		.context("joining the held local call")??;
	let held_after = killed.elapsed();
	let details = held_payload.get("details").unwrap_or(&held_payload);
	if details.get("held").and_then(Value::as_u64) != Some(HOLD_MILLIS) {
		return Err(error(format!("the held local call returned {held_payload}")));
	}
	for value in 0..LOCAL_CALLS_AFTER_KILL {
		echo(&client, value).await?;
	}

	let survivors = {
		let census = Census::take().await?;
		let alive = census.alive(&family);
		(census.describe(&alive), alive)
	};
	if !survivors.1.is_empty() {
		kill_all(&survivors.1);
		return Err(error(format!(
			"the killed daemon left child processes running: {}",
			survivors.0
		)));
	}
	drop(sleeper_a);
	drop(sleeper_b);
	Ok(Observation { terminal, held: held_after, family: family.len(), read_error })
}

async fn daemon_kill(state: DaemonState, plan: &Plan) -> Result<Sample> {
	let grace = interrupt_grace()?;
	let recorder = std::process::id();
	let baseline = Census::take().await?.descendants(recorder);

	let target = Staging::new(plan).await?.start(Transport::Attached).await?;
	let daemon = target.daemon_pid()?;
	let observation = kill_during_work(&target, state, grace).await;
	let closed = target.close().await;
	let observation = observation?;
	let closed = closed?;

	let router_exit = match closed.router {
		Some(RouterExit::ClientClosed { after }) => after,
		other => {
			return Err(error(format!(
				"the partition router did not end because its client closed: {other:?}"
			)));
		},
	};
	wait_process_group_dead(daemon.cast_signed(), SETTLE)
		.await
		.context("the killed daemon's process group")?;
	settle_children(recorder, &baseline, "the daemon-kill proof").await?;
	Ok(Sample {
		metrics: vec![
			("kill_to_terminal_ns", observation.terminal.as_nanos() as f64),
			("held_call_after_kill_ns", observation.held.as_nanos() as f64),
			("router_exit_ns", router_exit.as_nanos() as f64),
			("daemon_family_count", observation.family as f64),
			("reaped_children_count", closed.reaped.count as f64),
		],
		facts:   vec![
			("remote_streams_terminated", 3),
			("pending_after_grace", 0),
			("orphaned_children", 0),
			("local_calls_after_kill", LOCAL_CALLS_AFTER_KILL + 1),
			("typed_reader_error", u64::from(is_typed_terminal(&observation.read_error))),
		],
	})
}
