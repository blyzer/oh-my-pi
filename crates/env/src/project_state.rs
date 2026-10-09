//! Project-scoped runtime state paths kept outside tool-writable workspaces.
//!
//! Both the environment host and every client derive the same owner-local
//! addresses from a data directory and a project root, so the derivation lives
//! beside the client rather than in any one composition.

use std::{
	env, fmt, fs, io,
	path::{Path, PathBuf},
};

use omp_core::{
	Hash32,
	encoding::{hex, hex::ArrayStr},
};

#[cfg(any(unix, windows))]
use crate::build_id::current;
#[cfg(windows)]
use crate::windows::current_user_pipe_scope;

/// Digest of the sandbox and approval policy one project environment daemon
/// enforces.
///
/// A daemon compiles its command sandbox, its egress broker and its approval
/// posture from the control context it starts under and keeps them for its
/// whole life. The digest keys the environment socket beside the executable
/// generation ([`environment_socket`], under the project's private socket key),
/// so a client whose policy differs reaches a daemon of its own, and every
/// `ServerHello` carries it (`policy_digest`), so a client attaches only to a
/// daemon that enforces the policy it resolved itself. `omp-envd` derives it
/// from a control context.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct DaemonPolicy(Hash32);

impl DaemonPolicy {
	/// Wraps the digest of one resolved policy.
	#[must_use]
	pub const fn new(digest: Hash32) -> Self {
		Self(digest)
	}

	/// Returns the policy digest.
	#[must_use]
	pub const fn digest(&self) -> &Hash32 {
		&self.0
	}

	/// Reads the digest a `ServerHello` carries; `None` unless it is exactly
	/// 32 bytes.
	#[must_use]
	pub fn from_wire(bytes: &[u8]) -> Option<Self> {
		<[u8; 32]>::try_from(bytes)
			.ok()
			.map(|digest| Self(Hash32::new(digest)))
	}

	/// The short form that names the policy in diagnostics: the first eight
	/// digest bytes as sixteen lowercase hexadecimal digits. Socket names never
	/// carry it ([`environment_socket`]).
	#[must_use]
	pub const fn short(&self) -> ArrayStr<8> {
		let [b0, b1, b2, b3, b4, b5, b6, b7, ..] = *self.0.as_bytes();
		hex::encode_n(&[b0, b1, b2, b3, b4, b5, b6, b7])
	}
}

impl fmt::Display for DaemonPolicy {
	fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
		formatter.write_str(self.short().as_str())
	}
}

/// Returns the canonical per-project state directory below an owner's private
/// data directory.
///
/// Canonicalizing the project root gives aliases and symlinked paths one stable
/// state identity.
///
/// # Errors
///
/// Fails when `project_root` cannot be canonicalized.
pub fn directory(data_dir: &Path, project_root: &Path) -> io::Result<PathBuf> {
	let root = fs::canonicalize(project_root)?;
	let digest = Hash32::sum(root.as_os_str().as_encoded_bytes());
	Ok(data_dir
		.join("projects")
		.join(hex::encode_n(digest.as_bytes()).as_str()))
}

/// Returns the project blob store root below a project state directory: the
/// content-addressed store the environment host spills tool output into and
/// resolves `artifact://sha256/<digest>` from.
#[must_use]
pub fn blob_store(state_dir: &Path) -> PathBuf {
	state_dir.join("blobs")
}

/// Name of the project-state bucket that holds sessions whose recorded project
/// directory no longer exists.
///
/// Project buckets are 64-digit hex digests ([`directory`]), so this name
/// never collides with one.
pub const NO_DIRECTORY: &str = "no-directory";

/// Returns the state directory for sessions without an existing project
/// directory: `<data>/projects/no-directory`, beside the digest-named project
/// buckets that session listings and journal GC already walk.
#[must_use]
pub fn no_directory(data_dir: &Path) -> PathBuf {
	data_dir.join("projects").join(NO_DIRECTORY)
}

