//! Process groups the harness owns, reachable from a panic hook.
//!
//! Test builds are LLVM (`[profile.test]` in `.cargo/config.toml`), so a
//! failing proof's panic unwinds through its frames and [`OwnedProcess`]'s
//! `Drop` kills the group. This registry is defense in depth for panics that
//! run no destructors: a panic while panicking (which aborts), a
//! `panic = "abort"` build, or a test build that is Cranelift again, whose
//! frames carry no landing pads
//! (`docs/audits/cranelift-panic-cleanup.md`). Each group is also leased here,
//! and a panic hook kills the groups the panicking thread owns. Nothing here
//! runs on SIGKILL, such as a nextest timeout. This covers only harness-owned
//! process groups; it is not a general substitute for `Drop`.
//!
//! Scope is the thread that spawned the leader; proofs spawn from their test
//! body. A panic on another thread kills only that thread's groups.
//!
//! [`OwnedProcess`]: crate::support::OwnedProcess
//!
//! Identity: a process-group id cannot be reused while its leader is
//! unreaped, so an entry lives only while its leader is unreaped. The only
//! reap before release happens under the registry lock
//! ([`GroupLease::reap`]), which removes the entry in the same critical
//! section. Every signal is sent under that lock after checking the entry is
//! still present, so no path signals a group whose id might have been reused.

use std::{
	io, panic,
	sync::Once,
	thread::{self, ThreadId},
	time::Duration,
};

use nix::{
	sys::signal::{self, Signal},
	unistd::Pid,
};
use parking_lot::Mutex;

/// Longest the panic hook waits for the registry. Other holders do only
/// non-blocking syscalls, so this bounds a contended hook without letting it
/// hang when the panicking thread itself holds the lock.
const HOOK_LOCK_BOUND: Duration = Duration::from_millis(100);

static REGISTRY: Mutex<Registry> = Mutex::new(Registry { next: 0, entries: Vec::new() });
static HOOK: Once = Once::new();

struct Registry {
	next:    u64,
	entries: Vec<Entry>,
}

struct Entry {
	token: u64,
	group: i32,
	/// Thread that spawned the leader; only a panic there kills the group.
	owner: ThreadId,
}

impl Registry {
	fn position(&self, token: u64) -> Option<usize> {
		self.entries.iter().position(|entry| entry.token == token)
	}
}

/// Registration of one process group whose unreaped leader this process
/// spawned. Dropping it kills the group if it is still registered.
#[derive(Debug)]
pub(super) struct GroupLease {
	token: u64,
	group: i32,
}

impl GroupLease {
	/// Registers `group` to the current thread. The caller must hold the
	/// unreaped leader, whose process id is `group`.
	pub(super) fn acquire(group: i32) -> Self {
		HOOK.call_once(install_hook);
		let mut registry = REGISTRY.lock();
		let token = registry.next;
		registry.next += 1;
		registry
			.entries
			.push(Entry { token, group, owner: thread::current().id() });
		Self { token, group }
	}

	/// Returns the leased process-group id.
	pub(super) const fn group(&self) -> i32 {
		self.group
	}

	/// Signals the group while it is still registered.
	pub(super) fn signal(&self, signal: Signal) {
		let registry = REGISTRY.lock();
		if registry.position(self.token).is_some() {
			let _ = signal::killpg(Pid::from_raw(self.group), Some(signal));
		}
	}

	/// Runs the non-blocking `reap` of the leader under the registry lock and
	/// unregisters the group in the same critical section when it succeeds.
	pub(super) fn reap<T>(
		&self,
		reap: impl FnOnce() -> io::Result<Option<T>>,
	) -> io::Result<Option<T>> {
		let mut registry = REGISTRY.lock();
		let reaped = reap()?;
		if reaped.is_some()
			&& let Some(index) = registry.position(self.token)
		{
			registry.entries.swap_remove(index);
		}
		Ok(reaped)
	}
}

impl Drop for GroupLease {
	fn drop(&mut self) {
		let mut registry = REGISTRY.lock();
		if let Some(index) = registry.position(self.token) {
			let _ = signal::killpg(Pid::from_raw(self.group), Some(Signal::SIGKILL));
			registry.entries.swap_remove(index);
		}
	}
}

/// Chains cleanup in front of whichever hook was installed before.
fn install_hook() {
	let previous = panic::take_hook();
	panic::set_hook(Box::new(move |info| {
		kill_current_thread_groups();
		previous(info);
	}));
}

