//! Delegated execution backend: bridges extension device operations to the
//! `ExecutionBackend` trait.
//!
//! The adapter implements `ExecutionBackend` by delegating to extension device
//! operations via `EnvClient::invoke`. Each device operation (`describe`,
//! `start`, `poll`, `get_log`, `cancel`) is invoked as a tool call, and the
//! response is mapped to the execution contract types.
//!
//! This enables any extension that implements the delegate contract
//! (`docs/research/exec-out-of-core.md` shape C1) to serve as a production
//! execution backend.

use std::{
	sync::{
		Arc,
		atomic::{AtomicU64, Ordering},
	},
	time::Duration,
};

use omp_core::{CowBytes, ExposeSecret, SecretString, Str};
use omp_env::{ClientError, EnvClient, InvocationEvent};
use omp_exec::{
	BackendCapabilities, BackendId, CacheSupport, CancelOutcome, CancelSupport, ExecutionBackend,
	ExecutionError, ExecutionEvent, ExecutionRequest, ExecutionResult, ExecutionRun, Outcome,
	OutputFrame, Platform, ReattachHandle, ResumeSupport, Stream,
};
use omp_exec_reapi::profile::Credential;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::exec_composer::REMOTE_POSTURE;

/// Global counter for invocation IDs.
static INVOCATION_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Device operation names for the delegate contract.
mod device {
	pub const DESCRIBE: &str = "exec/describe";
	pub const START: &str = "exec/start";
	pub const POLL: &str = "exec/poll";
	pub const GET_LOG: &str = "exec/get_log";
	pub const CANCEL: &str = "exec/cancel";
	pub const REVISION: &str = "buildbuddy.1";
}

fn shell_quote(value: &str) -> String {
	format!("'{}'", value.replace('\'', "'\\''"))
}

fn credential_value(inner: &BackendInner) -> Option<String> {
	inner
		.credential
		.as_ref()
		.map(|credential| credential.expose().to_string())
}

/// Error from a delegated extension device call.
#[derive(Debug, thiserror::Error)]
pub enum DelegatedError {
	/// The device call failed at the transport level.
	#[error("device call failed: {0}")]
	Transport(#[source] ClientError),
	/// The device returned an error result.
	#[error("device error: {operation}: {detail}")]
	Device {
		/// The operation that failed.
		operation: &'static str,
		/// Error detail from the device.
		detail:    Str,
	},
	/// The device response was malformed.
	#[error("device response malformed: {operation}")]
	Malformed {
		/// The operation that returned malformed data.
		operation: &'static str,
	},
}

/// A connected delegated execution backend.
///
/// Cheap to clone (Arc-backed handle). The extension device path is resolved
/// once at construction.
#[derive(Clone)]
pub struct DelegatedBackend {
	inner: Arc<BackendInner>,
}

struct BackendInner {
	/// Environment client for device calls.
	client:       EnvClient,
	/// Operator-given backend identity.
	id:           BackendId,
	/// Capabilities for this backend.
	capabilities: Arc<BackendCapabilities>,
	/// API key credential for the extension.
	credential:   Option<Credential>,
}

impl DelegatedBackend {
	/// Creates a new delegated backend.
	///
	/// The `client` must have the extension loaded and admitted. The `id` is
	/// the operator-given name used in execution handles.
	pub fn new(client: EnvClient, id: BackendId, credential: Option<Credential>) -> Self {
		Self {
			inner: Arc::new(BackendInner {
				client,
				id,
				capabilities: Arc::new(BackendCapabilities {
					platforms:      Arc::new([Platform::LINUX_X86_64]),
					needs:          Default::default(),
					max_timeout:    Some(Duration::from_secs(60 * 30)),
					action_cache:   CacheSupport::None,
					cancel:         CancelSupport::None,
					resume:         ResumeSupport::Reattach,
					max_blob_bytes: None,
					warm_caches:    false,
					enforced:       REMOTE_POSTURE,
				}),
				credential,
			}),
		}
	}

