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

// ─── AcpExecutor / AcpDelegatedSession tests ─────────────────────────────────

mod executor_tests {
	use std::{path::PathBuf, time::Duration};

	use omp_agent::delegated::{
		ApprovalPosture, CliSessionId, DelegatedEvent, DelegatedSession, ExecutorError, FrameFault,
		NetworkPosture, PermissionAnswer, PromptRole, PromptSection, SandboxPosture, SessionRequest,
		UnknownAuth,
	};
	use omp_core::Str;
	use tokio::io::{AsyncWriteExt as _, DuplexStream, duplex};

	use crate::{
		acp_client::{
			delegated_session::{AcpDelegatedSession, AcpSessionConfig},
			executor::{self, AcpExecutor},
			policy::{ACP_POLICY, is_supported},
		},
		line_reader::LineReader,
	};

	// ─── helpers ──────────────────────────────────────────────────────────────

	const TIMEOUT: Duration = Duration::from_secs(5);

	fn cfg() -> AcpSessionConfig {
		AcpSessionConfig {
			handshake_timeout: TIMEOUT,
			terminate_grace:   Duration::from_millis(100),
			max_frame_bytes:   64 * 1024,
		}
	}

	fn strict_request(cwd: &'static str) -> SessionRequest {
		SessionRequest {
			mode:         omp_agent::delegated::ExecutionMode::CliManaged,
			workdir:      PathBuf::from(cwd),
			policy:       ACP_POLICY,
			resume:       None,
			unknown_auth: UnknownAuth::Proceed,
		}
	}

	fn simple_prompt() -> omp_agent::delegated::DelegatedPrompt {
		let sections =
			vec![PromptSection { role: PromptRole::User, text: Str::new_static("hello world") }];
		let bands = [omp_core::Hash32::new([0u8; 32]); 4];
		omp_agent::delegated::DelegatedPrompt::from_sections(sections, bands)
	}

	struct MockPeer {
		client_rx: LineReader<DuplexStream>,
		agent_tx:  DuplexStream,
	}

	impl MockPeer {
		async fn recv(&mut self) -> String {
			let line = tokio::time::timeout(TIMEOUT, self.client_rx.next_line())
				.await
				.expect("recv timeout")
				.expect("frame error")
				.expect("eof before line");
			String::from_utf8(line.to_vec()).expect("utf8")
		}

		async fn send(&mut self, msg: &str) {
			self
				.agent_tx
				.write_all(msg.as_bytes())
				.await
				.expect("write");
			self.agent_tx.write_all(b"\n").await.expect("newline");
		}

