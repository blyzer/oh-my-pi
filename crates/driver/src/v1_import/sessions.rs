//! The `sessions` step and the on-demand conversion behind the resume picker:
//! v1 session transcripts into native `.oms` journals (owner decision #4).
//!
//! # When sessions convert
//!
//! - On demand: the resume picker (`/resume @v1`) lists v1 transcripts from
//!   their headers alone ([`list`]) and converts only the one picked
//!   ([`import_selected`]).
//! - In bulk: `omp config import-v1 --sessions` runs the step with
//!   [`SessionImport::Bulk`], converting every transcript.
//!
//! The automatic first run and a plain `omp config import-v1` run the step
//! with [`SessionImport::OnDemand`]: it reports where sessions wait, scans
//! nothing, and never sets its marker.
//!
//! # Where they land
//!
//! A session lands in the project bucket of its recorded `cwd`
//! (`<data>/projects/<sha256(canonical cwd)>/sessions/<ULID>.oms`), or, when
//! that directory is gone, in the dedicated
//! [`no-directory`](omp_env::project_state::no_directory) bucket
//! ([`ProjectBucket`]). Its subagent transcripts (`<session>/<agentId>.jsonl`)
//! become child journals beside it (`<ULID>.oms`, the name native children
//! use), linked from the parent's `<meta><jobs>` as settled subagents.
//!
//! # Idempotency and pins
//!
//! An imported journal records its provenance in `<meta>` before any
//! transcript entry ([`omp_session::import`]: `import-format omp1`,
//! `import-source-id <v1 id>`, `import-source <v1 transcript>`, and
//! `import-source-blob`, the transcript's exact bytes). That journal is the
//! only record of the import: [`ImportedIndex`] derives "which v1 sessions
//! already have a journal" by reading the provenance prefix of every journal
//! under `<data>/projects/*/sessions/`, so while an imported journal exists,
//! neither the picker nor a bulk run converts its session again (a bulk run
//! reports it [`SkipReason::SessionImported`]), and deleting the journal
//! makes the session importable again. The picker marks such rows
//! ([`V1SessionInfo::imported`]); picking one reopens its journal.
//! Conversion also remaps the session's v1 pin
//! (`<agent>/session-pins.json`, keyed by v1 id) to its ULID in the project's
//! own `session-pins.json`.
//!
//! # Artifacts
//!
//! v1 spilled large tool output into the session's artifact directory
//! (`<session>/<N>.<tool>.log`, v1's `ArtifactManager`) and addressed it as
//! `artifact://<N>`, a per-session counter shared with every subagent of the
//! session. Journaled text keeps those URIs exactly as v1 wrote them. Instead,
//! every file of the artifact directory is copied into the bucket's project
//! blob store ([`omp_env::project_state::blob_store`], where the environment
//! host resolves `artifact://`) and journaled as a `<meta><foreign-artifact>`
//! naming its `artifact://sha256/<digest>` and v1 id
//! ([`omp_session::import::foreign_artifact`]). The environment host's
//! `artifact://` resolver translates a numeric id through that mapping for
//! the session reading it. A subagent's journal carries the entries for the
//! ids its own transcript references. A referenced id with no file is
//! reported ([`ImportedSession::missing_artifacts`], and one
//! [`Attention::ArtifactMissing`] entry per id in a bulk run), never fatal.
//!
//! The JSONL-to-journal conversion itself is the app's session importer
//! (`ForeignFormat::Omp1`, beside the Claude Code and Codex importers),
//! reached through [`V1SessionConverter`]. Nothing under the v1 tree is ever
//! written.

use std::{
	fs, io,
	path::{Path, PathBuf},
	time::{SystemTime, UNIX_EPOCH},
};

use omp_core::{FastHashMap, Str, Ulid, sf};
use omp_journal::{
	Journal,
	blob::{self, BlobRef, BlobStore},
};
use omp_session::import;
use serde::Deserialize;
use serde_json::value::RawValue;
use thiserror::Error;

use super::{
	ImportEntry, ImportError, ImportMode, ImportOutcome, ImportStep, StepContext, V1Item, V1Layout,
	V2Target,
	report::{Attention, SkipReason},
	step::atomic_replace,
};

/// The URI scheme v1 addressed session artifacts by (`artifact://<N>`).
const ARTIFACT_SCHEME: &[u8] = b"artifact://";
/// Pinned session ids: v1 keeps them in the agent directory, v2 in each
/// project's state directory.
const PINS_FILE: &str = "session-pins.json";
/// v1 transcript extension.
const TRANSCRIPT_EXTENSION: &str = "jsonl";
/// Transcripts up to this size are counted exactly when listed; larger ones
/// are read only up to their first user message.
const MAX_EAGER_INDEX_BYTES: u64 = 1024 * 1024;
/// Lines read for a header before giving up on one.
const MAX_HEAD_LINES: usize = 64;
/// Subagent nesting followed below one session.
const MAX_CHILD_DEPTH: u8 = 8;

/// The error a [`V1SessionConverter`] reports, carried as the typed source of
/// [`SessionImportError::Convert`].
pub type ConvertError = Box<dyn std::error::Error + Send + Sync + 'static>;

