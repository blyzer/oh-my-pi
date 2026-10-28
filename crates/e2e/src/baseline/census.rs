//! Process census for the W8 containment proofs: who is alive, who parented
//! whom, and whether anything a proof started outlived it.

use std::{
	collections::BTreeSet,
	process::{Command, Stdio},
	time::{Duration, Instant},
};

use nix::{
	sys::signal::{self, Signal},
	unistd::Pid,
};
use tokio::time;

use crate::{Context as _, Result, error};

/// How long killed children get to show up gone.
const REAP_LIMIT: Duration = Duration::from_secs(5);

/// One observation window of [`quiesce`].
const QUIET_WINDOW: Duration = Duration::from_millis(50);
/// Consecutive quiet windows that end [`quiesce`].
const QUIET_WINDOWS: u32 = 3;
/// Upper bound on [`quiesce`].
const QUIET_LIMIT: Duration = Duration::from_secs(3);

#[derive(Clone, Debug)]
struct Process {
	pid:     u32,
	ppid:    u32,
	pgid:    u32,
	zombie:  bool,
	command: String,
}

/// One `ps` snapshot of every process on the host.
#[derive(Debug)]
pub(super) struct Census {
	processes: Vec<Process>,
}

impl Census {
	/// Snapshots the process table without blocking the async runtime.
	pub(super) async fn take() -> Result<Self> {
		tokio::task::spawn_blocking(Self::take_blocking)
			.await
			.context("joining process census")?
	}

	fn take_blocking() -> Result<Self> {
		let child = Command::new("ps")
			.args(["-A", "-o", "pid=,ppid=,pgid=,stat=,command="])
			.stdout(Stdio::piped())
			.spawn()
			.context("running ps")?;
		let snapshotter = child.id();
		let output = child.wait_with_output().context("reading ps")?;
		if !output.status.success() {
			return Err(error(format!("ps exited with {}", output.status)));
		}
		let processes = String::from_utf8_lossy(&output.stdout)
			.lines()
			.filter_map(parse_line)
			.filter(|process| process.pid != snapshotter)
			.collect();
		Ok(Self { processes })
	}

	/// Live processes below `root` in the parent chain, not including `root`.
	pub(super) fn descendants(&self, root: u32) -> BTreeSet<u32> {
		let mut found = BTreeSet::new();
		let mut frontier = vec![root];
		while let Some(parent) = frontier.pop() {
			for process in &self.processes {
				if process.ppid == parent && !process.zombie && found.insert(process.pid) {
					frontier.push(process.pid);
				}
			}
		}
		found
	}

	/// Live members of process group `pgid`.
	pub(super) fn group(&self, pgid: u32) -> BTreeSet<u32> {
		self
			.processes
			.iter()
			.filter(|process| process.pgid == pgid && !process.zombie)
			.map(|process| process.pid)
			.collect()
	}

	/// The members of `pids` that are still alive.
	pub(super) fn alive(&self, pids: &BTreeSet<u32>) -> BTreeSet<u32> {
		self
			.processes
			.iter()
			.filter(|process| pids.contains(&process.pid) && !process.zombie)
			.map(|process| process.pid)
			.collect()
	}

	/// Renders `pids` as `pid (parent, group) command` lines for a failure
	/// message.
	pub(super) fn describe(&self, pids: &BTreeSet<u32>) -> String {
		self
			.processes
			.iter()
			.filter(|process| pids.contains(&process.pid))
			.map(|process| {
				format!(
					"{} (parent {}, group {}) {}",
					process.pid, process.ppid, process.pgid, process.command
				)
			})
			.collect::<Vec<_>>()
			.join("; ")
	}
}

/// The children the recorder had before an authority started, so what the
/// authority leaves behind can be told from the recorder's own children.
///
/// An in-process authority has no public stop for its extension host or MCP
/// servers: they outlive the authority even when every handle to it is gone.
/// The harness owns them, so it reaps them here instead of leaking them into
/// the next repetition.
#[derive(Debug)]
pub(super) struct Ledger {
	recorder: u32,
	before:   BTreeSet<u32>,
}