/// Returns the short owner-local environment socket path for `state_dir` and
/// the daemon `policy`.
///
/// The path is keyed by the running executable's filesystem generation: a
/// rebuilt `omp` binds its own listener immediately while stale-build listeners
/// drain and idle-exit, with no takeover protocol. It is keyed by the policy
/// the daemon enforces as well, so clients whose sandbox or approval
/// configuration differs never share a daemon; each such daemon idle-exits on
/// its own. The document socket stays build- and policy-stable because its
/// authority must remain singular per project.
///
/// The policy enters the name only through a keyed digest under the project's
/// private socket key ([`SOCKET_KEY_FILE`] in `state_dir`, created on first
/// use), because the policy digest covers sandbox values such as injected
/// environment variables, hosts and paths, and other local users can list
/// `/tmp` and confirm guesses against an unkeyed digest.
///
/// # Errors
///
/// Fails when the socket key can be neither read nor created in `state_dir`.
#[cfg(unix)]
pub fn environment_socket(state_dir: &Path, policy: &DaemonPolicy) -> io::Result<PathBuf> {
	Ok(socket_path(state_dir, &environment_kind(state_dir, policy)?))
}

/// Returns the deterministic current-user environment named pipe for the
/// daemon `policy`.
///
/// The executable-generation key lets rebuilt owners bind immediately while
/// older listeners drain independently, and the policy key, a keyed digest
/// under the project's private socket key, gives every distinct sandbox and
/// approval configuration its own owner without naming it to other users.
///
/// # Errors
///
/// Fails when the socket key can be neither read nor created in `state_dir`.
#[cfg(windows)]
pub fn environment_socket(state_dir: &Path, policy: &DaemonPolicy) -> io::Result<PathBuf> {
	Ok(windows_pipe_path(state_dir, &environment_kind(state_dir, policy)?))
}

/// Name of the file in a project state directory that holds the random key
/// under which environment socket names carry the daemon policy.
pub const SOCKET_KEY_FILE: &str = "env-socket.key";

/// Separates the socket-name policy key from every other SHA-256 use.
#[cfg(any(unix, windows))]
const SOCKET_POLICY_DOMAIN: &[u8] = b"omp/environment-socket-policy/v1\0";

/// The environment endpoint kind: executable generation, then the daemon
/// policy keyed by the project's socket key.
#[cfg(any(unix, windows))]
fn environment_kind(state_dir: &Path, policy: &DaemonPolicy) -> io::Result<String> {
	let build = current();
	let build = if build.is_empty() {
		"unknown"
	} else {
		&build[..8]
	};
	let mut keyed = Hash32::hasher();
	keyed.update(SOCKET_POLICY_DOMAIN);
	keyed.update(socket_key(state_dir)?);
	keyed.update(policy.digest().as_bytes());
	let [b0, b1, b2, b3, b4, b5, b6, b7, ..] = *keyed.finalize().as_bytes();
	Ok(format!("{build}-{}-env", hex::encode_n(&[b0, b1, b2, b3, b4, b5, b6, b7])))
}

/// Reads the project's socket key, creating it on first use.
///
/// A new key is written whole to a private staging file and hard-linked into
/// place, which fails rather than replaces when another process published one
/// first; that key is then read. A key file of the wrong length is replaced.
#[cfg(any(unix, windows))]
fn socket_key(state_dir: &Path) -> io::Result<[u8; 32]> {
	use rand::RngExt as _;

	let path = state_dir.join(SOCKET_KEY_FILE);
	let replace = match fs::read(&path) {
		Ok(bytes) => match <[u8; 32]>::try_from(bytes.as_slice()) {
			Ok(key) => return Ok(key),
			Err(_) => true,
		},
		Err(error) if error.kind() == io::ErrorKind::NotFound => false,
		Err(error) => return Err(error),
	};
	let key: [u8; 32] = rand::rng().random();
	let staged = state_dir.join(format!("{SOCKET_KEY_FILE}.{}", omp_core::Ulid::generate()));
	write_private(&staged, &key)?;
	let published = if replace {
		fs::rename(&staged, &path)
	} else {
		fs::hard_link(&staged, &path)
	};
	let _ = fs::remove_file(&staged);
	match published {
		Ok(()) => Ok(key),
		Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
			<[u8; 32]>::try_from(fs::read(&path)?.as_slice()).map_err(|_| {
				io::Error::new(io::ErrorKind::InvalidData, "the project socket key is not 32 bytes")
			})
		},
		Err(error) => Err(error),
	}
}

