//! Claude Code, Codex, and omp v1 transcript import into native `.oms`
//! journals.
//!
//! omp v1 sessions are located, placed, and recorded by
//! [`omp_driver::v1_import::sessions`]; this module converts their bytes
//! ([`V1Converter`]).
//!
//! Every format is told "already imported" the same way: the driver's
//! [`ImportedIndex`] reads the provenance each imported journal records.
//! A Claude Code or Codex transcript whose import is current reopens that
//! journal instead of converting again; a changed one (or another file of an
//! imported session) converts into a fresh journal beside the earlier one.

mod convert;

use std::{
	fs,
	io::{self, BufRead, BufReader, IsTerminal as _, Write},
	path::{Path, PathBuf},
	time::{SystemTime, UNIX_EPOCH},
};

use miette::{IntoDiagnostic as _, miette};
use omp_chat::overlays::services::ForeignImport;
use omp_core::Str;
use omp_driver::session_imports::{ImportedIndex, PriorImport};
use serde_json::Value;

use crate::cli::ChatArgs;

/// Foreign transcript dialect accepted by the one-shot importer.
#[derive(Clone, Copy, Debug, Eq, PartialEq, strum::Display)]
pub enum ForeignFormat {
	/// Claude Code JSON-line events.
	Claude,
	/// Codex CLI rollout JSON-line events.
	Codex,
	/// omp v1 (TypeScript `omp`) session transcripts.
	Omp1,
}

/// Lightweight metadata for one importable foreign transcript.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ForeignCandidate {
	/// Stable source-local session identity.
	pub id:            Str,
	/// Source transcript path.
	pub path:          PathBuf,
	/// Project directory recorded by the source, or its containing directory.
	pub cwd:           PathBuf,
	/// Source-provided title, when present.
	pub title:         Option<Str>,
	/// Creation time, Unix milliseconds.
	pub created_ms:    u64,
	/// Last modification, Unix milliseconds.
	pub modified_ms:   u64,
	/// Exact user and assistant message count for transcripts small enough to
	/// index eagerly.
	pub messages:      u32,
	/// First user message, when it occurs in the indexed prefix.
	pub first_message: Option<Str>,
	/// What an earlier import of this transcript left, when a journal of one
	/// still exists: picking it reopens a current import's journal, or imports
	/// a changed transcript (or another file of an imported session).
	pub imported:      Option<ForeignImport>,
}

impl From<omp_chat::overlays::services::ForeignSessionSource> for ForeignFormat {
	fn from(source: omp_chat::overlays::services::ForeignSessionSource) -> Self {
		match source {
			omp_chat::overlays::services::ForeignSessionSource::Claude => Self::Claude,
			omp_chat::overlays::services::ForeignSessionSource::Codex => Self::Codex,
			omp_chat::overlays::services::ForeignSessionSource::Omp1 => Self::Omp1,
		}
	}
}

/// Enumerates transcripts for `format`, newest first, without materializing a
/// native session.
///
/// Each Claude Code or Codex row carries what an earlier import left
/// ([`ForeignCandidate::imported`]), found among the journals under
/// `data_dir`'s project buckets and in `sessions_dir` (the chat's session
/// directory, where the picker imports). omp v1 rows are the driver's
/// ([`omp_driver::v1_import::sessions::list`]).
pub fn candidates(
	format: ForeignFormat,
	data_dir: &Path,
	sessions_dir: &Path,
) -> miette::Result<Vec<ForeignCandidate>> {
	if format == ForeignFormat::Omp1 {
		return v1_candidates();
	}
	foreign_candidates(format, &foreign_root(format)?, data_dir, sessions_dir)
}

/// [`candidates`] of a Claude Code or Codex install rooted at `root`.
fn foreign_candidates(
	format: ForeignFormat,
	root: &Path,
	data_dir: &Path,
	sessions_dir: &Path,
) -> miette::Result<Vec<ForeignCandidate>> {
	let index = imported_index(format, data_dir, sessions_dir)?;
	let mut candidates = jsonl_candidates(format, root)?
		.into_iter()
		.map(|path| {
			let mut candidate = inspect_candidate(format, path, root)?;
			candidate.imported = index
				.prior(&candidate.id, &candidate.path)
				.into_diagnostic()?
				.map(foreign_import);
			Ok(candidate)
		})
		.collect::<miette::Result<Vec<_>>>()?;
	candidates.sort_by(|left, right| {
		right
			.modified_ms
			.cmp(&left.modified_ms)
			.then_with(|| left.path.cmp(&right.path))
	});
	Ok(candidates)
}

