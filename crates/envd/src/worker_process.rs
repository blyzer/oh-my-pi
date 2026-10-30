//! Process-backed named-worker authority for the envd worker CONTROL surface.

use std::{
	collections::BTreeMap,
	fs,
	path::PathBuf,
	sync::Arc,
	time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use omp_core::{Str, Ulid};
use omp_proto::{
	bounds::FRAME_MAX_BYTES,
	prost::Message,
	toolhost::v1::{WorkerFrame, worker_frame},
};
use serde_json::Value;
use tokio::{io::AsyncReadExt, time};
use tokio_util::sync::CancellationToken;

use super::{
	child_tree::ChildTree,
	worker_child::PY_WORKER_ARG,
	worker_pool::{
		SupervisedWorkerProcess, WorkerControlFailure, WorkerObservation, WorkerProcessAuthority,
		WorkerRoute, WorkerSessionEndpoint, WorkerSupervisor,
	},
};

const WORKER_API_LEVEL: u32 = 1;
const WORKER_START_TIMEOUT: Duration = Duration::from_secs(5);
struct Runtime {
	observation: WorkerObservation,
	endpoint:    Option<WorkerSessionEndpoint>,
	cancel:      CancellationToken,
	terminated:  CancellationToken,
}

/// Process-backed authority used by production worker CONTROL owners.
#[derive(Clone)]
pub struct PythonWorkerProcesses {
	supervisor: Arc<WorkerSupervisor>,
	executable: Arc<PathBuf>,
	runtimes:   Arc<tokio::sync::Mutex<BTreeMap<(Str, u64), Runtime>>>,
	starting:   Arc<tokio::sync::Mutex<BTreeMap<(Str, u64), ()>>>,
}

impl PythonWorkerProcesses {
	/// Creates a process authority using the current executable as the child.
	pub fn new(supervisor: Arc<WorkerSupervisor>) -> Self {
		let executable = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("omp"));
		Self::with_executable(supervisor, executable)
	}

	/// Creates a process authority with an explicit executable for joined tests.
	pub fn with_executable(supervisor: Arc<WorkerSupervisor>, executable: PathBuf) -> Self {
		Self {
			supervisor,
			executable: Arc::new(executable),
			runtimes: Arc::new(tokio::sync::Mutex::new(BTreeMap::new())),
			starting: Arc::new(tokio::sync::Mutex::new(BTreeMap::new())),
		}
	}

	async fn ensure_started(
		&self,
		route: &WorkerRoute,
	) -> Result<WorkerObservation, WorkerControlFailure> {
		if let Some(runtime) = self.runtime(route).await {
			if !runtime.terminated.is_cancelled() {
				return Ok(runtime.observation);
			}
			self.remove_runtime(route).await;
		}
		let key = (route.key.name.clone(), route.generation);
		let mut starting = self.starting.lock().await;
		if let Some(runtime) = self.runtime(route).await {
			return Ok(runtime.observation);
		}
		starting.insert(key.clone(), ());
		let result = self.spawn(route).await;
		starting.remove(&key);
		result
	}

	async fn spawn(&self, route: &WorkerRoute) -> Result<WorkerObservation, WorkerControlFailure> {
		let socket_path = std::env::temp_dir().join(format!(
			"omp-worker-{}-{}-{}.sock",
			std::process::id(),
			Ulid::generate(),
			route.generation
		));
		let _ = fs::remove_file(&socket_path);
		let mut command = tokio::process::Command::new(self.executable.as_path());
		command
			.arg(PY_WORKER_ARG)
			.env("OMP_WORKER_ID", route.key.name.as_str())
			.env("OMP_WORKER_LAYER", route.key.extension.as_str())
			.env("OMP_WORKER_TIER", "trusted")
			.env("OMP_WORKER_POOL", "default")
			.env("OMP_WORKER_SOCKET", &socket_path)
			.env("OMP_WORKER_HOST_GENERATION", route.generation.to_string())
			.env("OMP_WORKER_SESSION_GENERATION", "1");
		let spawned = ChildTree::spawn(command).map_err(|_| {
			WorkerControlFailure::Process(Str::new_static("worker process spawn failed"))
		})?;
		let mut stdout = spawned.stdout;
		let hello = time::timeout(WORKER_START_TIMEOUT, read_worker_frame(&mut stdout))
			.await
			.map_err(|_| WorkerControlFailure::Process(Str::new_static("worker hello timed out")))?
			.map_err(|_| WorkerControlFailure::Process(Str::new_static("worker hello failed")))?;
		let Some(worker_frame::Body::Hello(hello)) = hello.body else {
			return Err(WorkerControlFailure::Process(Str::new_static(
				"worker did not send WorkerHello",
			)));
		};
		if hello.schema_rev != omp_proto::SCHEMA_REV
			|| hello.api_level != WORKER_API_LEVEL
			|| hello.worker_id.as_ref() != route.key.name.as_bytes()
		{
			return Err(WorkerControlFailure::Process(Str::new_static(
				"worker hello identity mismatch",
			)));
		}
		let started = Instant::now();
		while !socket_path.exists() {
			if started.elapsed() >= WORKER_START_TIMEOUT {
				return Err(WorkerControlFailure::Process(Str::new_static(
					"worker session socket timed out",
				)));
			}
			time::sleep(Duration::from_millis(10)).await;
		}
		drop(stdout);
		drop(spawned.stderr);
		let endpoint = WorkerSessionEndpoint {
			generation: route.generation,
			family:     Str::new_static("unix"),
			address:    Value::String(socket_path.to_string_lossy().into_owned()),
			authkey:    None,
		};
		let cancel = CancellationToken::new();
		let terminated = CancellationToken::new();
		let monitor_cancel = cancel.clone();
		let monitor_terminated = terminated.clone();
		let monitor_socket = socket_path.clone();
		let mut tree = spawned.tree;
		let keep_stdin = spawned.stdin;
		tokio::spawn(async move {
			let _keep_stdin = keep_stdin;
			tokio::select! { _ = monitor_cancel.cancelled() => { tree.terminate_tree().await; }, _ = tree.wait() => {}, }
			let _ = fs::remove_file(monitor_socket);
			monitor_terminated.cancel();
		});
		let observation = WorkerObservation {
			name:            route.key.name.clone(),
			generation:      route.generation,
			state:           Str::new_static("ready"),
			site:            Default::default(),
			pid:             None,
			spawned_at_ms:   SystemTime::now()
				.duration_since(UNIX_EPOCH)
				.map_or(0, |value| value.as_millis().try_into().unwrap_or(u64::MAX)),
			last_call_at_ms: None,
			calls:           0,
			in_flight:       0,
			code_cached:     0,
			enforced:        Vec::new(),
			fault:           None,
		};
		let process = SupervisedWorkerProcess {
			observation: observation.clone(),
			endpoint:    Some(endpoint.clone()),
			cancel:      cancel.clone(),
			terminated:  terminated.clone(),
		};
		self
			.supervisor
			.publish_process(route, process)
			.map_err(|_| WorkerControlFailure::StaleConnection)?;
		self
			.runtimes
			.lock()
			.await
			.insert((route.key.name.clone(), route.generation), Runtime {
				observation: observation.clone(),
				endpoint: Some(endpoint),
				cancel,
				terminated,
			});
		Ok(observation)
	}

	async fn runtime(&self, route: &WorkerRoute) -> Option<Runtime> {
		self
			.runtimes
			.lock()
			.await
			.get(&(route.key.name.clone(), route.generation))
			.cloned()
	}

	async fn remove_runtime(&self, route: &WorkerRoute) {
		self
			.runtimes
			.lock()
			.await
			.remove(&(route.key.name.clone(), route.generation));
		self.supervisor.retire_process(route);
	}
}
impl Clone for Runtime {
	fn clone(&self) -> Self {
		Self {
			observation: self.observation.clone(),
			endpoint:    self.endpoint.clone(),
			cancel:      self.cancel.clone(),
			terminated:  self.terminated.clone(),
		}
	}
}

