//! Process-group isolation and teardown for child processes the environment
//! host owns.
//!
//! One implementation of "give the child its own targetable process group,
//! signal the whole group, TERM → grace → KILL, reap" shared by every resident
//! protocol child: the MCP stdio transport (`mcp::stdio`) and the delegated
//! agent adapters (`delegated`). Neither keeps a private copy.
//!
//! Every host spawn path creates a targetable process group — a session on
//! POSIX, a process group on macOS, `CREATE_NEW_PROCESS_GROUP` on Windows — so
//! teardown always sweeps the group after the leader settles.

use std::{
	io,
	process::{ExitStatus, Stdio},
	time::Duration,
};

#[cfg(unix)]
use nix::{sys::signal, unistd::Pid};
use tokio::{
	process::{Child, ChildStderr, ChildStdin, ChildStdout, Command},
	time,
};
#[cfg(windows)]
use windows_sys::Win32::System::Threading;

/// How long a process group gets to honor a terminate request before it is
/// killed.
pub(crate) const TERM_GRACE: Duration = Duration::from_secs(1);
/// How long a killed group leader is awaited.
pub(crate) const KILL_GRACE: Duration = Duration::from_millis(500);

/// Places the child in its own targetable process group.
///
/// `detached_session` selects a detached POSIX session (`setsid`) on non-macOS
/// POSIX hosts; macOS keeps the inherited session and TCC attachment and only
/// gets a separate process group, and Windows gets a new process group.
pub(crate) fn isolate(command: &mut Command, detached_session: bool) {
	let _ = detached_session;
	#[cfg(all(unix, not(target_os = "macos")))]
	if detached_session {
		use std::os::unix::process::CommandExt as _;
		// SAFETY: `setsid` has no memory-safety preconditions and this callback
		// performs no allocation between fork and exec.
		unsafe {
			command.as_std_mut().pre_exec(|| {
				if libc::setsid() == -1 {
					Err(io::Error::last_os_error())
				} else {
					Ok(())
				}
			});
		}
	}
	#[cfg(target_os = "macos")]
	{
		use std::os::unix::process::CommandExt as _;
		// A separate process group preserves the inherited macOS session/TCC
		// attachment while still giving teardown authority over descendants.
		command.as_std_mut().process_group(0);
	}
	#[cfg(windows)]
	{
		command.creation_flags(Threading::CREATE_NEW_PROCESS_GROUP);
	}
}

/// Signals the whole process group led by `pid`: terminate when `hard` is
/// false, kill when it is true. A group that is already gone is not an error.
pub(crate) fn signal_tree(pid: Option<u32>, hard: bool) {
	#[cfg(unix)]
	if let Some(pid) = pid {
		let signal = if hard {
			signal::Signal::SIGKILL
		} else {
			signal::Signal::SIGTERM
		};
		let raw = Pid::from_raw(pid.cast_signed());
		match signal::kill(Pid::from_raw(-raw.as_raw()), signal) {
			Ok(()) | Err(nix::errno::Errno::ESRCH) => {},
			Err(_) => {
				let _ = signal::kill(raw, signal);
			},
		}
	}
	#[cfg(windows)]
	if let Some(pid) = pid {
		if hard {
			terminate_windows_process_tree(pid);
		} else {
			// CREATE_NEW_PROCESS_GROUP makes `pid` a valid CTRL_BREAK group.
			// Failure is harmless: the bounded hard-kill sweep follows.
			// SAFETY: the event code and process-group identifier are plain values;
			// Windows validates whether the target group still exists.
			unsafe {
				windows_sys::Win32::System::Console::GenerateConsoleCtrlEvent(
					windows_sys::Win32::System::Console::CTRL_BREAK_EVENT,
					pid,
				);
			}
		}
	}
}

/// Terminates the group, waits a bounded time for the leader, then kills the
/// whole group and reaps the leader.
///
/// The kill sweep always runs, even when the leader exited inside the grace
/// period, because descendants may outlive it.
pub(crate) async fn reap(child: &mut Child, pid: Option<u32>) {
	let pid = child.id().or(pid);
	signal_tree(pid, false);
	let exited = time::timeout(TERM_GRACE, child.wait()).await.is_ok();
	signal_tree(pid, true);
	if !exited {
		let _ = time::timeout(KILL_GRACE, child.wait()).await;
	}
}