/// Lets the operator select a requested foreign session, imports it, and
/// rewrites the launch to resume the resulting native journal.
pub(crate) fn prepare(args: &mut ChatArgs) -> miette::Result<()> {
	let format = if args.from_claude {
		ForeignFormat::Claude
	} else {
		ForeignFormat::Codex
	};
	let root = foreign_root(format)?;
	let data_dir = omp_core::dirs::data_dir(None).into_diagnostic()?;
	let project = fs::canonicalize(&args.project).into_diagnostic()?;
	let state_dir =
		omp_env::project_state::directory(&data_dir, &project).map_err(|source| miette!(source))?;
	let sessions = args
		.session_dir
		.clone()
		.unwrap_or_else(|| state_dir.join("sessions"));
	let mut candidates = candidates(format, &data_dir, &sessions)?;
	let paths = candidates
		.iter()
		.map(|candidate| candidate.path.clone())
		.collect::<Vec<_>>();
	let source = match paths.as_slice() {
		[] => {
			return Err(miette!(
				"no importable {} sessions were found under {}",
				format,
				root.display(),
			));
		},
		[only] => only.clone(),
		_ if !io::stdin().is_terminal() => {
			return Err(miette!(
				"multiple foreign sessions were found; rerun from an interactive terminal to select \
				 one"
			));
		},
		_ => {
			let stdin = io::stdin();
			let mut input = stdin.lock();
			let stderr = io::stderr();
			let mut output = stderr.lock();
			select_candidate(&paths, &mut input, &mut output)?
		},
	};
	let picked = candidates
		.iter()
		.position(|candidate| candidate.path == source)
		.map(|index| candidates.swap_remove(index));
	if let Some(ForeignImport::Current(journal)) = picked.and_then(|picked| picked.imported) {
		eprintln!("Reopening {}, imported earlier from {}.", journal.display(), source.display());
		args.resume = Some(Str::new(journal.to_string_lossy()));
		args.session_dir = Some(sessions);
		args.from_claude = false;
		args.from_codex = false;
		return Ok(());
	}
	fs::create_dir_all(&sessions).into_diagnostic()?;
	let destination = sessions.join(format!("{}.oms", omp_core::Ulid::generate()));
	let count = import_file(format, &source, &destination)?;
	if count == 0 {
		return Err(miette!(
			"{} contains no importable user or assistant messages",
			source.display()
		));
	}
	eprintln!(
		"Imported {} messages from {} into {}.",
		count,
		source.display(),
		destination.display()
	);
	args.resume = Some(Str::new(destination.to_string_lossy()));
	args.session_dir = Some(sessions);
	args.from_claude = false;
	args.from_codex = false;
	Ok(())
}

/// Imports a picker selection into a fresh native journal, or reopens the
/// journal an earlier import made of the transcript as it is now.
///
/// The selected path is revalidated against the source authority. Earlier
/// imports are looked up among the journals under `data_dir`'s project
/// buckets and beside `destination` ([`ImportedIndex`]); a
/// [current](PriorImport::Current) one is returned as it is. Otherwise
/// conversion happens in a hidden sibling file and becomes visible only after
/// an atomic rename, so a failed import never leaves a resumable partial
/// journal; an earlier import of a changed transcript stays untouched.
///
/// An omp v1 session ignores `destination` and `data_dir`: it lands in its
/// recorded project's bucket of the active profile, or reopens its current
/// import (owner decision #4, [`omp_driver::v1_import::sessions`]).
pub fn import_selected(
	format: ForeignFormat,
	source: &Path,
	destination: &Path,
	data_dir: &Path,
) -> miette::Result<PathBuf> {
	if format == ForeignFormat::Omp1 {
		let pair = omp_driver::v1_import::active_pair().into_diagnostic()?;
		return omp_driver::v1_import::sessions::import_selected(&pair, source, &V1Converter)
			.map(|imported| imported.journal)
			.into_diagnostic();
	}
	let source = validate_selection(format, source)?;
	import_picked(format, &source, destination, data_dir)
}

