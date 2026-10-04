//! The ACP editor's buffer as the document base (ADR 0037 §2–§3).
//!
//! In ACP mode the user looks at the editor's buffers, not at the files on
//! disk. When the invoking connection bound an editor, Read and Edit ask it
//! for the whole buffer B of an eligible file and choose the effective base E
//! against the disk head D:
//!
//! | Buffer B | E | Anchor K |
//! | --- | --- | --- |
//! | B ≡ D | D | K := D |
//! | B ≡ K, or B ≡ a disk revision seen earlier | D | unchanged |
//! | otherwise, K present | `rebase_content(K, D, B)` | K := B on commit |
//! | otherwise, no K | B, with an `editor_buffer_unanchored` warning | K := D now, K := B on commit |
//!
//! The last row's K := D goes beyond the ADR's table: without it every read
//! until the first commit would be "first contact" again, and a disk change
//! made after the first read (another host, `git checkout`) would be reverted
//! wholesale by the buffer. With it, such a change is merged under the user's
//! delta or conflicts, exactly like the anchored row.
//!
//! "≡" ignores line endings and a BOM, and every E carries D's encoding. A
//! conflicting anchored merge rejects an Edit before any effect; a failed or
//! oversize buffer read falls back to D with an `editor_buffer_unavailable`
//! warning.
//!
//! Once a commit is durable and the editor accepts writes, the committed bytes
//! are written back to it ([`start_write_backs`], queued by the editor's
//! connection in [`crate::editor_sync`]); a write-back that lands sets K to the
//! bytes sent (ADR 0037 §3(c)), and one that fails or is skipped leaves K at
//! the buffer the commit used.
//!
//! The buffer never becomes an authority head. E is only the base a tool reads
//! and proposes against; the commit is an ordinary authority transaction
//! against D's revision, so the docserver actor's rule that a head equals the
//! persisted disk bytes is untouched. Anchors live only in memory, one table
//! per editor binding: a new binding (session switch, resume) starts
//! unanchored.

use std::{
	borrow::Cow,
	fmt,
	path::{Path, PathBuf},
	str,
	sync::Arc,
	time::Duration,
};

use bytes::Bytes;
use omp_core::{FastHashMap, Hash32, Str, sf};
use omp_edit::text::{BOM, LineEnding, normalize_to_lf, restore_line_endings, strip_bom};
use omp_proto::document::v1 as pb;
use omp_tool::{Diag, DiagKind};
use omp_tools::{
	editor_sync::{EditorSync, EditorSyncReport, EditorSyncTarget},
	read::SNAPSHOT_MAX_BYTES,
};
use parking_lot::Mutex;
use similar::{Algorithm, DiffOp, capture_diff_slices_deadline};
use tokio::time;
use url::Url;

use crate::{
	docs::{
		AcpDocumentBackend, DocumentHost, EditorCapabilities, EditorIoError, WriteBack,
		WriteBackOutcome,
	},
	docserver::{ByteRange, rebase_content},
};

/// Disk revisions remembered per path for the stale-clean rule.
const RETAINED_REVISIONS: usize = 8;

/// Paths one editor binding tracks before the least recently used is
/// forgotten (and so becomes unanchored again).
const MAX_TRACKED_PATHS: usize = 512;

/// Wait applied when a binding did not state the editor's deadline.
const DEFAULT_DEADLINE: Duration = Duration::from_secs(5);

/// Upper bound on the line diffs that project conflict ranges for display.
const MAX_PROJECTION_TIME: Duration = Duration::from_millis(50);

/// Per-path lineage one editor binding has observed.
#[derive(Debug, Default)]
struct PathLineage {
	/// K: the last editor content known to be part of the authority's
	/// lineage, normalized to LF without BOM.
	anchor:   Option<Bytes>,
	/// Digests of normalized disk contents seen through the authority.
	retained: [Option<Hash32>; RETAINED_REVISIONS],
	/// Next slot of `retained` to overwrite.
	next:     usize,
	/// Recency stamp for eviction.
	touched:  u64,
	/// Last write-back ticket issued for this path.
	issued:   u64,
	/// Ticket of the write-back that last set the anchor, so an older
	/// write-back finishing late never overrides a newer one.
	synced:   u64,
}

