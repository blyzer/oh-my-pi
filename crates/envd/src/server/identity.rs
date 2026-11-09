//! Server identity, connection policy, and extension DATA bindings.

use super::prelude::*;
#[cfg(unix)]
use super::runtime::ensure_directory;

/// Identity advertised by every transport served from one environment.
#[derive(Clone, Debug)]
pub struct ServerIdentity {
	/// Canonical document workspace identity.
	pub workspace_id:    Bytes,
	/// Canonical workspace root URI.
	pub root_uri:        Str,
	/// Epoch of the connected document authority.
	pub server_epoch:    Bytes,
	/// Human-readable server build version.
	pub server_version:  Str,
	/// Executable-generation identity of the serving environment.
	pub server_build:    Str,
	/// Sandbox and approval policy the serving environment enforces.
	pub policy:          DaemonPolicy,
	/// Workspace active-content policy fixed at composition.
	pub workspace_trust: WorkspaceTrust,
}

/// Per-connection transport and exact DATA grant bounds.
#[derive(Clone)]
pub(crate) struct ConnectionPolicy {
	pub(in crate::server) retire: Option<CancellationToken>,
	pub(in crate::server) grants: Grants,
	pub(in crate::server) host:   Option<HostKey>,
}

impl ConnectionPolicy {
	pub(in crate::server) fn in_process() -> Self {
		Self { retire: None, grants: Grants::all(), host: None }
	}

	/// Grants owner-local lifecycle traffic while retaining DATA phase checks.
	pub(crate) fn external(retire: Option<CancellationToken>) -> Self {
		Self { retire, grants: Grants::all(), host: None }
	}

	/// Restricts an extension-host connection to explicitly granted, reachable
	/// DATA capabilities.
	pub(crate) fn extension<I, S>(host: HostKey, grants: I) -> Self
	where
		I: IntoIterator<Item = S>,
		S: AsRef<str>,
	{
		Self { retire: None, grants: Grants::supported(grants), host: Some(host) }
	}
}

pub(in crate::server) struct AcceptedHello {
	pub(in crate::server) grants:        Grants,
	pub(in crate::server) capabilities:  BTreeSet<Str>,
	pub(in crate::server) props:         Option<ValueMap>,
	pub(in crate::server) approval_mode: Option<ApprovalMode>,
}

pub(in crate::server) fn approval_mode_from_wire(value: i32) -> Result<Option<ApprovalMode>, i32> {
	match pb::ApprovalMode::try_from(value).map_err(|_| value)? {
		pb::ApprovalMode::Unspecified => Ok(None),
		pb::ApprovalMode::AlwaysAsk => Ok(Some(ApprovalMode::AlwaysAsk)),
		pb::ApprovalMode::Write => Ok(Some(ApprovalMode::Write)),
		pb::ApprovalMode::Yolo => Ok(Some(ApprovalMode::Yolo)),
	}
}

/// One extension host's isolated DATA listener identity and grants.
#[derive(Debug)]
#[doc(hidden)]
pub struct ExtensionDataBinding {
	pub(in crate::server) key:      HostKey,
	pub(in crate::server) path:     PathBuf,
	pub(in crate::server) grants:   Grants,
	#[cfg(unix)]
	pub(in crate::server) prepared: Option<PreparedEndpoint>,
}

/// A bound, published extension DATA socket awaiting its server.
///
/// Owns the published path from the moment it is bound: dropping it at any
/// point (an unserved binding, a serve future dropped before its first poll)
/// removes the socket, and the server inherits the same guard when it takes
/// the listener over.
#[cfg(unix)]
#[derive(Debug)]
pub(in crate::server) struct PreparedEndpoint {
	pub(in crate::server) listener: std::os::unix::net::UnixListener,
	pub(in crate::server) guard:    UnixSocketPathGuard,
}

impl ExtensionDataBinding {
	/// Derives the deterministic owner-local socket path and the exact
	/// built-in extension DATA grant set for `key`.
	#[allow(dead_code, reason = "called from server::tests; production uses scoped")]
	pub(crate) fn built_in(
		state_dir: &Path,
		key: HostKey,
		session_id: &str,
		session_generation: u64,
	) -> Self {
		Self::scoped(
			state_dir,
			key,
			session_id,
			session_generation,
			Grants::supported([
				"env.doc.read",
				"env.doc.write",
				"env.fs.read",
				"env.fs.write",
				"env.exec",
				"env.process",
				"env.blob",
				"env.search",
				"env.lsp",
				"env.net",
				"env.workspace.snapshot",
				"env.worktree",
			]),
		)
	}

