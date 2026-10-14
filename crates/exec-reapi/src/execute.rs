//! REAPI action construction, CAS upload, execution streaming and cancellation.
//!
//! The backend is generic over the caller-owned blob authority. It never opens
//! a path or keeps a second blob store: capture and reads are delegated to
//! [`crate::cas::InputSource`].

use std::{collections::BTreeMap, sync::Arc, time::Duration};

use bytes::Bytes;
use omp_adw::Posture;
use omp_core::Str;
use omp_exec::{
	Artifact, BackendCapabilities, BackendId, CachePolicy, CancelOutcome, CancelSupport, EnvEntry,
	ExecutionBackend, ExecutionError, ExecutionEvent, ExecutionRequest, ExecutionResult,
	ExecutionRun, InputSet, Logs, Outcome, Platform, Reattach, RemoteCancel, Timing, TransferCounts,
};
use parking_lot::Mutex;
use prost::{Message as _, Name as _};
use prost_types::Duration as WireDuration;
use tonic::{Code, Request, Status, metadata::MetadataValue, transport::Channel};

use crate::{
	capabilities::{self, Capabilities},
	cas::{self, InputSource},
	digest::{self, DigestError},
	endpoint::Endpoint,
	profile::{Credential, Profile},
	proto::{
		build::bazel::remote::execution::v2 as wire,
		google::{bytestream, longrunning},
	},
	status::{self, Phase},
	tree::{self, InputFile},
};

/// A toolchain image and the environment it contributes to an action.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Toolchain {
	/// Digest-pinned image reference, if the service profile requires one.
	pub image: Option<Str>,
	/// Image-provided environment variables.
	pub env:   Arc<[EnvEntry]>,
}

/// Configuration for a [`ReapiBackend`].
#[derive(Debug, Clone)]
pub struct Config {
	/// Operator identity for this backend.
	pub id:                  BackendId,
	/// REAPI instance name.
	pub instance:            Str,
	/// Service profile (portable REAPI or another profile expressed as data).
	pub profile:             Profile,
	/// Credential written only to the profile's metadata header.
	pub credential:          Option<Credential>,
	/// Platform offered by this backend.
	pub platform:            Platform,
	/// Enforced posture offered by this backend.
	pub enforced:            Posture,
	/// Action-cache policy.
	pub cache:               CachePolicy,
	/// Extra action platform properties.
	pub platform_properties: BTreeMap<Str, Str>,
	/// Toolchain mapping supplied by the composing layer.
	pub toolchains:          BTreeMap<Str, Toolchain>,
}

/// A connected generic REAPI backend.
#[derive(Clone)]
pub struct ReapiBackend<S> {
	inner: Arc<BackendInner<S>>,
}

struct BackendInner<S> {
	source:              S,
	endpoint:            Endpoint,
	profile:             Profile,
	credential:          Option<Credential>,
	instance:            Str,
	clients:             Clients,
	caps:                Capabilities,
	backend_caps:        BackendCapabilities,
	cache:               CachePolicy,
	platform_properties: BTreeMap<Str, Str>,
	toolchains:          BTreeMap<Str, Toolchain>,
	id:                  BackendId,
}

#[derive(Clone)]
struct Clients {
	cas:          wire::content_addressable_storage_client::ContentAddressableStorageClient<Channel>,
	capabilities: wire::capabilities_client::CapabilitiesClient<Channel>,
	byte_stream:  bytestream::byte_stream_client::ByteStreamClient<Channel>,
	execution:    wire::execution_client::ExecutionClient<Channel>,
	operations:   longrunning::operations_client::OperationsClient<Channel>,
}