/// [`import_selected`] of a Claude Code or Codex transcript already
/// validated against its source authority.
fn import_picked(
	format: ForeignFormat,
	source: &Path,
	destination: &Path,
	data_dir: &Path,
) -> miette::Result<PathBuf> {
	if destination.extension().and_then(|value| value.to_str()) != Some("oms") {
		return Err(miette!("native session destination must use the .oms extension"));
	}
	if destination.exists() {
		return Err(miette!("native session destination already exists"));
	}
	let parent = destination
		.parent()
		.ok_or_else(|| miette!("native session destination has no parent directory"))?;
	if let Some(journal) = current_import(format, source, data_dir, parent)? {
		return Ok(journal);
	}
	fs::create_dir_all(parent).into_diagnostic()?;
	let staging = parent.join(format!(".{}.importing.oms", omp_core::Ulid::generate()));
	let imported = import_file(format, source, &staging);
	let count = match imported {
		Ok(count) => count,
		Err(error) => {
			let _ = fs::remove_file(&staging);
			return Err(error);
		},
	};
	if count == 0 {
		let _ = fs::remove_file(&staging);
		return Err(miette!(
			"Selected {format} session contains no importable user or assistant messages"
		));
	}
	if let Err(source) = fs::rename(&staging, destination) {
		let _ = fs::remove_file(&staging);
		return Err(source).into_diagnostic();
	}
	Ok(destination.to_path_buf())
}

/// Imports one foreign JSONL transcript into a replayable `.oms` journal.
///
/// Conversion retains the exact source bytes in the journal's content-addressed
/// store and materializes every representable message, content block, tool
/// exchange, attachment, timestamp, usage record, and branch.
pub fn import_file(
	format: ForeignFormat,
	source: &Path,
	destination: &Path,
) -> miette::Result<usize> {
	convert::import_file(format, source, destination)
}

/// The omp v1 session converter the driver's import runs through: the
/// picker's on-demand import and `omp config import-v1 --sessions`.
#[derive(Clone, Copy, Debug, Default)]
pub struct V1Converter;

impl omp_driver::v1_import::V1SessionConverter for V1Converter {
	fn convert(
		&self,
		conversion: &omp_driver::v1_import::V1Conversion<'_>,
	) -> Result<usize, omp_driver::v1_import::sessions::ConvertError> {
		convert::import_v1(conversion).map_err(Into::into)
	}
}

/// The import format a converted journal records for `format`
/// (`import-format`), which [`ImportedIndex`] recognizes its imports by.
fn import_format(format: ForeignFormat) -> String {
	format.to_string().to_ascii_lowercase()
}

/// The journals imports of `format` wrote under `data_dir`'s project
/// buckets and in `sessions_dir`.
fn imported_index(
	format: ForeignFormat,
	data_dir: &Path,
	sessions_dir: &Path,
) -> miette::Result<ImportedIndex> {
	let mut index = ImportedIndex::scan(data_dir, &import_format(format)).into_diagnostic()?;
	index.scan_sessions(sessions_dir).into_diagnostic()?;
	Ok(index)
}

/// The journal of a current earlier import of the Claude Code or Codex
/// transcript at `source` (canonical), when one exists under `data_dir`'s
/// project buckets or in `sessions_dir`.
fn current_import(
	format: ForeignFormat,
	source: &Path,
	data_dir: &Path,
	sessions_dir: &Path,
) -> miette::Result<Option<PathBuf>> {
	let id = inspect_candidate(format, source.to_path_buf(), sessions_dir)?.id;
	let prior = imported_index(format, data_dir, sessions_dir)?
		.prior(&id, source)
		.into_diagnostic()?;
	Ok(match prior {
		Some(PriorImport::Current(journal)) => Some(journal),
		Some(PriorImport::Changed(_) | PriorImport::OtherFile(_)) | None => None,
	})
}

/// How the picker shows what an earlier import left.
fn foreign_import(prior: PriorImport) -> ForeignImport {
	match prior {
		PriorImport::Current(journal) => ForeignImport::Current(journal),
		PriorImport::Changed(journal) => ForeignImport::Changed(journal),
		PriorImport::OtherFile(journal) => ForeignImport::OtherFile(journal),
	}
}

