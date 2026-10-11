//! Detached environment terminals retain resource identity without owning its lifetime.

use std::{sync::Arc, time::Duration};

use bytes::Bytes;
use futures::StreamExt;
use omp_agent::{ExternalDispatchEvent, ExternalDispatchRequest, ExternalDispatchStream, ExternalToolExecutor};
use omp_core::{Hash32, Str};
use omp_driver::headless::kernel::EnvToolExecutor;
use omp_env::{InProcessEnvTransport, frame::{self, client_frame, server_frame}};
use omp_journal::blob::BlobStore;
use omp_tool::{ArtifactLifetime, ExpectedArtifact, JobKind, JobMetadata, JobOwner, JobRef, OutputRequest, Rev, ToolIdentity, ToolRoute, ToolTerminal, WorkerSiteKind};
use tokio_util::sync::CancellationToken;

const WAIT: Duration = Duration::from_secs(2);

fn job() -> JobRef {
	JobRef {
		id: Str::new_static("job-a"),
		owner: JobOwner::NamedProcess { name: Str::new_static("bash-bg-1"), generation: 7 },
		metadata: Arc::new(JobMetadata::running(JobKind::Shell, Str::new_static("command"), 123)),
		artifact: ExpectedArtifact {
			description: Str::new_static("command settlement"),
			media_type: Some(Str::new_static("application/vnd.omp.process-settlement+json")),
			lifetime: ArtifactLifetime::Session,
		},
	}
}

fn executor(store: BlobStore, cancellation: CancellationToken) -> (ExternalDispatchStream, InProcessEnvTransport, omp_env::EnvClient) {
	let (client, transport) = omp_env::EnvClient::in_process(0);
	let (approvals, _inbox) = omp_agent::ApprovalRoute::new(Arc::new(omp_agent::ApprovalBook::new()), None);
	let stream = EnvToolExecutor::new(client.clone(), approvals).invoke(ExternalDispatchRequest {
		identity: ToolIdentity { name: Str::new_static("bash"), rev: Rev { family: Str::new_static("test"), n: 1 } },
		session_id: Str::new_static("session-a"),
		blobs: store,
		call_id: Str::new_static("call-a"),
		args: serde_json::value::RawValue::from_string("{}".to_owned()).expect("args"),
		route: ToolRoute::Worker { site: WorkerSiteKind::Env, name: Str::new_static("bash") },
		blocking_limit: Duration::from_secs(30),
		output_request: OutputRequest::Bounded,
		cancellation,
		restrictions: None,
	});
	(stream, transport, client)
}

async fn receive(transport: &InProcessEnvTransport) -> frame::ClientFrame {
	tokio::time::timeout(WAIT, transport.recv()).await.expect("bounded client request").expect("transport open")
}

async fn send(transport: &InProcessEnvTransport, request_id: u64, body: server_frame::Body) {
	transport.send(frame::ServerFrame { request_id, body: Some(body), ..Default::default() }).await.expect("server response");
}

async fn start(stream: &mut ExternalDispatchStream, transport: &InProcessEnvTransport) -> u64 {
	let request = tokio::select! {
		request = receive(transport) => request,
		_ = stream.next() => panic!("terminal before acceptance"),
	};
	assert!(matches!(&request.body, Some(client_frame::Body::InvokeTool(invoke)) if invoke.invocation_id == "call-a"));
	send(transport, request.request_id, server_frame::Body::InvocationAccepted(frame::InvokeAccepted { invocation_id: "call-a".to_owned(), ..Default::default() })).await;
	let committed = tokio::select! {
		request = receive(transport) => request,
		_ = stream.next() => panic!("terminal before commitment"),
	};
	assert!(matches!(&committed.body, Some(client_frame::Body::ArgsCommitted(args)) if args.invocation_id == "call-a"));
	request.request_id
}

fn verdict(bytes: Bytes) -> frame::Verdict {
	let size = u64::try_from(bytes.len()).expect("size");
	let artifact = omp_proto::thread::v1::Blob {
		hash: Bytes::copy_from_slice(Hash32::sum(&bytes).as_bytes()),
		mime: "application/json".to_owned(),
		size,
		..Default::default()
	};
	frame::Verdict {
		invocation_id: "call-a".to_owned(),
		json: bytes,
		details_blob: Some(artifact.clone()),
		projection: Some(frame::OutputProjection {
			request: frame::OutputRequest::Bounded as i32,
			source_bytes: size,
			inline_bytes: size,
			artifact: Some(artifact),
			..Default::default()
		}),
		..Default::default()
	}
}