/// An error while establishing a transport connection.
#[derive(Debug, thiserror::Error)]
enum ConnectError {
	/// Tonic rejected the endpoint or failed to connect.
	#[error("REAPI transport connection failed")]
	Transport(#[source] tonic::transport::Error),
}

/// An error in a local request or a malformed remote response.
#[derive(Debug, thiserror::Error)]
enum DecodeError {
	/// A protobuf `Any` could not be decoded.
	#[error("REAPI response protobuf is malformed")]
	Proto(#[source] prost::DecodeError),
	/// The response omitted a required action result.
	#[error("REAPI completed operation omitted its action result")]
	MissingResult,
	/// The request had no captured inputs.
	#[error("REAPI execution request has no captured input set")]
	MissingInputs,
	/// Metadata could not be encoded as a gRPC value.
	#[error("REAPI credential metadata is invalid")]
	Metadata(#[source] tonic::metadata::errors::InvalidMetadataValue),
}

fn foreign<E: std::error::Error + Send + Sync + 'static>(
	error: E,
) -> Arc<dyn std::error::Error + Send + Sync> {
	Arc::new(error)
}

fn config_error<E: std::error::Error + Send + Sync + 'static>(error: E) -> ExecutionError {
	ExecutionError::Config { source: foreign(error) }
}

fn transport<E: std::error::Error + Send + Sync + 'static>(error: E) -> ExecutionError {
	ExecutionError::Transport { source: foreign(error) }
}

impl<S> ReapiBackend<S>
where
	S: InputSource,
{
	/// Connects to an endpoint and loads the server's REAPI capabilities.
	///
	/// # Errors
	///
	/// Returns a typed transport, configuration, authentication, or capability
	/// error when the endpoint cannot serve this backend.
	pub async fn connect(
		endpoint: Endpoint,
		config: Config,
		source: S,
	) -> Result<Self, ExecutionError> {
		let mut builder = tonic::transport::Endpoint::from_shared(endpoint.transport_url())
			.map_err(config_error)?
			.connect_timeout(Duration::from_secs(30));
		if endpoint.scheme() == crate::endpoint::Scheme::Tls {
			builder = builder
				.tls_config(tonic::transport::ClientTlsConfig::new().with_webpki_roots())
				.map_err(config_error)?;
		}
		let channel = builder
			.connect()
			.await
			.map_err(ConnectError::Transport)
			.map_err(transport)?;
		let clients = Clients {
			cas:
				wire::content_addressable_storage_client::ContentAddressableStorageClient::new(
					channel.clone(),
				),
			capabilities: wire::capabilities_client::CapabilitiesClient::new(channel.clone()),
			byte_stream:  bytestream::byte_stream_client::ByteStreamClient::new(channel.clone()),
			execution:    wire::execution_client::ExecutionClient::new(channel.clone()),
			operations:   longrunning::operations_client::OperationsClient::new(channel),
		};
		let reply = clients_capabilities(&clients, &config, &endpoint).await?;
		let caps = capabilities::interpret(&reply).map_err(config_error)?;
		let backend_caps = BackendCapabilities {
			platforms:      Arc::new([config.platform]),
			needs:          omp_exec::NeedSet::new(),
			max_timeout:    None,
			action_cache:   caps.action_cache,
			cancel:         caps.cancel,
			max_blob_bytes: caps.max_blob_bytes,
			warm_caches:    false,
			enforced:       config.enforced,
		};
		Ok(Self {
			inner: Arc::new(BackendInner {
				source,
				endpoint,
				profile: config.profile,
				credential: config.credential,
				instance: config.instance,
				clients,
				caps,
				backend_caps,
				cache: config.cache,
				platform_properties: config.platform_properties,
				toolchains: config.toolchains,
				id: config.id,
			}),
		})
	}

	/// Creates a connected backend from an already-open channel. This is useful
	/// for tests and for callers that own channel construction and TLS policy.
	///
	/// The caller supplies capabilities obtained from `GetCapabilities`.
	pub fn from_channel(channel: Channel, config: Config, caps: Capabilities, source: S) -> Self {
		let clients = Clients {
			cas:
				wire::content_addressable_storage_client::ContentAddressableStorageClient::new(
					channel.clone(),
				),
			capabilities: wire::capabilities_client::CapabilitiesClient::new(channel.clone()),
			byte_stream:  bytestream::byte_stream_client::ByteStreamClient::new(channel.clone()),
			execution:    wire::execution_client::ExecutionClient::new(channel.clone()),
			operations:   longrunning::operations_client::OperationsClient::new(channel),
		};
		let backend_caps = BackendCapabilities {
			platforms:      Arc::new([config.platform]),
			needs:          omp_exec::NeedSet::new(),
			max_timeout:    None,
			action_cache:   caps.action_cache,
			cancel:         caps.cancel,
			max_blob_bytes: caps.max_blob_bytes,
			warm_caches:    false,
			enforced:       config.enforced,
		};
		Self {
			inner: Arc::new(BackendInner {
				source,
				endpoint: Endpoint::parse("grpc://127.0.0.1:1").unwrap_or_else(|_| unreachable!()),
				profile: config.profile,
				credential: config.credential,
				instance: config.instance,
				clients,
				caps,
				backend_caps,
				cache: config.cache,
				platform_properties: config.platform_properties,
				toolchains: config.toolchains,
				id: config.id,
			}),
		}
	}

	/// Returns the server capabilities loaded at connection time.
	pub fn reapi_capabilities(&self) -> &Capabilities {
		&self.inner.caps
	}
}