/// The active profile's v1 sessions, from their headers alone (and, for the
/// ones imported earlier, their digests).
fn v1_candidates() -> miette::Result<Vec<ForeignCandidate>> {
	let pair = omp_driver::v1_import::active_pair().into_diagnostic()?;
	Ok(omp_driver::v1_import::sessions::list(&pair)
		.into_diagnostic()?
		.into_iter()
		.map(|session| ForeignCandidate {
			cwd:           session.cwd.unwrap_or_else(|| {
				session
					.path
					.parent()
					.map(Path::to_path_buf)
					.unwrap_or_default()
			}),
			id:            session.id,
			path:          session.path,
			title:         session.title,
			created_ms:    session.created_ms,
			modified_ms:   session.modified_ms,
			messages:      session.messages,
			first_message: session.first_message,
			imported:      session.imported.map(foreign_import),
		})
		.collect())
}

fn foreign_root(format: ForeignFormat) -> miette::Result<PathBuf> {
	let home = std::env::var_os("HOME")
		.map(PathBuf::from)
		.ok_or_else(|| miette!("HOME is unset"))?;
	Ok(match format {
		ForeignFormat::Claude => std::env::var_os("CLAUDE_CONFIG_DIR")
			.map(PathBuf::from)
			.unwrap_or_else(|| home.join(".claude")),
		ForeignFormat::Codex => home.join(".codex"),
		ForeignFormat::Omp1 => omp_driver::v1_import::active_pair()
			.into_diagnostic()?
			.source
			.locate(omp_driver::v1_import::V1Item::Sessions)
			.ok_or_else(|| miette!("no v1 sessions directory was found"))?,
	})
}

fn transcript_roots(format: ForeignFormat, root: &Path) -> Vec<PathBuf> {
	match format {
		ForeignFormat::Claude => vec![root.join("projects"), root.join(".projects")],
		ForeignFormat::Codex => {
			vec![root.join("sessions"), root.join(".sessions"), root.join("archived_sessions")]
		},
		ForeignFormat::Omp1 => vec![root.to_path_buf()],
	}
}

fn validate_selection(format: ForeignFormat, source: &Path) -> miette::Result<PathBuf> {
	let source = match fs::canonicalize(source) {
		Ok(path) => path,
		Err(error) if error.kind() == io::ErrorKind::NotFound => {
			return Err(miette!("Selected {format} session is no longer available"));
		},
		Err(error) => return Err(error).into_diagnostic(),
	};
	let root = foreign_root(format)?;
	let allowed = transcript_roots(format, &root)
		.into_iter()
		.filter_map(|path| fs::canonicalize(path).ok())
		.any(|path| source.starts_with(path));
	if source.extension().and_then(|value| value.to_str()) != Some("jsonl") || !allowed {
		return Err(miette!(
			"Selected {format} session is outside the {format} transcript directory"
		));
	}
	Ok(source)
}

