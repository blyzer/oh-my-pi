//! Local `omp-exec` backend through envd's process authority.

use std::{
	future::Future,
	path::{Component, Path, PathBuf},
	sync::Arc,
	time::Duration,
};

use omp_adw::Posture;
use omp_core::{Str, encoding::hex};
use omp_envd::exec::{ExecError, ExecEvent, ExecHost, ExecRun};
use omp_exec::{
	BackendCapabilities, BackendId, CacheSupport, CancelOutcome, CancelSupport, ExecutionBackend,
	ExecutionError, ExecutionEvent, ExecutionId, ExecutionRequest, ExecutionResult, ExecutionRun,
	Outcome, OutputFrame, OutputSpec, ReattachHandle, ResumeSupport, Stream, Timing, TransferCounts,
};
use omp_proto::env::v1::{
	EnvironmentDelta, ExecOutcome, ExecRequest, OpenSessionRequest, OutputRequest, Script,
};
use thiserror::Error;
use url::Url;

/// Configuration for one local backend.
#[derive(Clone)]
pub struct Config {
	/// Operator-visible backend identity.
	pub id:       BackendId,
	/// Root used to resolve request-relative working directories.
	pub root:     PathBuf,
	/// Environment process authority.
	pub host:     ExecHost,
	/// Platform offered by the host.
	pub platform: omp_exec::Platform,
	/// Posture enforced by the host.
	pub enforced: Posture,
}

/// Local backend backed by envd's in-process `ExecHost`.
#[derive(Clone)]
pub struct LocalBackend {
	config:       Arc<Config>,
	capabilities: BackendCapabilities,
}

impl LocalBackend {
	/// Creates a local backend from an already-composed environment host.
	#[must_use]
	pub fn new(config: Config) -> Self {
		let capabilities = BackendCapabilities {
			platforms:      Arc::new([config.platform]),
			needs:          omp_exec::NeedSet::new(),
			max_timeout:    None,
			action_cache:   CacheSupport::None,
			cancel:         CancelSupport::Confirmed,
			resume:         ResumeSupport::Unsupported,
			max_blob_bytes: None,
			warm_caches:    true,
			enforced:       config.enforced,
		};
		Self { config: Arc::new(config), capabilities }
	}
}

impl ExecutionBackend for LocalBackend {
	type Run = LocalRun;

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
			let cwd = resolve_cwd(&self.config.root, request.cwd.as_str()).map_err(config_error)?;
			let cwd_uri = Url::from_directory_path(&cwd)
				.map_err(|()| config_error(LocalError::InvalidCwd { cwd: request.cwd.clone() }))?
				.to_string();
			let mut environment = EnvironmentDelta::default();
			for entry in request.env.iter() {
				environment
					.set
					.insert(entry.name.to_string(), entry.value.to_string());
			}
			let opened = self
				.config
				.host
				.open_session(OpenSessionRequest {
					cwd_uri,
					env_delta: Some(environment),
					..OpenSessionRequest::default()
				})
				.await
				.map_err(infrastructure)?;
			let output_request = match &request.outputs {
				OutputSpec::Metadata => OutputRequest::Bounded,
				OutputSpec::Logs | OutputSpec::Artifacts { .. } | OutputSpec::All => {
					OutputRequest::Complete
				},
			};
			let script = quote_argv(&request.argv).map_err(config_error)?;
			let (_, run) = self
				.config
				.host
				.exec(
					ExecRequest {
						session: opened.session.clone(),
						source: Some(Script { text: script, ..Script::default() }),
						output_request: output_request as i32,
						..ExecRequest::default()
					},
					Some(request.timeout),
				)
				.await
				.map_err(infrastructure)?;
			Ok(LocalRun {
				host: self.config.host.clone(),
				session: opened.session,
				run,
				backend: self.config.id.clone(),
				id: request.id,
				operation: None,
				state: RunState::Queued,
				timeout: request.timeout,
			})
		}
	}

	fn reattach(
		&self,
		handle: ReattachHandle,
	) -> impl Future<Output = Result<Self::Run, ExecutionError>> + Send + '_ {
		async move {
			let _ = handle;
			Err(config_error(LocalError::ReattachUnsupported))
		}
	}
}

