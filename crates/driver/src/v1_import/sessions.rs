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
//! `<data>/v1-sessions/<v1 id>.json` records each converted v1 session's
//! journal: while that journal exists, neither the picker nor a bulk run
//! converts the session again. Conversion also remaps the session's v1 pin
//! (`<agent>/session-pins.json`, keyed by v1 id) to its ULID in the project's
//! own `session-pins.json`.
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

use omp_core::{Hash32, Str, Ulid, sf};
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
use thiserror::Error;

use super::{
	ImportEntry, ImportError, ImportMode, ImportOutcome, ImportStep, StepContext, V1Item, V1Layout,
	V2Target,
	report::{Attention, SkipReason},
	step::atomic_replace,
};

/// Directory, under a v2 profile's data directory, of per-session import
/// records.
const RECORDS_DIR: &str = "v1-sessions";
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
	/// v1's content-addressed blob store, which resolves
	/// `blob:sha256:<hex>` image data.
	pub blobs:       Option<&'a Path>,
	/// The session's artifact directory (`<session>/`), whose tool-output
	/// files the journal retains.
	pub artifacts:   Option<&'a Path>,
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
}

/// A converted (or earlier converted) session.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ImportedSession {
	/// v1 session id.
	pub id:        Str,
	/// Its native journal.
	pub journal:   PathBuf,
	/// Where the journal lives.
	pub bucket:    ProjectBucket,
	/// Whether this call converted it (`false`: an earlier import's journal).
	pub converted: bool,
	/// Messages imported by this call (0 when not converted).
	pub messages:  usize,
	/// Child journals created by this call.
	pub children:  usize,
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
	/// An import record or pin list is not valid JSON.
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

/// Lists v1 transcripts newest first, reading only headers (and, for small
/// files, message counts). Nothing is converted or written.
///
/// # Errors
///
/// Returns [`SessionImportError::Read`] when the sessions directory cannot be
/// listed. Unreadable transcripts are skipped.
pub fn list(layout: &V1Layout) -> Result<Vec<V1SessionInfo>, SessionImportError> {
	let Some(root) = layout.locate(V1Item::Sessions) else {
		return Ok(Vec::new());
	};
	let mut rows = Vec::new();
	for path in transcripts(&root)? {
		match inspect(&path) {
			Ok(Some(row)) => rows.push(row),
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
	}))
}

/// Where one converted session's import record lives: `<data>/v1-sessions/`
/// named after the v1 id, or its digest when the id is not file-name safe.
fn record_path(target: &V2Target, id: &str) -> PathBuf {
	let safe = !id.is_empty()
		&& id.len() <= 128
		&& id
			.bytes()
			.all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'));
	let directory = target.data_dir.join(RECORDS_DIR);
	if safe {
		let mut name = String::with_capacity(id.len() + 5);
		name.push_str(id);
		name.push_str(".json");
		directory.join(name)
	} else {
		let mut name = String::with_capacity(69);
		name.push_str(Hash32::sum(id.as_bytes()).to_hex().as_str());
		name.push_str(".json");
		directory.join(name)
	}
}

/// One converted session's idempotency record.
#[derive(Debug, Deserialize, Serialize)]
struct ImportRecord {
	/// v1 session id.
	id:      Str,
	/// The transcript it came from.
	source:  PathBuf,
	/// Its native journal.
	journal: PathBuf,
}

/// The earlier import of `id`, while its journal still exists.
fn imported(target: &V2Target, id: &str) -> Result<Option<ImportRecord>, SessionImportError> {
	let path = record_path(target, id);
	let bytes = match fs::read(&path) {
		Ok(bytes) => bytes,
		Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
		Err(source) => return Err(SessionImportError::Read { path, source }),
	};
	let record: ImportRecord = serde_json::from_slice(&bytes)
		.map_err(|source| SessionImportError::Json { path: path.clone(), source })?;
	Ok((record.id.as_str() == id && record.journal.is_file()).then_some(record))
}

