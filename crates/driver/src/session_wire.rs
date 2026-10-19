//! Wire adapter that turns a DOM subscription into a stream of [`AttachFrame`]
//! values suitable for the `omp.session.v1.SessionControl.Attach` gRPC stream.
//!
//! # Protocol
//!
//! 1. **Welcome** — one frame carrying the bridge epoch, `head_seq = 0`, an
//!    empty label, and empty `granted_scopes`.
//! 2. **SnapshotChunk** — the canonical `omp_dom` snapshot bytes sliced into ≤
//!    64 KiB pieces, each carrying a monotonically increasing `seq` and
//!    `index`; the last piece carries `r#final = true`.
//! 3. **EventFrame** — one frame per [`omp_dom::Event`], with `seq` incremented
//!    each time, carrying `serde_json`-serialized event bytes.
//! 4. **HeartbeatFrame** — emitted every 30 s of idle to signal liveness.
//!
//! The caller obtains `None` once the DOM channel closes.

use std::time::Duration;

use omp_dom::{Event, Snapshot};
use omp_proto::session::v1::{
	AttachFrame, EventFrame, HeartbeatFrame, SnapshotChunk, WelcomeFrame, attach_frame::Frame,
};
use serde::Serialize;
use tokio::time::sleep;

/// Maximum payload byte length of a single [`SnapshotChunk`].
const CHUNK_SIZE: usize = 64 * 1024;

/// Idle interval between heartbeat frames during live event forwarding.
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(30);

// ─── event serialization ─────────────────────────────────────────────────────

/// Serializable form of the `Event::Stream` variant's fields.
#[derive(Serialize)]
struct WireStream<'a> {
	kind:  &'static str,
	cause: omp_journal::EntryId,
	sid:   omp_dom::Sid,
	op:    omp_dom::StreamOp,
	node:  Option<omp_dom::Handle>,
	prop:  Option<&'a omp_dom::PropKey>,
	text:  Option<&'a omp_core::Str>,
}

/// Produces the canonical wire bytes for a [`omp_dom::Event`].
///
/// - `Patch` → serialized [`omp_dom::Patch`] (already `Serialize`).
/// - `Reset` → the snapshot's pre-computed canonical bytes (already JSON).
/// - `Stream` → a thin JSON object with a `"kind": "stream"` discriminant.
fn serialize_event(event: &Event) -> Vec<u8> {
	match event {
		Event::Patch(patch) => serde_json::to_vec(patch).expect("Patch is always JSON-serializable"),
		Event::Reset { snapshot } => snapshot.as_bytes().to_vec(),
		Event::Stream { cause, sid, op, node, prop, text } => serde_json::to_vec(&WireStream {
			kind:  "stream",
			cause: *cause,
			sid:   *sid,
			op:    *op,
			node:  *node,
			prop:  prop.as_ref(),
			text:  text.as_ref(),
		})
		.expect("WireStream fields are always JSON-serializable"),
	}
}

// ─── state machine
// ────────────────────────────────────────────────────────────

/// Phase of the wire pump.
///
/// `Welcome` carries the pre-encoded snapshot bytes so ownership transfers
/// directly into `Snapshot` after the Welcome frame is returned.
enum Phase {
	/// The Welcome frame has not yet been sent.
	Welcome { snapshot_bytes: Vec<u8> },
	/// Emitting snapshot chunks.  `offset` is the next byte to send; `seq`
	/// is the next sequence number (also used as the chunk index).
	Snapshot { snapshot_bytes: Vec<u8>, offset: usize, seq: u64 },
	/// Forwarding live DOM events.
	Live,
	/// The DOM channel has closed; nothing more to emit.
	Done,
}

// ─── public API ──────────────────────────────────────────────────────────────

/// Wire pump for a single `Attach` gRPC stream.
///
/// Constructed with [`SessionWire::attach`], then driven by repeated calls to
/// [`SessionWire::next_frame`].
pub struct SessionWire {
	epoch:    u64,
	head_seq: u64,
	phase:    Phase,
	rx:       flume::Receiver<Event>,
}

impl SessionWire {
	/// Creates a new wire pump from a DOM snapshot and a live-event receiver.
	///
	/// `epoch` identifies the bridge instance and is echoed verbatim in every
	/// outgoing frame.
	#[must_use]
	pub fn attach(snapshot: Snapshot, receiver: flume::Receiver<Event>, epoch: u64) -> Self {
		Self {
			epoch,
			head_seq: 0,
			phase: Phase::Welcome { snapshot_bytes: snapshot.into_bytes() },
			rx: receiver,
		}
	}

