use std::{
	fs::{self, OpenOptions},
	io::{self, Write as _},
	mem,
	path::{Path, PathBuf},
	thread,
	time::Duration,
};

use rand::RngExt as _;
use thiserror::Error;
use zeroize::{Zeroize as _, Zeroizing};

/// File name used for the per-install placeholder key.
pub const PLACEHOLDER_KEY_FILE: &str = "secret-placeholder.key";
const WINNER_READ_ATTEMPTS: usize = 50;
const WINNER_READ_DELAY: Duration = Duration::from_millis(10);

/// Resolves the native path for the persistent placeholder key: the file
/// under the v2 state root ([`omp_core::dirs::native_directories`]:
/// `OMP_STATE_DIR`, else `$XDG_STATE_HOME/omp`, else `~/.local/state/omp`).
///
/// A v1 key is carried over once by `omp config import-v1`, never read here.
pub fn native_path() -> Result<PathBuf, SecretKeyError> {
	let home = omp_core::dirs::home_dir().ok_or(SecretKeyError::MissingHome)?;
	Ok(path_in(&omp_core::dirs::native_directories(&home).state))
}

/// The placeholder key file under a state root.
#[must_use]
pub fn path_in(state_dir: &Path) -> PathBuf {
	state_dir.join(PLACEHOLDER_KEY_FILE)
}

/// Loads the native key without creating a file.
pub fn read_without_create() -> Result<Option<String>, SecretKeyError> {
	read_at(&native_path()?)
}

/// Loads the native key or exclusively creates it.
pub fn load_or_create() -> Result<String, SecretKeyError> {
	load_or_create_at(&native_path()?)
}

/// Loads a key at `path` without creating it.
pub fn read_at(path: &Path) -> Result<Option<String>, SecretKeyError> {
	read_once(path, true)
}

/// Loads a key at `path`, or creates one with mode 0600 and converges with
/// racing creators.
pub fn load_or_create_at(path: &Path) -> Result<String, SecretKeyError> {
	match read_once(path, true) {
		Ok(Some(existing)) => return Ok(existing),
		Ok(None) => {},
		Err(SecretKeyError::InvalidKey { .. }) => return read_winner(path),
		Err(error) => return Err(error),
	}
	let mut random = Zeroizing::new(rand::rng().random::<[u8; 32]>());
	let mut encoded = Zeroizing::new(omp_core::base64_url::encode_raw(&*random).into_string());
	random.zeroize();
	if create_exclusive(path, &encoded)? {
		Ok(mem::take(&mut *encoded))
	} else {
		read_winner(path)
	}
}

/// What [`adopt_at`] found at its path.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Adoption {
	/// The key was installed.
	Installed,
	/// A key was already there and was kept.
	Kept {
		/// Whether the kept key is the offered one.
		same: bool,
	},
}

/// Installs `key`, which must satisfy [`is_valid_key`], at `path` with mode
/// 0600 unless a key is already there. An existing key is never replaced.
pub fn adopt_at(path: &Path, key: &str) -> Result<Adoption, SecretKeyError> {
	let kept = |existing: String| Adoption::Kept { same: existing == key };
	if let Some(existing) = read_once(path, true)? {
		return Ok(kept(existing));
	}
	if create_exclusive(path, key)? {
		Ok(Adoption::Installed)
	} else {
		read_winner(path).map(kept)
	}
}

/// Exclusively creates `path` holding `key`; `false` when another creator
/// already made it.
fn create_exclusive(path: &Path, key: &str) -> Result<bool, SecretKeyError> {
	let parent = path
		.parent()
		.filter(|parent| !parent.as_os_str().is_empty())
		.ok_or_else(|| SecretKeyError::NoParent { path: path.to_path_buf() })?;
	fs::create_dir_all(parent).map_err(|source| SecretKeyError::Io {
		operation: "create key directory",
		path: parent.to_path_buf(),
		source,
	})?;
	match open_exclusive(path) {
		Ok(mut file) => {
			file
				.write_all(key.as_bytes())
				.map_err(|source| SecretKeyError::Io {
					operation: "write placeholder key",
					path: path.to_path_buf(),
					source,
				})?;
			file.sync_all().map_err(|source| SecretKeyError::Io {
				operation: "sync placeholder key",
				path: path.to_path_buf(),
				source,
			})?;
			Ok(true)
		},
		Err(source) if source.kind() == io::ErrorKind::AlreadyExists => Ok(false),
		Err(source) => Err(SecretKeyError::Io {
			operation: "exclusively create placeholder key",
			path: path.to_path_buf(),
			source,
		}),
	}
}

