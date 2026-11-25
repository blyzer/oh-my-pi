//! Verifies `HttpRequest.credential` is additive (ADR 0075 D2, D11).
//!
//! The field round-trips, a request written before it existed decodes with the
//! field absent, and a receiver that predates it skips it as an unknown field,
//! which is why a sender must check the handshake capability first.

use omp_proto::{
	env::v1::{HttpRequest, ProtocolErrorCode},
	prost::{self, Message},
};

#[test]
fn credential_round_trips_and_old_requests_decode_unchanged() {
	let placed = HttpRequest {
		method: "POST".into(),
		url: "https://example.test/run".into(),
		credential: Some("p".into()),
		..Default::default()
	};
	let decoded = HttpRequest::decode(placed.encode_to_vec().as_slice()).expect("decode placed");
	assert_eq!(decoded, placed);

	// method = "GET" (field 1), written before `credential` existed.
	let legacy = [0x0a, 0x03, b'G', b'E', b'T'];
	let old = HttpRequest::decode(legacy.as_slice()).expect("legacy request decodes");
	assert_eq!(old, HttpRequest { method: "GET".into(), ..Default::default() });
	assert_eq!(old.credential, None);
	assert_eq!(old.encode_to_vec(), legacy);
}

#[test]
fn a_receiver_without_the_field_skips_it() {
	#[derive(Clone, PartialEq, prost::Message)]
	struct BeforeCredential {
		#[prost(string, tag = "1")]
		method: String,
		#[prost(string, tag = "2")]
		url:    String,
	}
	let placed = HttpRequest {
		method: "POST".into(),
		url: "https://example.test/run".into(),
		credential: Some("p".into()),
		..Default::default()
	};
	let old = BeforeCredential::decode(placed.encode_to_vec().as_slice()).expect("old decode");
	assert_eq!((old.method.as_str(), old.url.as_str()), ("POST", "https://example.test/run"));
}

#[test]
fn placement_unavailable_is_a_distinct_protocol_code() {
	assert_eq!(ProtocolErrorCode::PlacementUnavailable as i32, 14);
	assert_eq!(ProtocolErrorCode::try_from(14), Ok(ProtocolErrorCode::PlacementUnavailable));
}