#[cfg(windows)]
fn terminate_windows_process_tree(root: u32) {
	use std::mem::size_of;

	use windows_sys::Win32::{
		Foundation::{CloseHandle, INVALID_HANDLE_VALUE},
		System::{
			Diagnostics::ToolHelp::{
				CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
				TH32CS_SNAPPROCESS,
			},
			Threading::{OpenProcess, PROCESS_TERMINATE, TerminateProcess},
		},
	};

	// SAFETY: the flags request a system-owned snapshot and require no caller
	// pointers.
	let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
	if snapshot == INVALID_HANDLE_VALUE {
		return;
	}
	let mut entry =
		PROCESSENTRY32W { dwSize: size_of::<PROCESSENTRY32W>() as u32, ..PROCESSENTRY32W::default() };
	let mut relationships = Vec::new();
	// SAFETY: `snapshot` is live and `entry` has the required size initialized.
	if unsafe { Process32FirstW(snapshot, &mut entry) } != 0 {
		loop {
			relationships.push((entry.th32ProcessID, entry.th32ParentProcessID));
			// SAFETY: the same live snapshot and initialized writable entry remain
			// valid for the duration of enumeration.
			if unsafe { Process32NextW(snapshot, &mut entry) } == 0 {
				break;
			}
		}
	}
	// SAFETY: `snapshot` is a live owned handle and is closed exactly once here.
	unsafe {
		CloseHandle(snapshot);
	}

	let mut tree = vec![root];
	loop {
		let before = tree.len();
		for &(pid, parent) in &relationships {
			if !tree.contains(&pid) && tree.contains(&parent) {
				tree.push(pid);
			}
		}
		if tree.len() == before {
			break;
		}
	}
	for pid in tree.into_iter().rev() {
		// SAFETY: Windows validates the process identifier and requested access.
		let process = unsafe { OpenProcess(PROCESS_TERMINATE, 0, pid) };
		if !process.is_null() {
			// SAFETY: `process` is a live owned handle, terminated and then closed
			// exactly once in this branch.
			unsafe {
				TerminateProcess(process, 1);
				CloseHandle(process);
			}
		}
	}
}

/// An owned child process and the process group it leads.
///
/// Dropping it kills the whole group synchronously and reaps the leader on the
/// live runtime when there is one; [`ChildTree::terminate_tree`] is the orderly
/// TERM → grace → KILL path.
pub struct ChildTree {
	child: Option<Child>,
	pid:   Option<u32>,
}

/// A freshly spawned [`ChildTree`] with its three pipes.
pub(crate) struct Spawned {
	/// The owned process tree.
	pub(crate) tree:   ChildTree,
	/// Write end of the child's stdin.
	pub(crate) stdin:  ChildStdin,
	/// Read end of the child's stdout.
	pub(crate) stdout: ChildStdout,
	/// Read end of the child's stderr.
	pub(crate) stderr: ChildStderr,
}

impl ChildTree {
	/// Spawns `command` in its own process group with piped stdio and
	/// kill-on-drop. The caller owns program, arguments, working directory and
	/// environment; this owns isolation and teardown.
	pub(crate) fn spawn(mut command: Command) -> io::Result<Spawned> {
		isolate(&mut command, true);
		command
			.stdin(Stdio::piped())
			.stdout(Stdio::piped())
			.stderr(Stdio::piped())
			.kill_on_drop(true);
		let mut child = command.spawn()?;
		let pid = child.id();
		let (Some(stdin), Some(stdout), Some(stderr)) =
			(child.stdin.take(), child.stdout.take(), child.stderr.take())
		else {
			signal_tree(pid, true);
			return Err(io::Error::other("child pipe missing after spawn"));
		};
		Ok(Spawned { tree: Self { child: Some(child), pid }, stdin, stdout, stderr })
	}

	/// Waits for the leader to exit. `None` when it was already reaped by
	/// [`ChildTree::terminate_tree`] or the wait failed.
	pub(crate) async fn wait(&mut self) -> Option<ExitStatus> {
		match self.child.as_mut() {
			Some(child) => child.wait().await.ok(),
			None => None,
		}
	}

	/// TERM → bounded grace → KILL of the whole group, then reaps the leader.
	/// Idempotent. Returns whether the leader was reaped.
	pub(crate) async fn terminate_tree(&mut self) -> bool {
		let Some(mut child) = self.child.take() else {
			return true;
		};
		reap(&mut child, self.pid).await;
		matches!(child.try_wait(), Ok(Some(_)))
	}
}

impl Drop for ChildTree {
	fn drop(&mut self) {
		let Some(mut child) = self.child.take() else {
			return;
		};
		// Drop has no async grace window: kill the complete group synchronously,
		// then reap opportunistically on a live runtime.
		signal_tree(self.pid, true);
		if let Ok(runtime) = tokio::runtime::Handle::try_current() {
			runtime.spawn(async move {
				let _ = time::timeout(KILL_GRACE, child.wait()).await;
			});
		}
	}
}
