//! In-process and daemon session host: live kernel registry and owner-only
//! UDS discovery endpoint.
//!
//! Every `omp chat/rpc/print` process owns a [`SessionHost`] that tracks its
//! live kernel mailboxes and exposes a metadata-only listing over an
//! owner-only Unix-domain socket under `<data dir>/run/session.sock`
//! (0700 directory, 0600 socket).
//!
//! `omp serve --sessions` uses the daemon deployment variant, where
//! [`SessionHost::daemon`] hosts kernels that outlive any client.
//!
//! The UDS is the filesystem authenticator; no bearer token is required.
//! `omp.session.v1` gRPC projection and per-verb access control are out of
//! scope here and live in `omp-serve` (Part A2/A4 of ADR 0039).

#[cfg(unix)]
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::{FileTypeExt as _, PermissionsExt as _};
use std::{
	path::{Path, PathBuf},
	time::SystemTime,
};

use omp_agent::Up;
use omp_core::FastHashMap;
use tokio_util::sync::CancellationToken;

use crate::sessions::SessionId;

/// The kernel's upward mailbox: the sole channel through which remote actors
/// drive a live session.
///
/// This is a type alias over the channel end already held by every
/// [`KernelHandle`][crate::sessions::KernelHandle].
pub type KernelMailbox = flume::Sender<Up>;

/// Lightweight per-session metadata returned by [`SessionHost::list`].
///
/// No mailbox reference is exposed; callers that need to send must hold a
/// [`KernelMailbox`] obtained at [`SessionHost::register`] time.
#[derive(Clone, Debug)]
pub struct SessionRef {
	/// Stable session identifier.
	pub id:         SessionId,
	/// Lifecycle status at snapshot time.
	pub status:     SessionStatus,
	/// Wall-clock creation time of this entry.
	pub created_at: SystemTime,
}

/// Coarse lifecycle status of a hosted session.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionStatus {
	/// The kernel is running or waiting for input.
	Live,
	/// The kernel has been cancelled and is winding down.
	Cancelled,
}

/// Internal per-session bookkeeping.
struct Entry {
	mailbox:    KernelMailbox,
	cancel:     CancellationToken,
	status:     SessionStatus,
	created_at: SystemTime,
}

/// How the host was constructed.
enum Mode {
	/// A single owner process; the UDS listener is at `socket`.
	InProcess {
		#[cfg(unix)]
		_listener: tokio::net::UnixListener,
		#[cfg(unix)]
		socket:    PathBuf,
	},
	/// A headless daemon; sessions outlive individual client connections.
	Daemon,
}

/// In-process and daemon session host.
///
/// Holds live kernel mailboxes and exposes a metadata-only listing over an
/// owner-only UDS. The gRPC `SessionControl` projection (ADR 0039 A2) reads
/// through this host; it never holds mailboxes directly.
pub struct SessionHost {
	sessions: FastHashMap<SessionId, Entry>,
	mode:     Mode,
}

impl SessionHost {
	/// Creates an in-process host that binds an owner-only UDS at
	/// `data_dir/run/session.sock`.
	///
	/// The `run/` directory is created with mode 0700. The socket file is
	/// set to 0600 after bind. A stale socket that is not connected is
	/// removed and replaced.
	///
	/// On non-Unix platforms the socket binding is skipped and the host
	/// behaves the same in all other respects.
	pub async fn in_process(data_dir: &Path) -> Result<Self, SessionHostError> {
		let mode = bind_in_process(data_dir).await?;
		Ok(Self { sessions: FastHashMap::default(), mode })
	}

	/// Creates a daemon host that does not bind a per-process socket.
	///
	/// The socket for a daemon deployment is managed by `app/src/daemon.rs`
	/// which assembles the full tonic server.
	#[must_use]
	pub fn daemon() -> Self {
		Self { sessions: FastHashMap::default(), mode: Mode::Daemon }
	}

	/// Registers a live kernel mailbox under `session_id`.
	///
	/// If a prior entry exists under the same id it is displaced; its
	/// cancellation token is **not** fired — the caller owns that decision.
	pub fn register(
		&mut self,
		session_id: SessionId,
		mailbox: KernelMailbox,
		cancel: CancellationToken,
	) {
		let entry =
			Entry { mailbox, cancel, status: SessionStatus::Live, created_at: SystemTime::now() };
		self.sessions.insert(session_id, entry);
	}