fn record(target: &V2Target, record: &ImportRecord) -> Result<(), SessionImportError> {
	let path = record_path(target, &record.id);
	let bytes = serde_json::to_vec_pretty(record)
		.map_err(|source| SessionImportError::Json { path: path.clone(), source })?;
	if let Some(parent) = path.parent() {
		fs::create_dir_all(parent).map_err(write_error(parent))?;
	}
	atomic_replace(&path, &bytes).map_err(write_error(&path))
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

/// Converts `source` and, first, every subagent transcript in its artifact
/// directory, into staged journals under `sessions`. Returns the new journal
/// id and its message count.
fn convert_tree(
	sessions: &Path,
	source: &Path,
	blobs: Option<&Path>,
	converter: &dyn V1SessionConverter,
	staged: &mut Vec<Staged>,
	depth: u8,
) -> Result<(Str, usize), SessionImportError> {
	let artifacts = source.with_extension("");
	let artifacts = artifacts.is_dir().then_some(artifacts.as_path());
	let mut children = Vec::new();
	if let Some(directory) = artifacts
		&& depth < MAX_CHILD_DEPTH
	{
		for child in child_transcripts(directory)? {
			let head = match read_head(&child) {
				Ok(head) => head,
				Err(SessionImportError::NotASession { .. }) => continue,
				Err(error) => return Err(error),
			};
			let (id, _) = convert_tree(sessions, &child, blobs, converter, staged, depth + 1)?;
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
	let id = sf!("{}", Ulid::generate());
	let mut name = String::with_capacity(id.len() + 16);
	name.push('.');
	name.push_str(&id);
	name.push_str(".importing.oms");
	let staging = sessions.join(name);
	let mut name = String::with_capacity(id.len() + 4);
	name.push_str(&id);
	name.push_str(".oms");
	let journal = sessions.join(name);
	staged.push(Staged { staging: staging.clone(), journal });
	let messages = converter
		.convert(&V1Conversion {
			source,
			destination: &staging,
			id: &id,
			blobs,
			artifacts,
			children: &children,
		})
		.map_err(|error| SessionImportError::Convert { path: source.to_owned(), source: error })?;
	if depth == 0 && messages == 0 && children.is_empty() {
		return Err(SessionImportError::Empty { path: source.to_owned() });
	}
	Ok((id, messages))
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
	let head = read_head(source)?;
	let id = head.id.unwrap_or_default();
	let bucket = ProjectBucket::for_cwd(head.cwd.as_deref());
	if let Some(earlier) = imported(target, &id)? {
		return Ok(ImportedSession {
			id,
			journal: earlier.journal,
			bucket,
			converted: false,
			messages: 0,
			children: 0,
		});
	}
	let state_dir = bucket
		.state_dir(&target.data_dir)
		.map_err(read_error(source))?;
	let sessions = state_dir.join("sessions");
	fs::create_dir_all(&sessions).map_err(write_error(&sessions))?;
	let blobs = layout.locate(V1Item::Blobs);
	let mut staged = Vec::new();
	let (journal_id, messages) =
		match convert_tree(&sessions, source, blobs.as_deref(), converter, &mut staged, 0) {
			Ok(converted) => converted,
			Err(error) => {
				discard(&staged);
				return Err(error);
			},
		};
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
	record(target, &ImportRecord {
		id:      id.clone(),
		source:  source.to_owned(),
		journal: journal.clone(),
	})?;
	remap_pin(layout, &state_dir, &id, &journal_id)?;
	Ok(ImportedSession { id, journal, bucket, converted: true, messages, children: staged.len() })
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
	if files.is_empty() {
		if cx.mode == ImportMode::Apply {
			set_marker()?;
		}
		return Ok(vec![entry(Some(root), ImportOutcome::NothingToImport)]);
	}
	let mut entries = Vec::with_capacity(files.len());
	let mut failed = false;
	for file in files {
		let (subject, outcome) = match cx.mode {
			ImportMode::DryRun => match read_head(&file) {
				Ok(head) => {
					let id = head.id.unwrap_or_default();
					let outcome = if imported(&cx.pair.target, &id)?.is_some() {
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
				match import_session(&cx.pair.target, &cx.pair.source, &file, converter) {
					Ok(session) => (
						Some(subject(&session.id, &session.bucket)),
						if session.converted {
							ImportOutcome::Imported
						} else {
							ImportOutcome::Skipped(SkipReason::SessionImported)
						},
					),
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
	}
	// A failed session leaves the marker unset, so the next run retries it;
	// the per-session records keep the converted ones from converting again.
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