/// Local run state mapped from envd events.
pub struct LocalRun {
	host:      ExecHost,
	session:   bytes::Bytes,
	run:       ExecRun,
	backend:   BackendId,
	id:        ExecutionId,
	operation: Option<Str>,
	state:     RunState,
	timeout:   Duration,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RunState {
	Queued,
	WaitingStart,
	WaitingEvents,
	Settled,
}

impl ExecutionRun for LocalRun {
	fn next_event(&mut self) -> impl Future<Output = Option<ExecutionEvent>> + Send + '_ {
		async move {
			match self.state {
				RunState::Queued => {
					self.state = RunState::WaitingStart;
					return Some(ExecutionEvent::Queued);
				},
				RunState::Settled => return None,
				RunState::WaitingStart | RunState::WaitingEvents => {},
			}
			match self.run.next_event().await? {
				ExecEvent::Started { exec_id } => {
					let operation = Str::from(hex::encode(&exec_id).into_string());
					self.operation = Some(operation.clone());
					self.state = RunState::WaitingEvents;
					Some(ExecutionEvent::Accepted {
						handle: ReattachHandle::new(self.backend.clone(), operation, self.id.clone()),
					})
				},
				ExecEvent::Output(frame) => Some(ExecutionEvent::Output(OutputFrame {
					stream: match omp_proto::env::v1::OutputChannel::try_from(frame.channel)
						.unwrap_or(omp_proto::env::v1::OutputChannel::Unspecified)
					{
						omp_proto::env::v1::OutputChannel::Stdout
						| omp_proto::env::v1::OutputChannel::Pty => Stream::Stdout,
						omp_proto::env::v1::OutputChannel::Stderr => Stream::Stderr,
						omp_proto::env::v1::OutputChannel::Unspecified => Stream::Stdout,
					},
					bytes:  omp_core::CowBytes::from(frame.data.to_vec()),
				})),
				ExecEvent::Exit(exit) => {
					self.state = RunState::Settled;
					let status = exit.status.unwrap_or_default();
					Some(settle(&self.backend, &self.id, self.timeout, status))
				},
			}
		}
	}

	fn cancel(&self) -> impl Future<Output = CancelOutcome> + Send + '_ {
		async move {
			self.run.cancel();
			CancelOutcome::Requested
		}
	}

	fn detach(self) -> ReattachHandle {
		ReattachHandle::new(
			self.backend.clone(),
			self.operation.clone().unwrap_or_default(),
			self.id.clone(),
		)
	}
}

impl Drop for LocalRun {
	fn drop(&mut self) {
		let _ = self.host.close_session(&self.session);
	}
}

fn settle(
	backend: &BackendId<str>,
	id: &ExecutionId,
	timeout: Duration,
	status: omp_proto::env::v1::ExecStatusMsg,
) -> ExecutionEvent {
	let outcome = ExecOutcome::try_from(status.outcome).unwrap_or(ExecOutcome::Unspecified);
	let event = match outcome {
		ExecOutcome::Exited => Ok(ExecutionResult {
			id:             id.clone(),
			backend:        backend.to_owned(),
			outcome:        Outcome::Exited { code: status.exit_code.unwrap_or(1) },
			logs:           None,
			artifacts:      Arc::from([]),
			output_digests: std::collections::BTreeMap::new(),
			timing:         Timing {
				total: status.wall_clock_ms.min(u64::from(u32::MAX)) as u32,
				..Timing::default()
			},
			transfer:       TransferCounts::default(),
			cache_hit:      false,
			fallback:       None,
		}),
		ExecOutcome::Failed => status.exit_code.map_or_else(
			|| Err(ExecutionError::Infrastructure { source: Arc::new(LocalError::Failed) }),
			|code| {
				Ok(ExecutionResult {
					id:             id.clone(),
					backend:        backend.to_owned(),
					outcome:        Outcome::Exited { code },
					logs:           None,
					artifacts:      Arc::from([]),
					output_digests: std::collections::BTreeMap::new(),
					timing:         Timing::default(),
					transfer:       TransferCounts::default(),
					cache_hit:      false,
					fallback:       None,
				})
			},
		),
		ExecOutcome::Timeout => Err(ExecutionError::TimedOut { limit: timeout }),
		ExecOutcome::Cancelled => {
			Err(ExecutionError::Cancelled { remote: omp_exec::RemoteCancel::Confirmed })
		},
		ExecOutcome::Denied | ExecOutcome::Unspecified => {
			Err(ExecutionError::Infrastructure { source: Arc::new(LocalError::Failed) })
		},
	};
	ExecutionEvent::Settled(event)
}