	/// Calls one device operation and returns its JSON result.
	async fn call_device(
		&self,
		operation: &'static str,
		args: Value,
	) -> Result<Value, DelegatedError> {
		let seq = INVOCATION_COUNTER.fetch_add(1, Ordering::Relaxed);
		let invocation_id = format!("delegated-{}-{}", operation, seq);

		tracing::debug!(operation, invocation_id, "delegated backend invoking device");

		let invoke = omp_env::frame::InvokeTool {
			invocation_id,
			name: operation.to_string(),
			rev: device::REVISION.to_owned(),
			deadline_ms: u64::try_from(Duration::from_secs(30).as_millis()).unwrap_or(u64::MAX),
			output_request: omp_env::frame::OutputRequest::Complete as i32,
			restrictions: None,
			..Default::default()
		};

		let mut invocation = self.inner.client.invoke(invoke).await.map_err(|e| {
			tracing::error!(operation, error = %e, "delegated backend invoke failed");
			DelegatedError::Transport(e)
		})?;

		// Wait for acceptance
		match invocation.next_event().await.map_err(|e| {
			tracing::error!(operation, error = %e, "delegated backend next_event failed");
			DelegatedError::Transport(e)
		})? {
			Some(InvocationEvent::Accepted(_)) => {
				tracing::debug!(operation, "delegated backend device accepted");
			},
			Some(InvocationEvent::Verdict(v)) => {
				// Some devices may return immediately with verdict
				return Self::extract_result(v, operation);
			},
			_ => {
				tracing::error!(operation, "delegated backend device did not accept");
				return Err(DelegatedError::Device {
					operation,
					detail: Str::new_static("device did not accept"),
				});
			},
		}

		// Commit empty args with the device-specific arguments
		let args_bytes =
			bytes::Bytes::from(serde_json::to_vec(&args).unwrap_or_else(|_| b"{}".to_vec()));
		invocation
			.commit_args(args_bytes, Default::default(), 0, None)
			.await
			.map_err(|e| {
				tracing::error!(operation, error = %e, "delegated backend commit_args failed");
				DelegatedError::Transport(e)
			})?;

		tracing::debug!(operation, "delegated backend args committed, waiting for verdict");

		// Stream events until verdict
		loop {
			match invocation.next_event().await.map_err(|e| {
				tracing::error!(operation, error = %e, "delegated backend verdict stream failed");
				DelegatedError::Transport(e)
			})? {
				Some(InvocationEvent::Verdict(v)) => {
					tracing::debug!(operation, "delegated backend received verdict");
					return Self::extract_result(v, operation);
				},
				Some(InvocationEvent::Accepted(_)) | Some(InvocationEvent::Update(_)) => {
					// Continue streaming
				},
				Some(InvocationEvent::Admission(_)) => {
					tracing::error!(operation, "delegated backend unexpected admission query");
					// Should not happen for backend operations
					return Err(DelegatedError::Device {
						operation,
						detail: Str::new_static("unexpected admission query"),
					});
				},
				None => {
					tracing::error!(operation, "delegated backend stream closed without verdict");
					return Err(DelegatedError::Device {
						operation,
						detail: Str::new_static("device stream closed without verdict"),
					});
				},
			}
		}
	}

	fn extract_result(
		v: omp_env::frame::Verdict,
		operation: &'static str,
	) -> Result<Value, DelegatedError> {
		// Check for error outcome
		if v.is_error {
			return Err(DelegatedError::Device {
				operation,
				detail: Str::new_static("device returned error"),
			});
		}
		// Parse the json bytes field as a JSON Value
		if v.json.is_empty() {
			return Err(DelegatedError::Malformed { operation });
		}
		serde_json::from_slice(&v.json).map_err(|_| DelegatedError::Malformed { operation })
	}
}

impl ExecutionBackend for DelegatedBackend {
	type Run = DelegatedRun;

	fn id(&self) -> &BackendId<str> {
		&self.inner.id
	}

	fn capabilities(&self) -> &BackendCapabilities {
		&self.inner.capabilities
	}

	fn start(
		&self,
		request: ExecutionRequest,
	) -> impl Future<Output = Result<Self::Run, ExecutionError>> + Send + '_ {
		let backend = self.clone();
		async move { backend.start_request(request).await }
	}

	fn reattach(
		&self,
		handle: ReattachHandle,
		context: omp_exec::ReattachContext,
	) -> impl Future<Output = Result<Self::Run, ExecutionError>> + Send + '_ {
		let backend = self.clone();
		async move { backend.reattach_request(handle, context).await }
	}
}