fn inspect_candidate(
	format: ForeignFormat,
	path: PathBuf,
	root: &Path,
) -> miette::Result<ForeignCandidate> {
	const MAX_EAGER_INDEX_BYTES: u64 = 1024 * 1024;

	let metadata = fs::metadata(&path).into_diagnostic()?;
	let modified = metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH);
	let created = metadata.created().unwrap_or(modified);
	let mut candidate = ForeignCandidate {
		id: Str::new(
			path
				.file_stem()
				.and_then(|value| value.to_str())
				.unwrap_or_default(),
		),
		cwd: path.parent().unwrap_or(root).to_path_buf(),
		path,
		title: None,
		created_ms: system_time_millis(created),
		modified_ms: system_time_millis(modified),
		messages: 0,
		first_message: None,
		imported: None,
	};
	let exact_count = metadata.len() <= MAX_EAGER_INDEX_BYTES;
	let mut input = BufReader::new(fs::File::open(&candidate.path).into_diagnostic()?);
	let mut indexed_bytes = 0_u64;
	let mut source_created_ms = None::<u64>;
	let mut source_modified_ms = None::<u64>;
	let mut named = false;
	let text = |record: &'_ Value, key: &str| {
		record
			.get(key)
			.and_then(Value::as_str)
			.filter(|value| !value.is_empty())
			.map(Str::new)
	};
	let mut line = Vec::new();
	loop {
		line.clear();
		let read = input.read_until(b'\n', &mut line).into_diagnostic()?;
		if read == 0 {
			break;
		}
		indexed_bytes = indexed_bytes.saturating_add(u64::try_from(read).unwrap_or(u64::MAX));
		if !exact_count && indexed_bytes > MAX_EAGER_INDEX_BYTES {
			break;
		}
		let Ok(value) = serde_json::from_slice::<Value>(&line) else {
			continue;
		};
		let payload = value.get("payload").unwrap_or(&value);
		if let Some(timestamp) = foreign_timestamp_ms(
			value
				.get("timestamp")
				.or_else(|| value.get("ts"))
				.or_else(|| payload.get("timestamp")),
		) {
			source_created_ms = Some(source_created_ms.map_or(timestamp, |old| old.min(timestamp)));
			source_modified_ms = Some(source_modified_ms.map_or(timestamp, |old| old.max(timestamp)));
		}
		// The first id a record names, as the importer records it
		// (`import-source-id`), so the row finds its earlier imports.
		if !named
			&& let Some(id) = text(&value, "sessionId")
				.or_else(|| text(&value, "session_id"))
				.or_else(|| {
					(value.get("type").and_then(Value::as_str) == Some("session_meta"))
						.then(|| text(payload, "id"))
						.flatten()
				}) {
			candidate.id = id;
			named = true;
		}
		if let Some(cwd) = value
			.get("cwd")
			.and_then(Value::as_str)
			.or_else(|| payload.get("cwd").and_then(Value::as_str))
		{
			candidate.cwd = PathBuf::from(cwd);
		}
		let record_title = match value.get("type").and_then(Value::as_str) {
			Some("custom-title") => value.get("customTitle").and_then(Value::as_str),
			Some("ai-title") => value.get("aiTitle").and_then(Value::as_str),
			_ if payload.get("type").and_then(Value::as_str) == Some("thread_name_updated") => {
				payload.get("thread_name").and_then(Value::as_str)
			},
			_ => value
				.get("summary")
				.and_then(Value::as_str)
				.or_else(|| value.get("title").and_then(Value::as_str))
				.or_else(|| payload.get("title").and_then(Value::as_str)),
		};
		if let Some(title) = record_title.filter(|title| !title.trim().is_empty()) {
			candidate.title = Some(Str::new(title));
		}
		if let Some((role, text)) = foreign_message(format, &value) {
			candidate.messages = candidate.messages.saturating_add(1);
			if role == "user" && candidate.first_message.is_none() && !text.trim().is_empty() {
				candidate.first_message = Some(text);
			}
		}
		if !exact_count && candidate.first_message.is_some() {
			candidate.messages = 0;
			break;
		}
	}
	if !exact_count {
		candidate.messages = 0;
	}
	if let Some(created) = source_created_ms {
		candidate.created_ms = created;
	}
	if exact_count && let Some(modified) = source_modified_ms {
		candidate.modified_ms = modified;
	}
	Ok(candidate)
}

fn system_time_millis(time: SystemTime) -> u64 {
	time
		.duration_since(UNIX_EPOCH)
		.unwrap_or_default()
		.as_millis()
		.try_into()
		.unwrap_or(u64::MAX)
}

fn foreign_timestamp_ms(value: Option<&Value>) -> Option<u64> {
	let value = value?;
	if let Some(number) = value.as_u64() {
		return Some(if number < 10_000_000_000 {
			number.saturating_mul(1000)
		} else {
			number
		});
	}
	let timestamp = value.as_str()?.parse::<jiff::Timestamp>().ok()?;
	u64::try_from(timestamp.as_millisecond()).ok()
}

fn foreign_message(format: ForeignFormat, value: &Value) -> Option<(&'static str, Str)> {
	match format {
		ForeignFormat::Claude => {
			let role = value
				.get("type")
				.and_then(Value::as_str)
				.or_else(|| value.pointer("/message/role").and_then(Value::as_str))?;
			let role = match role {
				"user" | "human" => "user",
				"assistant" => "assistant",
				_ => return None,
			};
			let content = value
				.pointer("/message/content")
				.or_else(|| value.get("content"))?;
			text_content(content).map(|text| (role, text))
		},
		ForeignFormat::Codex => {
			let payload = value.get("payload").unwrap_or(value);
			if payload
				.get("type")
				.and_then(Value::as_str)
				.is_some_and(|kind| !matches!(kind, "message" | "user_message" | "assistant_message"))
			{
				return None;
			}
			let role = payload.get("role").and_then(Value::as_str).or_else(|| {
				match payload.get("type").and_then(Value::as_str) {
					Some("user_message") => Some("user"),
					Some("assistant_message") => Some("assistant"),
					_ => None,
				}
			})?;
			let role = match role {
				"user" => "user",
				"assistant" => "assistant",
				_ => return None,
			};
			let content = payload.get("content").or_else(|| payload.get("message"))?;
			text_content(content).map(|text| (role, text))
		},
		ForeignFormat::Omp1 => {
			let message = value.get("message")?;
			let role = match message.get("role")?.as_str()? {
				"user" => "user",
				"assistant" => "assistant",
				_ => return None,
			};
			text_content(message.get("content")?).map(|text| (role, text))
		},
	}
}