/// Converts one v1 transcript into a new native journal.
///
/// Implemented by the app's session importer; the driver owns everything
/// around it (locating, placement, children, idempotency, pins, and the
/// atomic publish).
pub trait V1SessionConverter {
	/// Writes a new journal at `conversion.destination` from
	/// `conversion.source` and returns how many messages it imported.
	///
	/// # Errors
	///
	/// Returns why the transcript could not be converted; the caller removes
	/// the partial destination.
	fn convert(&self, conversion: &V1Conversion<'_>) -> Result<usize, ConvertError>;
}

/// One transcript to convert, with everything v1 kept beside it.
#[derive(Clone, Copy, Debug)]
pub struct V1Conversion<'a> {
	/// The v1 transcript.
	pub source:      &'a Path,
	/// The staged journal to create (a hidden sibling of the final one).
	pub destination: &'a Path,
	/// The journal's final id (its ULID file stem), the owner of its
	/// children's jobs.
	pub id:          &'a str,
	/// The v1 session id (the header `id`, else the file name's), which the
	/// journal records as its `import-source-id` provenance: what
	/// [`ImportedIndex`] recognizes the import by.
	pub source_id:   &'a str,
	/// v1's content-addressed blob store, which resolves
	/// `blob:sha256:<hex>` image data.
	pub blobs:       Option<&'a Path>,
	/// v1 artifacts already copied into the project blob store, which the
	/// journal names as `<meta><foreign-artifact>`
	/// ([`omp_session::import::foreign_artifact`]): every file of a session's
	/// artifact directory, or the ones a subagent's transcript references.
	pub artifacts:   &'a [V1Artifact],
	/// Subagent transcripts already converted into child journals, to link
	/// from this journal's `<meta><jobs>`.
	pub children:    &'a [V1ChildJob],
}

/// A converted subagent transcript, as its parent's job links it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct V1ChildJob {
	/// Job id and child journal stem (a fresh ULID).
	pub id:         Str,
	/// Agent class v1 ran (`session_init.agent`), else the v1 agent id.
	pub agent:      Str,
	/// v1 agent id: the transcript's file stem (`0-Scout`).
	pub source_id:  Str,
	/// Start time, Unix milliseconds.
	pub started_ms: u64,
}

/// A v1 artifact file copied into the v2 project blob store.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct V1Artifact {
	/// File name in the v1 artifact directory (`3.bash.log`).
	pub name: Str,
	/// The id v1 resolved `artifact://<id>` to (the file name's numeric
	/// prefix; the first such file in name order when several share one).
	pub id:   Option<u64>,
	/// The copy in the project blob store.
	pub blob: BlobRef,
}

/// A v1 `artifact://<id>` reference whose file was gone at import.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MissingArtifact {
	/// The transcript (the session's or a subagent's) that references it.
	pub transcript: PathBuf,
	/// The v1 artifact id.
	pub id:         u64,
}

/// How a run treats v1 sessions (owner decision #4).
#[derive(Clone, Copy, Default)]
pub enum SessionImport<'a> {
	/// Sessions convert when picked (`/resume @v1`): the step points there,
	/// scans nothing, and leaves its marker unset. The first run and a plain
	/// `omp config import-v1`.
	#[default]
	OnDemand,
	/// Convert every session with this converter
	/// (`omp config import-v1 --sessions`).
	Bulk(&'a dyn V1SessionConverter),
}

/// The project bucket an imported session lands in.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProjectBucket {
	/// The recorded project directory, canonicalized.
	Project(PathBuf),
	/// The recorded directory no longer exists (or none was recorded):
	/// [`omp_env::project_state::no_directory`].
	NoDirectory,
}

impl ProjectBucket {
	/// The bucket for a recorded working directory.
	#[must_use]
	pub fn for_cwd(cwd: Option<&Path>) -> Self {
		cwd.filter(|cwd| cwd.is_absolute())
			.and_then(|cwd| fs::canonicalize(cwd).ok())
			.filter(|cwd| cwd.is_dir())
			.map_or(Self::NoDirectory, Self::Project)
	}

	/// The bucket's project state directory under `data_dir`.
	///
	/// # Errors
	///
	/// Returns the failure to canonicalize a project directory that vanished
	/// meanwhile.
	pub fn state_dir(&self, data_dir: &Path) -> io::Result<PathBuf> {
		match self {
			Self::Project(root) => omp_env::project_state::directory(data_dir, root),
			Self::NoDirectory => Ok(omp_env::project_state::no_directory(data_dir)),
		}
	}
}

/// Lightweight metadata of one v1 transcript, for the picker.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct V1SessionInfo {
	/// v1 session id (header `id`).
	pub id:            Str,
	/// The transcript.
	pub path:          PathBuf,
	/// Recorded working directory.
	pub cwd:           Option<PathBuf>,
	/// Current title: the title slot, else the latest `title_change`, else
	/// the header's.
	pub title:         Option<Str>,
	/// Creation time (the header timestamp), Unix milliseconds.
	pub created_ms:    u64,
	/// Last modification, Unix milliseconds.
	pub modified_ms:   u64,
	/// User and assistant messages, counted for transcripts small enough to
	/// read whole (0 otherwise).
	pub messages:      u32,
	/// First user message.
	pub first_message: Option<Str>,
	/// The v2 journal an earlier import made, when one still exists: picking
	/// the row reopens it instead of converting again.
	pub imported:      Option<PathBuf>,
}