/// Writes `contents` to a new file at `path` that only its owner can read.
#[cfg(any(unix, windows))]
fn write_private(path: &Path, contents: &[u8]) -> io::Result<()> {
	use std::io::Write as _;

	let mut options = fs::OpenOptions::new();
	options.write(true).create_new(true);
	#[cfg(unix)]
	{
		use std::os::unix::fs::OpenOptionsExt as _;
		options.mode(0o600);
	}
	let mut file = options.open(path)?;
	file.write_all(contents)?;
	file.sync_all()
}

/// Returns the short owner-local document socket path for `state_dir`.
#[cfg(unix)]
#[must_use]
pub fn document_socket(state_dir: &Path) -> PathBuf {
	socket_path(state_dir, "doc")
}
/// Returns the short owner-local DATA socket for one extension host identity.
///
/// The address is domain-separated by the canonical state directory, exact
/// host key fields, runtime session, and runtime generation while its
/// fixed-size filename remains within every supported Unix `sockaddr_un`.
#[cfg(unix)]
#[must_use]
pub fn extension_socket(
	state_dir: &Path,
	layer: &str,
	tier: &str,
	extension: &str,
	session_id: &str,
	session_generation: u64,
) -> PathBuf {
	let canonical = fs::canonicalize(state_dir).unwrap_or_else(|_| state_dir.to_path_buf());
	let mut digest = Hash32::hasher();
	digest.update(b"omp/extension-data-socket/v1");
	for field in [
		canonical.as_os_str().as_encoded_bytes(),
		layer.as_bytes(),
		tier.as_bytes(),
		extension.as_bytes(),
		session_id.as_bytes(),
	] {
		digest.update((field.len() as u64).to_le_bytes());
		digest.update(field);
	}
	digest.update(session_generation.to_le_bytes());
	unix_socket_path(&digest.finalize(), "ext")
}

/// Returns the deterministic current-user document-authority named pipe.
#[cfg(windows)]
#[must_use]
pub fn document_socket(state_dir: &Path) -> PathBuf {
	windows_pipe_path(state_dir, "doc")
}

/// Returns the base directory holding every Environment-owned worktree.
///
/// `OMP_WORKTREE_DIR` overrides configuration. A relative `configured` path
/// resolves against `data_dir`; an absent one defaults to
/// `<data_dir>/worktrees`.
#[must_use]
pub fn worktree_base(data_dir: &Path, configured: Option<&Path>) -> PathBuf {
	if let Some(path) = env::var_os("OMP_WORKTREE_DIR").filter(|value| !value.is_empty()) {
		return PathBuf::from(path);
	}
	match configured {
		Some(path) if path.is_absolute() => path.to_path_buf(),
		Some(path) => data_dir.join(path),
		None => data_dir.join("worktrees"),
	}
}

/// Resolves the project-specific worktree root used by the Environment.
///
/// `configured` is the persisted `worktree.base` policy of the data directory
/// owning `state_dir`.
#[must_use]
pub fn project_worktree_root(state_dir: &Path, configured: Option<&Path>) -> PathBuf {
	let data_dir = owning_data_dir(state_dir);
	let project_key = state_dir
		.file_name()
		.filter(|name| !name.is_empty())
		.map_or_else(
			|| {
				Hash32::sum(state_dir.as_os_str().as_encoded_bytes())
					.to_hex()
					.to_string()
			},
			|name| name.to_string_lossy().into_owned(),
		);
	worktree_base(&data_dir, configured).join(project_key)
}

/// Recovers the data directory owning a `<data>/projects/<key>` state path.
fn owning_data_dir(state_dir: &Path) -> PathBuf {
	state_dir
		.parent()
		.filter(|parent| parent.file_name().is_some_and(|name| name == "projects"))
		.and_then(Path::parent)
		.map_or_else(|| state_dir.to_path_buf(), Path::to_path_buf)
}

#[cfg(unix)]
fn socket_path(state_dir: &Path, kind: &str) -> PathBuf {
	let canonical = fs::canonicalize(state_dir).unwrap_or_else(|_| state_dir.to_path_buf());
	let digest = Hash32::sum(canonical.as_os_str().as_encoded_bytes());
	unix_socket_path(&digest, kind)
}

#[cfg(unix)]
fn unix_socket_path(digest: &Hash32, kind: &str) -> PathBuf {
	let short: [u8; 16] = digest.as_bytes()[..16]
		.try_into()
		.expect("a SHA-256 digest contains 16 prefix bytes");
	PathBuf::from("/tmp").join(format!(
		"omp-{}-{}-{kind}.sock",
		nix::unistd::geteuid().as_raw(),
		hex::encode_n(&short)
	))
}