impl PathLineage {
	fn retain(&mut self, digest: Hash32) {
		if self.retained.contains(&Some(digest)) {
			return;
		}
		self.retained[self.next] = Some(digest);
		self.next = (self.next + 1) % RETAINED_REVISIONS;
	}
}

#[derive(Debug, Default)]
struct Lineages {
	paths: FastHashMap<Str, PathLineage>,
	clock: u64,
}

impl Lineages {
	fn entry(&mut self, path: &Str) -> &mut PathLineage {
		self.clock += 1;
		if !self.paths.contains_key(path) && self.paths.len() >= MAX_TRACKED_PATHS {
			let oldest = self
				.paths
				.iter()
				.min_by_key(|(_, lineage)| lineage.touched)
				.map(|(path, _)| path.clone());
			if let Some(oldest) = oldest {
				self.paths.remove(&oldest);
			}
		}
		let clock = self.clock;
		let lineage = self.paths.entry(path.clone()).or_default();
		lineage.touched = clock;
		lineage
	}
}

/// One editor binding's runtime state: its deadline and the anchor table.
///
/// Created per bind, so a session switch or resume starts unanchored. This is
/// session runtime state, never a second source of truth: it only decides
/// which bytes a tool treats as its base.
#[derive(Debug)]
pub struct EditorSession {
	deadline:     Duration,
	capabilities: EditorCapabilities,
	lineages:     Mutex<Lineages>,
}

impl EditorSession {
	/// Starts an unanchored binding of an editor with `capabilities` whose
	/// requests wait at most `deadline` (zero means the default).
	pub fn new(deadline: Duration, capabilities: EditorCapabilities) -> Self {
		Self {
			deadline: if deadline.is_zero() {
				DEFAULT_DEADLINE
			} else {
				deadline
			},
			capabilities,
			lineages: Mutex::default(),
		}
	}

	/// Deadline for each editor request of this binding.
	pub const fn deadline(&self) -> Duration {
		self.deadline
	}

	/// The `fs/*` methods the bound editor serves.
	pub const fn capabilities(&self) -> EditorCapabilities {
		self.capabilities
	}

	/// Chooses the effective base for `path` and records what the choice
	/// teaches: the disk revision is retained, and a clean buffer anchors.
	fn select(&self, path: &Str, disk: &str, buffer: &str) -> Selection {
		let (anchor, retained) = {
			let mut lineages = self.lineages.lock();
			let lineage = lineages.entry(path);
			(lineage.anchor.clone(), lineage.retained)
		};
		let selection =
			select_base(disk, buffer, anchor.as_ref(), |digest| retained.contains(&Some(*digest)));
		let mut lineages = self.lineages.lock();
		let lineage = lineages.entry(path);
		lineage.retain(Hash32::sum(&selection.disk));
		match selection.choice {
			// B ≡ D: the buffer is in the authority's lineage.
			Choice::Clean => lineage.anchor = Some(selection.disk.clone()),
			// First contact: the buffer wins now, and the disk it was compared
			// with becomes the base its changes are measured from. A disk change
			// made after this read is then merged into the buffer (or conflicts)
			// instead of being reverted by the next wholesale buffer.
			Choice::Unanchored(_) => lineage.anchor = Some(selection.disk.clone()),
			Choice::StaleClean | Choice::Merged(_) | Choice::Conflict(_) => {},
		}
		selection
	}

	#[cfg(test)]
	fn retain(&self, path: &Str, disk: &str) {
		self
			.lineages
			.lock()
			.entry(path)
			.retain(Hash32::sum(normalized(disk)));
	}

