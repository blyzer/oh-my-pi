//! The write-back queue against a scripted editor (ADR 0037 §4.3–§4.6).

use std::sync::atomic::{AtomicUsize, Ordering};

use super::*;

/// An editor holding one buffer per path; every request is logged.
#[derive(Default)]
struct Editor {
	buffers:     Mutex<FastHashMap<Str, Str>>,
	/// `read <path>` / `write <path>`, in order.
	log:         Mutex<Vec<String>>,
	/// Applied to every written buffer, like a client's format-on-save.
	formatter:   Option<fn(&str) -> String>,
	/// Upcoming writes that fail.
	fail_writes: AtomicUsize,
	/// Each write of a path ending in the given name waits for one release on
	/// the gate.
	gate:        Option<(&'static str, flume::Receiver<()>)>,
}

impl Editor {
	fn set(&self, path: &str, text: &str) {
		self.buffers.lock().insert(Str::new(path), Str::new(text));
	}

	fn buffer(&self, path: &str) -> Option<Str> {
		self.buffers.lock().get(path).cloned()
	}

	fn log(&self) -> Vec<String> {
		self.log.lock().clone()
	}
}

struct Files(Arc<Editor>);

impl EditorFiles for Files {
	fn read_text(&self, path: Str) -> impl Future<Output = Result<Str, EditorIoError>> + Send + '_ {
		self.0.log.lock().push(format!("read {path}"));
		std::future::ready(self.0.buffer(&path).ok_or(EditorIoError::Refused))
	}

	async fn write_text(&self, path: Str, content: Str) -> Result<(), EditorIoError> {
		if let Some((name, gate)) = &self.0.gate
			&& path.ends_with(name)
		{
			gate
				.recv_async()
				.await
				.map_err(|_| EditorIoError::Disconnected)?;
		}
		self.0.log.lock().push(format!("write {path}"));
		if self
			.0
			.fail_writes
			.try_update(Ordering::SeqCst, Ordering::SeqCst, |left| left.checked_sub(1))
			.is_ok()
		{
			return Err(EditorIoError::Refused);
		}
		let stored = self
			.0
			.formatter
			.map_or_else(|| content.clone(), |format| Str::new(format(&content)));
		self.0.buffers.lock().insert(path, stored);
		Ok(())
	}
}

const BOTH: EditorCapabilities = EditorCapabilities { read: true, write: true };
const WRITES_ONLY: EditorCapabilities = EditorCapabilities { read: false, write: true };

fn backend(editor: &Arc<Editor>, capabilities: EditorCapabilities) -> EditorBackend<Files> {
	EditorBackend::new(Files(Arc::clone(editor)), capabilities, Duration::from_secs(2))
}

fn job(path: &str, content: &str, base: Option<&str>) -> WriteBack {
	WriteBack { path: Str::new(path), content: Str::new(content), base: base.map(Str::new) }
}

async fn outcome(
	receiver: flume::Receiver<Result<WriteBackOutcome, EditorIoError>>,
) -> Result<WriteBackOutcome, EditorIoError> {
	time::timeout(Duration::from_secs(5), receiver.recv_async())
		.await
		.expect("the write-back finishes in time")
		.expect("the queue reports every write-back")
}

const fn written() -> WriteBackOutcome {
	WriteBackOutcome::Written { merged: None, read_back: None }
}

#[test]
fn the_pre_write_plan_keeps_the_users_later_changes() {
	let committed = Str::new_static("a\r\nAGENT\r\nc\r\n");
	// Nothing typed since the base: R as is.
	assert_eq!(plan("a\nb\nc\n", &committed, "a\r\nb\r\nc\r\n"), Plan::Write(committed.clone()));
	// The editor already shows R.
	assert_eq!(plan("a\nb\nc\n", &committed, "a\nAGENT\nc\n"), Plan::Current(committed.clone()));
	// A non-overlapping keystroke is merged onto R, in R's encoding.
	assert_eq!(
		plan("a\nb\nc\n", &committed, "a\nb\nc\nuser\n"),
		Plan::Write(Str::new_static("a\r\nAGENT\r\nc\r\nuser\r\n"))
	);
	// An overlapping keystroke conflicts in the base's coordinates.
	let Plan::Conflict(ranges) = plan("a\nb\nc\n", &committed, "a\nUSER\nc\n") else {
		panic!("an overlapping change conflicts");
	};
	assert!(!ranges.is_empty());
	assert!(
		ranges
			.iter()
			.all(|range| range.start() >= 2 && range.end() <= 4),
		"{ranges:?}"
	);
}

/// A clean write-back writes R, reads it back, and reports no drift.
#[tokio::test]
async fn a_clean_write_back_writes_the_committed_bytes() {
	let editor = Arc::new(Editor::default());
	editor.set("/w/a.txt", "old\n");
	let queue = backend(&editor, BOTH);
	let result = outcome(queue.submit(job("/w/a.txt", "new\n", Some("old\n")))).await;
	assert_eq!(result, Ok(written()));
	assert_eq!(editor.buffer("/w/a.txt").as_deref(), Some("new\n"));
	assert_eq!(editor.log(), ["read /w/a.txt", "write /w/a.txt", "read /w/a.txt"]);
	assert_eq!(queue.outstanding(), 0);
}

