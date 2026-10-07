//! Build identity of the running executable.
//!
//! Project daemons only need to know whether they were launched from the same
//! local executable generation. Content addressability is unnecessary: the
//! client launches its daemon from the same file, while a relink replaces or
//! mutates that file.
//!
//! The identity hashes the executable path and filesystem generation metadata.
//! Its cost is constant in the executable size on every supported platform; it
//! never opens or reads the executable contents.

#[cfg(not(any(unix, windows)))]
use std::time;
use std::{env, fs, io, path::Path, sync::LazyLock};

use omp_core::{Hash32, hex::ArrayStr};

/// Returns the memoized local generation identity of the current executable,
/// or an empty string when its filesystem metadata cannot be read.
///
/// An empty identity means "unknown": callers must never initiate daemon
/// replacement from an unknown identity, and must treat an empty advertised
/// identity as stale only when their own identity is known.
pub fn current() -> &'static str {
	static BUILD_ID: LazyLock<ArrayStr<32>> = LazyLock::new(compute);
	BUILD_ID.as_str()
}

/// Returns whether a daemon advertising `theirs` should be replaced by a
/// client whose identity is `ours`.
///
/// Replacement requires a known local identity; a daemon with an unknown
/// (empty) identity predates build identification and counts as stale.
pub fn is_stale(ours: &str, theirs: &str) -> bool {
	!ours.is_empty() && ours != theirs
}

fn compute() -> ArrayStr<32> {
	env::current_exe()
		.and_then(|executable| of_executable(&executable))
		.unwrap_or_default()
}

/// Returns the local generation identity of the executable at `executable`,
/// hashing the path exactly as spelled.
///
/// [`current`] hashes the path the running process's `current_exe` reports,
/// so the two agree only for that spelling: on macOS the path the process was
/// started from, on Linux its canonical path with every symlink resolved
/// (`/proc/self/exe`). A process that is not that executable uses this to
/// advertise the identity a daemon started from the file expects of a live
/// owner, such as a test harness that hosts the document authority a spawned
/// `omp envd` attaches to. Such a caller canonicalizes the executable path
/// once and uses that one path both to start the daemon and here; the two
/// identities then match on every platform.
///
/// # Errors
///
/// Returns the error reading the file's metadata.
pub fn of_executable(executable: &Path) -> io::Result<ArrayStr<32>> {
	let metadata = fs::metadata(executable)?;
	let mut digest = Hash32::hasher();
	digest.update(b"omp/executable-generation/v1");

	let path = executable.as_os_str().as_encoded_bytes();
	digest.update((path.len() as u64).to_le_bytes());
	digest.update(path);
	digest.update(metadata.len().to_le_bytes());

	#[cfg(unix)]
	{
		use std::os::unix::fs::MetadataExt as _;

		digest.update(metadata.dev().to_le_bytes());
		digest.update(metadata.ino().to_le_bytes());
		digest.update(metadata.mtime().to_le_bytes());
		digest.update(metadata.mtime_nsec().to_le_bytes());
		digest.update(metadata.ctime().to_le_bytes());
		digest.update(metadata.ctime_nsec().to_le_bytes());
	}

	#[cfg(windows)]
	{
		use std::os::windows::fs::MetadataExt as _;

		digest.update(metadata.creation_time().to_le_bytes());
		digest.update(metadata.last_write_time().to_le_bytes());
	}

	#[cfg(not(any(unix, windows)))]
	{
		let (before_epoch, modified) = match metadata.modified()?.duration_since(time::UNIX_EPOCH) {
			Ok(modified) => (false, modified),
			Err(error) => (true, error.duration()),
		};
		digest.update([u8::from(before_epoch)]);
		digest.update(modified.as_secs().to_le_bytes());
		digest.update(modified.subsec_nanos().to_le_bytes());
	}

	Ok(digest.finalize().to_hex())
}

#[cfg(test)]
mod tests {

	use super::*;

	#[test]
	fn current_is_stable_nonempty_hex() {
		let first = current();
		assert_eq!(first, current());
		assert!(!first.is_empty(), "test executable must be identifiable");
		assert_eq!(first.len(), 64);
		assert!(first.bytes().all(|byte| byte.is_ascii_hexdigit()));
	}

	/// Where a child run of the test below writes the identity it computes
	/// for itself.
	const CHILD_REPORT: &str = "OMP_BUILD_ID_TEST_REPORT";

	/// A process started from a path reports [`of_executable`] of the path its
	/// `current_exe` names: the spelled path on macOS, the canonical one on
	/// Linux. Each child is this test binary rerunning this test, which then
	/// only reports.
	#[test]
	fn a_started_process_reports_the_identity_of_its_reported_path() {
		if let Some(report) = env::var_os(CHILD_REPORT) {
			fs::write(report, current()).expect("report the child identity");
			return;
		}
		let scratch = tempfile::tempdir().expect("scratch directory");
		let started_from = |executable: &Path, report: &str| {
			let report = scratch.path().join(report);
			let status = std::process::Command::new(executable)
				.args([
					"--exact",
					"build_id::tests::a_started_process_reports_the_identity_of_its_reported_path",
					"--test-threads=1",
				])
				.env(CHILD_REPORT, &report)
				.stdin(std::process::Stdio::null())
				.stdout(std::process::Stdio::null())
				.stderr(std::process::Stdio::null())
				.status()
				.expect("run the child");
			assert!(status.success(), "the child run failed: {status}");
			fs::read_to_string(&report).expect("the child reported its identity")
		};
		let identity = |executable: &Path| {
			of_executable(executable)
				.expect("executable identity")
				.as_str()
				.to_owned()
		};

		let canonical = fs::canonicalize(env::current_exe().expect("test executable"))
			.expect("canonical test executable");
		let reported = started_from(&canonical, "canonical");
		assert!(!reported.is_empty(), "the child could not identify itself");
		assert_eq!(reported, identity(&canonical));

		#[cfg(any(target_os = "linux", target_os = "macos"))]
		{
			let link = scratch.path().join("linked-test-binary");
			std::os::unix::fs::symlink(&canonical, &link).expect("link the test binary");
			assert_ne!(identity(&link), identity(&canonical), "the spelling is not hashed");
			let expected = if cfg!(target_os = "linux") {
				&canonical
			} else {
				&link
			};
			assert_eq!(started_from(&link, "linked"), identity(expected));
		}
	}

	#[test]
	fn executable_identity_is_stable_and_changes_with_file_generation() {
		let directory = tempfile::tempdir().expect("temporary executable directory");
		let executable = directory.path().join("omp");
		fs::write(&executable, b"first generation").expect("write first generation");

		let first = of_executable(&executable).expect("fingerprint first generation");
		assert_eq!(
			first.as_str(),
			of_executable(&executable)
				.expect("fingerprint unchanged generation")
				.as_str()
		);

		fs::write(&executable, b"replacement executable generation")
			.expect("write replacement generation");
		let replacement = of_executable(&executable).expect("fingerprint replacement generation");
		assert_ne!(first.as_str(), replacement.as_str());
	}

	#[test]
	fn staleness_requires_known_local_identity() {
		assert!(!is_stale("", "abc"));
		assert!(!is_stale("", ""));
		assert!(is_stale("abc", ""));
		assert!(is_stale("abc", "def"));
		assert!(!is_stale("abc", "abc"));
	}
}