#[async_trait::async_trait]
impl WorkerProcessAuthority for PythonWorkerProcesses {
	async fn ensure(
		&self,
		route: &WorkerRoute,
		cancel: CancellationToken,
	) -> Result<WorkerObservation, WorkerControlFailure> {
		if cancel.is_cancelled() {
			return Err(WorkerControlFailure::Process(Str::new_static("worker request cancelled")));
		}
		self.ensure_started(route).await
	}

	async fn observe(&self, route: &WorkerRoute) -> Result<WorkerObservation, WorkerControlFailure> {
		let runtime = self
			.runtime(route)
			.await
			.ok_or(WorkerControlFailure::Evicted)?;
		if runtime.terminated.is_cancelled() {
			let mut observation = runtime.observation;
			observation.state = Str::new_static("failed");
			observation.fault = Some(Str::new_static("worker process exited"));
			return Ok(observation);
		}
		Ok(runtime.observation)
	}

	async fn warm(
		&self,
		route: &WorkerRoute,
		cancel: CancellationToken,
	) -> Result<WorkerObservation, WorkerControlFailure> {
		self.ensure(route, cancel).await
	}

	async fn stop(
		&self,
		route: &WorkerRoute,
		grace: Duration,
		cancel: CancellationToken,
	) -> Result<(), WorkerControlFailure> {
		if cancel.is_cancelled() {
			return Err(WorkerControlFailure::Process(Str::new_static("worker request cancelled")));
		}
		let runtime = self
			.runtime(route)
			.await
			.ok_or(WorkerControlFailure::Evicted)?;
		runtime.cancel.cancel();
		tokio::select! { _ = runtime.terminated.cancelled() => {}, _ = time::sleep(grace) => {}, _ = cancel.cancelled() => return Err(WorkerControlFailure::Process(Str::new_static("worker request cancelled"))), }
		self.remove_runtime(route).await;
		Ok(())
	}

	async fn session(
		&self,
		route: &WorkerRoute,
		cancel: CancellationToken,
	) -> Result<WorkerSessionEndpoint, WorkerControlFailure> {
		if cancel.is_cancelled() {
			return Err(WorkerControlFailure::Process(Str::new_static("worker request cancelled")));
		}
		self
			.runtime(route)
			.await
			.ok_or(WorkerControlFailure::Evicted)?
			.endpoint
			.ok_or_else(|| {
				WorkerControlFailure::Process(Str::new_static("worker session endpoint is unavailable"))
			})
	}
}

async fn read_worker_frame(
	reader: &mut (impl AsyncReadExt + Unpin),
) -> Result<WorkerFrame, std::io::Error> {
	let mut value = 0u64;
	for shift in (0..70).step_by(7) {
		let mut byte = [0u8; 1];
		reader.read_exact(&mut byte).await?;
		value |= u64::from(byte[0] & 0x7f) << shift;
		if byte[0] & 0x80 == 0 {
			break;
		}
	}
	if value > FRAME_MAX_BYTES as u64 {
		return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "worker frame too large"));
	}
	let mut bytes = vec![
		0;
		usize::try_from(value).map_err(|_| std::io::Error::new(
			std::io::ErrorKind::InvalidData,
			"worker frame too large"
		))?
	];
	reader.read_exact(&mut bytes).await?;
	WorkerFrame::decode(bytes.as_slice()).map_err(std::io::Error::other)
}
