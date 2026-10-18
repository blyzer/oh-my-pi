//! SSH execution backend through envd's native SSH authority.

use std::{future::Future, sync::Arc};

use omp_adw::Posture;
use omp_core::Str;
use omp_envd::ssh::SshService;
use omp_exec::{
	BackendCapabilities, BackendId, CacheSupport, CancelOutcome, CancelSupport, ExecutionBackend,
	ExecutionError, ExecutionEvent, ExecutionId, ExecutionRequest, ExecutionResult, ExecutionRun,
	Outcome, OutputFrame, Reattach, Stream, Timing, TransferCounts,
};
use thiserror::Error;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

/// Configuration for one SSH backend.
#[derive(Clone)]
pub struct Config {
	/// Backend identity used in placement and results.
	pub id:               BackendId,
	/// envd configured-host alias.
	pub alias:            Str,
	/// Native SSH authority.
	pub service:          SshService,
	/// Platform offered by the remote host.
	pub platform:         omp_exec::Platform,
	/// Posture enforced by the remote host policy.
	pub enforced:         Posture,
	/// Maximum combined stdout/stderr bytes requested from envd.
	pub max_output_bytes: usize,
}

/// SSH execution backend.
#[derive(Clone)]
pub struct SshBackend {
	config:       Arc<Config>,
	capabilities: BackendCapabilities,
}

impl SshBackend {
	/// Constructs an SSH backend over an envd-owned native SSH service.
	#[must_use]
	pub fn new(config: Config) -> Self {
		let capabilities = BackendCapabilities {
			platforms:      Arc::new([config.platform]),
			needs:          omp_exec::NeedSet::new(),
			max_timeout:    None,
			action_cache:   CacheSupport::None,
			cancel:         CancelSupport::BestEffort,
			max_blob_bytes: Some(config.max_output_bytes as u64),
			warm_caches:    false,
			enforced:       config.enforced,
		};
		Self { config: Arc::new(config), capabilities }
	}
}

impl ExecutionBackend for SshBackend {
	type Run = SshRun;

	fn id(&self) -> &BackendId<str> {
		&self.config.id
	}

	fn capabilities(&self) -> &BackendCapabilities {
		&self.capabilities
	}

	fn start(
		&self,
		request: ExecutionRequest,
	) -> impl Future<Output = Result<Self::Run, ExecutionError>> + Send + '_ {
		async move {
			let command = quote_argv(&request.argv).map_err(config_error)?;
			let (events, receiver) = flume::bounded(16);
			let cancellation = CancellationToken::new();
			let task = tokio::spawn(run_remote(
				self.config.service.clone(),
				self.config.alias.clone(),
				self.config.id.clone(),
				request.id.clone(),
				command,
				self.config.max_output_bytes,
				events,
				cancellation.clone(),
			));
			Ok(SshRun {
				events: receiver,
				task,
				cancellation,
				backend: self.config.id.clone(),
				id: request.id,
			})
		}
	}

	fn reattach(
		&self,
		_handle: Reattach,
	) -> impl Future<Output = Result<Self::Run, ExecutionError>> + Send + '_ {
		async move { Err(config_error(SshError::ReattachUnsupported)) }
	}
}

/// One SSH run. Dropping or cancelling aborts the authority task.
pub struct SshRun {
	events:       flume::Receiver<ExecutionEvent>,
	task:         JoinHandle<()>,
	cancellation: CancellationToken,
	backend:      BackendId,
	id:           ExecutionId,
}

impl ExecutionRun for SshRun {
	fn next_event(&mut self) -> impl Future<Output = Option<ExecutionEvent>> + Send + '_ {
		async move { self.events.recv_async().await.ok() }
	}

	fn cancel(&self) -> impl Future<Output = CancelOutcome> + Send + '_ {
		async move {
			self.cancellation.cancel();
			self.task.abort();
			CancelOutcome::Requested
		}
	}

	fn detach(self) -> Reattach {
		Reattach {
			backend:   self.backend.clone(),
			operation: Str::new("ssh"),
			id:        self.id.clone(),
		}
	}
}

impl Drop for SshRun {
	fn drop(&mut self) {
		self.cancellation.cancel();
		self.task.abort();
	}
}

async fn run_remote(
	service: SshService,
	alias: Str,
	backend: BackendId,
	id: ExecutionId,
	command: String,
	max_output_bytes: usize,
	events: flume::Sender<ExecutionEvent>,
	cancellation: CancellationToken,
) {
	let _ = events.send_async(ExecutionEvent::Queued).await;
	if cancellation.is_cancelled() {
		return;
	}
	let operation = Str::from(format!("ssh: {alias}"));
	let _ = events
		.send_async(ExecutionEvent::Accepted { operation })
		.await;
	let _ = events.send_async(ExecutionEvent::Executing).await;
	let result = service
		.exec(alias.as_str(), &command, max_output_bytes)
		.await;
	if cancellation.is_cancelled() {
		return;
	}
	match result {
		Ok(output) => {
			if !output.stdout.is_empty() {
				let _ = events
					.send_async(ExecutionEvent::Output(OutputFrame {
						stream: Stream::Stdout,
						bytes:  output.stdout.clone(),
					}))
					.await;
			}
			if !output.stderr.is_empty() {
				let _ = events
					.send_async(ExecutionEvent::Output(OutputFrame {
						stream: Stream::Stderr,
						bytes:  output.stderr.clone(),
					}))
					.await;
			}
			let code = output.exit_status.map_or(1, |status| status as i32);
			let _ = events
				.send_async(ExecutionEvent::Settled(Ok(ExecutionResult {
					id,
					backend,
					outcome: Outcome::Exited { code },
					logs: None,
					artifacts: Arc::from([]),
					output_digests: std::collections::BTreeMap::new(),
					timing: Timing::default(),
					transfer: TransferCounts::default(),
					cache_hit: false,
					fallback: None,
				})))
				.await;
		},
		Err(error) => {
			let _ = events
				.send_async(ExecutionEvent::Settled(Err(ExecutionError::Infrastructure {
					source: Arc::new(error),
				})))
				.await;
		},
	}
}

fn quote_argv(argv: &[Str]) -> Result<String, SshError> {
	if argv.is_empty() {
		return Err(SshError::EmptyArgv);
	}
	let mut command = String::new();
	for (index, arg) in argv.iter().enumerate() {
		if index != 0 {
			command.push(' ');
		}
		command.push('\'');
		for (part_index, part) in arg.split("'").enumerate() {
			if part_index != 0 {
				command.push_str("'\\''");
			}
			command.push_str(part.as_str());
		}
		command.push('\'');
	}
	Ok(command)
}

fn config_error(error: SshError) -> ExecutionError {
	ExecutionError::Config { source: Arc::new(error) }
}

#[derive(Debug, Error)]
enum SshError {
	#[error("SSH execution argv is empty")]
	EmptyArgv,
	#[error("SSH execution reattach is unsupported")]
	ReattachUnsupported,
}
