//! TSP framing: `ESC _ tsp ; <verb> [; key=value]* ; <body> ESC \`.
//!
//! Bodies are UTF-8 JSON (which escapes every control character, so a body
//! never holds ESC or BEL) or base64 for blobs. A body over the negotiated
//! `apc` limit is split across messages of the same verb tagged
//! `c=<chunk-id>`, with `m=1` on every chunk but the last; the terminal joins
//! the bodies byte for byte. Cuts land on UTF-8 boundaries and never where the
//! next chunk would lead with a `key=value;` segment, which the receiver
//! would read as one more parameter (Tern's `tsp_chunks` rule).

use std::mem;

use bytes::{BufMut, BytesMut};
use omp_core::{FastHashMap, Hash32, Str, base64, hex};
use serde::Serialize;
use smallvec::SmallVec;

use super::{
	DEFAULT_APC_LIMIT, MAX_JOINED_BYTES,
	wire::{Event, Reply, Verb},
};

const APC: &[u8] = b"\x1b_";
const ST: &[u8] = b"\x1b\\";
/// The APC payload prefix every TSP message starts with.
pub const PREFIX: &[u8] = b"tsp;";
/// The OSC payload prefix of terminal → program messages through a Windows
/// `ConPTY` (`ESC ] 877 ; tsp ; ...`).
pub const OSC_PREFIX: &[u8] = b"877;tsp;";

/// Whether `bytes[at..]` leads with a `key=value;` segment.
fn leads_with_parameter(bytes: &[u8], at: usize) -> bool {
	let key = bytes[at..]
		.iter()
		.take_while(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
		.count();
	if key == 0 || bytes.get(at + key) != Some(&b'=') {
		return false;
	}
	let value_start = at + key + 1;
	let value = bytes[value_start..]
		.iter()
		.take_while(|byte| (0x21..=0x7e).contains(*byte) && **byte != b';')
		.count();
	bytes.get(value_start + value) == Some(&b';')
}

/// Whether a chunk may end at `at`: a UTF-8 boundary where the rest does not
/// lead with a parameter-shaped segment.
fn safe_cut(bytes: &[u8], at: usize) -> bool {
	bytes[at] & 0xc0 != 0x80 && !leads_with_parameter(bytes, at)
}

/// Where the chunk of `bytes` starting at `start` ends: the safe cut nearest
/// below `start + limit` (above `start`), else the nearest above it, else
/// the end.
pub(crate) fn chunk_end(bytes: &[u8], start: usize, limit: usize) -> usize {
	let target = start + limit.max(1);
	if target >= bytes.len() {
		return bytes.len();
	}
	(start + 1..=target)
		.rev()
		.find(|at| safe_cut(bytes, *at))
		.or_else(|| (target + 1..bytes.len()).find(|at| safe_cut(bytes, *at)))
		.unwrap_or(bytes.len())
}

/// Writes TSP messages into an output buffer, chunking bodies over the
/// negotiated limit. One encoder per connection: chunk ids are unique
/// within it.
#[derive(Debug)]
pub struct Encoder {
	limit:      usize,
	next_chunk: u64,
	body:       Vec<u8>,
}

impl Default for Encoder {
	fn default() -> Self {
		Self::new(DEFAULT_APC_LIMIT)
	}
}

impl Encoder {
	/// An encoder chunking bodies over `limit` bytes.
	#[must_use]
	pub const fn new(limit: usize) -> Self {
		Self { limit, next_chunk: 1, body: Vec::new() }
	}

	/// Adopts the limit a `hello` reply announced.
	pub const fn set_limit(&mut self, limit: usize) {
		self.limit = limit;
	}

	/// Appends one logical message to `out`: one APC string, or its chunks in
	/// order, the first carrying `params`.
	pub fn message(&mut self, verb: Verb, params: &[(&str, &str)], body: &[u8], out: &mut BytesMut) {
		let verb: &'static str = verb.into();
		if body.len() <= self.limit {
			frame(out, verb, params, None, false, body);
			return;
		}
		let id = self.next_chunk;
		self.next_chunk += 1;
		let chunk = hex::encode_n(&id.to_be_bytes());
		let mut start = 0;
		while start < body.len() {
			let end = chunk_end(body, start, self.limit);
			let first = start == 0;
			frame(
				out,
				verb,
				if first { params } else { &[] },
				Some(chunk.as_str()),
				end < body.len(),
				&body[start..end],
			);
			start = end;
		}
	}

	/// Serializes `value` as the JSON body of one message.
	///
	/// # Errors
	/// Returns the serializer's error; nothing is written then.
	pub fn json<T: Serialize + ?Sized>(
		&mut self,
		verb: Verb,
		value: &T,
		out: &mut BytesMut,
	) -> serde_json::Result<()> {
		let mut body = mem::take(&mut self.body);
		body.clear();
		let written = serde_json::to_writer(&mut body, value);
		if written.is_ok() {
			self.message(verb, &[], &body, out);
		}
		self.body = body;
		written
	}

	/// Appends a blob message for `bytes` and returns its id, the lowercase
	/// hex SHA-256 that `image` nodes name.
	pub fn blob(&mut self, bytes: &[u8], mime: Option<&str>, out: &mut BytesMut) -> Hash32 {
		let digest = Hash32::sum(bytes);
		let hex = digest.to_hex();
		let mut body = mem::take(&mut self.body);
		body.clear();
		base64::encode(bytes).extend_into(&mut body);
		let mut params: SmallVec<(&str, &str), 2> = SmallVec::new();
		params.push(("id", hex.as_str()));
		if let Some(mime) = mime {
			params.push(("mime", mime));
		}
		self.message(Verb::Blob, &params, &body, out);
		self.body = body;
		digest
	}
}

fn frame(
	out: &mut BytesMut,
	verb: &str,
	params: &[(&str, &str)],
	chunk: Option<&str>,
	more: bool,
	body: &[u8],
) {
	out.reserve(APC.len() + PREFIX.len() + verb.len() + body.len() + ST.len() + 32);
	out.put_slice(APC);
	out.put_slice(PREFIX);
	out.put_slice(verb.as_bytes());
	for (key, value) in params {
		out.put_u8(b';');
		out.put_slice(key.as_bytes());
		out.put_u8(b'=');
		out.put_slice(value.as_bytes());
	}
	if let Some(chunk) = chunk {
		out.put_slice(b";c=");
		out.put_slice(chunk.as_bytes());
		if more {
			out.put_slice(b";m=1");
		}
	}
	out.put_u8(b';');
	out.put_slice(body);
	out.put_slice(ST);
}

/// One message split into verb, parameters and body. Borrowed from the
/// payload: nothing is copied.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Raw<'a> {
	/// The verb letter(s).
	pub verb:   &'a str,
	/// `key=value` parameters, in order.
	pub params: SmallVec<(&'a str, &'a str), 4>,
	/// The body bytes.
	pub body:   &'a [u8],
}