async fn clients_capabilities(
	clients: &Clients,
	config: &Config,
	_endpoint: &Endpoint,
) -> Result<wire::ServerCapabilities, ExecutionError> {
	let request = with_metadata(
		Request::new(wire::GetCapabilitiesRequest { instance_name: config.instance.to_string() }),
		&config.profile,
		config.credential.as_ref(),
	)?;
	let mut client = clients.capabilities.clone();
	client
		.get_capabilities(request)
		.await
		.map(|reply| reply.into_inner())
		.map_err(|error| {
			status::classify(error, Phase::FindMissing, &config.id, Duration::from_secs(30), None)
		})
}

fn with_metadata<T>(
	mut request: Request<T>,
	profile: &Profile,
	credential: Option<&Credential>,
) -> Result<Request<T>, ExecutionError> {
	if let (Some(header), Some(credential)) = (&profile.credential_header, credential) {
		let key = tonic::metadata::MetadataKey::from_bytes(header.as_bytes())
			.map_err(|_| config_error(DecodeError::MissingResult))?;
		let mut value = MetadataValue::try_from(credential.expose())
			.map_err(DecodeError::Metadata)
			.map_err(config_error)?;
		value.set_sensitive(true);
		request.metadata_mut().insert(key, value);
	}
	Ok(request)
}

impl<S> ExecutionBackend for ReapiBackend<S>
where
	S: InputSource,
{
	type Run = ReapiRun<S>;

	fn id(&self) -> &BackendId<str> {
		&self.inner.id
	}

	fn capabilities(&self) -> &BackendCapabilities {
		&self.inner.backend_caps
	}

	fn start(
		&self,
		request: ExecutionRequest,
	) -> impl Future<Output = Result<Self::Run, ExecutionError>> + Send + '_ {
		let this = self.clone();
		async move { this.start_request(request).await }
	}

	fn reattach(
		&self,
		handle: Reattach,
	) -> impl Future<Output = Result<Self::Run, ExecutionError>> + Send + '_ {
		let this = self.clone();
		async move { this.reattach_request(handle).await }
	}
}