/// A converted (or earlier converted) session.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ImportedSession {
	/// v1 session id.
	pub id:                Str,
	/// Its native journal.
	pub journal:           PathBuf,
	/// Where the journal lives.
	pub bucket:            ProjectBucket,
	/// Whether this call converted it (`false`: an earlier import's journal).
	pub converted:         bool,
	/// Messages imported by this call (0 when not converted).
	pub messages:          usize,
	/// Child journals created by this call.
	pub children:          usize,
	/// v1 artifacts this call copied into the project blob store.
	pub artifacts:         usize,
	/// `artifact://<id>` references with no v1 file, found by this call.
	pub missing_artifacts: Vec<MissingArtifact>,
}

/// A v1 session that could not be listed or imported.
#[derive(Debug, Error)]
pub enum SessionImportError {
	/// A transcript or directory could not be read.
	#[error("could not read {}", path.display())]
	Read {
		/// What was read.
		path:   PathBuf,
		/// Typed filesystem failure.
		#[source]
		source: io::Error,
	},
	/// A v2 file or directory could not be written.
	#[error("could not write {}", path.display())]
	Write {
		/// What was written.
		path:   PathBuf,
		/// Typed filesystem failure.
		#[source]
		source: io::Error,
	},
	/// A v1 artifact could not be copied into the project blob store.
	#[error("could not store {} in the v2 blob store", path.display())]
	Store {
		/// The v1 artifact (or the blob store, when it could not open).
		path:   PathBuf,
		/// Typed blob-store failure.
		#[source]
		source: blob::Error,
	},
	/// A pin list is not valid JSON.
	#[error("{} is not valid JSON", path.display())]
	Json {
		/// The file.
		path:   PathBuf,
		/// Typed parse failure.
		#[source]
		source: serde_json::Error,
	},
	/// The file has no v1 session header.
	#[error("{} is not a v1 session transcript", path.display())]
	NotASession {
		/// The file.
		path: PathBuf,
	},
	/// The picked file is not a transcript under the v1 sessions directory.
	#[error("{} is not a transcript in the v1 sessions directory", path.display())]
	OutsideSessions {
		/// The picked file.
		path: PathBuf,
	},
	/// The transcript has no user or assistant message and no subagent.
	#[error("{} contains no importable messages", path.display())]
	Empty {
		/// The transcript.
		path: PathBuf,
	},
	/// The converter failed.
	#[error("could not convert {}", path.display())]
	Convert {
		/// The transcript.
		path:   PathBuf,
		/// The converter's failure.
		#[source]
		source: ConvertError,
	},
}

/// One line of a v1 transcript, reduced to what listing and placement read.
/// Every line kind shares this flat shape; absent fields stay `None`.
#[derive(Default, Deserialize)]
#[serde(default)]
struct Line<'a> {
	#[serde(rename = "type")]
	kind:      Str,
	id:        Option<Str>,
	cwd:       Option<PathBuf>,
	title:     Option<Str>,
	agent:     Option<Str>,
	#[serde(borrow)]
	timestamp: Option<&'a RawValue>,
	#[serde(borrow)]
	message:   Option<&'a RawValue>,
}

#[derive(Deserialize)]
struct LineMessage<'a> {
	#[serde(default)]
	role:    Str,
	#[serde(borrow, default)]
	content: Option<&'a RawValue>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum LineContent {
	Text(Str),
	Blocks(Vec<LineBlock>),
}

#[derive(Deserialize)]
struct LineBlock {
	#[serde(default)]
	text: Option<Str>,
}

impl LineContent {
	fn text(self) -> Str {
		match self {
			Self::Text(text) => text,
			Self::Blocks(blocks) => {
				let mut text = String::new();
				for block in blocks.into_iter().filter_map(|block| block.text) {
					text.push_str(&block);
				}
				Str::new(text)
			},
		}
	}
}

/// The header facts of one transcript.
#[derive(Debug, Default)]
struct Head {
	id:         Option<Str>,
	cwd:        Option<PathBuf>,
	created_ms: Option<u64>,
	agent:      Option<Str>,
}

fn timestamp_ms(raw: Option<&RawValue>) -> Option<u64> {
	let raw = raw?;
	if let Ok(number) = serde_json::from_str::<u64>(raw.get()) {
		return Some(number);
	}
	let text = serde_json::from_str::<&str>(raw.get()).ok()?;
	let stamp = text.parse::<jiff::Timestamp>().ok()?;
	u64::try_from(stamp.as_millisecond()).ok()
}

fn read_error(path: &Path) -> impl FnOnce(io::Error) -> SessionImportError + '_ {
	move |source| SessionImportError::Read { path: path.to_owned(), source }
}

fn write_error(path: &Path) -> impl FnOnce(io::Error) -> SessionImportError + '_ {
	move |source| SessionImportError::Write { path: path.to_owned(), source }
}

