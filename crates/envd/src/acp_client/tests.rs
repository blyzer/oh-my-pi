//! Tests for the ACP wire transport.
//!
//! All tests use in-memory duplex pipes (`tokio::io::duplex`).  No real agent
//! process is spawned, no network is touched.
//!
//! Test coverage:
//! - Framing: split lines, oversize, truncated, malformed JSON.
//! - ID correlation: out-of-order responses, unknown id dropped, pending count
//!   tracking.
//! - Notification vs request discrimination.
//! - Agent-to-client request round trip (`session/request_permission`).
//! - Cancel-safety of the read future.

use std::time::Duration;

use tokio::io::{AsyncWriteExt as _, DuplexStream, duplex};

use super::{
	AcpSession, InboundMessage, MAX_FRAME_BYTES,
	wire::{
		CancelParams, Method, PermissionOption, PermissionOptionKind, PermissionOutcome, RequestId,
		RequestPermissionParams, RequestPermissionResponse, StopReason, UpdateParams,
	},
};
use crate::line_reader::{FrameError, LineReader};

// ─── helpers ─────────────────────────────────────────────────────────────────

/// Construct a session paired with a full mock agent (read + write).
fn make_pair(cap: usize) -> (DuplexStream, AcpSession<DuplexStream>, DuplexStream) {
	let (agent_to_client_read, agent_to_client_write) = duplex(256 * 1024);
	let (client_to_agent_read, client_to_agent_write) = duplex(256 * 1024);
	let session = AcpSession::new(agent_to_client_read, client_to_agent_write, cap);
	// Return: (client_to_agent_read, session, agent_to_client_write)
	// Tests destructure as (_, mut session, mut agent_tx) or (mut read, mut
	// session, mut agent_tx)
	(client_to_agent_read, session, agent_to_client_write)
}

async fn recv_line(stream: &mut DuplexStream) -> String {
	let mut buf = Vec::new();
	loop {
		let mut byte = [0u8];
		tokio::io::AsyncReadExt::read_exact(stream, &mut byte)
			.await
			.expect("read");
		if byte[0] == b'\n' {
			break;
		}
		buf.push(byte[0]);
	}
	String::from_utf8(buf).expect("utf8")
}

// ─── framing tests (LineReader)
// ───────────────────────────────────────────────

#[tokio::test]
async fn line_reader_splits_lines_and_skips_blank_ones() {
	let (mut tx, rx) = duplex(64 * 1024);
	tx.write_all(b"one\n\n two \r\nthree\n")
		.await
		.expect("write");
	drop(tx);
	let mut reader = LineReader::new(rx, 64);
	assert_eq!(reader.next_line().await.expect("line").as_deref(), Some(&b"one"[..]));
	assert_eq!(reader.next_line().await.expect("line").as_deref(), Some(&b" two "[..]));
	assert_eq!(reader.next_line().await.expect("line").as_deref(), Some(&b"three"[..]));
	assert!(reader.next_line().await.expect("eof").is_none());
}

#[tokio::test]
async fn line_reader_oversize_rejected() {
	let (mut tx, rx) = duplex(64 * 1024);
	tx.write_all(b"12345\n").await.expect("write"); // exactly at cap
	drop(tx);
	let mut reader = LineReader::new(rx, 5);
	assert_eq!(reader.next_line().await.expect("line").as_deref(), Some(&b"12345"[..]));

	let (mut tx2, rx2) = duplex(64 * 1024);
	tx2.write_all(b"123456\n").await.expect("write"); // one byte over
	drop(tx2);
	let mut reader2 = LineReader::new(rx2, 5);
	assert!(matches!(reader2.next_line().await, Err(FrameError::Oversize { limit: 5 })));
}

#[tokio::test]
async fn line_reader_truncated_stream() {
	let (mut tx, rx) = duplex(64 * 1024);
	tx.write_all(b"whole\nincomplete").await.expect("write");
	drop(tx);
	let mut reader = LineReader::new(rx, 64);
	assert!(reader.next_line().await.expect("first").is_some());
	assert!(matches!(reader.next_line().await, Err(FrameError::Truncated)));
}

