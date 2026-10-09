//! Envd-owned authority bridges behind the embedded shell's `dyn` builtin.

use std::{collections::BTreeSet, future::Future, sync::Arc};

use bytes::Bytes;
use futures::StreamExt as _;
use omp_agent::{ApprovalRoute, GateEvent, GateOutcome, HookGate};
use omp_core::{Duration, DurationUnit, Hash32, Str, sf};
use omp_proto::toolhost::v1::HookEventId;
use omp_shell_builtins::{
	DynCallOutput, DynDevice, DynFault, DynFuture, DynHost as ShellDynHost, DynOutput, DynSchema,
};
use omp_tool::{
	Confinement, DevicePath, Diag, DiagEnvelope, DiagKind, ErasedEv, ErasedOutcome, ErasedStream,
	IncomingParams, InvocationPins, Part, PromptCaps, Registry, RegistryError, ToolIdentity,
	ToolRoute,
};
use omp_tools::{
	device::{DeviceCatalog, DeviceInvokeRequest, ErasedDeviceInvoker},
	staging::{ProposalDecision, ProposalRejection, StagedProposalRegistry},
};
use parking_lot::Mutex;
use serde_json::{Map, Value, json};
use tokio_util::sync::CancellationToken;

use super::{
	admission::{DynamicAdmission, DynamicAdmissionError, DynamicInvocationSource},
	approval_relay::OwnedApprovals,
	blobs::{BlobHost, BlobId},
	fetch_host::{FetchHost, FetchHostNamer, NamedFetches},
	mcp::manager::McpManager,
	reflection_relay::OwnedReflection,
};

tokio::task_local! {
	static EXEC_DIAGS: Arc<Mutex<Vec<Diag>>>;
}

/// Runs one shell execution with a concurrency-safe diagnostic sink.
pub(crate) async fn scope_exec_diags<T>(
	sink: Arc<Mutex<Vec<Diag>>>,
	future: impl Future<Output = T>,
) -> T {
	EXEC_DIAGS.scope(sink, future).await
}

fn capture_exec_diags(diags: &[Diag]) {
	if diags.is_empty() {
		return;
	}
	let _ = EXEC_DIAGS.try_with(|sink| sink.lock().extend_from_slice(diags));
}

/// The relays of the connection that issued the command a session shell is
/// running, which a nested `dyn` call answers to.
///
/// A project daemon's host binds no approval route or reflection authority:
/// a `dyn` target that needs a prompt asks the issuing connection, and a
/// `dyn reflect` synthesizes on its session. Without them (an in-process
/// composition's command, or one issued by no relaying connection) the host's
/// own bindings answer.
#[derive(Clone, Default)]
pub(crate) struct CommandIssuer {
	/// The approval relay of the connection that issued the command.
	pub(crate) approvals:  Option<OwnedApprovals>,
	/// The reflection relay of the connection that issued the command.
	pub(crate) reflection: Option<OwnedReflection>,
}

/// One session shell's record of the command it is running.
///
/// The shell runs one command at a time, and every builtin of that command,
/// in a pipeline stage or a subshell too, reads the same record, so a nested
/// call answers to the command that issued it. A background job still running
/// after its command finished reads whatever command runs next in the same
/// shell, or nothing between commands.
#[derive(Clone, Default)]
pub(crate) struct IssuerCell(Arc<Mutex<CommandIssuer>>);

impl IssuerCell {
	/// Records `issuer` as the running command's until the returned guard
	/// drops.
	pub(crate) fn enter(&self, issuer: CommandIssuer) -> IssuedCommand<'_> {
		*self.0.lock() = issuer;
		IssuedCommand { cell: self }
	}

	fn current(&self) -> CommandIssuer {
		self.0.lock().clone()
	}
}

/// Clears the running command's relays when the command's run ends.
pub(crate) struct IssuedCommand<'c> {
	cell: &'c IssuerCell,
}

impl Drop for IssuedCommand<'_> {
	fn drop(&mut self) {
		*self.cell.0.lock() = CommandIssuer::default();
	}
}

/// One session shell's `dyn` builtin: the environment's device host, called
/// with the relays of the command that shell is running.
pub(crate) struct CommandDevices {
	host:   Arc<DynHost>,
	issuer: IssuerCell,
}

impl CommandDevices {
	/// Serves `host` to a shell whose running command `issuer` records.
	pub(crate) const fn new(host: Arc<DynHost>, issuer: IssuerCell) -> Self {
		Self { host, issuer }
	}
}

impl ShellDynHost for CommandDevices {
	fn list(&self) -> DynFuture<'_, Vec<DynDevice>> {
		self.host.list()
	}

	fn schema(&self, name: &str) -> DynFuture<'_, DynSchema> {
		self.host.schema(name)
	}

	fn call(
		&self,
		name: &str,
		args: Value,
		cancellation: CancellationToken,
	) -> DynFuture<'_, DynCallOutput> {
		self
			.host
			.call_issued(name, args, cancellation, self.issuer.current())
	}
}

/// Envd-owned loopback bridge behind the `dyn` shell builtin.
pub struct DynHost {
	catalog:            DeviceCatalog,
	invoker:            Arc<dyn ErasedDeviceInvoker>,
	proposals:          StagedProposalRegistry,
	hooks:              Arc<HookGate>,
	blobs:              BlobHost,
	mcp:                Arc<McpManager>,
	admission:          DynamicAdmission,
	fetch_hosts:        FetchHostNamer,
	next_invocation_id: std::sync::atomic::AtomicU64,
}