impl<S> ReapiBackend<S>
where
	S: InputSource,
{
	async fn start_request(&self, request: ExecutionRequest) -> Result<ReapiRun<S>, ExecutionError> {
		let action = self.prepare_action(&request).await?;
		let action_timeout = request.timeout;
		let request_id = request.id.clone();
		let request_wire = wire::ExecuteRequest {
			instance_name: self.inner.instance.to_string(),
			skip_cache_lookup: self.inner.cache == CachePolicy::Off,
			action_digest: Some(digest::to_wire(action.digest)),
			..Default::default()
		};
		let metadata = metadata_for(&request);
		let request = with_metadata(
			Request::new(request_wire),
			&self.inner.profile,
			self.inner.credential.as_ref(),
		)?;
		let mut execution = self.inner.clients.execution.clone();
		let stream = execution.execute(request).await.map_err(|error| {
			status::classify(
				error,
				Phase::Execute,
				&self.inner.id,
				action_timeout,
				Some(action.digest),
			)
		})?;
		Ok(ReapiRun::new(self.clone(), stream.into_inner(), action.digest, request_id, metadata))
	}

	async fn reattach_request(&self, handle: Reattach) -> Result<ReapiRun<S>, ExecutionError> {
		let request_wire = wire::WaitExecutionRequest { name: handle.operation.to_string() };
		let request = with_metadata(
			Request::new(request_wire),
			&self.inner.profile,
			self.inner.credential.as_ref(),
		)?;
		let mut execution = self.inner.clients.execution.clone();
		let stream = execution.wait_execution(request).await.map_err(|error| {
			status::classify(error, Phase::Execute, &self.inner.id, Duration::from_secs(30), None)
		})?;
		Ok(ReapiRun::new(
			self.clone(),
			stream.into_inner(),
			omp_exec::Digest::new(omp_core::Hash32::default(), 0),
			handle.id,
			RequestMetadataState::default(),
		))
	}

	async fn prepare_action(
		&self,
		request: &ExecutionRequest,
	) -> Result<ActionData, ExecutionError> {
		let InputSet { manifest, .. } = request
			.inputs
			.as_ref()
			.ok_or_else(|| config_error(DecodeError::MissingInputs))?;
		let files = self
			.inner
			.source
			.files(*manifest)
			.await
			.map_err(|error| ExecutionError::InputCapture { source: foreign(error) })?;
		let tree = tree::build(&files).map_err(config_error)?;
		let toolchain = self.inner.toolchains.get(request.toolchain.as_str());
		let mut env = request.env.iter().cloned().collect::<Vec<_>>();
		if let Some(toolchain) = toolchain {
			env.extend(toolchain.env.iter().cloned());
		}
		let command = wire::Command {
			arguments: request.argv.iter().map(ToString::to_string).collect(),
			environment_variables: env
				.into_iter()
				.map(|entry| wire::command::EnvironmentVariable {
					name:  entry.name.to_string(),
					value: entry.value.to_string(),
				})
				.collect(),
			output_paths: output_paths(&request.outputs),
			working_directory: request.cwd.to_string(),
			..Default::default()
		};
		let command_bytes = command.encode_to_vec();
		let command_digest = blob_digest(&command_bytes);
		let platform = wire::Platform {
			properties: self
				.inner
				.profile
				.platform_properties(request.platform.platform(), &self.inner.platform_properties)
				.into_iter()
				.map(|(name, value)| wire::platform::Property {
					name:  name.to_string(),
					value: value.to_string(),
				})
				.collect(),
		};
		let action = wire::Action {
			command_digest: Some(digest::to_wire(command_digest)),
			input_root_digest: Some(digest::to_wire(tree.root)),
			timeout: Some(timeout(request.timeout)),
			do_not_cache: self.inner.cache == CachePolicy::Off,
			platform: Some(platform),
			..Default::default()
		};
		let action_bytes = action.encode_to_vec();
		let action_digest = blob_digest(&action_bytes);
		let mut blobs = Vec::with_capacity(tree.directories.len() + 2);
		for (digest, directory) in tree.directories {
			blobs.push((digest, Bytes::from(directory.encode_to_vec())));
		}
		blobs.push((command_digest, Bytes::from(command_bytes)));
		blobs.push((action_digest, Bytes::from(action_bytes)));
		self.upload(&files, blobs).await?;
		Ok(ActionData { digest: action_digest })
	}

	async fn upload(
		&self,
		files: &[InputFile],
		mut generated: Vec<(omp_exec::Digest, Bytes)>,
	) -> Result<(), ExecutionError> {
		for file in files {
			if !generated.iter().any(|(digest, _)| digest == &file.digest) {
				generated.push((
					file.digest,
					self
						.inner
						.source
						.read(file.digest)
						.await
						.map_err(|_| ExecutionError::Cas {
							op:     omp_exec::CasOp::Upload,
							digest: file.digest,
						})?,
				));
			}
		}
		let digests: Vec<_> = generated.iter().map(|(digest, _)| *digest).collect();
		let request = with_metadata(
			Request::new(wire::FindMissingBlobsRequest {
				instance_name:   self.inner.instance.to_string(),
				blob_digests:    digests.iter().copied().map(digest::to_wire).collect(),
				digest_function: wire::digest_function::Value::Sha256 as i32,
			}),
			&self.inner.profile,
			self.inner.credential.as_ref(),
		)?;
		let mut client = self.inner.clients.cas.clone();
		let missing = client
			.find_missing_blobs(request)
			.await
			.map_err(|error| {
				status::classify(
					error,
					Phase::FindMissing,
					&self.inner.id,
					Duration::from_secs(30),
					None,
				)
			})?
			.into_inner()
			.missing_blob_digests;
		let missing = missing
			.iter()
			.map(digest::from_wire)
			.collect::<Result<Vec<_>, DigestError>>()
			.map_err(config_error)?;
		let plan = cas::plan_upload(&missing, &self.inner.caps);
		for batch in plan.batches {
			let requests = batch
				.iter()
				.map(|digest| {
					let data = generated
						.iter()
						.find(|(candidate, _)| candidate == digest)
						.map_or_else(Bytes::new, |(_, data)| data.clone());
					wire::batch_update_blobs_request::Request {
						digest: Some(digest::to_wire(*digest)),
						data,
						compressor: wire::compressor::Value::Identity as i32,
					}
				})
				.collect();
			let request = with_metadata(
				Request::new(wire::BatchUpdateBlobsRequest {
					instance_name: self.inner.instance.to_string(),
					requests,
					digest_function: wire::digest_function::Value::Sha256 as i32,
				}),
				&self.inner.profile,
				self.inner.credential.as_ref(),
			)?;
			let reply = client
				.batch_update_blobs(request)
				.await
				.map_err(|error| {
					status::classify(error, Phase::Upload, &self.inner.id, Duration::from_secs(30), None)
				})?
				.into_inner();
			for response in reply.responses {
				if let Some(status) = response.status.filter(|status| status.code != 0) {
					return Err(status::classify(
						wire_status(status, self.inner.credential.as_ref()),
						Phase::Upload,
						&self.inner.id,
						Duration::from_secs(30),
						response
							.digest
							.and_then(|digest| digest::from_wire(&digest).ok()),
					));
				}
			}
		}
		for digest in plan.streamed {
			self.stream_blob(digest).await?;
		}
		Ok(())
	}

	async fn stream_blob(&self, digest: omp_exec::Digest) -> Result<(), ExecutionError> {
		let source = self.inner.source.clone();
		let resource = cas::write_resource(
			self.inner.instance.as_str(),
			"omp-upload",
			&digest.hash.to_hex().to_string(),
			digest.size,
			cas::Compression::Identity,
		);
		let stream = async_stream::stream! {
			let mut offset = 0_u64;
			while offset < digest.size {
				let len = (digest.size - offset).min(1024 * 1024);
				let Ok(data) = source.read_range(digest, offset, len).await else { return };
				offset = offset.saturating_add(data.len() as u64);
				yield bytestream::WriteRequest { resource_name: resource.clone(), write_offset: offset as i64 - data.len() as i64, finish_write: offset >= digest.size, data };
			}
		};
		let request =
			with_metadata(Request::new(stream), &self.inner.profile, self.inner.credential.as_ref())?;
		let mut client = self.inner.clients.byte_stream.clone();
		client.write(request).await.map_err(|error| {
			status::classify(
				error,
				Phase::Upload,
				&self.inner.id,
				Duration::from_secs(30),
				Some(digest),
			)
		})?;
		Ok(())
	}
}

