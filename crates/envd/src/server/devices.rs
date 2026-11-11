//! Worker-device and prelude bridges into the extension host.

use super::{
	PRELUDE_CALL_DEADLINE,
	forward::{materialize_worker_completion, materialize_worker_details},
	prelude::*,
};

/// Concrete environment host shared by in-process and UDS connections.
///
/// Executors remain env-side beside these resources. The server never passes a
/// capability/facet trait bundle through a tool signature.
/// Device-router bridge for final, worker-routed invocations.
#[derive(Clone)]
pub(in crate::server) struct WorkerDeviceInvoker {
	hosts: Arc<ExtHostSupervisor>,
	blobs: BlobHost,
}

impl WorkerDeviceInvoker {
	pub(in crate::server) const fn new(hosts: Arc<ExtHostSupervisor>, blobs: BlobHost) -> Self {
		Self { hosts, blobs }
	}
}

pub(in crate::server) fn internal_worker_authorization() -> (Bytes, u64) {
	let token = Bytes::from(Ulid::generate().to_string());
	let authorized_at_ms = SystemTime::now()
		.duration_since(UNIX_EPOCH)
		.map_or(0, |elapsed| elapsed.as_millis().try_into().unwrap_or(u64::MAX));
	(token, authorized_at_ms)
}

impl DeviceInvoker for WorkerDeviceInvoker {
	async fn invoke(&self, request: DeviceInvokeRequest) -> omp_tool::ErasedStream<'static> {
		let hosts = Arc::clone(&self.hosts);
		let blobs = self.blobs.clone();
		Box::pin(async_stream::stream! {
			let deadline = match request.deadline.to_std() {
				Ok(deadline) => deadline,
				Err(error) => {
					yield Err(RegistryError::VerdictShape(Str::from(error.to_string())));
					return;
				},
			};
			let mut invocation = match hosts.open(ExtHostToolCall {
				invocation_id: request.invocation_id.clone(),
				name: request.name.clone(),
				rev: request.rev.clone(),
				deadline,
			}) {
				Ok(invocation) => invocation,
				Err(error) => {
					yield Err(RegistryError::VerdictShape(Str::from(error.to_string())));
					return;
				},
			};
			let (effect_token, authorized_at_ms) = internal_worker_authorization();
			let committed = omp_proto::env::v1::ArgsCommitted {
				invocation_id: request.invocation_id.to_string(),
				raw: request.args_json,
				effect_token,
				authorized_at_ms,
				effects: Some(invocation.maximum_effect_envelope()),
				..omp_proto::env::v1::ArgsCommitted::default()
			};
			if let Err(error) = invocation.args_committed(committed) {
				yield Err(RegistryError::VerdictShape(Str::from(error.to_string())));
				return;
			}
			while let Ok(event) = invocation.next().await {
				match event {
					ExtHostEvent::Update(update) => yield Ok(ErasedEv::Update(Bytes::from(update.encode_to_vec()))),
					ExtHostEvent::Complete(complete) => match materialize_worker_completion(&blobs, &complete) {
						Ok(verdict) => {
							yield Ok(ErasedEv::Done(ErasedOutcome::Done {
								verdict,
								useless: complete.useless,
							}));
							return;
						},
						Err(error) => {
							yield Err(RegistryError::VerdictShape(error));
							return;
						},
					},
					ExtHostEvent::Aborted(abort) => {
						yield Err(RegistryError::VerdictShape(abort.reason));
						return;
					},
					ExtHostEvent::ProtocolError(_) => {
						yield Err(RegistryError::VerdictShape(sf!(
							"extension-host device protocol rejected final invocation"
						)));
						return;
					},
				}
			}
		})
	}
}

#[derive(Clone)]
pub(in crate::server) struct PreludeBridgeInvoker {
	hosts: Arc<ExtHostSupervisor>,
	blobs: BlobHost,
}

impl PreludeBridgeInvoker {
	pub(in crate::server) const fn new(hosts: Arc<ExtHostSupervisor>, blobs: BlobHost) -> Self {
		Self { hosts, blobs }
	}
}