/// The processes a [`Ledger`] had to kill.
#[derive(Debug, Default)]
pub(super) struct Reaped {
	pub(super) count:       usize,
	pub(super) description: String,
}

impl Ledger {
	/// Records the recorder's current children.
	pub(super) async fn open() -> Result<Self> {
		let recorder = std::process::id();
		let before = Census::take().await?.descendants(recorder);
		Ok(Self { recorder, before })
	}

	/// Kills every child gained since [`Self::open`] and waits until none is
	/// left.
	pub(super) async fn reap(&self) -> Result<Reaped> {
		let mut reaped = Reaped::default();
		let deadline = Instant::now() + REAP_LIMIT;
		loop {
			let census = Census::take().await?;
			let gained: BTreeSet<u32> = census
				.descendants(self.recorder)
				.difference(&self.before)
				.copied()
				.collect();
			if gained.is_empty() {
				return Ok(reaped);
			}
			if reaped.count == 0 {
				reaped.count = gained.len();
				reaped.description = census.describe(&gained);
			}
			if Instant::now() >= deadline {
				return Err(error(format!(
					"child processes survived being killed: {}",
					census.describe(&gained)
				)));
			}
			kill_all(&gained);
			time::sleep(Duration::from_millis(25)).await;
		}
	}
}

/// CPU time this process has used so far, across all of its threads.
fn process_cpu() -> Duration {
	// SAFETY: `rusage` is plain data that all-zero bytes initialize, and
	// `getrusage` only writes through the pointer it is given.
	let usage = unsafe {
		let mut usage: libc::rusage = std::mem::zeroed();
		libc::getrusage(libc::RUSAGE_SELF, &raw mut usage);
		usage
	};
	let seconds = |value: libc::timeval| {
		Duration::new(
			u64::try_from(value.tv_sec).unwrap_or(0),
			u32::try_from(value.tv_usec)
				.unwrap_or(0)
				.saturating_mul(1_000),
		)
	};
	seconds(usage.ru_utime) + seconds(usage.ru_stime)
}

/// Waits until this process is nearly idle, so the teardown of what ran before
/// cannot distort the next measurement, and returns how long that took.
///
/// Authorities the recorder stopped keep unwinding background work for a
/// moment. A window counts as quiet when the process used at most a tenth of
/// one CPU in it; three quiet windows in a row end the wait, and `QUIET_LIMIT`
/// bounds it for a process that never settles.
pub(super) async fn quiesce() -> Duration {
	let started = Instant::now();
	let mut quiet = 0;
	let mut last = process_cpu();
	while quiet < QUIET_WINDOWS && started.elapsed() < QUIET_LIMIT {
		time::sleep(QUIET_WINDOW).await;
		let now = process_cpu();
		if now.saturating_sub(last) <= QUIET_WINDOW / 10 {
			quiet += 1;
		} else {
			quiet = 0;
		}
		last = now;
	}
	started.elapsed()
}

/// Kills `pids` so a failed proof does not leave them running.
pub(super) fn kill_all(pids: &BTreeSet<u32>) {
	for pid in pids {
		if let Ok(raw) = i32::try_from(*pid) {
			let _ = signal::kill(Pid::from_raw(raw), Signal::SIGKILL);
		}
	}
}

fn parse_line(line: &str) -> Option<Process> {
	let mut fields = line.split_whitespace();
	let pid = fields.next()?.parse().ok()?;
	let ppid = fields.next()?.parse().ok()?;
	let pgid = fields.next()?.parse().ok()?;
	let zombie = fields.next()?.starts_with('Z');
	let command = fields.collect::<Vec<_>>().join(" ");
	Some(Process { pid, ppid, pgid, zombie, command })
}