fn text_content(value: &Value) -> Option<Str> {
	if let Some(text) = value.as_str() {
		return Some(Str::new(text));
	}
	let parts = value.as_array()?;
	let mut text = String::new();
	for part in parts {
		if let Some(value) = part
			.as_str()
			.or_else(|| part.get("text").and_then(Value::as_str))
		{
			text.push_str(value);
		}
	}
	(!text.is_empty()).then(|| Str::new(text))
}

fn jsonl_candidates(format: ForeignFormat, root: &Path) -> miette::Result<Vec<PathBuf>> {
	let mut stack = transcript_roots(format, root);
	stack.sort();
	stack.dedup();
	let mut candidates = Vec::new();
	while let Some(directory) = stack.pop() {
		let entries = match fs::read_dir(&directory) {
			Ok(entries) => entries,
			Err(source) if source.kind() == io::ErrorKind::NotFound => continue,
			Err(source) => return Err(source).into_diagnostic(),
		};
		for entry in entries {
			let entry = entry.into_diagnostic()?;
			let path = entry.path();
			let metadata = entry.metadata().into_diagnostic()?;
			if metadata.is_dir() {
				stack.push(path);
			} else if path.extension().and_then(|value| value.to_str()) == Some("jsonl") {
				candidates.push((metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH), path));
			}
		}
	}
	sort_candidate_paths(&mut candidates);
	Ok(candidates.into_iter().map(|(_, path)| path).collect())
}

fn sort_candidate_paths(candidates: &mut [(SystemTime, PathBuf)]) {
	candidates.sort_by(|(left_time, left_path), (right_time, right_path)| {
		right_time
			.cmp(left_time)
			.then_with(|| left_path.cmp(right_path))
	});
}

fn select_candidate(
	candidates: &[PathBuf],
	input: &mut impl BufRead,
	output: &mut impl Write,
) -> miette::Result<PathBuf> {
	writeln!(output, "Select a foreign session to import:").into_diagnostic()?;
	for (index, path) in candidates.iter().enumerate() {
		writeln!(output, "  {}. {}", index + 1, path.display()).into_diagnostic()?;
	}
	write!(output, "Selection [1-{}]: ", candidates.len()).into_diagnostic()?;
	output.flush().into_diagnostic()?;
	let mut line = String::new();
	input.read_line(&mut line).into_diagnostic()?;
	let selected = line
		.trim()
		.parse::<usize>()
		.ok()
		.and_then(|value| value.checked_sub(1))
		.and_then(|index| candidates.get(index))
		.ok_or_else(|| miette!("invalid foreign session selection"))?;
	Ok(selected.clone())
}

#[cfg(test)]
mod tests {
	use omp_dom::{PropKey, Value as DomValue};
	use omp_session::{ComponentRegistry, Session};

	use super::*;

	#[test]
	fn foreign_picker_lists_every_candidate_and_honors_the_explicit_selection() {
		let candidates = vec![PathBuf::from("newest.jsonl"), PathBuf::from("older.jsonl")];
		let mut input = io::Cursor::new(b"2\n");
		let mut output = Vec::new();
		let selected = select_candidate(&candidates, &mut input, &mut output).unwrap();
		assert_eq!(selected, PathBuf::from("older.jsonl"));
		let rendered = String::from_utf8(output).unwrap();
		assert!(rendered.contains("1. newest.jsonl"));
		assert!(rendered.contains("2. older.jsonl"));
	}

	#[test]
	fn candidate_order_is_newest_first_then_path_ascending() {
		let earlier = UNIX_EPOCH + std::time::Duration::from_secs(1);
		let later = UNIX_EPOCH + std::time::Duration::from_secs(2);
		let mut rows = vec![
			(later, PathBuf::from("z.jsonl")),
			(earlier, PathBuf::from("old.jsonl")),
			(later, PathBuf::from("a.jsonl")),
		];
		sort_candidate_paths(&mut rows);
		assert_eq!(rows.into_iter().map(|(_, path)| path).collect::<Vec<_>>(), vec![
			PathBuf::from("a.jsonl"),
			PathBuf::from("z.jsonl"),
			PathBuf::from("old.jsonl"),
		]);
	}