#[async_trait::async_trait]
impl PreludeInvoker for PreludeBridgeInvoker {
	async fn invoke(
		&self,
		name: &str,
		rev: &str,
		args: serde_json::Value,
	) -> Result<serde_json::Value, BridgeHostError> {
		let invocation_id = Str::from(Ulid::generate().to_string());
		let mut invocation = self
			.hosts
			.open(ExtHostToolCall {
				invocation_id: invocation_id.clone(),
				name:          Str::new(name),
				rev:           Str::new(rev),
				deadline:      PRELUDE_CALL_DEADLINE,
			})
			.map_err(|error| {
				BridgeHostError::because("failed to open prelude helper invocation", error)
			})?;
		let raw = serde_json::to_vec(&args).map_err(|error| {
			BridgeHostError::because("failed to encode prelude helper arguments", error)
		})?;
		let (effect_token, authorized_at_ms) = internal_worker_authorization();
		invocation
			.args_committed(pb::ArgsCommitted {
				invocation_id: invocation_id.to_string(),
				raw: Bytes::from(raw),
				effect_token,
				authorized_at_ms,
				..pb::ArgsCommitted::default()
			})
			.map_err(|error| {
				BridgeHostError::because("failed to commit prelude helper arguments", error)
			})?;
		loop {
			let event = invocation.next().await.map_err(|_| {
				BridgeHostError::message("prelude helper invocation channel closed before completion")
			})?;
			match event {
				ExtHostEvent::Update(_) => {},
				ExtHostEvent::Complete(complete) => {
					let details = materialize_worker_details(&self.blobs, &complete).map_err(|_| {
						BridgeHostError::message(
							"prelude helper result artifact is unavailable or exceeds the bounded \
							 projection",
						)
					})?;
					match complete.kind {
						ExtHostOutcomeKind::Ok => {
							return serde_json::from_slice(&details).map_err(|error| {
								BridgeHostError::because("prelude helper returned invalid JSON", error)
							});
						},
						ExtHostOutcomeKind::ArgsRejected => {
							let issue = complete.args_issue.as_ref().ok_or_else(|| {
								BridgeHostError::message(
									"prelude helper rejected its arguments without an argument issue",
								)
							})?;
							let path = if issue.path.is_empty() {
								sf!("<arguments>")
							} else {
								Str::from(issue.path.join("."))
							};
							return Err(BridgeHostError::message(sf!(
								"prelude helper arguments rejected at {path}: expected {} ({})",
								issue.expected,
								issue.kind
							)));
						},
						ExtHostOutcomeKind::Faulted | ExtHostOutcomeKind::Aborted => {
							let kind = if complete.kind == ExtHostOutcomeKind::Faulted {
								"faulted"
							} else {
								"aborted"
							};
							let detail = match serde_json::from_slice::<serde_json::Value>(&details) {
								Ok(serde_json::Value::String(detail)) => Str::from(detail),
								Ok(_) => Str::from_utf8(&details).map_err(|_| {
									BridgeHostError::message(sf!(
										"prelude helper {kind} with invalid UTF-8 details"
									))
								})?,
								Err(error) => {
									return Err(BridgeHostError::because(
										sf!("prelude helper {kind} with invalid JSON details"),
										error,
									));
								},
							};
							return Err(BridgeHostError::message(sf!("prelude helper {kind}: {detail}")));
						},
					}
				},
				ExtHostEvent::Aborted(abort) => {
					return Err(BridgeHostError::message(sf!(
						"prelude helper invocation aborted: {}",
						abort.reason
					)));
				},
				ExtHostEvent::ProtocolError(error) => {
					return Err(BridgeHostError::message(sf!(
						"prelude helper protocol error: {}",
						error.message
					)));
				},
			}
		}
	}
}

#[derive(Default)]
pub(in crate::server) struct ProductionDeviceCatalogObserver;

impl DeviceCatalogObserver for ProductionDeviceCatalogObserver {
	fn catalog_changed(&self, epoch: u64, catalog: Arc<[DynamicDeviceCatalogEntry]>) {
		tracing::debug!(epoch, entries = catalog.len(), "dynamic device catalog changed");
	}
}

pub(in crate::server) struct ProductionDeviceInvocationAdmission;

#[async_trait::async_trait]
impl DeviceInvocationAdmission for ProductionDeviceInvocationAdmission {
	async fn admit(
		&self,
		caller: &ControlRequestContext,
		target: &DynamicDeviceCatalogEntry,
		_arguments: &serde_json::Map<String, serde_json::Value>,
	) -> Result<(), ControlProtocolError> {
		if caller.connection.extension == target.claimant
			|| caller.connection.capabilities.contains("devices.invoke")
		{
			Ok(())
		} else {
			Err(ControlProtocolError::new(
				"AccessDenied",
				"cross-extension device invocation requires devices.invoke",
			))
		}
	}
}