/// Reads a transcript's header: the `session` line (after an optional title
/// slot) and, for a subagent, its `session_init` agent class.
fn read_head(path: &Path) -> Result<Head, SessionImportError> {
	let file = fs::File::open(path).map_err(read_error(path))?;
	let mut input = io::BufReader::new(file);
	let mut line = Vec::new();
	let mut head = Head::default();
	let mut found = false;
	for _ in 0..MAX_HEAD_LINES {
		line.clear();
		let read = io::BufRead::read_until(&mut input, b'\n', &mut line).map_err(read_error(path))?;
		if read == 0 {
			break;
		}
		let Ok(parsed) = serde_json::from_slice::<Line<'_>>(&line) else {
			continue;
		};
		match parsed.kind.as_str() {
			"session" => {
				found = true;
				head.id = parsed.id;
				head.cwd = parsed.cwd;
				head.created_ms = timestamp_ms(parsed.timestamp);
			},
			"session_init" => {
				head.agent = parsed.agent;
				break;
			},
			"message" if found => break,
			_ => {},
		}
	}
	if !found {
		return Err(SessionImportError::NotASession { path: path.to_owned() });
	}
	if head.id.is_none() {
		head.id = file_id(path);
	}
	Ok(head)
}

/// The id in a `<timestamp>_<id>.jsonl` file name.
fn file_id(path: &Path) -> Option<Str> {
	let stem = path.file_stem()?.to_str()?;
	Some(Str::new(stem.rsplit_once('_').map_or(stem, |(_, id)| id)))
}

fn millis(time: SystemTime) -> u64 {
	time
		.duration_since(UNIX_EPOCH)
		.unwrap_or_default()
		.as_millis()
		.try_into()
		.unwrap_or(u64::MAX)
}

fn is_transcript(path: &Path) -> bool {
	path.extension().and_then(|value| value.to_str()) == Some(TRANSCRIPT_EXTENSION)
}

/// Every top-level transcript under the v1 sessions directory
/// (`<root>/<enc-cwd>/<file>.jsonl`), sorted. Subagent transcripts inside a
/// session's artifact directory are not top-level.
fn transcripts(root: &Path) -> Result<Vec<PathBuf>, SessionImportError> {
	let mut found = Vec::new();
	for project in fs::read_dir(root).map_err(read_error(root))? {
		let project = project.map_err(read_error(root))?.path();
		if !project.is_dir() {
			continue;
		}
		for file in fs::read_dir(&project).map_err(read_error(&project))? {
			let file = file.map_err(read_error(&project))?.path();
			if is_transcript(&file) && file.is_file() {
				found.push(file);
			}
		}
	}
	found.sort();
	Ok(found)
}

/// Subagent transcripts directly inside a session's artifact directory.
fn child_transcripts(artifacts: &Path) -> Result<Vec<PathBuf>, SessionImportError> {
	let mut found = Vec::new();
	for file in fs::read_dir(artifacts).map_err(read_error(artifacts))? {
		let file = file.map_err(read_error(artifacts))?.path();
		if is_transcript(&file) && file.is_file() {
			found.push(file);
		}
	}
	found.sort();
	Ok(found)
}

/// Lists the pair's v1 transcripts newest first, reading only headers (and,
/// for small files, message counts). Nothing is converted or written.
///
/// Each row an earlier import already made a journal for carries it
/// ([`V1SessionInfo::imported`]).
///
/// # Errors
///
/// Returns [`SessionImportError::Read`] when the sessions directory cannot be
/// listed. Unreadable transcripts and journals are skipped.
pub fn list(pair: &super::ImportPair) -> Result<Vec<V1SessionInfo>, SessionImportError> {
	let Some(root) = pair.source.locate(V1Item::Sessions) else {
		return Ok(Vec::new());
	};
	let index = ImportedIndex::scan(&pair.target)?;
	let mut rows = Vec::new();
	for path in transcripts(&root)? {
		match inspect(&path) {
			Ok(Some(mut row)) => {
				row.imported = index.journal(&row.id).map(Path::to_owned);
				rows.push(row);
			},
			Ok(None) => {},
			Err(error) => tracing::warn!(
				transcript = %path.display(),
				error = &error as &dyn std::error::Error,
				"skipping unreadable v1 session"
			),
		}
	}
	rows.sort_by(|left, right| {
		right
			.modified_ms
			.cmp(&left.modified_ms)
			.then_with(|| left.path.cmp(&right.path))
	});
	Ok(rows)
}

fn inspect(path: &Path) -> Result<Option<V1SessionInfo>, SessionImportError> {
	let metadata = fs::metadata(path).map_err(read_error(path))?;
	let modified_ms = millis(metadata.modified().unwrap_or(UNIX_EPOCH));
	let exact = metadata.len() <= MAX_EAGER_INDEX_BYTES;
	let mut input = io::BufReader::new(fs::File::open(path).map_err(read_error(path))?);
	let mut line = Vec::new();
	let mut header = None::<Head>;
	let mut slot_title = None;
	let mut changed_title = None;
	let mut header_title = None;
	let mut messages = 0_u32;
	let mut first_message = None;
	loop {
		line.clear();
		let read = io::BufRead::read_until(&mut input, b'\n', &mut line).map_err(read_error(path))?;
		if read == 0 {
			break;
		}
		let Ok(parsed) = serde_json::from_slice::<Line<'_>>(&line) else {
			continue;
		};
		match parsed.kind.as_str() {
			"title" => slot_title = parsed.title,
			"title_change" => changed_title = parsed.title.or(changed_title),
			"session" => {
				header_title = parsed.title;
				header = Some(Head {
					id:         parsed.id,
					cwd:        parsed.cwd,
					created_ms: timestamp_ms(parsed.timestamp),
					agent:      None,
				});
			},
			"message" => {
				let Some(message) = parsed
					.message
					.and_then(|raw| serde_json::from_str::<LineMessage<'_>>(raw.get()).ok())
				else {
					continue;
				};
				match message.role.as_str() {
					"user" => {
						messages = messages.saturating_add(1);
						if first_message.is_none() {
							first_message = message
								.content
								.and_then(|raw| serde_json::from_str::<LineContent>(raw.get()).ok())
								.map(LineContent::text)
								.filter(|text| !text.trim().is_empty());
						}
					},
					"assistant" => messages = messages.saturating_add(1),
					_ => {},
				}
			},
			_ => {},
		}
		if header.is_none() && messages > 0 {
			break;
		}
		if !exact && first_message.is_some() {
			break;
		}
	}
	let Some(header) = header else {
		return Ok(None);
	};
	Ok(Some(V1SessionInfo {
		id: header.id.or_else(|| file_id(path)).unwrap_or_default(),
		path: path.to_owned(),
		cwd: header.cwd,
		title: slot_title.or(changed_title).or(header_title),
		created_ms: header.created_ms.unwrap_or(modified_ms),
		modified_ms,
		messages: if exact { messages } else { 0 },
		first_message,
		imported: None,
	}))
}