struct ActionData {
	digest: omp_exec::Digest,
}

fn blob_digest(bytes: &[u8]) -> omp_exec::Digest {
	omp_exec::Digest::new(omp_core::Hash32::sum(bytes), bytes.len() as u64)
}

fn timeout(duration: Duration) -> WireDuration {
	WireDuration {
		seconds: duration.as_secs().try_into().unwrap_or(i64::MAX),
		nanos:   duration.subsec_nanos() as i32,
	}
}

fn output_paths(outputs: &omp_exec::OutputSpec) -> Vec<String> {
	match outputs {
		omp_exec::OutputSpec::Paths(paths) => paths.iter().map(ToString::to_string).collect(),
		omp_exec::OutputSpec::All => vec![".".to_owned()],
		omp_exec::OutputSpec::Metadata | omp_exec::OutputSpec::Logs => Vec::new(),
	}
}

#[derive(Default)]
struct RequestMetadataState;

fn metadata_for(_request: &ExecutionRequest) -> RequestMetadataState {
	RequestMetadataState
}

struct ReapiRun<S> {
	backend:       ReapiBackend<S>,
	stream:        tonic::Streaming<longrunning::Operation>,
	action_digest: omp_exec::Digest,
	id:            omp_exec::ExecutionId,
	operation:     Arc<Mutex<Option<Str>>>,
	reconnected:   bool,
	accepted:      bool,
	settled:       bool,
	metadata:      RequestMetadataState,
}

