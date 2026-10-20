//! Newline-delimited frame reader with a hard size cap.
//!
//! The reader owns its partial-line buffer, so [`LineReader::next_line`] is
//! cancel-safe: dropping the future between reads loses no byte.
//!
//! Both the Codex `app-server` adapter and the ACP server transport use this
//! reader. It lives here rather than inside either module so neither has to
//! re-implement it.

use bytes::{Buf as _, Bytes, BytesMut};
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncReadExt as _};

/// Initial read chunk. Not a limit.
const CHUNK: usize = 8 * 1024;

/// Why a line could not be produced.
#[derive(Debug, Error)]
pub enum FrameError {
	/// A line grew past the cap without a terminating newline.
	#[error("line exceeds {limit} bytes")]
	Oversize {
		/// The enforced cap in bytes, excluding the newline.
		limit: usize,
	},
	/// The stream ended inside a line.
	#[error("stream ended inside a line")]
	Truncated,
	/// Reading the stream failed.
	#[error("reading the stream failed")]
	Io(#[source] std::io::Error),
}

/// Splits a byte stream into non-empty lines of at most `cap` bytes.
pub struct LineReader<R> {
	reader: R,
	buf:    BytesMut,
	cap:    usize,
}

impl<R: AsyncRead + Unpin> LineReader<R> {
	/// Wraps `reader`; no line longer than `cap` bytes is ever returned.
	pub fn new(reader: R, cap: usize) -> Self {
		Self { reader, buf: BytesMut::with_capacity(CHUNK.min(cap + 1)), cap }
	}

	/// Returns the next non-empty line without its terminator (`\n` or
	/// `\r\n`), or `None` at a clean end of stream.
	///
	/// Cancel-safe.
	///
	/// # Errors
	///
	/// [`FrameError::Oversize`] as soon as more than `cap` bytes are buffered
	/// without a newline (or a complete line is longer than `cap`),
	/// [`FrameError::Truncated`] when the stream ends mid-line.
	pub async fn next_line(&mut self) -> Result<Option<Bytes>, FrameError> {
		loop {
			if let Some(end) = self.buf.iter().position(|&byte| byte == b'\n') {
				let mut line = self.buf.split_to(end);
				self.buf.advance(1);
				if line.last() == Some(&b'\r') {
					line.truncate(line.len() - 1);
				}
				if line.len() > self.cap {
					return Err(FrameError::Oversize { limit: self.cap });
				}
				if line.is_empty() {
					continue;
				}
				return Ok(Some(line.freeze()));
			}
			if self.buf.len() > self.cap {
				return Err(FrameError::Oversize { limit: self.cap });
			}
			self
				.buf
				.reserve(CHUNK.min(self.cap + 1 - self.buf.len()).max(1));
			match self.reader.read_buf(&mut self.buf).await {
				Ok(0) if self.buf.is_empty() => return Ok(None),
				Ok(0) => return Err(FrameError::Truncated),
				Ok(_) => {},
				Err(error) => return Err(FrameError::Io(error)),
			}
		}
	}
}

#[cfg(test)]
mod tests {
	use std::time::Duration;

	use tokio::io::{AsyncWriteExt as _, duplex};

	use super::*;

	async fn reader(input: &[u8], cap: usize) -> LineReader<tokio::io::DuplexStream> {
		let (mut tx, rx) = duplex(64 * 1024);
		tx.write_all(input).await.expect("write");
		drop(tx);
		LineReader::new(rx, cap)
	}

	#[tokio::test]
	async fn splits_lines_and_skips_blank_ones() {
		let mut lines = reader(b"one\n\n two \r\nthree\n", 64).await;
		assert_eq!(lines.next_line().await.expect("line").as_deref(), Some(&b"one"[..]));
		assert_eq!(lines.next_line().await.expect("line").as_deref(), Some(&b" two "[..]));
		assert_eq!(lines.next_line().await.expect("line").as_deref(), Some(&b"three"[..]));
		assert!(lines.next_line().await.expect("eof").is_none());
	}

	#[tokio::test]
	async fn a_line_at_the_cap_is_accepted_and_one_byte_more_is_not() {
		let mut ok = reader(b"12345\n", 5).await;
		assert_eq!(ok.next_line().await.expect("line").as_deref(), Some(&b"12345"[..]));
		let mut over = reader(b"123456\n", 5).await;
		assert!(matches!(over.next_line().await, Err(FrameError::Oversize { limit: 5 })));
	}

	#[tokio::test]
	async fn an_unterminated_flood_fails_without_buffering_it_all() {
		let (mut tx, rx) = duplex(1024 * 1024);
		tx.write_all(&vec![b'x'; 100_000]).await.expect("write");
		let mut lines = LineReader::new(rx, 1000);
		assert!(matches!(lines.next_line().await, Err(FrameError::Oversize { limit: 1000 })));
		assert!(lines.buf.len() <= 1000 + CHUNK, "buffered {} bytes", lines.buf.len());
	}

	#[tokio::test]
	async fn a_stream_ending_mid_line_is_truncated() {
		let mut lines = reader(b"whole\npart", 64).await;
		assert!(lines.next_line().await.expect("line").is_some());
		assert!(matches!(lines.next_line().await, Err(FrameError::Truncated)));
	}

	#[tokio::test]
	async fn dropping_the_future_mid_line_loses_nothing() {
		let (mut tx, rx) = duplex(1024);
		let mut lines = LineReader::new(rx, 64);
		tx.write_all(b"hel").await.expect("write");
		let waited = tokio::time::timeout(Duration::from_millis(20), lines.next_line()).await;
		assert!(waited.is_err(), "no newline yet, the read must still be pending");
		tx.write_all(b"lo\n").await.expect("write");
		assert_eq!(lines.next_line().await.expect("line").as_deref(), Some(&b"hello"[..]));
	}
}
