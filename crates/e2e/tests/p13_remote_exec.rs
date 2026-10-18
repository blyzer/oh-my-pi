//! P13: `ReapiBackend` over a fake in-process REAPI server.
//!
//! Exercises the four observable outcomes that the domain promises:
//!
//! - exit 0 is reported as a successful result (`Outcome::Exited { code: 0 }`);
//! - exit 1 (non-zero) is a distinct domain outcome, not an error;
//! - an infra fault (gRPC `ResourceExhausted`) returns a typed
//!   `ExecutionError::Capacity`; the caller never needs to unwrap a string;
//! - a cancel call while the run is in-flight reaches the server and returns
//!   `CancelOutcome::Requested`.
//!
//! The REAPI server runs in-process via tonic's `serve_with_incoming` on a
//! loopback TCP port. No file system, no Environment daemon, no BuildBuddy
//! account.

use std::{collections::BTreeMap, sync::Arc, time::Duration};

use bytes::Bytes;
use omp_adw::{ApprovalScope, NetworkScope, Posture, WriteScope};
use omp_core::{Hash32, Str};
use omp_exec::{
	BackendId, CachePolicy, CancelOutcome, Eligibility, ExecutionBackend, ExecutionError,
	ExecutionEvent, ExecutionId, ExecutionRun, InputSet, Intent, NeedSet, OutputSpec, Platform,
	PlatformRequirement, ResourceRequirements, ExclusionCounts,
};
use omp_exec_reapi::{
	ExecuteConfig, ReapiBackend,
	capabilities,
	cas::{BlobSource, InputSource},
	proto::{
		build::bazel::remote::execution::v2 as wire,
		google::{bytestream, longrunning},
	},
	tree::InputFile,
};
use prost::Message as _;
use prost_types::Any;
use tokio::sync::Mutex as AsyncMutex;
use tokio_stream::wrappers::TcpListenerStream;
use tonic::{Code, Request, Response, Status, transport::Server};

// ── fake REAPI server ──────────────────────────────────────────────────────

#[derive(Clone)]
struct Fake {
	/// Digests recorded by `batch_update_blobs`.
	uploaded:  Arc<AsyncMutex<Vec<omp_exec::Digest>>>,
	/// Set to true when `cancel_operation` is called.
	cancelled: Arc<AsyncMutex<bool>>,
	/// When true the first `execute` returns `done=false`; `wait_execution`
	/// returns `done=true`, exercising the reattach path.
	reconnect: bool,
	/// Exit code the fake action should return.
	exit_code: i32,
	/// When `Some`, `execute` returns this gRPC error instead of an operation.
	fault: Option<Code>,
}

type ExecStream = tonic::codegen::BoxStream<longrunning::Operation>;

#[tonic::async_trait]
impl wire::execution_server::Execution for Fake {
	async fn execute(
		&self,
		_req: Request<wire::ExecuteRequest>,
	) -> Result<Response<ExecStream>, Status> {
		if let Some(code) = self.fault {
			return Err(Status::new(code, "injected fault"));
		}
		let op = make_operation(self.reconnect.then_some(false), self.exit_code);
		Ok(Response::new(Box::pin(tokio_stream::iter([Ok(op)]))))
	}

	async fn wait_execution(
		&self,
		_req: Request<wire::WaitExecutionRequest>,
	) -> Result<Response<ExecStream>, Status> {
		let op = make_operation(Some(true), self.exit_code);
		Ok(Response::new(Box::pin(tokio_stream::iter([Ok(op)]))))
	}
}

#[tonic::async_trait]
impl wire::capabilities_server::Capabilities for Fake {
	async fn get_capabilities(
		&self,
		_req: Request<wire::GetCapabilitiesRequest>,
	) -> Result<Response<wire::ServerCapabilities>, Status> {
		Ok(Response::new(server_caps()))
	}
}

#[tonic::async_trait]
impl wire::content_addressable_storage_server::ContentAddressableStorage for Fake {
	async fn find_missing_blobs(
		&self,
		req: Request<wire::FindMissingBlobsRequest>,
	) -> Result<Response<wire::FindMissingBlobsResponse>, Status> {
		Ok(Response::new(wire::FindMissingBlobsResponse {
			missing_blob_digests: req.into_inner().blob_digests,
		}))
	}

	async fn batch_update_blobs(
		&self,
		req: Request<wire::BatchUpdateBlobsRequest>,
	) -> Result<Response<wire::BatchUpdateBlobsResponse>, Status> {
		let mut guard = self.uploaded.lock().await;
		let responses = req
			.into_inner()
			.requests
			.into_iter()
			.map(|r| {
				if let Some(d) = r.digest.as_ref().and_then(|d| {
					let hash: [u8; 32] = d.hash.as_bytes().try_into().ok()?;
					Some(omp_exec::Digest::new(Hash32::new(hash), d.size_bytes as u64))
				}) {
					guard.push(d);
				}
				wire::batch_update_blobs_response::Response { digest: r.digest, status: None }
			})
			.collect();
		Ok(Response::new(wire::BatchUpdateBlobsResponse { responses }))
	}
}

