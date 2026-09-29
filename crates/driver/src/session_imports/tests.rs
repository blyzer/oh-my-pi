//! Index proofs over Claude Code and Codex transcripts, with journals carrying
//! exactly the provenance the app's importer records (whose conversion
//! `omp-app`'s `session_import` tests prove).

use std::{
	fs,
	path::{Path, PathBuf},
	time::{Duration, SystemTime},
};

use omp_core::{Hash32, Str, Ulid};
use omp_dom::{Op, PropKey, Txn, Value};
use omp_session::import::{
	IMPORT_FORMAT, IMPORT_SOURCE, IMPORT_SOURCE_BLOB, IMPORT_SOURCE_ID, STAMP_SETTLE, SourceStamp,
};
use serde_json::json;

use super::{DIGESTS, ImportedIndex, PriorImport};

const CLAUDE: &str = "claude";
const CODEX: &str = "codex";

/// How many transcripts this thread digested so far.
fn digests() -> usize {
	DIGESTS.with(std::cell::Cell::get)
}

fn write(path: &Path, contents: &str) {
	fs::create_dir_all(path.parent().expect("parent")).expect("parent dir");
	fs::write(path, contents).expect("write");
}

/// Sets `path`'s modification time a minute back and, on Unix, waits for
/// the change time that moved to now to settle: an import then records its
/// stamp. No call sets the change time, so only waiting settles it.
fn backdate(path: &Path) {
	let earlier = SystemTime::now() - Duration::from_secs(60);
	fs::File::options()
		.write(true)
		.open(path)
		.and_then(|file| file.set_modified(earlier))
		.expect("backdate");
	if cfg!(unix) {
		std::thread::sleep(STAMP_SETTLE + Duration::from_millis(50));
	}
}

/// Appends a line to a transcript, as its tool does while the session goes
/// on.
fn grow(transcript: &Path, line: &serde_json::Value) {
	let mut bytes = fs::read(transcript).expect("transcript");
	bytes.extend_from_slice(line.to_string().as_bytes());
	bytes.push(b'\n');
	fs::write(transcript, bytes).expect("grow");
}

/// A Claude Code transcript: every record names the session.
fn claude(path: &Path, session: &str) {
	let lines = [
		json!({"type": "user", "sessionId": session, "cwd": "/project", "message": {"role": "user", "content": "hello"}}),
		json!({"type": "assistant", "sessionId": session, "message": {"role": "assistant", "content": [{"type": "text", "text": "world"}]}}),
	];
	write(path, &lines.map(|line| line.to_string() + "\n").concat());
}

/// A Codex rollout: the session id is in its `session_meta` record.
fn codex(path: &Path, session: Option<&str>) {
	let mut lines = Vec::new();
	if let Some(session) = session {
		lines.push(json!({"type": "session_meta", "payload": {"id": session, "cwd": "/project"}}));
	}
	lines.push(json!({"type": "response_item", "payload": {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "ping"}]}}));
	write(
		path,
		&lines
			.iter()
			.map(|line| line.to_string() + "\n")
			.collect::<String>(),
	);
}

/// What one import recorded beyond the transcript itself.
#[derive(Clone, Copy)]
struct Import<'a> {
	format:    &'a str,
	source_id: Option<&'a str>,
	/// Whether the importer recorded the transcript's stamp.
	stamped:   bool,
}

/// Imports `source` into a fresh journal in `sessions`, recording the
/// provenance the app's importer records first: the transcript's path, its
/// digest, its source id when it names one, and its settled stamp.
fn import(sessions: &Path, source: &Path, how: Import<'_>) -> PathBuf {
	fs::create_dir_all(sessions).expect("sessions");
	let journal = sessions.join(format!("{}.oms", Ulid::generate()));
	let mut session =
		omp_session::Session::create(&journal, omp_session::ComponentRegistry::standard())
			.expect("journal");
	let meta = session.dom().meta();
	let cause = session.head().expect("genesis");
	let set = |prop: &'static str, value: &str| Op::Set {
		h:     meta,
		prop:  PropKey::Custom(Str::new_static(prop)),
		value: Value::Str(Str::new(value)),
	};
	let taken = SystemTime::now();
	let stat = fs::metadata(source).expect("stat");
	let bytes = fs::read(source).expect("source");
	let mut ops = vec![
		set(IMPORT_SOURCE, &source.to_string_lossy()),
		set(IMPORT_FORMAT, how.format),
		set(IMPORT_SOURCE_BLOB, &format!("artifact://sha256/{}", Hash32::sum(&bytes).to_hex())),
	];
	if let Some(id) = how.source_id {
		ops.push(set(IMPORT_SOURCE_ID, id));
	}
	if how.stamped
		&& let Some(stamp) =
			SourceStamp::of(&stat).and_then(|stamp| stamp.recordable(bytes.len() as u64, taken))
	{
		ops.extend(stamp.ops(meta));
	}
	session
		.patch(Txn { cause, label: None, ops })
		.expect("provenance");
	session.begin_turn().expect("turn");
	session.user("imported", Vec::new()).expect("message");
	journal
}

