//! Test-only process liveness probe for teardown assertions.

use nix::{errno::Errno, sys::signal, unistd::Pid};

/// Whether `pid` has terminated, treating an unreaped zombie as terminated.
///
/// `kill(pid, 0)` succeeds for a zombie, so it cannot tell "still running" from
/// "exited but not yet waited for". A teardown test that kills a grandchild
/// (not envd's own child) cannot rely on anyone reaping it: on macOS launchd
/// adopts and reaps orphans, but a container whose PID 1 never calls `wait`
/// leaves them as zombies forever. Linux therefore reads the state field of
/// `/proc/<pid>/stat` and counts `Z` (zombie) and `X` (dead) as terminated;
/// other platforms use plain `kill(pid, 0)` semantics.
pub fn process_terminated(pid: Pid) -> bool {
	#[cfg(target_os = "linux")]
	if let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) {
		// The command name is parenthesised and may itself contain spaces or
		// parentheses, so the state is the first field after the last `)`.
		return stat
			.rsplit_once(')')
			.and_then(|(_, rest)| rest.trim_start().chars().next())
			.is_some_and(|state| matches!(state, 'Z' | 'X'));
	}
	!matches!(signal::kill(pid, None), Ok(()) | Err(Errno::EPERM))
}
