//! A journal-backed [`Session`] in a temporary directory the test owns.
//!
//! [`Session`] needs a journal path, and the directory holding it must outlive
//! the session. Returning only the session from a fixture would drop the
//! [`TempDir`] early, so fixtures used to `keep()` it, which leaked one
//! directory into the shared temp dir per test. [`ScratchSession`] owns both
//! and removes the directory when the test drops it.
//!
//! The unit tests use this module as `crate::test_support`, and the integration
//! tests include the same file with `#[path]`.

use std::ops::{Deref, DerefMut};

use omp_session::{ComponentRegistry, Session};
use tempfile::TempDir;

/// A [`Session`] and the temporary directory holding its journal.
pub struct ScratchSession {
	// Declared first: the session closes its journal before the directory is
	// removed.
	session:    Session,
	_directory: TempDir,
}

impl ScratchSession {
	/// Creates a session whose journal is `name` in a fresh temporary
	/// directory.
	pub fn create(name: &str) -> Self {
		let directory = tempfile::tempdir().expect("temp directory");
		let session = Session::create(directory.path().join(name), ComponentRegistry::standard())
			.expect("create session");
		Self { session, _directory: directory }
	}

	/// Closes the session and returns the directory still holding its
	/// journal, for a test that reopens the journal by path.
	#[allow(dead_code, reason = "only some including test crates reopen a journal")]
	pub fn close(self) -> TempDir {
		let Self { session, _directory: directory } = self;
		drop(session);
		directory
	}
}

impl Deref for ScratchSession {
	type Target = Session;

	fn deref(&self) -> &Session {
		&self.session
	}
}

impl DerefMut for ScratchSession {
	fn deref_mut(&mut self) -> &mut Session {
		&mut self.session
	}
}
