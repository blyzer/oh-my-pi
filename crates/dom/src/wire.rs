//! Canonical JSON codec for replicated [`Event`] values.
//!
//! The envelope is the ADR 0003 kind-tagged object: `patch@1`, `snapshot@1`,
//! and `stream@1`. It carries no transport framing, so every bridge — the
//! collaboration journal, the gRPC session projection, and future adapters —
//! shares one byte-for-byte event representation.
//!
//! `snapshot@1` carries snapshot bytes verbatim in both directions. The
//! encoder embeds [`Snapshot::as_bytes`] without reparsing, and the decoder
//! hands the borrowed envelope slice straight to [`Snapshot::from_bytes`].
//! Neither side materializes an intermediate [`serde_json::Value`], so an
//! embedded [`crate::Value::Json`] property crosses the wire with the exact
//! spacing and number form the producer wrote.
//!
//! Byte identity end to end is still bounded by [`Snapshot::from_bytes`],
//! which canonicalizes every property it validates. A snapshot whose own bytes
//! are not a fixed point of that validator — one carrying a non-compact
//! [`crate::Value::Json`] minted locally through [`crate::Dom`] — decodes to
//! the canonicalized form, not to the producer's bytes.

use std::borrow::Cow;

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
	/// The envelope named a kind this revision does not replicate.
	#[error("replicated DOM event envelope used unknown kind {kind}")]
	UnknownKind {
		/// Kind tag carried by the envelope.
		kind: Str,
	},
	/// The envelope omitted a field its kind requires.
	#[error("replicated DOM event envelope omitted the {field} field")]
	MissingField {
		/// Name of the absent field.
		field: &'static str,
	},
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

/// Borrowed envelope view.
///
/// The kind tag is read as an ordinary field rather than as a serde enum tag,
/// because an internally tagged enum buffers its content before dispatching
/// and that buffer cannot carry a [`RawValue`]. Reading the envelope flat
/// keeps `data` borrowed from the input slice for every kind.
#[derive(Default, Deserialize)]
#[serde(default)]
struct Incoming<'a> {
	#[serde(borrow)]
	kind:  Option<Cow<'a, str>>,
	#[serde(borrow)]
	data:  Option<&'a RawValue>,
	cause: Option<EntryId>,
	sid:   Option<Sid>,
	op:    Option<StreamOp>,
	node:  Option<Handle>,
	prop:  Option<PropKey>,
	text:  Option<Str>,
}

fn required<T>(value: Option<T>, field: &'static str) -> Result<T, EventDecodeError> {
	value.ok_or(EventDecodeError::MissingField { field })
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
	let incoming = serde_json::from_slice::<Incoming<'_>>(bytes)?;
	Ok(match required(incoming.kind.as_deref(), "kind")? {
		"patch@1" => Event::Patch(serde_json::from_str(required(incoming.data, "data")?.get())?),
		"snapshot@1" => Event::Reset {
			snapshot: Snapshot::from_bytes(required(incoming.data, "data")?.get().as_bytes())?,
		},
		"stream@1" => Event::Stream {
			cause: required(incoming.cause, "cause")?,
			sid:   required(incoming.sid, "sid")?,
			op:    required(incoming.op, "op")?,
			node:  incoming.node,
			prop:  incoming.prop,
			text:  incoming.text,
		},
		other => return Err(EventDecodeError::UnknownKind { kind: Str::new(other) }),
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
		assert!(matches!(decode_event(b"{}"), Err(EventDecodeError::MissingField { field: "kind" })));
		assert!(matches!(
			decode_event(br#"{"kind":"patch@2"}"#),
			Err(EventDecodeError::UnknownKind { .. })
		));
		assert!(matches!(
			decode_event(br#"{"kind":"patch@1"}"#),
			Err(EventDecodeError::MissingField { field: "data" })
		));
		assert!(matches!(
			decode_event(br#"{"kind":"stream@1","sid":1,"op":"close"}"#),
			Err(EventDecodeError::MissingField { field: "cause" })
		));
		assert!(matches!(
			decode_event(br#"{"kind":"snapshot@1","data":{"high_water":1}}"#),
			Err(EventDecodeError::Envelope(_) | EventDecodeError::Snapshot(_))
		));
	}

	/// Snapshot whose canonical bytes carry a `Value::Json` property with
	/// interior whitespace and a non-compact number, so any reparse through
	/// [`serde_json::Value`] shows up as a byte difference.
	fn uncanonical() -> Snapshot {
		let mut dom = Dom::new();
		let body = dom.body();
		dom.apply(&Txn {
			cause: EntryId::default(),
			label: None,
			ops:   vec![Op::Ins {
				parent: body,
				after:  None,
				node:   NodeSpec::new(KnownTag::Turn).with_prop(
					PropId::Data,
					Value::Json(
						RawValue::from_string(r#"{ "a" : 1.50 }"#.to_owned())
							.expect("fixture is valid JSON"),
					),
				),
			}],
		})
		.expect("fixture transaction applies");
		dom.snapshot()
	}

	#[test]
	fn snapshot_envelope_embeds_canonical_bytes_verbatim() {
		let snapshot = uncanonical();
		assert!(
			snapshot
				.as_bytes()
				.windows(14)
				.any(|window| window == br#"{ "a" : 1.50 }"#),
			"fixture snapshot must itself hold the non-compact value"
		);
		let mut golden = Vec::from(br#"{"kind":"snapshot@1","data":"#.as_slice());
		golden.extend_from_slice(snapshot.as_bytes());
		golden.push(b'}');
		assert_eq!(
			encode_event(&Event::Reset { snapshot }).expect("event encodes"),
			golden,
			"snapshot@1 must wrap snapshot bytes without reserializing them"
		);
	}

	#[test]
	fn snapshot_decode_adds_no_normalization_of_its_own() {
		let snapshot = uncanonical();
		let bytes = encode_event(&Event::Reset { snapshot: snapshot.clone() }).expect("encodes");
		let Event::Reset { snapshot: decoded } = decode_event(&bytes).expect("decodes") else {
			panic!("snapshot@1 decodes as a reset");
		};
		assert_eq!(
			decoded.as_bytes(),
			Snapshot::from_bytes(snapshot.as_bytes())
				.expect("fixture snapshot validates")
				.as_bytes(),
			"decode must equal a direct validation of the producer's own bytes"
		);
	}
}