		async fn reply(&mut self, id: i64, result: &str) {
			self
				.send(&format!(r#"{{"jsonrpc":"2.0","id":{id},"result":{result}}}"#))
				.await;
		}

		async fn close(&mut self) {
			self.agent_tx.shutdown().await.expect("shutdown");
		}

		async fn handshake_new(&mut self, session_id: &str) {
			let init = self.recv().await;
			let v: serde_json::Value = serde_json::from_str(&init).unwrap();
			let id = v["id"].as_i64().unwrap();
			self
				.reply(
					id,
					r#"{"protocolVersion":1,"agentCapabilities":{"loadSession":false},"authMethods":[]}"#,
				)
				.await;
			let new = self.recv().await;
			let v: serde_json::Value = serde_json::from_str(&new).unwrap();
			let id = v["id"].as_i64().unwrap();
			assert_eq!(v["method"], "session/new");
			self
				.reply(id, &format!(r#"{{"sessionId":"{session_id}"}}"#))
				.await;
		}

		async fn handshake_load(&mut self, session_id: &str) {
			let init = self.recv().await;
			let v: serde_json::Value = serde_json::from_str(&init).unwrap();
			let id = v["id"].as_i64().unwrap();
			self
				.reply(
					id,
					r#"{"protocolVersion":1,"agentCapabilities":{"loadSession":true},"authMethods":[]}"#,
				)
				.await;
			let load = self.recv().await;
			let v: serde_json::Value = serde_json::from_str(&load).unwrap();
			let id = v["id"].as_i64().unwrap();
			assert_eq!(v["method"], "session/load");
			self
				.reply(id, &format!(r#"{{"sessionId":"{session_id}"}}"#))
				.await;
		}
	}

	/// Open a session through the real handshake path.
	async fn open_session(request: &SessionRequest) -> (AcpDelegatedSession, MockPeer) {
		let (agent_read, agent_write) = duplex(256 * 1024);
		let (client_read, client_write) = duplex(256 * 1024);
		let mut peer =
			MockPeer { client_rx: LineReader::new(client_read, 64 * 1024), agent_tx: agent_write };
		let handshake = AcpDelegatedSession::handshake(
			AcpExecutor::executor_id(),
			agent_read,
			client_write,
			None,
			request,
			cfg(),
		);
		let (result, ()) = tokio::join!(handshake, peer.handshake_new("sess-1"));
		let (session, _info) = result.expect("handshake ok");
		(session, peer)
	}

	/// Drains events until the first `Permission`, skipping lifecycle events.
	async fn next_permission(session: &mut AcpDelegatedSession) -> omp_agent::delegated::PromptId {
		loop {
			let event = session.next_event().await.expect("event");
			if let DelegatedEvent::Permission(req) = event {
				return req.prompt;
			}
		}
	}

	// ─── policy ───────────────────────────────────────────────────────────────

	#[test]
	fn strictest_is_supported_looser_is_not() {
		assert!(is_supported(ACP_POLICY));
		assert!(!is_supported(omp_agent::delegated::DelegatedPolicy::new(
			SandboxPosture::WorkspaceWrite,
			NetworkPosture::Disabled,
			ApprovalPosture::AlwaysAsk,
		)));
		assert!(!is_supported(omp_agent::delegated::DelegatedPolicy::new(
			SandboxPosture::ReadOnly,
			NetworkPosture::Open,
			ApprovalPosture::AlwaysAsk,
		)));
		assert!(!is_supported(omp_agent::delegated::DelegatedPolicy::new(
			SandboxPosture::ReadOnly,
			NetworkPosture::Disabled,
			ApprovalPosture::Yolo,
		)));
	}

	// ─── handshake ────────────────────────────────────────────────────────────

	#[tokio::test]
	async fn handshake_success_new_session() {
		let request = strict_request("/tmp/work");
		let (agent_read, agent_write) = duplex(256 * 1024);
		let (client_read, client_write) = duplex(256 * 1024);
		let mut peer =
			MockPeer { client_rx: LineReader::new(client_read, 64 * 1024), agent_tx: agent_write };
		let handshake = AcpDelegatedSession::handshake(
			AcpExecutor::executor_id(),
			agent_read,
			client_write,
			None,
			&request,
			cfg(),
		);
		let (result, ()) = tokio::join!(handshake, peer.handshake_new("my-session"));
		let (_session, info) = result.expect("handshake");
		assert_eq!(info.session, CliSessionId::new("my-session"));
		assert!(!info.resumed);
		assert_eq!(info.workdir, PathBuf::from("/tmp/work"));
		assert_eq!(info.policy, ACP_POLICY);
	}

	/// Non-empty `authMethods` → `AuthRequired`; no `session/new` is sent.
	#[tokio::test]
	async fn unauthenticated_agent_gives_auth_required() {
		let (agent_read, agent_write) = duplex(256 * 1024);
		let (client_read, client_write) = duplex(256 * 1024);
		let request = strict_request("/tmp");

		let handshake = AcpDelegatedSession::handshake(
			AcpExecutor::executor_id(),
			agent_read,
			client_write,
			None,
			&request,
			cfg(),
		);
		let peer_fut = async move {
			let mut rx = LineReader::new(client_read, 64 * 1024);
			let line = rx.next_line().await.expect("ok").expect("line");
			let v: serde_json::Value = serde_json::from_slice(&line).expect("json");
			let id = v["id"].as_i64().unwrap();
			let mut tx = agent_write;
			tx.write_all(
				format!(
					r#"{{"jsonrpc":"2.0","id":{id},"result":{{"protocolVersion":1,"authMethods":["oauth2"]}}}}"#
				)
				.as_bytes(),
			)
			.await
			.ok();
			tx.write_all(b"\n").await.ok();
			// No session/new response — client must not request it.
		};
		let (result, ()) = tokio::join!(handshake, peer_fut);
		assert!(matches!(result, Err(ExecutorError::AuthRequired { .. })), "got: {result:?}");
	}

	/// The id of the JSON-RPC request in `line`.
	fn request_id(line: &str) -> i64 {
		serde_json::from_str::<serde_json::Value>(line).unwrap()["id"]
			.as_i64()
			.unwrap()
	}

	/// Runs the handshake against a peer whose `session/new` fails with `error`
	/// (the raw JSON of the JSON-RPC `error` member).
	async fn handshake_failing_session_new(error: &str) -> Result<(), ExecutorError> {
		let (agent_read, agent_write) = duplex(256 * 1024);
		let (client_read, client_write) = duplex(256 * 1024);
		let mut peer =
			MockPeer { client_rx: LineReader::new(client_read, 64 * 1024), agent_tx: agent_write };
		let request = strict_request("/tmp");
		let handshake = AcpDelegatedSession::handshake(
			AcpExecutor::executor_id(),
			agent_read,
			client_write,
			None,
			&request,
			cfg(),
		);
		let script = async {
			let id = request_id(&peer.recv().await);
			peer
				.reply(
					id,
					r#"{"protocolVersion":1,"agentCapabilities":{"loadSession":false},"authMethods":[]}"#,
				)
				.await;
			let id = request_id(&peer.recv().await);
			peer
				.send(&format!(r#"{{"jsonrpc":"2.0","id":{id},"error":{error}}}"#))
				.await;
		};
		let (result, ()) = tokio::join!(handshake, script);
		result.map(|_| ())
	}

	/// Runs one turn against a peer that answers the prompt with `error` and
	/// returns the failure the turn ends with.
	async fn prompt_failure(error: &str) -> omp_agent::delegated::RemoteFailure {
		let request = strict_request("/tmp");
		let (mut session, mut peer) = open_session(&request).await;
		let input = omp_agent::delegated::TurnInput { prompt: simple_prompt() };
		let script = async {
			let id = request_id(&peer.recv().await);
			peer
				.send(&format!(r#"{{"jsonrpc":"2.0","id":{id},"error":{error}}}"#))
				.await;
		};
		let (accepted, ()) = tokio::join!(session.start_turn(&input), script);
		accepted.expect("start_turn ok");
		loop {
			if let DelegatedEvent::Failure { failure, .. } = session.next_event().await.expect("event")
			{
				return failure;
			}
		}
	}

	/// Grok's signed-out answer: `-32000` with `data: "no auth method id
	/// provided"`.
	const MISSING_LOGIN: &str =
		r#"{"code":-32000,"message":"Authentication required","data":"no auth method id provided"}"#;

	/// Grok fails `session/new` while signed out; that is a missing login.
	#[tokio::test]
	async fn session_new_missing_login_gives_auth_required() {
		let result = handshake_failing_session_new(MISSING_LOGIN).await;
		assert!(matches!(result, Err(ExecutorError::AuthRequired { .. })), "got: {result:?}");
	}

	/// A `session/new` rejection that is not the missing-login shape stays a
	/// protocol rejection carrying its code.
	#[tokio::test]
	async fn session_new_other_rejections_stay_rejections() {
		for error in [
			r#"{"code":-32000,"message":"Authentication required","data":"something else"}"#,
			r#"{"code":-32000,"message":"Authentication required"}"#,
			r#"{"code":-32603,"message":"x","data":"no auth method id provided"}"#,
		] {
			let result = handshake_failing_session_new(error).await;
			assert!(
				matches!(
					result,
					Err(ExecutorError::Protocol {
						violation: omp_agent::delegated::ProtocolViolation::RequestRejected { .. },
						..
					})
				),
				"{error} gave {result:?}"
			);
		}
	}

	/// A prompt that fails with the missing-login shape ends the turn as
	/// `AuthRequired`, not as an internal failure.
	#[tokio::test]
	async fn prompt_missing_login_ends_the_turn_as_auth_required() {
		let failure = prompt_failure(MISSING_LOGIN).await;
		assert!(
			matches!(failure, omp_agent::delegated::RemoteFailure::AuthRequired),
			"got: {failure:?}"
		);
	}

	/// Everything that is not exactly the missing-login shape stays internal:
	/// dsh answers a signed-out prompt with `-32603`, Devin with `-32000` and
	/// no `data`, and a structured `data` is not text.
	#[tokio::test]
	async fn other_prompt_errors_stay_internal() {
		for error in [
			r#"{"code":-32603,"message":"Internal error"}"#,
			r#"{"code":-32603,"message":"x","data":"no auth method id provided"}"#,
			r#"{"code":-32000,"message":"Authentication required"}"#,
			r#"{"code":-32000,"message":"x","data":"something else"}"#,
			r#"{"code":-32000,"message":"x","data":{"detail":"no auth method id provided"}}"#,
		] {
			let failure = prompt_failure(error).await;
			assert!(
				matches!(failure, omp_agent::delegated::RemoteFailure::Internal),
				"{error} gave {failure:?}"
			);
		}
	}

	/// Relative `workdir` is rejected before any I/O.
	#[tokio::test]
	async fn relative_cwd_rejected() {
		let (agent_read, _aw) = duplex(1024);
		let (_cr, client_write) = duplex(1024);
		let request = SessionRequest {
			mode:         omp_agent::delegated::ExecutionMode::CliManaged,
			workdir:      PathBuf::from("relative/path"),
			policy:       ACP_POLICY,
			resume:       None,
			unknown_auth: UnknownAuth::Proceed,
		};
		let result = AcpDelegatedSession::handshake(
			AcpExecutor::executor_id(),
			agent_read,
			client_write,
			None,
			&request,
			cfg(),
		)
		.await;
		assert!(matches!(result, Err(ExecutorError::WorkdirNotAbsolute { .. })), "got: {result:?}");
	}

	/// `PolicyUnmappable` for anything looser than `STRICTEST`.
	#[tokio::test]
	async fn policy_looser_than_strictest_is_unmappable() {
		let (agent_read, _aw) = duplex(1024);
		let (_cr, client_write) = duplex(1024);
		let request = SessionRequest {
			mode:         omp_agent::delegated::ExecutionMode::CliManaged,
			workdir:      PathBuf::from("/tmp"),
			policy:       omp_agent::delegated::DelegatedPolicy::new(
				SandboxPosture::WorkspaceWrite,
				NetworkPosture::Disabled,
				ApprovalPosture::AlwaysAsk,
			),
			resume:       None,
			unknown_auth: UnknownAuth::Proceed,
		};
		let result = AcpDelegatedSession::handshake(
			AcpExecutor::executor_id(),
			agent_read,
			client_write,
			None,
			&request,
			cfg(),
		)
		.await;
		assert!(matches!(result, Err(ExecutorError::PolicyUnmappable { .. })), "got: {result:?}");
	}

	/// `session/load` is used when the agent advertises it and `resume` is set.
	#[tokio::test]
	async fn session_load_used_on_resume() {
		let (agent_read, agent_write) = duplex(256 * 1024);
		let (client_read, client_write) = duplex(256 * 1024);
		let mut peer =
			MockPeer { client_rx: LineReader::new(client_read, 64 * 1024), agent_tx: agent_write };
		let request = SessionRequest {
			mode:         omp_agent::delegated::ExecutionMode::CliManaged,
			workdir:      PathBuf::from("/tmp"),
			policy:       ACP_POLICY,
			resume:       Some(CliSessionId::new("old-sess")),
			unknown_auth: UnknownAuth::Proceed,
		};
		let handshake = AcpDelegatedSession::handshake(
			AcpExecutor::executor_id(),
			agent_read,
			client_write,
			None,
			&request,
			cfg(),
		);
		let (result, ()) = tokio::join!(handshake, peer.handshake_load("old-sess"));
		let (_session, info) = result.expect("load ok");
		assert!(info.resumed);
		assert_eq!(info.session, CliSessionId::new("old-sess"));
	}

	/// When `loadSession` is false, resume returns `ResumeFailed`.
	#[tokio::test]
	async fn resume_fails_when_load_session_not_advertised() {
		let (agent_read, agent_write) = duplex(256 * 1024);
		let (client_read, client_write) = duplex(256 * 1024);
		let request = SessionRequest {
			mode:         omp_agent::delegated::ExecutionMode::CliManaged,
			workdir:      PathBuf::from("/tmp"),
			policy:       ACP_POLICY,
			resume:       Some(CliSessionId::new("old")),
			unknown_auth: UnknownAuth::Proceed,
		};
		let handshake = AcpDelegatedSession::handshake(
			AcpExecutor::executor_id(),
			agent_read,
			client_write,
			None,
			&request,
			cfg(),
		);
		let peer_fut = async move {
			let mut rx = LineReader::new(client_read, 64 * 1024);
			let line = rx.next_line().await.expect("ok").expect("line");
			let v: serde_json::Value = serde_json::from_slice(&line).expect("json");
			let id = v["id"].as_i64().unwrap();
			let mut tx = agent_write;
			tx.write_all(
				format!(
					r#"{{"jsonrpc":"2.0","id":{id},"result":{{"protocolVersion":1,"agentCapabilities":{{"loadSession":false}},"authMethods":[]}}}}"#
				)
				.as_bytes(),
			)
			.await
			.ok();
			tx.write_all(b"\n").await.ok();
		};
		let (result, ()) = tokio::join!(handshake, peer_fut);
		assert!(matches!(result, Err(ExecutorError::ResumeFailed { .. })), "got: {result:?}");
	}

	// ─── prompt digest ────────────────────────────────────────────────────────

	#[tokio::test]
	async fn every_prompt_section_is_one_text_block_and_the_digest_matches_the_host() {
		let request = strict_request("/tmp");
		let (mut session, mut peer) = open_session(&request).await;
		let sections = vec![
			PromptSection { role: PromptRole::System, text: Str::new_static("be careful") },
			PromptSection { role: PromptRole::User, text: Str::new_static("hello world") },
		];
		let bands = [omp_core::Hash32::new([7u8; 32]); 4];
		let prompt = omp_agent::delegated::DelegatedPrompt::from_sections(sections, bands);
		let expected = prompt.digest();
		let input = omp_agent::delegated::TurnInput { prompt };
		let peer_fut = async {
			let line = peer.recv().await;
			let v: serde_json::Value = serde_json::from_str(&line).unwrap();
			let id = v["id"].as_i64().unwrap();
			peer.reply(id, r#"{"stopReason":"end_turn"}"#).await;
			v["params"]["prompt"].clone()
		};
		let (accepted, blocks) = tokio::join!(session.start_turn(&input), peer_fut);
		let accepted = accepted.expect("start_turn ok");
		assert_eq!(
			blocks,
			serde_json::json!([
				{"type": "text", "text": "be careful"},
				{"type": "text", "text": "hello world"},
			]),
			"each section travels as its own block, verbatim and in order"
		);
		assert_eq!(
			accepted.forwarded, expected,
			"the host compares these digests; a mismatch ends the session as PromptRewritten"
		);
	}

	#[tokio::test]
	async fn every_event_of_a_turn_carries_the_id_start_turn_returned() {
		let request = strict_request("/tmp");
		let (mut session, mut peer) = open_session(&request).await;
		let input = omp_agent::delegated::TurnInput { prompt: simple_prompt() };

		let peer_fut = async {
			let line = peer.recv().await;
			let v: serde_json::Value = serde_json::from_str(&line).unwrap();
			let id = v["id"].as_i64().unwrap();
			peer.send(
				r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"sess-1","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"hi"}}}}"#,
			)
			.await;
			peer.reply(id, r#"{"stopReason":"end_turn"}"#).await;
		};
		let (accepted, ()) = tokio::join!(session.start_turn(&input), peer_fut);
		let accepted = accepted.expect("start_turn ok");

		let mut seen = 0;
		loop {
			let event = session.next_event().await.expect("event");
			let done = matches!(
				event,
				DelegatedEvent::Lifecycle(omp_agent::delegated::LifecycleEvent::TurnCompleted { .. })
			);
			assert_eq!(
				event.turn(),
				Some(&accepted.turn),
				"the host ends the session as TurnMismatch when an event names another turn: {event:?}"
			);
			seen += 1;
			if done {
				break;
			}
		}
		assert!(seen >= 3, "started, a text delta and completed were all checked, saw {seen}");
	}

	// ─── permission ───────────────────────────────────────────────────────────

	#[tokio::test]
	async fn permission_allow_once_selects_allow_once() {
		let request = strict_request("/tmp");
		let (mut session, mut peer) = open_session(&request).await;
		let input = omp_agent::delegated::TurnInput { prompt: simple_prompt() };

		// The peer sends the permission request, then immediately replies to
		// session/prompt — the answer arrives asynchronously via session.answer().
		let peer_fut = async {
			let pl = peer.recv().await;
			let v: serde_json::Value = serde_json::from_str(&pl).unwrap();
			let pid = v["id"].as_i64().unwrap();
			peer.send(
				r#"{"jsonrpc":"2.0","id":99,"method":"session/request_permission","params":{"sessionId":"sess-1","toolCall":{"toolCallId":"tc1","title":"rm -rf /"},"options":[{"optionId":"allow_once","name":"Allow once","kind":"allow_once"},{"optionId":"reject_once","name":"Deny","kind":"reject_once"}]}}"#,
			)
			.await;
			// Reply to session/prompt immediately; answer comes later.
			peer.reply(pid, r#"{"stopReason":"end_turn"}"#).await;
			peer
		};
		let (accepted, mut peer) = tokio::join!(session.start_turn(&input), peer_fut);
		let _ = accepted.expect("start ok");

		let prompt_id = next_permission(&mut session).await;
		session
			.answer(&prompt_id, PermissionAnswer::AllowOnce)
			.await
			.expect("answer");

		// Now read the answer the session sent to the peer.
		let answer = peer.recv().await;
		assert!(answer.contains("\"outcome\":\"selected\""), "{answer}");
		assert!(answer.contains("allow_once"), "{answer}");
	}

	#[tokio::test]
	async fn permission_deny_selects_reject_once() {
		let request = strict_request("/tmp");
		let (mut session, mut peer) = open_session(&request).await;
		let input = omp_agent::delegated::TurnInput { prompt: simple_prompt() };

		let peer_fut = async {
			let pl = peer.recv().await;
			let v: serde_json::Value = serde_json::from_str(&pl).unwrap();
			let pid = v["id"].as_i64().unwrap();
			peer.send(
				r#"{"jsonrpc":"2.0","id":100,"method":"session/request_permission","params":{"sessionId":"sess-1","toolCall":{"toolCallId":"tc2","title":"edit"},"options":[{"optionId":"allow_once","name":"Allow","kind":"allow_once"},{"optionId":"reject_once","name":"Deny","kind":"reject_once"}]}}"#,
			)
			.await;
			peer.reply(pid, r#"{"stopReason":"end_turn"}"#).await;
			peer
		};
		let (accepted, mut peer) = tokio::join!(session.start_turn(&input), peer_fut);
		let _ = accepted.expect("start ok");

		let prompt_id = next_permission(&mut session).await;
		session
			.answer(&prompt_id, PermissionAnswer::Deny)
			.await
			.expect("answer");

		let answer = peer.recv().await;
		assert!(answer.contains("\"outcome\":\"selected\""), "{answer}");
		assert!(answer.contains("reject_once"), "{answer}");
	}

	/// When no matching option exists (only allow_always), we send `cancelled`.
	#[tokio::test]
	async fn permission_unknown_option_sends_cancelled() {
		let request = strict_request("/tmp");
		let (mut session, mut peer) = open_session(&request).await;
		let input = omp_agent::delegated::TurnInput { prompt: simple_prompt() };

		let peer_fut = async {
			let pl = peer.recv().await;
			let v: serde_json::Value = serde_json::from_str(&pl).unwrap();
			let pid = v["id"].as_i64().unwrap();
			// Only allow_always: no allow_once / reject_once.
			peer.send(
				r#"{"jsonrpc":"2.0","id":101,"method":"session/request_permission","params":{"sessionId":"sess-1","toolCall":{"toolCallId":"tc3","title":"network"},"options":[{"optionId":"always","name":"Always","kind":"allow_always"}]}}"#,
			)
			.await;
			peer.reply(pid, r#"{"stopReason":"end_turn"}"#).await;
			peer
		};
		let (accepted, mut peer) = tokio::join!(session.start_turn(&input), peer_fut);
		let _ = accepted.expect("start ok");

		let prompt_id = next_permission(&mut session).await;
		// Deny with only allow_always → no reject_once option → Cancelled.
		session
			.answer(&prompt_id, PermissionAnswer::Deny)
			.await
			.expect("answer");

		let answer = peer.recv().await;
		assert!(answer.contains("\"outcome\":\"cancelled\""), "{answer}");
	}

	// ─── cancel mid-turn ──────────────────────────────────────────────────────

	#[tokio::test]
	async fn cancel_mid_turn_sends_session_cancel_notification() {
		let request = strict_request("/tmp");
		let (mut session, mut peer) = open_session(&request).await;
		let input = omp_agent::delegated::TurnInput { prompt: simple_prompt() };

		// The peer reads session/prompt and holds it; the session is now live.
		// start_turn returns after sending the prompt request.
		let peer_fut = async {
			let pl = peer.recv().await;
			let v: serde_json::Value = serde_json::from_str(&pl).unwrap();
			let pid = v["id"].as_i64().unwrap();
			(peer, pid)
		};
		let (accepted, (mut peer, pid)) = tokio::join!(session.start_turn(&input), peer_fut);
		let _ = accepted.expect("start ok");

		// Now call interrupt, which sends session/cancel.
		session.interrupt().await.expect("interrupt ok");

		// Peer reads the cancel notification and replies to session/prompt.
		let cancel_line = peer.recv().await;
		peer.reply(pid, r#"{"stopReason":"cancelled"}"#).await;

		assert!(cancel_line.contains("session/cancel"), "{cancel_line}");
	}

	// ─── peer crash ───────────────────────────────────────────────────────────

	#[tokio::test]
	async fn peer_crash_gives_crashed_error() {
		let request = strict_request("/tmp");
		let (mut session, mut peer) = open_session(&request).await;
		peer.close().await;
		let result = session.next_event().await;
		assert!(matches!(result, Err(ExecutorError::Crashed { .. })), "got: {result:?}");
	}

	// ─── oversize / malformed ─────────────────────────────────────────────────

	#[tokio::test]
	async fn oversize_frame_gives_malformed_oversize() {
		let request = strict_request("/tmp");
		let (mut session, mut peer) = open_session(&request).await;
		// cfg() sets max_frame_bytes = 64 KiB; this frame exceeds it.
		let huge_title = "x".repeat(64 * 1024 + 100);
		let msg = format!(
			r#"{{"jsonrpc":"2.0","method":"session/update","params":{{"sessionId":"s","update":{{"sessionUpdate":"notice","severity":"info","title":"{huge_title}"}}}}}}"#
		);
		peer.send(&msg).await;
		let result = session.next_event().await;
		assert!(
			matches!(
				result,
				Err(ExecutorError::MalformedStream { fault: FrameFault::Oversize { .. }, .. })
			),
			"got: {result:?}"
		);
	}

	#[tokio::test]
	async fn malformed_json_gives_malformed_not_json() {
		let request = strict_request("/tmp");
		let (mut session, mut peer) = open_session(&request).await;
		peer.send("this is not json").await;
		let result = session.next_event().await;
		assert!(
			matches!(result, Err(ExecutorError::MalformedStream { fault: FrameFault::NotJson, .. })),
			"got: {result:?}"
		);
	}

	// ─── drop kills process tree ──────────────────────────────────────────────

	#[tokio::test]
	async fn drop_kills_sh_child() {
		#[cfg(unix)]
		use nix::unistd::Pid;
		use tokio::process::Command;

		use crate::{child_tree::ChildTree, test_process::process_terminated};

		let dir = tempfile::tempdir().expect("tempdir");
		let pid_file = dir.path().join("grandchild.pid");
		let script = format!("sleep 300 & echo $! > '{}'; wait", pid_file.display());
		let mut cmd = Command::new("sh");
		cmd.arg("-c").arg(&script);
		let spawned = ChildTree::spawn(cmd).expect("spawn sh");

		// Wait for the grandchild to write its pid.
		let grandchild_pid: i32 = {
			let mut pid = None;
			for _ in 0..200 {
				if let Ok(text) = std::fs::read_to_string(&pid_file)
					&& let Ok(p) = text.trim().parse()
				{
					pid = Some(p);
					break;
				}
				tokio::time::sleep(Duration::from_millis(25)).await;
			}
			pid.expect("grandchild pid file never appeared")
		};

		#[cfg(unix)]
		assert!(!process_terminated(Pid::from_raw(grandchild_pid)), "grandchild alive before drop");

		// Drop the tree → kills the whole process group.
		drop(spawned.tree);

		// Give the OS time to propagate the signal.
		#[cfg(unix)]
		{
			for _ in 0..200 {
				if process_terminated(Pid::from_raw(grandchild_pid)) {
					return;
				}
				tokio::time::sleep(Duration::from_millis(25)).await;
			}
			panic!("grandchild {grandchild_pid} outlived the dropped ChildTree");
		}
	}

	// ─── start_session end-to-end ─────────────────────────────────────────────

	#[tokio::test]
	async fn start_session_end_to_end() {
		use omp_agent::delegated::{Capabilities, ExecutionMode, start_session};

		struct MemExec {
			caps: Capabilities,
			tx:   flume::Sender<MockPeer>,
		}

		impl omp_agent::delegated::DelegatedExecutor for MemExec {
			type Session = AcpDelegatedSession;

			fn capabilities(&self) -> &Capabilities {
				&self.caps
			}

			async fn probe(&self) -> Result<omp_agent::delegated::ProbeReport, ExecutorError> {
				Ok(omp_agent::delegated::ProbeReport {
					version: omp_agent::delegated::CliVersion::new(0, 0, 0),
					auth:    omp_agent::delegated::AuthStatus::Unknown,
				})
			}

			async fn open<'a>(
				&'a self,
				request: &'a SessionRequest,
			) -> Result<(Self::Session, omp_agent::delegated::SessionInfo), ExecutorError> {
				let (agent_read, agent_write) = duplex(256 * 1024);
				let (client_read, client_write) = duplex(256 * 1024);
				let mut peer = MockPeer {
					client_rx: LineReader::new(client_read, 64 * 1024),
					agent_tx:  agent_write,
				};
				let handshake = AcpDelegatedSession::handshake(
					AcpExecutor::executor_id(),
					agent_read,
					client_write,
					None,
					request,
					cfg(),
				);
				let (result, ()) = tokio::join!(handshake, peer.handshake_new("contract-sess"));
				let (session, info) = result?;
				let _ = self.tx.send(peer);
				Ok((session, info))
			}
		}

		let (tx, rx) = flume::bounded(1);
		let exec = MemExec { caps: executor::capabilities(), tx };
		let request = SessionRequest {
			mode:         ExecutionMode::CliManaged,
			workdir:      PathBuf::from("/tmp"),
			policy:       ACP_POLICY,
			resume:       None,
			unknown_auth: UnknownAuth::Proceed,
		};
		let active = start_session(&exec, &request)
			.await
			.expect("start_session ok");
		assert!(active.is_open());
		assert_eq!(active.info().policy, ACP_POLICY);
		let _peer = rx.try_recv().expect("peer sent");
	}
}

#[cfg(test)]
mod child_env_tests {
	use std::{ffi::OsStr, fs};

	use omp_agent::delegated::{DelegatedExecutor, ExecutionMode, SessionRequest, UnknownAuth};

	use crate::{acp_client::AcpExecutor, child_env::ChildEnv};

	fn request(dir: &std::path::Path) -> SessionRequest {
		SessionRequest {
			mode:         ExecutionMode::CliManaged,
			workdir:      dir.to_path_buf(),
			policy:       omp_agent::delegated::DelegatedPolicy::STRICTEST,
			resume:       None,
			unknown_auth: UnknownAuth::Proceed,
		}
	}

	/// Spawns `sh -c 'env > FILE'` as the "agent" through the real `open`; the
	/// handshake then fails (the script never speaks ACP), but the file holds
	/// exactly the environment the child was given.
	async fn child_environment(executor: AcpExecutor, dir: &std::path::Path) -> String {
		let file = dir.join("seen.env");
		let _ = executor.open(&request(dir)).await;
		fs::read_to_string(&file).unwrap_or_default()
	}

	fn spy(dir: &std::path::Path) -> AcpExecutor {
		let file = dir.join("seen.env");
		AcpExecutor::new("sh", ["-c".to_owned(), format!("env > '{}'", file.display())])
			.with_handshake_timeout(std::time::Duration::from_millis(500))
	}

	#[tokio::test]
	async fn the_default_environment_hides_a_parent_secret_from_the_agent() {
		let dir = tempfile::tempdir().expect("dir");
		// SAFETY: set before the child spawns; the name is unique to this test.
		unsafe { std::env::set_var("OMP_ACP_TEST_PARENT_API_KEY", "must-not-leak-1") };
		let seen = child_environment(spy(dir.path()), dir.path()).await;
		assert!(seen.contains("PATH="), "child lost its ambient PATH: {seen:?}");
		assert!(!seen.contains("must-not-leak-1"), "child inherited a parent secret: {seen:?}");
	}

	#[tokio::test]
	async fn an_operator_named_variable_reaches_the_agent_and_only_that_one() {
		let dir = tempfile::tempdir().expect("dir");
		// SAFETY: set before the child spawns; names are unique to this test.
		unsafe {
			std::env::set_var("OMP_ACP_TEST_NAMED_API_KEY", "named-value-2");
			std::env::set_var("OMP_ACP_TEST_OTHER_API_KEY", "must-not-leak-3");
		}
		let env = ChildEnv::ambient().with_inherited(["OMP_ACP_TEST_NAMED_API_KEY"]);
		assert!(env.contains(OsStr::new("OMP_ACP_TEST_NAMED_API_KEY")));
		let seen = child_environment(spy(dir.path()).with_env(env), dir.path()).await;
		assert!(seen.contains("OMP_ACP_TEST_NAMED_API_KEY=named-value-2"), "{seen:?}");
		assert!(!seen.contains("must-not-leak-3"), "an unnamed sibling secret leaked: {seen:?}");
	}
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