	/// Records that the authority committed with `buffer` as its base: K := B.
	fn anchor(&self, path: &Str, buffer: Bytes) {
		self.lineages.lock().entry(path).anchor = Some(buffer);
	}

	/// Issues the ticket of a new write-back of `path`.
	fn issue(&self, path: &Str) -> u64 {
		let mut lineages = self.lineages.lock();
		let lineage = lineages.entry(path);
		lineage.issued += 1;
		lineage.issued
	}

	/// A write-back succeeded: the editor holds `sent`, so K := the bytes sent
	/// (ADR 0037 §3(c)), unless a newer write-back already set K.
	fn synced(&self, path: &Str, ticket: u64, sent: Bytes) {
		let mut lineages = self.lineages.lock();
		let lineage = lineages.entry(path);
		if ticket > lineage.synced {
			lineage.synced = ticket;
			lineage.anchor = Some(sent);
		}
	}

	#[cfg(test)]
	fn anchor_of(&self, path: &str) -> Option<Bytes> {
		self
			.lineages
			.lock()
			.paths
			.get(path)
			.and_then(|lineage| lineage.anchor.clone())
	}
}

/// The editor bound to an invoking connection plus its binding's anchors.
#[derive(Clone)]
pub struct EditorRoute {
	backend: Arc<dyn AcpDocumentBackend>,
	session: Arc<EditorSession>,
}

impl EditorRoute {
	/// Pairs an editor with the anchor table of its binding.
	pub fn new(backend: Arc<dyn AcpDocumentBackend>, session: Arc<EditorSession>) -> Self {
		Self { backend, session }
	}

	/// Reads the whole buffer of `path`, bounded by the binding's deadline and
	/// the snapshot cap.
	async fn read(&self, path: &Str) -> Result<Str, EditorIoError> {
		if !self.session.capabilities.read {
			return Err(EditorIoError::Unavailable);
		}
		let buffer = time::timeout(self.session.deadline, self.backend.read_text(path.clone()))
			.await
			.unwrap_or(Err(EditorIoError::Timeout))?;
		if buffer.len() > SNAPSHOT_MAX_BYTES {
			return Err(EditorIoError::Oversize);
		}
		Ok(buffer)
	}
}

/// How the effective base relates to the disk head.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Choice {
	/// B ≡ D: disk is the base, and B anchors.
	Clean,
	/// B ≡ K or an earlier disk revision: the editor has not reloaded, so
	/// disk wins.
	StaleClean,
	/// The user's delta K→B replayed onto D, in D's encoding.
	Merged(Bytes),
	/// No anchor: B wholesale, in D's encoding.
	Unanchored(Bytes),
	/// The anchored merge overlaps a disk change; ranges in K's normalized
	/// coordinates.
	Conflict(Vec<ByteRange>),
}

/// A base choice plus the normalized inputs it was made from.
#[derive(Clone, Debug)]
pub struct Selection {
	/// The choice.
	pub(super) choice: Choice,
	/// B normalized to LF without BOM: what an anchor stores.
	pub(super) buffer: Bytes,
	/// D normalized to LF without BOM.
	pub(super) disk:   Bytes,
	/// The anchor K the choice was made against.
	pub(super) anchor: Option<Bytes>,
}

/// Text with any BOM removed and every line ending normalized to LF.
pub fn normalized(text: &str) -> Bytes {
	match normalize_to_lf(strip_bom(text).1) {
		Cow::Borrowed(text) => Bytes::copy_from_slice(text.as_bytes()),
		Cow::Owned(text) => Bytes::from(text),
	}
}

/// The line ending most of `text`'s lines use.
fn dominant_line_ending(text: &str) -> LineEnding {
	let lines = bytecount::count(text.as_bytes(), b'\n');
	let crlf = text.matches("\r\n").count();
	if crlf * 2 > lines {
		LineEnding::CrLf
	} else {
		LineEnding::Lf
	}
}