impl DynHost {
	/// Binds one live device catalog, worker dispatcher, proposal registry,
	/// session hook gate, and the environment resolvers that name the hosts a
	/// device call fetches from.
	#[expect(
		clippy::too_many_arguments,
		reason = "the device host joins every authority a nested call reaches"
	)]
	pub(crate) fn new(
		catalog: DeviceCatalog,
		invoker: Arc<dyn ErasedDeviceInvoker>,
		proposals: StagedProposalRegistry,
		hooks: Arc<HookGate>,
		blobs: BlobHost,
		mcp: Arc<McpManager>,
		admission: DynamicAdmission,
		fetch_hosts: FetchHostNamer,
	) -> Self {
		Self {
			catalog,
			invoker,
			proposals,
			hooks,
			blobs,
			mcp,
			admission,
			fetch_hosts,
			next_invocation_id: std::sync::atomic::AtomicU64::new(1),
		}
	}

	/// Binds or clears the session's live approval route for nested calls.
	pub(crate) fn bind_approval_route(&self, route: Option<ApprovalRoute>) {
		self.admission.bind_route(route);
	}

	fn invocation_id(&self) -> Str {
		let sequence = self
			.next_invocation_id
			.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
		sf!("dyn-{sequence}")
	}

	async fn visible_names(
		&self,
		registry: &Registry,
		dynamic: &[DynDevice],
	) -> Result<Option<BTreeSet<Str>>, DynFault> {
		if !self.hooks.subscribed(HookEventId::HookEventDeviceList) {
			return Ok(None);
		}
		let mut catalog_hash = Hash32::hasher();
		catalog_hash.update(registry.device_hash().as_bytes());
		for device in dynamic {
			catalog_hash.update(device.name.as_bytes());
			catalog_hash.update(b"\0");
			if let Some(description) = &device.description {
				catalog_hash.update(description.as_bytes());
			}
			catalog_hash.update(b"\0");
		}
		let device_hash = catalog_hash.finalize();
		let mut devices = registry
			.devices()
			.map(device_event_json)
			.collect::<Vec<_>>();
		devices.extend(dynamic.iter().map(|device| {
			json!({
				"name": device.name,
				"path": device.name,
				"summary": device.description,
				"place": "mcp",
				"mounted": true,
				"enabled": true,
				"available": true,
			})
		}));
		let payload = serde_json::to_vec(&json!({ "devices": devices, "turn_id": null }))
			.map(Bytes::from)
			.map_err(|_| DynFault::new("failed to encode the dynamic-device catalog"))?;
		let outcome = self
			.hooks
			.gate(
				HookEventId::HookEventDeviceList,
				GateEvent::new(sf!("device_list:{}", device_hash.to_hex()), payload),
			)
			.await;
		let effective = match outcome {
			GateOutcome::Allow { event, .. } => event.effective_args,
			GateOutcome::Deny { reason, .. } => return Err(DynFault::new(reason)),
			GateOutcome::Approval { .. } => {
				return Err(DynFault::new("device listing cannot require approval"));
			},
		};
		let effective: Value = serde_json::from_slice(&effective)
			.map_err(|_| DynFault::new("device-list hook returned malformed JSON"))?;
		let devices = effective
			.get("devices")
			.and_then(Value::as_array)
			.ok_or_else(|| DynFault::new("device-list hook omitted its effective devices"))?;
		Ok(Some(
			devices
				.iter()
				.filter_map(|device| device.get("name").and_then(Value::as_str).map(Str::new))
				.collect(),
		))
	}

	async fn call_mcp(
		&self,
		name: Str,
		args: Value,
		cancellation: CancellationToken,
		approvals: Option<&OwnedApprovals>,
	) -> Result<DynCallOutput, DynFault> {
		if let Some((effects, server)) = self.mcp.dynamic_effects(name.as_str()) {
			// MCP servers run outside the sandbox: stdio servers are spawned
			// unconfined and HTTP servers are reached from this host. A fetch
			// is asked per server.
			self
				.admission
				.admit(
					self.invocation_id(),
					name.clone(),
					&effects,
					Confinement::Host,
					&NamedFetches::named([FetchHost::mcp(server)]),
					DynamicInvocationSource::ShellDyn,
					approvals,
					cancellation.clone(),
				)
				.await
				.map_err(|error| DynFault::new(error.to_string()))?;
		}
		self.mcp.call(name.as_str(), args, cancellation).await
	}

	fn proposal_schema(name: &str) -> Option<DynSchema> {
		matches!(name, "resolve" | "reject").then(|| DynSchema {
			name:        Str::new(name),
			description: Some(Str::new_static("Finalize one exact staged proposal.")),
			schema:      json!({
				"type": "object",
				"properties": {
					"proposal_id": {
						"type": "string",
						"minLength": 1,
						"description": "Exact pending proposal id printed by the staging tool."
					},
					"reason": {
						"type": "string",
						"minLength": 1,
						"description": "One-sentence decision reason."
					}
				},
				"required": ["proposal_id", "reason"],
				"additionalProperties": false
			}),
		})
	}

	fn finalize_proposal(&self, name: &str, args: &Value) -> Result<DynCallOutput, DynFault> {
		let object = args
			.as_object()
			.ok_or_else(|| DynFault::new("proposal finalization arguments must be an object"))?;
		if object
			.keys()
			.any(|key| !matches!(key.as_str(), "proposal_id" | "reason"))
		{
			return Err(DynFault::new(
				"proposal finalization accepts only `proposal_id` and `reason`",
			));
		}
		let id = args
			.get("proposal_id")
			.and_then(Value::as_str)
			.map(str::trim)
			.filter(|id| !id.is_empty())
			.ok_or_else(|| DynFault::new("an exact staged proposal id is required"))?;
		let reason = args
			.get("reason")
			.and_then(Value::as_str)
			.map(str::trim)
			.filter(|reason| !reason.is_empty())
			.ok_or_else(|| DynFault::new("a one-sentence reason is required"))?;
		let decision = if name == "resolve" {
			ProposalDecision::Resolve { reason: Str::new(reason) }
		} else {
			ProposalDecision::Reject(ProposalRejection::Requested { reason: Str::new(reason) })
		};
		let outcome = self
			.proposals
			.finalize(id, decision)
			.map_err(|error| DynFault::new(error.to_string()))?;
		let diags = recovery_snapshot_diag(&outcome.payload)
			.into_iter()
			.collect();
		let payload =
			serde_json::to_value(outcome).map_err(|error| DynFault::new(error.to_string()))?;
		Ok(DynCallOutput { output: DynOutput::Json(payload), diags })
	}
}

fn recovery_snapshot_diag(payload: &Value) -> Option<Diag> {
	let recovery = payload.get("recovery_root").and_then(Value::as_str)?;
	Some(if recovery.starts_with("artifact://") {
		Diag::info(DiagKind::Snapshot, "Recovery snapshot recorded").artifact(Str::new(recovery))
	} else {
		Diag::info(DiagKind::Snapshot, sf!("Recovery snapshot recorded at {recovery}"))
	})
}