struct Fixture {
	root:     tempfile::TempDir,
	/// The v2 data directory.
	data:     PathBuf,
	/// A project bucket's session directory under it.
	sessions: PathBuf,
	/// Where the foreign tools keep their transcripts.
	foreign:  PathBuf,
}

impl Fixture {
	fn new() -> Self {
		let root = tempfile::tempdir().expect("scratch");
		let data = root.path().join("share/omp");
		let sessions = data.join("projects/0123abcd/sessions");
		let foreign = root.path().join("home");
		Self { root, data, sessions, foreign }
	}

	fn index(&self, format: &str) -> ImportedIndex {
		ImportedIndex::scan(&self.data, format).expect("index")
	}
}

#[test]
fn a_claude_session_is_current_changed_and_current_again_after_reimport() {
	let fixture = Fixture::new();
	let transcript = fixture
		.foreign
		.join(".claude/projects/-project/claude-1.jsonl");
	claude(&transcript, "claude-1");
	backdate(&transcript);
	let how = Import { format: CLAUDE, source_id: Some("claude-1"), stamped: true };
	assert_eq!(
		fixture
			.index(CLAUDE)
			.prior("claude-1", &transcript)
			.expect("prior"),
		None
	);
	let first = import(&fixture.sessions, &transcript, how);

	// Unchanged: current from its size and modification time alone.
	let before = digests();
	let index = fixture.index(CLAUDE);
	assert_eq!(index.len(), 1);
	assert_eq!(
		index.prior("claude-1", &transcript).expect("prior"),
		Some(PriorImport::Current(first.clone()))
	);
	assert_eq!(digests(), before, "an unchanged transcript is never digested");

	// The session went on: changed, and the earlier journal stays.
	grow(
		&transcript,
		&json!({"type": "user", "sessionId": "claude-1", "message": {"content": "more"}}),
	);
	assert_eq!(
		fixture
			.index(CLAUDE)
			.prior("claude-1", &transcript)
			.expect("prior"),
		Some(PriorImport::Changed(first.clone()))
	);
	assert_eq!(digests(), before + 1);

	// Importing it again makes the fresh journal the current one.
	backdate(&transcript);
	let second = import(&fixture.sessions, &transcript, how);
	let index = fixture.index(CLAUDE);
	assert_eq!(
		index.prior("claude-1", &transcript).expect("prior"),
		Some(PriorImport::Current(second.clone()))
	);
	assert_eq!(index.journal("claude-1"), Some(second.as_path()));
	assert!(first.is_file());

	// A deleted journal makes the transcript importable again.
	fs::remove_file(&first).expect("remove");
	fs::remove_file(&second).expect("remove");
	assert_eq!(
		fixture
			.index(CLAUDE)
			.prior("claude-1", &transcript)
			.expect("prior"),
		None
	);
}

#[test]
fn a_touched_but_unchanged_codex_rollout_is_digested_once_and_stays_current() {
	let fixture = Fixture::new();
	let rollout = fixture
		.foreign
		.join(".codex/sessions/2026/01/02/rollout-a.jsonl");
	codex(&rollout, Some("codex-1"));
	backdate(&rollout);
	let journal = import(&fixture.sessions, &rollout, Import {
		format:    CODEX,
		source_id: Some("codex-1"),
		stamped:   true,
	});

	let before = digests();
	fs::File::options()
		.write(true)
		.open(&rollout)
		.and_then(|file| file.set_modified(SystemTime::now()))
		.expect("touch");
	assert_eq!(
		fixture
			.index(CODEX)
			.prior("codex-1", &rollout)
			.expect("prior"),
		Some(PriorImport::Current(journal.clone()))
	);
	assert_eq!(digests(), before + 1, "a moved stamp falls back to the digest");

	// Rewritten in place at the same size: the digest finds the change.
	let bytes = fs::read(&rollout).expect("rollout");
	let edited = String::from_utf8(bytes)
		.expect("utf-8")
		.replace("ping", "pong");
	fs::write(&rollout, edited).expect("rewrite");
	assert_eq!(
		fixture
			.index(CODEX)
			.prior("codex-1", &rollout)
			.expect("prior"),
		Some(PriorImport::Changed(journal))
	);
}

#[test]
fn an_unstamped_import_falls_back_to_the_digest() {
	let fixture = Fixture::new();
	let transcript = fixture
		.foreign
		.join(".claude/projects/-project/claude-2.jsonl");
	claude(&transcript, "claude-2");
	backdate(&transcript);
	let journal = import(&fixture.sessions, &transcript, Import {
		format:    CLAUDE,
		source_id: Some("claude-2"),
		stamped:   false,
	});
	let before = digests();
	assert_eq!(
		fixture
			.index(CLAUDE)
			.prior("claude-2", &transcript)
			.expect("prior"),
		Some(PriorImport::Current(journal))
	);
	assert_eq!(digests(), before + 1, "no stamp to trust: digested");
}

