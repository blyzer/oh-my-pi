//! Golden `.oms` corpus: journals written by this crate's real writer, checked
//! in under `fixtures/journal/`, replayed by today's reader.
//!
//! Each `<name>.oms` is paired with `<name>.snapshot.json`, the canonical DOM
//! snapshot the session folded to when the journal was written. The replay test
//! opens the checked-in bytes and requires the fold to reproduce that snapshot
//! exactly, which is the only way to pin old bytes against a new reader: a
//! change to the codec, the payload structs or the fold that no longer
//! understands yesterday's journal fails here.
//!
//! The corpus is append-only history. Add a journal when a kind or a payload
//! field is added; never regenerate an existing one, because that would defeat
//! the pin. The regenerator refuses to overwrite a fixture. To add the initial
//! corpus (or a new journal) run, from the repository root:
//!
//! ```text
//! cargo nextest run -p omp-session --test golden_journals \
//!     --run-ignored ignored-only regenerate_golden_journals
//! ```
//!
//! It writes `fixtures/journal/<name>.oms` and `<name>.snapshot.json` for each
//! journal built by [`corpus`], then the files are committed.

use std::{
	fs,
	path::{Path, PathBuf},
};

use omp_core::Str;
use omp_dom::{KnownTag, Op, PropId, PropKey, Tag, Txn, Value};
use omp_journal::{Journal, data::TurnReceipt};
use omp_session::{ComponentRegistry, Session, ToolReceipt};
use serde_json::value::RawValue;

/// The fewest journals the corpus may contain.
const MIN_FIXTURES: usize = 5;

/// Repository-relative corpus directory.
fn fixtures() -> PathBuf {
	Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/journal")
}

fn copy_tree(source: &Path, target: &Path) {
	if !source.is_dir() {
		return;
	}
	fs::create_dir_all(target).expect("create fixture tree");
	for entry in fs::read_dir(source).expect("read fixture tree") {
		let entry = entry.expect("fixture tree entry");
		let source_path = entry.path();
		let target_path = target.join(entry.file_name());
		if source_path.is_dir() {
			copy_tree(&source_path, &target_path);
		} else {
			fs::copy(source_path, target_path).expect("copy fixture blob");
		}
	}
}

const REGENERATE: &str = "cargo nextest run -p omp-session --test golden_journals --run-ignored \
                          ignored-only regenerate_golden_journals";

fn raw(value: serde_json::Value) -> Box<RawValue> {
	serde_json::value::to_raw_value(&value).expect("test JSON serializes")
}

fn latest_assistant(session: &Session) -> omp_dom::Handle {
	let turn = *session
		.dom()
		.children(session.dom().body())
		.last()
		.expect("a turn exists");
	session
		.dom()
		.children(turn)
		.iter()
		.rev()
		.copied()
		.find(|handle| {
			session
				.dom()
				.get(*handle)
				.is_some_and(|node| node.tag == Tag::Known(KnownTag::Assistant))
		})
		.expect("an assistant exists")
}

fn rename(session: &mut Session, name: &'static str) {
	let cause = session.head().expect("head");
	let meta = session.dom().meta();
	session
		.patch(Txn {
			cause,
			label: Some(Str::new_static("rename")),
			ops: vec![Op::Set {
				h:     meta,
				prop:  PropId::Name.into(),
				value: Value::Str(Str::new_static(name)),
			}],
		})
		.expect("rename patch");
}

/// A turn with a user message, a streamed assistant property, a receipt and a
/// session name.
fn chat_stream(session: &mut Session) {
	session.begin_turn().expect("turn");
	session.user("hello there", Vec::new()).expect("user");
	session
		.assistant_start("model-a", "provider-a", "route-a")
		.expect("assistant start");
	let assistant = latest_assistant(session);
	let sid = session
		.stream_open(assistant, PropKey::Custom(Str::new_static("note")))
		.expect("stream open");
	session.stream_append(sid, "Hello").expect("append");
	session.stream_append(sid, ", world").expect("append");
	session.stream_close(sid).expect("close");
	session.assistant_end("end_turn").expect("assistant end");
	session
		.receipt(TurnReceipt::tokens(12, 5, 3_400))
		.expect("receipt");
	rename(session, "Greeting");
}