	/// Yields the next [`AttachFrame`], or `None` when the session has ended.
	///
	/// Drives the internal state machine:
	/// 1. Welcome frame on the first call.
	/// 2. One or more `SnapshotChunk` frames.
	/// 3. `EventFrame` for each live DOM event (30 s idle → `HeartbeatFrame`).
	/// 4. `None` once the channel closes.
	pub async fn next_frame(&mut self) -> Option<AttachFrame> {
		loop {
			match &mut self.phase {
				Phase::Welcome { snapshot_bytes } => {
					let snapshot_bytes = std::mem::take(snapshot_bytes);
					self.phase = Phase::Snapshot { snapshot_bytes, offset: 0, seq: 0 };
					return Some(AttachFrame {
						frame: Some(Frame::Welcome(WelcomeFrame {
							epoch:          self.epoch,
							head_seq:       0,
							label:          String::new(),
							granted_scopes: vec![],
						})),
					});
				},

				Phase::Snapshot { snapshot_bytes, offset, seq } => {
					let remaining = snapshot_bytes.len().saturating_sub(*offset);
					if remaining == 0 {
						self.phase = Phase::Live;
						continue;
					}
					let take = remaining.min(CHUNK_SIZE);
					let is_final = take == remaining;
					let chunk: prost::bytes::Bytes =
						snapshot_bytes[*offset..*offset + take].to_vec().into();
					let index = u32::try_from(*seq).unwrap_or(u32::MAX);
					let chunk_seq = *seq;
					*offset += take;
					*seq += 1;
					self.head_seq = chunk_seq;
					return Some(AttachFrame {
						frame: Some(Frame::SnapshotChunk(SnapshotChunk {
							epoch: self.epoch,
							seq: chunk_seq,
							index,
							bytes: chunk,
							r#final: is_final,
						})),
					});
				},

				Phase::Live => {
					tokio::select! {
						result = self.rx.recv_async() => {
							match result {
								Ok(event) => {
									self.head_seq += 1;
									let seq = self.head_seq;
									let bytes = serialize_event(&event).into();
									return Some(AttachFrame {
										frame: Some(Frame::Event(EventFrame {
											epoch: self.epoch,
											seq,
											bytes,
										})),
									});
								}
								Err(_) => {
									self.phase = Phase::Done;
									return None;
								}
							}
						}
						_ = sleep(HEARTBEAT_INTERVAL) => {
							return Some(AttachFrame {
								frame: Some(Frame::Heartbeat(HeartbeatFrame {
									head_seq: self.head_seq,
								})),
							});
						}
					}
				},

				Phase::Done => return None,
			}
		}
	}
}

#[cfg(test)]
mod tests {
	use omp_dom::{Dom, Txn};

	use super::*;

	fn wire_for(dom: &mut Dom, epoch: u64) -> SessionWire {
		let (snapshot, rx) = dom.subscribe();
		SessionWire::attach(snapshot, rx, epoch)
	}

	#[tokio::test]
	async fn welcome_is_first_frame() {
		let mut dom = Dom::new();
		let mut wire = wire_for(&mut dom, 42);
		let frame = wire.next_frame().await.expect("welcome");
		let Some(Frame::Welcome(w)) = frame.frame else {
			panic!("expected Welcome");
		};
		assert_eq!(w.epoch, 42);
		assert_eq!(w.head_seq, 0);
		assert!(w.granted_scopes.is_empty());
	}

	#[tokio::test]
	async fn snapshot_chunk_follows_welcome() {
		let mut dom = Dom::new();
		let mut wire = wire_for(&mut dom, 1);
		wire.next_frame().await; // Welcome
		let frame = wire.next_frame().await.expect("chunk");
		let Some(Frame::SnapshotChunk(chunk)) = frame.frame else {
			panic!("expected SnapshotChunk");
		};
		assert_eq!(chunk.epoch, 1);
		assert!(chunk.r#final, "empty DOM fits in one chunk");
	}

	#[tokio::test]
	async fn event_frame_carries_epoch_and_seq() {
		let mut dom = Dom::new();
		let mut wire = wire_for(&mut dom, 7);
		// Drain Welcome + all snapshot chunks.
		wire.next_frame().await;
		loop {
			let f = wire.next_frame().await.expect("snapshot in progress");
			if let Some(Frame::SnapshotChunk(c)) = &f.frame {
				if c.r#final {
					break;
				}
			}
		}
		// Commit an empty transaction to emit a DOM event.
		let txn = Txn { cause: omp_journal::EntryId::default(), label: None, ops: vec![] };
		dom.apply(&txn).expect("empty txn applies");
		let frame = wire.next_frame().await.expect("event");
		let Some(Frame::Event(ev)) = frame.frame else {
			panic!("expected Event");
		};
		assert_eq!(ev.epoch, 7);
		assert_eq!(ev.seq, 1);
		assert!(!ev.bytes.is_empty());
	}
}
