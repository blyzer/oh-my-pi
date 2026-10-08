//! Pins the `env/v1` reflection-relay frames to their oneof tags and checks
//! that their optional fields and failure answers survive the wire.
//!
//! The approval relay holds `ServerFrame` 42 and 43 and `ClientFrame` 42 and
//! 43, so the reflection query takes `ServerFrame` 44, its withdrawal 45, and
//! the answer `ClientFrame` 44.

use omp_proto::{
	env::v1::{
		ClientFrame, ReflectionAnswer, ReflectionFailure, ReflectionQuery, ReflectionWithdrawn,
		ServerFrame, client_frame, reflection_answer, server_frame,
	},
	prost::Message,
};

/// Field 44 with wire type 2 (length-delimited) followed by a zero length.
const EMPTY_FIELD_44: [u8; 3] = [0xe2, 0x02, 0x00];
/// Field 45 with wire type 2 (length-delimited) followed by a zero length.
const EMPTY_FIELD_45: [u8; 3] = [0xea, 0x02, 0x00];

#[test]
fn reflection_frames_use_their_oneof_tags() {
	let query = ServerFrame {
		body: Some(server_frame::Body::ReflectionQuery(ReflectionQuery::default())),
		..ServerFrame::default()
	};
	assert_eq!(query.encode_to_vec(), EMPTY_FIELD_44);
	let withdrawn = ServerFrame {
		body: Some(server_frame::Body::ReflectionWithdrawn(ReflectionWithdrawn::default())),
		..ServerFrame::default()
	};
	assert_eq!(withdrawn.encode_to_vec(), EMPTY_FIELD_45);
	let answer = ClientFrame {
		body: Some(client_frame::Body::ReflectionAnswer(ReflectionAnswer::default())),
		..ClientFrame::default()
	};
	assert_eq!(answer.encode_to_vec(), EMPTY_FIELD_44);

	assert_eq!(ServerFrame::decode(EMPTY_FIELD_44.as_slice()).expect("query frame"), query);
	assert_eq!(ServerFrame::decode(EMPTY_FIELD_45.as_slice()).expect("withdrawn frame"), withdrawn);
	assert_eq!(ClientFrame::decode(EMPTY_FIELD_44.as_slice()).expect("answer frame"), answer);
}

#[test]
fn reflection_query_round_trips_with_optional_context() {
	for context in [None, Some(String::new()), Some("Preparing the release".to_owned())] {
		let frame = ServerFrame {
			request_id: 7,
			body: Some(server_frame::Body::ReflectionQuery(ReflectionQuery {
				query_id: 3,
				question: "deploy target".into(),
				context:  context.clone(),
				evidence: vec!["The deploy target is fly.io".into(), "Releases tag main".into()],
			})),
			..ServerFrame::default()
		};
		let decoded = ServerFrame::decode(frame.encode_to_vec().as_slice()).expect("query frame");
		assert_eq!(decoded, frame);
		let Some(server_frame::Body::ReflectionQuery(query)) = decoded.body else {
			panic!("expected a reflection query");
		};
		assert_eq!(query.context, context, "the context keeps its presence");
	}
}

#[test]
fn reflection_answers_round_trip_an_answer_or_a_failure() {
	let bodies = [
		reflection_answer::Body::Answer("Deploys go to fly.io.".into()),
		reflection_answer::Body::Failure(ReflectionFailure::Unavailable as i32),
		reflection_answer::Body::Failure(ReflectionFailure::Inference as i32),
	];
	for body in bodies {
		let frame = ClientFrame {
			request_id: 7,
			body: Some(client_frame::Body::ReflectionAnswer(ReflectionAnswer {
				query_id: 3,
				body:     Some(body),
			})),
			..ClientFrame::default()
		};
		let decoded = ClientFrame::decode(frame.encode_to_vec().as_slice()).expect("answer frame");
		assert_eq!(decoded, frame);
		assert!(decoded.scope.is_none(), "an answer carries no invocation scope");
	}
}
