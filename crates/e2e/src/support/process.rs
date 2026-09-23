use std::{env, io, path::PathBuf, process, process::Stdio, sync::Once, time::Duration};

#[cfg(unix)]
use nix::{
	errno::Errno,
	sys::signal::{self, Signal},
	unistd::Pid,
};
use tokio::{
	process::{Child, Command},
	time,
};

#[cfg(unix)]
use super::owned_groups::GroupLease;
use super::within;
use crate::{Context as _, Result};

static OMP_BINARY_ENV: Once = Once::new();

/// Exposes the Cargo-built acceptance host to production same-binary child
/// resolvers for the lifetime of this test process.
pub fn install_omp_binary_env() -> io::Result<()> {
	let path = omp_binary()?;
	OMP_BINARY_ENV.call_once(|| {
		// Every proof installs the same immutable Cargo path before opening an
		// environment authority, and the value is never changed or removed.
		unsafe {
			env::set_var("CARGO_BIN_EXE_omp", path);
		}
	});
	Ok(())
}

/// Resolves the worker-capable application binary Cargo builds with `omp-e2e`
/// tests.
pub fn omp_binary() -> io::Result<PathBuf> {
	if let Some(path) = env::var_os("CARGO_BIN_EXE_omp_e2e_host") {
		let path = PathBuf::from(path);
		if path.is_file() {
			return Ok(path);
		}
	}
	let current = env::current_exe()?;
	if current
		.file_stem()
		.is_some_and(|name| name == "omp_e2e_host")
	{
		return Ok(current);
	}
	let profile = current
		.parent()
		.and_then(|parent| {
			(parent.file_name().is_some_and(|name| name == "deps")).then(|| parent.parent())
		})
		.flatten()
		.ok_or_else(|| {
			io::Error::new(
				io::ErrorKind::NotFound,
				"test executable is not under Cargo's deps directory",
			)
		})?;
	let binary = profile.join(format!("omp_e2e_host{}", std::env::consts::EXE_SUFFIX));
	if binary.is_file() {
		Ok(binary)
	} else {
		Err(io::Error::new(
			io::ErrorKind::NotFound,
			format!("Cargo-built omp_e2e_host is missing at {}", binary.display()),
		))
	}
}

/// Poll interval while reaping an owned child. The reap runs as a non-blocking
/// `try_wait` under the group registry lock, so the leader is never reaped
/// while its group is still registered.
const REAP_POLL: Duration = Duration::from_millis(5);

/// Child process placed in its own process group and killed as a tree on drop
/// or when its spawning thread panics.
#[derive(Debug)]
#[must_use]
pub struct OwnedProcess {
	// Declared before `child`: the lease must leave the registry before the
	// child handle's own drop can reap the leader.
	#[cfg(unix)]
	lease:  GroupLease,
	child:  Child,
	exited: bool,
}

impl OwnedProcess {
	/// Spawns one directly addressed executable without a shell.
	pub fn spawn(mut command: Command) -> io::Result<Self> {
		command.stdin(Stdio::null()).kill_on_drop(true);
		#[cfg(unix)]
		{
			use std::os::unix::process::CommandExt as _;
			command.as_std_mut().process_group(0);
		}
		let child = command.spawn()?;
		#[cfg(unix)]
		let lease = {
			let group = child
				.id()
				.and_then(|pid| i32::try_from(pid).ok())
				.ok_or_else(|| io::Error::other("spawned child has no usable process id"))?;
			GroupLease::acquire(group)
		};
		Ok(Self {
			#[cfg(unix)]
			lease,
			child,
			exited: false,
		})
	}

	/// Returns the operating-system child identifier while it is known.
	pub fn id(&self) -> Option<u32> {
		self.child.id()
	}

	/// Returns the dedicated Unix process-group identifier.
	pub const fn process_group(&self) -> Option<i32> {
		#[cfg(unix)]
		return Some(self.lease.group());
		#[cfg(not(unix))]
		None
	}

	/// Waits for normal process exit within `limit`.
	pub async fn wait(&mut self, limit: Duration) -> Result<process::ExitStatus> {
		Ok(within("owned child exit", limit, self.reap()).await??)
	}

	/// Requests TERM, then escalates to KILL after `grace`, always targeting the
	/// tree.
	pub async fn terminate(mut self, grace: Duration) -> Result<()> {
		if self.exited {
			return Ok(());
		}
		#[cfg(unix)]
		self.lease.signal(Signal::SIGTERM);
		#[cfg(not(unix))]
		let _ = self.child.start_kill();
		if time::timeout(grace, self.reap()).await.is_err() {
			#[cfg(unix)]
			self.lease.signal(Signal::SIGKILL);
			#[cfg(not(unix))]
			let _ = self.child.start_kill();
			self.reap().await.context("waiting for killed child")?;
		}
		Ok(())
	}

	async fn reap(&mut self) -> io::Result<process::ExitStatus> {
		loop {
			#[cfg(unix)]
			let status = self.lease.reap(|| self.child.try_wait())?;
			#[cfg(not(unix))]
			let status = self.child.try_wait()?;
			if let Some(status) = status {
				self.exited = true;
				return Ok(status);
			}
			time::sleep(REAP_POLL).await;
		}
	}
}

// On Unix the lease's own drop kills a still-registered group.
#[cfg(not(unix))]
impl Drop for OwnedProcess {
	fn drop(&mut self) {
		if !self.exited {
			let _ = self.child.start_kill();
		}
	}
}

/// Reports whether any process remains in a Unix process group.
#[cfg(unix)]
pub fn process_group_alive(group: i32) -> bool {
	match signal::killpg(Pid::from_raw(group), None) {
		Ok(()) | Err(Errno::EPERM) => true,
		Err(Errno::ESRCH) => false,
		Err(_) => true,
	}
}

/// Waits until a Unix process group disappears, with deterministic polling and
/// a hard bound.
#[cfg(unix)]
pub async fn wait_process_group_dead(group: i32, limit: Duration) -> Result<()> {
	within("process-group death", limit, async move {
		while process_group_alive(group) {
			time::sleep(Duration::from_millis(10)).await;
		}
	})
	.await
}