/// The v2 journals imported from v1 sessions, keyed by v1 session id.
///
/// Derived, never stored: [`Self::scan`] reads the import provenance each
/// journal under `<data>/projects/*/sessions/` records in its first patches
/// ([`omp_session::import::import_origin`]), so the journals stay the only
/// record of an import and a deleted journal makes its session importable
/// again.
#[derive(Clone, Debug, Default)]
pub struct ImportedIndex {
	by_id: FastHashMap<Str, PathBuf>,
}

impl ImportedIndex {
	/// Scans every project bucket of `target` for journals an omp v1 import
	/// wrote. When several journals name one v1 session, the earliest (the
	/// smallest ULID) wins.
	///
	/// # Errors
	///
	/// Returns [`SessionImportError::Read`] when a bucket directory cannot be
	/// listed. An unreadable or invalid journal is skipped.
	pub fn scan(target: &V2Target) -> Result<Self, SessionImportError> {
		let projects = target.data_dir.join("projects");
		let buckets = match fs::read_dir(&projects) {
			Ok(entries) => entries,
			Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Self::default()),
			Err(source) => return Err(SessionImportError::Read { path: projects, source }),
		};
		let mut journals = Vec::new();
		for bucket in buckets {
			let sessions = bucket
				.map_err(read_error(&projects))?
				.path()
				.join("sessions");
			let entries = match fs::read_dir(&sessions) {
				Ok(entries) => entries,
				Err(error)
					if matches!(
						error.kind(),
						io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
					) =>
				{
					continue;
				},
				Err(source) => return Err(SessionImportError::Read { path: sessions, source }),
			};
			for entry in entries {
				let path = entry.map_err(read_error(&sessions))?.path();
				// Hidden files are journals still being staged.
				let visible = path
					.file_name()
					.and_then(|name| name.to_str())
					.is_some_and(|name| !name.starts_with('.'));
				if visible
					&& path.extension().and_then(|value| value.to_str())
						== Some(omp_journal::FILE_EXTENSION)
				{
					journals.push(path);
				}
			}
		}
		journals.sort();
		let mut index = Self::default();
		for journal in journals {
			let entries = match Journal::scan_prefix(&journal, import::PROVENANCE_PREFIX_BYTES) {
				Ok(entries) => entries,
				Err(error) => {
					tracing::debug!(
						journal = %journal.display(),
						error = &error as &dyn std::error::Error,
						"skipping unreadable journal while indexing v1 imports"
					);
					continue;
				},
			};
			let Some(origin) = import::import_origin(&entries) else {
				continue;
			};
			if origin.format == import::OMP1_FORMAT
				&& let Some(id) = origin.source_id
			{
				index.by_id.entry(id).or_insert(journal);
			}
		}
		Ok(index)
	}

	/// The journal an earlier import made for v1 session `id`.
	#[must_use]
	pub fn journal(&self, id: &str) -> Option<&Path> {
		self.by_id.get(id).map(PathBuf::as_path)
	}

	/// How many v1 sessions have an imported journal.
	#[must_use]
	pub fn len(&self) -> usize {
		self.by_id.len()
	}

	/// Whether no v1 session has an imported journal.
	#[must_use]
	pub fn is_empty(&self) -> bool {
		self.by_id.is_empty()
	}
}

