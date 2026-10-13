//! Raw Server-Sent Events framing used by `.oms` journals.
//!
//! The committing boundary is the blank line after a frame. Canonical output
//! orders fields as comment, `event`, `id`, `by`, `prior`, then `data`.

use std::{
	io::{self, BufRead, Write as _},
	ops::Range,
	str::FromStr as _,
};

use memchr::memchr;
use omp_core::{Str, UlidParseError};
use thiserror::Error;

use crate::{Entry, Kind, KindError};

/// Maximum inline JSON payload size: one mebibyte.
pub const DATA_HARD_CAP: usize = 1 << 20;

/// Bytes a frame may spend on everything except its payload: the `event`,
/// `id`, `by`, `prior` and `data:` field lines, the committing blank line, and
/// the optional label comment.
pub const FRAME_OVERHEAD_CAP: usize = 64 << 10;

/// Largest legal frame, from its first byte through its committing blank
/// line: a maximal payload plus [`FRAME_OVERHEAD_CAP`].
///
/// Readers fail a block that grows past this without a terminator before
/// buffering any more of it, so a hostile or damaged `.oms` cannot make a
/// reader allocate without bound. [`encode`] refuses to write a larger frame,
/// so a journal this build wrote never trips the reader.
pub const FRAME_HARD_CAP: usize = DATA_HARD_CAP + FRAME_OVERHEAD_CAP;

/// One decoded complete frame and its byte span.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
	/// Decoded entry.
	pub entry: Entry,
	/// Frame bytes, including the committing blank line.
	pub span:  Range<usize>,
}

/// Failure to encode or decode an SSE frame.
#[derive(Debug, Error)]
pub enum SseError {
	/// The mandatory `id` field is absent.
	#[error("journal frame is missing `id`")]
	MissingId,
	/// The mandatory `event` field is absent.
	#[error("journal frame is missing `event`")]
	MissingEvent,
	/// The mandatory `data` field is absent.
	#[error("journal frame is missing `data`")]
	MissingData,
	/// A frame is not UTF-8.
	#[error("journal frame is not UTF-8")]
	InvalidUtf8 {
		/// UTF-8 decoder failure.
		#[source]
		source: std::str::Utf8Error,
	},
	/// The `id` field is not a ULID.
	#[error("journal frame `id` is invalid")]
	InvalidId {
		/// ULID parse failure.
		#[source]
		source: UlidParseError,
	},
	/// The `by` field is not a ULID.
	#[error("journal frame `by` is invalid")]
	InvalidCause {
		/// ULID parse failure.
		#[source]
		source: UlidParseError,
	},
	/// The `prior` field is not a ULID.
	#[error("journal frame `prior` is invalid")]
	InvalidPrior {
		/// ULID parse failure.
		#[source]
		source: UlidParseError,
	},
	/// The `event` field is not a versioned kind.
	#[error("journal frame `event` is invalid")]
	InvalidKind {
		/// Kind parse failure.
		#[source]
		source: KindError,
	},
	/// The `event` field is outside the exact closed vocabulary.
	#[error("journal frame kind {kind} is not in the closed revision-1 vocabulary")]
	UnknownKind {
		/// Unsupported versioned kind.
		kind: Kind,
	},
	/// The JSON payload is invalid.
	#[error("journal frame `data` is invalid JSON")]
	InvalidData {
		/// JSON decoder failure.
		#[source]
		source: serde_json::Error,
	},
	/// The JSON payload contains a physical line break.
	#[error("journal frame `data` must occupy one physical line")]
	MultilineData,
	/// The optional label contains a physical line break.
	#[error("journal frame label must occupy one physical line")]
	MultilineLabel,
	/// The JSON payload exceeds the hard cap.
	#[error("journal frame `data` is {len} bytes; maximum is 1048576")]
	DataTooLarge {
		/// Payload byte length.
		len: usize,
	},
	/// A frame exceeds [`FRAME_HARD_CAP`] bytes: either a block that reached
	/// the cap without its committing blank line, or a frame too large to
	/// write.
	#[error("journal frame is larger than the {limit}-byte frame limit")]
	FrameTooLarge {
		/// The frame byte limit that was exceeded.
		limit: usize,
	},
}

/// Encodes one entry in canonical field order.
///
/// # Errors
///
/// Returns [`SseError`] when the label or payload cannot be represented in a
/// single frame, the payload is invalid JSON, it exceeds the hard cap, or the
/// whole frame would exceed [`FRAME_HARD_CAP`].
pub fn encode(entry: &Entry, output: &mut Vec<u8>) -> Result<(), SseError> {
	validate_label(entry.label.as_deref())?;
	validate_data(entry.data.as_str())?;
	if !entry.kind.is_known() {
		return Err(SseError::UnknownKind { kind: entry.kind.clone() });
	}
	let start = output.len();
	if let Some(label) = &entry.label {
		let _ = writeln!(output, ": {label}");
	}
	let _ = writeln!(output, "event: {}", entry.kind);
	let _ = writeln!(output, "id: {}", entry.id);
	if let Some(by) = entry.by {
		let _ = writeln!(output, "by: {by}");
	}
	if let Some(prior) = entry.prior {
		let _ = writeln!(output, "prior: {prior}");
	}
	let _ = write!(output, "data: {}\n\n", entry.data);
	if output.len() - start > FRAME_HARD_CAP {
		output.truncate(start);
		return Err(SseError::FrameTooLarge { limit: FRAME_HARD_CAP });
	}
	Ok(())
}