/// Keystrokes typed during the turn are merged by the pre-write re-read; an
/// overlapping one skips the write-back with typed ranges.
#[tokio::test]
async fn the_pre_write_re_read_merges_or_conflicts() {
	let editor = Arc::new(Editor::default());
	let queue = backend(&editor, BOTH);
	editor.set("/w/merge.txt", "one\ntwo\nthree\nlater\n");
	let merged =
		outcome(queue.submit(job("/w/merge.txt", "ONE\ntwo\nthree\n", Some("one\ntwo\nthree\n"))))
			.await;
	assert_eq!(
		merged,
		Ok(WriteBackOutcome::Written {
			merged:    Some(Str::new_static("ONE\ntwo\nthree\nlater\n")),
			read_back: None,
		})
	);
	assert_eq!(editor.buffer("/w/merge.txt").as_deref(), Some("ONE\ntwo\nthree\nlater\n"));

	editor.set("/w/conflict.txt", "uno\ntwo\nthree\n");
	let conflict =
		outcome(queue.submit(job("/w/conflict.txt", "ONE\ntwo\nthree\n", Some("one\ntwo\nthree\n"))))
			.await;
	let Ok(WriteBackOutcome::Conflict { ranges }) = conflict else {
		panic!("overlapping keystrokes conflict: {conflict:?}");
	};
	assert!(ranges.iter().all(|range| range.end() <= 4), "line 1 of the base: {ranges:?}");
	assert_eq!(editor.buffer("/w/conflict.txt").as_deref(), Some("uno\ntwo\nthree\n"), "untouched");
	assert!(!editor.log().contains(&"write /w/conflict.txt".to_owned()));
}

/// The read-back after the write reports client reformatting.
#[tokio::test]
async fn the_read_back_reports_client_formatting() {
	let editor =
		Arc::new(Editor { formatter: Some(|text| text.replace("  ", "\t")), ..Editor::default() });
	editor.set("/w/f.rs", "fn f() {}\n");
	let queue = backend(&editor, BOTH);
	let result =
		outcome(queue.submit(job("/w/f.rs", "fn f() {\n  g();\n}\n", Some("fn f() {}\n")))).await;
	assert_eq!(
		result,
		Ok(WriteBackOutcome::Written {
			merged:    None,
			read_back: Some(Str::new_static("fn f() {\n\tg();\n}\n")),
		})
	);
}

/// Without `readTextFile` the bytes are written as is: no re-read, no
/// read-back. Without `writeTextFile` nothing is written.
#[tokio::test]
async fn capabilities_gate_every_request() {
	let editor = Arc::new(Editor::default());
	editor.set("/w/a.txt", "typed meanwhile\n");
	let queue = backend(&editor, WRITES_ONLY);
	assert_eq!(outcome(queue.submit(job("/w/a.txt", "new\n", Some("old\n")))).await, Ok(written()));
	assert_eq!(editor.log(), ["write /w/a.txt"]);
	assert_eq!(queue.read(Str::new_static("/w/a.txt")).await, Err(EditorIoError::Unavailable));
	let unwritable = backend(&editor, EditorCapabilities { read: true, write: false });
	assert_eq!(
		outcome(unwritable.submit(job("/w/a.txt", "x\n", None))).await,
		Err(EditorIoError::Unavailable)
	);
	assert_eq!(editor.log(), ["write /w/a.txt"], "nothing reached the editor");
}

/// Write-backs of one path run first in first out; a newer one supersedes
/// one still waiting; other paths proceed meanwhile; and a failure does not
/// stop the queue.
#[tokio::test]
async fn write_backs_queue_per_path_with_supersession() {
	let (release, gate) = flume::unbounded();
	let editor = Arc::new(Editor { gate: Some(("a.txt", gate)), ..Editor::default() });
	let queue = backend(&editor, WRITES_ONLY);

	let first = queue.submit(job("/w/a.txt", "a1\n", None));
	let second = queue.submit(job("/w/a.txt", "a2\n", None));
	let third = queue.submit(job("/w/a.txt", "a3\n", None));
	assert_eq!(
		outcome(second).await,
		Ok(WriteBackOutcome::Superseded),
		"a newer revision replaces one not yet sent"
	);
	assert_eq!(queue.outstanding(), 2, "the running write-back and the latest waiting one");

	// Another path proceeds while a.txt's first write is held.
	assert_eq!(outcome(queue.submit(job("/w/b.txt", "b1\n", None))).await, Ok(written()));
	assert_eq!(editor.buffer("/w/b.txt").as_deref(), Some("b1\n"));

	editor.fail_writes.store(1, Ordering::SeqCst);
	release.send(()).expect("release the first write");
	assert_eq!(outcome(first).await, Err(EditorIoError::Refused), "the editor refused it");
	release.send(()).expect("release the waiting write");
	assert_eq!(outcome(third).await, Ok(written()), "the queue continues after a failure");
	let order: Vec<_> = editor
		.log()
		.into_iter()
		.filter(|entry| entry.ends_with("a.txt"))
		.collect();
	assert_eq!(order, ["write /w/a.txt", "write /w/a.txt"], "a1 then a3; a2 is never sent");
	assert_eq!(editor.buffer("/w/a.txt").as_deref(), Some("a3\n"));
	assert!(queue.drain(Duration::from_secs(1)).await);
}

/// A write-back belongs to the queue, not to its caller: dropping the
/// caller's future leaves it running, and a drain waits for it within its
/// bound.
#[tokio::test]
async fn dropping_the_caller_never_cancels_a_queued_write_back() {
	let (release, gate) = flume::unbounded();
	let editor = Arc::new(Editor { gate: Some(("a.txt", gate)), ..Editor::default() });
	let queue = backend(&editor, WRITES_ONLY);
	let backend: &dyn AcpDocumentBackend = &queue;
	drop(backend.write_back(job("/w/a.txt", "committed\n", None)));
	assert_eq!(queue.outstanding(), 1, "queued when the call returned");
	assert!(!queue.drain(Duration::from_millis(50)).await, "the drain is bounded");
	release.send(()).expect("release the write");
	assert!(queue.drain(Duration::from_secs(5)).await);
	assert_eq!(editor.buffer("/w/a.txt").as_deref(), Some("committed\n"));
}