/// Every v1 artifact id `transcript` references (`artifact://<N>`), sorted
/// and deduplicated. A number followed by an identifier character is not an
/// id v1 would have parsed.
fn artifact_references(transcript: &[u8]) -> Vec<u64> {
	let mut ids = Vec::new();
	for start in memchr::memmem::find_iter(transcript, ARTIFACT_SCHEME) {
		let rest = &transcript[start + ARTIFACT_SCHEME.len()..];
		let digits = rest.iter().take_while(|byte| byte.is_ascii_digit()).count();
		if digits == 0
			|| rest
				.get(digits)
				.is_some_and(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
		{
			continue;
		}
		if let Some(id) = std::str::from_utf8(&rest[..digits])
			.ok()
			.and_then(|digits| digits.parse::<u64>().ok())
		{
			ids.push(id);
		}
	}
	ids.sort_unstable();
	ids.dedup();
	ids
}

/// The id v1 resolved a file of its artifact directory to: the numeric
/// prefix before the first `.` (`3.bash.log`).
fn artifact_id(name: &str) -> Option<u64> {
	let (prefix, _) = name.split_once('.')?;
	(!prefix.is_empty() && prefix.bytes().all(|byte| byte.is_ascii_digit()))
		.then(|| prefix.parse().ok())
		.flatten()
}

/// Copies every file of a v1 artifact directory (not its subagent
/// transcripts or subdirectories) into `store`, in name order. The first
/// file with a given numeric prefix carries that id, as v1's lookup found
/// only one.
fn store_artifacts(
	directory: &Path,
	store: &BlobStore,
) -> Result<Vec<V1Artifact>, SessionImportError> {
	let mut files = Vec::new();
	for entry in fs::read_dir(directory).map_err(read_error(directory))? {
		let path = entry.map_err(read_error(directory))?.path();
		if path.is_file() && !is_transcript(&path) {
			files.push(path);
		}
	}
	files.sort();
	let mut artifacts = Vec::<V1Artifact>::with_capacity(files.len());
	for path in files {
		let file = match fs::File::open(&path) {
			Ok(file) => file,
			// Removed since it was listed: as good as never written.
			Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
			Err(source) => return Err(SessionImportError::Read { path, source }),
		};
		let blob = store
			.put_reader(file)
			.map_err(|source| SessionImportError::Store { path: path.clone(), source })?;
		let name = Str::new(path.file_name().unwrap_or_default().to_string_lossy());
		let id =
			artifact_id(&name).filter(|id| !artifacts.iter().any(|artifact| artifact.id == Some(*id)));
		artifacts.push(V1Artifact { name, id, blob });
	}
	Ok(artifacts)
}

/// The shared artifacts a transcript references, recording each id with no
/// file in `missing`.
fn referenced_artifacts(
	transcript: &Path,
	shared: &[V1Artifact],
	missing: &mut Vec<MissingArtifact>,
) -> Result<Vec<V1Artifact>, SessionImportError> {
	let bytes = fs::read(transcript).map_err(read_error(transcript))?;
	let mut artifacts = Vec::new();
	for id in artifact_references(&bytes) {
		match shared.iter().find(|artifact| artifact.id == Some(id)) {
			Some(artifact) => artifacts.push(artifact.clone()),
			None => missing.push(MissingArtifact { transcript: transcript.to_owned(), id }),
		}
	}
	Ok(artifacts)
}

/// Reads a pin list; a missing file is empty.
fn read_pins(path: &Path) -> Result<Vec<Str>, SessionImportError> {
	match fs::read(path) {
		Ok(bytes) => serde_json::from_slice(&bytes)
			.map_err(|source| SessionImportError::Json { path: path.to_owned(), source }),
		Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
		Err(source) => Err(SessionImportError::Read { path: path.to_owned(), source }),
	}
}

/// Pins `journal_id` in `state_dir` when v1 pinned `v1_id`. A corrupt v1 pin
/// list pins nothing, as v1 itself degraded.
fn remap_pin(
	layout: &V1Layout,
	state_dir: &Path,
	v1_id: &str,
	journal_id: &str,
) -> Result<(), SessionImportError> {
	let v1_pins = read_pins(&layout.agent_dir().join(PINS_FILE)).unwrap_or_default();
	if !v1_pins.iter().any(|pin| pin.as_str() == v1_id) {
		return Ok(());
	}
	let path = state_dir.join(PINS_FILE);
	let mut pins = read_pins(&path)?;
	if pins.iter().any(|pin| pin.as_str() == journal_id) {
		return Ok(());
	}
	pins.push(Str::new(journal_id));
	let bytes = serde_json::to_vec(&pins)
		.map_err(|source| SessionImportError::Json { path: path.clone(), source })?;
	fs::create_dir_all(state_dir).map_err(write_error(state_dir))?;
	atomic_replace(&path, &bytes).map_err(write_error(&path))
}

/// A converted journal waiting in its hidden staging file.
struct Staged {
	staging: PathBuf,
	journal: PathBuf,
}

impl Staged {
	/// The writer lock the journal left beside the staging file
	/// (`<staging>.lock`), stale once the converter dropped its session.
	fn lock(&self) -> PathBuf {
		let mut name = self.staging.as_os_str().to_owned();
		name.push(".lock");
		PathBuf::from(name)
	}
}

fn discard(staged: &[Staged]) {
	for file in staged {
		let _ = fs::remove_file(&file.staging);
		let _ = fs::remove_file(file.lock());
	}
}

/// One session's conversion: where its journals stage, and what they share.
struct Tree<'a> {
	sessions:  &'a Path,
	blobs:     Option<&'a Path>,
	store:     &'a BlobStore,
	converter: &'a dyn V1SessionConverter,
	/// Journals converted so far, children before their parents.
	staged:    Vec<Staged>,
	/// The top-level session's artifacts, which every subagent shares: v1
	/// subagents adopted their root session's `ArtifactManager`.
	shared:    Vec<V1Artifact>,
	/// References with no v1 file.
	missing:   Vec<MissingArtifact>,
}