impl Raw<'_> {
	/// The value of parameter `key`.
	#[must_use]
	pub fn param(&self, key: &str) -> Option<&str> {
		self
			.params
			.iter()
			.find_map(|(name, value)| (*name == key).then_some(*value))
	}
}

fn parameter(segment: &[u8]) -> Option<(&str, &str)> {
	let eq = segment.iter().position(|byte| *byte == b'=')?;
	let (key, value) = (&segment[..eq], &segment[eq + 1..]);
	let key_ok = !key.is_empty()
		&& key
			.iter()
			.all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'));
	let value_ok = value
		.iter()
		.all(|byte| (0x21..=0x7e).contains(byte) && *byte != b';');
	(key_ok && value_ok)
		.then(|| Some((str::from_utf8(key).ok()?, str::from_utf8(value).ok()?)))
		.flatten()
}

/// Splits a message payload: the bytes between `ESC _` and `ESC \` (`tsp;...`),
/// or an OSC 877 payload (`877;tsp;...`). `None` when it is not TSP.
#[must_use]
pub fn split(payload: &[u8]) -> Option<Raw<'_>> {
	let inner = payload
		.strip_prefix(PREFIX)
		.or_else(|| payload.strip_prefix(OSC_PREFIX))?;
	let mut segments = inner.split(|byte| *byte == b';');
	let verb = segments.next().filter(|verb| !verb.is_empty())?;
	let verb = str::from_utf8(verb).ok()?;
	let mut params = SmallVec::new();
	let mut at = verb.len() + 1;
	// A segment is a parameter only when another `;` follows it; the first
	// segment that is not one starts the body.
	while at < inner.len() {
		let Some(next) = inner[at..].iter().position(|byte| *byte == b';') else {
			break;
		};
		let Some(param) = parameter(&inner[at..at + next]) else {
			break;
		};
		params.push(param);
		at += next + 1;
	}
	Some(Raw { verb, params, body: inner.get(at..).unwrap_or_default() })
}

/// A decoded terminal → program message.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Incoming {
	/// A reply to a query.
	Reply(Reply),
	/// An event.
	Event(Event),
}

/// Reassembles chunked terminal → program messages and decodes replies and
/// events. Tern never chunks what it sends, so this is defensive; a chunk
/// sequence over [`MAX_JOINED_BYTES`] is dropped.
#[derive(Debug, Default)]
pub struct Reader {
	chunks: FastHashMap<Str, Vec<u8>>,
}

impl Reader {
	/// A reader with nothing pending.
	#[must_use]
	pub fn new() -> Self {
		Self::default()
	}

	/// Feeds one complete payload (APC or OSC 877). Returns the decoded
	/// message once complete; `None` for a pending chunk, a program →
	/// terminal verb, or malformed input.
	pub fn feed(&mut self, payload: &[u8]) -> Option<Incoming> {
		let raw = split(payload)?;
		let Some(chunk) = raw.param("c") else {
			return decode(raw.verb, raw.body);
		};
		let joined = self.chunks.entry(Str::new(chunk)).or_default();
		joined.extend_from_slice(raw.body);
		if joined.len() > MAX_JOINED_BYTES {
			self.chunks.remove(chunk);
			return None;
		}
		if raw.param("m") == Some("1") {
			return None;
		}
		let body = self.chunks.remove(chunk)?;
		decode(raw.verb, &body)
	}
}

