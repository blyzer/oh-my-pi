//! Data and memory steps: prompt history, the install id, Mnemopi stores,
//! `learned.md` lessons, dropped memory backends, and the Claude-format
//! marketplace.
//!
//! Every v1 SQLite database is read without touching its directory: through
//! an `immutable=1` URI, or, when a `-wal` holds pages not yet checkpointed,
//! through a private snapshot of the database and its WAL in a temporary
//! directory ([`sqlite::V1Database`]).

pub(super) mod history;
pub(super) mod install_id;
pub(super) mod marketplace;
pub(super) mod memory;
mod sqlite;

#[cfg(test)]
mod tests;

use std::{
	fmt::Write as _,
	fs, io,
	path::{Path, PathBuf},
};

use omp_core::{Str, StrMut};
use thiserror::Error;

use super::{ImportMode, ImportOutcome};

/// A data or memory step failure. Each carries the path it concerns.
#[derive(Debug, Error)]
pub enum DataImportError {
	/// A v1 file or directory could not be read.
	#[error("could not read {}", path.display())]
	Read {
		/// What was read.
		path:   PathBuf,
		/// Filesystem failure.
		#[source]
		source: io::Error,
	},
	/// A v2 file or directory could not be written.
	#[error("could not write {}", path.display())]
	Write {
		/// What was written.
		path:   PathBuf,
		/// Filesystem failure.
		#[source]
		source: io::Error,
	},
	/// A SQLite database could not be read or written.
	#[error("SQLite failed on {}", path.display())]
	Sqlite {
		/// The database.
		path:   PathBuf,
		/// SQLite failure.
		#[source]
		source: rusqlite::Error,
	},
	/// A JSON registry could not be parsed or encoded.
	#[error("invalid JSON in {}", path.display())]
	Json {
		/// The registry.
		path:   PathBuf,
		/// Decoding failure.
		#[source]
		source: serde_json::Error,
	},
	/// The v1 settings could not be parsed.
	#[error("invalid v1 settings in {}", path.display())]
	Yaml {
		/// The settings file.
		path:   PathBuf,
		/// Decoding failure.
		#[source]
		source: serde_yaml::Error,
	},
	/// The target profile's configuration could not be loaded.
	#[error("could not load the v2 configuration under {}", path.display())]
	Config {
		/// The profile configuration root.
		path:   PathBuf,
		/// Console failure.
		#[source]
		source: omp_con::ConError,
	},
	/// A project's repository could not be inspected for its bank identity.
	#[error("could not inspect the repository at {}", path.display())]
	Repository {
		/// The project directory.
		path:   PathBuf,
		/// VCS failure.
		#[source]
		source: omp_vcs::Error,
	},
	/// A project's Mnemopi store could not be opened or written.
	#[error("could not store lessons for {}", project.display())]
	Memory {
		/// The project directory.
		project: PathBuf,
		/// Memory failure.
		#[source]
		source:  omp_memory::Error,
	},
}

impl DataImportError {
	fn read(path: &Path) -> impl FnOnce(io::Error) -> Self + '_ {
		|source| Self::Read { path: path.to_owned(), source }
	}

	fn write(path: &Path) -> impl FnOnce(io::Error) -> Self + '_ {
		|source| Self::Write { path: path.to_owned(), source }
	}

	fn sqlite(path: &Path) -> impl FnOnce(rusqlite::Error) -> Self + '_ {
		|source| Self::Sqlite { path: path.to_owned(), source }
	}
}

/// What a step reports for data it copies: done, or would be done.
const fn copied(mode: ImportMode) -> ImportOutcome {
	match mode {
		ImportMode::Apply => ImportOutcome::Imported,
		ImportMode::DryRun => ImportOutcome::WouldImport,
	}
}

/// `<count> <noun><tail>`, the noun plural (`s`) unless the count is one,
/// optionally prefixed by `<scope>: `.
fn counted(scope: Option<&Path>, count: usize, noun: &str, tail: &str) -> Str {
	let mut text = StrMut::default();
	if let Some(scope) = scope {
		let _ = write!(text, "{}: ", scope.display());
	}
	let _ = write!(text, "{count} {noun}");
	if count != 1 {
		text.push('s');
	}
	text.push_str(tail);
	text.freeze()
}

/// A path as a report subject.
fn subject(path: &Path) -> Str {
	let mut text = StrMut::default();
	let _ = write!(text, "{}", path.display());
	text.freeze()
}

/// Whether `v1` and `v2` are the same existing file or directory (a v1 XDG
/// root v2 shares).
fn same_file(v1: &Path, v2: &Path) -> bool {
	match (fs::canonicalize(v1), fs::canonicalize(v2)) {
		(Ok(v1), Ok(v2)) => v1 == v2,
		_ => false,
	}
}

/// Copies the tree at `source` to the absent `target`, symlinks as symlinks.
fn copy_tree(source: &Path, target: &Path) -> Result<(), DataImportError> {
	let metadata = fs::symlink_metadata(source).map_err(DataImportError::read(source))?;
	if metadata.file_type().is_symlink() {
		let link = fs::read_link(source).map_err(DataImportError::read(source))?;
		return symlink(&link, target).map_err(DataImportError::write(target));
	}
	if metadata.is_file() {
		fs::copy(source, target).map_err(DataImportError::write(target))?;
		return Ok(());
	}
	fs::create_dir_all(target).map_err(DataImportError::write(target))?;
	for entry in fs::read_dir(source).map_err(DataImportError::read(source))? {
		let entry = entry.map_err(DataImportError::read(source))?;
		copy_tree(&entry.path(), &target.join(entry.file_name()))?;
	}
	Ok(())
}

#[cfg(unix)]
fn symlink(link: &Path, target: &Path) -> io::Result<()> {
	std::os::unix::fs::symlink(link, target)
}

#[cfg(windows)]
fn symlink(link: &Path, target: &Path) -> io::Result<()> {
	if link.is_dir() {
		std::os::windows::fs::symlink_dir(link, target)
	} else {
		std::os::windows::fs::symlink_file(link, target)
	}
}