#[cfg(windows)]
fn windows_pipe_path(state_dir: &Path, kind: &str) -> PathBuf {
	let owner =
		current_user_pipe_scope().expect("the process has an authenticated Windows user SID");
	let mut digest = Hash32::hasher();
	digest.update(b"omp/project-owner-pipe/v1");
	digest.update(&(owner.len() as u64).to_le_bytes());
	digest.update(owner.as_bytes());
	let state = state_dir.as_os_str().as_encoded_bytes();
	digest.update(&(state.len() as u64).to_le_bytes());
	digest.update(state);
	digest.update(&(kind.len() as u64).to_le_bytes());
	digest.update(kind.as_bytes());
	let digest = hex::encode_n(digest.finalize().as_bytes());
	PathBuf::from(format!(r"\\.\pipe\omp-{}-{kind}", &digest[..32]))
}

#[cfg(test)]
mod policy_tests {
	use omp_core::Hash32;

	use super::DaemonPolicy;

	#[test]
	fn the_wire_digest_round_trips_and_only_32_bytes_are_a_policy() {
		let policy = DaemonPolicy::new(Hash32::sum(b"workspace-write"));
		assert_eq!(DaemonPolicy::from_wire(policy.digest().as_bytes()), Some(policy));
		assert_eq!(DaemonPolicy::from_wire(&[]), None, "an absent digest names no policy");
		assert_eq!(DaemonPolicy::from_wire(&policy.digest().as_bytes()[..16]), None);
		assert_eq!(policy.to_string(), &policy.digest().to_hex().as_str()[..16]);
	}
}

#[cfg(all(test, unix))]
mod tests {
	use std::{fs, mem, os::unix::fs::PermissionsExt as _};

	use omp_core::Hash32;

	use super::{
		DaemonPolicy, SOCKET_KEY_FILE, document_socket, environment_socket, extension_socket,
	};

	fn policy(name: &str) -> DaemonPolicy {
		DaemonPolicy::new(Hash32::sum(name))
	}

	#[test]
	fn socket_paths_fit_the_platform_address_limit() {
		let scratch = tempfile::tempdir().expect("scratch");
		let state_dir = scratch.path().join("long-project-state-segment".repeat(6));
		fs::create_dir_all(&state_dir).expect("state");
		let env = environment_socket(&state_dir, &policy("workspace-write")).expect("socket");
		let docs = document_socket(&state_dir);
		let extension = extension_socket(
			&state_dir,
			"workspace",
			"trusted",
			"fixture.extension",
			"fixture-session",
			u64::MAX,
		);
		// SAFETY: every all-zero bit pattern is valid for libc's sockaddr_un
		// integer fields and fixed-size character array.
		let address: libc::sockaddr_un = unsafe { mem::zeroed() };
		let capacity = address.sun_path.len();

		assert_ne!(env, docs);
		assert_ne!(env, extension);
		assert_ne!(docs, extension);
		assert!(env.as_os_str().as_encoded_bytes().len() < capacity);
		assert!(docs.as_os_str().as_encoded_bytes().len() < capacity);
		assert!(extension.as_os_str().as_encoded_bytes().len() < capacity);
	}

	/// Clients whose sandbox or approval policy differs reach different
	/// daemons of one project, which share its single document authority.
	#[test]
	fn the_daemon_policy_keys_only_the_environment_socket() {
		let scratch = tempfile::tempdir().expect("scratch");
		let state_dir = scratch.path();
		let sandboxed = environment_socket(state_dir, &policy("workspace-write")).expect("socket");
		let unconfined = environment_socket(state_dir, &policy("off")).expect("socket");

		assert_ne!(sandboxed, unconfined);
		assert_eq!(
			sandboxed,
			environment_socket(state_dir, &policy("workspace-write")).expect("socket")
		);
		assert!(
			sandboxed
				.file_name()
				.and_then(|name| name.to_str())
				.is_some_and(|name| name.ends_with("-env.sock")),
			"an environment socket: {}",
			sandboxed.display()
		);
		assert_eq!(document_socket(state_dir), document_socket(state_dir));
	}