impl<S> ReapiRun<S>
where
	S: InputSource,
{
	fn new(
		backend: ReapiBackend<S>,
		stream: tonic::Streaming<longrunning::Operation>,
		action_digest: omp_exec::Digest,
		id: omp_exec::ExecutionId,
		metadata: RequestMetadataState,
	) -> Self {
		Self {
			backend,
			stream,
			action_digest,
			id,
			operation: Arc::new(Mutex::new(None)),
			reconnected: false,
			accepted: false,
			settled: false,
			metadata,
		}
	}

	async fn next(&mut self) -> Option<ExecutionEvent> {
		loop {
			let operation = match self.stream.message().await {
				Ok(Some(operation)) => operation,
				Ok(None) if !self.reconnected && self.operation.lock().is_some() => {
					if let Err(error) = self.wait_again().await {
						self.settled = true;
						return Some(ExecutionEvent::Settled(Err(error)));
					}
					continue;
				},
				Ok(None) => {
					self.settled = true;
					return Some(ExecutionEvent::Settled(Err(transport(Status::unavailable(
						"REAPI execution stream ended",
					)))));
				},
				Err(error)
					if !self.reconnected
						&& self.operation.lock().is_some()
						&& error.code() == Code::Unavailable =>
				{
					if let Err(error) = self.wait_again().await {
						self.settled = true;
						return Some(ExecutionEvent::Settled(Err(error)));
					}
					continue;
				},
				Err(error) => {
					self.settled = true;
					return Some(ExecutionEvent::Settled(Err(status::classify(
						error,
						Phase::Execute,
						&self.backend.inner.id,
						Duration::from_secs(30),
						None,
					))));
				},
			};
			if operation.name.is_empty() {
				self.settled = true;
				return Some(ExecutionEvent::Settled(Err(config_error(DecodeError::MissingResult))));
			}
			*self.operation.lock() = Some(Str::new(&operation.name));
			if !self.accepted {
				self.accepted = true;
				return Some(ExecutionEvent::Accepted { operation: Str::new(&operation.name) });
			}
			if !operation.done {
				return Some(stage_event(&operation));
			}
			self.settled = true;
			return Some(self.settle(operation).await);
		}
	}

	async fn wait_again(&mut self) -> Result<(), ExecutionError> {
		self.reconnected = true;
		let Some(name) = self.operation.lock().clone() else {
			return Err(transport(Status::unavailable("REAPI operation name was lost")));
		};
		let request = with_metadata(
			Request::new(wire::WaitExecutionRequest { name: name.to_string() }),
			&self.backend.inner.profile,
			self.backend.inner.credential.as_ref(),
		)?;
		let mut client = self.backend.inner.clients.execution.clone();
		self.stream = client
			.wait_execution(request)
			.await
			.map_err(|error| {
				status::classify(
					error,
					Phase::Execute,
					&self.backend.inner.id,
					Duration::from_secs(30),
					None,
				)
			})?
			.into_inner();
		Ok(())
	}

	async fn settle(&self, operation: longrunning::Operation) -> ExecutionEvent {
		let Some(result_wire) = operation.result else {
			return ExecutionEvent::Settled(Err(config_error(DecodeError::MissingResult)));
		};
		let response = match result_wire {
			longrunning::operation::Result::Error(error) => {
				return ExecutionEvent::Settled(Err(status::classify(
					wire_status(error, self.backend.inner.credential.as_ref()),
					Phase::Execute,
					&self.backend.inner.id,
					Duration::from_secs(30),
					None,
				)));
			},
			longrunning::operation::Result::Response(response) => response,
		};
		let result = match wire::ExecuteResponse::decode(response.value.as_ref()) {
			Ok(result) => result,
			Err(error) => {
				return ExecutionEvent::Settled(Err(config_error(DecodeError::Proto(error))));
			},
		};
		if let Some(status) = result.status.filter(|status| status.code != 0) {
			return ExecutionEvent::Settled(Err(status::classify(
				wire_status(status, self.backend.inner.credential.as_ref()),
				Phase::Execute,
				&self.backend.inner.id,
				Duration::from_secs(30),
				None,
			)));
		}
		let Some(action) = result.result else {
			return ExecutionEvent::Settled(Err(config_error(DecodeError::MissingResult)));
		};
		let logs = match (action.stdout_digest.as_ref(), action.stderr_digest.as_ref()) {
			(Some(stdout), Some(stderr)) => {
				match (digest::from_wire(stdout), digest::from_wire(stderr)) {
					(Ok(stdout), Ok(stderr)) => Some(Logs { stdout, stderr }),
					(Err(error), _) | (_, Err(error)) => {
						return ExecutionEvent::Settled(Err(config_error(error)));
					},
				}
			},
			_ => None,
		};
		let artifacts = action
			.output_files
			.into_iter()
			.filter_map(|file| {
				Some(Artifact {
					path:       Str::new(file.path),
					blob:       digest::from_wire(file.digest.as_ref()?).ok()?,
					executable: file.is_executable,
					fetched:    false,
				})
			})
			.collect();
		ExecutionEvent::Settled(Ok(ExecutionResult {
			id: self.id.clone(),
			backend: self.backend.inner.id.clone(),
			outcome: Outcome::Exited { code: action.exit_code },
			logs,
			artifacts,
			timing: Timing::default(),
			transfer: TransferCounts::default(),
			cache_hit: result.cached_result,
			fallback: None,
		}))
	}
}