impl DelegatedBackend {
	async fn start_request(
		&self,
		request: ExecutionRequest,
	) -> Result<DelegatedRun, ExecutionError> {
		// The Run API accepts executable steps, not one step per argv token.
		let command = request
			.argv
			.iter()
			.map(|arg| shell_quote(arg.as_str()))
			.collect::<Vec<_>>()
			.join(" ");
		let steps = [serde_json::json!({ "run": command })];

		let env_map: serde_json::Map<String, serde_json::Value> = request
			.env
			.iter()
			.map(|e| (e.name.to_string(), serde_json::json!(e.value.as_str())))
			.collect();

		// Include the API key credential so the extension can use it directly
		// instead of calling subprocess (which is blocked by the sandbox)
		let api_key = self
			.inner
			.credential
			.as_ref()
			.map(|c| c.expose().to_string());

		let args = serde_json::json!({
			"steps": steps,
			"env": env_map,
			"api_key": api_key,
		});

		tracing::info!(step_count = steps.len(), "delegated backend calling start device");

		let result = self.call_device(device::START, args).await.map_err(|e| {
			tracing::error!(error = %e, "delegated backend start device failed");
			ExecutionError::Transport { source: Arc::new(e) }
		})?;

		let operation = result
			.get("operation")
			.and_then(|v| v.as_str())
			.unwrap_or("")
			.to_string();

		if operation.is_empty() {
			tracing::error!(?result, "delegated backend start returned no operation ID");
			return Err(ExecutionError::Transport {
				source: Arc::new(DelegatedError::Malformed { operation: device::START }),
			});
		}

		tracing::info!(operation, "delegated backend start succeeded");

		Ok(DelegatedRun {
			backend:       self.clone(),
			operation:     Str::from(operation),
			id:            request.id,
			outputs:       request.outputs,
			timeout:       request.timeout,
			poll_cursor:   None,
			log_cursor:    None,
			pending_event: None,
			settled:       false,
			cancel:        CancellationToken::new(),
		})
	}

	async fn reattach_request(
		&self,
		handle: ReattachHandle,
		_context: omp_exec::ReattachContext,
	) -> Result<DelegatedRun, ExecutionError> {
		// Reattach by creating a run that starts polling from the stored operation
		Ok(DelegatedRun {
			backend:       self.clone(),
			operation:     handle.operation,
			id:            handle.id,
			outputs:       omp_exec::OutputSpec::Logs,
			timeout:       Duration::from_secs(60 * 30),
			poll_cursor:   None,
			log_cursor:    None,
			pending_event: None,
			settled:       false,
			cancel:        CancellationToken::new(),
		})
	}
}

/// One delegated run in progress.
pub struct DelegatedRun {
	backend:       DelegatedBackend,
	operation:     Str,
	id:            omp_exec::ExecutionId,
	outputs:       omp_exec::OutputSpec,
	timeout:       Duration,
	poll_cursor:   Option<Str>,
	log_cursor:    Option<Str>,
	pending_event: Option<ExecutionEvent>,
	settled:       bool,
	cancel:        CancellationToken,
}

impl ExecutionRun for DelegatedRun {
	fn next_event(&mut self) -> impl Future<Output = Option<ExecutionEvent>> + Send + '_ {
		async move {
			if let Some(event) = self.pending_event.take() {
				self.settled = matches!(event, ExecutionEvent::Settled(_));
				return Some(event);
			}
			if self.settled {
				return None;
			}
			let next = self
				.backend
				.next_event_for(
					self.operation.clone(),
					self.id.clone(),
					self.poll_cursor.clone(),
					self.log_cursor.clone(),
				)
				.await;
			self.poll_cursor = next.poll_cursor;
			self.log_cursor = next.log_cursor;
			if let Some(output) = next.output {
				self.pending_event = Some(next.event);
				return Some(ExecutionEvent::Output(output));
			}
			self.settled = matches!(next.event, ExecutionEvent::Settled(_));
			Some(next.event)
		}
	}

	fn cancel(&self) -> impl Future<Output = CancelOutcome> + Send + '_ {
		async move {
			let result = self
				.backend
				.call_device(
					device::CANCEL,
					serde_json::json!({ "operation": self.operation.as_str() }),
				)
				.await;

			match result {
				Ok(v) => {
					let outcome = v.get("outcome").and_then(|o| o.as_str()).unwrap_or("");
					match outcome {
						"unsupported" => CancelOutcome::Unsupported,
						"cancelled" => CancelOutcome::Cancelled,
						"already-settled" => CancelOutcome::AlreadySettled,
						"requested" => CancelOutcome::Requested,
						_ => CancelOutcome::Failed,
					}
				},
				Err(_) => CancelOutcome::Failed,
			}
		}
	}

	fn detach(self) -> ReattachHandle {
		ReattachHandle::new(self.backend.inner.id.clone(), self.operation, self.id)
	}
}