impl DynHost {
	fn list(&self) -> DynFuture<'_, Vec<DynDevice>> {
		Box::pin(async move {
			let registry = self
				.catalog
				.registry()
				.ok_or_else(|| DynFault::new("device catalog is not available in this session"))?;
			let mcp = self.mcp.list().await?;
			let visible = self.visible_names(&registry, &mcp).await?;
			let mut devices = registry
				.devices()
				.map(|device| DynDevice {
					name:        device.name.clone(),
					description: Some(device.summary.clone()),
				})
				.chain(mcp)
				.filter(|device| {
					visible
						.as_ref()
						.is_none_or(|names| names.contains(device.name.as_str()))
				})
				.collect::<Vec<_>>();
			if self.proposals.latest_pending().is_some() {
				devices.extend([
					DynDevice {
						name:        sf!("resolve"),
						description: Some(sf!("Apply one exact staged proposal.")),
					},
					DynDevice {
						name:        sf!("reject"),
						description: Some(sf!("Discard one exact staged proposal.")),
					},
				]);
			}
			devices.sort_by(|left, right| left.name.cmp(&right.name));
			devices.dedup_by(|left, right| left.name == right.name);
			Ok(devices)
		})
	}

	fn schema(&self, name: &str) -> DynFuture<'_, DynSchema> {
		let name = Str::new(name);
		Box::pin(async move {
			if let Some(schema) = Self::proposal_schema(name.as_str()) {
				return Ok(schema);
			}
			let registry = self
				.catalog
				.registry()
				.ok_or_else(|| DynFault::new("device catalog is not available in this session"))?;
			if let Ok(path) = DevicePath::parse(name.as_str())
				&& let Some(device) = registry
					.devices()
					.find(|device| device.name.as_str() == path.root())
			{
				let schema = serde_json::from_slice(device.schema)
					.map_err(|_| DynFault::new(format!("device `{name}` has an invalid JSON schema")))?;
				return Ok(DynSchema { name, description: Some(device.summary.clone()), schema });
			}
			self.mcp.schema(name.as_str()).await
		})
	}

	/// Invokes one target for the command `issuer` records: its admission
	/// prompt asks the issuing connection when that connection relays
	/// approvals, and a reflection it needs synthesizes on that connection's
	/// session.
	pub(crate) fn call_issued(
		&self,
		name: &str,
		args: Value,
		cancellation: CancellationToken,
		issuer: CommandIssuer,
	) -> DynFuture<'_, DynCallOutput> {
		let name = Str::new(name);
		Box::pin(async move {
			let CommandIssuer { approvals, reflection } = issuer;
			let result = async {
				if matches!(name.as_str(), "resolve" | "reject") {
					if cancellation.is_cancelled() {
						return Err(DynFault::new("staged proposal finalization was cancelled"));
					}
					// Finalization is the foreground mutation boundary: once the
					// exact transaction starts, it runs through commit or rollback.
					return self.finalize_proposal(name.as_str(), &args);
				}
				let registry = self
					.catalog
					.registry()
					.ok_or_else(|| DynFault::new("device catalog is not available in this session"))?;
				let Ok(path) = DevicePath::parse(name.as_str()) else {
					return self
						.call_mcp(name, args, cancellation, approvals.as_ref())
						.await;
				};
				let target = match registry.resolve_device(&path) {
					Ok(target) => target,
					Err(_)
						if registry
							.devices()
							.any(|device| device.name.as_str() == path.root()) =>
					{
						return Err(DynFault::new(format!(
							"device `{name}` rejected its path arguments"
						)));
					},
					Err(_) => {
						return self
							.call_mcp(name, args, cancellation, approvals.as_ref())
							.await;
					},
				};
				let identity = target.identity();
				let raw = Str::new(args.to_string());
				// A call is judged by its arguments, as a slot's is at commit: a
				// device that scopes its effects to them is admitted on that
				// envelope (a local search is a read, never asked under any
				// mode), and its fetches ask for the hosts they reach. It is
				// judged inside the pins its executor runs in, so the hosts asked
				// for are the ones it reaches. An envelope beyond the declared
				// maximum is refused, never admitted on that maximum.
				let pins = Arc::new(InvocationPins::default());
				let judged = InvocationPins::judge(Some(&pins), || {
					let effects = registry.device_invocation_effects(&path, &raw)?;
					let fetches = if effects.fetch.is_some() {
						self
							.fetch_hosts
							.name(&registry.device_fetch_locators(&path, &raw))
					} else {
						NamedFetches::default()
					};
					Ok::<_, RegistryError>((effects, fetches))
				});
				let invocation_id = self.invocation_id();
				async {
					let (effects, fetches) = judged.map_err(|source| {
						DynamicAdmissionError::Unjudged { target: target.name.clone(), source }
					})?;
					self
						.admission
						.admit(
							invocation_id.clone(),
							target.name.clone(),
							&effects,
							target.confinement,
							&fetches,
							DynamicInvocationSource::ShellDyn,
							approvals.as_ref(),
							cancellation.clone(),
						)
						.await
				}
				.await
				.map_err(|error| DynFault::new(error.to_string()))?;
				let args_json = Bytes::from(raw.clone());
				// The registry reads a dropped feed as an aborted invocation, so a
				// native target's feed must outlive its stream; otherwise any target
				// that awaits loses the race to `InputDropped`.
				let mut feed = None;
				let mut stream = match target.route.clone() {
					ToolRoute::Native => {
						let (committed, params) =
							IncomingParams::channel_for(None, Some(invocation_id.clone()));
						committed.args_committed(raw).map_err(|_| {
							DynFault::new("device argument channel closed before dispatch")
						})?;
						feed = Some(committed);
						registry
							.invoke_device(&path, params)
							.map_err(|error| DynFault::new(format!("device dispatch failed: {error}")))?
					},
					ToolRoute::Remote => {
						return Err(DynFault::new("device is owned by the remote environment host"));
					},
					ToolRoute::Worker { site, name: worker } => {
						self
							.invoker
							.invoke(DeviceInvokeRequest {
								path,
								name: target.name.clone(),
								rev: Str::from(target.rev.to_string()),
								owner: Some(target.claimant.clone()),
								site: Some(site),
								worker: Some(worker),
								invocation_id,
								deadline: Duration::new(5, DurationUnit::Minutes),
								args_json,
							})
							.await
					},
				};
				// A native target runs on this task: a reflection it needs
				// synthesizes on the issuing connection's session, and it
				// reaches what its judgment pinned.
				let output = InvocationPins::scope(
					Some(pins),
					crate::tools::with_invocation_reflection(
						reflection,
						consume(&registry, &self.blobs, &identity, &mut stream, cancellation),
					),
				)
				.await;
				drop(feed);
				output
			}
			.await;
			if let Ok(call) = &result {
				capture_exec_diags(&call.diags);
			}
			result
		})
	}
}