#[tokio::test]
async fn line_reader_flood_does_not_buffer_all() {
	let (mut tx, rx) = duplex(2 * 1024 * 1024);
	tx.write_all(&vec![b'x'; 200_000]).await.expect("write");
	let mut reader = LineReader::new(rx, 1000);
	assert!(matches!(reader.next_line().await, Err(FrameError::Oversize { limit: 1000 })));
	// Check buf did not grow unboundedly — it's pub(crate) in line_reader.
}

#[tokio::test]
async fn line_reader_cancel_safe() {
	let (mut tx, rx) = duplex(1024);
	let mut reader = LineReader::new(rx, 64);
	// Write half a line; the future must suspend.
	tx.write_all(b"hel").await.expect("write");
	let timeout = tokio::time::timeout(Duration::from_millis(20), reader.next_line()).await;
	assert!(timeout.is_err(), "should suspend waiting for newline");
	// Complete the line; the reader must now return it.
	tx.write_all(b"lo\n").await.expect("write");
	let line = reader.next_line().await.expect("line").expect("some");
	assert_eq!(&line[..], b"hello");
}

// ─── malformed JSON tests
// ─────────────────────────────────────────────────────

#[tokio::test]
async fn malformed_json_surfaces_as_malformed() {
	let (_, mut session, mut agent_tx) = make_pair(MAX_FRAME_BYTES);
	agent_tx
		.write_all(b"not json at all\n")
		.await
		.expect("write");
	let msg = session.next_message().await.expect("ok").expect("some");
	assert!(matches!(msg, InboundMessage::Malformed { .. }));
}

#[tokio::test]
async fn valid_json_but_not_jsonrpc_surfaces_as_malformed() {
	let (_, mut session, mut agent_tx) = make_pair(MAX_FRAME_BYTES);
	agent_tx.write_all(b"{\"foo\":1}\n").await.expect("write");
	let msg = session.next_message().await.expect("ok").expect("some");
	assert!(matches!(msg, InboundMessage::Malformed { .. }));
}

// ─── id correlation tests
// ─────────────────────────────────────────────────────

