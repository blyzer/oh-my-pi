//! Write-back of committed bytes to the ACP editor (ADR 0037 §4.3–§4.6).
//!
//! Every agent write commits through the document authority first. Only then
//! is the committed content R pushed to the editor, by the connection that
//! owns the editor, through [`EditorBackend`]:
//!
//! 1. Write-backs queue per path, first in first out. A newer committed
//!    revision supersedes a queued one that has not been sent: R is the whole
//!    file, so the latest wins. Different paths proceed concurrently.
//! 2. When the editor serves reads and the commit's base buffer B is known, the
//!    buffer B′ is read again. If the user typed meanwhile, the user's changes
//!    B→B′ are merged onto R (`rebase_content(B, R, B′)`), so keystrokes typed
//!    during the turn survive; an overlapping merge skips the write-back with
//!    typed ranges.
//! 3. `fs/write_text_file` replaces the buffer. ACP has no conditional write,
//!    so the round trip between the re-read and the write stays open.
//! 4. When the editor serves reads, the buffer is read back once; a difference
//!    from what was sent is the client reformatting it.
//!
//! A write-back belongs to the editor's session, not to the tool call that
//! committed: dropping the caller's future never cancels it. The connection
//! drains the queue, bounded, on session close, switch and graceful shutdown;
//! transport loss fails every request, which drops it.

use std::{future::Future, pin::Pin, str, sync::Arc, time::Duration};

use bytes::Bytes;
use omp_core::{FastHashMap, Str};
use omp_tools::read::SNAPSHOT_MAX_BYTES;
use parking_lot::Mutex;
use tokio::{sync::watch, time};

use crate::{
	docs::{AcpDocumentBackend, EditorCapabilities, EditorIoError, WriteBack, WriteBackOutcome},
	docserver::{ByteRange, rebase_content},
	editor_base::{encode_like, normalized},
};

/// The editor's own whole-file methods, as a connection reaches them
/// (`fs/read_text_file`, `fs/write_text_file`).
///
/// Implementations apply the request deadline and the connection's gates;
/// [`EditorBackend`] builds base reads and the write-back queue on top.
pub trait EditorFiles: Send + Sync + 'static {
	/// Reads the editor's whole buffer for a canonical absolute path.
	fn read_text(&self, path: Str) -> impl Future<Output = Result<Str, EditorIoError>> + Send + '_;

	/// Replaces the editor's buffer for a canonical absolute path.
	fn write_text(
		&self,
		path: Str,
		content: Str,
	) -> impl Future<Output = Result<(), EditorIoError>> + Send + '_;
}

type Reply = flume::Sender<Result<WriteBackOutcome, EditorIoError>>;

/// One queued write-back and where its outcome goes.
struct Job {
	write_back: WriteBack,
	reply:      Reply,
}

struct Shared<E> {
	files:        E,
	capabilities: EditorCapabilities,
	deadline:     Duration,
	/// Paths with a running worker, each with the one write-back waiting
	/// behind it; a path is present exactly while its worker runs.
	queues:       Mutex<FastHashMap<Str, Option<Job>>>,
	/// Write-backs submitted and not yet finished.
	outstanding:  watch::Sender<usize>,
}

/// An editor as the environment binds it: base reads, and write-backs on a
/// per-path queue (ADR 0037 §4.3).
///
/// Cloning is cheap; every clone shares the queue.
pub struct EditorBackend<E> {
	shared: Arc<Shared<E>>,
}

impl<E> Clone for EditorBackend<E> {
	fn clone(&self) -> Self {
		Self { shared: Arc::clone(&self.shared) }
	}
}

impl<E: EditorFiles> EditorBackend<E> {
	/// Binds `files` with the editor's `capabilities`; each request waits at
	/// most `deadline`.
	pub fn new(files: E, capabilities: EditorCapabilities, deadline: Duration) -> Self {
		Self {
			shared: Arc::new(Shared {
				files,
				capabilities,
				deadline,
				queues: Mutex::default(),
				outstanding: watch::Sender::new(0),
			}),
		}
	}

	/// The editor's file methods.
	pub fn files(&self) -> &E {
		&self.shared.files
	}

	/// Write-backs submitted and not yet finished.
	pub fn outstanding(&self) -> usize {
		*self.shared.outstanding.borrow()
	}