/// Re-encodes normalized `text` with `disk`'s BOM and dominant line ending.
pub fn encode_like(disk: &str, text: &[u8]) -> Bytes {
	let text = str::from_utf8(text).expect("normalized editor text is UTF-8");
	let bom = if strip_bom(disk).0.is_empty() {
		""
	} else {
		BOM
	};
	let body = restore_line_endings(text, dominant_line_ending(disk));
	if bom.is_empty() {
		return Bytes::from(body);
	}
	let mut encoded = String::with_capacity(bom.len() + body.len());
	encoded.push_str(bom);
	encoded.push_str(&body);
	Bytes::from(encoded)
}

/// ADR 0037 §3's base selection, a pure function of D, B, K and the retained
/// disk revisions.
pub fn select_base(
	disk: &str,
	buffer: &str,
	anchor: Option<&Bytes>,
	retained: impl Fn(&Hash32) -> bool,
) -> Selection {
	let normalized_disk = normalized(disk);
	let normalized_buffer = normalized(buffer);
	let choice = if buffer == disk || normalized_buffer == normalized_disk {
		Choice::Clean
	} else if anchor == Some(&normalized_buffer) || retained(&Hash32::sum(&normalized_buffer)) {
		Choice::StaleClean
	} else if let Some(anchor) = anchor {
		match rebase_content(anchor, &normalized_disk, &normalized_buffer) {
			Ok(Ok(merged)) => Choice::Merged(encode_like(disk, merged.content())),
			Ok(Err(conflict)) => Choice::Conflict(conflict.into_ranges()),
			// Canonical edits are valid by construction; an invalid list can
			// only mean the merge has no defined result, which is a conflict
			// over the whole anchor.
			Err(_) => Choice::Conflict(
				ByteRange::new(0, u64::try_from(anchor.len()).unwrap_or(u64::MAX))
					.into_iter()
					.collect(),
			),
		}
	} else {
		Choice::Unanchored(encode_like(disk, &normalized_buffer))
	};
	Selection { choice, buffer: normalized_buffer, disk: normalized_disk, anchor: anchor.cloned() }
}

/// What a durable commit of one document tells the bound editor: K := B for
/// the buffer B its base was chosen against (ADR 0037 §3(b)), then the
/// write-back of the committed bytes (§4.3).
#[derive(Clone)]
pub struct CommitAnchor {
	route:  EditorRoute,
	path:   Str,
	/// B normalized to LF without BOM; `None` when no buffer was read (a
	/// create, or an editor that serves no reads).
	buffer: Option<Bytes>,
}

impl fmt::Debug for CommitAnchor {
	fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
		formatter
			.debug_struct("CommitAnchor")
			.field("path", &self.path)
			.field("buffer", &self.buffer.as_ref().map(Bytes::len))
			.finish_non_exhaustive()
	}
}

impl CommitAnchor {
	/// The commit is durable but its bytes are not written back (a move): the
	/// buffer is part of the authority's lineage, so an editor that has not
	/// reloaded reads as stale-clean.
	pub fn anchored(self) {
		if let Some(buffer) = self.buffer {
			self.route.session.anchor(&self.path, buffer);
		}
	}

	/// The commit is durable with `committed` (R, the bytes on disk) as
	/// `revision`: K := B, and when the editor accepts writes the write-back
	/// of R is returned for [`start_write_backs`].
	pub fn committed(self, committed: Bytes, revision: Str) -> Option<WriteBackJob> {
		if let Some(buffer) = &self.buffer {
			self.route.session.anchor(&self.path, buffer.clone());
		}
		if !self.route.session.capabilities.write {
			return None;
		}
		let content = Str::from_utf8_owned(committed).ok()?;
		let ticket = self.route.session.issue(&self.path);
		Some(WriteBackJob {
			route: self.route,
			path: self.path,
			content,
			base: self.buffer,
			revision,
			ticket,
		})
	}
}

