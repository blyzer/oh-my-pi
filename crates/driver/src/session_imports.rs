//! Which foreign transcripts already have a native journal, derived from the
//! journals themselves.
//!
//! Every session importer (Claude Code, Codex, omp v1) records where a
//! journal came from in `<meta>` before any transcript entry
//! ([`omp_session::import`]): the source format (`import-format`), the
//! source's own session id (`import-source-id`), the transcript's path
//! (`import-source`), the address of its exact bytes (`import-source-blob`,
//! whose SHA-256 digest is the transcript's content digest at import), and
//! its size, modification time, and on Unix and Windows change time
//! ([`import::SourceStamp`]).
//!
//! [`ImportedIndex`] reads that provenance back from every journal of one
//! format under `<data>/projects/*/sessions/` (and any further session
//! directory a caller names), so the journals stay the only record of an
//! import: deleting a journal makes its transcript importable again. Judged
//! against a transcript as it is now, the index tells what earlier imports
//! left ([`PriorImport`]):
//!
//! - [current](PriorImport::Current): a journal holds the transcript as it is
//!   now, and importing reopens it;
//! - [changed](PriorImport::Changed): the transcript changed since every import
//!   of this file, and importing converts it again into a fresh journal beside
//!   the earlier one, which journals being append-only never touches;
//! - [other file](PriorImport::OtherFile): only another file carrying the same
//!   session id was imported (a copy, or a session its tool moved or resumed
//!   into a new file), and this file converts into a journal of its own.
//!
//! Imports are told apart by the transcript path they recorded, compared
//! canonicalized. Telling whether a transcript changed does not read it while
//! it is unchanged: a journal of this very file whose recorded stamp still
//! matches the file's is current without the transcript being digested.
//!
//! A journal that recorded no source id (a transcript whose records name
//! none) is keyed by its source file's stem, the id a listing falls back to
//! for such a transcript.

use std::{
	fs, io,
	path::{Path, PathBuf},
};

use omp_core::{FastHashMap, Hash32, Str};
use omp_journal::Journal;
use omp_session::import;
use smallvec::SmallVec;
use thiserror::Error;

/// The journal an earlier import of a transcript left, and whether the
/// transcript changed since.
///
/// A source tool names a session by its id, and two transcript files can
/// carry the same id (a copy, or a session the tool moved or continued in a
/// new file). Each file's imports are told apart by the transcript path they
/// recorded (`import-source`, compared canonicalized).
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PriorImport {
	/// The newest journal imported from the transcript as it is now (a
	/// journal of this file, or one whose recorded digest matches it):
	/// importing reopens it.
	Current(PathBuf),
	/// The transcript changed since every import of this file (its digest
	/// matches no journal's `import-source-blob`); this is that file's newest
	/// earlier journal, which stays. Importing converts the transcript again
	/// into a fresh journal, which supersedes it.
	Changed(PathBuf),
	/// This file was never imported, but another file carrying the same
	/// session id was; this is that file's newest journal, which is left
	/// alone. Importing converts this file into a journal of its own.
	OtherFile(PathBuf),
}

/// A directory or transcript the index could not read.
#[derive(Debug, Error)]
#[error("could not read {}", path.display())]
pub struct ImportIndexError {
	/// What was read.
	pub path:   PathBuf,
	/// Typed filesystem failure.
	#[source]
	pub source: io::Error,
}

fn read_error(path: &Path) -> impl FnOnce(io::Error) -> ImportIndexError + '_ {
	move |source| ImportIndexError { path: path.to_owned(), source }
}

/// One journal an import of the index's format wrote.
#[derive(Clone, Debug)]
struct IndexedJournal {
	path:   PathBuf,
	/// The transcript it was imported from ([`import::IMPORT_SOURCE`]),
	/// canonicalized when that file still exists.
	source: Option<PathBuf>,
	/// The transcript's digest at import
	/// ([`import::ImportOrigin::source_digest`]), when recorded.
	digest: Option<Hash32>,
	/// The transcript's size, modification time, and (on Unix and Windows)
	/// change time at import ([`import::ImportOrigin::source_stamp`]), when
	/// recorded.
	stamp:  Option<import::SourceStamp>,
}

impl IndexedJournal {
	/// Orders journals by their ULID file stem, oldest first.
	fn age(&self) -> Option<&std::ffi::OsStr> {
		self.path.file_name()
	}

