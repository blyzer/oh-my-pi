//! JSON-RPC correlation, framing, and the cancel-safe ACP session transport.
//!
//! The session is generic over `AsyncRead + AsyncWrite` so tests can use
//! in-memory duplex pipes without spawning a real agent process.
//!
//! ## Concurrency model
//!
//! A writer task owns the output stream and drains an unbounded flume queue,
//! so sending a message never blocks the reader.  The reader is polled
//! inline through the cancel-safe [`LineReader`].
//!
//! ## Correlation
//!
//! `next_message` returns the next inbound line classified as either a
//! [`InboundMessage`]:
//! - a response (matched by id and removed from the pending table), or
//! - an agent-to-client request (`session/request_permission`), or
//! - a notification (`session/update`, `session/cancel`, …).
//!
//! An unknown id in a response is silently dropped; the pending table is
//! only ever modified by `send_request`/`complete_request`.
//!
//! ## Cancel safety
//!
//! `next_message` is cancel-safe because [`LineReader::next_line`] is
//! cancel-safe and all state lives outside the future.

use std::collections::HashMap;

use bytes::Bytes;
use omp_core::Str;
use serde::Serialize;
use serde_json::value::RawValue;
use tokio::{
	io::{AsyncRead, AsyncWrite, AsyncWriteExt as _},
	task::JoinHandle,
};

use super::wire::{
	Envelope, JsonRpcMessage, Method, OutError, OutErrorBody, OutNotification, OutRequest,
	OutResponse, RequestId,
};
use crate::line_reader::{FrameError, LineReader};

/// Largest inbound line accepted from an ACP agent.
///
/// Permission requests and update notifications can be large; 8 MiB is the
/// same cap used for the Codex adapter.
pub const MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;

// ─── Public types ────────────────────────────────────────────────────────────

/// A decoded inbound ACP message with the raw bytes retained.
pub enum InboundMessage {
	/// A successful response to a request we sent.
	Response {
		/// Id of the matched request.
		id:     RequestId,
		/// The raw `result` value.
		result: Box<RawValue>,
	},
	/// An error response to a request we sent.
	ErrorResponse {
		/// Id of the matched request (absent for parse-error responses).
		id:   Option<RequestId>,
		code: i64,
	},
	/// An agent-to-client request waiting for our answer.
	AgentRequest {
		id:     RequestId,
		method: Method,
		/// Raw params; caller decodes the concrete type.
		params: Option<Box<RawValue>>,
	},
	/// A notification; no response is expected.
	Notification {
		method: Method,
		/// Raw params.
		params: Option<Box<RawValue>>,
	},
	/// A line that could not be parsed as JSON-RPC.
	Malformed {
		/// The raw bytes.
		line:   Bytes,
		/// A human-readable description of the parse failure.
		reason: Str,
	},
}

/// Error produced by [`AcpSession`].
#[derive(Debug, thiserror::Error)]
pub enum SessionError {
	/// A frame violated the hard size cap or a stream-level I/O error.
	#[error("framing error: {0}")]
	Frame(#[from] FrameError),
	/// Serializing an outbound message failed (should be impossible for our
	/// typed structs, but the type system requires the branch).
	#[error("serialization failed: {0}")]
	Serialize(#[source] serde_json::Error),
}

// ─── AcpSession ──────────────────────────────────────────────────────────────

/// A live ACP connection over a pair of byte streams.
///
/// The session is transport-agnostic: production code hands it the stdio
/// pipes of a spawned agent process; tests hand it in-memory duplex streams.
pub struct AcpSession<R> {
	reader:  LineReader<R>,
	/// Queue to the writer task.
	tx:      flume::Sender<Bytes>,
	/// Next request id to use.
	next_id: i64,
	/// Requests we have sent and not yet received a response to.
	pending: HashMap<i64, ()>,
	_writer: JoinHandle<()>,
}

impl<R: AsyncRead + Unpin> AcpSession<R> {
	/// Construct a session from separate reader and writer streams.
	///
	/// The writer task is spawned immediately.
	pub fn new<W: AsyncWrite + Send + Unpin + 'static>(reader: R, writer: W, cap: usize) -> Self {
		let (tx, rx) = flume::unbounded::<Bytes>();
		let handle = tokio::spawn(write_loop(writer, rx));
		Self {
			reader: LineReader::new(reader, cap),
			tx,
			next_id: 1,
			pending: HashMap::new(),
			_writer: handle,
		}
	}

	/// Construct with the default [`MAX_FRAME_BYTES`] cap.
	pub fn with_default_cap<W: AsyncWrite + Send + Unpin + 'static>(reader: R, writer: W) -> Self {
		Self::new(reader, writer, MAX_FRAME_BYTES)
	}

	/// Send a request to the agent and return the id allocated for it.
	///
	/// The caller is responsible for reading responses with `next_message`
	/// and matching them by id.
	///
	/// # Errors
	/// Returns [`SessionError::Serialize`] if serialization fails (should
	/// never happen for our concrete types).
	/// Serialize `value` and enqueue it as one newline-terminated frame.
	fn send_line<V: Serialize>(&self, value: &V) -> Result<(), SessionError> {
		let mut line = serde_json::to_vec(value).map_err(SessionError::Serialize)?;
		line.push(b'\n');
		let _ = self.tx.send(Bytes::from(line));
		Ok(())
	}