/// Validates one inline JSON payload.
///
/// # Errors
///
/// Returns [`SseError`] for oversized, multiline, or malformed JSON.
pub fn validate_data(data: &str) -> Result<(), SseError> {
	if data.len() > DATA_HARD_CAP {
		return Err(SseError::DataTooLarge { len: data.len() });
	}
	if data.bytes().any(|byte| matches!(byte, b'\r' | b'\n')) {
		return Err(SseError::MultilineData);
	}
	serde_json::from_str::<serde::de::IgnoredAny>(data)
		.map(|_| ())
		.map_err(|source| SseError::InvalidData { source })
}

fn validate_label(label: Option<&str>) -> Result<(), SseError> {
	if label.is_some_and(|value| value.bytes().any(|byte| matches!(byte, b'\r' | b'\n'))) {
		return Err(SseError::MultilineLabel);
	}
	Ok(())
}

/// Single-pass scanner over complete blank-line-committed frames.
#[derive(Debug)]
pub struct Scanner<'a> {
	bytes:  &'a [u8],
	offset: usize,
}

impl<'a> Scanner<'a> {
	/// Creates a scanner over an SSE byte buffer.
	#[must_use]
	pub const fn new(bytes: &'a [u8]) -> Self {
		Self { bytes, offset: 0 }
	}

	/// Returns the byte immediately after the last complete frame.
	#[must_use]
	pub const fn offset(&self) -> usize {
		self.offset
	}

	/// Returns whether bytes remain after the last complete frame.
	#[must_use]
	pub const fn has_torn_tail(&self) -> bool {
		self.offset < self.bytes.len()
	}

	/// Decodes the next complete frame, stopping before a torn tail.
	///
	/// # Errors
	///
	/// Returns [`SseError`] when a complete frame is malformed, or when a
	/// block is larger than [`FRAME_HARD_CAP`] (terminated or not).
	#[allow(clippy::should_implement_trait, reason = "scanner reports typed frame errors")]
	pub fn next(&mut self) -> Option<Result<Frame, SseError>> {
		loop {
			let start = self.offset;
			let Some(end) = complete_block_end(self.bytes, start) else {
				return (self.bytes.len() - start > FRAME_HARD_CAP)
					.then_some(Err(SseError::FrameTooLarge { limit: FRAME_HARD_CAP }));
			};
			if end - start > FRAME_HARD_CAP {
				return Some(Err(SseError::FrameTooLarge { limit: FRAME_HARD_CAP }));
			}
			self.offset = end;
			let raw = &self.bytes[start..end];
			if raw == b"\n" || raw == b"\r\n" {
				continue;
			}
			return Some(parse_frame(raw).map(|entry| Frame { entry, span: start..end }));
		}
	}
}

fn complete_block_end(bytes: &[u8], start: usize) -> Option<usize> {
	let mut cursor = start;
	loop {
		let relative = memchr(b'\n', &bytes[cursor..])?;
		let newline = cursor + relative;
		let line = &bytes[cursor..newline];
		cursor = newline + 1;
		if line.is_empty() || line == b"\r" {
			return Some(cursor);
		}
	}
}

/// How reading one block from a stream ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Block {
	/// The buffer holds one complete block, committing blank line included.
	Complete,
	/// The stream ended inside a block: a torn tail. The buffer holds it.
	Torn,
	/// The stream ended exactly on a block boundary.
	End,
	/// The block would grow past [`FRAME_HARD_CAP`] before its committing
	/// blank line. Nothing beyond the cap was buffered.
	TooLarge,
}

/// Reads the next blank-line-terminated block into `block`, replacing its
/// contents, buffering at most [`FRAME_HARD_CAP`] bytes.
///
/// The cap is checked before each line is copied, so an endless line or a
/// stream of lines that never reaches a blank one stops at the cap instead of
/// growing the buffer.
pub(crate) fn read_block<R: BufRead>(reader: &mut R, block: &mut Vec<u8>) -> io::Result<Block> {
	block.clear();
	let mut line_start = 0;
	loop {
		let available = reader.fill_buf()?;
		if available.is_empty() {
			return Ok(if block.is_empty() {
				Block::End
			} else {
				Block::Torn
			});
		}
		let (take, line_done) = match memchr(b'\n', available) {
			Some(index) => (index + 1, true),
			None => (available.len(), false),
		};
		if take > FRAME_HARD_CAP - block.len() {
			return Ok(Block::TooLarge);
		}
		block.extend_from_slice(&available[..take]);
		reader.consume(take);
		if line_done {
			let line = &block[line_start..];
			if line == b"\n" || line == b"\r\n" {
				return Ok(Block::Complete);
			}
			line_start = block.len();
		}
	}
}