#[cfg(test)]
impl CommitAnchor {
	/// An anchor for `path` against `buffer`, as a prepare would record it.
	pub(crate) const fn for_test(route: EditorRoute, path: Str, buffer: Option<Bytes>) -> Self {
		Self { route, path, buffer }
	}
}

/// One committed document to write back to the editor.
pub struct WriteBackJob {
	route:    EditorRoute,
	path:     Str,
	/// R: the committed bytes.
	content:  Str,
	/// B, normalized.
	base:     Option<Bytes>,
	revision: Str,
	ticket:   u64,
}

impl WriteBackJob {
	fn target(&self) -> EditorSyncTarget {
		EditorSyncTarget { path: self.path.clone(), revision: self.revision.clone() }
	}

	/// Writes R back and returns the notices for the tool element. A failure
	/// leaves K at B (set by the commit), so the stale buffer reads as
	/// stale-clean and resolves to disk (ADR 0037 §4.5).
	async fn run(self) -> Vec<Diag> {
		let base = self
			.base
			.clone()
			.and_then(|base| Str::from_utf8_owned(base).ok());
		let write_back = WriteBack { path: self.path.clone(), content: self.content.clone(), base };
		match self.route.backend.write_back(write_back).await {
			Ok(WriteBackOutcome::Written { merged, read_back }) => {
				let sent = merged.unwrap_or_else(|| self.content.clone());
				self
					.route
					.session
					.synced(&self.path, self.ticket, normalized(&sent));
				read_back
					.map(|_| {
						Diag::info(
							DiagKind::ClientFormatDrift,
							sf!(
								"the editor reformatted {} after revision {} was written back to it \
								 (client_formatted=true bytes_changed_after_client_format=true); the file \
								 on disk keeps the committed bytes until the editor saves it",
								self.path,
								self.revision
							),
						)
					})
					.into_iter()
					.collect()
			},
			Ok(WriteBackOutcome::Conflict { ranges }) => vec![self.conflict_diag(&ranges)],
			Ok(WriteBackOutcome::Superseded) => Vec::new(),
			// The environment stopped waiting (an interrupt, or the editor's
			// connection ended); the editor's queue may still finish it.
			Err(EditorIoError::Disconnected) => vec![Diag::info(
				DiagKind::EditorSyncPending,
				sf!(
					"{} is committed at revision {}; writing it back to the editor continues after \
					 this call",
					self.path,
					self.revision
				),
			)],
			Err(error) => {
				let reason: &'static str = error.into();
				vec![Diag::warn(
					DiagKind::EditorSyncFailed,
					sf!(
						"revision {} of {} is committed, but writing it back to the editor failed \
						 ({reason}); the editor still shows its earlier buffer",
						self.revision,
						self.path
					),
				)]
			},
		}
	}

	/// The user's changes made during the call overlap the commit: the ranges
	/// in the base B, and the same lines in B and in the committed bytes R.
	fn conflict_diag(&self, ranges: &[ByteRange]) -> Diag {
		use std::fmt::Write as _;
		let base = self.base.as_deref().unwrap_or_default();
		let committed = normalized(&self.content);
		let base_lines = split_lines(base);
		let to_committed = line_ops(&base_lines, &split_lines(&committed));
		let mut text = format!(
			"unsaved editor changes to {} made during this call overlap the committed revision {}; \
			 the editor was not updated and keeps its own changes",
			self.path, self.revision
		);
		for range in ranges {
			let start = line_index(base, range.start());
			let end = line_index(base, range.end().saturating_sub(1).max(range.start()));
			let _ = write!(
				text,
				"; bytes {}-{} of the base (lines {}-{}) vs committed lines {}-{}",
				range.start(),
				range.end(),
				start + 1,
				end + 1,
				project(&to_committed, start) + 1,
				project(&to_committed, end) + 1
			);
		}
		Diag::warn(DiagKind::EditorSyncConflict, text)
	}
}