	/// The socket name carries the policy only under the project's private
	/// key, so another local user listing `/tmp` cannot confirm a guessed
	/// policy (an injected variable, a host, a path) against it.
	#[test]
	fn the_socket_names_the_policy_only_under_the_private_project_key() {
		let scratch = tempfile::tempdir().expect("scratch");
		let first = scratch.path().join("first");
		let second = scratch.path().join("second");
		fs::create_dir_all(&first).expect("first state");
		fs::create_dir_all(&second).expect("second state");
		let shipped = policy("workspace-write");

		let socket = environment_socket(&first, &shipped).expect("socket");
		let name = socket
			.file_name()
			.and_then(|name| name.to_str())
			.expect("socket name");
		assert!(
			!name.contains(shipped.short().as_str()),
			"the unkeyed policy digest is never in the socket name: {name}"
		);

		let key = first.join(SOCKET_KEY_FILE);
		assert_eq!(fs::read(&key).expect("key").len(), 32);
		assert_eq!(
			fs::metadata(&key)
				.expect("key metadata")
				.permissions()
				.mode() & 0o777,
			0o600,
			"only the owner reads the socket key"
		);
		assert_eq!(fs::read_dir(&first).expect("state").count(), 1, "no staging file is left behind");

		// Two projects of one policy hold different keys: the policy part of
		// their names differs, whatever their state digests.
		let policy_part = |socket: &std::path::Path| {
			socket
				.file_name()
				.and_then(|name| name.to_str())
				.and_then(|name| name.rsplit('-').nth(1))
				.expect("socket name")
				.to_owned()
		};
		let other = environment_socket(&second, &shipped).expect("socket");
		assert_ne!(policy_part(&socket), policy_part(&other));

		// A damaged key is replaced, which moves every name it keyed.
		fs::write(&key, b"short").expect("damage the key");
		let rekeyed = environment_socket(&first, &shipped).expect("socket");
		assert_ne!(rekeyed, socket);
		assert_eq!(fs::read(&key).expect("key").len(), 32);
		assert_eq!(rekeyed, environment_socket(&first, &shipped).expect("socket"));
	}

	/// Concurrent first uses agree on one key, so they agree on one socket.
	#[test]
	fn concurrent_first_uses_publish_one_key() {
		let scratch = tempfile::tempdir().expect("scratch");
		let state_dir = scratch.path().to_path_buf();
		let shipped = policy("workspace-write");
		let sockets = std::thread::scope(|scope| {
			// Every worker starts before any is joined, so their first uses race.
			let mut workers = Vec::with_capacity(8);
			for _ in 0..8 {
				workers.push(scope.spawn(|| environment_socket(&state_dir, &shipped).expect("socket")));
			}
			workers
				.into_iter()
				.map(|worker| worker.join().expect("worker"))
				.collect::<Vec<_>>()
		});
		assert!(sockets.windows(2).all(|pair| pair[0] == pair[1]), "{sockets:?}");
		assert_eq!(fs::read_dir(&state_dir).expect("state").count(), 1);
	}
}

#[cfg(all(test, windows))]
mod windows_tests {
	use omp_core::Hash32;

	use super::{DaemonPolicy, document_socket, environment_socket};

	fn policy(name: &str) -> DaemonPolicy {
		DaemonPolicy::new(Hash32::sum(name))
	}

	#[test]
	fn pipe_names_are_local_deterministic_and_domain_separated() {
		let scratch = tempfile::tempdir().expect("scratch");
		let state = scratch.path();
		let first = environment_socket(state, &policy("workspace-write")).expect("pipe");
		assert_eq!(first, environment_socket(state, &policy("workspace-write")).expect("pipe"));
		assert_ne!(first, environment_socket(state, &policy("off")).expect("pipe"));
		assert_ne!(first, document_socket(state));
		assert!(first.to_string_lossy().starts_with(r"\\.\pipe\omp-"));
	}

	#[test]
	fn project_identity_changes_the_pipe_name() {
		let scratch = tempfile::tempdir().expect("scratch");
		let first = scratch.path().join("one");
		let second = scratch.path().join("two");
		std::fs::create_dir_all(&first).expect("first state");
		std::fs::create_dir_all(&second).expect("second state");
		assert_ne!(
			environment_socket(&first, &policy("workspace-write")).expect("pipe"),
			environment_socket(&second, &policy("workspace-write")).expect("pipe")
		);
		assert_ne!(document_socket(&first), document_socket(&second));
	}
}