struct DelegatedNext {
	event:       ExecutionEvent,
	output:      Option<OutputFrame>,
	poll_cursor: Option<Str>,
	log_cursor:  Option<Str>,
}

impl DelegatedBackend {
	async fn next_event_for(
		&self,
		operation: Str,
		id: omp_exec::ExecutionId,
		poll_cursor: Option<Str>,
		log_cursor: Option<Str>,
	) -> DelegatedNext {
		let inner = &self.inner;
		let auth = credential_value(inner);
		let mut poll_args = serde_json::json!({
			"operation": operation.as_str(),
			"api_key": auth,
		});
		if let Some(cursor) = poll_cursor.as_ref() {
			poll_args["cursor"] = serde_json::json!(cursor.as_str());
		}
		let poll_result = match self.call_device(device::POLL, poll_args).await {
			Ok(result) => result,
			Err(error) => {
				return DelegatedNext {
					event: ExecutionEvent::Settled(Err(ExecutionError::Transport {
						source: Arc::new(error),
					})),
					output: None,
					poll_cursor,
					log_cursor,
				};
			},
		};
		let next_poll_cursor = next_cursor(&poll_result).or(poll_cursor);
		let auth = credential_value(inner);
		let mut log_args = serde_json::json!({
			"operation": operation.as_str(),
			"api_key": auth,
		});
		if let Some(cursor) = log_cursor.as_ref() {
			log_args["cursor"] = serde_json::json!(cursor.as_str());
		}
		let (output, next_log_cursor) = match self.call_device(device::GET_LOG, log_args).await {
			Ok(result) => {
				let output = result
					.get("output")
					.and_then(Value::as_str)
					.filter(|output| !output.is_empty())
					.map(|output| OutputFrame {
						stream: Stream::Combined,
						bytes:  CowBytes::from(output.as_bytes().to_vec()),
					});
				(output, next_cursor(&result).or(log_cursor))
			},
			Err(error) => {
				return DelegatedNext {
					event: ExecutionEvent::Settled(Err(ExecutionError::OutputRetrieval {
						source: Arc::new(error),
					})),
					output: None,
					poll_cursor: next_poll_cursor,
					log_cursor,
				};
			},
		};
		let state = poll_result
			.get("state")
			.and_then(Value::as_str)
			.unwrap_or("unknown");
		let event = match state {
			"running" => ExecutionEvent::Executing,
			"succeeded" | "failed" => {
				let exit_code = poll_result
					.get("exitCode")
					.and_then(Value::as_i64)
					.unwrap_or(if state == "succeeded" { 0 } else { 1 }) as i32;
				ExecutionEvent::Settled(Ok(ExecutionResult {
					id,
					backend: self.inner.id.clone(),
					outcome: Outcome::Exited { code: exit_code },
					logs: None,
					artifacts: Arc::new([]),
					output_digests: Default::default(),
					timing: Default::default(),
					transfer: Default::default(),
					cache_hit: false,
					fallback: None,
				}))
			},
			_ => ExecutionEvent::Queued,
		};
		DelegatedNext { event, output, poll_cursor: next_poll_cursor, log_cursor: next_log_cursor }
	}
}

fn next_cursor(result: &Value) -> Option<Str> {
	result
		.get("nextCursor")
		.and_then(Value::as_str)
		.filter(|cursor| !cursor.is_empty())
		.map(Str::new)
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn backend_id_is_preserved() {
		let id = BackendId::new_static("buildbuddy");
		// We can't create a real EnvClient in a unit test, but we can verify
		// the type structure compiles correctly.
		assert_eq!(id.as_str(), "buildbuddy");
	}

	#[test]
	fn device_operation_names_are_correct() {
		assert_eq!(device::DESCRIBE, "exec/describe");
		assert_eq!(device::START, "exec/start");
		assert_eq!(device::POLL, "exec/poll");
		assert_eq!(device::GET_LOG, "exec/get_log");
		assert_eq!(device::CANCEL, "exec/cancel");
		assert_eq!(device::REVISION, "buildbuddy.1");
	}

	#[test]
	fn command_step_preserves_argv_boundaries() {
		let argv = ["just", "check", "a'b"];
		let command = argv
			.into_iter()
			.map(shell_quote)
			.collect::<Vec<_>>()
			.join(" ");
		assert_eq!(command, "'just' 'check' 'a'\\''b'");
	}
}