/// Starts the write-backs of one commit on their own tasks and returns the
/// handle the tool settles on. The tasks outlive the tool: an interrupt only
/// stops the tool waiting (ADR 0037 §4.6).
pub fn start_write_backs(jobs: Vec<WriteBackJob>) -> EditorSync {
	if jobs.is_empty() {
		return EditorSync::default();
	}
	let (reports, received) = flume::unbounded();
	let targets = jobs.iter().map(WriteBackJob::target).collect();
	for (target, job) in jobs.into_iter().enumerate() {
		let reports = reports.clone();
		tokio::spawn(async move {
			let diags = job.run().await;
			let _ = reports.send(EditorSyncReport { target, diags });
		});
	}
	EditorSync::new(targets, received)
}

/// The base a Read or Edit uses for one document.
#[derive(Debug, Default)]
pub struct EffectiveBase {
	/// E when it differs from the disk head; `None` means disk is the base.
	pub(super) bytes:     Option<Bytes>,
	/// Notices for the tool element.
	pub(super) diags:     Vec<Diag>,
	/// Anchor to record once a commit used this base.
	pub(super) on_commit: Option<CommitAnchor>,
}

impl EffectiveBase {
	fn unavailable(path: &str, error: EditorIoError) -> Self {
		let reason: &'static str = error.into();
		Self {
			diags: vec![Diag::warn(
				DiagKind::EditorBufferUnavailable,
				sf!("the editor buffer for {path} could not be read ({reason}); used the file on disk"),
			)],
			..Self::default()
		}
	}
}

/// One conflicting range, as one-based inclusive line spans.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ConflictLines {
	/// Lines in the anchor K (the typed ranges' own coordinates).
	pub(super) anchor: (usize, usize),
	/// The same span projected into the editor buffer B.
	pub(super) buffer: (usize, usize),
	/// The same span projected into the disk head D.
	pub(super) disk:   (usize, usize),
}

/// The user's unsaved changes overlap a disk change made since the anchor.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EditorConflict {
	/// Canonical path of the document.
	pub(super) path:   Str,
	/// Typed conflicting ranges in K's normalized byte coordinates.
	pub(super) ranges: Vec<ByteRange>,
	/// The ranges as line spans in K, B and D.
	pub(super) lines:  Vec<ConflictLines>,
}

impl EditorConflict {
	fn new(path: Str, ranges: Vec<ByteRange>, selection: &Selection) -> Self {
		let anchor = selection.anchor.as_deref().unwrap_or_default();
		let anchor_lines = split_lines(anchor);
		let to_buffer = line_ops(&anchor_lines, &split_lines(&selection.buffer));
		let to_disk = line_ops(&anchor_lines, &split_lines(&selection.disk));
		let mut lines: Vec<ConflictLines> = ranges
			.iter()
			.map(|range| {
				let start = line_index(anchor, range.start());
				let end = line_index(anchor, range.end().saturating_sub(1).max(range.start()));
				ConflictLines {
					anchor: (start + 1, end + 1),
					buffer: (project(&to_buffer, start) + 1, project(&to_buffer, end) + 1),
					disk:   (project(&to_disk, start) + 1, project(&to_disk, end) + 1),
				}
			})
			.collect();
		// Several byte ranges on the same lines present as one span.
		lines.dedup();
		Self { path, ranges, lines }
	}

	/// The warning a Read attaches when it falls back to disk.
	pub fn diag(&self) -> Diag {
		let mut text = format!(
			"unsaved editor changes to {} overlap a change on disk; read the file on disk",
			self.path
		);
		for span in &self.lines {
			use std::fmt::Write as _;
			let _ = write!(
				text,
				"; editor lines {}-{} vs disk lines {}-{}",
				span.buffer.0, span.buffer.1, span.disk.0, span.disk.1
			);
		}
		Diag::warn(DiagKind::EditorBufferConflict, text)
	}
}

fn split_lines(text: &[u8]) -> Vec<&[u8]> {
	text.split_inclusive(|byte| *byte == b'\n').collect()
}