	/// Waits until every queued write-back has finished, at most `bound`.
	/// Returns whether the queue drained.
	pub async fn drain(&self, bound: Duration) -> bool {
		let mut outstanding = self.shared.outstanding.subscribe();
		time::timeout(bound, outstanding.wait_for(|count| *count == 0))
			.await
			.is_ok_and(|drained| drained.is_ok())
	}

	/// Queues `write_back` behind the path's earlier write-backs and returns
	/// where its outcome arrives. A write-back already waiting for the same
	/// path, not yet sent, is superseded.
	pub fn submit(
		&self,
		write_back: WriteBack,
	) -> flume::Receiver<Result<WriteBackOutcome, EditorIoError>> {
		let (reply, outcome) = flume::bounded(1);
		if !self.shared.capabilities.write {
			let _ = reply.send(Err(EditorIoError::Unavailable));
			return outcome;
		}
		let path = write_back.path.clone();
		let job = Job { write_back, reply };
		let mut queues = self.shared.queues.lock();
		if let Some(waiting) = queues.get_mut(&path) {
			match waiting.replace(job) {
				Some(superseded) => {
					let _ = superseded.reply.send(Ok(WriteBackOutcome::Superseded));
				},
				None => self.shared.outstanding.send_modify(|count| *count += 1),
			}
			return outcome;
		}
		queues.insert(path.clone(), None);
		self.shared.outstanding.send_modify(|count| *count += 1);
		drop(queues);
		tokio::spawn(Self::work(Arc::clone(&self.shared), path, job));
		outcome
	}

	/// Runs one path's write-backs in order until none waits.
	///
	/// A write-back queued behind another usually read its base before the
	/// earlier one reached the editor. The editor's change from that base to
	/// what the earlier write-back left is this queue's own doing, not the
	/// user's, so such a base is replaced by those bytes before merging.
	async fn work(shared: Arc<Shared<E>>, path: Str, mut job: Job) {
		let mut previous: Option<Landed> = None;
		loop {
			let mut write_back = job.write_back;
			if let (Some(landed), Some(base)) = (&previous, &write_back.base)
				&& landed.before.contains(&normalized(base))
			{
				write_back.base = Some(landed.after.clone());
			}
			let (outcome, buffer) = run(&shared, &write_back).await;
			previous = match &outcome {
				Ok(WriteBackOutcome::Written { merged, read_back }) => Some(Landed {
					before: [write_back.base.as_deref(), buffer.as_deref()]
						.map(|text| text.map(normalized).unwrap_or_default()),
					after:  read_back
						.clone()
						.or_else(|| merged.clone())
						.unwrap_or_else(|| write_back.content.clone()),
				}),
				_ => None,
			};
			let _ = job.reply.send(outcome);
			let mut queues = shared.queues.lock();
			shared
				.outstanding
				.send_modify(|count| *count = count.saturating_sub(1));
			let Some(next) = queues.get_mut(&path).and_then(Option::take) else {
				queues.remove(&path);
				return;
			};
			job = next;
		}
	}

	async fn read(&self, path: Str) -> Result<Str, EditorIoError> {
		read(&self.shared, path).await
	}
}

/// Reads one buffer within the deadline and the snapshot cap.
async fn read<E: EditorFiles>(shared: &Shared<E>, path: Str) -> Result<Str, EditorIoError> {
	if !shared.capabilities.read {
		return Err(EditorIoError::Unavailable);
	}
	let buffer = time::timeout(shared.deadline, shared.files.read_text(path))
		.await
		.unwrap_or(Err(EditorIoError::Timeout))?;
	if buffer.len() > SNAPSHOT_MAX_BYTES {
		return Err(EditorIoError::Oversize);
	}
	Ok(buffer)
}

/// What a finished write-back left in the editor, for the next write-back of
/// the same path.
struct Landed {
	/// Normalized buffers the editor held before it: the base and the re-read
	/// buffer (empty when not read).
	before: [Bytes; 2],
	/// What the editor holds after it.
	after:  Str,
}

/// One write-back, and the buffer its pre-write re-read found.
async fn run<E: EditorFiles>(
	shared: &Shared<E>,
	write_back: &WriteBack,
) -> (Result<WriteBackOutcome, EditorIoError>, Option<Str>) {
	let mut buffer = None;
	let outcome = write_back_once(shared, write_back, &mut buffer).await;
	(outcome, buffer)
}