async fn next(stream: &mut ExternalDispatchStream) -> ExternalDispatchEvent {
	tokio::time::timeout(WAIT, stream.next()).await.expect("bounded terminal").expect("terminal event")
}

#[tokio::test]
async fn detached_terminal_preserves_job_artifact_and_disarms_invocation_cleanup() {
	let scratch = tempfile::tempdir().expect("tempdir");
	let store = BlobStore::open(scratch.path()).expect("CAS");
	let (mut stream, transport, _client) = executor(store.clone(), CancellationToken::new());
	let id = start(&mut stream, &transport).await;
	let expected = job();
	let bytes = Bytes::from(serde_json::to_vec(&ToolTerminal::<(), ()>::Detached(expected.clone())).expect("terminal"));
	send(&transport, id, server_frame::Body::Verdict(verdict(bytes.clone()))).await;
	let ExternalDispatchEvent::DetachedProjected { job, parts, source_artifact, projection } = next(&mut stream).await else { panic!("expected detached terminal") };
	assert_eq!(job, expected);
	assert_eq!(store.get(&source_artifact).expect("original descriptor"), bytes);
	assert_eq!(parts, vec![omp_tool::Part::Text { text: Str::new_static("detached job job-a") }]);
	assert!(!projection.omitted);
	assert!(stream.next().await.is_none());
	drop(stream);
	assert!(tokio::time::timeout(Duration::from_millis(25), transport.recv()).await.is_err(), "terminal ownership transfer must not cancel the job");
}

#[tokio::test]
async fn detached_terminal_replicates_omitted_descriptor_under_original_invocation_scope() {
	let scratch = tempfile::tempdir().expect("tempdir");
	let store = BlobStore::open(scratch.path()).expect("CAS");
	let (mut stream, transport, _client) = executor(store.clone(), CancellationToken::new());
	let id = start(&mut stream, &transport).await;
	let bytes = Bytes::from(serde_json::to_vec(&ToolTerminal::<(), ()>::Detached(job())).expect("terminal"));
	let mut response = verdict(bytes.clone());
	response.json = Bytes::new();
	let projection = response.projection.as_mut().expect("projection");
	projection.inline_bytes = 0;
	projection.omitted = true;
	send(&transport, id, server_frame::Body::Verdict(response)).await;
	let request = tokio::select! {
		request = receive(&transport) => request,
		_ = stream.next() => panic!("terminal before replication"),
	};
	assert!(matches!(request.body, Some(client_frame::Body::BlobGet(_))));
	let scope = request.scope.expect("invocation scope");
	assert_eq!((scope.session_id.as_str(), scope.invocation_id.as_str()), ("session-a", "call-a"));
	let hash = Bytes::copy_from_slice(Hash32::sum(&bytes).as_bytes());
	send(&transport, request.request_id, server_frame::Body::BlobChunk(omp_env::blob_frame::Chunk { data: bytes.clone(), hash: hash.clone(), size: Some(bytes.len() as u64) })).await;
	send(&transport, request.request_id, server_frame::Body::BlobGetComplete(frame::BlobGetComplete { hash, bytes_sent: bytes.len() as u64, ..Default::default() })).await;
	let ExternalDispatchEvent::DetachedProjected { source_artifact, projection, .. } = next(&mut stream).await else { panic!("expected detached terminal") };
	assert_eq!(store.get(&source_artifact).expect("replica"), bytes);
	assert!(projection.omitted);
}