	/// The driver and the environment recognize an omp v1 import by the
	/// format name this importer journals.
	#[test]
	fn the_omp1_format_name_is_the_one_the_driver_indexes() {
		assert_eq!(
			ForeignFormat::Omp1.to_string().to_ascii_lowercase(),
			omp_session::import::OMP1_FORMAT
		);
	}

	/// A Claude Code transcript of session `id` with one exchange.
	fn claude_transcript(path: &Path, id: &str) {
		fs::create_dir_all(path.parent().unwrap()).unwrap();
		let lines = [
			serde_json::json!({"type": "user", "sessionId": id, "message": {"role": "user", "content": "hello"}}),
			serde_json::json!({"type": "assistant", "sessionId": id, "message": {"role": "assistant", "content": [{"type": "text", "text": "world"}]}}),
		];
		fs::write(path, lines.map(|line| line.to_string() + "\n").concat()).unwrap();
	}

	/// Sets `path`'s modification time a minute back: settled, so an import
	/// records its stamp.
	fn backdate(path: &Path) {
		let earlier = SystemTime::now() - std::time::Duration::from_secs(60);
		fs::File::options()
			.write(true)
			.open(path)
			.and_then(|file| file.set_modified(earlier))
			.unwrap();
	}

	/// The visible journals directly in `directory`.
	fn journals(directory: &Path) -> Vec<PathBuf> {
		let mut found = fs::read_dir(directory)
			.unwrap()
			.map(|entry| entry.unwrap().path())
			.filter(|path| {
				path.extension().and_then(|value| value.to_str()) == Some("oms")
					&& !path.file_name().unwrap().to_string_lossy().starts_with('.')
			})
			.collect::<Vec<_>>();
		found.sort();
		found
	}

	fn fresh(sessions: &Path) -> PathBuf {
		sessions.join(format!("{}.oms", omp_core::Ulid::generate()))
	}

	#[test]
	fn picking_the_same_claude_session_twice_reopens_its_journal() {
		let directory = tempfile::tempdir().unwrap();
		let root = directory.path().join(".claude");
		let data = directory.path().join("data");
		let sessions = data.join("projects/0123abcd/sessions");
		let source = root.join("projects/-project/claude-1.jsonl");
		claude_transcript(&source, "claude-1");
		backdate(&source);

		let first = import_picked(ForeignFormat::Claude, &source, &fresh(&sessions), &data).unwrap();
		let again = import_picked(ForeignFormat::Claude, &source, &fresh(&sessions), &data).unwrap();
		assert_eq!(again, first, "a current import reopens");
		assert_eq!(journals(&sessions), [first.clone()]);
		// Another project's chat reopens it too: every bucket is looked in.
		let elsewhere = data.join("projects/4567ef01/sessions");
		let other = import_picked(ForeignFormat::Claude, &source, &fresh(&elsewhere), &data).unwrap();
		assert_eq!(other, first);
		assert!(!elsewhere.exists());

		// The picker marks the row.
		let rows = foreign_candidates(ForeignFormat::Claude, &root, &data, &sessions).unwrap();
		assert_eq!(rows.len(), 1);
		assert_eq!(rows[0].id, "claude-1");
		assert_eq!(rows[0].imported, Some(ForeignImport::Current(first.clone())));

		// The session went on: marked changed, and picking it imports it
		// again into a fresh journal beside the earlier one.
		let mut bytes = fs::read(&source).unwrap();
		bytes.extend_from_slice(
			b"{\"type\":\"user\",\"sessionId\":\"claude-1\",\"message\":{\"content\":\"more\"}}\n",
		);
		fs::write(&source, bytes).unwrap();
		let rows = foreign_candidates(ForeignFormat::Claude, &root, &data, &sessions).unwrap();
		assert_eq!(rows[0].imported, Some(ForeignImport::Changed(first.clone())));
		let second = import_picked(ForeignFormat::Claude, &source, &fresh(&sessions), &data).unwrap();
		assert_ne!(second, first);
		assert_eq!(journals(&sessions).len(), 2);
		assert!(first.is_file());
		let rows = foreign_candidates(ForeignFormat::Claude, &root, &data, &sessions).unwrap();
		assert_eq!(rows[0].imported, Some(ForeignImport::Current(second.clone())));
		assert_eq!(
			import_picked(ForeignFormat::Claude, &source, &fresh(&sessions), &data).unwrap(),
			second
		);
	}