	/// Derives one private endpoint carrying only the manifest-derived grants
	/// admitted for `key`.
	pub fn scoped(
		state_dir: &Path,
		key: HostKey,
		session_id: &str,
		session_generation: u64,
		grants: Grants,
	) -> Self {
		#[cfg(unix)]
		let path = omp_env::project_state::extension_socket(
			state_dir,
			key.layer().as_str(),
			key.tier().as_str(),
			key.extension().as_str(),
			session_id,
			session_generation,
		);
		#[cfg(not(unix))]
		let path = {
			let mut hasher = Hash32::hasher();
			hasher.update(b"omp/extension-data-binding/v1");
			hasher.update((session_id.len() as u64).to_le_bytes());
			hasher.update(session_id.as_bytes());
			hasher.update(session_generation.to_le_bytes());
			for field in key.fields() {
				hasher.update((field.len() as u64).to_le_bytes());
				hasher.update(field.as_bytes());
			}
			PathBuf::from(format!("extension-data-{}", hasher.finalize().to_hex()))
		};
		Self {
			key,
			path,
			grants,
			#[cfg(unix)]
			prepared: None,
		}
	}

	/// Returns the socket path passed only to this binding's child.
	pub fn path(&self) -> &Path {
		&self.path
	}

	/// Materializes the generated socket inode before sandbox policy
	/// compilation.
	///
	/// The real DATA server takes over this listener after the extension host
	/// has been admitted, preserving the inode granted to Linux sandboxes.
	#[cfg(unix)]
	pub fn prepare_endpoint(&mut self) -> io::Result<()> {
		use std::os::unix::fs::PermissionsExt as _;

		let parent = self.path.parent().ok_or_else(|| {
			io::Error::new(io::ErrorKind::InvalidInput, "extension DATA socket has no parent")
		})?;
		ensure_directory(parent)?;
		let listener = std::os::unix::net::UnixListener::bind(&self.path)?;
		// Own the published path before anything else can fail: an error below,
		// or a later drop of this binding or of the serve future holding it,
		// removes exactly this inode and never a competing daemon's socket.
		let guard = UnixSocketPathGuard::new(self.path.clone(), &fs::symlink_metadata(&self.path)?);
		fs::set_permissions(&self.path, fs::Permissions::from_mode(0o600))?;
		listener.set_nonblocking(true)?;
		self.prepared = Some(PreparedEndpoint { listener, guard });
		Ok(())
	}

	/// Returns the exact grants enforced for this listener.
	#[allow(dead_code, reason = "called from server::tests; production reads the field")]
	pub(crate) const fn grants(&self) -> &Grants {
		&self.grants
	}

	/// Returns the immutable connection policy carried by this listener.
	pub(crate) fn policy(&self) -> ConnectionPolicy {
		ConnectionPolicy::extension(self.key.clone(), self.grants.iter())
	}
}

#[cfg(unix)]
#[derive(Debug)]
pub(in crate::server) struct UnixSocketPathGuard {
	path: PathBuf,
	dev:  u64,
	ino:  u64,
}

#[cfg(unix)]
impl UnixSocketPathGuard {
	pub(in crate::server) fn new(path: PathBuf, metadata: &fs::Metadata) -> Self {
		use std::os::unix::fs::MetadataExt as _;

		Self { path, dev: metadata.dev(), ino: metadata.ino() }
	}

	/// Whether `metadata` describes the inode this guard owns.
	pub(in crate::server) fn owns(&self, metadata: &fs::Metadata) -> bool {
		use std::os::unix::fs::MetadataExt as _;

		metadata.dev() == self.dev && metadata.ino() == self.ino
	}
}

#[cfg(unix)]
impl Drop for UnixSocketPathGuard {
	fn drop(&mut self) {
		if let Ok(metadata) = fs::symlink_metadata(&self.path)
			&& self.owns(&metadata)
		{
			let _ = fs::remove_file(&self.path);
		}
	}
}