async fn consume(
	registry: &Registry,
	blobs: &BlobHost,
	identity: &ToolIdentity,
	stream: &mut ErasedStream<'_>,
	cancellation: CancellationToken,
) -> Result<DynCallOutput, DynFault> {
	let mut diags = Vec::new();
	loop {
		let event = tokio::select! {
			biased;
			() = cancellation.cancelled() => {
				return Err(DynFault::new("dynamic device invocation was cancelled"));
			},
			event = stream.next() => event,
		};
		match event {
			Some(Ok(ErasedEv::Update(update))) => {
				if let Ok(envelope) = serde_json::from_slice::<DiagEnvelope>(&update) {
					diags.push(envelope.diag);
				}
			},
			Some(Ok(ErasedEv::Done(ErasedOutcome::Done { verdict, .. }))) => {
				let output = project_result(registry, blobs, identity, &verdict)?;
				return Ok(DynCallOutput { output, diags });
			},
			Some(Ok(ErasedEv::Done(ErasedOutcome::Detached(job)))) => {
				return Ok(DynCallOutput {
					output: DynOutput::Text(sf!("detached job: {}", job.id)),
					diags,
				});
			},
			Some(Err(error)) => {
				return Err(DynFault::new(format!("device dispatch failed: {error}")));
			},
			None => return Err(DynFault::new("device dispatch ended without an outcome")),
		}
	}
}

fn project_result(
	registry: &Registry,
	blobs: &BlobHost,
	identity: &ToolIdentity,
	verdict: &[u8],
) -> Result<DynOutput, DynFault> {
	let caps = PromptCaps {
		maximum_parts:      u16::MAX,
		maximum_text_bytes: u32::MAX,
		media:              true,
		dialect:            Default::default(),
		model_class:        Default::default(),
	};
	let output = match registry.prompt(identity, verdict, &caps) {
		Ok(Some(parts)) => {
			let outputs = parts
				.iter()
				.cloned()
				.map(|part| project_part(blobs, part))
				.collect::<Result<Vec<_>, _>>()?;
			join_outputs(outputs)
		},
		Ok(None) => DynOutput::Text(Str::default()),
		Err(RegistryError::UnsupportedExternal { .. }) => project_external_verdict(verdict)?,
		Err(error) => {
			return Err(DynFault::new(format!("device result projection failed: {error}")));
		},
	};
	if faulted(verdict) {
		Err(DynFault::new(output_error_message(output)))
	} else {
		Ok(output)
	}
}

fn project_part(blobs: &BlobHost, part: Part) -> Result<DynOutput, DynFault> {
	match part {
		Part::Text { text } => Ok(DynOutput::Text(text)),
		Part::Json { json } => serde_json::from_slice(&json)
			.map(DynOutput::Json)
			.map_err(|_| DynFault::new("device returned a malformed JSON output part")),
		Part::Blob { blob, .. } => {
			let hash = blob
				.hash
				.parse::<Hash32>()
				.map_err(|_| DynFault::new("device returned an invalid blob identity"))?;
			let bytes = blobs
				.get(BlobId { hash: hash.into_bytes(), size: blob.byte_len })
				.map_err(|_| DynFault::new("device output blob is unavailable"))?;
			Ok(DynOutput::Blob { mime: blob.media_type, bytes })
		},
	}
}

fn join_outputs(mut outputs: Vec<DynOutput>) -> DynOutput {
	if outputs.len() == 1 {
		outputs.pop().expect("one output")
	} else {
		DynOutput::Parts(outputs)
	}
}

fn project_external_verdict(verdict: &[u8]) -> Result<DynOutput, DynFault> {
	let verdict = serde_json::from_slice::<Value>(verdict)
		.map_err(|_| DynFault::new("external device returned a malformed verdict"))?;
	let value = verdict
		.get("value")
		.cloned()
		.ok_or_else(|| DynFault::new("external device verdict omitted its value"))?;
	Ok(match value {
		Value::String(text) => DynOutput::Text(Str::new(text)),
		other => DynOutput::Json(other),
	})
}

fn output_error_message(output: DynOutput) -> Str {
	match output {
		DynOutput::Text(text) | DynOutput::Markdown(text) => text,
		DynOutput::Json(value) => Str::new(value.to_string()),
		DynOutput::Blob { mime, .. } => sf!("device returned a binary error payload ({mime})"),
		DynOutput::Parts(parts) => {
			let text = parts
				.into_iter()
				.map(output_error_message)
				.filter(|part| !part.is_empty())
				.collect::<Vec<_>>();
			Str::new(text.join("\n"))
		},
	}
}

fn faulted(verdict: &[u8]) -> bool {
	serde_json::from_slice::<Value>(verdict)
		.ok()
		.is_some_and(|value| {
			value
				.get("kind")
				.and_then(Value::as_str)
				.is_some_and(|kind| matches!(kind, "fault" | "faulted"))
		})
}

fn device_event_json(device: omp_tool::MountedDevice<'_>) -> Value {
	let place = match device.route {
		ToolRoute::Native => String::from("env"),
		ToolRoute::Remote => String::from("remote"),
		ToolRoute::Worker { name, .. } => format!("worker:{name}"),
	};
	let mut row = Map::from_iter([
		("name".to_owned(), Value::String(device.name.to_string())),
		("family".to_owned(), Value::String(device.rev.family.to_string())),
		("rev".to_owned(), Value::from(device.rev.n)),
		("claimant".to_owned(), Value::String(device.claimant.to_string())),
		("path".to_owned(), Value::String(device.name.to_string())),
		("summary".to_owned(), Value::String(device.summary.to_string())),
		("place".to_owned(), Value::String(place)),
		("mounted".to_owned(), Value::Bool(true)),
		("enabled".to_owned(), Value::Bool(true)),
		("available".to_owned(), Value::Bool(true)),
	]);
	if let Some(metadata) = device.metadata {
		let mut provenance = Map::new();
		for (name, value) in [
			("publisher", metadata.publisher.as_ref()),
			("extension_id", metadata.extension_id.as_ref()),
			("version", metadata.version.as_ref()),
			("artifact_digest", metadata.artifact_digest.as_ref()),
			("layer", metadata.layer.as_ref()),
			("tier", metadata.tier.as_ref()),
		] {
			if let Some(value) = value {
				provenance.insert(name.to_owned(), Value::String(value.to_string()));
			}
		}
		if let Some(generation) = metadata.generation {
			provenance.insert("generation".to_owned(), Value::from(generation));
		}
		if !provenance.is_empty() {
			row.insert("provenance".to_owned(), Value::Object(provenance));
		}
	}
	Value::Object(row)
}