fn line_index(text: &[u8], offset: u64) -> usize {
	let offset = usize::try_from(offset)
		.unwrap_or(usize::MAX)
		.min(text.len());
	bytecount::count(&text[..offset], b'\n')
}

fn line_ops(old: &[&[u8]], new: &[&[u8]]) -> Vec<DiffOp> {
	let deadline = std::time::Instant::now().checked_add(MAX_PROJECTION_TIME);
	capture_diff_slices_deadline(Algorithm::Myers, old, new, deadline)
}

/// Maps zero-based `line` of the diff's old side to its new side; a changed
/// line maps to the start of its replacement.
fn project(ops: &[DiffOp], line: usize) -> usize {
	let mut end = 0;
	for op in ops {
		let (old, new) = (op.old_range(), op.new_range());
		if old.contains(&line) {
			return match op {
				DiffOp::Equal { .. } => new.start + (line - old.start),
				_ => new.start,
			};
		}
		end = new.end;
	}
	end
}

/// Canonical local path of `head` when its buffer may be read from the editor
/// (ADR 0037 §2), else `None`: the document must be a present text file inside
/// the workspace root, not a notebook, and its disk bytes UTF-8 within the
/// snapshot cap.
fn eligible_path(root_uri: &str, head: &pb::DocumentHead, disk: &[u8]) -> Option<Str> {
	if head.presence != pb::DocumentPresence::Present as i32
		|| disk.len() > SNAPSHOT_MAX_BYTES
		|| str::from_utf8(disk).is_err()
	{
		return None;
	}
	editor_path(root_uri, head)
}

/// Canonical local path of a text document inside the workspace root that is
/// not a notebook: a path the editor may be told about, present or not.
fn editor_path(root_uri: &str, head: &pb::DocumentHead) -> Option<Str> {
	if pb::DocumentKind::try_from(head.kind) != Ok(pb::DocumentKind::Text) {
		return None;
	}
	let path = file_path(head.document.as_ref()?.uri.as_str())?;
	inside_root(root_uri, &path)
}

/// `path` as text when it lies strictly inside the workspace root and is not
/// a notebook.
fn inside_root(root_uri: &str, path: &Path) -> Option<Str> {
	let root = file_path(root_uri)?;
	if path == root || !path.starts_with(&root) {
		return None;
	}
	if path
		.extension()
		.is_some_and(|extension| extension.eq_ignore_ascii_case("ipynb"))
	{
		return None;
	}
	path.to_str().map(Str::new)
}

fn file_path(uri: &str) -> Option<PathBuf> {
	let url = Url::parse(uri).ok()?;
	(url.scheme() == "file")
		.then(|| url.to_file_path().ok())
		.flatten()
}

impl DocumentHost {
	/// The editor reachable from the current call: the invoking connection's
	/// binding for an invocation over the environment wire, or this host's
	/// in-process binding for a native call made by the owning composition's
	/// kernel (an embedded environment runs its native tools in-process).
	pub fn editor_route(&self) -> Option<EditorRoute> {
		match super::tools::invocation_acp_documents() {
			super::tools::InvocationEditor::Connection(route) => route,
			super::tools::InvocationEditor::InProcess => self.in_process_editor(),
		}
	}

	/// Chooses the effective base for one document whose disk head is `head`
	/// with bytes `disk` (ADR 0037 §3).
	///
	/// Without an editor bound to the invoking connection, or for an
	/// ineligible document, disk is the base and the editor is never asked.
	/// Otherwise the editor's buffer is read once. A conflicting anchored merge
	/// is returned as the error; every other outcome, including a failed read,
	/// yields a base.
	pub async fn editor_base(
		&self,
		head: &pb::DocumentHead,
		disk: &[u8],
	) -> Result<EffectiveBase, EditorConflict> {
		let Some(route) = self.editor_route() else {
			return Ok(EffectiveBase::default());
		};
		self.editor_base_with(&route, head, disk).await
	}