/// Two tool calls (one settled, one failed, with kernel start and receipt
/// updates), then a rewind to the user message and a second branch, so the
/// journal carries an abandoned branch and a `prior` link.
fn tool_calls_and_rewind(session: &mut Session) {
	session.begin_turn().expect("turn");
	let user = session.user("list the files", Vec::new()).expect("user");
	session
		.assistant_start("model-b", "provider-b", "route-b")
		.expect("assistant start");
	session.assistant_end("tool_use").expect("assistant end");

	let read = session
		.call(
			"read",
			1,
			"call-1",
			Some(Str::new_static("inspect the tree")),
			Some(raw(serde_json::json!({ "path": "src/lib.rs" }))),
			None,
		)
		.expect("read call");
	session.call_started(read).expect("read started");
	session
		.call_receipt(read, ToolReceipt {
			source_bytes: 4_096,
			inline_bytes: 512,
			elapsed_ms:   17,
			outcome:      "ok",
		})
		.expect("read receipt");
	session
		.settle(read, raw(serde_json::json!({ "text": "fn main() {}" })))
		.expect("read settles");

	let bash = session
		.call("bash", 1, "call-2", None, Some(raw(serde_json::json!({ "command": "ls" }))), None)
		.expect("bash call");
	session.call_started(bash).expect("bash started");
	session
		.fail(bash, raw(serde_json::json!({ "code": "denied" })))
		.expect("bash fails");
	session
		.receipt(TurnReceipt::tokens(40, 11, 9_100))
		.expect("receipt");

	session.rewind(user).expect("rewind to the user message");
	session.begin_turn().expect("commit the branch selection");
	session
		.user("never mind, rename the session", Vec::new())
		.expect("second-branch user");
	session
		.assistant_start("model-b", "provider-b", "route-b")
		.expect("second-branch assistant");
	session.assistant_end("end_turn").expect("assistant end");
	rename(session, "Second branch");
}
fn compaction(session: &mut Session) {
	session.begin_turn().expect("turn");
	let boundary = session
		.user("old context", Vec::new())
		.expect("boundary user");
	let summary = session.blobs().put(b"compacted history").expect("summary");
	let mut compaction = omp_journal::data::Compaction::new(summary, boundary);
	compaction.method = Some(Str::new_static("auto"));
	compaction.tokens_before = Some(1_024);
	compaction.tokens_after = Some(256);
	compaction.warning = Some(Str::new_static("bounded fixture"));
	session.compaction(compaction).expect("compaction");
	session.begin_turn().expect("post-compaction turn");
	session
		.user("after compaction", Vec::new())
		.expect("post-compaction user");
}

fn tool_update(session: &mut Session) {
	session.begin_turn().expect("turn");
	session.user("run the tool", Vec::new()).expect("user");
	session
		.assistant_start("model-c", "provider-c", "route-c")
		.expect("assistant start");
	session.assistant_end("tool_use").expect("assistant end");
	let call = session
		.call(
			"progress",
			1,
			"call-update",
			None,
			Some(raw(serde_json::json!({ "task": "index" }))),
			None,
		)
		.expect("tool call");
	session.call_started(call).expect("tool started");
	session
		.call_update(call, raw(serde_json::json!({ "phase": "scan", "completed": 1 })))
		.expect("first update");
	session
		.call_update(call, raw(serde_json::json!({ "phase": "done", "completed": 2 })))
		.expect("second update");
	session
		.settle(call, raw(serde_json::json!({ "text": "indexed" })))
		.expect("tool settles");
}

fn patch(session: &mut Session) {
	session.begin_turn().expect("turn");
	session.user("patch the queue", Vec::new()).expect("user");
	let cause = session.head().expect("head");
	session
		.patch(Txn {
			cause,
			label: Some(Str::new_static("golden.patch")),
			ops: vec![Op::Set {
				h:     session.dom().queues(),
				prop:  PropId::Data.into(),
				value: Value::Json(raw(serde_json::json!({ "fixture": "patch", "enabled": true }))),
			}],
		})
		.expect("patch");
}

/// Builds one corpus journal on a fresh session.
type Build = fn(&mut Session);

