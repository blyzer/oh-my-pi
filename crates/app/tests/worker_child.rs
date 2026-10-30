use std::{
	io::{Read, Write},
	process::{Command, Stdio},
};

use omp_proto::{
	prost::Message,
	toolhost::v1::{HostFrame, Ping, WorkerFrame, host_frame, worker_frame},
};

fn read_frame<R: Read>(reader: &mut R) -> WorkerFrame {
	let mut length = [0u8; 1];
	let mut value = 0u64;
	let mut shift = 0;
	loop {
		reader.read_exact(&mut length).expect("worker frame length");
		value |= u64::from(length[0] & 0x7f) << shift;
		if length[0] & 0x80 == 0 {
			break;
		}
		shift += 7;
	}
	let mut bytes = vec![0; usize::try_from(value).expect("frame length")];
	reader.read_exact(&mut bytes).expect("worker frame body");
	WorkerFrame::decode(bytes.as_slice()).expect("worker protobuf frame")
}
fn write_frame<W: Write>(writer: &mut W, frame: HostFrame) {
	let mut bytes = Vec::new();
	frame.encode(&mut bytes).expect("host protobuf frame");
	let mut value = bytes.len() as u64;
	loop {
		let mut byte = (value & 0x7f) as u8;
		value >>= 7;
		if value != 0 {
			byte |= 0x80;
		}
		writer.write_all(&[byte]).expect("host frame length");
		if value == 0 {
			break;
		}
	}
	writer.write_all(&bytes).expect("host frame body");
	writer.flush().expect("host frame flush");
}

#[test]
fn hidden_worker_child_emits_hello_and_answers_ping() {
	let mut child = Command::new(env!("CARGO_BIN_EXE_omp"))
		.arg("__omp-py-worker")
		.stdin(Stdio::piped())
		.stdout(Stdio::piped())
		.stderr(Stdio::piped())
		.spawn()
		.expect("spawn omp worker child");
	let mut stdout = child.stdout.take().expect("worker stdout");
	let mut stdin = child.stdin.take().expect("worker stdin");
	let hello = read_frame(&mut stdout);
	let Some(worker_frame::Body::Hello(hello)) = hello.body else {
		panic!("worker did not emit WorkerHello");
	};
	assert_eq!(hello.schema_rev, omp_proto::SCHEMA_REV);
	assert_eq!(hello.api_level, 1);
	assert_eq!(hello.worker_id.as_ref(), b"unknown-worker");
	write_frame(&mut stdin, HostFrame {
		request_id: 7,
		body:       Some(host_frame::Body::Ping(Ping { nonce: 42, props: None })),
		props:      None,
	});
	let pong = read_frame(&mut stdout);
	assert_eq!(pong.request_id, 7);
	assert!(matches!(pong.body, Some(worker_frame::Body::Pong(frame)) if frame.nonce == 42));
	drop(stdin);
	let _ = child.kill();
}