	/// Whether this journal was imported from the transcript at `source`
	/// (canonical), or recorded no source to tell otherwise.
	fn imported_from(&self, source: &Path) -> bool {
		self
			.source
			.as_deref()
			.is_none_or(|recorded| recorded == source)
	}

	/// Whether this journal holds the transcript at `source` (canonical) as
	/// it is now, whose digest is `digest`: the digest it recorded matches,
	/// or it recorded none and came from that file.
	fn holds(&self, source: &Path, digest: Option<Hash32>) -> bool {
		match self.digest {
			Some(recorded) => Some(recorded) == digest,
			None => self.imported_from(source),
		}
	}
}

/// The newest of `journals`: the greatest ULID.
fn newest<'j>(journals: impl Iterator<Item = &'j IndexedJournal>) -> Option<&'j IndexedJournal> {
	journals.max_by(|left, right| left.age().cmp(&right.age()))
}

/// `path` canonicalized, or as given when it cannot be (it is gone): how
/// transcript paths compare, whichever spelling (a symlinked data root,
/// macOS `/var` for `/private/var`) an import recorded or a listing found.
fn canonical(path: &Path) -> PathBuf {
	fs::canonicalize(path).unwrap_or_else(|_| path.to_owned())
}

/// The v2 journals imported from one foreign format, keyed by the source's
/// session id.
///
/// Derived, never stored: [`Self::scan`] reads the import provenance each
/// journal under `<data>/projects/*/sessions/` records in its first patches
/// ([`omp_session::import::import_origin`]); see the [module docs](self).
#[derive(Clone, Debug, Default)]
pub struct ImportedIndex {
	/// The [`import::IMPORT_FORMAT`] indexed.
	format:  Str,
	by_id:   FastHashMap<Str, SmallVec<IndexedJournal, 1>>,
	/// Session directories already read (canonical), so naming one twice,
	/// or one inside a project bucket, indexes its journals once.
	scanned: SmallVec<PathBuf, 4>,
}

impl ImportedIndex {
	/// Scans every project bucket under `data_dir` for journals an import of
	/// `format` ([`import::IMPORT_FORMAT`]: `claude`, `codex`,
	/// [`import::OMP1_FORMAT`]) wrote, keeping every journal of a session with
	/// the transcript it was imported from and that transcript's recorded
	/// digest and stamp.
	///
	/// # Errors
	///
	/// Returns why a bucket directory could not be listed. An unreadable or
	/// invalid journal is skipped.
	pub fn scan(data_dir: &Path, format: &str) -> Result<Self, ImportIndexError> {
		let mut index = Self { format: Str::new(format), ..Self::default() };
		let projects = data_dir.join("projects");
		let buckets = match fs::read_dir(&projects) {
			Ok(entries) => entries,
			Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(index),
			Err(source) => return Err(ImportIndexError { path: projects, source }),
		};
		for bucket in buckets {
			let sessions = bucket
				.map_err(read_error(&projects))?
				.path()
				.join("sessions");
			index.scan_sessions(&sessions)?;
		}
		Ok(index)
	}

	/// Also indexes the journals directly in `sessions`, a session directory
	/// outside the project buckets (a chat's explicit session directory). A
	/// missing directory, or one already scanned, adds nothing.
	///
	/// # Errors
	///
	/// Returns why the directory could not be listed.
	pub fn scan_sessions(&mut self, sessions: &Path) -> Result<(), ImportIndexError> {
		let entries = match fs::read_dir(sessions) {
			Ok(entries) => entries,
			Err(error)
				if matches!(error.kind(), io::ErrorKind::NotFound | io::ErrorKind::NotADirectory) =>
			{
				return Ok(());
			},
			Err(source) => return Err(ImportIndexError { path: sessions.to_owned(), source }),
		};
		let directory = canonical(sessions);
		if self.scanned.contains(&directory) {
			return Ok(());
		}
		self.scanned.push(directory);
		for entry in entries {
			let journal = entry.map_err(read_error(sessions))?.path();
			// Hidden files are journals still being staged.
			let visible = journal
				.file_name()
				.and_then(|name| name.to_str())
				.is_some_and(|name| !name.starts_with('.'));
			if visible
				&& journal.extension().and_then(|value| value.to_str())
					== Some(omp_journal::FILE_EXTENSION)
			{
				self.learn(journal);
			}
		}
		Ok(())
	}

