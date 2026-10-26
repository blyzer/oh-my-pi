//! Canonical JSON codec for replicated [`Event`] values.
//!
//! The envelope is the ADR 0003 kind-tagged object: `patch@1`, `snapshot@1`,
//! and `stream@1`. It carries no transport framing, so every bridge — the
//! collaboration journal, the gRPC session projection, and future adapters —
//! shares one byte-for-byte event representation.

use omp_core::Str;
use omp_journal::EntryId;
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
use thiserror::Error;

use crate::{Event, Handle, Patch, PropKey, Sid, Snapshot, SnapshotDecodeError, StreamOp};

/// Failure to decode a replicated [`Event`] from canonical bytes.
#[derive(Debug, Error)]
pub enum EventDecodeError {
	/// The payload was not a well-formed replicated event envelope.
	#[error("replicated DOM event envelope was malformed")]
	Envelope(#[from] serde_json::Error),
	/// The embedded session snapshot was malformed or inconsistent.
	#[error("replicated DOM event carried an invalid session snapshot")]
	Snapshot(#[from] SnapshotDecodeError),
}

#[derive(Serialize)]
#[serde(tag = "kind")]
enum Outgoing<'a> {
	#[serde(rename = "patch@1")]
	Patch { data: &'a Patch },
	#[serde(rename = "snapshot@1")]
	Reset { data: &'a RawValue },
	#[serde(rename = "stream@1")]
	Stream {
		cause: &'a EntryId,
		sid:   &'a Sid,
		op:    &'a StreamOp,
		node:  &'a Option<Handle>,
		prop:  &'a Option<PropKey>,
		text:  &'a Option<Str>,
	},
}

#[derive(Deserialize)]
#[serde(tag = "kind")]
enum Incoming {
	#[serde(rename = "patch@1")]
	Patch { data: Patch },
	#[serde(rename = "snapshot@1")]
	Reset { data: serde_json::Value },
	#[serde(rename = "stream@1")]
	Stream {
		cause: EntryId,
		sid:   Sid,
		op:    StreamOp,
		node:  Option<Handle>,
		prop:  Option<PropKey>,
		text:  Option<Str>,
	},
}

/// Encodes one subscription event as canonical envelope bytes.
pub fn encode_event(event: &Event) -> Result<Vec<u8>, serde_json::Error> {
	let outgoing = match event {
		Event::Patch(patch) => Outgoing::Patch { data: patch },
		Event::Reset { snapshot } => {
			return serde_json::to_vec(&Outgoing::Reset {
				data: serde_json::from_slice(snapshot.as_bytes())?,
			});
		},
		Event::Stream { cause, sid, op, node, prop, text } => {
			Outgoing::Stream { cause, sid, op, node, prop, text }
		},
	};
	serde_json::to_vec(&outgoing)
}

/// Decodes one subscription event from untrusted canonical envelope bytes.
pub fn decode_event(bytes: &[u8]) -> Result<Event, EventDecodeError> {
	Ok(match serde_json::from_slice::<Incoming>(bytes)? {
		Incoming::Patch { data } => Event::Patch(data),
		Incoming::Reset { data } => {
			Event::Reset { snapshot: Snapshot::from_bytes(&serde_json::to_vec(&data)?)? }
		},
		Incoming::Stream { cause, sid, op, node, prop, text } => {
			Event::Stream { cause, sid, op, node, prop, text }
		},
	})
}

#[cfg(test)]
mod tests {
	use omp_vocab::{KnownTag, PropId};
	use pretty_assertions::assert_eq;

	use super::*;
	use crate::{Dom, NodeSpec, Op, Txn, Value};

	fn populated() -> Dom {
		let mut dom = Dom::new();
		let body = dom.body();
		let turn = Handle::new(5).expect("five is nonzero");
		dom.apply(&Txn {
			cause: EntryId::default(),
			label: Some(Str::new_static("seed")),
			ops:   vec![
				Op::Ins { parent: body, after: None, node: NodeSpec::new(KnownTag::Turn) },
				Op::Ins {
					parent: turn,
					after:  None,
					node:   NodeSpec::new(KnownTag::User)
						.with_prop(PropId::Text, Value::Str(Str::new_static("hello")))
						.with_content("hello"),
				},
				Op::Ins {
					parent: turn,
					after:  None,
					node:   NodeSpec::new(KnownTag::Assistant)
						.with_prop(PropId::Status, Value::Str(Str::new_static("done")))
						.with_prop(PropId::TokensIn, Value::Int(12)),
				},
			],
		})
		.expect("seed transaction applies");
		dom
	}

	fn round_trip(event: &Event) {
		let bytes = encode_event(event).expect("event encodes");
		let decoded = decode_event(&bytes).expect("encoded event decodes");
		assert_eq!(&decoded, event);
	}

	#[test]
	fn patch_round_trips() {
		let body = Dom::new().body();
		round_trip(&Event::Patch(Patch {
			cause: EntryId::default(),
			prior: Some(EntryId::default()),
			label: Some(Str::new_static("turn")),
			ops:   vec![
				Op::Ins { parent: body, after: None, node: NodeSpec::new(KnownTag::Turn) },
				Op::Set {
					h:     body,
					prop:  PropId::Status.into(),
					value: Value::Str(Str::new_static("live")),
				},
				Op::Mv { h: body, parent: body, after: None },
				Op::Rm(body),
			],
		}));
	}

	#[test]
	fn reset_round_trips_with_a_populated_snapshot() {
		let snapshot = populated().snapshot();
		let event = Event::Reset { snapshot: snapshot.clone() };
		round_trip(&event);
		let bytes = encode_event(&event).expect("event encodes");
		let Event::Reset { snapshot: decoded } = decode_event(&bytes).expect("event decodes") else {
			panic!("snapshot@1 decodes as a reset");
		};
		assert_eq!(decoded.as_bytes(), snapshot.as_bytes());
	}

	#[test]
	fn stream_round_trips_in_every_phase() {
		let body = Dom::new().body();
		for event in [
			Event::Stream {
				cause: EntryId::default(),
				sid:   7,
				op:    StreamOp::Open,
				node:  Some(body),
				prop:  Some(PropId::Text.into()),
				text:  None,
			},
			Event::Stream {
				cause: EntryId::default(),
				sid:   7,
				op:    StreamOp::Append,
				node:  None,
				prop:  None,
				text:  Some(Str::new_static("delta")),
			},
			Event::Stream {
				cause: EntryId::default(),
				sid:   7,
				op:    StreamOp::Close,
				node:  None,
				prop:  None,
				text:  None,
			},
		] {
			round_trip(&event);
		}
	}

	#[test]
	fn kind_tags_are_stable() {
		let kind = |event: &Event| {
			let value: serde_json::Value =
				serde_json::from_slice(&encode_event(event).expect("event encodes"))
					.expect("envelope is an object");
			value["kind"]
				.as_str()
				.expect("envelope is tagged")
				.to_owned()
		};
		assert_eq!(
			kind(&Event::Patch(Patch {
				cause: EntryId::default(),
				prior: None,
				label: None,
				ops:   Vec::new(),
			})),
			"patch@1"
		);
		assert_eq!(kind(&Event::Reset { snapshot: Dom::new().snapshot() }), "snapshot@1");
		assert_eq!(
			kind(&Event::Stream {
				cause: EntryId::default(),
				sid:   1,
				op:    StreamOp::Close,
				node:  None,
				prop:  None,
				text:  None,
			}),
			"stream@1"
		);
	}

	#[test]
	fn malformed_envelopes_are_rejected() {
		assert!(matches!(decode_event(b"{}"), Err(EventDecodeError::Envelope(_))));
		assert!(matches!(
			decode_event(br#"{"kind":"snapshot@1","data":{"high_water":1}}"#),
			Err(EventDecodeError::Envelope(_) | EventDecodeError::Snapshot(_))
		));
	}
}
