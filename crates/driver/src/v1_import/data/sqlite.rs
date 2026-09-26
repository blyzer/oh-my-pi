//! Read-only access to v1 SQLite databases, and whole-database copies.
//!
//! Opening a database in place, even read-only, can create `-wal` / `-shm`
//! files beside it. A v1 database is therefore opened through an
//! `immutable=1` URI, which never touches its directory. SQLite ignores a WAL
//! under `immutable`, so a database whose `-wal` still holds pages is read
//! from a private copy of the database and its WAL in a temporary directory,
//! where recovery replays them and a checkpoint folds them into the copy.

use std::{
	ffi::OsString,
	fs, io,
	path::{Path, PathBuf},
	time::Duration,
};

use rusqlite::{Connection, OpenFlags};

use super::DataImportError;

/// One v1 database, readable without writing beside it.
pub(super) struct V1Database {
	/// The v1 database file.
	path:      PathBuf,
	/// The immutable URI every connection opens (or `ATTACH`es).
	uri:       String,
	/// The private snapshot `uri` names instead of the v1 file, if any.
	_snapshot: Option<tempfile::TempDir>,
}

impl V1Database {
	/// Prepares read access to the v1 database at `path`.
	pub(super) fn open(path: &Path) -> Result<Self, DataImportError> {
		let wal = sibling(path, "-wal");
		let pending = fs::metadata(&wal).is_ok_and(|metadata| metadata.len() > 0);
		if !pending {
			let uri = immutable_uri(path).map_err(DataImportError::read(path))?;
			return Ok(Self { path: path.to_owned(), uri, _snapshot: None });
		}
		let directory = tempfile::Builder::new()
			.prefix("omp-v1-import-")
			.tempdir()
			.map_err(DataImportError::write(&std::env::temp_dir()))?;
		let copy = directory.path().join("snapshot.db");
		fs::copy(path, &copy).map_err(DataImportError::read(path))?;
		fs::copy(&wal, sibling(&copy, "-wal")).map_err(DataImportError::read(&wal))?;
		{
			let recovered = Connection::open(&copy).map_err(DataImportError::sqlite(path))?;
			recovered
				.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))
				.and_then(|()| recovered.pragma_update(None, "journal_mode", "DELETE"))
				.map_err(DataImportError::sqlite(path))?;
		}
		let uri = immutable_uri(&copy).map_err(DataImportError::read(path))?;
		Ok(Self { path: path.to_owned(), uri, _snapshot: Some(directory) })
	}

	/// The v1 database file.
	pub(super) fn path(&self) -> &Path {
		&self.path
	}

	/// The URI to `ATTACH` on a connection opened with URI names enabled.
	pub(super) fn uri(&self) -> &str {
		&self.uri
	}

	/// A read-only connection to the database.
	pub(super) fn connect(&self) -> Result<Connection, DataImportError> {
		let connection = Connection::open_with_flags(
			&self.uri,
			OpenFlags::SQLITE_OPEN_READ_ONLY
				| OpenFlags::SQLITE_OPEN_URI
				| OpenFlags::SQLITE_OPEN_NO_MUTEX,
		)
		.map_err(DataImportError::sqlite(&self.path))?;
		connection
			.busy_timeout(Duration::from_secs(2))
			.map_err(DataImportError::sqlite(&self.path))?;
		Ok(connection)
	}

	/// Writes a consistent copy of the whole database to the absent `target`
	/// (`VACUUM INTO` a sibling temporary, then rename), keeping its schema and
	/// `user_version` for v2 to migrate on open.
	pub(super) fn copy_to(&self, target: &Path) -> Result<(), DataImportError> {
		if let Some(parent) = target.parent() {
			fs::create_dir_all(parent).map_err(DataImportError::write(parent))?;
		}
		let temporary = sibling(target, &format!(".v1-import-{}", std::process::id()));
		let name = temporary
			.to_str()
			.ok_or_else(|| DataImportError::Write {
				path:   target.to_owned(),
				source: io::ErrorKind::InvalidFilename.into(),
			})?
			.to_owned();
		let _ = fs::remove_file(&temporary);
		let result = self
			.connect()?
			.execute("VACUUM INTO ?1", [name])
			.map_err(DataImportError::sqlite(&self.path))
			.and_then(|_| fs::rename(&temporary, target).map_err(DataImportError::write(target)));
		if result.is_err() {
			let _ = fs::remove_file(&temporary);
		}
		result
	}
}

/// `path` with `suffix` appended to its file name.
fn sibling(path: &Path, suffix: &str) -> PathBuf {
	let mut name = OsString::from(path.as_os_str());
	name.push(suffix);
	PathBuf::from(name)
}

/// The percent-encoded, read-only `file:` URI of an absolute form of `path`.
fn immutable_uri(path: &Path) -> io::Result<String> {
	let absolute = std::path::absolute(path)?;
	let mut uri = url::Url::from_file_path(&absolute)
		.map(String::from)
		.map_err(|()| io::Error::from(io::ErrorKind::InvalidFilename))?;
	uri.push_str("?immutable=1");
	Ok(uri)
}