impl Tree<'_> {
	/// Converts `source` and, first, every subagent transcript in its
	/// artifact directory, into staged journals. Returns the new journal id
	/// and its message count.
	fn convert(
		&mut self,
		source: &Path,
		source_id: &str,
		depth: u8,
	) -> Result<(Str, usize), SessionImportError> {
		let directory = source.with_extension("");
		let directory = directory.is_dir().then_some(directory.as_path());
		if depth == 0
			&& let Some(directory) = directory
		{
			self.shared = store_artifacts(directory, self.store)?;
		}
		let mut children = Vec::new();
		if let Some(directory) = directory
			&& depth < MAX_CHILD_DEPTH
		{
			for child in child_transcripts(directory)? {
				let head = match read_head(&child) {
					Ok(head) => head,
					Err(SessionImportError::NotASession { .. }) => continue,
					Err(error) => return Err(error),
				};
				let (id, _) =
					self.convert(&child, head.id.as_deref().unwrap_or_default(), depth + 1)?;
				let source_id = Str::new(
					child
						.file_stem()
						.and_then(|stem| stem.to_str())
						.unwrap_or_default(),
				);
				children.push(V1ChildJob {
					id,
					agent: head.agent.unwrap_or_else(|| source_id.clone()),
					source_id,
					started_ms: head.created_ms.unwrap_or_default(),
				});
			}
		}
		// The session keeps its whole artifact directory; a subagent, what
		// its transcript references.
		let referenced = referenced_artifacts(source, &self.shared, &mut self.missing)?;
		let artifacts = if depth == 0 {
			self.shared.as_slice()
		} else {
			referenced.as_slice()
		};
		let id = sf!("{}", Ulid::generate());
		let mut name = String::with_capacity(id.len() + 16);
		name.push('.');
		name.push_str(&id);
		name.push_str(".importing.oms");
		let staging = self.sessions.join(name);
		let mut name = String::with_capacity(id.len() + 4);
		name.push_str(&id);
		name.push_str(".oms");
		let journal = self.sessions.join(name);
		self
			.staged
			.push(Staged { staging: staging.clone(), journal });
		let messages = self
			.converter
			.convert(&V1Conversion {
				source,
				destination: &staging,
				id: &id,
				source_id,
				blobs: self.blobs,
				artifacts,
				children: &children,
			})
			.map_err(|error| SessionImportError::Convert {
				path:   source.to_owned(),
				source: error,
			})?;
		if depth == 0 && messages == 0 && children.is_empty() {
			return Err(SessionImportError::Empty { path: source.to_owned() });
		}
		Ok((id, messages))
	}
}

/// Converts one v1 transcript of `layout` into `target`, unless an earlier
/// import's journal still exists; see the [module docs](self).
///
/// # Errors
///
/// Returns why the transcript could not be read, converted, or published.
/// Nothing partial stays visible: journals are staged in hidden files and
/// renamed into place once every one of them converted.
pub fn import_session(
	target: &V2Target,
	layout: &V1Layout,
	source: &Path,
	converter: &dyn V1SessionConverter,
) -> Result<ImportedSession, SessionImportError> {
	let mut index = ImportedIndex::scan(target)?;
	import_indexed(target, layout, source, converter, &mut index)
}

/// [`import_session`] against an index the caller already scanned, which
/// learns the new journal.
fn import_indexed(
	target: &V2Target,
	layout: &V1Layout,
	source: &Path,
	converter: &dyn V1SessionConverter,
	index: &mut ImportedIndex,
) -> Result<ImportedSession, SessionImportError> {
	let head = read_head(source)?;
	let id = head.id.unwrap_or_default();
	let bucket = ProjectBucket::for_cwd(head.cwd.as_deref());
	if let Some(earlier) = index.journal(&id) {
		return Ok(ImportedSession {
			journal: earlier.to_owned(),
			id,
			bucket,
			converted: false,
			messages: 0,
			children: 0,
			artifacts: 0,
			missing_artifacts: Vec::new(),
		});
	}
	let state_dir = bucket
		.state_dir(&target.data_dir)
		.map_err(read_error(source))?;
	let sessions = state_dir.join("sessions");
	fs::create_dir_all(&sessions).map_err(write_error(&sessions))?;
	let store_root = omp_env::project_state::blob_store(&state_dir);
	let store = BlobStore::open(&store_root)
		.map_err(|source| SessionImportError::Store { path: store_root, source })?;
	let blobs = layout.locate(V1Item::Blobs);
	let mut tree = Tree {
		sessions: &sessions,
		blobs: blobs.as_deref(),
		store: &store,
		converter,
		staged: Vec::new(),
		shared: Vec::new(),
		missing: Vec::new(),
	};
	let (journal_id, messages) = match tree.convert(source, &id, 0) {
		Ok(converted) => converted,
		Err(error) => {
			discard(&tree.staged);
			return Err(error);
		},
	};
	let Tree { mut staged, shared, missing, .. } = tree;
	// Children first, the parent last: a visible parent always finds its
	// children.
	for (index, file) in staged.iter().enumerate() {
		if let Err(source) = fs::rename(&file.staging, &file.journal) {
			discard(&staged[index..]);
			return Err(SessionImportError::Write { path: file.journal.clone(), source });
		}
		let _ = fs::remove_file(file.lock());
	}
	let journal = staged
		.pop()
		.map(|file| file.journal)
		.expect("the parent journal is staged last");
	index.by_id.insert(id.clone(), journal.clone());
	remap_pin(layout, &state_dir, &id, &journal_id)?;
	for reference in &missing {
		tracing::warn!(
			session = %id,
			transcript = %reference.transcript.display(),
			artifact = reference.id,
			"v1 artifact referenced by an imported session is missing"
		);
	}
	Ok(ImportedSession {
		id,
		journal,
		bucket,
		converted: true,
		messages,
		children: staged.len(),
		artifacts: shared.len(),
		missing_artifacts: missing,
	})
}

