use std::{
	collections::BTreeMap,
	error::Error,
	io,
	sync::{
		Arc,
		atomic::{AtomicBool, Ordering},
	},
	time::{Duration, Instant},
};

use bytes::Bytes;
use omp_adw::{ApprovalScope, NetworkScope, Posture, WriteScope};
use omp_core::{Hash32, Str};
use omp_exec::{
	BackendId, CachePolicy, CancelOutcome, Digest, Eligibility, ExclusionCounts, ExecutionBackend,
	ExecutionError, ExecutionEvent, ExecutionId, ExecutionRequest, ExecutionResult, ExecutionRun,
	InputSet, Intent, NeedSet, Outcome, OutputSpec, Platform, PlatformRequirement, ReattachContext,
	ReattachHandle, ResourceRequirements, ToolchainId,
};
use omp_exec_reapi::{
	ExecuteConfig, ReapiBackend,
	cas::{BlobSource, InputSource},
	endpoint::Endpoint,
	profile::{Credential, Profile},
	proto::build::bazel::remote::execution::v2 as wire,
	tree::InputFile,
};

const ENDPOINT: &str = "grpcs://remote.buildbuddy.io";
static STOP_REMOTE: AtomicBool = AtomicBool::new(false);

#[derive(Clone)]
struct EmptyInputs;

impl BlobSource for EmptyInputs {
	async fn read(&self, _: Digest) -> io::Result<Bytes> {
		Err(io::Error::new(io::ErrorKind::NotFound, "the input set is empty"))
	}

	async fn read_range(&self, _: Digest, _: u64, _: u64) -> io::Result<Bytes> {
		Err(io::Error::new(io::ErrorKind::NotFound, "the input set is empty"))
	}
}

impl InputSource for EmptyInputs {
	async fn files(&self, _: Digest) -> io::Result<Vec<InputFile>> {
		Ok(Vec::new())
	}
}

type Backend = ReapiBackend<EmptyInputs>;
type Run = <Backend as ExecutionBackend>::Run;

fn credential() -> Option<Credential> {
	assert!(
		!STOP_REMOTE.load(Ordering::SeqCst),
		"not attempted because an earlier test observed an account or executor blocker"
	);
	match std::env::var("BUILDBUDDY_API_KEY") {
		Ok(value) if !value.is_empty() => Some(Credential::new(value)),
		_ => {
			eprintln!("SKIP: BUILDBUDDY_API_KEY is missing or empty");
			None
		},
	}
}

fn redact(value: &str) -> String {
	match std::env::var("BUILDBUDDY_API_KEY") {
		Ok(secret) if !secret.is_empty() => value.replace(&secret, "[REDACTED]"),
		_ => value.to_owned(),
	}
}