	/// Removes the entry for `session_id`, if present.
	pub fn deregister(&mut self, session_id: &SessionId) {
		self.sessions.remove(session_id);
	}

	/// Returns lightweight metadata for every registered session.
	///
	/// Entries whose cancellation tokens have already fired are reported as
	/// [`SessionStatus::Cancelled`]; they are not automatically removed.
	#[must_use]
	pub fn list(&self) -> Vec<SessionRef> {
		self
			.sessions
			.iter()
			.map(|(id, entry)| {
				let status = if entry.cancel.is_cancelled() {
					SessionStatus::Cancelled
				} else {
					entry.status
				};
				SessionRef { id: id.clone(), status, created_at: entry.created_at }
			})
			.collect()
	}

	/// Returns the kernel mailbox for `session_id`, if registered.
	#[must_use]
	pub fn mailbox(&self, session_id: &SessionId) -> Option<&KernelMailbox> {
		self.sessions.get(session_id).map(|e| &e.mailbox)
	}

	/// The path to the owner-only UDS, if this is an in-process host.
	#[cfg(unix)]
	#[must_use]
	pub fn socket_path(&self) -> Option<&Path> {
		match &self.mode {
			Mode::InProcess { socket, .. } => Some(socket.as_path()),
			Mode::Daemon => None,
		}
	}
}

impl Drop for SessionHost {
	fn drop(&mut self) {
		#[cfg(unix)]
		if let Mode::InProcess { socket, .. } = &self.mode {
			// Best-effort cleanup; ignore errors (process may be exiting).
			let _ = fs::remove_file(socket);
		}
	}
}

/// Failure to create an in-process [`SessionHost`].
#[derive(Debug, thiserror::Error)]
pub enum SessionHostError {
	/// Failed to create or restrict the `run/` directory.
	#[error("could not create owner-only run directory {path}: {source}")]
	CreateDir {
		/// Directory that could not be created.
		path:   PathBuf,
		/// Underlying OS error.
		source: std::io::Error,
	},
	/// Failed to bind or restrict the session socket.
	#[error("could not bind session socket {path}: {source}")]
	BindSocket {
		/// Socket path that failed.
		path:   PathBuf,
		/// Underlying OS error.
		source: std::io::Error,
	},
}

#[cfg(unix)]
async fn bind_in_process(data_dir: &Path) -> Result<Mode, SessionHostError> {
	use tokio::net::{UnixListener, UnixStream};

	let run_dir = data_dir.join("run");
	// Create run/ as 0700.
	tokio::fs::create_dir_all(&run_dir)
		.await
		.map_err(|source| SessionHostError::CreateDir { path: run_dir.clone(), source })?;
	fs::set_permissions(&run_dir, fs::Permissions::from_mode(0o700))
		.map_err(|source| SessionHostError::CreateDir { path: run_dir.clone(), source })?;

	let socket = run_dir.join("session.sock");

	// Remove a stale socket that is not actively accepting.
	match tokio::fs::symlink_metadata(&socket).await {
		Ok(metadata) if metadata.file_type().is_socket() => {
			match UnixStream::connect(&socket).await {
				Ok(_) => {
					return Err(SessionHostError::BindSocket {
						path:   socket,
						source: std::io::Error::new(
							std::io::ErrorKind::AddrInUse,
							"session socket is already accepting connections",
						),
					});
				},
				Err(_) => {
					let _ = tokio::fs::remove_file(&socket).await;
				},
			}
		},
		Ok(_) => {},
		Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
		Err(source) => {
			return Err(SessionHostError::BindSocket { path: socket, source });
		},
	}

	let listener = UnixListener::bind(&socket)
		.map_err(|source| SessionHostError::BindSocket { path: socket.clone(), source })?;
	// Restrict to 0600 so no other OS user can connect.
	fs::set_permissions(&socket, fs::Permissions::from_mode(0o600))
		.map_err(|source| SessionHostError::BindSocket { path: socket.clone(), source })?;

	tracing::info!(socket = %socket.display(), "session host UDS listener ready");
	Ok(Mode::InProcess { _listener: listener, socket })
}

#[cfg(not(unix))]
async fn bind_in_process(_data_dir: &Path) -> Result<Mode, SessionHostError> {
	Ok(Mode::Daemon)
}