fn open_exclusive(path: &Path) -> io::Result<fs::File> {
	let mut options = OpenOptions::new();
	options.write(true).create_new(true);
	#[cfg(unix)]
	{
		use std::os::unix::fs::OpenOptionsExt as _;
		options.mode(0o600);
	}
	options.open(path)
}

fn read_winner(path: &Path) -> Result<String, SecretKeyError> {
	for attempt in 0..WINNER_READ_ATTEMPTS {
		if attempt > 0 {
			thread::sleep(WINNER_READ_DELAY);
		}
		match read_once(path, false) {
			Ok(Some(key)) => return Ok(key),
			Ok(None) => {},
			Err(SecretKeyError::InvalidKey { .. }) => {},
			Err(error) => return Err(error),
		}
	}
	Err(SecretKeyError::WinnerUnavailable { path: path.to_path_buf() })
}

fn read_once(path: &Path, reject_invalid: bool) -> Result<Option<String>, SecretKeyError> {
	let bytes = match fs::read(path) {
		Ok(bytes) => Zeroizing::new(bytes),
		Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(None),
		Err(source) => {
			return Err(SecretKeyError::Io {
				operation: "read placeholder key",
				path: path.to_path_buf(),
				source,
			});
		},
	};
	validate_permissions(path)?;
	let value = str::from_utf8(&bytes).ok().map(str::trim);
	if let Some(value) = value.filter(|value| is_valid_key(value)) {
		return Ok(Some(value.to_owned()));
	}
	if !reject_invalid && bytes.iter().all(u8::is_ascii_whitespace) {
		return Ok(None);
	}
	Err(SecretKeyError::InvalidKey { path: path.to_path_buf() })
}

/// Whether `value` is one 256-bit base64url key, the only form v1 and v2
/// write.
#[must_use]
pub fn is_valid_key(value: &str) -> bool {
	if value.len() != 43
		|| !value
			.bytes()
			.all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
	{
		return false;
	}
	omp_core::base64_url::decode_raw(value)
		.into_vec()
		.is_ok_and(|mut bytes| {
			let valid = bytes.len() == 32;
			bytes.zeroize();
			valid
		})
}

#[cfg(unix)]
fn validate_permissions(path: &Path) -> Result<(), SecretKeyError> {
	use std::os::unix::fs::PermissionsExt as _;
	let mode = fs::metadata(path)
		.map_err(|source| SecretKeyError::Io {
			operation: "inspect placeholder key permissions",
			path: path.to_path_buf(),
			source,
		})?
		.permissions()
		.mode()
		& 0o777;
	if mode != 0o600 {
		return Err(SecretKeyError::InsecurePermissions { path: path.to_path_buf(), mode });
	}
	Ok(())
}

#[cfg(not(unix))]
fn validate_permissions(_path: &Path) -> Result<(), SecretKeyError> {
	Ok(())
}

/// Persistent placeholder-key failure.
#[derive(Debug, Error)]
pub enum SecretKeyError {
	/// The user home, which anchors the state root, cannot be resolved.
	#[error("HOME is unavailable while resolving the secret placeholder key")]
	MissingHome,
	/// The caller supplied a path without a parent directory.
	#[error("secret placeholder key path has no parent: {path}")]
	NoParent {
		/// Invalid path.
		path: PathBuf,
	},
	/// A filesystem operation failed.
	#[error("failed to {operation} at {path}")]
	Io {
		/// Operation being attempted.
		operation: &'static str,
		/// Affected path.
		path:      PathBuf,
		/// Underlying I/O failure.
		#[source]
		source:    io::Error,
	},
	/// Existing bytes are not one valid 256-bit base64url key.
	#[error("secret placeholder key is invalid: {path}")]
	InvalidKey {
		/// Invalid file path.
		path: PathBuf,
	},
	/// Existing key permissions permit access beyond the owner.
	#[error("secret placeholder key at {path} has mode {mode:o}; expected 600")]
	InsecurePermissions {
		/// Insecure file path.
		path: PathBuf,
		/// Observed Unix permission bits.
		mode: u32,
	},
	/// A racing creator left no readable valid winner.
	#[error("racing creator did not publish a valid secret placeholder key at {path}")]
	WinnerUnavailable {
		/// Winner file path.
		path: PathBuf,
	},
}