	/// Send a request to the agent and return the id allocated for it.
	///
	/// The caller is responsible for reading responses with `next_message`
	/// and matching them by id.
	///
	/// # Errors
	/// Returns [`SessionError::Serialize`] if serialization fails (should
	/// never happen for our concrete types).
	pub fn send_request<P: Serialize>(
		&mut self,
		method: &str,
		params: P,
	) -> Result<i64, SessionError> {
		let id = self.next_id;
		self.next_id += 1;
		let msg = OutRequest { jsonrpc: "2.0", id, method, params };
		self.pending.insert(id, ());
		self.send_line(&msg)?;
		Ok(id)
	}

	/// Send a notification to the agent (no id, no response expected).
	///
	/// # Errors
	/// Returns [`SessionError::Serialize`] if serialization fails.
	pub fn send_notification<P: Serialize>(
		&mut self,
		method: &'static str,
		params: P,
	) -> Result<(), SessionError> {
		let msg = OutNotification { jsonrpc: "2.0", method, params };
		self.send_line(&msg)
	}

	/// Answer an agent-to-client request with a successful result.
	///
	/// # Errors
	/// Returns [`SessionError::Serialize`] if serialization fails.
	pub fn answer_request<R2: Serialize>(
		&mut self,
		id: &RequestId,
		result: R2,
	) -> Result<(), SessionError> {
		let msg = OutResponse { jsonrpc: "2.0", id, result };
		self.send_line(&msg)
	}

	/// Answer an agent-to-client request with an error.
	///
	/// # Errors
	/// Returns [`SessionError::Serialize`] if serialization fails.
	pub fn answer_error(
		&mut self,
		id: &RequestId,
		code: i64,
		message: Str,
	) -> Result<(), SessionError> {
		let msg = OutError { jsonrpc: "2.0", id, error: OutErrorBody { code, message } };
		self.send_line(&msg)
	}

	/// Read and classify the next inbound line.
	///
	/// - A response whose id is not in the pending table is **dropped** and the
	///   method loops to the next line (unknown id does not surface to the
	///   caller).
	/// - A successfully correlated response is removed from the pending table.
	/// - Notifications and agent requests are surfaced directly.
	/// - A line that fails JSON-RPC classification surfaces as
	///   [`InboundMessage::Malformed`].
	/// - Returns `Ok(None)` on a clean EOF.
	///
	/// Cancel-safe: dropping the future loses no byte.
	///
	/// # Errors
	/// Returns [`SessionError::Frame`] on I/O errors or oversize/truncated
	/// stream conditions.
	pub async fn next_message(&mut self) -> Result<Option<InboundMessage>, SessionError> {
		loop {
			let line = match self.reader.next_line().await? {
				Some(l) => l,
				None => return Ok(None),
			};

			// Borrow the bytes for zero-copy deserialization of the envelope.
			let text = match std::str::from_utf8(&line) {
				Ok(s) => s,
				Err(e) => {
					return Ok(Some(InboundMessage::Malformed {
						line,
						reason: Str::new(e.to_string()),
					}));
				},
			};

			let env: Envelope<'_> = match serde_json::from_str(text) {
				Ok(e) => e,
				Err(e) => {
					return Ok(Some(InboundMessage::Malformed {
						line,
						reason: Str::new(e.to_string()),
					}));
				},
			};

			let classified = match env.classify() {
				Some(c) => c,
				None => {
					return Ok(Some(InboundMessage::Malformed {
						line,
						reason: Str::new_static("not a valid JSON-RPC message"),
					}));
				},
			};

			match classified {
				JsonRpcMessage::Response { id, result } => {
					// We only issue integer ids; a text-id response is unsolicited.
					let matched = match &id {
						RequestId::Int(i) => self.pending.remove(i).is_some(),
						RequestId::Text(_) => false,
					};
					if !matched {
						// Unknown or text id — silently drop and read the next line.
						continue;
					}
					let result = result.to_owned();
					return Ok(Some(InboundMessage::Response { id, result }));
				},
				JsonRpcMessage::Error { id, code } => {
					if let Some(RequestId::Int(i)) = &id {
						self.pending.remove(i);
					}
					return Ok(Some(InboundMessage::ErrorResponse { id, code }));
				},
				JsonRpcMessage::Request { id, method, params } => {
					let method = Method::classify(&method);
					let params = params.map(|p| p.to_owned());
					return Ok(Some(InboundMessage::AgentRequest { id, method, params }));
				},
				JsonRpcMessage::Notification { method, params } => {
					let method = Method::classify(&method);
					let params = params.map(|p| p.to_owned());
					return Ok(Some(InboundMessage::Notification { method, params }));
				},
			}
		}
	}

	/// The number of requests currently in-flight (sent but not answered).
	#[cfg(test)]
	pub fn pending_count(&self) -> usize {
		self.pending.len()
	}
}

// ─── Writer task ─────────────────────────────────────────────────────────────

async fn write_loop<W: AsyncWrite + Unpin>(mut writer: W, queue: flume::Receiver<Bytes>) {
	while let Ok(chunk) = queue.recv_async().await {
		if writer.write_all(&chunk).await.is_err() {
			break;
		}
	}
}
