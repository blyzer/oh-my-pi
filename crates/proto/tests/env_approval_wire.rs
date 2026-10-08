//! Pins the `env/v1` approval-relay frames to their oneof tags and checks that
//! their optional fields keep presence across the wire.
//!
//! `ServerFrame` tags 40 and 41 are reserved for the removed ACP terminal
//! execution, so the relay takes 42 and 43; `ClientFrame` 41 is reserved too,
//! so the answer takes 42 and the grant revocation 43.

use omp_proto::{
	env::v1::{
		ApprovalAnswer, ApprovalDecision, ApprovalQuery, ApprovalSpec, ApprovalWithdrawn,
		ClientFrame, RevokeApprovalGrants, ServerFrame, client_frame, server_frame,
	},
	prost::Message,
};

/// Field 42 with wire type 2 (length-delimited) followed by a zero length.
const EMPTY_FIELD_42: [u8; 3] = [0xd2, 0x02, 0x00];
/// Field 43 with wire type 2 (length-delimited) followed by a zero length.
const EMPTY_FIELD_43: [u8; 3] = [0xda, 0x02, 0x00];

#[test]
fn approval_frames_use_their_oneof_tags() {
	let query = ServerFrame {
		body: Some(server_frame::Body::ApprovalQuery(ApprovalQuery::default())),
		..ServerFrame::default()
	};
	assert_eq!(query.encode_to_vec(), EMPTY_FIELD_42);
	let withdrawn = ServerFrame {
		body: Some(server_frame::Body::ApprovalWithdrawn(ApprovalWithdrawn::default())),
		..ServerFrame::default()
	};
	assert_eq!(withdrawn.encode_to_vec(), EMPTY_FIELD_43);
	let answer = ClientFrame {
		body: Some(client_frame::Body::ApprovalAnswer(ApprovalAnswer::default())),
		..ClientFrame::default()
	};
	assert_eq!(answer.encode_to_vec(), EMPTY_FIELD_42);
	let revoke = ClientFrame {
		body: Some(client_frame::Body::RevokeApprovalGrants(RevokeApprovalGrants {})),
		..ClientFrame::default()
	};
	assert_eq!(revoke.encode_to_vec(), EMPTY_FIELD_43);

	assert_eq!(ServerFrame::decode(EMPTY_FIELD_42.as_slice()).expect("query frame"), query);
	assert_eq!(ServerFrame::decode(EMPTY_FIELD_43.as_slice()).expect("withdrawn frame"), withdrawn);
	assert_eq!(ClientFrame::decode(EMPTY_FIELD_42.as_slice()).expect("answer frame"), answer);
	assert_eq!(ClientFrame::decode(EMPTY_FIELD_43.as_slice()).expect("revoke frame"), revoke);
}

#[test]
fn approval_query_round_trips_with_optional_presence() {
	let amendment = ApprovalSpec {
		title:           "Approve scoped sandbox amendment".into(),
		body:            "write outside the workspace".into(),
		subject:         "echo x > .git/a".into(),
		kind:            "sandbox_amendment".into(),
		scopes:          vec!["once".into()],
		timeout_default: Some(false),
		route:           "user".into(),
		approver:        None,
		timeout_ms:      120_000,
		unreachable:     "fail_closed".into(),
		require_human:   true,
		pattern:         Some("echo x > .git/a".into()),
		evidence:        vec!["write: .git/a".into()],
	};
	let frame = ServerFrame {
		request_id: 7,
		body: Some(server_frame::Body::ApprovalQuery(ApprovalQuery {
			query_id:      3,
			invocation_id: None,
			reasons:       vec![amendment, invocation_tool_spec()],
			created_at_ms: 1_700_000_000_000,
		})),
		..ServerFrame::default()
	};
	let decoded = ServerFrame::decode(frame.encode_to_vec().as_slice()).expect("query frame");
	assert_eq!(decoded, frame);
	let Some(server_frame::Body::ApprovalQuery(query)) = decoded.body else {
		panic!("expected an approval query");
	};
	assert_eq!(query.invocation_id, None, "an absent invocation id must stay absent");
	assert_eq!(
		query.reasons[0].timeout_default,
		Some(false),
		"a false default must keep its presence"
	);
	assert_eq!(query.reasons[1].timeout_default, None);
}

#[test]
fn approval_answer_round_trips_its_decision() {
	let frame = ClientFrame {
		request_id: 7,
		body: Some(client_frame::Body::ApprovalAnswer(ApprovalAnswer {
			query_id: 3,
			decision: Some(ApprovalDecision {
				approved:   true,
				scope:      "once".into(),
				source:     "user".into(),
				decided_by: Some("operator".into()),
				reason:     None,
				audited:    false,
			}),
		})),
		..ClientFrame::default()
	};
	let decoded = ClientFrame::decode(frame.encode_to_vec().as_slice()).expect("answer frame");
	assert_eq!(decoded, frame);
	assert!(decoded.scope.is_none());
}

fn invocation_tool_spec() -> ApprovalSpec {
	ApprovalSpec {
		title: "Approve bash".into(),
		subject: "cargo test".into(),
		kind: "exec".into(),
		scopes: vec!["once".into(), "session".into()],
		..ApprovalSpec::default()
	}
}