fn fail_execution(phase: &str, error: ExecutionError) -> ! {
	let mut blocked = matches!(error, ExecutionError::Auth { .. } | ExecutionError::Capacity { .. });
	let mut source: Option<&(dyn Error + 'static)> = Some(&error);
	while let Some(cause) = source {
		if let Some(status) = cause.downcast_ref::<tonic::Status>() {
			blocked |= matches!(
				status.code(),
				tonic::Code::Unauthenticated
					| tonic::Code::PermissionDenied
					| tonic::Code::ResourceExhausted
			);
			let message = status.message().to_ascii_lowercase();
			blocked |= message.contains("no executor") || message.contains("no available executor");
		}
		source = cause.source();
	}
	if blocked {
		STOP_REMOTE.store(true, Ordering::SeqCst);
	}
	panic!("{phase}: {}", redact(&format!("{error:?}")));
}

fn config(credential: Credential) -> ExecuteConfig {
	ExecuteConfig {
		id: BackendId::new_static("live-buildbuddy"),
		instance: Str::empty(),
		profile: Profile::buildbuddy(),
		credential: Some(credential),
		platform: Platform::LINUX_X86_64,
		enforced: Posture {
			write:    WriteScope::ReadOnly,
			network:  NetworkScope::Unrestricted,
			approval: ApprovalScope::Yolo,
		},
		cache: CachePolicy::Off,
		platform_properties: BTreeMap::new(),
		toolchains: BTreeMap::new(),
		#[cfg(feature = "buildbuddy-cancel")]
		buildbuddy_canceller: None,
	}
}

async fn connect(credential: Credential) -> Backend {
	ReapiBackend::connect(
		Endpoint::parse(ENDPOINT).expect("valid endpoint"),
		config(credential),
		EmptyInputs,
	)
	.await
	.unwrap_or_else(|error| fail_execution("connect", error))
}

fn request(id: &'static str, script: &'static str, timeout: Duration) -> ExecutionRequest {
	ExecutionRequest {
		id: ExecutionId::new_static(id),
		intent: Intent::Test,
		argv: Arc::new([Str::new_static("/bin/sh"), Str::new_static("-c"), Str::new_static(script)]),
		cwd: Str::empty(),
		env: Arc::new([]),
		inputs: Some(InputSet {
			manifest: Digest::new(Hash32::sum(b""), 0),
			files:    0,
			bytes:    0,
			excluded: ExclusionCounts { rules: Arc::new([]), symlinks: 0 },
		}),
		outputs: OutputSpec::Logs,
		toolchain: ToolchainId::new_static("live-test"),
		caches: Arc::new([]),
		platform: PlatformRequirement {
			os:            Platform::LINUX_X86_64.os,
			arch:          Platform::LINUX_X86_64.arch,
			needs_network: false,
			needs:         NeedSet::new(),
		},
		requirement: Default::default(),
		resources: ResourceRequirements::default(),
		eligibility: Eligibility::RemotePreferred,
		timeout,
	}
}

async fn start(backend: &Backend, request: ExecutionRequest) -> Run {
	match tokio::time::timeout(Duration::from_secs(45), backend.start(request)).await {
		Ok(Ok(run)) => run,
		Ok(Err(error)) => fail_execution("start", error),
		Err(_) => {
			STOP_REMOTE.store(true, Ordering::SeqCst);
			panic!("start did not return within 45 seconds; acceptance is unknown");
		},
	}
}

async fn accepted_and_executing(run: &mut Run) -> ReattachHandle {
	let observed = tokio::time::timeout(Duration::from_secs(45), async {
		let mut accepted = None;
		loop {
			match run.next_event().await {
				Some(ExecutionEvent::Accepted { handle }) => accepted = Some(handle),
				Some(ExecutionEvent::Executing) => {
					return accepted.expect("execution before acceptance");
				},
				Some(ExecutionEvent::Settled(Err(error))) => fail_execution("before execution", error),
				Some(ExecutionEvent::Settled(Ok(_))) => {
					panic!("action settled before it was observed executing")
				},
				Some(_) => {},
				None => panic!("stream ended before execution"),
			}
		}
	})
	.await;
	match observed {
		Ok(handle) => handle,
		Err(_) => {
			STOP_REMOTE.store(true, Ordering::SeqCst);
			panic!("action did not reach executing within 45 seconds");
		},
	}
}

async fn settle(run: &mut Run) -> Result<ExecutionResult, ExecutionError> {
	loop {
		match run.next_event().await {
			Some(ExecutionEvent::Settled(result)) => return result,
			Some(_) => {},
			None => panic!("stream ended without a terminal event"),
		}
	}
}

async fn assert_stdout(backend: &Backend, result: &ExecutionResult, expected: &[u8]) {
	let logs = result
		.logs
		.expect("execution returned no stdout/stderr digests");
	let stdout = backend
		.download_blob(logs.stdout)
		.await
		.unwrap_or_else(|error| fail_execution("stdout", error));
	assert!(stdout.as_ref() == expected, "stdout did not match expected bytes");
}

#[tokio::test]
#[ignore = "live: needs BUILDBUDDY_API_KEY"]
async fn live_capabilities() {
	let Some(credential) = credential() else {
		return;
	};
	let started = Instant::now();
	let endpoint = Endpoint::parse(ENDPOINT).expect("valid endpoint");
	let channel = tonic::transport::Endpoint::from_shared(endpoint.transport_url())
		.expect("valid transport URL")
		.connect_timeout(Duration::from_secs(30))
		.tls_config(tonic::transport::ClientTlsConfig::new().with_webpki_roots())
		.expect("valid TLS configuration")
		.connect()
		.await
		.expect("TLS connection");
	let mut request =
		tonic::Request::new(wire::GetCapabilitiesRequest { instance_name: String::new() });
	request
		.metadata_mut()
		.insert("x-buildbuddy-api-key", credential.expose().parse().expect("valid API-key metadata"));
	let raw = match wire::capabilities_client::CapabilitiesClient::new(channel)
		.get_capabilities(request)
		.await
	{
		Ok(response) => response.into_inner(),
		Err(status) => {
			STOP_REMOTE.store(true, Ordering::SeqCst);
			panic!("GetCapabilities {:?}: {}", status.code(), redact(status.message()));
		},
	};
	println!(
		"GetCapabilities OK, low_api={:?}, high_api={:?}",
		raw.low_api_version, raw.high_api_version
	);
	let backend = connect(credential).await;
	let capabilities = backend.reapi_capabilities();
	assert!(capabilities.execution, "server did not advertise execution");
	println!("ReapiBackend connected in {:?}; capabilities={capabilities:?}", started.elapsed());
	println!("backend_max_timeout={:?}", backend.capabilities().max_timeout);
}

#[tokio::test]
#[ignore = "live: needs BUILDBUDDY_API_KEY"]
async fn live_execute_echo() {
	let Some(credential) = credential() else {
		return;
	};
	let started = Instant::now();
	let backend = connect(credential).await;
	let mut run =
		start(&backend, request("live-echo", "echo live-reapi-echo", Duration::from_secs(30))).await;
	let result = settle(&mut run)
		.await
		.unwrap_or_else(|error| fail_execution("echo", error));
	assert_eq!(result.outcome, Outcome::Exited { code: 0 });
	assert_stdout(&backend, &result, b"live-reapi-echo\n").await;
	println!("exit=0 stdout=live-reapi-echo elapsed={:?}", started.elapsed());
}

#[tokio::test]
#[ignore = "live: needs BUILDBUDDY_API_KEY"]
async fn live_execute_nonzero() {
	let Some(credential) = credential() else {
		return;
	};
	let started = Instant::now();
	let backend = connect(credential).await;
	let mut run = start(&backend, request("live-nonzero", "exit 23", Duration::from_secs(30))).await;
	let result = settle(&mut run)
		.await
		.unwrap_or_else(|error| fail_execution("nonzero", error));
	assert_eq!(result.outcome, Outcome::Exited { code: 23 });
	println!("outcome=Exited {{ code: 23 }} elapsed={:?}", started.elapsed());
}

#[tokio::test]
#[ignore = "live: needs BUILDBUDDY_API_KEY"]
async fn live_detach_and_reattach() {
	let Some(credential) = credential() else {
		return;
	};
	let started = Instant::now();
	let backend = connect(credential.clone()).await;
	let mut run =
		start(&backend, request("live-detach", "sleep 25; echo done", Duration::from_secs(60))).await;
	let accepted = accepted_and_executing(&mut run).await;
	let detached_at = Instant::now();
	let handle = run.detach();
	assert_eq!(handle, accepted, "detach changed the accepted handle");
	drop(backend);
	tokio::time::sleep(Duration::from_secs(3)).await;
	let backend = connect(credential).await;
	let mut resumed = backend
		.reattach(handle, ReattachContext {
			outputs:            OutputSpec::Logs,
			remaining_deadline: Duration::from_secs(60).saturating_sub(detached_at.elapsed()),
		})
		.await
		.unwrap_or_else(|error| fail_execution("reattach", error));
	let result = settle(&mut resumed)
		.await
		.unwrap_or_else(|error| fail_execution("reattached run", error));
	assert_eq!(result.outcome, Outcome::Exited { code: 0 });
	assert_stdout(&backend, &result, b"done\n").await;
	assert!(
		detached_at.elapsed() >= Duration::from_secs(15),
		"sleep completed too soon to establish detached execution"
	);
	println!(
		"new connection reattached; exit=0 stdout=done detached_to_settle={:?} elapsed={:?}",
		detached_at.elapsed(),
		started.elapsed()
	);
}

#[tokio::test]
#[ignore = "live: needs BUILDBUDDY_API_KEY"]
async fn live_cancel() {
	let Some(credential) = credential() else {
		return;
	};
	let started = Instant::now();
	let backend = connect(credential).await;
	let mut run =
		start(&backend, request("live-cancel", "sleep 120", Duration::from_secs(60))).await;
	accepted_and_executing(&mut run).await;
	let cancelled_at = Instant::now();
	let outcome = run.cancel().await;
	println!("cancel_outcome={outcome:?} after={:?}", started.elapsed());
	let terminal = settle(&mut run).await;
	match (outcome, terminal) {
		(CancelOutcome::Unsupported, Err(ExecutionError::TimedOut { limit })) => {
			assert_eq!(limit, Duration::from_secs(60));
			println!(
				"unsupported cancel left action until TimedOut {{ limit: 60s }}; after_cancel={:?}",
				cancelled_at.elapsed()
			);
		},
		(
			CancelOutcome::Requested | CancelOutcome::Cancelled,
			Err(ExecutionError::Cancelled { remote }),
		) => {
			println!(
				"action settled Cancelled {{ remote: {remote:?} }}; after_cancel={:?}",
				cancelled_at.elapsed()
			);
		},
		(_, Err(error)) => fail_execution("cancelled run", error),
		(_, Ok(result)) => panic!("cancelled action settled unexpectedly with {:?}", result.outcome),
	}
}
