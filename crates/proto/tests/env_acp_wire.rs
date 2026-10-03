//! Verifies the removed ACP terminal-execution wire surface stays retired.
//!
//! `terminal/*` is unused (ADR 0037), so `AcpBind.exec`, `AcpExec*`, and their
//! frame tags are `reserved`. A peer that still emits them must decode as a
//! frame the receiver skips, never as a routed command.

use omp_proto::{
	env::v1::{AcpBind, ClientFrame, ServerFrame},
	prost::Message,
};

#[test]
fn legacy_acp_bind_exec_flag_is_skipped_as_an_unknown_field() {
	// documents = true (field 1), exec = true (reserved field 2).
	let legacy = [0x08, 0x01, 0x10, 0x01];
	let bind = AcpBind::decode(legacy.as_slice()).expect("legacy bind decodes");
	assert_eq!(bind, AcpBind { documents: true, fs_timeout_ms: 0 });
	assert_eq!(bind.encode_to_vec(), [0x08, 0x01]);
}

#[test]
fn legacy_exec_frames_decode_without_a_body() {
	// Field 41 (`acp_exec_event`) of ClientFrame: tag 0xCA 0x02, empty payload.
	let client = ClientFrame::decode([0xca, 0x02, 0x00].as_slice()).expect("legacy client frame");
	assert!(client.body.is_none());
	// Fields 40 and 41 (`acp_exec_query`, `acp_exec_cancel`) of ServerFrame.
	for tag in [[0xc2, 0x02, 0x00], [0xca, 0x02, 0x00]] {
		let server = ServerFrame::decode(tag.as_slice()).expect("legacy server frame");
		assert!(server.body.is_none());
	}
}