pub(crate) fn parse_frame(raw: &[u8]) -> Result<Entry, SseError> {
	let text = std::str::from_utf8(raw).map_err(|source| SseError::InvalidUtf8 { source })?;
	let mut id = None;
	let mut kind = None;
	let mut by = None;
	let mut prior = None;
	let mut label = None;
	let mut data = None::<String>;

	for line in text.lines() {
		if line.is_empty() {
			break;
		}
		if let Some(comment) = line.strip_prefix(':') {
			if label.is_none() {
				label = Some(Str::new(comment.strip_prefix(' ').unwrap_or(comment)));
			}
			continue;
		}
		let Some((field, value)) = line.split_once(':') else {
			continue;
		};
		let value = value.strip_prefix(' ').unwrap_or(value);
		match field {
			"id" => {
				id = Some(
					value
						.parse()
						.map_err(|source| SseError::InvalidId { source })?,
				);
			},
			"event" => {
				kind = Some(Kind::from_str(value).map_err(|source| SseError::InvalidKind { source })?);
			},
			"by" => {
				by = Some(
					value
						.parse()
						.map_err(|source| SseError::InvalidCause { source })?,
				);
			},
			"prior" => {
				prior = Some(
					value
						.parse()
						.map_err(|source| SseError::InvalidPrior { source })?,
				);
			},
			"data" => {
				if let Some(existing) = &mut data {
					existing.push('\n');
					existing.push_str(value);
				} else {
					data = Some(value.to_owned());
				}
			},
			_ => {},
		}
	}

	let data = data.ok_or(SseError::MissingData)?;
	if data.len() > DATA_HARD_CAP {
		return Err(SseError::DataTooLarge { len: data.len() });
	}
	serde_json::from_str::<serde::de::IgnoredAny>(&data)
		.map_err(|source| SseError::InvalidData { source })?;
	let kind = kind.ok_or(SseError::MissingEvent)?;
	if !kind.is_known() {
		return Err(SseError::UnknownKind { kind });
	}
	Ok(Entry { id: id.ok_or(SseError::MissingId)?, kind, by, prior, label, data: Str::new(data) })
}

#[cfg(test)]
mod tests {
	use std::io::{self, BufReader};

	use super::*;

	/// An endless stream that never contains a newline must stop at the frame
	/// cap, with the buffer bounded by it: the reader never asks for more bytes
	/// than the cap plus one read-buffer.
	#[test]
	fn endless_unterminated_line_stops_at_the_frame_cap() {
		let mut reader = BufReader::with_capacity(4096, io::repeat(b'a'));
		let mut block = Vec::new();
		assert_eq!(read_block(&mut reader, &mut block).expect("read"), Block::TooLarge);
		assert!(block.len() <= FRAME_HARD_CAP, "buffered {} bytes", block.len());
	}

	/// The same bound holds for a block of many short lines that never reaches
	/// the committing blank line.
	#[test]
	fn endless_lines_without_a_blank_line_stop_at_the_frame_cap() {
		let lines = LineEvery { inner: io::repeat(b'x'), every: 8, since: 0 };
		let mut reader = BufReader::with_capacity(4096, lines);
		let mut block = Vec::new();
		assert_eq!(read_block(&mut reader, &mut block).expect("read"), Block::TooLarge);
		assert!(block.len() <= FRAME_HARD_CAP);
	}

	#[test]
	fn a_maximal_legal_frame_is_complete_and_a_torn_one_is_torn() {
		let payload = "x".repeat(DATA_HARD_CAP - 2);
		let frame =
			format!("event: patch@1\nid: 01ARZ3NDEKTSV4RRFFQ69G5FAV\ndata: \"{payload}\"\n\n");
		let mut block = Vec::new();
		let mut reader = BufReader::new(frame.as_bytes());
		assert_eq!(read_block(&mut reader, &mut block).expect("read"), Block::Complete);
		assert_eq!(block.len(), frame.len());
		assert_eq!(read_block(&mut reader, &mut block).expect("read"), Block::End);

		let mut reader = BufReader::new(&frame.as_bytes()[..frame.len() - 1]);
		assert_eq!(read_block(&mut reader, &mut block).expect("read"), Block::Torn);
	}

	/// Inserts a newline after every `every` bytes of the inner stream.
	struct LineEvery<R> {
		inner: R,
		every: usize,
		since: usize,
	}

	impl<R: io::Read> io::Read for LineEvery<R> {
		fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
			if buf.is_empty() {
				return Ok(0);
			}
			if self.since == self.every {
				self.since = 0;
				buf[0] = b'\n';
				return Ok(1);
			}
			let want = buf.len().min(self.every - self.since);
			let read = self.inner.read(&mut buf[..want])?;
			self.since += read;
			Ok(read)
		}
	}
}
