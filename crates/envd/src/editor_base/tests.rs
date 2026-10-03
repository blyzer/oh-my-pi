//! Base selection against a fake editor and a real document authority.

use std::{
	collections::HashMap,
	fs,
	future::Future,
	path::PathBuf,
	pin::Pin,
	sync::atomic::{AtomicUsize, Ordering},
};

use omp_tools::{
	edit::{
		EditAction, EditDocuments, EditPrepared, EditProposal, FormatPolicy, PrepareRequest,
		RejectionReason, StalePolicy,
	},
	read::{ReadLease, ReadSources},
	write::{PlainWriteRequest, WriteDocuments},
};
use proptest::prelude::*;
use tokio::io::duplex;

use super::*;
use crate::{
	docserver::{
		Environment, ServerConfig,
		connection::{ConnectionConfig, serve_connection},
	},
	document_cache::project_document_cache,
	tool_document::{read_document_metadata, read_whole},
	tool_read_sources::ReadSourceAdapter,
	tools::{InvocationAcpBackends, with_acp_scope},
	workspace::WorkspaceHost,
};

/// An editor answering `fs/read_text_file` from an in-memory buffer map.
#[derive(Default)]
struct FakeEditor {
	buffers: Mutex<HashMap<PathBuf, Result<Str, EditorIoError>>>,
	reads:   AtomicUsize,
	writes:  AtomicUsize,
	delay:   Option<Duration>,
}

impl AcpDocumentBackend for FakeEditor {
	fn deadline(&self) -> Duration {
		Duration::from_millis(250)
	}

	fn read_text(
		&self,
		absolute_path: Str,
	) -> Pin<Box<dyn Future<Output = Result<Str, EditorIoError>> + Send + '_>> {
		Box::pin(async move {
			self.reads.fetch_add(1, Ordering::SeqCst);
			if let Some(delay) = self.delay {
				time::sleep(delay).await;
			}
			self
				.buffers
				.lock()
				.get(Path::new(absolute_path.as_str()))
				.cloned()
				.unwrap_or(Err(EditorIoError::Refused))
		})
	}

	fn write_text(
		&self,
		_absolute_path: Str,
		content: Str,
	) -> Pin<Box<dyn Future<Output = Result<Str, EditorIoError>> + Send + '_>> {
		Box::pin(async move {
			self.writes.fetch_add(1, Ordering::SeqCst);
			Ok(content)
		})
	}
}

struct Fixture {
	_root:       tempfile::TempDir,
	root:        PathBuf,
	environment: Environment,
	host:        DocumentHost,
	editor:      Arc<FakeEditor>,
	session:     Arc<EditorSession>,
}

async fn connect(environment: &Environment) -> DocumentHost {
	let (client, server) = duplex(1 << 20);
	tokio::spawn(serve_connection(environment.clone(), server, ConnectionConfig::default()));
	DocumentHost::connect(client).await.expect("document hello")
}

impl Fixture {
	async fn new(files: &[(&str, &[u8])]) -> Self {
		Self::with_editor(files, FakeEditor::default()).await
	}

	async fn with_editor(files: &[(&str, &[u8])], editor: FakeEditor) -> Self {
		let directory = tempfile::tempdir().expect("workspace");
		let root = fs::canonicalize(directory.path()).expect("canonical workspace");
		for (name, content) in files {
			fs::write(root.join(name), content).expect("fixture file");
		}
		let environment =
			Environment::new(ServerConfig::new(&root).expect("docserver config")).expect("authority");
		let host = connect(&environment).await;
		let editor = Arc::new(editor);
		let session = Arc::new(EditorSession::new(editor.deadline()));
		Self { _root: directory, root, environment, host, editor, session }
	}

	fn path(&self, name: &str) -> PathBuf {
		self.root.join(name)
	}

	fn key(&self, name: &str) -> Str {
		Str::new(self.path(name).to_str().expect("UTF-8 path"))
	}

	fn buffer(&self, name: &str, buffer: Result<&str, EditorIoError>) {
		self
			.editor
			.buffers
			.lock()
			.insert(self.path(name), buffer.map(Str::new));
	}

	fn route(&self) -> InvocationAcpBackends {
		InvocationAcpBackends::new(
			Some(EditorRoute::new(
				Arc::clone(&self.editor) as Arc<dyn AcpDocumentBackend>,
				Arc::clone(&self.session),
			)),
			None,
		)
	}

	async fn scoped<T>(&self, future: impl Future<Output = T>) -> T {
		with_acp_scope(self.route(), future).await
	}