impl<S> ExecutionRun for ReapiRun<S>
where
	S: InputSource,
{
	fn next_event(&mut self) -> impl Future<Output = Option<ExecutionEvent>> + Send + '_ {
		self.next()
	}

	fn cancel(&self) -> impl Future<Output = CancelOutcome> + Send + '_ {
		let backend = self.backend.clone();
		let operation = self.operation.clone();
		async move {
			let Some(name) = operation.lock().clone() else {
				return CancelOutcome::Unsupported;
			};
			let request = match with_metadata(
				Request::new(longrunning::CancelOperationRequest { name: name.to_string() }),
				&backend.inner.profile,
				backend.inner.credential.as_ref(),
			) {
				Ok(request) => request,
				Err(_) => return CancelOutcome::Failed,
			};
			let mut client = backend.inner.clients.operations.clone();
			match client.cancel_operation(request).await {
				Ok(_) => CancelOutcome::Requested,
				Err(error) if error.code() == Code::Unimplemented => CancelOutcome::Unsupported,
				Err(_) => CancelOutcome::Failed,
			}
		}
	}

	fn detach(self) -> Reattach {
		Reattach {
			backend:   self.backend.inner.id.clone(),
			operation: self.operation.lock().clone().unwrap_or_default(),
			id:        self.id.clone(),
		}
	}
}

fn stage_event(operation: &longrunning::Operation) -> ExecutionEvent {
	let stage = operation
		.metadata
		.as_ref()
		.and_then(|metadata| wire::ExecuteOperationMetadata::decode(metadata.value.as_ref()).ok())
		.map(|metadata| metadata.stage);
	match stage {
		Some(value) if value == wire::execution_stage::Value::Executing as i32 => {
			ExecutionEvent::Executing
		},
		_ => ExecutionEvent::Queued,
	}
}

fn wire_status(
	status: crate::proto::google::rpc::Status,
	credential: Option<&Credential>,
) -> Status {
	let mut message = status.message;
	if let Some(credential) = credential {
		if !credential.expose().is_empty() {
			message = message.replace(credential.expose(), "[REDACTED]");
		}
	}
	Status::new(Code::from_i32(status.code), message)
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn credential_never_reaches_a_wire_status_message() {
		let credential = Credential::new(SecretString::from("secret-token"));
		let status = wire_status(
			crate::proto::google::rpc::Status {
				code:    16,
				message: "secret-token rejected".to_owned(),
				details: Vec::new(),
			},
			Some(&credential),
		);
		assert!(!status.message().contains("secret-token"));
	}

	#[test]
	fn root_cwd_is_encoded_as_empty_not_dot() {
		assert!(output_paths(&omp_exec::OutputSpec::Logs).is_empty());
	}
}