#[test]
fn another_file_of_a_codex_session_is_its_own_import() {
	let fixture = Fixture::new();
	let rollout = fixture
		.foreign
		.join(".codex/sessions/2026/01/02/rollout-a.jsonl");
	codex(&rollout, Some("codex-1"));
	// The session went on in a second rollout carrying the same id.
	let resumed = fixture
		.foreign
		.join(".codex/sessions/2026/01/03/rollout-b.jsonl");
	codex(&resumed, Some("codex-1"));
	grow(
		&resumed,
		&json!({"type": "response_item", "payload": {"type": "message", "role": "user", "content": "again"}}),
	);
	let how = Import { format: CODEX, source_id: Some("codex-1"), stamped: true };
	let first = import(&fixture.sessions, &rollout, how);
	let index = fixture.index(CODEX);
	assert_eq!(
		index.prior("codex-1", &rollout).expect("prior"),
		Some(PriorImport::Current(first.clone()))
	);
	assert_eq!(
		index.prior("codex-1", &resumed).expect("prior"),
		Some(PriorImport::OtherFile(first.clone()))
	);

	// Imported on its own, each file keeps its own journal; a change to one
	// is a change to that file's import only.
	let own = import(&fixture.sessions, &resumed, how);
	grow(
		&rollout,
		&json!({"type": "response_item", "payload": {"type": "message", "role": "user", "content": "later"}}),
	);
	let index = fixture.index(CODEX);
	assert_eq!(index.prior("codex-1", &resumed).expect("prior"), Some(PriorImport::Current(own)));
	assert_eq!(index.prior("codex-1", &rollout).expect("prior"), Some(PriorImport::Changed(first)));
}

#[test]
fn an_identical_copy_of_a_claude_transcript_is_current_by_its_digest() {
	let fixture = Fixture::new();
	let transcript = fixture
		.foreign
		.join(".claude/projects/-project/claude-3.jsonl");
	claude(&transcript, "claude-3");
	let copy = fixture
		.foreign
		.join(".claude/.projects/-project/claude-3.jsonl");
	fs::create_dir_all(copy.parent().expect("parent")).expect("dir");
	fs::copy(&transcript, &copy).expect("copy");
	let journal = import(&fixture.sessions, &transcript, Import {
		format:    CLAUDE,
		source_id: Some("claude-3"),
		stamped:   true,
	});
	assert_eq!(
		fixture
			.index(CLAUDE)
			.prior("claude-3", &copy)
			.expect("prior"),
		Some(PriorImport::Current(journal))
	);
}

#[test]
fn a_journal_without_a_source_id_is_keyed_by_its_transcript_stem() {
	let fixture = Fixture::new();
	let rollout = fixture
		.foreign
		.join(".codex/sessions/rollout-anonymous.jsonl");
	codex(&rollout, None);
	backdate(&rollout);
	let journal = import(&fixture.sessions, &rollout, Import {
		format:    CODEX,
		source_id: None,
		stamped:   true,
	});
	assert_eq!(
		fixture
			.index(CODEX)
			.prior("rollout-anonymous", &rollout)
			.expect("prior"),
		Some(PriorImport::Current(journal))
	);
}

#[test]
fn each_format_indexes_only_its_own_imports() {
	let fixture = Fixture::new();
	let transcript = fixture
		.foreign
		.join(".claude/projects/-project/shared-id.jsonl");
	claude(&transcript, "shared-id");
	let journal = import(&fixture.sessions, &transcript, Import {
		format:    CLAUDE,
		source_id: Some("shared-id"),
		stamped:   true,
	});
	assert!(fixture.index(CODEX).is_empty());
	assert!(fixture.index(omp_session::import::OMP1_FORMAT).is_empty());
	assert_eq!(fixture.index(CLAUDE).journal("shared-id"), Some(journal.as_path()));
}

#[test]
fn an_explicit_session_directory_is_indexed_once() {
	let fixture = Fixture::new();
	let transcript = fixture
		.foreign
		.join(".claude/projects/-project/claude-4.jsonl");
	claude(&transcript, "claude-4");
	let how = Import { format: CLAUDE, source_id: Some("claude-4"), stamped: true };
	let bucketed = import(&fixture.sessions, &transcript, how);
	let explicit = fixture.root.path().join("explicit-sessions");
	let outside = import(&explicit, &transcript, how);
	// A staged journal is not an import yet.
	fs::copy(&outside, explicit.join(".01STAGED.importing.oms")).expect("staged");

	let mut index = fixture.index(CLAUDE);
	assert_eq!(index.journal("claude-4"), Some(bucketed.as_path()));
	index.scan_sessions(&explicit).expect("explicit");
	assert_eq!(index.journal("claude-4"), Some(outside.as_path()));
	// Naming a directory again, or a bucket's, adds nothing twice.
	index.scan_sessions(&explicit).expect("again");
	index.scan_sessions(&fixture.sessions).expect("bucket");
	assert_eq!(index.by_id.get("claude-4").map(|journals| journals.len()), Some(2));
	// A directory that does not exist yet is empty.
	index
		.scan_sessions(&fixture.root.path().join("missing"))
		.expect("missing");
}