/// Decodes an unchunked reply or event body.
#[must_use]
pub fn decode(verb: &str, body: &[u8]) -> Option<Incoming> {
	match verb.parse::<Verb>().ok()? {
		Verb::Reply => serde_json::from_slice(body).ok().map(Incoming::Reply),
		Verb::Event => serde_json::from_slice(body).ok().map(Incoming::Event),
		_ => None,
	}
}

#[cfg(test)]
mod tests {
	use bytes::BytesMut;
	use proptest::prelude::*;

	use super::{Encoder, Incoming, Reader, chunk_end, leads_with_parameter, split};
	use crate::tsp::wire::{Event, Verb};

	/// Splits a written stream into the APC payloads it holds.
	fn payloads(out: &[u8]) -> Vec<&[u8]> {
		let mut found = Vec::new();
		let mut rest = out;
		while let Some(start) = rest.windows(2).position(|window| window == b"\x1b_") {
			let after = &rest[start + 2..];
			let end = after
				.windows(2)
				.position(|window| window == b"\x1b\\")
				.expect("terminated message");
			found.push(&after[..end]);
			rest = &after[end + 2..];
		}
		found
	}

	#[test]
	fn a_small_message_is_one_unchunked_string() {
		let mut encoder = Encoder::default();
		let mut out = BytesMut::new();
		encoder
			.json(Verb::Close, &serde_json::json!({ "id": "s1", "keep": true }), &mut out)
			.expect("serialize");
		assert_eq!(&out[..], b"\x1b_tsp;x;{\"id\":\"s1\",\"keep\":true}\x1b\\");
	}

	#[test]
	fn parameters_lead_and_the_body_keeps_its_semicolons() {
		let raw = split(b"tsp;b;id=ab12;mime=image/png;a=b;c").expect("tsp");
		assert_eq!(raw.verb, "b");
		assert_eq!(raw.params.as_slice(), [("id", "ab12"), ("mime", "image/png"), ("a", "b")]);
		assert_eq!(raw.body, b"c");
		let json = split(b"tsp;f;{\"a\":\"x=1;y\"}").expect("tsp");
		assert!(json.params.is_empty());
		assert_eq!(json.body, b"{\"a\":\"x=1;y\"}");
		assert!(split(b"Gi=1;OK").is_none(), "kitty graphics is not TSP");
		assert_eq!(split(b"877;tsp;e;{}").expect("osc 877").verb, "e");
	}

	#[test]
	fn a_cut_never_leaves_a_parameter_shaped_prefix() {
		let body = b"aa;;x=1;yyyy";
		assert!(leads_with_parameter(body, 4));
		// A limit of 4 would cut right before `x=1;`: the cut moves back to
		// the nearest safe byte.
		assert_eq!(chunk_end(body, 0, 4), 3);
		// When every byte below the target leads with one (key characters up
		// to the `=`), the cut moves forward instead.
		let body = b"aaaax=1;yyyy";
		assert_eq!(chunk_end(body, 0, 4), 5);
		assert!(!leads_with_parameter(body, 5));
	}

	#[test]
	fn events_decode_and_unknown_ones_are_tolerated() {
		let mut reader = Reader::new();
		assert_eq!(
			reader.feed(b"tsp;e;{\"ev\":\"ack\",\"sf\":\"s1\",\"s\":3,\"future\":1}"),
			Some(Incoming::Event(Event::Ack { sf: "s1".into(), s: 3 }))
		);
		assert_eq!(
			reader.feed(b"tsp;e;{\"ev\":\"teleport\"}"),
			Some(Incoming::Event(Event::Unknown))
		);
		assert_eq!(reader.feed(b"tsp;e;{not json"), None);
		assert_eq!(reader.feed(b"tsp;f;{}"), None, "program -> terminal verbs are not input");
	}

	proptest! {
		#[test]
		fn chunked_bodies_rejoin_byte_for_byte(
			text in "[a-z0-9=;é漢🙂 \"{}]{0,400}",
			limit in 1usize..64,
		) {
			let mut encoder = Encoder::new(limit);
			let mut out = BytesMut::new();
			let body = serde_json::to_vec(&serde_json::json!({ "ev": "send", "sf": "s", "id": "e", "text": text })).expect("json");
			encoder.message(Verb::Event, &[], &body, &mut out);
			let mut joined = Vec::new();
			let parts = payloads(&out);
			for (index, payload) in parts.iter().enumerate() {
				let raw = split(payload).expect("tsp payload");
				prop_assert!(str::from_utf8(raw.body).is_ok(), "every chunk is valid UTF-8");
				if parts.len() > 1 {
					prop_assert!(raw.param("c").is_some());
					prop_assert_eq!(raw.param("m"), (index + 1 < parts.len()).then_some("1"));
				}
				joined.extend_from_slice(raw.body);
			}
			prop_assert_eq!(&joined, &body);
			let mut reader = Reader::new();
			let decoded = parts.iter().filter_map(|payload| reader.feed(payload)).collect::<Vec<_>>();
			prop_assert_eq!(decoded.len(), 1);
		}
	}
}