	/// Indexes `journal` when an import of this index's format wrote it; an
	/// unreadable journal, or one no such import wrote, is left out.
	pub fn learn(&mut self, journal: PathBuf) {
		let entries = match Journal::scan_prefix(&journal, import::PROVENANCE_PREFIX_BYTES) {
			Ok(entries) => entries,
			Err(error) => {
				tracing::debug!(
					journal = %journal.display(),
					error = &error as &dyn std::error::Error,
					"skipping unreadable journal while indexing imports"
				);
				return;
			},
		};
		let Some(origin) = import::import_origin(&entries) else {
			return;
		};
		if origin.format != self.format {
			return;
		}
		let source = origin
			.source
			.as_deref()
			.map(|source| canonical(Path::new(source)));
		let Some(id) = origin.source_id.or_else(|| {
			let stem = Path::new(origin.source.as_deref()?).file_stem()?.to_str()?;
			Some(Str::new(stem))
		}) else {
			return;
		};
		self.by_id.entry(id).or_default().push(IndexedJournal {
			path: journal,
			source,
			digest: origin.source_digest,
			stamp: origin.source_stamp,
		});
	}

	/// The newest journal an import made for session `id`.
	#[must_use]
	pub fn journal(&self, id: &str) -> Option<&Path> {
		newest(self.by_id.get(id)?.iter()).map(|journal| journal.path.as_path())
	}

	/// What earlier imports of session `id` left, judged against
	/// `transcript` as it is now; see [`PriorImport`].
	///
	/// A journal imported from this very file whose recorded size,
	/// modification time, and (on Unix and Windows) change time
	/// ([`import::SourceStamp`]) still match the file's is current without
	/// reading the transcript. Otherwise the transcript is read whole to digest
	/// it, when a journal of `id` recorded a digest to compare with.
	///
	/// # Errors
	///
	/// Returns why the transcript could not be read.
	pub fn prior(
		&self,
		id: &str,
		transcript: &Path,
	) -> Result<Option<PriorImport>, ImportIndexError> {
		let Some(journals) = self.by_id.get(id) else {
			return Ok(None);
		};
		let source = canonical(transcript);
		if let Some(stamp) = fs::metadata(transcript)
			.ok()
			.and_then(|metadata| import::SourceStamp::of(&metadata))
			&& let Some(journal) = newest(journals.iter().filter(|journal| {
				journal.stamp == Some(stamp) && journal.source.as_deref() == Some(&source)
			})) {
			return Ok(Some(PriorImport::Current(journal.path.clone())));
		}
		let digest = if journals.iter().any(|journal| journal.digest.is_some()) {
			Some(transcript_digest(transcript)?)
		} else {
			None
		};
		let holds = |journal: &&IndexedJournal| journal.holds(&source, digest);
		let from_file = |journal: &&IndexedJournal| journal.imported_from(&source);
		Ok(Some(if let Some(current) = newest(journals.iter().filter(holds)) {
			PriorImport::Current(current.path.clone())
		} else if let Some(earlier) = newest(journals.iter().filter(from_file)) {
			PriorImport::Changed(earlier.path.clone())
		} else {
			let Some(other) = newest(journals.iter()) else {
				return Ok(None);
			};
			PriorImport::OtherFile(other.path.clone())
		}))
	}

	/// How many sessions have an imported journal.
	#[must_use]
	pub fn len(&self) -> usize {
		self.by_id.len()
	}

	/// Whether no session has an imported journal.
	#[must_use]
	pub fn is_empty(&self) -> bool {
		self.by_id.is_empty()
	}
}

#[cfg(test)]
thread_local! {
	/// How many transcripts this thread digested ([`transcript_digest`]):
	/// what proves an unchanged transcript is not read.
	pub(crate) static DIGESTS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// The SHA-256 digest of a transcript's exact bytes: what an importer
/// records as its `import-source-blob`.
fn transcript_digest(path: &Path) -> Result<Hash32, ImportIndexError> {
	#[cfg(test)]
	DIGESTS.with(|digests| digests.set(digests.get() + 1));
	let mut file = fs::File::open(path).map_err(read_error(path))?;
	let mut hasher = Hash32::hasher();
	io::copy(&mut file, &mut hasher).map_err(read_error(path))?;
	Ok(hasher.finalize())
}

#[cfg(test)]
mod tests;