#[cfg(test)]
mod tests {
	use std::sync::{Arc, Barrier};

	use super::*;

	#[test]
	fn exclusive_creators_converge_on_one_key() {
		let scratch = tempfile::tempdir().expect("scratch");
		let path = Arc::new(scratch.path().join(PLACEHOLDER_KEY_FILE));
		let barrier = Arc::new(Barrier::new(8));
		let threads: Vec<_> = (0..8)
			.map(|_| {
				let path = Arc::clone(&path);
				let barrier = Arc::clone(&barrier);
				thread::spawn(move || {
					barrier.wait();
					load_or_create_at(&path).expect("creator")
				})
			})
			.collect();
		let keys: Vec<_> = threads
			.into_iter()
			.map(|thread| thread.join().expect("thread"))
			.collect();
		assert!(keys.iter().all(|key| key == &keys[0]));
		assert_eq!(read_at(&path).expect("read").as_deref(), Some(keys[0].as_str()));
		#[cfg(unix)]
		{
			use std::os::unix::fs::PermissionsExt as _;
			assert_eq!(fs::metadata(&*path).expect("metadata").permissions().mode() & 0o777, 0o600);
		}
	}

	/// The key lives under the v2 state root. v1's `~/.omp/agent` copy is
	/// imported once (`omp config import-v1`), never read or created here.
	#[test]
	fn the_native_key_lives_under_the_state_root_not_the_v1_agent_dir() {
		let scratch = tempfile::tempdir().expect("scratch");
		let home = scratch.path().join("home");
		let v1 = home.join(".omp/agent").join(PLACEHOLDER_KEY_FILE);
		adopt_at(&v1, &"A".repeat(43)).expect("v1 key");
		// SAFETY: nextest runs each test in its own process, before anything
		// else reads the environment.
		unsafe {
			std::env::set_var("HOME", &home);
			std::env::remove_var("OMP_STATE_DIR");
			std::env::remove_var("XDG_STATE_HOME");
		}
		let native = native_path().expect("native path");
		assert_eq!(native, home.join(".local/state/omp").join(PLACEHOLDER_KEY_FILE));
		assert_eq!(read_without_create().expect("no native key"), None);
		let created = load_or_create().expect("created");
		assert_ne!(created, "A".repeat(43), "the v1 key is not adopted live");
		assert_eq!(read_at(&native).expect("native").as_deref(), Some(created.as_str()));
		assert_eq!(read_at(&v1).expect("v1").as_deref(), Some("A".repeat(43).as_str()));
	}

	#[test]
	fn adoption_installs_an_owner_only_key_and_never_replaces_one() {
		let scratch = tempfile::tempdir().expect("scratch");
		let path = scratch.path().join("state").join(PLACEHOLDER_KEY_FILE);
		let (first, second) = ("A".repeat(43), "B".repeat(42) + "A");
		assert!(is_valid_key(&first) && !is_valid_key("short"));
		assert_eq!(adopt_at(&path, &first).expect("install"), Adoption::Installed);
		assert_eq!(adopt_at(&path, &first).expect("same"), Adoption::Kept { same: true });
		assert_eq!(adopt_at(&path, &second).expect("other"), Adoption::Kept { same: false });
		assert_eq!(read_at(&path).expect("read").as_deref(), Some(first.as_str()));
		#[cfg(unix)]
		{
			use std::os::unix::fs::PermissionsExt as _;
			assert_eq!(fs::metadata(&path).expect("metadata").permissions().mode() & 0o777, 0o600);
		}
	}

	#[test]
	fn read_without_create_does_not_touch_disk() {
		let scratch = tempfile::tempdir().expect("scratch");
		let path = scratch.path().join(PLACEHOLDER_KEY_FILE);
		assert_eq!(read_at(&path).expect("missing key"), None);
		assert!(!path.exists());
	}
}