#[tokio::test]
async fn malformed_and_stale_detached_descriptors_never_become_successful_results() {
	let mut stale = job();
	Arc::make_mut(&mut stale.metadata).owner_session = Some(Str::new_static("different-session"));
	let mut generation = job();
	generation.owner = JobOwner::NamedProcess { name: Str::new_static("bash-bg-1"), generation: 0 };
	let mut settled = job();
	Arc::make_mut(&mut settled.metadata).status = omp_tool::JobStatus::Completed;
	let descriptors = [
		Bytes::from_static(br#"{"kind":"detached","id":"job-a"}"#),
		Bytes::from(serde_json::to_vec(&ToolTerminal::<(), ()>::Detached(stale)).expect("stale")),
		Bytes::from(serde_json::to_vec(&ToolTerminal::<(), ()>::Detached(generation)).expect("generation")),
		Bytes::from(serde_json::to_vec(&ToolTerminal::<(), ()>::Detached(settled)).expect("settled")),
	];
	for bytes in descriptors {
		let scratch = tempfile::tempdir().expect("tempdir");
		let (mut stream, transport, _client) = executor(BlobStore::open(scratch.path()).expect("CAS"), CancellationToken::new());
		let id = start(&mut stream, &transport).await;
		send(&transport, id, server_frame::Body::Verdict(verdict(bytes))).await;
		assert!(matches!(next(&mut stream).await, ExternalDispatchEvent::Aborted(omp_tool::Abort::EffectsUnknown { .. })));
		assert!(stream.next().await.is_none());
	}
}

#[tokio::test]
async fn detached_verdict_with_stale_invocation_or_error_flag_is_rejected() {
	for stale_invocation in [true, false] {
		let scratch = tempfile::tempdir().expect("tempdir");
		let (mut stream, transport, _client) = executor(BlobStore::open(scratch.path()).expect("CAS"), CancellationToken::new());
		let id = start(&mut stream, &transport).await;
		let mut response = verdict(Bytes::from(serde_json::to_vec(&ToolTerminal::<(), ()>::Detached(job())).expect("terminal")));
		if stale_invocation { response.invocation_id = "other-call".to_owned(); } else { response.is_error = true; }
		send(&transport, id, server_frame::Body::Verdict(response)).await;
		assert!(matches!(next(&mut stream).await, ExternalDispatchEvent::Aborted(omp_tool::Abort::EffectsUnknown { .. })));
	}
}

#[tokio::test]
async fn dropping_unsettled_executor_cancels_only_its_invocation_once() {
	let scratch = tempfile::tempdir().expect("tempdir");
	let (mut stream, transport, _client) = executor(BlobStore::open(scratch.path()).expect("CAS"), CancellationToken::new());
	let id = start(&mut stream, &transport).await;
	drop(stream);
	let request = receive(&transport).await;
	assert!(matches!(request.body, Some(client_frame::Body::Cancel(frame::CancelRequest { target: Some(frame::cancel_request::Target::TargetRequestId(target)), .. })) if target == id));
	assert!(tokio::time::timeout(Duration::from_millis(25), transport.recv()).await.is_err());
}

#[tokio::test]
async fn cancellation_forwards_interrupt_and_drop_releases_pending_invocation() {
	let scratch = tempfile::tempdir().expect("tempdir");
	let cancellation = CancellationToken::new();
	let (mut stream, transport, _client) = executor(BlobStore::open(scratch.path()).expect("CAS"), cancellation.clone());
	let id = start(&mut stream, &transport).await;
	cancellation.cancel();
	let interrupt = tokio::select! {
		request = receive(&transport) => request,
		_ = stream.next() => panic!("executor must await owner settlement after interrupt"),
	};
	assert_eq!(interrupt.request_id, id);
	assert!(matches!(&interrupt.body, Some(client_frame::Body::Interrupt(interrupt)) if interrupt.invocation_id == "call-a"));
	drop(stream);
	assert!(matches!(receive(&transport).await.body, Some(client_frame::Body::Cancel(_))));
}

#[tokio::test]
async fn cancellation_during_detached_artifact_download_cleans_transfer_not_job() {
	let scratch = tempfile::tempdir().expect("tempdir");
	let cancellation = CancellationToken::new();
	let (mut stream, transport, _client) = executor(BlobStore::open(scratch.path()).expect("CAS"), cancellation.clone());
	let id = start(&mut stream, &transport).await;
	let bytes = Bytes::from(serde_json::to_vec(&ToolTerminal::<(), ()>::Detached(job())).expect("terminal"));
	let mut response = verdict(bytes);
	response.json = Bytes::new();
	let projection = response.projection.as_mut().expect("projection");
	projection.inline_bytes = 0;
	projection.omitted = true;
	send(&transport, id, server_frame::Body::Verdict(response)).await;
	let download = tokio::select! {
		request = receive(&transport) => request,
		_ = stream.next() => panic!("terminal before replication"),
	};
	assert!(matches!(download.body, Some(client_frame::Body::BlobGet(_))));
	cancellation.cancel();
	assert!(matches!(next(&mut stream).await, ExternalDispatchEvent::Aborted(omp_tool::Abort::Interrupted { .. })));
	drop(stream);
	let cancelled = receive(&transport).await;
	assert!(matches!(cancelled.body, Some(client_frame::Body::Cancel(frame::CancelRequest { target: Some(frame::cancel_request::Target::TargetRequestId(target)), .. })) if target == download.request_id));
	assert!(tokio::time::timeout(Duration::from_millis(25), transport.recv()).await.is_err(), "descriptor download cleanup cannot cancel already-detached work");
}