	async fn lease(&self, name: &str) -> crate::docs::DocumentLease {
		let uri = Url::from_file_path(self.path(name)).expect("file URI");
		self
			.host
			.open(Str::new(uri.as_str()), None, &tokio_util::sync::CancellationToken::new())
			.await
			.expect("open lease")
	}

	async fn base(&self, name: &str) -> Result<EffectiveBase, EditorConflict> {
		let lease = self.lease(name).await;
		let disk = read_whole(&self.host, &lease).await.expect("disk bytes");
		self
			.scoped(self.host.editor_base(lease.head(), &disk))
			.await
	}

	fn reads(&self) -> usize {
		self.editor.reads.load(Ordering::SeqCst)
	}

	fn prepare_request(name: &str) -> PrepareRequest {
		PrepareRequest {
			path:            Str::new(name),
			file_hash:       None,
			anchor_lines:    Vec::new(),
			allow_unpinned:  true,
			allow_missing:   false,
			guard_generated: false,
		}
	}
}

fn kinds(diags: &[Diag]) -> Vec<Str> {
	diags.iter().map(|diag| diag.kind.clone()).collect()
}

fn kind(kind: DiagKind) -> Str {
	Str::new_static(kind.into())
}

/// What one table row expects of the effective base.
#[derive(Debug)]
enum Expect {
	/// Disk is the base; these diag kinds are attached.
	Disk(&'static [DiagKind]),
	/// E is these bytes; these diag kinds are attached.
	Editor(&'static str, &'static [DiagKind]),
	/// The anchored merge conflicts at these (buffer, disk) line spans.
	Conflict(&'static [((usize, usize), (usize, usize))]),
}

struct Row {
	name:     &'static str,
	disk:     &'static str,
	buffer:   Result<&'static str, EditorIoError>,
	anchor:   Option<&'static str>,
	retained: Option<&'static str>,
	expect:   Expect,
	/// The anchor after selection (before any commit).
	anchored: Option<&'static str>,
}

const ANCESTOR: &str = "one\ntwo\nthree\nfour\nfive\n";

fn rows() -> Vec<Row> {
	use DiagKind::{EditorBufferUnanchored as Unanchored, EditorBufferUnavailable as Unavailable};
	vec![
		Row {
			name:     "clean: B = D, disk is the base and anchors",
			disk:     "a\nb\n",
			buffer:   Ok("a\nb\n"),
			anchor:   None,
			retained: None,
			expect:   Expect::Disk(&[]),
			anchored: Some("a\nb\n"),
		},
		Row {
			name:     "stale-clean via K: the editor has not reloaded, disk wins",
			disk:     "a\nB\n",
			buffer:   Ok("a\nb\n"),
			anchor:   Some("a\nb\n"),
			retained: None,
			expect:   Expect::Disk(&[]),
			anchored: Some("a\nb\n"),
		},
		Row {
			name:     "stale-clean via a retained disk revision",
			disk:     "a\nB\n",
			buffer:   Ok("a\nb\n"),
			anchor:   None,
			retained: Some("a\nb\n"),
			expect:   Expect::Disk(&[]),
			anchored: None,
		},
		Row {
			name:     "dirty anchored: the user's delta is replayed onto disk",
			disk:     "one\nTWO\nthree\nfour\nfive\n",
			buffer:   Ok("one\ntwo\nthree\nfour\nFIVE\n"),
			anchor:   Some(ANCESTOR),
			retained: None,
			expect:   Expect::Editor("one\nTWO\nthree\nfour\nFIVE\n", &[DiagKind::Provenance]),
			anchored: Some(ANCESTOR),
		},
		Row {
			name:     "dirty anchored conflict: typed ranges, no base",
			disk:     "one\nTWO\nthree\nfour\nfive\n",
			buffer:   Ok("one\nzwei\nthree\nfour\nfive\n"),
			anchor:   Some(ANCESTOR),
			retained: None,
			expect:   Expect::Conflict(&[((2, 2), (2, 2))]),
			anchored: Some(ANCESTOR),
		},
		Row {
			name:     "first contact: the unanchored buffer wins with a warning",
			disk:     "a\nb\n",
			buffer:   Ok("a\nc\n"),
			anchor:   None,
			retained: None,
			expect:   Expect::Editor("a\nc\n", &[Unanchored, DiagKind::Provenance]),
			anchored: Some("a\nb\n"),
		},
		Row {
			name:     "line endings only: clean, and the anchor is normalized",
			disk:     "a\r\nb\r\n",
			buffer:   Ok("a\nb\n"),
			anchor:   None,
			retained: None,
			expect:   Expect::Disk(&[]),
			anchored: Some("a\nb\n"),
		},
		Row {
			name:     "BOM only: clean",
			disk:     "\u{FEFF}a\nb\n",
			buffer:   Ok("a\nb\n"),
			anchor:   None,
			retained: None,
			expect:   Expect::Disk(&[]),
			anchored: Some("a\nb\n"),
		},
		Row {
			name:     "dirty buffer takes disk's CRLF and BOM",
			disk:     "\u{FEFF}a\r\nb\r\n",
			buffer:   Ok("a\nc\n"),
			anchor:   None,
			retained: None,
			expect:   Expect::Editor("\u{FEFF}a\r\nc\r\n", &[Unanchored, DiagKind::Provenance]),
			anchored: Some("a\nb\n"),
		},
		Row {
			name:     "timeout: disk with a warning",
			disk:     "a\n",
			buffer:   Err(EditorIoError::Timeout),
			anchor:   None,
			retained: None,
			expect:   Expect::Disk(&[Unavailable]),
			anchored: None,
		},
		Row {
			name:     "refused: disk with a warning",
			disk:     "a\n",
			buffer:   Err(EditorIoError::Refused),
			anchor:   None,
			retained: None,
			expect:   Expect::Disk(&[Unavailable]),
			anchored: None,
		},
		Row {
			name:     "malformed answer: disk with a warning",
			disk:     "a\n",
			buffer:   Err(EditorIoError::Malformed),
			anchor:   Some("a\n"),
			retained: None,
			expect:   Expect::Disk(&[Unavailable]),
			anchored: Some("a\n"),
		},
	]
}

#[tokio::test]
async fn base_selection_follows_the_adr_table() {
	for row in rows() {
		let fixture = Fixture::new(&[("doc.txt", row.disk.as_bytes())]).await;
		let key = fixture.key("doc.txt");
		if let Some(anchor) = row.anchor {
			fixture
				.session
				.anchor(&key, Bytes::from_static(anchor.as_bytes()));
		}
		if let Some(retained) = row.retained {
			fixture.session.retain(&key, retained);
		}
		fixture.buffer("doc.txt", row.buffer);
		let result = fixture.base("doc.txt").await;
		assert_eq!(fixture.reads(), 1, "{}: exactly one buffer read", row.name);
		match (&row.expect, result) {
			(Expect::Disk(expected), Ok(base)) => {
				assert_eq!(base.bytes, None, "{}: disk is the base", row.name);
				let expected = expected.iter().map(|diag| kind(*diag)).collect::<Vec<_>>();
				assert_eq!(kinds(&base.diags), expected, "{}", row.name);
			},
			(Expect::Editor(bytes, expected), Ok(base)) => {
				assert_eq!(
					base.bytes.as_deref(),
					Some(bytes.as_bytes()),
					"{}: effective base",
					row.name
				);
				let expected = expected.iter().map(|diag| kind(*diag)).collect::<Vec<_>>();
				assert_eq!(kinds(&base.diags), expected, "{}", row.name);
				assert!(base.on_commit.is_some(), "{}: the commit anchors B", row.name);
			},
			(Expect::Conflict(spans), Err(conflict)) => {
				assert!(!conflict.ranges.is_empty(), "{}: typed ranges", row.name);
				let actual = conflict
					.lines
					.iter()
					.map(|span| (span.buffer, span.disk))
					.collect::<Vec<_>>();
				assert_eq!(actual, spans.to_vec(), "{}", row.name);
				assert_eq!(conflict.diag().kind, kind(DiagKind::EditorBufferConflict), "{}", row.name);
			},
			(expect, result) => panic!("{}: expected {expect:?}, got {result:?}", row.name),
		}
		assert_eq!(
			fixture.session.anchor_of(key.as_str()).as_deref(),
			row.anchored.map(str::as_bytes),
			"{}: anchor after selection",
			row.name
		);
	}
}

/// After first contact, a disk change is merged under the user's unsaved
/// changes (or conflicts); the buffer never reverts it wholesale.
#[tokio::test]
async fn a_disk_change_after_first_contact_is_merged_not_reverted() {
	let fixture = Fixture::new(&[("doc.txt", ANCESTOR.as_bytes())]).await;
	let dirty = "one\ntwo\nthree\nfour\nFIVE\n";
	fixture.buffer("doc.txt", Ok(dirty));
	let first = fixture.base("doc.txt").await.expect("first contact");
	assert_eq!(first.bytes.as_deref(), Some(dirty.as_bytes()));
	assert_eq!(kinds(&first.diags), [
		kind(DiagKind::EditorBufferUnanchored),
		kind(DiagKind::Provenance)
	]);
	// Another writer changes line 1 on disk after the read.
	fs::write(fixture.path("doc.txt"), "ONE\ntwo\nthree\nfour\nfive\n").expect("disk change");
	let second = fixture.base("doc.txt").await.expect("merged");
	assert_eq!(
		second.bytes.as_deref(),
		Some(&b"ONE\ntwo\nthree\nfour\nFIVE\n"[..]),
		"the disk change survives under the user's delta"
	);
	assert_eq!(kinds(&second.diags), [kind(DiagKind::Provenance)], "warned once, at first contact");
}

#[tokio::test]
async fn an_oversize_or_slow_buffer_falls_back_to_disk() {
	let fixture = Fixture::new(&[("doc.txt", b"a\n")]).await;
	let oversize = "x".repeat(SNAPSHOT_MAX_BYTES + 1);
	fixture
		.editor
		.buffers
		.lock()
		.insert(fixture.path("doc.txt"), Ok(Str::from(oversize)));
	let base = fixture
		.base("doc.txt")
		.await
		.expect("oversize is not a conflict");
	assert_eq!(base.bytes, None);
	assert_eq!(kinds(&base.diags), [kind(DiagKind::EditorBufferUnavailable)]);
	assert!(base.diags[0].text.contains("oversize"), "{}", base.diags[0].text);

	let slow = Fixture::with_editor(&[("doc.txt", b"a\n")], FakeEditor {
		delay: Some(Duration::from_secs(5)),
		..FakeEditor::default()
	})
	.await;
	slow.buffer("doc.txt", Ok("b\n"));
	let started = std::time::Instant::now();
	let base = slow
		.base("doc.txt")
		.await
		.expect("timeout is not a conflict");
	assert!(started.elapsed() < Duration::from_secs(4), "the binding's deadline bounds the wait");
	assert_eq!(base.bytes, None);
	assert!(base.diags[0].text.contains("timeout"), "{}", base.diags[0].text);
}

#[tokio::test]
async fn ineligible_documents_and_unbound_invocations_never_ask_the_editor() {
	let big = vec![b'x'; SNAPSHOT_MAX_BYTES + 1];
	let fixture = Fixture::new(&[
		("big.txt", &big),
		("binary.bin", b"\xff\xfe\x00\x01"),
		("nb.ipynb", br#"{"cells": [], "metadata": {}, "nbformat": 4, "nbformat_minor": 5}"#),
		("doc.txt", b"a\n"),
	])
	.await;
	for name in ["big.txt", "binary.bin", "nb.ipynb", "doc.txt"] {
		fixture.buffer(name, Ok("dirty\n"));
	}
	for name in ["big.txt", "binary.bin", "nb.ipynb"] {
		let base = fixture.base(name).await.expect("ineligible is disk");
		assert_eq!(base.bytes, None, "{name}");
		assert!(base.diags.is_empty(), "{name}: no notice for an ineligible document");
	}
	// No editor bound for this invocation: disk, silently.
	let lease = fixture.lease("doc.txt").await;
	let disk = read_whole(&fixture.host, &lease).await.expect("disk");
	let base = fixture
		.host
		.editor_base(lease.head(), &disk)
		.await
		.expect("unbound is disk");
	assert_eq!(base.bytes, None);
	assert_eq!(fixture.reads(), 0, "the editor was never asked");

	// A path outside the project roots is never sent to the editor, not even
	// to read.
	let outside = tempfile::NamedTempFile::new().expect("outside file");
	fs::write(outside.path(), b"outside\n").expect("outside content");
	let sources = ReadSourceAdapter::new(
		fixture.host.clone(),
		WorkspaceHost::open(&fixture.root).expect("workspace"),
		project_document_cache(&fixture.root),
	);
	let outside_path = Str::new(outside.path().to_str().expect("UTF-8"));
	let lease = fixture
		.scoped(sources.open(outside_path))
		.await
		.expect("outside read");
	assert_eq!(lease.read_all().await.expect("bytes").as_ref(), b"outside\n");
	assert_eq!(fixture.reads(), 0, "outside paths never reach the editor");
}

#[test]
fn eligibility_is_a_present_utf8_text_file_inside_the_root() {
	let head =
		|uri: &str, kind: pb::DocumentKind, presence: pb::DocumentPresence| pb::DocumentHead {
			document: Some(pb::DocumentRef { uri: uri.into(), ..pb::DocumentRef::default() }),
			kind: kind as i32,
			presence: presence as i32,
			..pb::DocumentHead::default()
		};
	let text = pb::DocumentKind::Text;
	let present = pb::DocumentPresence::Present;
	let root = "file:///work/project/";
	assert_eq!(
		eligible_path(root, &head("file:///work/project/src/a.rs", text, present), b"a").as_deref(),
		Some("/work/project/src/a.rs")
	);
	for (uri, kind, presence, disk) in [
		("file:///work/other/a.rs", text, present, &b"a"[..]),
		("file:///work/project", text, present, b"a"),
		("file:///work/project/a.ipynb", text, present, b"{}"),
		("file:///work/project/a.bin", pb::DocumentKind::Binary, present, b"a"),
		("file:///work/project/gone.rs", text, pb::DocumentPresence::Missing, b""),
		("file:///work/project/a.rs", text, present, b"\xff"),
		("artifact://sha256/00", text, present, b"a"),
	] {
		assert_eq!(eligible_path(root, &head(uri, kind, presence), disk), None, "{uri}");
	}
}

/// Regression: the removed `write_plain` early return let an editor write
/// replace the commit. A Write with a bound editor creates an authority
/// revision, pushes nothing to the editor, and anchors the superseded buffer
/// so the stale editor cannot revert the write on the next read.
#[tokio::test]
async fn a_write_with_a_bound_editor_commits_an_authority_revision() {
	let fixture = Fixture::new(&[("w.txt", b"old\n")]).await;
	fixture.buffer("w.txt", Ok("old, unsaved\n"));
	let before = fixture
		.lease("w.txt")
		.await
		.head()
		.revision
		.clone()
		.expect("revision");
	let written = fixture
		.scoped(fixture.host.write_plain(PlainWriteRequest {
			path:            Str::new_static("w.txt"),
			content:         Str::new_static("new\n"),
			format_policy:   FormatPolicy::Disabled,
			guard_generated: false,
		}))
		.await
		.expect("write commits");
	assert_eq!(written.byte_len, 4);
	assert_eq!(fs::read(fixture.path("w.txt")).expect("disk"), b"new\n");
	let after = fixture
		.lease("w.txt")
		.await
		.head()
		.revision
		.clone()
		.expect("revision");
	assert_ne!(after, before, "the write is a new authority revision");
	assert_eq!(before.content_hash.as_ref(), Hash32::sum(b"old\n").as_bytes());
	assert_eq!(
		after.content_hash.as_ref(),
		Hash32::sum(b"new\n").as_bytes(),
		"the authority's head names the written bytes"
	);
	assert_eq!(fixture.editor.writes.load(Ordering::SeqCst), 0, "nothing is pushed to the editor");

	let base = fixture.base("w.txt").await.expect("stale editor");
	assert_eq!(base.bytes, None, "the unsaved buffer the write superseded does not revert it");
	assert!(base.diags.is_empty());
}

/// Regression: the removed `acp:<hash>` pseudo-revision. A Read with a bound
/// editor returns E, reports the disk head's real revision, and carries the
/// provenance of the bytes.
#[tokio::test]
async fn a_read_with_a_bound_editor_reports_the_authority_revision() {
	let fixture = Fixture::new(&[("r.txt", b"one\n")]).await;
	fixture.buffer("r.txt", Ok("one\ntwo\n"));
	let sources = ReadSourceAdapter::new(
		fixture.host.clone(),
		WorkspaceHost::open(&fixture.root).expect("workspace"),
		project_document_cache(&fixture.root),
	);
	let lease = fixture
		.scoped(sources.open(Str::new_static("r.txt")))
		.await
		.expect("read lease");
	let head = fixture.lease("r.txt").await;
	let (revision, _) = read_document_metadata(head.head()).expect("revision identity");
	assert_eq!(lease.revision(), &revision, "the lease names the authority's revision");
	assert!(!lease.revision().starts_with("acp:"));
	assert_eq!(lease.read_all().await.expect("bytes").as_ref(), b"one\ntwo\n");
	assert_eq!(kinds(lease.diags()), [
		kind(DiagKind::EditorBufferUnanchored),
		kind(DiagKind::Provenance)
	]);
	assert_eq!(fs::read(fixture.path("r.txt")).expect("disk"), b"one\n", "a read has no effect");
}

/// An Edit prepares against the effective base with the disk revision, a
/// concurrent non-overlapping disk edit from a second host rebases in the
/// authority, and the commit saves the user's unsaved change with the
/// agent's.
#[tokio::test]
async fn an_edit_on_a_dirty_buffer_commits_through_the_authority() {
	let fixture = Fixture::new(&[("e.txt", ANCESTOR.as_bytes())]).await;
	// A clean read anchors K.
	fixture.buffer("e.txt", Ok(ANCESTOR));
	assert_eq!(fixture.base("e.txt").await.expect("clean").bytes, None);
	// The user types on line 3; the buffer stays unsaved.
	let dirty = "one\ntwo\nTHREE\nfour\nfive\n";
	fixture.buffer("e.txt", Ok(dirty));

	let mut prepared = fixture
		.scoped(fixture.host.prepare(Fixture::prepare_request("e.txt")))
		.await
		.expect("prepare");
	assert_eq!(prepared.base_bytes().as_ref(), dirty.as_bytes(), "E = the user's buffer");
	let disk_head = fixture.lease("e.txt").await;
	let (disk_revision, _) = read_document_metadata(disk_head.head()).expect("revision");
	assert_eq!(prepared.base_revision(), &disk_revision, "the base revision stays Rd");
	drop(disk_head);

	// A second host edits line 1 on disk after the prepare.
	let second = connect(&fixture.environment).await;
	let mut other = second
		.prepare(Fixture::prepare_request("e.txt"))
		.await
		.expect("second prepare");
	let other_revision = other.base_revision().clone();
	EditDocuments::commit(
		&second,
		vec![&mut other],
		vec![EditProposal {
			action:        EditAction::Write {
				content: Bytes::from_static(b"ONE\ntwo\nthree\nfour\nfive\n"),
			},
			base_revision: other_revision,
			stale_policy:  StalePolicy::RebaseNonOverlapping,
			format_policy: FormatPolicy::Disabled,
		}],
		second.start_clipboard_batch(),
	)
	.await
	.expect("second host commits");

	// The agent appends a line to what it read.
	let revision = prepared.base_revision().clone();
	let clipboard = fixture.host.start_clipboard_batch();
	let result = fixture
		.scoped(EditDocuments::commit(
			&fixture.host,
			vec![&mut prepared],
			vec![EditProposal {
				action:        EditAction::Write {
					content: Bytes::from_static(b"one\ntwo\nTHREE\nfour\nfive\nsix\n"),
				},
				base_revision: revision,
				stale_policy:  StalePolicy::RebaseNonOverlapping,
				format_policy: FormatPolicy::Disabled,
			}],
			clipboard,
		))
		.await
		.expect("the agent's commit rebases over the second host");
	assert!(result.sections[0].rebased);
	assert_eq!(
		fs::read_to_string(fixture.path("e.txt")).expect("disk"),
		"ONE\ntwo\nTHREE\nfour\nfive\nsix\n",
		"user delta + second host delta + agent delta"
	);
	assert_eq!(fixture.editor.writes.load(Ordering::SeqCst), 0);
	assert_eq!(
		fixture
			.session
			.anchor_of(fixture.key("e.txt").as_str())
			.as_deref(),
		Some(dirty.as_bytes()),
		"K := B once the commit is durable"
	);
	// The editor has not reloaded: its buffer is stale-clean now.
	let base = fixture.base("e.txt").await.expect("stale-clean");
	assert_eq!(base.bytes, None);
}

/// Overlapping unsaved changes and a disk change reject the Edit before any
/// effect, with typed conflict ranges.
#[tokio::test]
async fn a_conflicting_dirty_buffer_rejects_the_edit_before_any_effect() {
	let fixture = Fixture::new(&[("c.txt", ANCESTOR.as_bytes())]).await;
	fixture.buffer("c.txt", Ok(ANCESTOR));
	assert_eq!(fixture.base("c.txt").await.expect("clean").bytes, None);
	let second = connect(&fixture.environment).await;
	let mut other = second
		.prepare(Fixture::prepare_request("c.txt"))
		.await
		.expect("second prepare");
	let other_revision = other.base_revision().clone();
	EditDocuments::commit(
		&second,
		vec![&mut other],
		vec![EditProposal {
			action:        EditAction::Write {
				content: Bytes::from_static(b"one\nTWO\nthree\nfour\nfive\n"),
			},
			base_revision: other_revision,
			stale_policy:  StalePolicy::RebaseNonOverlapping,
			format_policy: FormatPolicy::Disabled,
		}],
		second.start_clipboard_batch(),
	)
	.await
	.expect("second host commits");
	fixture.buffer("c.txt", Ok("one\nzwei\nthree\nfour\nfive\n"));
	let fault = fixture
		.scoped(fixture.host.prepare(Fixture::prepare_request("c.txt")))
		.await
		.expect_err("overlapping changes conflict");
	assert_eq!(fault.reason, RejectionReason::Conflict);
	assert_eq!(fault.conflicts.len(), 1);
	assert_eq!((fault.conflicts[0].start_line, fault.conflicts[0].end_line), (2, 2));
	assert!(fault.conflicts[0].message.contains("disk lines 2-2"), "{}", fault.conflicts[0].message);
	assert_eq!(
		fs::read_to_string(fixture.path("c.txt")).expect("disk"),
		"one\nTWO\nthree\nfour\nfive\n",
		"no effect"
	);
}

#[test]
fn editor_errors_cross_the_wire_as_their_own_classification() {
	for error in [
		EditorIoError::Unbound,
		EditorIoError::Unavailable,
		EditorIoError::Timeout,
		EditorIoError::Disconnected,
		EditorIoError::Refused,
		EditorIoError::Malformed,
		EditorIoError::Oversize,
		EditorIoError::Busy,
	] {
		assert_eq!(EditorIoError::from_protocol_code(error.protocol_code() as i32), error);
	}
	assert_eq!(EditorIoError::from_protocol_code(9_999), EditorIoError::Refused);
}

#[test]
fn a_new_binding_starts_unanchored() {
	let session = EditorSession::new(Duration::ZERO);
	assert_eq!(session.deadline(), DEFAULT_DEADLINE);
	let path = Str::new_static("/work/a.txt");
	let _ = session.select(&path, "a\n", "a\n");
	assert!(session.anchor_of("/work/a.txt").is_some());
	let rebound = EditorSession::new(Duration::from_secs(1));
	assert!(rebound.anchor_of("/work/a.txt").is_none());
}

fn text() -> impl Strategy<Value = String> {
	prop::collection::vec(prop::sample::select(vec!["a", "b", "c", "dd", ""]), 0..8)
		.prop_map(|lines| lines.iter().map(|line| format!("{line}\n")).collect())
}

/// Encodes LF `text` with CRLF endings and/or a BOM.
fn encoded(text: &str, crlf: bool, bom: bool) -> String {
	let body = if crlf {
		text.replace('\n', "\r\n")
	} else {
		text.to_owned()
	};
	if bom { format!("{BOM}{body}") } else { body }
}

fn assert_encoded_like(disk: &str, bytes: &[u8]) {
	let text = str::from_utf8(bytes).expect("UTF-8");
	assert_eq!(text.starts_with(BOM), disk.starts_with(BOM), "disk's BOM wins");
	if dominant_line_ending(disk) == LineEnding::CrLf {
		assert!(!text.replace("\r\n", "").contains('\n'), "disk's CRLF wins: {text:?}");
	} else {
		assert!(!text.contains('\r'), "disk's LF wins: {text:?}");
	}
}

proptest! {
	/// B ≡ K ⇒ E = D: a buffer the authority already absorbed never overrides
	/// disk, whatever the encodings.
	#[test]
	fn a_buffer_equal_to_the_anchor_resolves_to_disk(
		disk in text(), anchor in text(), crlf in any::<bool>(), bom in any::<bool>(),
	) {
		let buffer = encoded(&anchor, crlf, bom);
		let selection = select_base(&disk, &buffer, Some(&Bytes::from(anchor.clone())), |_| false);
		if anchor == disk {
			prop_assert_eq!(selection.choice, Choice::Clean);
		} else {
			prop_assert_eq!(selection.choice, Choice::StaleClean);
		}
	}

	/// D = K ⇒ E = B: with no disk change since the anchor, the user's buffer
	/// is the base, re-encoded as disk.
	#[test]
	fn an_unchanged_disk_resolves_to_the_buffer(
		anchor in text(), buffer in text(), crlf in any::<bool>(), bom in any::<bool>(),
	) {
		let disk = encoded(&anchor, crlf, bom);
		let selection = select_base(&disk, &buffer, Some(&Bytes::from(anchor.clone())), |_| false);
		match selection.choice {
			Choice::Clean => prop_assert_eq!(&buffer, &anchor),
			Choice::Merged(bytes) => {
				prop_assert_eq!(normalized(str::from_utf8(&bytes).expect("UTF-8")), Bytes::from(buffer));
				assert_encoded_like(&disk, &bytes);
			},
			other => prop_assert!(false, "unexpected {other:?}"),
		}
	}

	/// The disk authority is never diverged from: selecting a base changes no
	/// disk state, every E carries disk's encoding, and a disk change landing
	/// after the prepare survives the commit of an editor-based proposal (the
	/// authority rebases it or rejects the commit, never drops it).
	#[test]
	fn editor_bases_never_revert_a_later_disk_change(
		disk in text(), anchor in prop::option::of(text()), buffer in text(),
		crlf in any::<bool>(), bom in any::<bool>(), appended in "[a-z]{1,4}",
	) {
		let disk = encoded(&disk, crlf, bom);
		let anchor = anchor.map(Bytes::from);
		let selection = select_base(&disk, &buffer, anchor.as_ref(), |_| false);
		let base = match &selection.choice {
			Choice::Clean | Choice::StaleClean => Bytes::from(disk.clone()),
			Choice::Merged(bytes) | Choice::Unanchored(bytes) => {
				assert_encoded_like(&disk, bytes);
				bytes.clone()
			},
			Choice::Conflict(ranges) => {
				prop_assert!(!ranges.is_empty());
				return Ok(());
			},
		};
		// The agent appends a line to E and proposes it against Rd = D.
		let mut proposal = base.to_vec();
		proposal.extend_from_slice(format!("{appended}\n").as_bytes());
		let proposal = Bytes::from(proposal);
		// A second host prepends a line on disk after the prepare.
		let head = Bytes::from(format!("header\n{disk}"));
		let base_on_disk = Bytes::from(disk.clone());
		// The commit: E's divergence from D plus the agent's line, rebased by
		// the authority onto the moved head.
		if let Ok(Ok(committed)) = rebase_content(&base_on_disk, &head, &proposal) {
			let committed = committed.content().clone();
			prop_assert!(committed.starts_with(b"header\n"), "the later disk change survives");
			prop_assert!(
				committed.ends_with(format!("{appended}\n").as_bytes()),
				"the agent's change lands"
			);
		}
	}
}

/// Joined over the environment wire: a composition binds its editor through
/// [`crate::ProjectEnvironment::editor_documents`], the daemon's route asks it
/// for the buffer over `AcpReadQuery`, and the Read tool returns that buffer.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_bound_composition_answers_reads_over_the_environment_wire() {
	let directory = tempfile::tempdir().expect("scratch");
	let root = directory.path().join("workspace");
	let state = directory.path().join("state");
	fs::create_dir_all(&root).expect("workspace");
	fs::create_dir_all(&state).expect("state");
	let root = fs::canonicalize(&root).expect("canonical root");
	fs::write(root.join("notes.txt"), "on disk\n").expect("fixture");
	let environment = crate::ProjectEnvironment::isolated(
		&root,
		&state,
		Arc::new(omp_con::Ctx::new()),
		crate::RegistryBridges::default(),
	)
	.await
	.expect("environment");
	let editor = Arc::new(FakeEditor::default());
	editor
		.buffers
		.lock()
		.insert(root.join("notes.txt"), Ok(Str::new_static("on disk\nunsaved line\n")));
	environment
		.editor_documents()
		.bind(Some(Arc::clone(&editor) as Arc<dyn AcpDocumentBackend>));
	let client = environment.client().clone();
	let mut invocation = client
		.invoke(omp_proto::env::v1::InvokeTool {
			invocation_id: "read-1".to_owned(),
			name: "read".to_owned(),
			rev: "2".to_owned(),
			..omp_proto::env::v1::InvokeTool::default()
		})
		.await
		.expect("invoke");
	let accepted = invocation.next_event().await.expect("event");
	assert!(matches!(accepted, Some(omp_env::InvocationEvent::Accepted(_))), "{accepted:?}");
	invocation
		.commit_args(
			Bytes::from_static(br#"{"path":"notes.txt"}"#),
			Bytes::from_static(b"editor-base-test-token"),
			1000,
			None,
		)
		.await
		.expect("commit args");
	let verdict = loop {
		match invocation.next_event().await.expect("event") {
			Some(omp_env::InvocationEvent::Verdict(verdict)) => break verdict,
			Some(_) => {},
			None => panic!("invocation closed before its verdict"),
		}
	};
	let json = String::from_utf8_lossy(&verdict.json).into_owned();
	assert_eq!(editor.reads.load(Ordering::SeqCst), 1, "the daemon asked the editor: {json}");
	assert!(json.contains("unsaved line"), "{json}");
	assert_eq!(fs::read(root.join("notes.txt")).expect("disk"), b"on disk\n");
	drop(environment);
}