#[tokio::test]
async fn response_matched_by_id_and_pending_cleared() {
	let (_, mut session, mut agent_tx) = make_pair(MAX_FRAME_BYTES);
	let id = session
		.send_request("initialize", super::wire::InitializeParams {
			protocol_version:    super::wire::PROTOCOL_VERSION,
			client_info:         None,
			client_capabilities: super::wire::ClientCapabilities {},
		})
		.expect("send");
	assert_eq!(session.pending_count(), 1);

	let reply = format!(r#"{{"jsonrpc":"2.0","id":{id},"result":{{"protocolVersion":1}}}}"#);
	agent_tx.write_all(reply.as_bytes()).await.expect("write");
	agent_tx.write_all(b"\n").await.expect("write");

	let msg = session.next_message().await.expect("ok").expect("some");
	assert!(matches!(msg, InboundMessage::Response { .. }), "should be Response");
	assert_eq!(session.pending_count(), 0);
}

#[tokio::test]
async fn out_of_order_responses_matched_correctly() {
	let (_, mut session, mut agent_tx) = make_pair(MAX_FRAME_BYTES);
	let new_params = super::wire::NewSessionParams { cwd: "/", mcp_servers: &[] };
	let id1 = session
		.send_request("session/new", &new_params)
		.expect("s1");
	let new_params2 = super::wire::NewSessionParams { cwd: "/", mcp_servers: &[] };
	let id2 = session
		.send_request("session/new", &new_params2)
		.expect("s2");
	assert_eq!(session.pending_count(), 2);

	// Reply to id2 first.
	let r2 = format!(r#"{{"jsonrpc":"2.0","id":{id2},"result":{{"sessionId":"s2"}}}}"#);
	agent_tx.write_all(r2.as_bytes()).await.expect("write");
	agent_tx.write_all(b"\n").await.expect("write");
	let msg2 = session.next_message().await.expect("ok").expect("some");
	let matched_id2 = match msg2 {
		InboundMessage::Response { id: RequestId::Int(i), .. } => i,
		_ => panic!("expected Response for id2"),
	};
	assert_eq!(matched_id2, id2);
	assert_eq!(session.pending_count(), 1);

	// Then reply to id1.
	let r1 = format!(r#"{{"jsonrpc":"2.0","id":{id1},"result":{{"sessionId":"s1"}}}}"#);
	agent_tx.write_all(r1.as_bytes()).await.expect("write");
	agent_tx.write_all(b"\n").await.expect("write");
	let msg1 = session.next_message().await.expect("ok").expect("some");
	let matched_id1 = match msg1 {
		InboundMessage::Response { id: RequestId::Int(i), .. } => i,
		_ => panic!("expected Response for id1"),
	};
	assert_eq!(matched_id1, id1);
	assert_eq!(session.pending_count(), 0);
}

#[tokio::test]
async fn unknown_id_in_response_is_dropped_and_next_read() {
	let (_, mut session, mut agent_tx) = make_pair(MAX_FRAME_BYTES);
	// Do not send any request, so id=99 is unknown.
	agent_tx
		.write_all(b"{\"jsonrpc\":\"2.0\",\"id\":99,\"result\":{}}\n")
		.await
		.expect("w");
	// Then send a real notification.
	agent_tx
		.write_all(
			b"{\"jsonrpc\":\"2.0\",\"method\":\"session/update\",\"params\":{\"sessionId\":\"x\",\"update\":{\"sessionUpdate\":\"notice\",\"severity\":\"info\",\"title\":\"hi\"}}}\n",
		)
		.await
		.expect("w");

	let msg = session.next_message().await.expect("ok").expect("some");
	// The unknown-id response was dropped; we get the notification.
	assert!(
		matches!(msg, InboundMessage::Notification { method: Method::SessionUpdate, .. }),
		"expected Notification, got something else"
	);
}

// ─── notification vs request discrimination
// ───────────────────────────────────

#[tokio::test]
async fn session_update_is_notification() {
	let (_, mut session, mut agent_tx) = make_pair(MAX_FRAME_BYTES);
	agent_tx
		.write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"session/update\",\"params\":{\"sessionId\":\"s1\",\"update\":{\"sessionUpdate\":\"agent_message_chunk\",\"content\":{\"type\":\"text\",\"text\":\"hi\"}}}}\n")
		.await
		.expect("write");
	let msg = session.next_message().await.expect("ok").expect("some");
	assert!(
		matches!(msg, InboundMessage::Notification { method: Method::SessionUpdate, .. }),
		"expected SessionUpdate notification"
	);
}

#[tokio::test]
async fn session_request_permission_is_agent_request() {
	let (_, mut session, mut agent_tx) = make_pair(MAX_FRAME_BYTES);
	let req = r#"{"jsonrpc":"2.0","id":7,"method":"session/request_permission","params":{"sessionId":"s1","toolCall":{"toolCallId":"tc1","title":"run ls"},"options":[{"optionId":"allow_once","name":"Allow once","kind":"allow_once"}]}}"#;
	agent_tx.write_all(req.as_bytes()).await.expect("write");
	agent_tx.write_all(b"\n").await.expect("write");
	let msg = session.next_message().await.expect("ok").expect("some");
	assert!(
		matches!(msg, InboundMessage::AgentRequest { method: Method::SessionRequestPermission, .. }),
		"expected AgentRequest/SessionRequestPermission"
	);
}

#[tokio::test]
async fn unknown_method_notification_is_notification_with_unknown_method() {
	let (_, mut session, mut agent_tx) = make_pair(MAX_FRAME_BYTES);
	agent_tx
		.write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"future/exotic\",\"params\":{}}\n")
		.await
		.expect("write");
	let msg = session.next_message().await.expect("ok").expect("some");
	assert!(
		matches!(msg, InboundMessage::Notification { method: Method::Unknown, .. }),
		"expected Unknown notification"
	);
}

// ─── agent-to-client request round trip ──────────────────────────────────────

#[tokio::test]
async fn permission_request_round_trip_selected() {
	let (mut client_to_agent_read, mut session, mut agent_tx) = make_pair(MAX_FRAME_BYTES);

	// Agent sends session/request_permission.
	let req = r#"{"jsonrpc":"2.0","id":3,"method":"session/request_permission","params":{"sessionId":"sess1","toolCall":{"toolCallId":"call1","title":"run rm -rf"},"options":[{"optionId":"allow_once","name":"Allow","kind":"allow_once"},{"optionId":"reject_once","name":"Reject","kind":"reject_once"}]}}"#;
	agent_tx.write_all(req.as_bytes()).await.expect("write");
	agent_tx.write_all(b"\n").await.expect("write");

	// Client reads it.
	let msg = session.next_message().await.expect("ok").expect("some");
	let (req_id, params_raw) = match msg {
		InboundMessage::AgentRequest { id, method: Method::SessionRequestPermission, params } => {
			(id, params.expect("params"))
		},
		_ => panic!("expected permission request"),
	};

	// Decode and inspect the params.
	let params: RequestPermissionParams =
		serde_json::from_str(params_raw.get()).expect("deserialize params");
	assert_eq!(params.session_id.as_str(), "sess1");
	assert_eq!(params.tool_call.tool_call_id.as_str(), "call1");
	assert_eq!(params.options.len(), 2);
	assert_eq!(params.options[0].kind, PermissionOptionKind::AllowOnce);

	// Client answers with "selected: allow_once".
	let response = RequestPermissionResponse {
		outcome: PermissionOutcome::Selected { option_id: omp_core::Str::new_static("allow_once") },
	};
	session.answer_request(&req_id, response).expect("answer");

	// Agent reads the response.
	let line = recv_line(&mut client_to_agent_read).await;
	assert!(line.contains("\"outcome\":\"selected\""), "response: {line}");
	assert!(line.contains("\"optionId\":\"allow_once\""), "response: {line}");
	assert_eq!(req_id, RequestId::Int(3));
}

#[tokio::test]
async fn permission_request_round_trip_cancelled() {
	let (mut client_to_agent_read, mut session, mut agent_tx) = make_pair(MAX_FRAME_BYTES);

	let req = r#"{"jsonrpc":"2.0","id":8,"method":"session/request_permission","params":{"sessionId":"s","toolCall":{"toolCallId":"t1","title":"read /etc/passwd"},"options":[{"optionId":"allow","name":"Allow","kind":"allow_once"}]}}"#;
	agent_tx.write_all(req.as_bytes()).await.expect("write");
	agent_tx.write_all(b"\n").await.expect("write");

	let msg = session.next_message().await.expect("ok").expect("some");
	let req_id = match msg {
		InboundMessage::AgentRequest { id, method: Method::SessionRequestPermission, .. } => id,
		_ => panic!("expected permission request"),
	};

	let response = RequestPermissionResponse { outcome: PermissionOutcome::Cancelled };
	session.answer_request(&req_id, response).expect("answer");

	let line = recv_line(&mut client_to_agent_read).await;
	assert!(line.contains("\"outcome\":\"cancelled\""), "response: {line}");
}

// ─── cancel-safety of the read future ────────────────────────────────────────

#[tokio::test]
async fn cancel_safe_partial_line_retained_across_drop() {
	let (_, mut session, mut agent_tx) = make_pair(MAX_FRAME_BYTES);

	// Write half a JSON line; the reader must park, not consume or drop bytes.
	agent_tx
		.write_all(b"{\"jsonrpc\":\"2.0\",\"meth")
		.await
		.expect("write");

	let timeout = tokio::time::timeout(Duration::from_millis(20), session.next_message()).await;
	assert!(timeout.is_err(), "should have suspended");

	// Complete the line.
	agent_tx
		.write_all(b"od\":\"session/update\",\"params\":{\"sessionId\":\"s\",\"update\":{\"sessionUpdate\":\"notice\",\"severity\":\"info\",\"title\":\"t\"}}}\n")
		.await
		.expect("write");

	let msg = session.next_message().await.expect("ok").expect("some");
	assert!(
		matches!(msg, InboundMessage::Notification { method: Method::SessionUpdate, .. }),
		"expected update notification after resumed read"
	);
}

// ─── wire struct decode tests
// ─────────────────────────────────────────────────

#[test]
fn stop_reason_deserialization() {
	let r: super::wire::PromptResult =
		serde_json::from_str(r#"{"stopReason":"end_turn"}"#).expect("deser");
	assert_eq!(r.stop_reason, StopReason::EndTurn);

	let r: super::wire::PromptResult =
		serde_json::from_str(r#"{"stopReason":"cancelled"}"#).expect("deser");
	assert_eq!(r.stop_reason, StopReason::Cancelled);

	let r: super::wire::PromptResult =
		serde_json::from_str(r#"{"stopReason":"future_reason"}"#).expect("deser");
	assert_eq!(r.stop_reason, StopReason::Unknown);
}

#[test]
fn session_update_agent_message_chunk_decoded() {
	let raw = r#"{"sessionId":"s","update":{"sessionUpdate":"agent_message_chunk","messageId":"m1","content":{"type":"text","text":"hello"}}}"#;
	let p: UpdateParams = serde_json::from_str(raw).expect("deser");
	assert_eq!(p.session_id.as_str(), "s");
	assert!(
		matches!(
			p.update,
			super::wire::SessionUpdateEnvelope::AgentMessageChunk { ref message_id, .. }
				if message_id.as_deref() == Some("m1")
		),
		"wrong update variant"
	);
}

#[test]
fn session_update_unknown_variant_is_unknown() {
	let raw =
		r#"{"sessionId":"s","update":{"sessionUpdate":"something_from_the_future","data":42}}"#;
	let p: UpdateParams = serde_json::from_str(raw).expect("deser");
	assert!(matches!(p.update, super::wire::SessionUpdateEnvelope::Unknown));
}

#[test]
fn new_session_result_decoded() {
	let raw = r#"{"sessionId":"sess_abc123"}"#;
	let r: super::wire::NewSessionResult = serde_json::from_str(raw).expect("deser");
	assert_eq!(r.session_id.as_str(), "sess_abc123");
}

#[test]
fn initialize_result_decoded() {
	let raw = r#"{"protocolVersion":1,"agentCapabilities":{"loadSession":true},"agentInfo":{"name":"my-agent","version":"0.1.0"}}"#;
	let r: super::wire::InitializeResult = serde_json::from_str(raw).expect("deser");
	assert_eq!(r.protocol_version, 1);
	assert!(r.agent_capabilities.load_session);
	assert_eq!(r.agent_info.as_ref().unwrap().name.as_str(), "my-agent");
}

#[test]
fn permission_option_kind_round_trips() {
	let opt: PermissionOption =
		serde_json::from_str(r#"{"optionId":"a","name":"Allow","kind":"allow_once"}"#)
			.expect("deser");
	assert_eq!(opt.kind, PermissionOptionKind::AllowOnce);

	let opt2: PermissionOption =
		serde_json::from_str(r#"{"optionId":"x","name":"X","kind":"totally_new_kind"}"#)
			.expect("deser");
	assert_eq!(opt2.kind, PermissionOptionKind::Unknown);
}

#[test]
fn cancel_notification_serializes_correctly() {
	let params = CancelParams { session_id: "sess1" };
	let notif = super::wire::OutNotification { jsonrpc: "2.0", method: "session/cancel", params };
	let json = serde_json::to_string(&notif).expect("ser");
	assert!(json.contains("\"method\":\"session/cancel\""), "{json}");
	assert!(json.contains("\"sessionId\":\"sess1\""), "{json}");
}

#[test]
fn session_new_always_carries_mcp_servers_even_when_empty() {
	// ACP requires `mcpServers` on every `session/new`. A real agent (DeepSeek
	// Harness 0.2.0-rc.2) rejects a request without it: -32602 "mcpServers:
	// Required value is missing". See docs/research/acp-handshakes.
	let params = super::wire::NewSessionParams { cwd: "/work", mcp_servers: &[] };
	let value = serde_json::to_value(&params).expect("serialize");
	assert_eq!(value, serde_json::json!({"cwd": "/work", "mcpServers": []}));
}