/// Kills and unregisters every group the panicking thread owns. Only sends
/// signals: no waits, no joins.
fn kill_current_thread_groups() {
	let owner = thread::current().id();
	let Some(mut registry) = REGISTRY.try_lock_for(HOOK_LOCK_BOUND) else {
		return;
	};
	registry.entries.retain(|entry| {
		if entry.owner != owner {
			return true;
		}
		let _ = signal::killpg(Pid::from_raw(entry.group), Some(Signal::SIGKILL));
		false
	});
}

#[cfg(test)]
mod tests {
	use std::{
		env, fs, panic,
		process::{self, Stdio},
		sync::Arc,
		thread,
		time::{Duration, Instant},
	};

	use parking_lot::Mutex;
	use tokio::{process::Command, runtime::Handle};

	use super::REGISTRY;
	use crate::support::{OwnedProcess, process_group_alive};

	const CHILD_TEST: &str = "support::owned_groups::tests::panicking_owner_child";
	const REPORT_ENV: &str = "OMP_E2E_PANIC_REPORT";
	const GROUP_DEATH: Duration = Duration::from_secs(20);

	fn sleeper() -> Command {
		let mut command = Command::new("/bin/sleep");
		command
			.arg("300")
			.stdout(Stdio::null())
			.stderr(Stdio::null());
		command
	}

	fn registered(group: i32) -> bool {
		REGISTRY
			.lock()
			.entries
			.iter()
			.any(|entry| entry.group == group)
	}

	/// A real panic on libtest's thread, in a child run of this binary. The
	/// hook runs before any unwinding, so a hook installed before the
	/// registry's records the state the registry's hook left behind, whatever
	/// the backend does with destructors afterwards.
	#[test]
	fn panic_cleanup_is_scoped_to_the_panicking_thread_and_chains_the_previous_hook() {
		let scratch = tempfile::tempdir().expect("scratch");
		let report = scratch.path().join("report");
		let status = process::Command::new(env::current_exe().expect("test binary"))
			.args(["--exact", CHILD_TEST, "--ignored", "--nocapture", "--test-threads=1"])
			.env(REPORT_ENV, &report)
			.stdin(Stdio::null())
			.stdout(Stdio::null())
			.stderr(Stdio::null())
			.status()
			.expect("run child proof");
		assert!(!status.success(), "the child proof fails by panicking");
		let report = fs::read_to_string(&report).expect("the previous hook ran and reported");
		let field = |name: &str| {
			report
				.split_whitespace()
				.find_map(|pair| pair.strip_prefix(name)?.strip_prefix('='))
				.unwrap_or_else(|| panic!("report lacks {name}: {report}"))
		};
		assert_eq!(field("doomed_registered"), "false", "the hook unregisters its kills");
		assert_eq!(field("survivor_registered"), "true", "another thread's group stays leased");
		assert_eq!(field("survivor_alive"), "true", "another thread's group keeps running");
		for name in ["doomed", "survivor"] {
			let group = field(name).parse().expect("group id");
			let deadline = Instant::now() + GROUP_DEATH;
			while process_group_alive(group) {
				assert!(Instant::now() < deadline, "{name} group {group} outlived the child proof");
				thread::sleep(Duration::from_millis(20));
			}
		}
	}

	/// Child half of the test above; a no-op unless it runs as that child.
	#[tokio::test]
	#[ignore = "run by panic_cleanup_is_scoped_to_the_panicking_thread_and_chains_the_previous_hook"]
	async fn panicking_owner_child() {
		let Some(report) = env::var_os(REPORT_ENV) else {
			return;
		};
		let held: Arc<Mutex<Option<(OwnedProcess, i32)>>> = Arc::default();
		let hook_held = Arc::clone(&held);
		// Installed before the first lease, so the registry's hook chains it.
		panic::set_hook(Box::new(move |_| {
			let Some((survivor, doomed)) = hook_held.lock().take() else {
				return;
			};
			let survivor_group = survivor.process_group().unwrap_or_default();
			let line = format!(
				"doomed={doomed} doomed_registered={} survivor={survivor_group} \
				 survivor_registered={} survivor_alive={}\n",
				registered(doomed),
				registered(survivor_group),
				process_group_alive(survivor_group),
			);
			let _ = fs::write(&report, line);
			// Releases the survivor through its own lease.
			drop(survivor);
		}));
		let runtime = Handle::current();
		let survivor = thread::spawn(move || {
			let _runtime = runtime.enter();
			OwnedProcess::spawn(sleeper())
		})
		.join()
		.expect("survivor thread")
		.expect("spawn survivor");
		let doomed = OwnedProcess::spawn(sleeper()).expect("spawn doomed");
		let doomed_group = doomed.process_group().expect("doomed group");
		*held.lock() = Some((survivor, doomed_group));
		panic!("owner thread fails holding {doomed:?}");
	}
}