/// Every journal in the corpus, by file stem.
fn corpus() -> [(&'static str, Build); MIN_FIXTURES] {
	[
		("chat-stream", chat_stream as Build),
		("tool-calls-and-rewind", tool_calls_and_rewind as Build),
		("compaction", compaction as Build),
		("tool-update", tool_update as Build),
		("patch", patch as Build),
	]
}

fn fixture_journals() -> Vec<PathBuf> {
	let Ok(entries) = fs::read_dir(fixtures()) else {
		return Vec::new();
	};
	let mut paths = entries
		.map(|entry| entry.expect("fixture entry").path())
		.filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("oms"))
		.collect::<Vec<_>>();
	paths.sort();
	paths
}

/// Opens every checked-in journal and requires the fold to equal the recorded
/// snapshot. An empty or short corpus is a failure, never a pass.
#[test]
fn golden_journals_open_and_fold_to_their_recorded_snapshots() {
	let journals = fixture_journals();
	assert!(
		journals.len() >= MIN_FIXTURES,
		"fixtures/journal/ holds {} journal(s) but the golden corpus needs at least {MIN_FIXTURES}; \
		 generate and commit them with: {REGENERATE}",
		journals.len(),
	);
	for fixture in journals {
		let name = fixture
			.file_stem()
			.and_then(|stem| stem.to_str())
			.expect("fixture name")
			.to_owned();
		let recorded = fs::read(fixture.with_extension("snapshot.json")).unwrap_or_else(|_| {
			panic!("{name}.oms has no {name}.snapshot.json beside it; the pair is committed together")
		});
		let bytes = fs::read(&fixture).expect("fixture reads");

		// Replay a copy: opening takes a writer lock, creates sidecars and a
		// blob store beside the journal, and must not touch the checked-in tree.
		let directory = tempfile::tempdir().expect("scratch directory");
		let copy = directory.path().join(format!("{name}.oms"));
		fs::write(&copy, &bytes).expect("copy fixture");

		copy_tree(&fixtures().join("blobs"), &directory.path().join("blobs"));

		let entries = Journal::scan(&copy).unwrap_or_else(|error| {
			panic!("{name}.oms no longer decodes: {error:?}");
		});
		assert!(entries.len() > 1, "{name}.oms holds no entries beyond its genesis");

		let session = Session::open(&copy, ComponentRegistry::standard())
			.unwrap_or_else(|error| panic!("{name}.oms no longer opens: {error:?}"));
		assert_eq!(
			String::from_utf8_lossy(session.dom().snapshot().as_bytes()),
			String::from_utf8_lossy(&recorded),
			"{name}.oms folds to a different snapshot than the one recorded when it was written",
		);
		drop(session);
		assert_eq!(
			fs::read(&copy).expect("copy reads"),
			bytes,
			"opening {name}.oms must not rewrite a complete journal",
		);
	}
}

/// Writes the corpus with the real writer. Ignored: it changes the working
/// tree, so it runs only on request, and refuses to overwrite a fixture.
#[test]
#[ignore = "writes fixtures/journal/*; run once to add the corpus, then commit the files"]
fn regenerate_golden_journals() {
	let target = fixtures();
	fs::create_dir_all(&target).expect("create fixtures/journal");
	for (name, build) in corpus() {
		let journal = target.join(format!("{name}.oms"));
		let snapshot = target.join(format!("{name}.snapshot.json"));
		if journal.exists() || snapshot.exists() {
			assert!(
				journal.exists() && snapshot.exists(),
				"{name} has only one side of its golden fixture pair",
			);
			continue;
		}

		let scratch = tempfile::tempdir().expect("scratch directory");
		let path = scratch.path().join(format!("{name}.oms"));
		let mut session =
			Session::create(&path, ComponentRegistry::standard()).expect("session creates");
		build(&mut session);
		let live = session.dom().snapshot();
		drop(session);

		let replayed = Session::open(&path, ComponentRegistry::standard()).expect("journal reopens");
		assert_eq!(
			replayed.dom().snapshot().as_bytes(),
			live.as_bytes(),
			"{name} must replay to the tree it folded live before it is recorded"
		);
		drop(replayed);

		fs::write(&snapshot, live.as_bytes()).expect("write snapshot");
		fs::copy(&path, &journal).expect("copy journal");
		copy_tree(&scratch.path().join("blobs"), &target.join("blobs"));
	}
}
