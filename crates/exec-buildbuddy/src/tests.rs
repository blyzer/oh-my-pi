//! Protocol proof over a real loopback HTTP server.

use std::{
	collections::BTreeMap,
	sync::{
		Arc,
		atomic::{AtomicUsize, Ordering},
	},
	time::Duration,
};

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{Request, Response, body::Incoming, server::conn::http1, service::service_fn};
use hyper_util::rt::TokioIo;
use omp_adw::{ApprovalScope, NetworkScope, Posture, WriteScope};
use omp_core::{Str, base64};
use omp_exec::{
	BackendId, CacheDecl, Eligibility, EnvEntry, ExecutionBackend, ExecutionEvent, ExecutionId,
	ExecutionRequest, ExecutionRun, Intent, NeedSet, Outcome, OutputSpec, Platform,
	PlatformRequirement, ResourceRequirements, ToolchainId,
};
use serde_json::{Value, json};
use tokio::{net::TcpListener, sync::Mutex};

use super::{BuildBuddyBackend, Config, PreparedInputSource, RunInput};

fn request() -> ExecutionRequest {
	ExecutionRequest {
		id:          ExecutionId::new_static("run-test"),
		intent:      Intent::Test,
		argv:        Arc::new([
			Str::new_static("just"),
			Str::new_static("test-pkg"),
			Str::new_static("a'b"),
		]),
		cwd:         Str::new_static("subdirectory"),
		env:         Arc::<[EnvEntry]>::from([]),
		inputs:      None,
		outputs:     OutputSpec::Logs,
		toolchain:   ToolchainId::new_static("test"),
		caches:      Arc::<[CacheDecl]>::from([]),
		platform:    PlatformRequirement {
			os:            Platform::LINUX_X86_64.os,
			arch:          Platform::LINUX_X86_64.arch,
			needs_network: false,
			needs:         NeedSet::new(),
		},
		requirement: Default::default(),
		resources:   ResourceRequirements::default(),
		eligibility: Eligibility::RemotePreferred,
		timeout:     Duration::from_secs(20),
	}
}

#[tokio::test]
async fn run_submits_binary_patch_and_polls_to_normalized_failure_with_paginated_logs() {
	let listener = TcpListener::bind("127.0.0.1:0").await.expect("listen");
	let port = listener.local_addr().expect("address").port();
	let submitted = Arc::new(Mutex::new(None::<Value>));
	let polls = Arc::new(AtomicUsize::new(0));
	let submitted_server = Arc::clone(&submitted);
	let polls_server = Arc::clone(&polls);
	let server = tokio::spawn(async move {
		loop {
			let (stream, _) = listener.accept().await.expect("accept");
			let submitted = Arc::clone(&submitted_server);
			let polls = Arc::clone(&polls_server);
			tokio::spawn(async move {
				let serve = service_fn(move |request: Request<Incoming>| {
					let submitted = Arc::clone(&submitted);
					let polls = Arc::clone(&polls);
					async move {
						assert_eq!(request.headers()["x-buildbuddy-api-key"], "test-key");
						let path = request.uri().path().to_owned();
						let body = request
							.into_body()
							.collect()
							.await
							.expect("body")
							.to_bytes();
						let json: Value = serde_json::from_slice(&body).expect("json");
						let reply = match path.as_str() {
							"/api/v1/Run" => {
								*submitted.lock().await = Some(json);
								json!({"invocationId": "invocation-1"})
							},
							"/api/v1/GetInvocation" => {
								assert_eq!(json["selector"]["invocationId"], "invocation-1");
								let status = if polls.fetch_add(1, Ordering::SeqCst) == 0 {
									"PARTIAL_INVOCATION_STATUS"
								} else {
									"COMPLETE_INVOCATION_STATUS"
								};
								json!({"invocation": [{"success": false, "invocationStatus": status}]})
							},
							"/api/v1/GetLog" => {
								assert_eq!(json["selector"]["invocationId"], "invocation-1");
								if json["pageToken"] == "" {
									json!({"log": {"contents": "first\n"}, "nextPageToken": "p2"})
								} else {
									json!({"log": {"contents": "second\n"}})
								}
							},
							_ => panic!("unexpected path {path}"),
						};
						Ok::<_, std::convert::Infallible>(Response::new(Full::new(Bytes::from(
							reply.to_string(),
						))))
					}
				});
				http1::Builder::new()
					.serve_connection(TokioIo::new(stream), serve)
					.await
					.expect("serve HTTP");
			});
		}
	});
	let input = RunInput {
		repo:       Str::new_static("https://github.com/example/repo"),
		commit_sha: Str::new_static("0123456789abcdef"),
		branch:     Some(Str::new_static("feature")),
		patches:    Arc::new([Bytes::from_static(b"diff --git a/x b/x\n")]),
	};
	let backend = BuildBuddyBackend::new(
		Config {
			id: BackendId::new_static("buildbuddy-run"),
			endpoint: Str::new(format!("http://127.0.0.1:{port}")),
			credential: "test-key".into(),
			platforms: Arc::new([Platform::LINUX_X86_64]),
			enforced: Posture {
				write:    WriteScope::ReadOnly,
				network:  NetworkScope::Disabled,
				approval: ApprovalScope::Yolo,
			},
			platform_properties: BTreeMap::new(),
			#[cfg(feature = "buildbuddy-cancel")]
			canceller: None,
		},
		PreparedInputSource(input),
	)
	.expect("backend");
	let mut run = backend.start(request()).await.expect("start");
	let events = tokio::time::timeout(Duration::from_secs(10), async {
		let mut events = Vec::new();
		while let Some(event) = run.next_event().await {
			events.push(event);
		}
		events
	})
	.await
	.expect("run deadline");
	assert!(
		matches!(&events[0], ExecutionEvent::Accepted { operation } if operation == "invocation-1")
	);
	assert!(matches!(&events[1], ExecutionEvent::Executing));
	assert!(
		matches!(&events[4], ExecutionEvent::Settled(Ok(result)) if result.outcome == Outcome::Exited { code: 1 })
	);
	let sent = submitted.lock().await.clone().expect("submitted request");
	assert_eq!(sent["repo"], "https://github.com/example/repo");
	assert_eq!(sent["commitSha"], "0123456789abcdef");
	assert_eq!(sent["patches"][0], base64::encode(b"diff --git a/x b/x\n").into_string());
	assert_eq!(sent["steps"][0]["run"], "cd -- 'subdirectory' && 'just' 'test-pkg' 'a'\\''b'");
	server.abort();
}
