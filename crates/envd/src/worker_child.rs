//! Same-binary named-worker child entry and bounded WorkerFrame health loop.

use std::{
	env,
	io::{self, Read, Write},
	thread,
};

use omp_proto::{
	SCHEMA_REV,
	bounds::{FRAME_MAX_BYTES, validate_host_frame},
	prost::Message,
	toolhost::v1::{HostFrame, WorkerFrame, WorkerHello, host_frame, worker_frame},
};
use pyo3::{prelude::*, types::PyModule};
use thiserror::Error;

/// Hidden argument used by app bootstrap to enter the named-worker role.
pub const PY_WORKER_ARG: &str = "__omp-py-worker";
const API_LEVEL: u32 = 1;

/// Errors raised by the named-worker child protocol.
#[derive(Debug, Error)]
pub enum WorkerChildError {
	/// A decoded frame exceeded a protocol bound.
	#[error("worker frame violates the protocol bounds")]
	Bounds(#[source] omp_proto::bounds::FrameBoundsError),
	/// A generation environment value was malformed.
	#[error("invalid worker generation environment")]
	Generation(#[source] std::num::ParseIntError),
	/// The bounded stdio protocol failed.
	#[error("worker protocol I/O failed")]
	Io(#[from] io::Error),
	/// A frame exceeded the protocol bound.
	#[error("worker frame exceeds the protocol bound")]
	FrameTooLarge,
	/// A frame was not valid protobuf.
	#[error("worker frame protobuf decode failed")]
	Decode(#[source] prost::DecodeError),
	/// A response could not be encoded.
	#[error("worker frame protobuf encode failed")]
	Encode(#[source] prost::EncodeError),
	/// A length prefix was malformed.
	#[error("worker frame length prefix is invalid")]
	Length,
	/// The embedded Python runtime failed to boot.
	#[error("worker Python runtime failed to boot")]
	PythonInit(#[source] omp_py::InitError),
	/// The worker Python call failed.
	#[error("worker Python call failed")]
	Python(#[source] pyo3::PyErr),
}

/// Runs the hidden named-worker child role.
pub fn run_worker_child_entry() -> Result<(), WorkerChildError> {
	let hello = WorkerHello {
		schema_rev:         SCHEMA_REV,
		python_rev:         env::var("OMP_WORKER_PYTHON_REV").unwrap_or_else(|_| "3.14t".to_owned()),
		worker_id:          env::var("OMP_WORKER_ID")
			.unwrap_or_else(|_| "unknown-worker".to_owned())
			.into_bytes()
			.into(),
		api_level:          API_LEVEL,
		layer:              env::var("OMP_WORKER_LAYER").unwrap_or_else(|_| "project".to_owned()),
		tier:               env::var("OMP_WORKER_TIER").unwrap_or_else(|_| "trusted".to_owned()),
		pool:               env::var("OMP_WORKER_POOL").unwrap_or_else(|_| "default".to_owned()),
		host_version:       env!("CARGO_PKG_VERSION").to_owned(),
		host_generation:    generation("OMP_WORKER_HOST_GENERATION")?,
		session_generation: generation("OMP_WORKER_SESSION_GENERATION")?,
		props:              None,
	};
	let stdin = io::stdin();
	let stdout = io::stdout();
	let mut output = stdout.lock();
	write_frame(&mut output, WorkerFrame {
		request_id: 0,
		body:       Some(worker_frame::Body::Hello(hello)),
		props:      None,
	})?;
	drop(output);
	thread::spawn(move || {
		let _ = health_loop(stdin, stdout);
	});
	let engine = omp_py::Engine::builder()
		.init()
		.map_err(WorkerChildError::PythonInit)?;
	engine
		.attach(|py| -> PyResult<()> {
			PyModule::import(py, "omp._worker")?.call_method0("run_forever")?;
			Ok(())
		})
		.map_err(WorkerChildError::Python)
}

fn health_loop(stdin: io::Stdin, stdout: io::Stdout) -> Result<(), WorkerChildError> {
	let mut input = stdin.lock();
	let mut output = stdout.lock();
	loop {
		let Some(frame) = read_frame(&mut input)? else {
			return Ok(());
		};
		let body = match frame.body {
			Some(host_frame::Body::Ping(ping)) => {
				worker_frame::Body::Pong(omp_proto::toolhost::v1::Pong {
					nonce: ping.nonce,
					props: None,
				})
			},
			_ => worker_frame::Body::Error(omp_proto::toolhost::v1::ProtocolError {
				code:    omp_proto::toolhost::v1::ProtocolErrorCode::Unsupported as i32,
				message: "worker child only accepts Ping".to_owned(),
				props:   None,
			}),
		};
		write_frame(&mut output, WorkerFrame {
			request_id: frame.request_id,
			body:       Some(body),
			props:      None,
		})?;
	}
}
fn generation(name: &str) -> Result<u64, WorkerChildError> {
	env::var(name)
		.unwrap_or_else(|_| "0".to_owned())
		.parse()
		.map_err(WorkerChildError::Generation)
}
fn read_frame(reader: &mut impl Read) -> Result<Option<HostFrame>, WorkerChildError> {
	let Some(length) = read_length(reader)? else {
		return Ok(None);
	};
	if length > FRAME_MAX_BYTES as u64 {
		return Err(WorkerChildError::FrameTooLarge);
	}
	let mut bytes = vec![0; usize::try_from(length).map_err(|_| WorkerChildError::FrameTooLarge)?];
	reader.read_exact(&mut bytes)?;
	validate_host_frame(&bytes).map_err(WorkerChildError::Bounds)?;
	HostFrame::decode(bytes.as_slice())
		.map(Some)
		.map_err(WorkerChildError::Decode)
}
fn write_frame(writer: &mut impl Write, frame: WorkerFrame) -> Result<(), WorkerChildError> {
	let mut bytes = Vec::new();
	frame.encode(&mut bytes).map_err(WorkerChildError::Encode)?;
	if bytes.len() > FRAME_MAX_BYTES {
		return Err(WorkerChildError::FrameTooLarge);
	}
	write_length(writer, bytes.len() as u64)?;
	writer.write_all(&bytes)?;
	writer.flush().map_err(WorkerChildError::Io)
}
fn read_length(reader: &mut impl Read) -> Result<Option<u64>, WorkerChildError> {
	let mut value = 0;
	for shift in (0..70).step_by(7) {
		let mut byte = [0];
		match reader.read(&mut byte)? {
			0 if shift == 0 => return Ok(None),
			0 => return Err(WorkerChildError::Length),
			1 => {
				value |= u64::from(byte[0] & 0x7f) << shift;
				if byte[0] & 0x80 == 0 {
					return Ok(Some(value));
				}
			},
			_ => unreachable!(),
		}
	}
	Err(WorkerChildError::Length)
}
fn write_length(writer: &mut impl Write, mut value: u64) -> Result<(), WorkerChildError> {
	let mut bytes = [0; 10];
	let mut index = 0;
	loop {
		let mut byte = (value & 0x7f) as u8;
		value >>= 7;
		if value != 0 {
			byte |= 0x80;
		}
		bytes[index] = byte;
		index += 1;
		if value == 0 {
			break;
		}
	}
	writer
		.write_all(&bytes[..index])
		.map_err(WorkerChildError::Io)
}

#[cfg(test)]
mod tests {
	use omp_proto::toolhost::v1::Ping;

	use super::*;
	#[test]
	fn length_delimited_ping_round_trips() {
		let frame = HostFrame {
			request_id: 7,
			body:       Some(host_frame::Body::Ping(Ping { nonce: 42, props: None })),
			props:      None,
		};
		let mut bytes = Vec::new();
		frame.encode(&mut bytes).expect("host frame encodes");
		let mut stream = Vec::new();
		write_length(&mut stream, bytes.len() as u64).expect("length encodes");
		stream.extend_from_slice(&bytes);
		let decoded = read_frame(&mut stream.as_slice())
			.expect("frame reads")
			.expect("frame exists");
		assert_eq!(decoded.request_id, 7);
		assert!(matches!(decoded.body, Some(host_frame::Body::Ping(Ping { nonce: 42, .. }))));
	}
}