#[tonic::async_trait]
impl bytestream::byte_stream_server::ByteStream for Fake {
	async fn write(
		&self,
		mut req: Request<tonic::Streaming<bytestream::WriteRequest>>,
	) -> Result<Response<bytestream::WriteResponse>, Status> {
		while req.get_mut().message().await?.is_some() {}
		Ok(Response::new(bytestream::WriteResponse { committed_size: 0 }))
	}
}

#[tonic::async_trait]
impl longrunning::operations_server::Operations for Fake {
	async fn cancel_operation(
		&self,
		_req: Request<longrunning::CancelOperationRequest>,
	) -> Result<Response<()>, Status> {
		*self.cancelled.lock().await = true;
		Ok(Response::new(()))
	}
}

// ── helpers ────────────────────────────────────────────────────────────────

fn blob_digest(bytes: &[u8]) -> omp_exec::Digest {
	omp_exec::Digest::new(Hash32::sum(bytes), bytes.len() as u64)
}

fn make_operation(done: Option<bool>, exit_code: i32) -> longrunning::Operation {
	let done = done.unwrap_or(true);
	let response = wire::ExecuteResponse {
		result: Some(wire::ActionResult {
			exit_code,
			stdout_digest: Some(omp_exec_reapi::digest::to_wire(blob_digest(b"out"))),
			stderr_digest: Some(omp_exec_reapi::digest::to_wire(blob_digest(b"err"))),
			..Default::default()
		}),
		..Default::default()
	};
	longrunning::Operation {
		name: "operations/p13-test".to_owned(),
		done,
		result: done.then(|| {
			longrunning::operation::Result::Response(Any {
				type_url: "type.googleapis.com/build.bazel.remote.execution.v2.ExecuteResponse"
					.to_owned(),
				value: response.encode_to_vec(),
			})
		}),
		..Default::default()
	}
}

fn server_caps() -> wire::ServerCapabilities {
	wire::ServerCapabilities {
		cache_capabilities: Some(wire::CacheCapabilities {
			digest_functions: vec![1], // SHA256
			action_cache_update_capabilities: Some(wire::ActionCacheUpdateCapabilities {
				update_enabled: false,
			}),
			..Default::default()
		}),
		execution_capabilities: Some(wire::ExecutionCapabilities {
			digest_function: 1,
			exec_enabled: true,
			..Default::default()
		}),
		..Default::default()
	}
}

/// Boots a tonic server hosting `fake` on a random loopback port and returns
/// a `ReapiBackend` connected to it.
async fn make_backend(fake: Fake) -> ReapiBackend<FixedSource> {
	let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
	let addr = listener.local_addr().unwrap();
	tokio::spawn(
		Server::builder()
			.add_service(wire::execution_server::ExecutionServer::new(fake.clone()))
			.add_service(wire::capabilities_server::CapabilitiesServer::new(fake.clone()))
			.add_service(
				wire::content_addressable_storage_server::ContentAddressableStorageServer::new(
					fake.clone(),
				),
			)
			.add_service(bytestream::byte_stream_server::ByteStreamServer::new(fake.clone()))
			.add_service(longrunning::operations_server::OperationsServer::new(fake))
			.serve_with_incoming(TcpListenerStream::new(listener)),
	);
	let channel = tonic::transport::Endpoint::from_shared(format!("http://{addr}"))
		.unwrap()
		.connect()
		.await
		.unwrap();
	let caps = capabilities::interpret(&server_caps()).unwrap();
	let config = ExecuteConfig {
		id:                  BackendId::new_static("p13"),
		instance:            Str::empty(),
		profile:             omp_exec_reapi::profile::Profile::reapi(),
		credential:          None,
		platform:            Platform::LINUX_X86_64,
		enforced:            Posture {
			write:    WriteScope::ReadOnly,
			network:  NetworkScope::Unrestricted,
			approval: ApprovalScope::Yolo,
		},
		cache:               CachePolicy::Off,
		platform_properties: BTreeMap::new(),
		toolchains:          BTreeMap::new(),
		buildbuddy_canceller: None,
	};
	ReapiBackend::from_channel(channel, config, caps, FixedSource)
}

