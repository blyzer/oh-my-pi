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
//! The buffer never becomes an authority head. E is only the base a tool reads
//! and proposes against; the commit is an ordinary authority transaction
//! against D's revision, so the docserver actor's rule that a head equals the
//! persisted disk bytes is untouched. Anchors live only in memory, one table
//! per editor binding: a new binding (session switch, resume) starts
//! unanchored.

use std::{
	borrow::Cow,
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
use omp_tools::read::SNAPSHOT_MAX_BYTES;
use parking_lot::Mutex;
use similar::{Algorithm, DiffOp, capture_diff_slices_deadline};
use tokio::time;
use url::Url;

use crate::{
	docs::{AcpDocumentBackend, DocumentHost, EditorIoError},
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
pub(crate) struct EditorSession {
	deadline: Duration,
	lineages: Mutex<Lineages>,
}

impl EditorSession {
	/// Starts an unanchored binding whose requests wait at most `deadline`
	/// (zero means the default).
	pub(crate) fn new(deadline: Duration) -> Self {
		Self {
			deadline: if deadline.is_zero() {
				DEFAULT_DEADLINE
			} else {
				deadline
			},
			lineages: Mutex::default(),
		}
	}

	/// Deadline for each editor request of this binding.
	pub(crate) const fn deadline(&self) -> Duration {
		self.deadline
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
pub(crate) struct EditorRoute {
	backend: Arc<dyn AcpDocumentBackend>,
	session: Arc<EditorSession>,
}

impl EditorRoute {
	/// Pairs an editor with the anchor table of its binding.
	pub(crate) fn new(backend: Arc<dyn AcpDocumentBackend>, session: Arc<EditorSession>) -> Self {
		Self { backend, session }
	}

	/// Reads the whole buffer of `path`, bounded by the binding's deadline and
	/// the snapshot cap.
	async fn read(&self, path: &Str) -> Result<Str, EditorIoError> {
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
pub(crate) enum Choice {
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
pub(crate) struct Selection {
	/// The choice.
	pub(crate) choice: Choice,
	/// B normalized to LF without BOM: what an anchor stores.
	pub(crate) buffer: Bytes,
	/// D normalized to LF without BOM.
	pub(crate) disk:   Bytes,
	/// The anchor K the choice was made against.
	pub(crate) anchor: Option<Bytes>,
}

/// Text with any BOM removed and every line ending normalized to LF.
fn normalized(text: &str) -> Bytes {
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
fn encode_like(disk: &str, text: &[u8]) -> Bytes {
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
pub(crate) fn select_base(
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

/// Records K := B once the authority committed an edit whose base was B.
#[derive(Debug)]
pub(crate) struct CommitAnchor {
	session: Arc<EditorSession>,
	path:    Str,
	buffer:  Bytes,
}

impl CommitAnchor {
	/// The commit is durable: the buffer is now part of the authority's
	/// lineage, so an editor that has not reloaded reads as stale-clean.
	pub(crate) fn committed(self) {
		self.session.anchor(&self.path, self.buffer);
	}
}

/// The base a Read or Edit uses for one document.
#[derive(Debug, Default)]
pub(crate) struct EffectiveBase {
	/// E when it differs from the disk head; `None` means disk is the base.
	pub(crate) bytes:     Option<Bytes>,
	/// Notices for the tool element.
	pub(crate) diags:     Vec<Diag>,
	/// Anchor to record once a commit used this base.
	pub(crate) on_commit: Option<CommitAnchor>,
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
pub(crate) struct ConflictLines {
	/// Lines in the anchor K (the typed ranges' own coordinates).
	pub(crate) anchor: (usize, usize),
	/// The same span projected into the editor buffer B.
	pub(crate) buffer: (usize, usize),
	/// The same span projected into the disk head D.
	pub(crate) disk:   (usize, usize),
}

/// The user's unsaved changes overlap a disk change made since the anchor.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct EditorConflict {
	/// Canonical path of the document.
	pub(crate) path:   Str,
	/// Typed conflicting ranges in K's normalized byte coordinates.
	pub(crate) ranges: Vec<ByteRange>,
	/// The ranges as line spans in K, B and D.
	pub(crate) lines:  Vec<ConflictLines>,
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
	pub(crate) fn diag(&self) -> Diag {
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
	if pb::DocumentKind::try_from(head.kind) != Ok(pb::DocumentKind::Text)
		|| head.presence != pb::DocumentPresence::Present as i32
		|| disk.len() > SNAPSHOT_MAX_BYTES
		|| str::from_utf8(disk).is_err()
	{
		return None;
	}
	let path = file_path(head.document.as_ref()?.uri.as_str())?;
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
	/// Chooses the effective base for one document whose disk head is `head`
	/// with bytes `disk` (ADR 0037 §3).
	///
	/// Without an editor bound to the invoking connection, or for an
	/// ineligible document, disk is the base and the editor is never asked.
	/// Otherwise the editor's buffer is read once. A conflicting anchored merge
	/// is returned as the error; every other outcome, including a failed read,
	/// yields a base.
	pub(crate) async fn editor_base(
		&self,
		head: &pb::DocumentHead,
		disk: &[u8],
	) -> Result<EffectiveBase, EditorConflict> {
		let Some(route) = super::tools::invocation_acp_documents() else {
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
		let Some(path) = eligible_path(self.hello().root_uri.as_str(), head, disk) else {
			return Ok(EffectiveBase::default());
		};
		let disk = str::from_utf8(disk).expect("eligible disk bytes are UTF-8");
		let buffer = match route.read(&path).await {
			Ok(buffer) => buffer,
			Err(error) => return Ok(EffectiveBase::unavailable(&path, error)),
		};
		let selection = route.session.select(&path, disk, &buffer);
		let (bytes, diags) = match &selection.choice {
			Choice::Clean | Choice::StaleClean => return Ok(EffectiveBase::default()),
			Choice::Conflict(ranges) => {
				return Err(EditorConflict::new(path, ranges.clone(), &selection));
			},
			Choice::Merged(merged) => (merged.clone(), Vec::new()),
			Choice::Unanchored(buffer) => (buffer.clone(), vec![Diag::warn(
				DiagKind::EditorBufferUnanchored,
				sf!(
					"{path} has unsaved editor changes with no known common base with the file on \
					 disk; the editor buffer was used as the base and replaces the file's content when \
					 an edit commits"
				),
			)]),
		};
		let bytes = (bytes.as_ref() != disk.as_bytes()).then_some(bytes);
		let mut diags = diags;
		if let Some(bytes) = &bytes {
			diags.push(Diag::info(
				DiagKind::Provenance,
				sf!("source=editor-buffer path={path} sha256={}", Hash32::sum(bytes)),
			));
		}
		let on_commit =
			CommitAnchor { session: Arc::clone(&route.session), path, buffer: selection.buffer };
		Ok(EffectiveBase { bytes, diags, on_commit: Some(on_commit) })
	}

	/// Reads the editor's buffer for a file an agent Write is about to replace,
	/// so the commit can anchor it: after the write the editor's content is
	/// known to be superseded by the authority, and an editor that has not
	/// reloaded reads as stale-clean instead of reverting the write. `None`
	/// without a bound editor, for an ineligible path, or when the editor
	/// cannot answer.
	pub(crate) async fn editor_superseded(&self, path: &Path) -> Option<CommitAnchor> {
		let route = super::tools::invocation_acp_documents()?;
		let root = file_path(self.hello().root_uri.as_str())?;
		let metadata = std::fs::metadata(path).ok()?;
		if path == root
			|| !path.starts_with(&root)
			|| !metadata.is_file()
			|| metadata.len() > SNAPSHOT_MAX_BYTES as u64
			|| path
				.extension()
				.is_some_and(|extension| extension.eq_ignore_ascii_case("ipynb"))
		{
			return None;
		}
		let path = Str::new(path.to_str()?);
		let buffer = route.read(&path).await.ok()?;
		Some(CommitAnchor { session: Arc::clone(&route.session), path, buffer: normalized(&buffer) })
	}
}

#[cfg(test)]
mod tests;