/// The picker's conversion: validates that `source` is a transcript in the
/// pair's v1 sessions directory, then [`import_session`]s it.
///
/// # Errors
///
/// Returns [`SessionImportError::OutsideSessions`] for any other file, and
/// the import's failure otherwise.
pub fn import_selected(
	pair: &super::ImportPair,
	source: &Path,
	converter: &dyn V1SessionConverter,
) -> Result<ImportedSession, SessionImportError> {
	let outside = || SessionImportError::OutsideSessions { path: source.to_owned() };
	let root = pair
		.source
		.locate(V1Item::Sessions)
		.and_then(|root| fs::canonicalize(root).ok())
		.ok_or_else(outside)?;
	let picked = fs::canonicalize(source).map_err(read_error(source))?;
	if !is_transcript(&picked) || !picked.starts_with(&root) {
		return Err(outside());
	}
	import_session(&pair.target, &pair.source, &picked, converter)
}

/// The `sessions` step.
pub(super) fn import_sessions(cx: &StepContext<'_>) -> Result<Vec<ImportEntry>, ImportError> {
	let root = cx.locate(V1Item::Sessions);
	let entry =
		|path, outcome| ImportEntry::new(ImportStep::Sessions, V1Item::Sessions, path, outcome);
	let SessionImport::Bulk(converter) = cx.sessions else {
		let outcome = if root.is_some() {
			ImportOutcome::Skipped(SkipReason::OnDemand)
		} else {
			ImportOutcome::NothingToImport
		};
		return Ok(vec![entry(root, outcome)]);
	};
	let marker = ImportStep::Sessions.marker(&cx.pair.target.config_dir);
	let set_marker = || marker.set(None).map_err(write_error(marker.path()));
	let Some(root) = root else {
		if cx.mode == ImportMode::Apply {
			set_marker()?;
		}
		return Ok(vec![entry(None, ImportOutcome::NothingToImport)]);
	};
	let files = transcripts(&root)?;
	let mut index = ImportedIndex::scan(&cx.pair.target)?;
	if files.is_empty() {
		if cx.mode == ImportMode::Apply {
			set_marker()?;
		}
		return Ok(vec![entry(Some(root), ImportOutcome::NothingToImport)]);
	}
	let mut entries = Vec::with_capacity(files.len());
	let mut failed = false;
	for file in files {
		let mut missing = Vec::new();
		let (subject, outcome) = match cx.mode {
			ImportMode::DryRun => match read_head(&file) {
				Ok(head) => {
					let id = head.id.unwrap_or_default();
					let outcome = if index.journal(&id).is_some() {
						ImportOutcome::Skipped(SkipReason::SessionImported)
					} else {
						ImportOutcome::WouldImport
					};
					(Some(subject(&id, &ProjectBucket::for_cwd(head.cwd.as_deref()))), outcome)
				},
				Err(SessionImportError::NotASession { .. }) => (None, ImportOutcome::NothingToImport),
				Err(error) => {
					failed = true;
					(None, ImportOutcome::NeedsAttention(Attention::Failed(error.into())))
				},
			},
			ImportMode::Apply => {
				match import_indexed(&cx.pair.target, &cx.pair.source, &file, converter, &mut index) {
					Ok(session) => {
						missing = session
							.missing_artifacts
							.into_iter()
							.map(|reference| ImportEntry {
								step:    ImportStep::Sessions,
								item:    V1Item::Sessions,
								path:    Some(reference.transcript),
								subject: Some(sf!("{} artifact://{}", session.id, reference.id)),
								outcome: ImportOutcome::NeedsAttention(Attention::ArtifactMissing),
							})
							.collect();
						(
							Some(subject(&session.id, &session.bucket)),
							if session.converted {
								ImportOutcome::Imported
							} else {
								ImportOutcome::Skipped(SkipReason::SessionImported)
							},
						)
					},
					Err(SessionImportError::NotASession { .. } | SessionImportError::Empty { .. }) => {
						(None, ImportOutcome::NothingToImport)
					},
					Err(error) => {
						failed = true;
						(None, ImportOutcome::NeedsAttention(Attention::Failed(error.into())))
					},
				}
			},
		};
		entries.push(ImportEntry {
			step: ImportStep::Sessions,
			item: V1Item::Sessions,
			path: Some(file),
			subject,
			outcome,
		});
		entries.append(&mut missing);
	}
	// A failed session leaves the marker unset, so the next run retries it;
	// the converted ones' journals keep them from converting again.
	if cx.mode == ImportMode::Apply && !failed {
		set_marker()?;
	}
	Ok(entries)
}

/// A report subject: the v1 id, noting the no-directory bucket.
fn subject(id: &str, bucket: &ProjectBucket) -> Str {
	match bucket {
		ProjectBucket::Project(_) => Str::new(id),
		ProjectBucket::NoDirectory => {
			sf!("{id} (project directory gone: {})", omp_env::project_state::NO_DIRECTORY)
		},
	}
}

#[cfg(test)]
mod tests;