/// One write-back: re-read and merge, write, read back.
async fn write_back_once<E: EditorFiles>(
	shared: &Shared<E>,
	write_back: &WriteBack,
	reread: &mut Option<Str>,
) -> Result<WriteBackOutcome, EditorIoError> {
	let committed = &write_back.content;
	let mut send = committed.clone();
	if shared.capabilities.read
		&& let Some(base) = &write_back.base
	{
		let buffer = reread.insert(read(shared, write_back.path.clone()).await?);
		match plan(base, committed, buffer) {
			Plan::Write(content) => send = content,
			Plan::Current(content) => {
				return Ok(WriteBackOutcome::Written {
					merged:    (content != *committed).then_some(content),
					read_back: None,
				});
			},
			Plan::Conflict(ranges) => return Ok(WriteBackOutcome::Conflict { ranges }),
		}
	}
	time::timeout(
		shared.deadline,
		shared
			.files
			.write_text(write_back.path.clone(), send.clone()),
	)
	.await
	.unwrap_or(Err(EditorIoError::Timeout))?;
	// A failed read-back leaves the write itself standing; only a buffer that
	// differs from what was sent is reported.
	let read_back = if shared.capabilities.read {
		read(shared, write_back.path.clone())
			.await
			.ok()
			.filter(|buffer| normalized(buffer) != normalized(&send))
	} else {
		None
	};
	Ok(WriteBackOutcome::Written { merged: (send != *committed).then_some(send), read_back })
}

/// What a write-back sends after re-reading the buffer.
#[derive(Debug, Eq, PartialEq)]
pub enum Plan {
	/// Send these bytes: R, or R with the user's later changes merged in.
	Write(Str),
	/// The buffer already holds these bytes; nothing is sent.
	Current(Str),
	/// The user's changes since the base overlap the commit; ranges in the
	/// base's normalized coordinates.
	Conflict(Vec<ByteRange>),
}

/// Chooses what to send given the base B (normalized), the committed bytes R
/// and the re-read buffer B′. Every result carries R's encoding.
pub fn plan(base: &str, committed: &Str, buffer: &str) -> Plan {
	let base = Bytes::copy_from_slice(base.as_bytes());
	let current = normalized(buffer);
	if current == base {
		return Plan::Write(committed.clone());
	}
	let target = normalized(committed);
	if current == target {
		return Plan::Current(committed.clone());
	}
	match rebase_content(&base, &target, &current) {
		Ok(Ok(merged)) => {
			let merged = if merged.content() == &target {
				committed.clone()
			} else {
				let encoded = encode_like(committed, merged.content());
				Str::from_utf8_owned(encoded).expect("merged editor text is UTF-8")
			};
			if normalized(&merged) == current {
				Plan::Current(merged)
			} else {
				Plan::Write(merged)
			}
		},
		Ok(Err(conflict)) => Plan::Conflict(conflict.into_ranges()),
		// Canonical edits are valid by construction; an invalid list means the
		// merge has no defined result, a conflict over the whole base.
		Err(_) => Plan::Conflict(
			ByteRange::new(0, u64::try_from(base.len()).unwrap_or(u64::MAX))
				.into_iter()
				.collect(),
		),
	}
}

impl<E: EditorFiles> AcpDocumentBackend for EditorBackend<E> {
	fn deadline(&self) -> Duration {
		self.shared.deadline
	}

	fn capabilities(&self) -> EditorCapabilities {
		self.shared.capabilities
	}

	fn read_text(
		&self,
		absolute_path: Str,
	) -> Pin<Box<dyn Future<Output = Result<Str, EditorIoError>> + Send + '_>> {
		Box::pin(self.read(absolute_path))
	}

	fn write_back(
		&self,
		write_back: WriteBack,
	) -> Pin<Box<dyn Future<Output = Result<WriteBackOutcome, EditorIoError>> + Send + '_>> {
		let outcome = self.submit(write_back);
		Box::pin(async move {
			outcome
				.recv_async()
				.await
				.unwrap_or(Err(EditorIoError::Disconnected))
		})
	}
}

#[cfg(test)]
mod tests;