	#[test]
	fn another_codex_rollout_of_an_imported_session_imports_on_its_own() {
		let directory = tempfile::tempdir().unwrap();
		let root = directory.path().join(".codex");
		let data = directory.path().join("data");
		// A chat with an explicit session directory, outside the buckets.
		let sessions = directory.path().join("explicit-sessions");
		let rollout = |name: &str, text: &str| {
			let path = root.join("sessions/2026/01").join(name);
			fs::create_dir_all(path.parent().unwrap()).unwrap();
			let lines = [
				serde_json::json!({"type": "session_meta", "payload": {"id": "codex-1", "cwd": "/project"}}),
				serde_json::json!({"type": "response_item", "payload": {"type": "message", "role": "user", "content": [{"type": "input_text", "text": text}]}}),
			];
			fs::write(&path, lines.map(|line| line.to_string() + "\n").concat()).unwrap();
			path
		};
		let original = rollout("rollout-a.jsonl", "ping");
		let resumed = rollout("rollout-b.jsonl", "ping again");
		let first = import_picked(ForeignFormat::Codex, &original, &fresh(&sessions), &data).unwrap();
		assert_eq!(
			import_picked(ForeignFormat::Codex, &original, &fresh(&sessions), &data).unwrap(),
			first
		);
		let rows = foreign_candidates(ForeignFormat::Codex, &root, &data, &sessions).unwrap();
		let prior = |path: &Path| {
			rows
				.iter()
				.find(|row| row.path == path)
				.and_then(|row| row.imported.clone())
		};
		assert_eq!(prior(&original), Some(ForeignImport::Current(first.clone())));
		assert_eq!(prior(&resumed), Some(ForeignImport::OtherFile(first.clone())));
		let own = import_picked(ForeignFormat::Codex, &resumed, &fresh(&sessions), &data).unwrap();
		assert_ne!(own, first);
		assert_eq!(journals(&sessions), {
			let mut both = vec![first, own];
			both.sort();
			both
		});
	}

	/// A row's id is the first one its records name, the one the importer
	/// records, so it finds that import.
	#[test]
	fn a_candidate_takes_the_first_session_id_its_records_name() {
		let directory = tempfile::tempdir().unwrap();
		let source = directory.path().join("resumed.jsonl");
		fs::write(
			&source,
			concat!(
				"{\"type\":\"user\",\"sessionId\":\"\",\"message\":{\"content\":\"a\"}}\n",
				"{\"type\":\"user\",\"sessionId\":\"original\",\"message\":{\"content\":\"b\"}}\n",
				"{\"type\":\"user\",\"sessionId\":\"resumed\",\"message\":{\"content\":\"c\"}}\n",
			),
		)
		.unwrap();
		let row = inspect_candidate(ForeignFormat::Claude, source.clone(), directory.path()).unwrap();
		assert_eq!(row.id, "original");
		let journal = directory.path().join("journal.oms");
		import_file(ForeignFormat::Claude, &source, &journal).unwrap();
		let entries = omp_journal::Journal::scan(&journal).unwrap();
		let origin = omp_session::import::import_origin(&entries).unwrap();
		assert_eq!(origin.source_id.as_deref(), Some("original"));
	}

	#[test]
	fn imported_session_records_source_selection_metadata() {
		let directory = tempfile::tempdir().unwrap();
		let source = directory.path().join("source.jsonl");
		let destination = directory.path().join("destination.oms");
		fs::write(&source, r#"{"type":"user","message":{"content":"hello"}}"#).unwrap();
		assert_eq!(import_file(ForeignFormat::Claude, &source, &destination).unwrap(), 1);
		let session = Session::open(&destination, ComponentRegistry::standard()).unwrap();
		let meta = session.dom().get(session.dom().meta()).unwrap();
		assert_eq!(
			meta
				.prop(&PropKey::Custom(Str::new_static(omp_session::import::IMPORT_SOURCE)))
				.and_then(DomValue::as_str),
			Some(source.to_string_lossy().as_ref())
		);
		assert_eq!(
			meta
				.prop(&PropKey::Custom(Str::new_static(omp_session::import::IMPORT_FORMAT)))
				.and_then(DomValue::as_str),
			Some("claude")
		);
	}
}