fn make_request() -> omp_exec::ExecutionRequest {
	let file_digest = blob_digest(b"file-content");
	omp_exec::ExecutionRequest {
		id:          ExecutionId::new_static("p13"),
		intent:      Intent::Test,
		argv:        Arc::new([Str::new_static("true")]),
		cwd:         Str::empty(),
		env:         Arc::new([]),
		inputs:      Some(InputSet {
			manifest: blob_digest(b"manifest"),
			files:    1,
			bytes:    file_digest.size,
			excluded: ExclusionCounts { rules: Arc::new([]), symlinks: 0 },
		}),
		outputs:     OutputSpec::Logs,
		toolchain:   omp_exec::ToolchainId::new_static("test"),
		caches:      Arc::new([]),
		platform:    PlatformRequirement {
			os:            Platform::LINUX_X86_64.os,
			arch:          Platform::LINUX_X86_64.arch,
			needs_network: false,
			needs:         NeedSet::new(),
		},
		requirement: Default::default(),
		resources:   ResourceRequirements::default(),
		eligibility: Eligibility::RemotePreferred,
		timeout:     Duration::from_secs(30),
	}
}

/// A minimal in-process blob source: one fixed file blob.
#[derive(Clone)]
struct FixedSource;

impl BlobSource for FixedSource {
	async fn read(&self, _digest: omp_exec::Digest) -> std::io::Result<Bytes> {
		Ok(Bytes::from_static(b"file-content"))
	}

	async fn read_range(
		&self,
		digest: omp_exec::Digest,
		offset: u64,
		len: u64,
	) -> std::io::Result<Bytes> {
		let bytes = self.read(digest).await?;
		let start = (offset as usize).min(bytes.len());
		let end = start.saturating_add(len as usize).min(bytes.len());
		Ok(bytes.slice(start..end))
	}
}

impl InputSource for FixedSource {
	async fn files(&self, _manifest: omp_exec::Digest) -> std::io::Result<Vec<InputFile>> {
		let digest = blob_digest(b"file-content");
		Ok(vec![InputFile { path: "file.txt".to_owned(), digest, executable: false }])
	}
}

fn new_fake(exit_code: i32) -> Fake {
	Fake {
		uploaded:  Arc::new(AsyncMutex::new(Vec::new())),
		cancelled: Arc::new(AsyncMutex::new(false)),
		reconnect: false,
		exit_code,
		fault:     None,
	}
}

async fn settle(mut run: impl ExecutionRun) -> omp_exec::ExecutionResult {
	loop {
		match run.next_event().await {
			Some(ExecutionEvent::Settled(Ok(result))) => return result,
			Some(ExecutionEvent::Settled(Err(err))) => panic!("unexpected error: {err}"),
			Some(_) => {},
			None => panic!("run ended without settlement"),
		}
	}
}

// ── proofs ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn exit_zero_is_accepted() {
	let backend = make_backend(new_fake(0)).await;
	let run = backend.start(make_request()).await.unwrap();
	let result = settle(run).await;
	assert_eq!(result.outcome, omp_exec::Outcome::Exited { code: 0 });
	assert!(result.outcome.is_success());
}

#[tokio::test]
async fn nonzero_exit_is_a_distinct_outcome_not_an_error() {
	let backend = make_backend(new_fake(1)).await;
	let run = backend.start(make_request()).await.unwrap();
	let result = settle(run).await;
	assert_eq!(result.outcome, omp_exec::Outcome::Exited { code: 1 });
	assert!(!result.outcome.is_success());
}

#[tokio::test]
async fn infra_fault_is_a_typed_execution_error() {
	let fake = Fake {
		uploaded:  Arc::new(AsyncMutex::new(Vec::new())),
		cancelled: Arc::new(AsyncMutex::new(false)),
		reconnect: false,
		exit_code: 0,
		fault:     Some(Code::ResourceExhausted),
	};
	let backend = make_backend(fake).await;
	let result = backend.start(make_request()).await;
	assert!(
		matches!(result, Err(ExecutionError::Capacity { .. })),
		"expected Capacity error",
	);
}

#[tokio::test]
async fn cancel_while_in_flight_reaches_the_server() {
	let cancelled = Arc::new(AsyncMutex::new(false));
	let fake = Fake {
		uploaded:  Arc::new(AsyncMutex::new(Vec::new())),
		cancelled: cancelled.clone(),
		reconnect: true, // execute returns done=false; wait_execution returns done=true
		exit_code: 0,
		fault:     None,
	};
	let backend = make_backend(fake).await;
	let mut run = backend.start(make_request()).await.unwrap();
	// drain until Accepted so the operation name is known before cancelling
	loop {
		match run.next_event().await {
			Some(ExecutionEvent::Accepted { .. }) => break,
			Some(_) => {},
			None => panic!("run ended before Accepted"),
		}
	}
	let outcome = run.cancel().await;
	assert_eq!(outcome, CancelOutcome::Requested);
	assert!(*cancelled.lock().await, "cancel_operation was not called on the server");
}