fn resolve_cwd(root: &Path, cwd: &str) -> Result<PathBuf, LocalError> {
	let relative = Path::new(cwd);
	if relative.is_absolute()
		|| relative
			.components()
			.any(|component| component == Component::ParentDir)
	{
		return Err(LocalError::InvalidCwd { cwd: Str::new(cwd) });
	}
	Ok(root.join(relative))
}

fn quote_argv(argv: &[Str]) -> Result<String, LocalError> {
	let Some(program) = argv.first() else {
		return Err(LocalError::EmptyArgv);
	};
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
	if program.is_empty() {
		return Err(LocalError::EmptyArgv);
	}
	Ok(command)
}

fn config_error(error: LocalError) -> ExecutionError {
	ExecutionError::Config { source: Arc::new(error) }
}

fn infrastructure(error: ExecError) -> ExecutionError {
	ExecutionError::Infrastructure { source: Arc::new(error) }
}

#[derive(Debug, Error)]
enum LocalError {
	#[error("local execution argv is empty")]
	EmptyArgv,
	#[error("local execution working directory is invalid: {cwd}")]
	InvalidCwd { cwd: Str },
	#[error("local execution reattach is unsupported")]
	ReattachUnsupported,
	#[error("local execution failed")]
	Failed,
}

#[cfg(test)]
mod tests {
	use omp_adw::{ApprovalScope, NetworkScope, WriteScope};
	use omp_exec::{
		Eligibility, Intent, NeedSet, Platform, PlatformRequirement, ResourceRequirements,
		ToolchainId,
	};

	use super::*;

	#[tokio::test]
	async fn exec_host_run_maps_output_and_exit() {
		let root = tempfile::tempdir().expect("temporary root");
		let backend = LocalBackend::new(Config {
			id:       BackendId::new_static("local"),
			root:     root.path().to_path_buf(),
			host:     ExecHost::new(),
			platform: Platform::LINUX_X86_64,
			enforced: Posture {
				write:    WriteScope::WorkspaceWrite,
				network:  NetworkScope::Unrestricted,
				approval: ApprovalScope::Yolo,
			},
		});
		let request = ExecutionRequest {
			id:          ExecutionId::new_static("local-test"),
			intent:      Intent::Test,
			argv:        Arc::new([Str::new_static("printf"), Str::new_static("local-ok")]),
			cwd:         Str::empty(),
			env:         Arc::new([]),
			inputs:      None,
			outputs:     OutputSpec::Logs,
			toolchain:   ToolchainId::new_static("local"),
			caches:      Arc::new([]),
			platform:    PlatformRequirement {
				os:            Platform::LINUX_X86_64.os,
				arch:          Platform::LINUX_X86_64.arch,
				needs_network: false,
				needs:         NeedSet::new(),
			},
			requirement: Default::default(),
			resources:   ResourceRequirements::default(),
			eligibility: Eligibility::LocalPreferred,
			timeout:     Duration::from_secs(10),
		};
		let mut run = backend.start(request).await.expect("local start");
		let mut output = Vec::new();
		loop {
			match run.next_event().await.expect("local event") {
				ExecutionEvent::Output(frame) => output.extend_from_slice(&frame.bytes),
				ExecutionEvent::Settled(Ok(result)) => {
					assert_eq!(result.outcome, Outcome::Exited { code: 0 });
					break;
				},
				ExecutionEvent::Settled(Err(error)) => panic!("local execution failed: {error}"),
				_ => {},
			}
		}
		assert_eq!(output, b"local-ok");
	}

	#[test]
	fn argv_quoting_preserves_single_quotes() {
		let argv = [Str::new_static("printf"), Str::new_static("a'b")];
		assert_eq!(quote_argv(&argv).expect("quote"), "'printf' 'a'\\''b'");
	}

	#[test]
	fn cwd_rejects_escape() {
		assert!(matches!(
			resolve_cwd(Path::new("/tmp/root"), "../escape"),
			Err(LocalError::InvalidCwd { .. })
		));
	}
}