	async fn editor_base_with(
		&self,
		route: &EditorRoute,
		head: &pb::DocumentHead,
		disk: &[u8],
	) -> Result<EffectiveBase, EditorConflict> {
		let root_uri = self.hello().root_uri.as_str();
		let capabilities = route.session.capabilities;
		let Some(path) = eligible_path(root_uri, head, disk) else {
			// A document about to be created is written back once committed.
			let on_commit = (capabilities.write
				&& head.presence != pb::DocumentPresence::Present as i32)
				.then(|| editor_path(root_uri, head))
				.flatten()
				.map(|path| CommitAnchor { route: route.clone(), path, buffer: None });
			return Ok(EffectiveBase { on_commit, ..EffectiveBase::default() });
		};
		if !capabilities.read {
			// An editor that serves no reads: disk is the base, and the commit is
			// written back as is.
			let on_commit =
				capabilities
					.write
					.then(|| CommitAnchor { route: route.clone(), path, buffer: None });
			return Ok(EffectiveBase { on_commit, ..EffectiveBase::default() });
		}
		let disk = str::from_utf8(disk).expect("eligible disk bytes are UTF-8");
		let buffer = match route.read(&path).await {
			Ok(buffer) => buffer,
			// No buffer known, so nothing is anchored and nothing is written
			// back: a blind write-back could overwrite changes the user made in
			// a buffer the environment never saw.
			Err(error) => return Ok(EffectiveBase::unavailable(&path, error)),
		};
		let selection = route.session.select(&path, disk, &buffer);
		let on_commit = CommitAnchor {
			route:  route.clone(),
			path:   path.clone(),
			buffer: Some(selection.buffer.clone()),
		};
		let (bytes, diags) = match &selection.choice {
			Choice::Clean | Choice::StaleClean => (None, Vec::new()),
			Choice::Conflict(ranges) => {
				return Err(EditorConflict::new(path, ranges.clone(), &selection));
			},
			Choice::Merged(merged) => (Some(merged.clone()), Vec::new()),
			Choice::Unanchored(buffer) => (Some(buffer.clone()), vec![Diag::warn(
				DiagKind::EditorBufferUnanchored,
				sf!(
					"{path} has unsaved editor changes with no known common base with the file on \
					 disk; the editor buffer was used as the base and replaces the file's content when \
					 an edit commits"
				),
			)]),
		};
		let bytes = bytes.filter(|bytes| bytes.as_ref() != disk.as_bytes());
		let mut diags = diags;
		if let Some(bytes) = &bytes {
			diags.push(Diag::info(
				DiagKind::Provenance,
				sf!("source=editor-buffer path={path} sha256={}", Hash32::sum(bytes)),
			));
		}
		Ok(EffectiveBase { bytes, diags, on_commit: Some(on_commit) })
	}

	/// What an agent Write of `path` tells the bound editor once committed:
	/// for an existing file the buffer it supersedes is read first and
	/// anchored, so an editor that has not reloaded reads as stale-clean
	/// instead of reverting the write, and the pre-write re-read keeps what
	/// the user types meanwhile; a created file is written back as is. `None`
	/// without a bound editor, for a path outside the root or a notebook, or
	/// when an existing file's buffer cannot be read.
	pub async fn editor_write_target(&self, path: &Path, existed: bool) -> Option<CommitAnchor> {
		let route = self.editor_route()?;
		let capabilities = route.session.capabilities;
		let path_text = inside_root(self.hello().root_uri.as_str(), path)?;
		if !existed || !capabilities.read {
			return capabilities
				.write
				.then(|| CommitAnchor { route, path: path_text, buffer: None });
		}
		let metadata = std::fs::metadata(path).ok()?;
		if !metadata.is_file() || metadata.len() > SNAPSHOT_MAX_BYTES as u64 {
			return None;
		}
		let buffer = route.read(&path_text).await.ok()?;
		Some(CommitAnchor { route, path: path_text, buffer: Some(normalized(&buffer)) })
	}
}

#[cfg(test)]
mod tests;