#[cfg(test)]
mod tests {
	use std::{
		future::Future,
		path::Path,
		sync::atomic::{AtomicUsize, Ordering},
	};

	use futures::Stream;
	use omp_tool::{
		Claims, Confinement, Constraint, Effects, Ev, ExecEffects, Precedence, Presentation, Rev,
		Tool, ToolSpec, ToolTerminal,
	};
	use omp_tools::device::{DeviceInvokeRequest, DeviceInvoker};

	use super::*;
	use crate::{
		admission::ApprovalMode,
		mcp::{McpService, manager::ProductionConnector},
	};

	struct CountingDevice {
		spec:  ToolSpec,
		calls: Arc<AtomicUsize>,
	}

	impl Tool for CountingDevice {
		type Fault = Value;
		type Params = Value;
		type Payload = Value;
		type Update = Value;

		fn spec(&self) -> &ToolSpec {
			&self.spec
		}

		fn call<'c>(
			&'c self,
			_incoming: IncomingParams<'c>,
		) -> impl Stream<Item = Ev<Value, Value, Value>> + Send + 'c {
			self.calls.fetch_add(1, Ordering::Relaxed);
			futures::stream::once(async {
				Ev::Done(ToolTerminal::Done { result: Ok(json!({"ok": true})), useless: false })
			})
		}

		fn prompt(&self, _view: Result<&Value, &Value>, _caps: &PromptCaps) -> Vec<Part> {
			Vec::new()
		}
	}

	#[derive(Clone)]
	struct NoWorker;

	impl DeviceInvoker for NoWorker {
		fn invoke(
			&self,
			_request: DeviceInvokeRequest,
		) -> impl Future<Output = ErasedStream<'static>> + Send {
			async { Box::pin(futures::stream::empty()) as ErasedStream<'static> }
		}
	}

	#[test]
	fn proposal_dyn_schema_requires_exact_identity_and_reason() {
		for name in ["resolve", "reject"] {
			let schema = DynHost::proposal_schema(name).expect("proposal device schema");
			assert_eq!(schema.schema["required"], json!(["proposal_id", "reason"]));
			assert_eq!(schema.schema["additionalProperties"], false);
			assert_eq!(
				schema.schema["properties"]["proposal_id"]["description"],
				"Exact pending proposal id printed by the staging tool."
			);
		}
	}

	#[tokio::test]
	async fn recovery_snapshot_is_captured_out_of_band() {
		let artifact =
			"artifact://sha256/0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
		let diag = recovery_snapshot_diag(&json!({ "recovery_root": artifact }))
			.expect("snapshot diagnostic");
		assert_eq!(diag.native_kind(), Some(DiagKind::Snapshot));
		assert_eq!(diag.artifact.as_deref(), Some(artifact));

		let sink = Arc::new(Mutex::new(Vec::new()));
		scope_exec_diags(Arc::clone(&sink), async {
			capture_exec_diags(std::slice::from_ref(&diag));
		})
		.await;
		assert_eq!(sink.lock().as_slice(), &[diag]);
	}

	#[test]
	fn native_projection_preserves_json_and_blob_parts() {
		let scratch = tempfile::tempdir().expect("scratch");
		let blobs = BlobHost::open(scratch.path()).expect("blobs");
		let id = blobs.put(b"image").expect("store image");
		let json = project_part(&blobs, Part::Json { json: Bytes::from_static(br#"{"ok":true}"#) })
			.expect("JSON part");
		assert_eq!(json, DynOutput::Json(json!({"ok": true})));
		let blob = project_part(&blobs, Part::Blob {
			blob: omp_tool::BlobRef {
				hash:       Str::new(Hash32::new(id.hash).to_hex().as_str()),
				media_type: sf!("image/png"),
				byte_len:   id.size,
			},
			alt:  None,
		})
		.expect("blob part");
		assert_eq!(blob, DynOutput::Blob {
			mime:  sf!("image/png"),
			bytes: Bytes::from_static(b"image"),
		});
	}

	fn counting_device(
		name: &'static str,
		effects: Effects,
		confinement: Confinement,
		calls: &Arc<AtomicUsize>,
	) -> CountingDevice {
		CountingDevice {
			spec:  ToolSpec {
				name: sf!(name),
				rev: Rev { family: sf!("test"), n: 1 },
				description: sf!("test device"),
				schema: Bytes::from_static(br#"{"type":"object","properties":{}}"#),
				constraint: Constraint::None,
				effects,
				confinement,
				projection_code: [1; 32],
			},
			calls: Arc::clone(calls),
		}
	}

	/// A `dyn` host over `devices`; the catalog holds the registry weakly, so
	/// the caller keeps the returned registry alive.
	fn dyn_host(
		scratch: &Path,
		devices: impl IntoIterator<Item = CountingDevice>,
		admission: DynamicAdmission,
	) -> (DynHost, Arc<Registry>) {
		let mut registry = Registry::new();
		for device in devices {
			register_device(&mut registry, device);
		}
		dyn_host_over(scratch, registry, admission)
	}

	fn register_device<T: Tool>(registry: &mut Registry, device: T) {
		registry
			.register(device, Presentation::Device, Claims {
				precedence: Precedence::ENHANCEMENT,
				claimant:   sf!("omp/test"),
				replaces:   None,
			})
			.expect("register target");
	}

	/// A `dyn` host over `registry`'s devices.
	fn dyn_host_over(
		scratch: &Path,
		registry: Registry,
		admission: DynamicAdmission,
	) -> (DynHost, Arc<Registry>) {
		let registry = Arc::new(registry);
		let catalog = DeviceCatalog::default();
		catalog
			.install_registry(Arc::clone(&registry))
			.expect("install catalog");
		let blobs = BlobHost::open(scratch.join("blobs")).expect("blobs");
		let mcp_service = McpService::open(scratch.join("mcp.sqlite3")).expect("MCP service");
		let mcp = McpManager::new(
			Arc::clone(&mcp_service),
			Arc::new(ProductionConnector::new(scratch.to_path_buf())),
			Arc::from([]),
			scratch.join("local"),
		);
		// No internal resolvers: an http(s) fetch is still named by its
		// authored host, an internal one is left unnamed.
		let resources = Arc::new(omp_tools::read::resolver::ResolverTable::default());
		let host = DynHost::new(
			catalog,
			Arc::new(NoWorker),
			StagedProposalRegistry::new(),
			Arc::new(HookGate::channel().0),
			blobs,
			mcp,
			admission,
			FetchHostNamer::new(&resources),
		);
		(host, registry)
	}

	fn commands_and_network() -> Effects {
		Effects {
			exec: Some(ExecEffects { commands: Arc::from([sf!("*")]), network: true }),
			..Effects::empty()
		}
	}

	#[tokio::test]
	async fn denied_dynamic_effects_never_invoke_native_target() {
		let scratch = tempfile::tempdir().expect("scratch");
		let calls = Arc::new(AtomicUsize::new(0));
		let admission = DynamicAdmission::new(
			crate::admission::ConfiguredApproval {
				mode:       ApprovalMode::AlwaysAsk,
				provenance: crate::admission::Provenance::Explicit,
			},
			crate::admission::SandboxState::Off,
			std::collections::BTreeMap::new(),
			None,
		);
		let (host, _registry) = dyn_host(
			scratch.path(),
			[counting_device("danger", commands_and_network(), Confinement::Host, &calls)],
			admission,
		);

		let refused = host
			.call_issued("danger", json!({}), CancellationToken::new(), CommandIssuer::default())
			.await
			.expect_err("an always-ask exec-tier target needs a prompt");
		assert!(
			refused
				.message
				.contains("requires an unavailable approval route"),
			"{refused:?}"
		);
		assert_eq!(calls.load(Ordering::Relaxed), 0);
	}

	/// Under the shipped (defaulted) `yolo` with an active sandbox, a `dyn`
	/// target that runs on the host is admitted as if no sandbox existed: its
	/// exec tier needs a prompt (refused here, with no route), its read tier
	/// proceeds, and only a target the sandbox confines inherits the `yolo`.
	#[tokio::test]
	async fn host_dyn_devices_are_not_auto_approved_inside_a_sandbox() {
		let scratch = tempfile::tempdir().expect("scratch");
		let network = Arc::new(AtomicUsize::new(0));
		let peek = Arc::new(AtomicUsize::new(0));
		let sandboxed = Arc::new(AtomicUsize::new(0));
		let admission = DynamicAdmission::new(
			crate::admission::ConfiguredApproval {
				mode:       ApprovalMode::Yolo,
				provenance: crate::admission::Provenance::Default,
			},
			crate::admission::SandboxState::Active,
			std::collections::BTreeMap::new(),
			None,
		);
		let (host, _registry) = dyn_host(
			scratch.path(),
			[
				counting_device("net", commands_and_network(), Confinement::Host, &network),
				counting_device("peek", Effects::empty(), Confinement::Host, &peek),
				counting_device(
					"confined",
					commands_and_network(),
					Confinement::ExecSandbox,
					&sandboxed,
				),
			],
			admission,
		);

		let refused = host
			.call_issued("net", json!({}), CancellationToken::new(), CommandIssuer::default())
			.await
			.expect_err("a host exec-tier target needs a prompt");
		assert!(
			refused
				.message
				.contains("requires an unavailable approval route"),
			"{refused:?}"
		);
		assert_eq!(network.load(Ordering::Relaxed), 0);
		host
			.call_issued("peek", json!({}), CancellationToken::new(), CommandIssuer::default())
			.await
			.expect("a host read-tier target proceeds");
		assert_eq!(peek.load(Ordering::Relaxed), 1);
		host
			.call_issued("confined", json!({}), CancellationToken::new(), CommandIssuer::default())
			.await
			.expect("a sandboxed target keeps the sandbox-kept yolo");
		assert_eq!(sandboxed.load(Ordering::Relaxed), 1);
	}

	fn admission(mode: ApprovalMode) -> DynamicAdmission {
		DynamicAdmission::new(
			crate::admission::ConfiguredApproval {
				mode,
				provenance: crate::admission::Provenance::Explicit,
			},
			crate::admission::SandboxState::Off,
			std::collections::BTreeMap::new(),
			None,
		)
	}

	/// A command's connection relay decides its `dyn` admission: the prompt
	/// travels on the command's request, an approval runs the target once, a
	/// refusal never runs it, and the host binds no route at all.
	#[tokio::test]
	async fn a_command_relay_decides_its_dyn_admission() {
		use crate::approval_relay::ConnectionApprovals;

		let scratch = tempfile::tempdir().expect("scratch");
		let calls = Arc::new(AtomicUsize::new(0));
		let (host, _registry) = dyn_host(
			scratch.path(),
			[counting_device("danger", commands_and_network(), Confinement::Host, &calls)],
			admission(ApprovalMode::AlwaysAsk),
		);
		let (responses, frames) = flume::bounded(4);
		let approvals = ConnectionApprovals::new(responses);
		let issuer = CommandIssuer { approvals: Some(approvals.owned(9)), reflection: None };
		for (approved, expected_calls) in [(true, 1), (false, 1)] {
			let call = host.call_issued("danger", json!({}), CancellationToken::new(), issuer.clone());
			tokio::pin!(call);
			let frame = tokio::select! {
				frame = frames.recv_async() => frame.expect("relayed admission prompt"),
				result = &mut call => panic!("the admission settled unasked: {result:?}"),
			};
			assert_eq!(frame.request_id, 9, "the prompt rides the command's request");
			let Some(omp_proto::env::v1::server_frame::Body::ApprovalQuery(query)) = frame.body else {
				panic!("expected an approval query, got {:?}", frame.body);
			};
			assert_eq!(query.reasons[0].subject, "danger");
			assert_eq!(query.reasons[0].kind, "exec");
			assert_eq!(query.reasons[0].scopes, ["once", "session"]);
			approvals.answer(9, omp_proto::env::v1::ApprovalAnswer {
				query_id: query.query_id,
				decision: Some(omp_proto::env::v1::ApprovalDecision {
					approved,
					scope: "once".to_owned(),
					source: "user".to_owned(),
					..omp_proto::env::v1::ApprovalDecision::default()
				}),
			});
			let result = call.await;
			assert_eq!(result.is_ok(), approved, "{result:?}");
			assert_eq!(calls.load(Ordering::Relaxed), expected_calls);
		}
		// A closed connection fails the prompt closed; no host route answers.
		approvals.disconnect();
		let refused = host
			.call_issued("danger", json!({}), CancellationToken::new(), issuer)
			.await
			.expect_err("a closed relay never falls back");
		assert!(refused.message.contains("denied"), "{refused:?}");
		assert_eq!(calls.load(Ordering::Relaxed), 1);
	}

	/// A `dyn reflect` synthesizes on the issuing connection's session: the
	/// recalled evidence travels on the command's request and the answer
	/// becomes the call's output. Without a relay, and with nothing bound in
	/// process, it answers with the evidence.
	#[tokio::test]
	async fn a_dyn_reflect_synthesizes_on_the_issuing_connection() {
		use omp_memory::{
			MemoryBackend, MemoryRuntime, MnemopiSettings,
			config::EmbeddingVariant,
			runtime::{RuntimeStart, SaveRequest},
		};
		use omp_proto::env::v1::{self as pb, reflection_answer, server_frame};

		use crate::reflection_relay::ConnectionReflection;

		let scratch = tempfile::tempdir().expect("scratch");
		let runtime = MemoryRuntime::start(RuntimeStart {
			session_id:             sf!("dyn-reflect"),
			data_dir:               scratch.path().join("data"),
			workspace_root:         scratch.path().to_path_buf(),
			canonical_primary_root: Some(scratch.path().to_path_buf()),
			backend:                MemoryBackend::Mnemopi,
			mnemopi:                MnemopiSettings {
				embedding_variant: EmbeddingVariant::Disabled,
				..MnemopiSettings::default()
			},
		})
		.expect("Mnemopi runtime");
		runtime
			.save_batch(
				&[SaveRequest { content: "The deploy target is fly.io", context: None }],
				"dyn-reflect",
				0.75,
			)
			.expect("retain the fact");
		let mut registry = Registry::new();
		register_device(
			&mut registry,
			omp_tools::memory::reflect_tool(
				runtime,
				Arc::new(crate::memory::ReflectionBridgeHost::new()),
			),
		);
		let (host, _registry) =
			dyn_host_over(scratch.path(), registry, admission(ApprovalMode::Yolo));
		let args = || json!({ "query": "deploy target", "i": "Proving the relay" });

		let fallback = host
			.call_issued("reflect", args(), CancellationToken::new(), CommandIssuer::default())
			.await
			.expect("evidence fallback");
		let fallback = format!("{:?}", fallback.output);
		assert!(fallback.contains("Based on recalled memories"), "{fallback}");

		let (responses, frames) = flume::bounded(4);
		let reflection = ConnectionReflection::new(responses);
		let issuer = CommandIssuer { approvals: None, reflection: Some(reflection.owned(9)) };
		let call = host.call_issued("reflect", args(), CancellationToken::new(), issuer);
		tokio::pin!(call);
		let frame = tokio::select! {
			frame = frames.recv_async() => frame.expect("relayed reflection"),
			result = &mut call => panic!("the reflection settled unasked: {result:?}"),
		};
		assert_eq!(frame.request_id, 9, "the reflection rides the command's request");
		let Some(server_frame::Body::ReflectionQuery(query)) = frame.body else {
			panic!("expected a reflection query, got {:?}", frame.body);
		};
		assert_eq!(query.question, "deploy target");
		assert_eq!(query.evidence, ["The deploy target is fly.io"]);
		reflection.answer(9, pb::ReflectionAnswer {
			query_id: query.query_id,
			body:     Some(reflection_answer::Body::Answer("Deploys go to fly.io.".to_owned())),
		});
		let synthesized = format!("{:?}", call.await.expect("synthesized answer").output);
		assert!(synthesized.contains("Deploys go to fly.io."), "{synthesized}");
		assert!(!synthesized.contains("Based on recalled memories"), "{synthesized}");
	}

	/// Waits for the admission prompt `call` relays, which must not settle
	/// first, and returns its `(kind, subject)` requirements and query id.
	async fn relayed_prompt(
		frames: &flume::Receiver<omp_proto::env::v1::ServerFrame>,
		call: impl Future<Output = Result<DynCallOutput, DynFault>>,
	) -> (Vec<(String, String)>, u64) {
		tokio::pin!(call);
		let frame = tokio::select! {
			frame = frames.recv_async() => frame.expect("relayed admission prompt"),
			result = &mut call => panic!("the admission settled unasked: {:?}", result.map(|call| call.output)),
		};
		let Some(omp_proto::env::v1::server_frame::Body::ApprovalQuery(query)) = frame.body else {
			panic!("expected an approval query, got {:?}", frame.body);
		};
		let reasons = query
			.reasons
			.iter()
			.map(|reason| (reason.kind.clone(), reason.subject.clone()))
			.collect();
		(reasons, query.query_id)
	}

	/// Answers the relayed query `query_id` on request 9.
	fn answer(
		approvals: &crate::approval_relay::ConnectionApprovals,
		query_id: u64,
		approved: bool,
	) {
		approvals.answer(9, omp_proto::env::v1::ApprovalAnswer {
			query_id,
			decision: Some(omp_proto::env::v1::ApprovalDecision {
				approved,
				scope: "once".to_owned(),
				source: "user".to_owned(),
				..omp_proto::env::v1::ApprovalDecision::default()
			}),
		});
	}

	/// A `dyn` call of a device that scopes its effects to its arguments is
	/// admitted on what that call does, as a slot's call is at commit. Under
	/// `always-ask` an `ast_grep` of local roots is a read and runs unasked;
	/// one with a URL root asks once, for the host it fetches from and for
	/// nothing else (its tier is a fetch), and a refusal searches nothing.
	#[tokio::test]
	async fn a_dyn_search_is_admitted_on_the_roots_it_reaches() {
		use crate::approval_relay::ConnectionApprovals;

		let scratch = tempfile::tempdir().expect("scratch");
		let workspace = scratch.path().join("workspace");
		std::fs::create_dir(&workspace).expect("workspace");
		std::fs::write(workspace.join("lib.rs"), "fn main() { call(1); }\n").expect("source");
		let mut registry = Registry::new();
		register_device(
			&mut registry,
			omp_tools::ast_grep::tool(workspace, omp_tools::grep::SearchPolicy {
				fetch_enabled:      true,
				credentialed_fetch: true,
			}),
		);
		let (host, _registry) =
			dyn_host_over(scratch.path(), registry, admission(ApprovalMode::AlwaysAsk));
		let (responses, frames) = flume::bounded(4);
		let approvals = ConnectionApprovals::new(responses);
		let issuer = CommandIssuer { approvals: Some(approvals.owned(9)), reflection: None };

		let local = host
			.call_issued(
				"ast_grep",
				json!({ "pat": "call($A)", "path": "lib.rs" }),
				CancellationToken::new(),
				issuer.clone(),
			)
			.await
			.expect("a local search runs unasked");
		assert!(frames.is_empty(), "a local search never asks");
		let output = format!("{:?}", local.output);
		assert!(output.contains("call(1)"), "{output}");

		let call = host.call_issued(
			"ast_grep",
			json!({ "pat": "call($A)", "path": "lib.rs; https://docs.rs/a.rs" }),
			CancellationToken::new(),
			issuer,
		);
		tokio::pin!(call);
		let (reasons, query_id) = relayed_prompt(&frames, &mut call).await;
		assert_eq!(reasons, [("network".to_owned(), "http:docs.rs:443".to_owned())]);
		answer(&approvals, query_id, false);
		let refused = call.await.expect_err("a refused prompt searches nothing");
		assert!(refused.message.contains("denied"), "{refused:?}");
	}

	/// A device whose judgment reads live state, as `read` reads which MCP
	/// server advertises a resource: judging a call pins what `live` names
	/// then. Its calls write documents, so `always-ask` asks, and a call with
	/// `{"widen": true}` is judged beyond its maximum. Its executor records
	/// what it finds pinned.
	struct PinningDevice {
		spec: ToolSpec,
		live: Arc<Mutex<Str>>,
		ran:  Arc<Mutex<Vec<Option<omp_tool::ResolutionPin>>>>,
	}

	impl Tool for PinningDevice {
		type Fault = Value;
		type Params = Value;
		type Payload = Value;
		type Update = Value;

		const ARGUMENT_SCOPED_EFFECTS: bool = true;

		fn spec(&self) -> &ToolSpec {
			&self.spec
		}

		fn invocation_effects(&self, params: &Value) -> Option<Effects> {
			if params["widen"] == true {
				return Some(Effects { subagents: 1, ..Effects::empty() });
			}
			InvocationPins::pin("probe", "target", || omp_tool::ResolutionPin {
				target: Some(self.live.lock().clone()),
				fetch:  None,
			});
			Some(self.spec.effects.clone())
		}

		fn call<'c>(
			&'c self,
			_incoming: IncomingParams<'c>,
		) -> impl Stream<Item = Ev<Value, Value, Value>> + Send + 'c {
			futures::stream::once(async {
				self
					.ran
					.lock()
					.push(InvocationPins::pinned("probe", "target"));
				Ev::Done(ToolTerminal::Done { result: Ok(json!({"ok": true})), useless: false })
			})
		}

		fn prompt(&self, _view: Result<&Value, &Value>, _caps: &PromptCaps) -> Vec<Part> {
			Vec::new()
		}
	}

	/// A `dyn` device call runs inside the pins its judgment fixed: what the
	/// judgment resolved from live state is what its executor reaches, though
	/// that state moved while the user was asked. A call judged beyond the
	/// device's declared maximum is refused before anyone is asked, never
	/// admitted on that maximum, and never runs.
	#[tokio::test]
	async fn a_dyn_call_runs_inside_the_pins_its_judgment_fixed() {
		use crate::approval_relay::ConnectionApprovals;

		let scratch = tempfile::tempdir().expect("scratch");
		let live = Arc::new(Mutex::new(sf!("judged")));
		let ran = Arc::new(Mutex::new(Vec::new()));
		let calls = Arc::new(AtomicUsize::new(0));
		let mut spec = counting_device("pinning", Effects::empty(), Confinement::Host, &calls).spec;
		spec.effects = Effects {
			documents: Some(omp_tool::DocEffects {
				read:        true,
				write_globs: Arc::from([sf!("**")]),
			}),
			..Effects::empty()
		};
		let mut registry = Registry::new();
		register_device(&mut registry, PinningDevice {
			spec,
			live: Arc::clone(&live),
			ran: Arc::clone(&ran),
		});
		let (host, _registry) =
			dyn_host_over(scratch.path(), registry, admission(ApprovalMode::AlwaysAsk));
		let (responses, frames) = flume::bounded(4);
		let approvals = ConnectionApprovals::new(responses);
		let issuer = CommandIssuer { approvals: Some(approvals.owned(9)), reflection: None };

		let call = host.call_issued("pinning", json!({}), CancellationToken::new(), issuer.clone());
		tokio::pin!(call);
		let (reasons, query_id) = relayed_prompt(&frames, &mut call).await;
		assert_eq!(reasons, [("write".to_owned(), "pinning".to_owned())]);
		*live.lock() = sf!("moved");
		answer(&approvals, query_id, true);
		call.await.expect("the approved call runs");
		let judged = omp_tool::ResolutionPin { target: Some(sf!("judged")), fetch: None };
		assert_eq!(*ran.lock(), [Some(judged.clone())]);

		let refused = host
			.call_issued("pinning", json!({ "widen": true }), CancellationToken::new(), issuer)
			.await
			.expect_err("a call beyond the maximum is refused");
		assert!(refused.message.contains("could not be judged"), "{refused:?}");
		assert!(frames.is_empty(), "the refused call asked no one");
		assert_eq!(*ran.lock(), [Some(judged)], "the refused call never ran");
	}
}
