//! Registry assembly, device CONTROL, checkpoints, and worker projection.

use std::{
	collections::{BTreeMap, BTreeSet},
	env,
	env::consts,
	future::Future,
	path::Path,
	sync::Arc,
	time,
};
#[cfg(test)]
use std::{path::PathBuf, sync::LazyLock};

use omp_cache::{forge_cache::ForgeCache, telemetry_cache::TelemetryIndex};
use omp_catalog::{ModelKey, snapshot::Catalog};
use omp_con::Ctx;
use omp_contract::{HookGate, KernelSender};
use omp_core::{Duration, FastHashSet, Hash32, InvocationPhase, LifecyclePhase, Str, Ulid, sf};
use omp_env::EnvClient;
use omp_proto::{
	env::v1 as env_wire,
	inference::{v1, v1::tool_def},
	prost::Message as _,
	toolhost::v1::{
		GrammarSyntax as WorkerGrammarSyntax, PreludeParamKind, ToolDecl,
		ToolExecutionMode as WorkerExecutionMode, tool_constraint,
	},
};
use omp_tool::{
	AvailabilityDelta, Claims, Constraint, ExecutionMode, GrammarSyntax, LeafOwner,
	LeafReplacementError, LeafReplacementRegistry, LeafVersion, Precedence, Presentation, Registry,
	RegistryLeaf, Rev, ToolLocus, ToolSpec, ToolsPolicy,
};
use omp_tools::{
	ask::PresenterSlot,
	checkpoint,
	device::{DeviceCatalog, FlattenCollision, dyn_enabled, flatten_slots},
	edit::{EditRevisionCandidates, resolve_edit_revision},
	eval::{EvalSessionControl, TaskDescriptionSnapshot},
	goal,
	read::resolver::ResolverTable,
	shell::TimeoutBounds,
	staging::StagedProposalRegistry,
};
use parking_lot::{Mutex, RwLock};
use serde_json::{Map as JsonMap, Value as JsonValue, json};
use tokio::sync::{Mutex as AsyncMutex, Notify};
use tokio_util::sync::CancellationToken;

#[cfg(test)]
use super::bridges::{environment_registry, register_instrumented};
use super::{
	bridges::{
		ActiveContentInputs, DynamicTool, DynamicToolFactory, GoalAuthority, RegistryBridges,
		SearchInference, TelemetryUpload,
	},
	roster_host,
};
#[cfg(feature = "cap-browser")]
use crate::browser_daemon::BrowserSettings;
#[cfg(test)]
use crate::eval::ProcessEvalExec;
#[cfg(feature = "cap-media")]
use crate::media_devices;
#[cfg(feature = "cap-media")]
use crate::media_tts::{SpeechConfig, SpeechPreference};
use crate::{
	EnvdError,
	admission::DynamicAdmission,
	blobs::BlobHost,
	devices_host::DynHost,
	docs::DocumentHost,
	eval::{
		PRELUDE_PYTHON_KEYWORDS, PRELUDE_RESERVED_NAMES, PreludeHelper, PreludeInvoker,
		PreludeParamStub, PreludeTable, SessionBridgeHost,
	},
	exec::ExecHost,
	exec_settings::{SandboxSettings, ShellSettings},
	exthost::{
		CallbackConcurrency, ExtensionManifest,
		control::{
			ControlAuthority, ControlAuthorityFactory, ControlCompositionError,
			ControlConnectionIdentity, ControlEffect, ControlProtocolError, ControlRequestContext,
		},
		dispatch::{CallbackDispatcher, NestedCallbackDispatcher},
		extensions::{SealedRegistryEvidence, SealedRegistryEvidenceError, seal_registry_evidence},
	},
	fetch_host::FetchHostNamer,
	github_url::GithubCredentialBridge,
	managed_skills::ManagedSkills,
	mcp::{McpService, manager::McpManager},
	memory::ReflectionBridgeHost,
	search_backend::SearchBridgeHost,
	tool_settings::ToolSettings,
	tool_url::UrlResolver,
	worker::{ExtHostSupervisor, HostKey},
	workspace::{WorkspaceHost, WorkspaceOperationError, WorkspaceOperations},
};

pub(super) type ControlConnectionKey = (Str, Str, Str, u64, u64);

pub(super) fn connection_key(identity: &ControlConnectionIdentity) -> ControlConnectionKey {
	(
		identity.layer.clone(),
		identity.tier.clone(),
		identity.extension.clone(),
		identity.host_generation,
		identity.session_generation,
	)
}

pub(super) fn same_connection(
	expected: &ControlConnectionIdentity,
	actual: &ControlConnectionIdentity,
) -> bool {
	expected.extension == actual.extension
		&& expected.principal == actual.principal
		&& expected.artifact_digest == actual.artifact_digest
		&& expected.layer == actual.layer
		&& expected.tier == actual.tier
		&& expected.trust == actual.trust
		&& expected.host_generation == actual.host_generation
		&& expected.session_generation == actual.session_generation
		&& expected.capabilities == actual.capabilities
}

pub(super) fn stale_connection() -> ControlProtocolError {
	ControlProtocolError::new(
		"StaleGeneration",
		"CONTROL authority belongs to a replaced extension-host connection",
	)
}

/// Owns manifest verification and retained sealed registry evidence.
#[derive(Clone)]
pub struct RegistryControlFactory {
	manifests: Arc<BTreeMap<(Str, Str, Str), ExtensionManifest>>,
	evidence:  Arc<RwLock<BTreeMap<ControlConnectionKey, Arc<SealedRegistryEvidence>>>>,
	published: Arc<Notify>,
}

impl RegistryControlFactory {
	/// Creates the registry owner from deployment-authenticated manifests.
	pub fn new(manifests: BTreeMap<(Str, Str, Str), ExtensionManifest>) -> Arc<Self> {
		Arc::new(Self {
			manifests: Arc::new(manifests),
			evidence:  Arc::new(RwLock::new(BTreeMap::new())),
			published: Arc::new(Notify::new()),
		})
	}

	/// Returns exact-generation evidence only after the child published and
	/// passed the sealed registry comparison.
	pub fn evidence(
		&self,
		identity: &ControlConnectionIdentity,
	) -> Option<Arc<SealedRegistryEvidence>> {
		self.evidence.read().get(&connection_key(identity)).cloned()
	}

	/// Waits until the exact child generation publishes sealed registry
	/// evidence.
	pub async fn wait_evidence(
		&self,
		identity: &ControlConnectionIdentity,
	) -> Arc<SealedRegistryEvidence> {
		loop {
			let published = self.published.notified();
			tokio::pin!(published);
			published.as_mut().enable();
			if let Some(evidence) = self.evidence(identity) {
				return evidence;
			}
			published.await;
		}
	}

	pub(super) fn admits(&self, identity: &ControlConnectionIdentity) -> bool {
		self.manifests.contains_key(&(
			identity.layer.clone(),
			identity.tier.clone(),
			identity.extension.clone(),
		))
	}

	/// Accepts the sole registry publication for one connection generation.
	pub fn publish(
		&self,
		context: &ControlRequestContext,
		payload: &JsonValue,
	) -> Result<Arc<SealedRegistryEvidence>, ControlProtocolError> {
		let key = (
			context.connection.layer.clone(),
			context.connection.tier.clone(),
			context.connection.extension.clone(),
		);
		let manifest = self.manifests.get(&key).ok_or_else(|| {
			ControlProtocolError::new(
				"RegistryUnauthorized",
				"no authenticated manifest owns this registry publication",
			)
		})?;
		let session = context
			.invocation
			.as_ref()
			.map(|invocation| invocation.session.clone())
			.ok_or_else(|| {
				ControlProtocolError::new(
					"InvalidPhase",
					"registry FREEZE evidence has no authenticated lifecycle session",
				)
			})?;
		let evidence = Arc::new(
			seal_registry_evidence(
				Arc::clone(&context.connection),
				session,
				manifest,
				payload.clone(),
			)
			.map_err(registry_evidence_error)?,
		);
		self.install_evidence(evidence)
	}

	/// Installs evidence already sealed by the trusted extension-host lifecycle.
	///
	/// A newer host/session generation replaces the retained generation for
	/// the same deployment. Re-publication within one exact generation must be
	/// declaration-identical.
	pub fn install_evidence(
		&self,
		evidence: Arc<SealedRegistryEvidence>,
	) -> Result<Arc<SealedRegistryEvidence>, ControlProtocolError> {
		if !self.admits(&evidence.identity) {
			return Err(ControlProtocolError::new(
				"RegistryUnauthorized",
				"no authenticated manifest owns this sealed registry evidence",
			));
		}
		let connection = connection_key(&evidence.identity);
		let mut published = self.evidence.write();
		if let Some(current) = published.get(&connection) {
			if !same_registry_evidence(current, &evidence) {
				return Err(ControlProtocolError::new(
					"RegistryConflict",
					"sealed registry changed within one host generation",
				));
			}
			return Ok(Arc::clone(current));
		}
		let generation = (connection.3, connection.4);
		if published.iter().any(|(key, _)| {
			key.0 == connection.0
				&& key.1 == connection.1
				&& key.2 == connection.2
				&& (key.3, key.4) > generation
		}) {
			return Err(stale_connection());
		}
		published
			.retain(|key, _| key.0 != connection.0 || key.1 != connection.1 || key.2 != connection.2);
		published.insert(connection, Arc::clone(&evidence));
		drop(published);
		self.published.notify_waiters();
		Ok(evidence)
	}
}

fn same_registry_evidence(
	current: &SealedRegistryEvidence,
	candidate: &SealedRegistryEvidence,
) -> bool {
	same_connection(&current.identity, &candidate.identity)
		&& current.session == candidate.session
		&& current.tools == candidate.tools
		&& current.prompts == candidate.prompts
		&& current.services == candidate.services
		&& current.hooks == candidate.hooks
		&& current.ui_registration == candidate.ui_registration
		&& current.ui.generation == candidate.ui.generation
		&& current.ui.extension == candidate.ui.extension
		&& current.ui.commands == candidate.ui.commands
		&& current.ui.shortcuts == candidate.ui.shortcuts
		&& current.ui.triggers == candidate.ui.triggers
		&& current.ui.message_renderers == candidate.ui.message_renderers
		&& current.ui.markdown_transformers == candidate.ui.markdown_transformers
		&& current.ui.renderers == candidate.ui.renderers
		&& current.providers == candidate.providers
		&& current.directors == candidate.directors
		&& current.components == candidate.components
}

fn registry_evidence_error(error: SealedRegistryEvidenceError) -> ControlProtocolError {
	let code = match error {
		SealedRegistryEvidenceError::Identity => "RegistryUnauthorized",
		SealedRegistryEvidenceError::ManifestDrift => "DeclarationDrift",
		SealedRegistryEvidenceError::Duplicate
		| SealedRegistryEvidenceError::SourceModule
		| SealedRegistryEvidenceError::Ui(_)
		| SealedRegistryEvidenceError::Prompt(_) => "RegistryDrift",
		SealedRegistryEvidenceError::Malformed => "RegistryMalformed",
	};
	ControlProtocolError::new(code, Str::from(error.to_string()))
}

/// One live dynamic device row published to catalog observers.
#[derive(Clone, Debug)]
pub struct DynamicDeviceCatalogEntry {
	/// Canonical mounted leaf path.
	pub path:       Str,
	/// Frozen parent family.
	pub family:     Str,
	/// Frozen parent revision.
	pub rev:        u16,
	/// Authenticated claimant.
	pub claimant:   Str,
	/// Frozen placement.
	pub place:      Str,
	/// Leaf summary.
	pub summary:    Str,
	/// Validated JSON Schema.
	pub schema:     JsonValue,
	/// Optional long-form documentation.
	pub docs:       Option<Str>,
	/// Current reachability.
	pub mounted:    bool,
	/// Current unavailable reason.
	pub reason:     Option<Str>,
	/// Authenticated installation provenance.
	pub provenance: omp_core::Provenance,
}

/// Receives exactly one notification after each effective catalog transition.
pub trait DeviceCatalogObserver: Send + Sync + 'static {
	/// Publishes an immutable old-or-new catalog snapshot.
	fn catalog_changed(&self, epoch: u64, catalog: Arc<[DynamicDeviceCatalogEntry]>);
}

/// Performs the independent policy/admission decision for a nested device call.
#[async_trait::async_trait]
pub trait DeviceInvocationAdmission: Send + Sync + 'static {
	/// Admits the exact resolved leaf and arguments without inheriting authority
	/// from the caller.
	async fn admit(
		&self,
		caller: &ControlRequestContext,
		target: &DynamicDeviceCatalogEntry,
		arguments: &JsonMap<String, JsonValue>,
	) -> Result<(), ControlProtocolError>;
}

#[derive(Clone)]
struct DynamicDeviceBinding {
	entry:    DynamicDeviceCatalogEntry,
	identity: Arc<ControlConnectionIdentity>,
}

/// Composes sealed registry evidence with dynamic device dispatch authority.
#[derive(Clone)]
pub struct DeviceControlFactory {
	registries: Arc<RegistryControlFactory>,
	catalog:    Arc<LeafReplacementRegistry<DynamicDeviceBinding>>,
	callbacks:  Arc<NestedCallbackDispatcher>,
	observer:   Arc<dyn DeviceCatalogObserver>,
	admission:  Arc<dyn DeviceInvocationAdmission>,
	mutation:   Arc<Mutex<()>>,
}

impl DeviceControlFactory {
	/// Binds dynamic catalog mutation to sealed evidence, policy admission, the
	/// live callback dispatcher, and the durable catalog observer.
	pub fn new(
		registries: Arc<RegistryControlFactory>,
		dispatcher: Arc<dyn CallbackDispatcher>,
		observer: Arc<dyn DeviceCatalogObserver>,
		admission: Arc<dyn DeviceInvocationAdmission>,
	) -> Arc<Self> {
		Arc::new(Self {
			registries,
			catalog: Arc::new(LeafReplacementRegistry::new()),
			callbacks: Arc::new(NestedCallbackDispatcher::new(dispatcher)),
			observer,
			admission,
			mutation: Arc::new(Mutex::new(())),
		})
	}

	/// Returns the current immutable catalog projection.
	pub fn catalog(&self) -> Arc<[DynamicDeviceCatalogEntry]> {
		self
			.catalog
			.snapshot()
			.leaves
			.iter()
			.map(|leaf| {
				let mut entry = leaf.value.entry.clone();
				entry.mounted = leaf.mounted;
				entry.reason = leaf.reason.clone();
				entry
			})
			.collect()
	}

	fn publish_catalog(&self, epoch: u64) -> Arc<[DynamicDeviceCatalogEntry]> {
		let catalog = self.catalog();
		self.observer.catalog_changed(epoch, Arc::clone(&catalog));
		catalog
	}

	fn mount(
		&self,
		context: &ControlRequestContext,
		arguments: &JsonMap<String, JsonValue>,
	) -> Result<JsonValue, ControlProtocolError> {
		require_active_invocation(context)?;
		let evidence = self
			.registries
			.evidence(&context.connection)
			.ok_or_else(|| {
				ControlProtocolError::new(
					"RegistryUnavailable",
					"dynamic mount requires sealed registry evidence",
				)
			})?;
		let parent = arguments
			.get("parent")
			.and_then(JsonValue::as_object)
			.ok_or_else(|| invalid_device("dynamic mount parent must be an object"))?;
		let parent_name = required_string(parent, "name")?;
		let family = required_string(parent, "family")?;
		let rev = required_u16(parent, "rev")?;
		let parent_revision = Rev { family: family.clone(), n: rev };
		let place = required_string(parent, "place")?;
		let _registration = evidence
			.tools
			.iter()
			.find(|tool| {
				tool
					.definition
					.as_ref()
					.is_some_and(|definition| definition.name.as_str() == parent_name.as_str())
					&& tool
						.rev
						.parse::<Rev>()
						.is_ok_and(|revision| revision == parent_revision)
					&& tool.place == place
			})
			.ok_or_else(|| {
				ControlProtocolError::new(
					"RegistryUnauthorized",
					"dynamic mount parent is not present in sealed registration evidence",
				)
			})?;
		let specs = arguments
			.get("specs")
			.and_then(JsonValue::as_array)
			.ok_or_else(|| invalid_device("dynamic mount specs must be an array"))?;
		let owner = LeafOwner {
			root:     parent_name.clone(),
			claimant: context.connection.extension.clone(),
		};
		let _guard = self.mutation.lock();
		let mut leaves = self
			.catalog
			.snapshot()
			.leaves
			.iter()
			.filter(|leaf| leaf.owner == owner)
			.map(|leaf| RegistryLeaf {
				name:  leaf.name.clone(),
				rev:   leaf.rev.clone(),
				code:  leaf.code,
				value: Arc::clone(&leaf.value),
			})
			.collect::<Vec<_>>();
		let mut paths = Vec::with_capacity(specs.len());
		for spec in specs {
			let spec = spec
				.as_object()
				.ok_or_else(|| invalid_device("dynamic mount spec must be an object"))?;
			let path = required_string(spec, "path")?;
			let subpath = required_string(spec, "subpath")?;
			if path.as_str() != format!("{parent_name}/{subpath}")
				|| !valid_device_subpath(subpath.as_str())
			{
				return Err(invalid_device("dynamic mount path is outside its sealed parent"));
			}
			if leaves.iter().any(|leaf| leaf.name == path)
				|| paths.iter().any(|existing| existing == &path)
			{
				return Err(ControlProtocolError::new(
					"DeviceNameError",
					"dynamic device path is already mounted",
				));
			}
			let schema = spec
				.get("schema")
				.filter(|schema| schema.is_object())
				.cloned()
				.ok_or_else(|| invalid_device("dynamic device schema must be an object"))?;
			let summary = required_string(spec, "summary")?;
			let docs = optional_string(spec, "docs")?;
			let entry = DynamicDeviceCatalogEntry {
				path: path.clone(),
				family: family.clone(),
				rev,
				claimant: context.connection.extension.clone(),
				place: place.clone(),
				summary,
				schema,
				docs,
				mounted: true,
				reason: None,
				provenance: evidence.provenance.clone(),
			};
			let mut hasher = Hash32::hasher();
			hasher.update(b"omp/dynamic-device-binding/v1\0");
			hasher.update(
				serde_json::to_vec(&json!({
					"path": entry.path.as_str(),
					"family": entry.family.as_str(),
					"rev": entry.rev,
					"place": entry.place.as_str(),
					"summary": entry.summary.as_str(),
					"schema": &entry.schema,
					"docs": entry.docs.as_deref(),
					"artifact": context.connection.artifact_digest.as_str(),
				}))
				.map_err(invalid_device)?,
			);
			leaves.push(RegistryLeaf {
				name:  path.clone(),
				rev:   Rev { family: family.clone(), n: rev },
				code:  hasher.finalize(),
				value: Arc::new(DynamicDeviceBinding {
					entry,
					identity: Arc::clone(&context.connection),
				}),
			});
			paths.push(path);
		}
		let epoch = self
			.catalog
			.replace(
				owner,
				LeafVersion {
					manager_generation: context.connection.host_generation,
					definition_epoch:   context.request_id,
				},
				leaves,
			)
			.map_err(device_catalog_error)?;
		let catalog = self.publish_catalog(epoch);
		Ok(json!({
			"paths": paths.iter().map(Str::as_str).collect::<Vec<_>>(),
			"catalog": catalog.iter().map(device_catalog_json).collect::<Vec<_>>(),
		}))
	}

	fn availability(
		&self,
		context: &ControlRequestContext,
		arguments: &JsonMap<String, JsonValue>,
	) -> Result<JsonValue, ControlProtocolError> {
		require_active_invocation(context)?;
		let rows = arguments
			.get("deltas")
			.and_then(JsonValue::as_array)
			.ok_or_else(|| invalid_device("availability deltas must be an array"))?;
		let mut deltas = Vec::with_capacity(rows.len());
		for row in rows {
			let row = row
				.as_object()
				.ok_or_else(|| invalid_device("availability delta must be an object"))?;
			let path = required_string(row, "path")?;
			let root = path
				.split("/")
				.next()
				.filter(|root| !root.is_empty())
				.ok_or_else(|| invalid_device("availability path is invalid"))?;
			let mounted = row
				.get("mounted")
				.and_then(JsonValue::as_bool)
				.ok_or_else(|| invalid_device("availability mounted must be boolean"))?;
			let reason = optional_string(row, "reason")?;
			deltas.push((
				LeafOwner { root: Str::new(root), claimant: context.connection.extension.clone() },
				AvailabilityDelta { name: path, mounted, reason },
			));
		}
		let _guard = self.mutation.lock();
		let epoch = self
			.catalog
			.set_availability_many(context.connection.host_generation, &deltas)
			.map_err(device_catalog_error)?;
		let catalog = self.publish_catalog(epoch);
		Ok(json!({
			"catalog": catalog.iter().map(device_catalog_json).collect::<Vec<_>>(),
		}))
	}

	async fn invoke(
		&self,
		context: &ControlRequestContext,
		arguments: &JsonMap<String, JsonValue>,
	) -> Result<JsonValue, ControlProtocolError> {
		require_active_invocation(context)?;
		let path = required_string(arguments, "path")?;
		let args = arguments
			.get("args")
			.and_then(JsonValue::as_object)
			.cloned()
			.ok_or_else(|| invalid_device("device invocation args must be an object"))?;
		let matches = self
			.catalog
			.snapshot()
			.leaves
			.iter()
			.filter(|leaf| leaf.name == path && leaf.mounted)
			.cloned()
			.collect::<Vec<_>>();
		let [leaf] = matches.as_slice() else {
			return Err(ControlProtocolError::new(
				"DeviceUnavailable",
				"no unique mounted device owns the requested path",
			));
		};
		self
			.admission
			.admit(context, &leaf.value.entry, &args)
			.await?;
		let mut callback_args = JsonMap::new();
		callback_args.insert(String::from("path"), JsonValue::String(path.to_string()));
		callback_args.insert(String::from("args"), JsonValue::Object(args));
		callback_args
			.insert(String::from("family"), JsonValue::String(leaf.value.entry.family.to_string()));
		callback_args.insert(String::from("rev"), JsonValue::from(leaf.value.entry.rev));
		self
			.callbacks
			.dispatch(
				Arc::clone(&leaf.value.identity),
				context,
				"omp.devices.call",
				callback_args,
				CallbackConcurrency::Serialized,
				request_timeout(arguments, time::Duration::from_secs(30))?,
				None,
				Some(path),
			)
			.await
	}

	/// Host-issued forge bridge: look up a mounted leaf and dispatch
	/// `omp.devices.call` (same destination as [`Self::invoke`], without a
	/// nested CONTROL parent).
	///
	/// Used by [`crate::github_adapter::LiveForgeDeviceClient`] once bound via
	/// [`Self::forge_invoker`].
	#[cfg(feature = "cap-forge")]
	pub async fn call_mounted(
		&self,
		path: &str,
		args: JsonValue,
	) -> Result<JsonValue, omp_forge::ForgeError> {
		use crate::github_adapter::forge_op_for_path;

		let op = forge_op_for_path(path);
		let args = args
			.as_object()
			.cloned()
			.ok_or(omp_forge::ForgeError::Unavailable { op })?;
		let matches = self
			.catalog
			.snapshot()
			.leaves
			.iter()
			.filter(|leaf| leaf.name.as_str() == path && leaf.mounted)
			.cloned()
			.collect::<Vec<_>>();
		let [leaf] = matches.as_slice() else {
			return Err(omp_forge::ForgeError::Unavailable { op });
		};
		let mut callback_args = JsonMap::new();
		callback_args.insert(String::from("path"), JsonValue::String(path.to_string()));
		callback_args.insert(String::from("args"), JsonValue::Object(args));
		callback_args
			.insert(String::from("family"), JsonValue::String(leaf.value.entry.family.to_string()));
		callback_args.insert(String::from("rev"), JsonValue::from(leaf.value.entry.rev));
		self
			.callbacks
			.dispatch_host(
				Arc::clone(&leaf.value.identity),
				"omp.devices.call",
				callback_args,
				CallbackConcurrency::Serialized,
				time::Duration::from_secs(30),
				Some(Str::new(path)),
			)
			.await
			.map_err(|_| omp_forge::ForgeError::Unavailable { op })
	}

	/// Catalog-backed [`crate::github_adapter::ForgeDeviceInvoker`] for the
	/// live forge client.
	#[cfg(feature = "cap-forge")]
	#[must_use]
	pub fn forge_invoker(self: &Arc<Self>) -> Arc<dyn crate::github_adapter::ForgeDeviceInvoker> {
		Arc::new(DeviceCatalogForgeInvoker { devices: Arc::clone(self) })
	}
}

/// [`ForgeDeviceInvoker`] that forwards to
/// [`DeviceControlFactory::call_mounted`].
#[cfg(feature = "cap-forge")]
struct DeviceCatalogForgeInvoker {
	devices: Arc<DeviceControlFactory>,
}

#[cfg(feature = "cap-forge")]
impl crate::github_adapter::ForgeDeviceInvoker for DeviceCatalogForgeInvoker {
	fn invoke(
		&self,
		path: &str,
		args: JsonValue,
	) -> std::pin::Pin<Box<dyn Future<Output = Result<JsonValue, omp_forge::ForgeError>> + Send + '_>>
	{
		let devices = Arc::clone(&self.devices);
		let path = path.to_owned();
		Box::pin(async move { devices.call_mounted(&path, args).await })
	}
}

fn device_catalog_json(entry: &DynamicDeviceCatalogEntry) -> JsonValue {
	json!({
		"name": entry.path.as_str(),
		"family": entry.family.as_str(),
		"rev": entry.rev,
		"identity": format!("{}@{}", entry.path, entry.claimant),
		"claimant": entry.claimant.as_str(),
		"path": entry.path.as_str(),
		"summary": entry.summary.as_str(),
		"place": entry.place.as_str(),
		"precedence": 0,
		"tier": entry.provenance.tier(),
		"effects": null,
		"mounted": entry.mounted,
		"enabled": true,
		"available": entry.mounted,
		"reason": entry.reason.as_deref(),
		"shadowed_by": null,
		"source": entry.provenance.extension_id(),
		"provenance": {
			"publisher": entry.provenance.publisher(),
			"extension_id": entry.provenance.extension_id(),
			"version": entry.provenance.version(),
			"artifact_digest": entry.provenance.artifact_digest().to_string(),
			"layer": entry.provenance.layer(),
			"tier": entry.provenance.tier(),
			"trust": entry.provenance.tier(),
		},
		"slotted": false,
		"schema_bytes": serde_json::to_vec(&entry.schema).map_or(0, |bytes| bytes.len()),
		"schema_tokens": 0,
	})
}

fn device_catalog_error(error: LeafReplacementError) -> ControlProtocolError {
	match error {
		LeafReplacementError::Stale { current_generation, manager_generation, .. }
		| LeafReplacementError::Generation {
			expected: current_generation,
			actual: manager_generation,
		} => ControlProtocolError::new(
			"StaleGeneration",
			format!(
				"device catalog generation is stale: expected {current_generation}, got \
				 {manager_generation}"
			),
		),
		LeafReplacementError::UnknownName { name } => ControlProtocolError::new(
			"DeviceUnavailable",
			format!("dynamic device is not mounted: {name}"),
		),
		other => ControlProtocolError::new("DeviceError", Str::from(other.to_string())),
	}
}

/// Creates a device-protocol error, rendering the cause once at this
/// model-facing fault boundary.
fn invalid_device(cause: impl std::fmt::Display) -> ControlProtocolError {
	ControlProtocolError::new("DeviceError", sf!("{cause}"))
}

pub(super) fn required_string(
	object: &JsonMap<String, JsonValue>,
	field: &'static str,
) -> Result<Str, ControlProtocolError> {
	object
		.get(field)
		.and_then(JsonValue::as_str)
		.filter(|value| !value.is_empty())
		.map(Str::new)
		.ok_or_else(|| invalid_device(format!("{field} must be a non-empty string")))
}

fn optional_string(
	object: &JsonMap<String, JsonValue>,
	field: &'static str,
) -> Result<Option<Str>, ControlProtocolError> {
	match object.get(field) {
		None | Some(JsonValue::Null) => Ok(None),
		Some(JsonValue::String(value)) => Ok(Some(Str::new(value))),
		_ => Err(invalid_device(format!("{field} must be a string or null"))),
	}
}

pub(super) fn required_u16(
	object: &JsonMap<String, JsonValue>,
	field: &'static str,
) -> Result<u16, ControlProtocolError> {
	object
		.get(field)
		.and_then(JsonValue::as_u64)
		.and_then(|value| u16::try_from(value).ok())
		.ok_or_else(|| invalid_device(format!("{field} must be a u16")))
}

fn valid_device_subpath(path: &str) -> bool {
	!path.is_empty()
		&& path.split('/').all(|segment| {
			!segment.is_empty()
				&& segment.len() <= 64
				&& segment.bytes().enumerate().all(|(index, byte)| {
					byte.is_ascii_lowercase()
						|| byte.is_ascii_digit() && index > 0
						|| byte == b'_' && index > 0
				})
		})
}

fn request_timeout(
	arguments: &JsonMap<String, JsonValue>,
	default: time::Duration,
) -> Result<time::Duration, ControlProtocolError> {
	let Some(value) = arguments.get("deadline") else {
		return Ok(default);
	};
	let Some(value) = value.as_str() else {
		if value.is_null() {
			return Ok(default);
		}
		return Err(invalid_device("deadline must be a duration string or null"));
	};
	value
		.parse::<Duration>()
		.map_err(invalid_device)?
		.to_std()
		.map_err(|_| invalid_device("deadline exceeds host duration range"))
}

pub(super) fn require_active_invocation(
	context: &ControlRequestContext,
) -> Result<(), ControlProtocolError> {
	let invocation = context.invocation.as_ref().ok_or_else(|| {
		ControlProtocolError::new(
			"InvalidPhase",
			"device mutation and invocation require a live callback",
		)
	})?;
	if invocation.lifecycle != LifecyclePhase::Active
		|| invocation.phase < InvocationPhase::EffectsAuthorized
	{
		return Err(ControlProtocolError::new(
			"InvalidPhase",
			"device operation requires ACTIVE effects-authorized phase",
		));
	}
	Ok(())
}

impl ControlAuthorityFactory for DeviceControlFactory {
	fn bind(
		&self,
		identity: Arc<ControlConnectionIdentity>,
	) -> Result<Arc<dyn ControlAuthority>, ControlCompositionError> {
		if !self.registries.admits(&identity) {
			return Err(ControlCompositionError::unavailable(
				"devices",
				"authenticated extension has no deployment manifest",
			));
		}
		Ok(Arc::new(BoundDeviceControl { identity, owner: self.clone() }))
	}
}

struct BoundDeviceControl {
	identity: Arc<ControlConnectionIdentity>,
	owner:    DeviceControlFactory,
}

#[async_trait::async_trait]
impl ControlAuthority for BoundDeviceControl {
	fn handles(&self, operation: &str) -> bool {
		matches!(
			operation,
			"omp.devices.dynamic_mount"
				| "omp.devices.set_availability"
				| "omp.devices.refresh"
				| "omp.devices.invoke"
		)
	}

	fn authorize(
		&self,
		context: &ControlRequestContext,
		operation: &str,
		_arguments: &JsonMap<String, JsonValue>,
	) -> Result<(), ControlProtocolError> {
		if !same_connection(&self.identity, &context.connection) {
			return Err(stale_connection());
		}
		if !self.handles(operation) {
			return Err(ControlProtocolError::new(
				"InvalidOperation",
				"device owner does not handle this operation",
			));
		}
		require_active_invocation(context)
	}

	async fn request(
		&self,
		context: ControlRequestContext,
		operation: Str,
		arguments: JsonMap<String, JsonValue>,
	) -> Result<JsonValue, ControlProtocolError> {
		self.authorize(&context, operation.as_str(), &arguments)?;
		match operation.as_str() {
			"omp.devices.dynamic_mount" => self.owner.mount(&context, &arguments),
			"omp.devices.set_availability" => self.owner.availability(&context, &arguments),
			"omp.devices.refresh" => {
				let catalog = self.owner.catalog();
				Ok(json!({
					"catalog": catalog.iter().map(device_catalog_json).collect::<Vec<_>>(),
				}))
			},
			"omp.devices.invoke" => self.owner.invoke(&context, &arguments).await,
			_ => unreachable!("authorized exact device operation"),
		}
	}

	async fn effect(
		&self,
		context: ControlRequestContext,
		_effect: ControlEffect,
	) -> Result<(), ControlProtocolError> {
		if same_connection(&self.identity, &context.connection) {
			Err(ControlProtocolError::new("InvalidEffect", "device owner accepts requests only"))
		} else {
			Err(stale_connection())
		}
	}
}

/// Failure behavior for one subscribed hook callback.

pub(super) fn configured_model_edit_revision(ctx: &Ctx) -> Result<Option<Rev>, EnvdError> {
	let settings = omp_catalog::settings::ModelSettings::from_con(ctx);
	let Some(selector) = settings.role_selector("default") else {
		return Ok(None);
	};
	let catalog = Catalog::embedded();
	let model = catalog
		.model(ModelKey::from_ref(selector))
		.or_else(|| catalog.resolve_alias(selector));
	let Some(revision) = model.and_then(|model| model.edit_revision.as_deref()) else {
		return Ok(None);
	};
	revision
		.parse::<Rev>()
		.map(Some)
		.map_err(edit_dialect_error)
}

pub(super) fn configured_model_identity(ctx: &Ctx) -> Option<Str> {
	let settings = omp_catalog::settings::ModelSettings::from_con(ctx);
	let selected = omp_contract::AI_MODEL.get(ctx);
	if let Some(role) = selected.strip_prefix("@") {
		return settings.role_selector(role.as_str()).cloned();
	}
	if !selected.is_empty() {
		return Some(selected);
	}
	settings.role_selector("default").cloned()
}

#[cfg(feature = "cap-media")]
fn image_config(ctx: &Ctx) -> media_devices::ImageConfig {
	let provider_order = omp_ai::settings::AI_PROVIDERS_IMAGE_ORDER
		.get(ctx)
		.into_iter()
		.filter_map(|provider| provider.parse().ok())
		.filter(|provider| *provider != media_devices::ImageProvider::Auto)
		.collect();
	media_devices::ImageConfig { provider_order, active_model: configured_model_identity(ctx) }
}

#[cfg(feature = "cap-media")]
fn speech_config(ctx: &Ctx) -> SpeechConfig {
	use omp_ai::speech_settings::{AI_TTS_PROVIDER, CL_TTS_MODEL, CL_TTS_VOICE, TtsProvider};
	let preference = match AI_TTS_PROVIDER.get(ctx) {
		TtsProvider::Auto => SpeechPreference::Auto,
		TtsProvider::Local => SpeechPreference::Local,
		TtsProvider::Xai => SpeechPreference::Xai,
		TtsProvider::Deepinfra => SpeechPreference::Deepinfra,
	};
	SpeechConfig {
		preference,
		local_model: Str::new(<&'static str>::from(CL_TTS_MODEL.get(ctx))),
		local_voice: Str::new(<&'static str>::from(CL_TTS_VOICE.get(ctx))),
	}
}

fn prepare_registry(
	registry: &mut Registry,
	roster: &omp_tools::roster::SelectedRoster,
) -> Result<(), EnvdError> {
	omp_tools::roster::guard(registry, roster)
		.map_err(|error| EnvdError::DuplicateToolName { name: Str::new_static(error.name) })
}

fn resolve_roster(
	roster: Option<omp_tools::roster::SelectedRoster>,
) -> omp_tools::roster::SelectedRoster {
	roster.unwrap_or_else(omp_tools::roster::SelectedRoster::production)
}

fn tool_name(family: omp_tools::roster::Family) -> &'static str {
	omp_tools::roster::entry(family)
		.expect("built-in roster row")
		.name
}

struct SessionBaseOutput {
	search_bridge:      Arc<SearchBridgeHost>,
	github_credentials: Arc<GithubCredentialBridge>,
	ask_presenter:      PresenterSlot,
	checkpoint_control: AgentCheckpointControl,
}

#[allow(
	clippy::too_many_arguments,
	reason = "session tool composition carries independent typed authorities"
)]
fn register_session_base(
	registry: &mut Registry,
	dynamic_tools: Vec<DynamicTool>,
	dynamic_tool_factories: Vec<Arc<dyn DynamicToolFactory>>,
	search: Option<Arc<dyn SearchInference>>,
	ask_presenter: Option<Arc<dyn omp_tools::ask::AskPresenter>>,
	goal_control: Option<Arc<dyn GoalAuthority>>,
	telemetry_upload: Option<Arc<dyn TelemetryUpload>>,
	blobs: &BlobHost,
	project_root: &Path,
	state_dir: &Path,
	telemetry: &Arc<TelemetryIndex>,
	forge_cache: Arc<ForgeCache>,
	#[cfg(feature = "cap-forge")] forge_devices: crate::github_adapter::LiveForgeDeviceClient,
	tool_settings: &ToolSettings,
	#[cfg(feature = "cap-media")] image_config: media_devices::ImageConfig,
	#[cfg(feature = "cap-media")] speech_config: SpeechConfig,
	policy: ToolsPolicy,
	roster: &omp_tools::roster::SelectedRoster,
) -> Result<SessionBaseOutput, EnvdError> {
	for dynamic in dynamic_tools {
		dynamic.register(registry)?;
	}
	for factory in dynamic_tool_factories {
		factory.register(registry)?;
	}
	let mut host = roster_host::SessionHost::new(
		registry,
		policy,
		tool_settings,
		search,
		ask_presenter,
		goal_control,
		telemetry_upload,
		blobs,
		project_root,
		state_dir,
		telemetry,
		forge_cache,
		#[cfg(feature = "cap-forge")]
		forge_devices,
		#[cfg(feature = "cap-media")]
		image_config,
		#[cfg(feature = "cap-media")]
		speech_config,
	);
	omp_tools::roster::install_session(&mut host, roster)?;
	let output = host.output();
	Ok(SessionBaseOutput {
		search_bridge:      output.search_bridge,
		github_credentials: output.github_credentials,
		ask_presenter:      output.ask_presenter,
		checkpoint_control: output.checkpoint_control,
	})
}

fn register_session_workers(
	registry: &mut Registry,
	workers: &ExtHostSupervisor,
	policy: ToolsPolicy,
) -> Result<(), EnvdError> {
	let flattened_slots = if policy == ToolsPolicy::ToolOnly {
		let mut slots = Vec::new();
		for registration in workers.registrations() {
			if is_prelude_declaration(&registration.declaration)? {
				continue;
			}
			let definition = registration
				.declaration
				.definition
				.as_ref()
				.ok_or_else(|| worker_declaration_error("worker tool declaration has no definition"))?;
			slots.push((Str::from(definition.name.as_str()), registration.owner.extension().clone()));
		}
		Some(flatten_slots(slots).map_err(slot_collision_error)?)
	} else {
		None
	};
	for registration in workers.registrations() {
		let declaration = &registration.declaration;
		if is_prelude_declaration(declaration)? {
			continue;
		}
		let mut spec = worker_spec(declaration, &registration.owner)?;
		if flattened_slots.is_some() {
			spec.name = Str::from(spec.name.as_str().replace('/', "_"));
		}
		let device_name = spec.name.clone();
		let owner = registration.owner.clone();
		ensure_name_absent(registry, &spec.name)?;
		let execution = match WorkerExecutionMode::try_from(declaration.execution_mode) {
			Ok(WorkerExecutionMode::Unspecified | WorkerExecutionMode::Parallel) => {
				ExecutionMode::Parallel
			},
			Ok(WorkerExecutionMode::Sequential) => ExecutionMode::Sequential,
			Err(_) => return Err(worker_declaration_error("worker execution mode is invalid")),
		};
		registry.register_worker_with_mode(
			spec,
			if flattened_slots.is_some() {
				Presentation::Slot
			} else {
				Presentation::Device
			},
			Claims {
				precedence: Precedence::DEFAULT,
				claimant:   registration.owner.extension().clone(),
				replaces:   None,
			},
			execution,
		)?;
		registry.bind_device_metadata(
			device_name,
			owner.extension().clone(),
			omp_tool::DeviceMetadata {
				extension_id: Some(owner.extension().clone()),
				layer: Some(owner.layer().clone()),
				tier: Some(owner.tier().clone()),
				..omp_tool::DeviceMetadata::default()
			},
		);
	}
	Ok(())
}

#[derive(Clone)]
pub(crate) struct EnvironmentDeclarationInputs {
	pub read_policy:      omp_tools::read::ReadPolicy,
	pub selected_edit:    Rev,
	pub eval_description: Option<Str>,
	pub shell_snapshot:   Option<omp_tools::shell::ShellPromptSnapshot>,
	pub memory:           omp_memory::Capabilities,
	pub managed_skills:   bool,
}
#[allow(
	clippy::too_many_arguments,
	reason = "declaration projection mirrors independent tool setting domains"
)]
pub(crate) fn build_environment_declaration_inputs(
	_state_dir: &Path,
	_project_root: &Path,
	con: &Ctx,
	workers: &ExtHostSupervisor,
	tool_settings: &ToolSettings,
	shell_settings: &ShellSettings,
	memory_settings: &omp_memory::MemorySettings,
	autolearn_settings: &omp_memory::AutolearnSettings,
	content: &ActiveContentInputs,
	policy: ToolsPolicy,
) -> Result<EnvironmentDeclarationInputs, EnvdError> {
	let environment_edit_dialect = env::var("OMP_EDIT_DIALECT").ok();
	let force_hashline = env::var_os("OMP_STRICT_EDIT_MODE").is_some();
	let model_edit_revision = configured_model_edit_revision(con)?;
	let selected_edit = resolve_edit_revision(EditRevisionCandidates {
		environment: environment_edit_dialect.as_deref(),
		model_rule: model_edit_revision.as_ref(),
		setting: tool_settings.edit_dialect.as_deref(),
		force_hashline,
		..EditRevisionCandidates::default()
	})
	.map_err(EnvdError::EditDialect)?
	.revision;
	let prelude = build_prelude_table(workers)?;
	let helper_docs = prelude
		.helpers()
		.map(|helper| omp_tools::eval::PreludeHelperDescription {
			signature: helper.signature.as_str(),
			summary:   helper.summary.as_str(),
		})
		.collect::<Vec<_>>();
	let mut task_snapshot =
		TaskDescriptionSnapshot { helpers: &helper_docs, ..TaskDescriptionSnapshot::standard() };
	if !tool_settings.enabled(tool_name(omp_tools::roster::Family::Task)) {
		task_snapshot.agents = &[];
	}
	let eval_description = tool_settings
		.enabled(tool_name(omp_tools::roster::Family::Eval))
		.then(|| omp_tools::eval::task_description(task_snapshot));
	let dyn_installed = tool_settings.enabled("dyn") && dyn_enabled(policy);
	let shell_snapshot = (tool_settings.enabled(tool_name(omp_tools::roster::Family::Bash))
		&& shell_settings.enabled)
		.then(|| omp_tools::shell::ShellPromptSnapshot {
			sibling_tools:       Arc::default(),
			platform:            Str::new(consts::OS),
			devices:             dyn_installed,
			embedded_builtins:   shell_settings.embedded_builtins,
			interceptor_enabled: shell_settings.interceptor.enabled,
			interceptor_rules:   shell_settings
				.interceptor
				.patterns
				.iter()
				.map(|rule| omp_tools::shell_intercept::Rule {
					pattern: rule.pattern.clone(),
					tool:    rule.tool.clone(),
					message: rule.message.clone(),
				})
				.collect(),
			command_prefix:      shell_settings.command_prefix.is_some(),
		});
	let memory = if memory_settings.backend == omp_memory::MemoryBackend::Off {
		omp_memory::Capabilities::default()
	} else {
		omp_memory::Capabilities {
			writable:   true,
			searchable: true,
			resolvable: true,
			editable:   true,
			lifecycle:  true,
			embeddings: false,
		}
	};
	Ok(EnvironmentDeclarationInputs {
		read_policy: production_read_policy(tool_settings, &selected_edit),
		selected_edit,
		eval_description,
		shell_snapshot,
		memory,
		managed_skills: autolearn_settings.enabled && content.managed_skills_root.is_some(),
	})
}

/// The `read@3` policy of a production environment, the one both its
/// declaration and its executor are built from, so the effects the
/// environment declares for `read` are the ones its calls are judged by.
///
/// [`crate::tool_url::production_url_resolvers`] registers the resolvers that
/// fetch with stored credentials (`ssh://`, `issue://`, `pr://`, `mcp://`)
/// whatever `tools.fetch.enabled` says, so the maximum always holds a
/// credentialed fetch; the setting only removes the anonymous URL fetch.
pub(super) fn production_read_policy(
	tool_settings: &ToolSettings,
	selected_edit: &Rev,
) -> omp_tools::read::ReadPolicy {
	omp_tools::read::ReadPolicy {
		fetch_enabled:      tool_settings.fetch_enabled,
		credentialed_fetch: true,
		render_markdown:    tool_settings.render_markdown,
		auto_resize_images: tool_settings.auto_resize_images,
		hashline_headers:   tool_settings.enabled(tool_name(omp_tools::roster::Family::Edit))
			&& selected_edit.family.as_str() == "hl",
		summarize:          tool_settings.read_summarize,
		line_numbers:       tool_settings.read_line_numbers,
	}
}

/// The search policy of a production environment (`grep@1`, `glob@1`,
/// `ast_grep`), the one both its declarations and its executors are built
/// from, so the effects the environment declares for them are the ones their
/// calls are judged by.
///
/// As for [`production_read_policy`], the credentialed resolvers grep reads
/// and glob walks are registered whatever `tools.fetch.enabled` says, so the
/// credentialed fetch is always declared; the setting only removes the
/// anonymous fetch of URL roots.
pub(super) const fn production_search_policy(
	tool_settings: &ToolSettings,
) -> omp_tools::grep::SearchPolicy {
	omp_tools::grep::SearchPolicy {
		fetch_enabled:      tool_settings.fetch_enabled,
		credentialed_fetch: true,
	}
}

#[derive(Clone)]
pub(super) struct EnvironmentDeclaration {
	pub(super) spec:         ToolSpec,
	pub(super) presentation: Presentation,
	pub(super) claims:       Claims,
}

#[cfg(test)]
fn environment_declarations(
	tool_settings: &ToolSettings,
	#[cfg(feature = "cap-browser")] browser_settings: &BrowserSettings,
	inputs: &EnvironmentDeclarationInputs,
	py_eval: bool,
	policy: ToolsPolicy,
) -> Vec<EnvironmentDeclaration> {
	declare_environment_rows(
		tool_settings,
		#[cfg(feature = "cap-browser")]
		browser_settings,
		inputs,
		py_eval,
		policy,
		&omp_tools::roster::SelectedRoster::production(),
	)
}

fn declare_environment_rows(
	tool_settings: &ToolSettings,
	#[cfg(feature = "cap-browser")] browser_settings: &BrowserSettings,
	inputs: &EnvironmentDeclarationInputs,
	py_eval: bool,
	policy: ToolsPolicy,
	roster: &omp_tools::roster::SelectedRoster,
) -> Vec<EnvironmentDeclaration> {
	roster_host::DeclarationHost::collect(
		tool_settings,
		#[cfg(feature = "cap-browser")]
		browser_settings.enabled,
		inputs,
		py_eval,
		policy,
		roster,
	)
}

/// Declares the enabled environment half in a session-owned registry.
///
/// All inputs are frozen settings or content-derived snapshots; constructing
/// this half never opens an environment resource host.
fn declare_remote_environment(
	registry: &mut Registry,
	tool_settings: &ToolSettings,
	#[cfg(feature = "cap-browser")] browser_settings: &BrowserSettings,
	inputs: &EnvironmentDeclarationInputs,
	py_eval: bool,
	policy: ToolsPolicy,
	roster: &omp_tools::roster::SelectedRoster,
) -> Result<(), EnvdError> {
	for declaration in declare_environment_rows(
		tool_settings,
		#[cfg(feature = "cap-browser")]
		browser_settings,
		inputs,
		py_eval,
		policy,
		roster,
	) {
		registry.declare_remote(
			declaration.spec,
			declaration.presentation,
			declaration.claims,
			ExecutionMode::Parallel,
		)?;
	}
	Ok(())
}

pub(crate) struct SessionRegistryBridges {
	pub dynamic_tools:          Vec<DynamicTool>,
	pub dynamic_tool_factories: Vec<Arc<dyn DynamicToolFactory>>,
	pub goal_control:           Option<Arc<dyn GoalAuthority>>,
	pub search:                 Option<Arc<dyn SearchInference>>,
	pub telemetry_upload:       Option<Arc<dyn TelemetryUpload>>,
	pub ask_presenter:          Option<Arc<dyn omp_tools::ask::AskPresenter>>,
	/// Built-ins this session installs. `None` is the full production roster.
	pub selected_roster:        Option<omp_tools::roster::SelectedRoster>,
}

pub(crate) struct SessionRegistryOutput {
	pub registry:           Arc<Registry>,
	pub search_bridge:      Arc<SearchBridgeHost>,
	pub github_credentials: Arc<GithubCredentialBridge>,
	pub ask_presenter:      PresenterSlot,
	pub checkpoint_control: AgentCheckpointControl,
}

#[allow(
	clippy::too_many_arguments,
	reason = "session registry composition carries independent typed authorities"
)]
pub(crate) fn session_registry(
	mut registry: Registry,
	blobs: &BlobHost,
	project_root: &Path,
	state_dir: &Path,
	telemetry: &Arc<TelemetryIndex>,
	forge_cache: Arc<ForgeCache>,
	#[cfg(feature = "cap-forge")] forge_devices: crate::github_adapter::LiveForgeDeviceClient,
	workers: &ExtHostSupervisor,
	py_eval: bool,
	con: &Ctx,
	policy: ToolsPolicy,
	tool_settings: &ToolSettings,
	#[cfg(feature = "cap-browser")] browser_settings: &BrowserSettings,
	environment: &EnvironmentDeclarationInputs,
	bridges: SessionRegistryBridges,
) -> Result<SessionRegistryOutput, EnvdError> {
	let SessionRegistryBridges {
		dynamic_tools,
		dynamic_tool_factories,
		goal_control,
		search,
		telemetry_upload,
		ask_presenter,
		selected_roster,
	} = bridges;
	let roster = resolve_roster(selected_roster);
	prepare_registry(&mut registry, &roster)?;
	let base = register_session_base(
		&mut registry,
		dynamic_tools,
		dynamic_tool_factories,
		search,
		ask_presenter,
		goal_control,
		telemetry_upload,
		blobs,
		project_root,
		state_dir,
		telemetry,
		forge_cache,
		#[cfg(feature = "cap-forge")]
		forge_devices,
		tool_settings,
		#[cfg(feature = "cap-media")]
		image_config(con),
		#[cfg(feature = "cap-media")]
		speech_config(con),
		policy,
		&roster,
	)?;
	let mut declarations = environment.clone();
	let shell = declarations.shell_snapshot.take();
	declare_remote_environment(
		&mut registry,
		tool_settings,
		#[cfg(feature = "cap-browser")]
		browser_settings,
		&declarations,
		py_eval,
		policy,
		&roster,
	)?;
	let bash = omp_tools::roster::entry(omp_tools::roster::Family::Bash).expect("bash row");
	if roster
		.iter()
		.any(|row| row.family == omp_tools::roster::Family::Bash)
		&& tool_settings.enabled(bash.name)
	{
		if let Some(mut snapshot) = shell {
			snapshot.sibling_tools = registry
				.live_identities()
				.filter_map(|(name, _)| {
					(name.as_str() != bash.name
						&& registry.presentation(name).ok() == Some(Presentation::Slot))
					.then(|| name.clone())
				})
				.collect();
			let declaration = omp_tools::shell::spec(&snapshot);
			ensure_name_absent(&registry, &declaration.name)?;
			let (presentation, claims) = omp_tools::roster::declaration(bash, policy);
			registry.declare_remote(declaration, presentation, claims, ExecutionMode::Parallel)?;
		}
	}
	register_session_workers(&mut registry, workers, policy)?;
	Ok(SessionRegistryOutput {
		registry:           Arc::new(registry),
		search_bridge:      base.search_bridge,
		github_credentials: base.github_credentials,
		ask_presenter:      base.ask_presenter,
		checkpoint_control: base.checkpoint_control,
	})
}

/// Returns the live tools whose authoritative executor is the environment.
///
/// The set is derived exclusively from registry locus metadata so settings,
/// revisions, and contributed entries cannot drift from routing.
pub(crate) fn environment_tool_names(registry: &Registry) -> FastHashSet<Str> {
	registry
		.live_identities()
		.filter_map(|(name, _)| {
			(registry.locus(name).ok() == Some(ToolLocus::Environment)).then(|| name.clone())
		})
		.collect()
}

fn managed_skills_enabled(con: &Ctx, autolearn_enabled: bool) -> bool {
	autolearn_enabled && crate::SV_SKILLS_ENABLED.get(con)
}

/// Builds the complete registry shared by environment dispatch and the agent.
///
/// Resource adapters are cloned into their typed executors. Worker declarations
/// occupy device presentation entries and explicit worker routes; only the
/// environment's worker supervisor can invoke them.
pub(crate) fn production_registry<
	I: omp_tools::device::DeviceInvoker + Clone + 'static,
	P: PreludeInvoker + 'static,
>(
	documents: &DocumentHost,
	blobs: &BlobHost,
	exec: &ExecHost,
	state_dir: &Path,
	con: &Ctx,
	session_id: &str,
	forge_cache: Arc<ForgeCache>,
	#[cfg(feature = "cap-forge")] forge_devices: crate::github_adapter::LiveForgeDeviceClient,
	mcp: &Arc<McpService>,
	mcp_manager: Arc<McpManager>,
	workspace: &WorkspaceHost,
	memory: &Arc<omp_memory::MemoryRuntime>,
	telemetry: &Arc<TelemetryIndex>,
	root_uri: &Str,
	workers: &ExtHostSupervisor,
	interrupt_grace: Duration,
	py_eval: bool,
	tool_settings: &ToolSettings,
	#[cfg(feature = "cap-browser")] browser_settings: &BrowserSettings,
	shell_settings: &ShellSettings,
	sandbox_settings: &SandboxSettings,
	autolearn_settings: &omp_memory::config::AutolearnSettings,
	hooks: Arc<HookGate>,
	device_invoker: I,
	prelude_invoker: P,
	policy: ToolsPolicy,
	mut registry: Registry,
	bridges: RegistryBridges,
) -> Result<
	(
		Arc<Registry>,
		Arc<SessionBridgeHost>,
		Arc<ReflectionBridgeHost>,
		EvalSessionControl,
		AgentCheckpointControl,
		StagedProposalRegistry,
		Arc<ResolverTable<UrlResolver>>,
		Arc<SearchBridgeHost>,
		Arc<GithubCredentialBridge>,
		PresenterSlot,
	),
	EnvdError,
> {
	let RegistryBridges {
		command_credentials: _,
		dynamic_tools,
		dynamic_tool_factories,
		url_resolvers,
		goal_control,
		search,
		edit_model,
		edit_repair,
		host_resources,
		session_authority,
		telemetry_upload,
		ask_presenter,
		content,
		selected_roster,
	} = bridges;
	let roster = resolve_roster(selected_roster);
	exec.configure_sandbox(sandbox_settings, workspace.root());
	let previews = StagedProposalRegistry::new();
	prepare_registry(&mut registry, &roster)?;
	let SessionBaseOutput { search_bridge, github_credentials, ask_presenter, checkpoint_control } =
		register_session_base(
			&mut registry,
			dynamic_tools,
			dynamic_tool_factories,
			search,
			ask_presenter,
			goal_control,
			telemetry_upload,
			blobs,
			workspace.root(),
			state_dir,
			telemetry,
			Arc::clone(&forge_cache),
			#[cfg(feature = "cap-forge")]
			forge_devices.clone(),
			tool_settings,
			#[cfg(feature = "cap-media")]
			image_config(con),
			#[cfg(feature = "cap-media")]
			speech_config(con),
			policy,
			&roster,
		)?;
	let ActiveContentInputs { authored_skills, managed_skills_root, workspace_trust, .. } = content;
	let skills = if managed_skills_enabled(con, autolearn_settings.enabled) {
		managed_skills_root
			.map(|root| Arc::new(ManagedSkills::new(root, authored_skills, Arc::clone(&hooks))))
	} else {
		None
	};
	let mut environment = roster_host::EnvironmentHost::new(
		&mut registry,
		policy,
		tool_settings,
		#[cfg(feature = "cap-browser")]
		browser_settings,
		shell_settings,
		sandbox_settings,
		con,
		documents,
		blobs,
		exec,
		workspace,
		state_dir,
		session_id,
		root_uri,
		memory,
		workers,
		interrupt_grace,
		py_eval,
		skills,
		Arc::clone(&github_credentials),
		forge_cache,
		#[cfg(feature = "cap-forge")]
		forge_devices,
		url_resolvers,
		host_resources,
		session_authority,
		Arc::clone(mcp),
		edit_model,
		edit_repair,
		workspace_trust,
		previews,
	);
	omp_tools::roster::construct_environment(
		&mut environment,
		&roster,
		omp_tools::roster::InstallPhase::EnvironmentEarly,
	)?;
	environment.ensure_resources()?;
	omp_tools::roster::construct_environment(
		&mut environment,
		&roster,
		omp_tools::roster::InstallPhase::Environment,
	)?;
	let catalog = DeviceCatalog::default();
	let dyn_installed = tool_settings.enabled("dyn") && dyn_enabled(policy);
	if dyn_installed {
		let resolvers = environment.resolvers();
		exec.install_devices(Arc::new(DynHost::new(
			catalog.clone(),
			Arc::new(device_invoker),
			environment.previews(),
			Arc::clone(&hooks),
			blobs.clone(),
			mcp_manager,
			DynamicAdmission::new(
				tool_settings.configured_approval(),
				exec.sandbox_state(),
				tool_settings.approval.clone(),
				None,
			),
			FetchHostNamer::new(&resolvers),
		)));
	}
	environment.set_dyn_installed(dyn_installed);
	omp_tools::roster::construct_environment(
		&mut environment,
		&roster,
		omp_tools::roster::InstallPhase::Shell,
	)?;
	let roster_host::EnvironmentParts {
		reflection: reflection_bridge,
		eval_host,
		eval_control,
		previews,
		resolvers,
		prelude,
	} = environment.finish();
	register_session_workers(&mut registry, workers, policy)?;
	let registry = Arc::new(registry);
	catalog
		.install_registry(Arc::clone(&registry))
		.map_err(|_| EnvdError::WorkerDeclaration(sf!("dynamic device catalog installed twice")))?;
	eval_host
		.bind_registry(Arc::clone(&registry))
		.map_err(eval_error)?;
	eval_host
		.bind_prelude(prelude, Arc::new(prelude_invoker))
		.map_err(eval_error)?;
	// Nested `tool.<name>()` calls obey explicit per-tool `deny` overrides,
	// frozen at composition like the `dyn` builtin's nested admission.
	eval_host
		.bind_approval_overrides(tool_settings.approval.clone())
		.map_err(eval_error)?;
	Ok((
		registry,
		eval_host,
		reflection_bridge,
		eval_control,
		checkpoint_control,
		previews,
		resolvers,
		search_bridge,
		github_credentials,
		ask_presenter,
	))
}

pub(super) fn compose_eval_executor<T>(
	executor: Result<T, std::io::Error>,
	py_eval_explicit: bool,
) -> Result<Option<T>, EnvdError> {
	match executor {
		Ok(executor) => Ok(Some(executor)),
		Err(error) if py_eval_explicit => Err(eval_error_from(
			"environment composition could not construct the explicitly requested py_eval executor",
			error,
		)),
		Err(error) => {
			tracing::warn!(
				error = %error,
				"Python tools omitted because the eval child configuration is unavailable"
			);
			Ok(None)
		},
	}
}

#[derive(Clone)]
pub(super) struct GoalControlAdapter(pub(super) Arc<dyn GoalAuthority>);

impl omp_tools::goal::GoalControl for GoalControlAdapter {
	fn apply(
		&self,
		params: omp_tools::goal::Params,
	) -> impl Future<Output = Result<Option<omp_tools::goal::Goal>, goal::Fault>> + Send + '_ {
		async move { self.0.apply(params).await }
	}
}

#[derive(Clone)]
struct CheckpointBinding {
	id:     u64,
	sender: KernelSender,
}

#[derive(Clone)]
enum CheckpointWorkspace {
	Local(WorkspaceOperations),
	Owner(EnvClient),
}

#[derive(Clone)]
struct ActiveCheckpoint {
	binding_id: u64,
	info:       Arc<checkpoint::CheckpointInfo>,
}

/// Late-bound bridge from environment-owned checkpoint tools to the active
/// Agent CONTROL mailbox.
#[derive(Clone, Default)]
pub struct AgentCheckpointControl {
	sender:             Arc<RwLock<Option<CheckpointBinding>>>,
	workspace:          Arc<RwLock<Option<CheckpointWorkspace>>>,
	active_checkpoints: Arc<RwLock<Vec<ActiveCheckpoint>>>,
	rewind_pending:     Arc<RwLock<bool>>,
	transition:         Arc<AsyncMutex<()>>,
}

impl AgentCheckpointControl {
	/// Binds the local project environment's document-backed workspace owner.
	pub fn bind_local_workspace(&self, workspace: WorkspaceOperations) {
		*self.workspace.write() = Some(CheckpointWorkspace::Local(workspace));
	}

	/// Binds a child session host to its project environment's workspace owner.
	pub fn bind_owner_workspace(&self, workspace: EnvClient) {
		*self.workspace.write() = Some(CheckpointWorkspace::Owner(workspace));
	}

	/// Replaces the active session binding.
	pub fn bind(&self, id: u64, sender: KernelSender) {
		*self.sender.write() = Some(CheckpointBinding { id, sender });
		self.active_checkpoints.write().clear();
		*self.rewind_pending.write() = false;
	}

	/// Re-derives checkpoint execution state from the selected journal's DOM.
	/// The process-local cache never decides whether a checkpoint survives a
	/// switch, rewind, or resume.
	pub fn restore_session(&self, id: u64, dom: &omp_dom::Dom) {
		if !self
			.sender
			.read()
			.as_ref()
			.is_some_and(|binding| binding.id == id)
		{
			return;
		}
		let checkpoints = dom
			.handles()
			.filter_map(|handle| {
				let node = dom.get(handle)?;
				if node.tag != omp_dom::Tag::Custom(Str::new_static("rewind-checkpoint")) {
					return None;
				}
				let text = |name: &'static str| {
					node
						.prop(&omp_dom::PropKey::Custom(Str::new_static(name)))
						.and_then(omp_dom::Value::as_str)
						.map(Str::new)
				};
				let number = |name: &'static str| match node
					.prop(&omp_dom::PropKey::Custom(Str::new_static(name)))?
				{
					omp_dom::Value::Int(value) => u64::try_from(*value).ok(),
					_ => None,
				};
				let boolean = |name: &'static str| match node
					.prop(&omp_dom::PropKey::Custom(Str::new_static(name)))?
				{
					omp_dom::Value::Bool(value) => Some(*value),
					_ => None,
				};
				let label = node
					.prop(&omp_dom::PropKey::from(omp_dom::PropId::Label))
					.and_then(omp_dom::Value::as_str)
					.map(Str::new)?;
				let snapshot = env_wire::WorkspaceSnapshot {
					snapshot_id: text("workspace-snapshot")?.to_string(),
					generation: number("workspace-generation")?,
					root_uri: text("workspace-root")?.to_string(),
					tree_hash: text("workspace-tree")?.to_string(),
					files: number("workspace-files")?,
					bytes: number("workspace-bytes")?,
					entry_count: number("workspace-files")?,
					created_ms: number("workspace-created-at")?,
					label: Some(label.to_string()),
					parent_snapshot_id: text("workspace-parent").map(|value| value.to_string()),
					partial: boolean("workspace-partial")?,
					wire_revision: omp_proto::SCHEMA_REV,
					..Default::default()
				};
				Some(ActiveCheckpoint {
					binding_id: id,
					info:       Arc::new(checkpoint::CheckpointInfo {
						token: text("token")?,
						label,
						goal: text("goal")?,
						started_at: number("started-at")?,
						parent_token: text("parent-token"),
						session_target: text("target"),
						workspace: checkpoint_snapshot(&snapshot),
					}),
				})
			})
			.collect();
		*self.active_checkpoints.write() = checkpoints;
		*self.rewind_pending.write() = false;
	}

	/// Releases the binding only when it is still owned by `id`, returning
	/// whether this lease was current.
	pub fn unbind(&self, id: u64) -> bool {
		let mut binding = self.sender.write();
		if binding.as_ref().is_some_and(|binding| binding.id == id) {
			*binding = None;
			self.active_checkpoints.write().clear();
			*self.rewind_pending.write() = false;
			true
		} else {
			false
		}
	}

	fn binding(&self) -> Result<CheckpointBinding, omp_tools::checkpoint::CheckpointFault> {
		self
			.sender
			.read()
			.clone()
			.ok_or_else(|| omp_tools::checkpoint::CheckpointFault {
				code:    checkpoint::FaultCode::Control,
				message: sf!("active Agent CONTROL is not bound"),
			})
	}

	fn ensure_binding(&self, id: u64) -> Result<(), omp_tools::checkpoint::CheckpointFault> {
		if self
			.sender
			.read()
			.as_ref()
			.is_some_and(|binding| binding.id == id)
		{
			Ok(())
		} else {
			Err(checkpoint_fault(
				checkpoint::FaultCode::NotFound,
				"checkpoint belongs to another session",
			))
		}
	}

	fn workspace(&self) -> Result<CheckpointWorkspace, omp_tools::checkpoint::CheckpointFault> {
		self
			.workspace
			.read()
			.clone()
			.ok_or_else(|| omp_tools::checkpoint::CheckpointFault {
				code:    checkpoint::FaultCode::Control,
				message: sf!("workspace authority is not bound"),
			})
	}
}

impl CheckpointWorkspace {
	async fn snapshot(
		&self,
		request: env_wire::SnapshotWorkspace,
		cancel: &CancellationToken,
	) -> Result<env_wire::WorkspaceSnapshot, omp_tools::checkpoint::CheckpointFault> {
		let result = match self {
			Self::Local(workspace) => {
				let workspace = workspace.clone();
				let cancel = cancel.clone();
				tokio::task::spawn_blocking(move || workspace.snapshot(&request, &cancel))
					.await
					.map_err(|source| {
						tracing::warn!(?source, "checkpoint workspace snapshot worker failed");
						checkpoint_fault(
							checkpoint::FaultCode::SnapshotFailed,
							"workspace snapshot worker failed",
						)
					})?
					.map_err(local_snapshot_fault)
			},
			Self::Owner(workspace) => {
				tokio::select! {
					result = workspace.snapshot_workspace(request) => {
						result.map_err(|source| {
							tracing::warn!(?source, "checkpoint workspace capture failed");
							checkpoint_fault(
								checkpoint::FaultCode::SnapshotFailed,
								"workspace snapshot failed",
							)
						})
					},
					() = cancel.cancelled() => {
						Err(checkpoint_fault(
							checkpoint::FaultCode::RestoreCancelled,
							"workspace snapshot was cancelled",
						))
					},
				}
			},
		};
		if cancel.is_cancelled() {
			return Err(checkpoint_fault(
				checkpoint::FaultCode::RestoreCancelled,
				"workspace snapshot was cancelled",
			));
		}
		result
	}

	async fn restore(
		&self,
		request: env_wire::RestoreWorkspace,
		cancel: &CancellationToken,
	) -> Result<env_wire::WorkspaceRestored, omp_tools::checkpoint::CheckpointFault> {
		match self {
			Self::Local(workspace) => workspace
				.restore(&request, cancel)
				.await
				.map_err(local_workspace_fault),
			Self::Owner(workspace) => {
				if cancel.is_cancelled() {
					return Err(checkpoint_fault(
						checkpoint::FaultCode::RestoreCancelled,
						"workspace restoration was cancelled",
					));
				}
				let dry_run = request.dry_run;
				let restore = workspace.restore_workspace(request);
				tokio::pin!(restore);
				let result = if dry_run {
					tokio::select! {
						result = &mut restore => result,
						() = cancel.cancelled() => {
							return Err(checkpoint_fault(
								checkpoint::FaultCode::RestoreCancelled,
								"workspace restoration was cancelled",
							));
						},
					}
				} else {
					restore.await
				};
				result.map_err(|source| {
					tracing::warn!(?source, "checkpoint workspace restore failed");
					checkpoint_fault(
						checkpoint::FaultCode::RestoreFailed,
						"workspace restoration failed",
					)
				})
			},
		}
	}
}

impl omp_tools::checkpoint::CheckpointControl for AgentCheckpointControl {
	async fn create_checkpoint(
		&self,
		goal: Str,
		label: Str,
		cancel: CancellationToken,
	) -> Result<omp_tools::checkpoint::CheckpointAck, omp_tools::checkpoint::CheckpointFault> {
		let _transition = self.transition.lock().await;
		let binding = self.binding()?;
		let parent_token = {
			let checkpoints = self.active_checkpoints.read();
			if checkpoints
				.iter()
				.any(|checkpoint| checkpoint.info.label == label)
			{
				return Err(checkpoint_fault(
					checkpoint::FaultCode::DuplicateLabel,
					"checkpoint label already exists on the selected branch",
				));
			}
			checkpoints
				.last()
				.map(|checkpoint| checkpoint.info.token.clone())
		};
		let started_at = epoch_millis()?;
		let token = sf!("checkpoint-{}-{}", binding.id, Ulid::generate());
		let snapshot = self
			.workspace()?
			.snapshot(
				env_wire::SnapshotWorkspace {
					scope: "checkpoint".to_owned(),
					label: Some(label.to_string()),
					wire_revision: omp_proto::SCHEMA_REV,
					..Default::default()
				},
				&cancel,
			)
			.await?;
		self.ensure_binding(binding.id)?;
		let info = Arc::new(checkpoint::CheckpointInfo {
			token: token.clone(),
			label: label.clone(),
			goal: goal.clone(),
			started_at,
			parent_token: parent_token.clone(),
			session_target: None,
			workspace: checkpoint_snapshot(&snapshot),
		});
		binding
			.sender
			.send(omp_contract::EnvEvent::CheckpointOpened {
				token,
				label,
				goal,
				parent_token,
				started_at,
				workspace: snapshot,
			})
			.map_err(|_| {
				checkpoint_fault(checkpoint::FaultCode::Control, "active Agent mailbox is closed")
			})?;
		self
			.active_checkpoints
			.write()
			.push(ActiveCheckpoint { binding_id: binding.id, info: info.clone() });
		Ok(omp_tools::checkpoint::CheckpointAck { checkpoint: info })
	}

	fn list_checkpoints(
		&self,
		limit: u16,
	) -> impl Future<
		Output = Result<Vec<Arc<checkpoint::CheckpointInfo>>, omp_tools::checkpoint::CheckpointFault>,
	> + Send {
		let result = self.binding().map(|binding| {
			self
				.active_checkpoints
				.read()
				.iter()
				.rev()
				.filter(|checkpoint| checkpoint.binding_id == binding.id)
				.take(usize::from(limit))
				.map(|checkpoint| checkpoint.info.clone())
				.collect()
		});
		std::future::ready(result)
	}

	async fn schedule_rewind(
		&self,
		selector: Str,
		report: Str,
		cancel: CancellationToken,
	) -> Result<omp_tools::checkpoint::RewindAck, omp_tools::checkpoint::CheckpointFault> {
		let _transition = self.transition.lock().await;
		if *self.rewind_pending.read() {
			return Err(checkpoint_fault(
				checkpoint::FaultCode::AlreadyScheduled,
				"a rewind is already scheduled",
			));
		}
		let binding = self.binding()?;
		let active = {
			let checkpoints = self.active_checkpoints.read();
			checkpoints
				.iter()
				.find(|checkpoint| {
					checkpoint.binding_id == binding.id && checkpoint.info.token == selector
				})
				.cloned()
				.or_else(|| {
					let mut labels = checkpoints.iter().filter(|checkpoint| {
						checkpoint.binding_id == binding.id && checkpoint.info.label == selector
					});
					let checkpoint = labels.next()?.clone();
					labels.next().is_none().then_some(checkpoint)
				})
		}
		.ok_or_else(|| {
			let ambiguous = self
				.active_checkpoints
				.read()
				.iter()
				.filter(|checkpoint| {
					checkpoint.binding_id == binding.id && checkpoint.info.label == selector
				})
				.count() > 1;
			if ambiguous {
				checkpoint_fault(
					checkpoint::FaultCode::AmbiguousSelector,
					"checkpoint label is ambiguous; select by token",
				)
			} else {
				checkpoint_fault(
					checkpoint::FaultCode::NotFound,
					"checkpoint token or label is not on the selected branch",
				)
			}
		})?;
		let workspace = self.workspace()?;
		let request = env_wire::RestoreWorkspace {
			snapshot_id: active.info.workspace.snapshot_id.to_string(),
			dry_run: true,
			scope: "checkpoint".to_owned(),
			wire_revision: omp_proto::SCHEMA_REV,
			..Default::default()
		};
		let preview = workspace.restore(request.clone(), &cancel).await?;
		ensure_complete_restore(&preview)?;
		self.ensure_binding(binding.id)?;
		if cancel.is_cancelled() {
			return Err(checkpoint_fault(
				checkpoint::FaultCode::RestoreCancelled,
				"workspace restoration was cancelled",
			));
		}
		let restored = workspace
			.restore(
				env_wire::RestoreWorkspace {
					dry_run: false,
					expected_generation: preview.from_generation,
					..request
				},
				&cancel,
			)
			.await?;
		if let Err(fault) = ensure_complete_restore(&restored) {
			if restored.partial {
				rollback_workspace(&workspace, &restored).await;
			}
			return Err(fault);
		}
		if let Err(fault) = self.ensure_binding(binding.id) {
			rollback_workspace(&workspace, &restored).await;
			return Err(fault);
		}
		let receipt = sf!("rewind-{}", Ulid::generate());
		let rewound_at = epoch_millis()?;
		if binding
			.sender
			.send(omp_contract::EnvEvent::CheckpointRewind {
				token: active.info.token.clone(),
				report: report.clone(),
				receipt: receipt.clone(),
				workspace: restored.clone(),
				rewound_at,
			})
			.is_err()
		{
			rollback_workspace(&workspace, &restored).await;
			return Err(checkpoint_fault(
				checkpoint::FaultCode::Control,
				"active Agent mailbox is closed",
			));
		}
		*self.rewind_pending.write() = true;
		Ok(omp_tools::checkpoint::RewindAck {
			checkpoint: active.info,
			receipt,
			workspace: checkpoint_restore(&restored),
		})
	}
}

async fn rollback_workspace(
	workspace: &CheckpointWorkspace,
	restored: &env_wire::WorkspaceRestored,
) {
	if restored.undo_snapshot_id.is_empty() {
		return;
	}
	let rollback_cancel = CancellationToken::new();
	let rollback = workspace
		.restore(
			env_wire::RestoreWorkspace {
				snapshot_id: restored.undo_snapshot_id.clone(),
				scope: "checkpoint-rollback".to_owned(),
				wire_revision: omp_proto::SCHEMA_REV,
				..Default::default()
			},
			&rollback_cancel,
		)
		.await;
	if rollback.as_ref().is_err()
		|| rollback
			.as_ref()
			.is_ok_and(|value| value.partial || !value.conflicts.is_empty())
	{
		tracing::error!(
			snapshot = %restored.undo_snapshot_id,
			"checkpoint restoration and automatic rollback both failed"
		);
	}
}

fn epoch_millis() -> Result<u64, omp_tools::checkpoint::CheckpointFault> {
	let elapsed = std::time::SystemTime::now()
		.duration_since(std::time::UNIX_EPOCH)
		.map_err(|source| {
			tracing::warn!(?source, "checkpoint clock is before the Unix epoch");
			checkpoint_fault(checkpoint::FaultCode::Control, "system clock is unavailable")
		})?;
	Ok(u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX))
}

fn local_snapshot_fault(source: WorkspaceOperationError) -> omp_tools::checkpoint::CheckpointFault {
	let cancelled = matches!(
		&source,
		WorkspaceOperationError::Workspace(crate::workspace::WorkspaceError::Cancelled)
	);
	tracing::warn!(?source, "checkpoint workspace snapshot failed");
	if cancelled {
		checkpoint_fault(checkpoint::FaultCode::RestoreCancelled, "workspace snapshot was cancelled")
	} else {
		checkpoint_fault(checkpoint::FaultCode::SnapshotFailed, "workspace snapshot failed")
	}
}

fn local_workspace_fault(
	source: WorkspaceOperationError,
) -> omp_tools::checkpoint::CheckpointFault {
	let cancelled = matches!(
		&source,
		WorkspaceOperationError::Workspace(crate::workspace::WorkspaceError::Cancelled)
	);
	tracing::warn!(?source, "checkpoint workspace operation failed");
	if cancelled {
		checkpoint_fault(checkpoint::FaultCode::RestoreCancelled, "workspace operation was cancelled")
	} else {
		checkpoint_fault(checkpoint::FaultCode::RestoreFailed, "workspace operation failed")
	}
}

fn ensure_complete_restore(
	restored: &env_wire::WorkspaceRestored,
) -> Result<(), omp_tools::checkpoint::CheckpointFault> {
	if restored.partial {
		return Err(checkpoint_fault(
			checkpoint::FaultCode::RestoreFailed,
			"workspace restoration partially committed; the undo snapshot was retained",
		));
	}
	if !restored.conflicts.is_empty() {
		return Err(omp_tools::checkpoint::CheckpointFault {
			code:    checkpoint::FaultCode::RestoreConflict,
			message: sf!(
				"workspace restoration blocked by {} conflict(s), first at {}",
				restored.conflicts.len(),
				restored.conflicts[0].path
			),
		});
	}
	Ok(())
}

fn checkpoint_snapshot(
	snapshot: &env_wire::WorkspaceSnapshot,
) -> omp_tools::checkpoint::WorkspaceSnapshot {
	omp_tools::checkpoint::WorkspaceSnapshot {
		snapshot_id:        Str::new(&snapshot.snapshot_id),
		root_uri:           Str::new(&snapshot.root_uri),
		generation:         snapshot.generation,
		tree_hash:          Str::new(&snapshot.tree_hash),
		files:              snapshot.files,
		bytes:              snapshot.bytes,
		label:              snapshot.label.as_deref().map(Str::new),
		parent_snapshot_id: snapshot.parent_snapshot_id.as_deref().map(Str::new),
		created_at:         snapshot.created_ms,
		partial:            snapshot.partial,
	}
}

fn checkpoint_restore(
	restored: &env_wire::WorkspaceRestored,
) -> omp_tools::checkpoint::WorkspaceRestore {
	omp_tools::checkpoint::WorkspaceRestore {
		snapshot_id:      Str::new(&restored.snapshot_id),
		undo_snapshot_id: Str::new(&restored.undo_snapshot_id),
		written:          restored.written,
		deleted:          restored.deleted,
		unchanged:        restored.unchanged,
		from_generation:  restored.from_generation,
		to_generation:    restored.to_generation,
	}
}

fn checkpoint_fault(
	code: checkpoint::FaultCode,
	message: &'static str,
) -> omp_tools::checkpoint::CheckpointFault {
	omp_tools::checkpoint::CheckpointFault { code, message: sf!(message) }
}

#[cfg(test)]
pub(crate) fn python_engine() -> Result<Arc<omp_py::Engine>, EnvdError> {
	static ENGINE: LazyLock<Result<Arc<omp_py::Engine>, Str>> = LazyLock::new(|| {
		omp_py::Engine::builder()
			.init()
			.map(Arc::new)
			.map_err(|error| Str::from(error.to_string()))
	});
	ENGINE
		.as_ref()
		.map(Arc::clone)
		.map_err(|error| EnvdError::Eval(error.clone()))
}

fn ensure_name_absent(registry: &Registry, name: &str) -> Result<(), EnvdError> {
	if registry.live_identity(name).is_some() {
		return Err(EnvdError::DuplicateToolName { name: Str::from(name) });
	}
	Ok(())
}

pub(super) fn shell_timeout_bounds(settings: &ToolSettings) -> TimeoutBounds {
	let mut bounds = TimeoutBounds::default();
	let Some(maximum) = settings.max_timeout else {
		return bounds;
	};
	let milliseconds = maximum
		.to_std()
		.ok()
		.and_then(|duration| u64::try_from(duration.as_millis()).ok())
		.unwrap_or(bounds.ceiling_ms);
	bounds.ceiling_ms = milliseconds.max(bounds.floor_ms).min(bounds.ceiling_ms);
	bounds.default_ms = bounds
		.default_ms
		.min(bounds.ceiling_ms)
		.max(bounds.floor_ms);
	bounds
}

pub(super) fn build_prelude_table(workers: &ExtHostSupervisor) -> Result<PreludeTable, EnvdError> {
	let mut registrations = Vec::new();
	let mut ordinary_names = BTreeSet::new();
	for registration in workers.registrations() {
		if !is_prelude_declaration(&registration.declaration)? {
			if let Some(definition) = &registration.declaration.definition {
				ordinary_names.insert(Str::from(definition.name.as_str()));
			}
			continue;
		}
		let definition = registration
			.declaration
			.definition
			.as_ref()
			.ok_or_else(|| worker_declaration_error("prelude helper declaration has no definition"))?;
		if registration.declaration.extension_id.is_empty() {
			return Err(worker_declaration_error("prelude helper declaration has no extension id"));
		}
		registrations.push((
			Str::from(definition.name.as_str()),
			registration.owner.extension().clone(),
			&registration.declaration,
		));
	}
	registrations.sort_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(&right.1)));

	let mut table = PreludeTable::new();
	let mut declared_by = BTreeMap::<Str, Str>::new();
	for (name, owner, declaration) in registrations {
		if let Some(first) = declared_by.get(&name) {
			return Err(EnvdError::WorkerDeclaration(Str::from(format!(
				"prelude helper {name} is declared by both {first} and {owner}"
			))));
		}
		if PRELUDE_RESERVED_NAMES.contains(&name.as_str()) {
			return Err(EnvdError::WorkerDeclaration(Str::from(format!(
				"prelude helper {name} shadows a prelude builtin"
			))));
		}
		if name.starts_with("__") {
			return Err(EnvdError::WorkerDeclaration(Str::from(format!(
				"prelude helper {name} uses the reserved dunder namespace"
			))));
		}
		if !valid_prelude_name(name.as_str()) {
			return Err(EnvdError::WorkerDeclaration(Str::from(format!(
				"prelude helper {name} has an invalid name"
			))));
		}
		if PRELUDE_PYTHON_KEYWORDS.contains(&name.as_str()) {
			return Err(EnvdError::WorkerDeclaration(Str::from(format!(
				"prelude helper {name} uses a Python keyword"
			))));
		}
		if ordinary_names.contains(&name) {
			return Err(EnvdError::WorkerDeclaration(Str::from(format!(
				"prelude helper {name} collides with a worker tool"
			))));
		}
		let params = prelude_params(&name, declaration)?;
		let helper = PreludeHelper::new(
			name.clone(),
			Str::from(declaration.rev.as_str()),
			Str::from(declaration.docs.as_str()),
			Str::from(declaration.summary.as_str()),
			params,
		);
		let previous = table.insert(helper);
		debug_assert!(previous.is_none(), "duplicate prelude helper checked above");
		declared_by.insert(name, owner);
	}
	Ok(table)
}
fn valid_prelude_name(name: &str) -> bool {
	let Some((&first, rest)) = name.as_bytes().split_first() else {
		return false;
	};
	name.len() <= 64
		&& first.is_ascii_lowercase()
		&& rest
			.iter()
			.all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'_')
}

fn valid_prelude_param_name(name: &str) -> bool {
	let Some((&first, rest)) = name.as_bytes().split_first() else {
		return false;
	};
	(first == b'_' || first.is_ascii_alphabetic())
		&& rest
			.iter()
			.all(|byte| *byte == b'_' || byte.is_ascii_alphanumeric())
}

fn prelude_params(
	helper_name: &str,
	declaration: &ToolDecl,
) -> Result<Vec<PreludeParamStub>, EnvdError> {
	let mut params = Vec::with_capacity(declaration.prelude_params.len());
	let mut names = BTreeSet::new();
	let mut keyword_only = false;
	let mut positional_default = false;
	for param in &declaration.prelude_params {
		if !valid_prelude_param_name(&param.name) {
			return Err(EnvdError::WorkerDeclaration(Str::from(format!(
				"prelude helper {helper_name} parameter {} has an invalid name",
				param.name
			))));
		}
		if PRELUDE_PYTHON_KEYWORDS.contains(&param.name.as_str()) {
			return Err(EnvdError::WorkerDeclaration(Str::from(format!(
				"prelude helper {helper_name} parameter {} uses a Python keyword",
				param.name
			))));
		}
		let param_name = Str::from(param.name.as_str());
		if !names.insert(param_name.clone()) {
			return Err(EnvdError::WorkerDeclaration(Str::from(format!(
				"prelude helper {helper_name} declares parameter {} more than once",
				param.name
			))));
		}
		let kind = PreludeParamKind::try_from(param.kind).map_err(|_| {
			EnvdError::WorkerDeclaration(Str::from(format!(
				"prelude helper {helper_name} parameter {} has an invalid kind",
				param.name
			)))
		})?;
		match kind {
			PreludeParamKind::Unspecified => {
				return Err(EnvdError::WorkerDeclaration(Str::from(format!(
					"prelude helper {helper_name} parameter {} has an unspecified kind",
					param.name
				))));
			},
			PreludeParamKind::PositionalOrKeyword => {
				if keyword_only {
					return Err(EnvdError::WorkerDeclaration(Str::from(format!(
						"prelude helper {helper_name} has a positional parameter after a keyword-only \
						 parameter"
					))));
				}
				if param.default_json.is_some() {
					positional_default = true;
				} else if positional_default {
					return Err(EnvdError::WorkerDeclaration(Str::from(format!(
						"prelude helper {helper_name} has a required positional parameter after a \
						 default"
					))));
				}
			},
			PreludeParamKind::KeywordOnly => keyword_only = true,
		}
		let default_json = param
			.default_json
			.as_deref()
			.map(|raw| {
				serde_json::from_slice::<serde_json::Value>(raw).map_err(|error| {
					worker_declaration_from(
						&format!(
							"prelude helper {helper_name} parameter {} has an invalid JSON default",
							param.name
						),
						error,
					)
				})?;
				Str::from_utf8(raw).map_err(|error| {
					worker_declaration_from(
						&format!(
							"prelude helper {helper_name} parameter {} has a non-UTF-8 default",
							param.name
						),
						error,
					)
				})
			})
			.transpose()?;
		params.push(PreludeParamStub {
			name: param_name,
			kind,
			default_json,
			annotation: param.annotation.as_deref().map(Str::from),
		});
	}
	Ok(params)
}

fn worker_revision(declaration: &ToolDecl) -> Result<Rev, EnvdError> {
	declaration
		.rev
		.parse::<Rev>()
		.map_err(worker_declaration_fault)
}

fn is_prelude_declaration(declaration: &ToolDecl) -> Result<bool, EnvdError> {
	Ok(worker_revision(declaration)?.family == "prelude")
}

/// Lowers one worker declaration into its registry spec.
///
/// `owner` is the extension host that registered it: its trust tier is the
/// effect ceiling of a declaration that carries no envelope
/// ([`HostKey::undeclared_effects`]), so an undeclared tool is never the
/// auto-approved `read` tier.
pub fn worker_spec(declaration: &ToolDecl, owner: &HostKey) -> Result<ToolSpec, EnvdError> {
	let definition = declaration.definition.as_ref().ok_or_else(|| {
		EnvdError::WorkerDeclaration(sf!("worker tool declaration has no definition"))
	})?;
	if declaration.extension_id.is_empty() {
		return Err(worker_declaration_error("worker tool declaration has no extension id"));
	}
	let Some(tool_def::Input::JsonSchema(json_schema)) = definition.input.as_ref() else {
		return Err(worker_declaration_error("worker tool definition requires a JSON Schema input"));
	};
	Ok(ToolSpec {
		name:            Str::from(definition.name.as_str()),
		rev:             worker_revision(declaration)?,
		description:     Str::from(definition.description.as_str()),
		schema:          omp_tool::inject_protocol_schema(&json_schema.schema_json)?,
		constraint:      worker_constraint(declaration)?,
		// Asserted here, never read from the declaration: an extension cannot
		// claim that the exec sandbox confines its tool, whatever its host tier.
		confinement:     omp_tool::Confinement::Host,
		projection_code: worker_projection_code(declaration),
		effects:         owner
			.declared_or_undeclared_effects(declaration.effects.as_ref())
			.map_err(worker_declaration_fault)?,
	})
}

fn worker_projection_code(declaration: &ToolDecl) -> [u8; 32] {
	let mut hasher = Hash32::hasher();
	hasher.update(b"omp/frozen-worker-registration/v1");
	hasher.update(declaration.encode_to_vec());
	hasher.finalize().into_bytes()
}

fn worker_constraint(declaration: &ToolDecl) -> Result<Constraint, EnvdError> {
	let Some(kind) = declaration
		.constraint
		.as_ref()
		.and_then(|value| value.kind.as_ref())
	else {
		let strict = declaration
			.definition
			.as_ref()
			.and_then(|definition| definition.input.as_ref())
			.and_then(|input| match input {
				tool_def::Input::JsonSchema(schema) => schema.strict,
				tool_def::Input::Grammar(_) => None,
			})
			.unwrap_or(false);
		return Ok(if strict {
			Constraint::Schema { priority: 100, on_unsupported: v1::Fallback::Unspecified }
		} else {
			Constraint::None
		});
	};
	match kind {
		tool_constraint::Kind::Schema(schema) => Ok(Constraint::Schema {
			priority:       constraint_priority(schema.priority)?,
			on_unsupported: worker_fallback(schema.on_unsupported)?,
		}),
		tool_constraint::Kind::Grammar(grammar) => {
			let syntax = match WorkerGrammarSyntax::try_from(grammar.syntax) {
				Ok(WorkerGrammarSyntax::Lark) => GrammarSyntax::Lark,
				Ok(WorkerGrammarSyntax::Regex) => GrammarSyntax::Regex,
				Ok(WorkerGrammarSyntax::Ebnf) => GrammarSyntax::Ebnf,
				_ => {
					return Err(worker_declaration_error(
						"worker grammar constraint has an unsupported syntax",
					));
				},
			};
			Ok(Constraint::Grammar {
				syntax,
				definition: Str::from(grammar.definition.as_str()),
				priority: constraint_priority(grammar.priority)?,
				on_unsupported: worker_fallback(grammar.on_unsupported)?,
			})
		},
		tool_constraint::Kind::Textual(_) => {
			Err(worker_declaration_error("worker textual constraints are not supported"))
		},
		tool_constraint::Kind::Json(_) => {
			Err(worker_declaration_error("worker JSON constraints are not supported"))
		},
	}
}

fn worker_fallback(value: i32) -> Result<v1::Fallback, EnvdError> {
	v1::Fallback::try_from(value)
		.map_err(|_| worker_declaration_error("worker constraint fallback is invalid"))
}

fn constraint_priority(priority: u32) -> Result<u8, EnvdError> {
	u8::try_from(priority)
		.map_err(|_| worker_declaration_error("worker constraint priority exceeds u8"))
}

const fn worker_declaration_error(message: &'static str) -> EnvdError {
	EnvdError::WorkerDeclaration(sf!(message))
}

/// Creates a worker-declaration error, rendering the cause once at this
/// model-facing fault boundary.
fn worker_declaration_fault(cause: impl std::fmt::Display) -> EnvdError {
	EnvdError::WorkerDeclaration(sf!("{cause}"))
}

/// Creates a worker-declaration error with leading context, rendering the
/// cause once at this model-facing fault boundary.
fn worker_declaration_from(context: &str, cause: impl std::fmt::Display) -> EnvdError {
	EnvdError::WorkerDeclaration(sf!("{context}: {cause}"))
}

/// Creates a slot-collision error, rendering the collision once at this
/// model-facing fault boundary.
fn slot_collision_error(collision: FlattenCollision) -> EnvdError {
	EnvdError::WorkerDeclaration(sf!(
		"tool_only slot {} is owned by both {} and {}",
		collision.slot,
		collision.existing_owner,
		collision.conflicting_owner
	))
}

/// Creates a state error, rendering the cause once at this model-facing
/// fault boundary.
pub(super) fn state_error(cause: impl std::fmt::Display) -> EnvdError {
	EnvdError::State(sf!("{cause}"))
}

/// Creates an eval error, rendering the cause once at this model-facing
/// fault boundary.
fn eval_error(cause: impl std::fmt::Display) -> EnvdError {
	EnvdError::Eval(sf!("{cause}"))
}

/// Creates an eval error with leading context, rendering the cause once at
/// this model-facing fault boundary.
fn eval_error_from(context: &str, cause: impl std::fmt::Display) -> EnvdError {
	EnvdError::Eval(sf!("{context}: {cause}"))
}

/// Creates an edit-dialect error, rendering the cause once at this
/// model-facing fault boundary.
fn edit_dialect_error(cause: impl std::fmt::Display) -> EnvdError {
	EnvdError::EditDialect(sf!("{cause}"))
}

#[cfg(test)]
mod tests {
	use super::*;

	static EVAL_ENV_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

	struct EvalEnvRestore(Vec<(&'static str, Option<std::ffi::OsString>)>);

	impl EvalEnvRestore {
		fn set(values: &[(&'static str, &std::ffi::OsStr)]) -> Self {
			let previous = values
				.iter()
				.map(|(name, _)| (*name, env::var_os(name)))
				.collect();
			for (name, value) in values {
				// SAFETY: every mutation of these eval-specific variables in this
				// module is serialized by EVAL_ENV_LOCK and restored on drop.
				unsafe { env::set_var(name, value) };
			}
			Self(previous)
		}
	}

	impl Drop for EvalEnvRestore {
		fn drop(&mut self) {
			for (name, value) in self.0.drain(..).rev() {
				// SAFETY: EVAL_ENV_LOCK remains held until after this guard drops.
				unsafe {
					if let Some(value) = value {
						env::set_var(name, value);
					} else {
						env::remove_var(name);
					}
				}
			}
		}
	}

	#[test]
	fn explicit_py_eval_failure_is_typed_and_environment_override_has_precedence() {
		let _lock = EVAL_ENV_LOCK.lock();
		let scratch = tempfile::tempdir().expect("eval scratch");
		let current_exe = env::current_exe().expect("current test executable");
		let invalid_override = scratch.path().join("missing-python");
		let _restore = EvalEnvRestore::set(&[
			("CARGO_BIN_EXE_omp", current_exe.as_os_str()),
			("OMP_PYTHON_INTERPRETER", invalid_override.as_os_str()),
		]);
		let blobs = BlobHost::open(scratch.path().join("blobs")).expect("blob host");
		let constructed = ProcessEvalExec::production(
			ExecHost::new(),
			Arc::new(SessionBridgeHost::new()),
			"1s".parse().expect("interrupt grace"),
			blobs,
			Some(current_exe),
		);
		let error = match compose_eval_executor(constructed, true) {
			Err(error) => error,
			Ok(_) => panic!("explicit py_eval must reject an invalid environment override"),
		};
		let EnvdError::Eval(message) = error else {
			panic!("explicit py_eval failure must use the typed eval composition error");
		};
		assert!(message.contains("explicitly requested py_eval"));
		assert!(
			message.contains(invalid_override.to_string_lossy().as_ref()),
			"the environment override must win over the valid configured interpreter"
		);
	}

	#[test]
	fn incidental_eval_executor_failure_remains_an_omission() {
		let unavailable = std::io::Error::new(
			std::io::ErrorKind::NotFound,
			"configured Python interpreter is unavailable",
		);
		assert!(matches!(compose_eval_executor::<()>(Err(unavailable), false), Ok(None)));
	}

	#[tokio::test]
	async fn checkpoint_cache_rehydrates_from_the_selected_branch_and_clears_on_switch() {
		let control = AgentCheckpointControl::default();
		let (sender, _mailbox) = flume::unbounded();
		control.bind(7, Arc::new(sender));
		let mut dom = omp_dom::Dom::new();
		dom.apply(&omp_dom::Txn {
			cause: omp_journal::EntryId::default(),
			label: None,
			ops:   vec![omp_dom::Op::Ins {
				parent: dom.meta(),
				after:  None,
				node:   omp_dom::NodeSpec::new(omp_dom::Tag::Custom(sf!("rewind-checkpoint")))
					.with_prop(
						omp_dom::PropKey::Custom(sf!("token")),
						omp_dom::Value::Str(sf!("checkpoint-1")),
					)
					.with_prop(omp_dom::PropId::Label, omp_dom::Value::Str(sf!("parser-baseline")))
					.with_prop(
						omp_dom::PropKey::Custom(sf!("goal")),
						omp_dom::Value::Str(sf!("inspect parser")),
					)
					.with_prop(
						omp_dom::PropKey::Custom(sf!("target")),
						omp_dom::Value::Str(sf!("01K4TARGET")),
					)
					.with_prop(
						omp_dom::PropKey::Custom(sf!("workspace-snapshot")),
						omp_dom::Value::Str(sf!("snapshot-1")),
					)
					.with_prop(
						omp_dom::PropKey::Custom(sf!("workspace-generation")),
						omp_dom::Value::Int(3),
					)
					.with_prop(
						omp_dom::PropKey::Custom(sf!("workspace-root")),
						omp_dom::Value::Str(sf!("file:///workspace")),
					)
					.with_prop(
						omp_dom::PropKey::Custom(sf!("workspace-tree")),
						omp_dom::Value::Str(sf!("tree")),
					)
					.with_prop(omp_dom::PropKey::Custom(sf!("workspace-files")), omp_dom::Value::Int(2))
					.with_prop(omp_dom::PropKey::Custom(sf!("workspace-bytes")), omp_dom::Value::Int(12))
					.with_prop(
						omp_dom::PropKey::Custom(sf!("workspace-parent")),
						omp_dom::Value::Str(sf!("snapshot-0")),
					)
					.with_prop(
						omp_dom::PropKey::Custom(sf!("workspace-created-at")),
						omp_dom::Value::Int(42),
					)
					.with_prop(
						omp_dom::PropKey::Custom(sf!("workspace-partial")),
						omp_dom::Value::Bool(false),
					)
					.with_prop(omp_dom::PropKey::Custom(sf!("started-at")), omp_dom::Value::Int(42)),
			}],
		})
		.expect("checkpoint DOM");

		control.restore_session(7, &dom);
		let active = control.active_checkpoints.read();
		assert_eq!(active.first().map(|value| value.info.token.as_str()), Some("checkpoint-1"));
		assert_eq!(
			active
				.first()
				.map(|value| value.info.workspace.snapshot_id.as_str()),
			Some("snapshot-1")
		);
		assert_eq!(
			active
				.first()
				.and_then(|value| value.info.session_target.as_deref()),
			Some("01K4TARGET")
		);
		assert_eq!(
			active
				.first()
				.and_then(|value| value.info.workspace.parent_snapshot_id.as_deref()),
			Some("snapshot-0")
		);
		drop(active);
		let listed = omp_tools::checkpoint::CheckpointControl::list_checkpoints(&control, 10)
			.await
			.expect("checkpoint list");
		assert_eq!(listed.first().map(|value| value.label.as_str()), Some("parser-baseline"));

		control.restore_session(7, &omp_dom::Dom::new());
		assert!(control.active_checkpoints.read().is_empty());
	}

	#[test]
	fn skills_enabled_gates_managed_skill_runtime() {
		let ctx = Ctx::new();
		assert!(managed_skills_enabled(&ctx, true));
		crate::SV_SKILLS_ENABLED
			.set(&ctx, false)
			.expect("disable skills");
		assert!(!managed_skills_enabled(&ctx, true));
		assert!(!managed_skills_enabled(&Ctx::new(), false));
	}

	fn worker_declaration_with_schema(schema: &'static [u8]) -> ToolDecl {
		ToolDecl {
			definition: Some(omp_proto::inference::v1::ToolDef {
				name:        "worker".to_owned(),
				description: "worker tool".to_owned(),
				input:       Some(tool_def::Input::JsonSchema(
					omp_proto::inference::v1::tool_def::JsonSchema {
						schema_json: bytes::Bytes::from_static(schema),
						strict:      Some(true),
					},
				)),
			}),
			rev: "1".to_owned(),
			extension_id: "test.extension".to_owned(),
			..ToolDecl::default()
		}
	}

	fn trusted_host() -> HostKey {
		HostKey::new("project", "trusted", "test.extension")
	}

	fn undeclared_worker() -> ToolDecl {
		worker_declaration_with_schema(br#"{"type":"object","properties":{}}"#)
	}

	fn admit(
		spec: &ToolSpec,
		mode: crate::admission::ApprovalMode,
	) -> crate::admission::ResolvedApproval {
		crate::admission::resolve_approval(
			"call-1",
			spec.name.clone(),
			&spec.effects,
			spec.confinement,
			crate::admission::ConfiguredApproval {
				mode,
				provenance: crate::admission::Provenance::Explicit,
				workspace_trust: omp_ext::workspace_trust::WorkspaceTrust::Trusted,
			},
			crate::admission::SandboxState::Active,
			None,
		)
	}

	/// A worker tool that declares no envelope resolves to its host's ceiling,
	/// never to the auto-approved `read` tier: `exec` under a trusted host (and
	/// any tier this build does not know), `write` under a sandboxed one. A
	/// declared envelope, even an empty one, is taken as declared.
	#[test]
	fn undeclared_worker_effects_follow_the_host_trust_tier() {
		use crate::admission::{ApprovalMode, ApprovalPolicy, ApprovalTier};
		let trusted = worker_spec(&undeclared_worker(), &trusted_host()).expect("trusted");
		assert_eq!(trusted.effects, omp_tool::Effects::unknown());
		assert_eq!(trusted.confinement, omp_tool::Confinement::Host);
		let resolved = admit(&trusted, ApprovalMode::Write);
		assert_eq!((resolved.tier, resolved.policy), (ApprovalTier::Exec, ApprovalPolicy::Prompt));

		let sandboxed_host = HostKey::new("project", "sandboxed", "test.extension");
		let sandboxed = worker_spec(&undeclared_worker(), &sandboxed_host).expect("sandboxed");
		assert_eq!(sandboxed.effects, sandboxed_host.undeclared_effects());
		assert_eq!(
			sandboxed.effects.documents,
			Some(omp_tool::DocEffects { read: true, write_globs: Arc::from([sf!("**")]) })
		);
		let resolved = admit(&sandboxed, ApprovalMode::Write);
		assert_eq!((resolved.tier, resolved.policy), (ApprovalTier::Write, ApprovalPolicy::Allow));
		let resolved = admit(&sandboxed, ApprovalMode::AlwaysAsk);
		assert_eq!(resolved.policy, ApprovalPolicy::Prompt);

		let unknown_tier = HostKey::new("project", "experimental", "test.extension");
		let spec = worker_spec(&undeclared_worker(), &unknown_tier).expect("unrecognized tier");
		assert_eq!(spec.effects, omp_tool::Effects::unknown());

		// Explicit envelopes are respected under either host.
		let mut declared = undeclared_worker();
		declared.effects = Some(omp_proto::policy::v1::EffectEnvelope::default());
		for host in [trusted_host(), sandboxed_host] {
			let spec = worker_spec(&declared, &host).expect("declared");
			assert_eq!(spec.effects, omp_tool::Effects::empty());
			let resolved = admit(&spec, ApprovalMode::AlwaysAsk);
			assert_eq!((resolved.tier, resolved.policy), (ApprovalTier::Read, ApprovalPolicy::Allow));
		}
	}

	#[test]
	fn worker_schema_injects_exact_protocol_fields() {
		let declaration = worker_declaration_with_schema(
			br#"{"type":"object","properties":{"value":{"type":"string"}},"required":["value"]}"#,
		);
		let spec = worker_spec(&declaration, &trusted_host()).expect("valid worker schema");
		let schema: JsonValue = serde_json::from_slice(&spec.schema).expect("injected schema");
		assert_eq!(schema["required"], json!(["i", "value"]));
		assert_eq!(schema["properties"]["i"]["type"], "string");
		assert_eq!(schema["properties"]["notrunc"]["type"], "boolean");
	}

	#[test]
	fn worker_schema_rejects_invalid_protocol_shapes() {
		for schema in [
			&b"{"[..],
			br#"{"type":"array"}"#,
			br#"{"type":"object","properties":[]}"#,
			br#"{"type":"object","required":{}}"#,
			br#"{"type":"object","required":[1]}"#,
		] {
			assert!(
				matches!(
					worker_spec(&worker_declaration_with_schema(schema), &trusted_host()),
					Err(EnvdError::WorkerProtocolSchema(_))
				),
				"schema should be rejected: {}",
				String::from_utf8_lossy(schema)
			);
		}
	}

	#[test]
	fn default_roster_has_exactly_five_slots_and_keeps_long_tail_devices_live() {
		let inputs = EnvironmentDeclarationInputs {
			read_policy:      omp_tools::read::ReadPolicy::default(),
			selected_edit:    omp_tools::edit::hashline_spec().rev,
			eval_description: Some(sf!("Evaluate code.")),
			shell_snapshot:   Some(omp_tools::shell::ShellPromptSnapshot {
				sibling_tools:       Arc::default(),
				platform:            sf!("linux"),
				command_prefix:      false,
				embedded_builtins:   true,
				devices:             true,
				interceptor_enabled: false,
				interceptor_rules:   Arc::default(),
			}),
			memory:           omp_memory::Capabilities::default(),
			managed_skills:   false,
		};
		let mut tool_settings = ToolSettings::default();
		tool_settings
			.enabled
			.insert(Str::new_static("ast_grep"), true);
		#[cfg(feature = "cap-browser")]
		let browser_settings = BrowserSettings::default();
		let py_eval_declaration = environment_declarations(
			&tool_settings,
			#[cfg(feature = "cap-browser")]
			&browser_settings,
			&inputs,
			true,
			ToolsPolicy::Auto,
		)
		.into_iter()
		.find(|declaration| declaration.spec.name == "py_eval")
		.expect("explicit py_eval declaration");
		assert_eq!(py_eval_declaration.spec, omp_tools::eval::py_eval_spec());

		let declarations = environment_declarations(
			&tool_settings,
			#[cfg(feature = "cap-browser")]
			&browser_settings,
			&inputs,
			false,
			ToolsPolicy::Auto,
		);
		assert!(
			declarations
				.iter()
				.all(|declaration| declaration.spec.name != "py_eval"),
			"py_eval must remain absent unless explicitly requested"
		);
		let slots = declarations
			.iter()
			.filter(|declaration| declaration.presentation == Presentation::Slot)
			.map(|declaration| declaration.spec.name.clone())
			.collect::<BTreeSet<_>>();
		assert_eq!(
			slots,
			[sf!("bash"), sf!("edit"), sf!("glob"), sf!("grep"), sf!("read")]
				.into_iter()
				.collect()
		);

		let mut registry = Registry::new();
		for declaration in declarations {
			registry
				.declare_remote(
					declaration.spec,
					declaration.presentation,
					declaration.claims,
					ExecutionMode::Parallel,
				)
				.expect("declare environment tool");
		}
		let registry = Arc::new(registry);
		let catalog = DeviceCatalog::default();
		catalog
			.install_registry(Arc::clone(&registry))
			.expect("install device catalog");
		let live = catalog.registry().expect("live catalog");
		let lsp = live
			.devices()
			.find(|device| device.name.as_str() == "lsp")
			.expect("LSP dynamic device");
		let live_lsp = omp_tools::lsp::spec();
		assert_eq!(*lsp.rev, live_lsp.rev);
		assert_eq!(lsp.schema, live_lsp.schema.as_ref());
		let devices = live
			.devices()
			.map(|device| device.name.clone())
			.collect::<BTreeSet<_>>();
		for expected in ["ast_edit", "ast_grep", "debug", "eval", "lsp", "write"] {
			assert!(devices.contains(expected), "{expected} must remain reachable through dyn");
		}
	}

	/// Only tools whose processes run under the exec sandbox claim it: `bash@2`
	/// and `hub@2`. Every environment declaration (all of them enabled), the
	/// session base, and the kernel's session tools are `Host`; a new tool
	/// that claims `ExecSandbox` without spawning through `ExecHost` fails here.
	#[test]
	fn only_sandbox_spawning_tools_are_exec_sandboxed() {
		let scratch = tempfile::tempdir().expect("scratch");
		let con = Ctx::new();
		let mut tool_settings = ToolSettings::default();
		for name in [
			"ast_edit",
			"ast_grep",
			"bash",
			"browser",
			"computer",
			"debug",
			"edit",
			"eval",
			"forge",
			"ci",
			"glob",
			"grep",
			"image_gen",
			"lsp",
			"read",
			"security_scan",
			"tts",
			"web_search",
			"write",
		] {
			tool_settings.enabled.insert(Str::new_static(name), true);
		}
		// Enabled in settings on purpose. With a cap off the row is absent, so
		// this map cannot reinstall browser, computer, image_gen, or tts.
		#[cfg(feature = "cap-browser")]
		let browser_settings = BrowserSettings { enabled: true, ..BrowserSettings::default() };
		let inputs = EnvironmentDeclarationInputs {
			read_policy:      omp_tools::read::ReadPolicy::default(),
			selected_edit:    omp_tools::edit::hashline_spec().rev,
			eval_description: Some(sf!("Evaluate code.")),
			shell_snapshot:   Some(omp_tools::shell::ShellPromptSnapshot {
				sibling_tools:       Arc::default(),
				platform:            sf!("linux"),
				command_prefix:      false,
				embedded_builtins:   true,
				devices:             true,
				interceptor_enabled: false,
				interceptor_rules:   Arc::default(),
			}),
			memory:           omp_memory::Capabilities {
				writable: true,
				searchable: true,
				resolvable: true,
				editable: true,
				..omp_memory::Capabilities::default()
			},
			managed_skills:   true,
		};
		let mut specs = environment_declarations(
			&tool_settings,
			#[cfg(feature = "cap-browser")]
			&browser_settings,
			&inputs,
			true,
			ToolsPolicy::Auto,
		)
		.into_iter()
		.map(|declaration| declaration.spec)
		.collect::<Vec<_>>();

		let mut session = Registry::new();
		register_session_base(
			&mut session,
			Vec::new(),
			Vec::new(),
			None,
			None,
			None,
			None,
			&BlobHost::open(scratch.path().join("blobs")).expect("blob host"),
			scratch.path(),
			scratch.path(),
			&Arc::new(
				TelemetryIndex::open(
					&scratch.path().join("telemetry"),
					&scratch.path().join("telemetry.sqlite3"),
				)
				.expect("telemetry index"),
			),
			Arc::new(
				ForgeCache::open(
					scratch.path().join("forge-cache.sqlite3"),
					crate::tool_settings::ForgeCacheSettings::from_con(&con).policy(),
				)
				.expect("forge cache"),
			),
			#[cfg(feature = "cap-forge")]
			crate::github_adapter::LiveForgeDeviceClient::default(),
			&tool_settings,
			#[cfg(feature = "cap-media")]
			image_config(&con),
			#[cfg(feature = "cap-media")]
			speech_config(&con),
			ToolsPolicy::Auto,
			&omp_tools::roster::SelectedRoster::production(),
		)
		.expect("session base registers");
		specs.extend(
			session
				.live_identities()
				.map(|(name, _)| session.live_spec(name).expect("live spec").clone()),
		);
		// The kernel's session tools run in the session process.
		specs.extend([
			omp_tools::task::spec(),
			omp_tools::hub::spec(),
			omp_tools::todo::spec(),
			omp_tools::goal::spec(),
		]);

		let names = specs
			.iter()
			.map(|spec| spec.name.as_str())
			.collect::<BTreeSet<_>>();
		for expected in [
			"bash",
			#[cfg(feature = "cap-browser")]
			"browser",
			#[cfg(feature = "cap-computer")]
			"computer",
			#[cfg(feature = "cap-media")]
			"image_gen",
			#[cfg(feature = "cap-media")]
			"tts",
			"eval",
			"py_eval",
			"read",
			"write",
			"web_search",
			#[cfg(feature = "cap-forge")]
			"forge",
			#[cfg(feature = "cap-ci")]
			"ci",
			"task",
			"hub",
			"reflect",
		] {
			assert!(names.contains(expected), "{expected} must be covered");
		}
		#[cfg(not(feature = "cap-browser"))]
		assert!(
			!names.contains("browser"),
			"browser is absent from the construction table when cap-browser is off"
		);
		#[cfg(not(feature = "cap-computer"))]
		assert!(
			!names.contains("computer"),
			"computer is absent from the construction table when cap-computer is off"
		);
		#[cfg(not(feature = "cap-media"))]
		assert!(
			!names.contains("image_gen"),
			"image_gen is absent from the construction table when cap-media is off"
		);
		#[cfg(not(feature = "cap-forge"))]
		assert!(
			!names.contains("forge"),
			"forge is absent from the construction table when cap-forge is off"
		);
		#[cfg(not(feature = "cap-ci"))]
		assert!(!names.contains("ci"), "ci is absent from the construction table when cap-ci is off");
		#[cfg(not(feature = "cap-media"))]
		assert!(
			!names.contains("tts"),
			"tts is absent from the construction table when cap-media is off"
		);
		let sandboxed = specs
			.iter()
			.filter(|spec| spec.confinement.sandboxed())
			.map(|spec| (spec.name.as_str(), spec.rev.n))
			.collect::<BTreeSet<_>>();
		assert_eq!(sandboxed, BTreeSet::from([("bash", 2), ("hub", 2)]));
	}

	#[test]
	fn registry_loci_and_remote_names_follow_authoritative_metadata() {
		let mut registry = Registry::new();
		register_instrumented(
			&mut registry,
			omp_tools::todo::tool(),
			Presentation::Slot,
			omp_tools::roster::claims(omp_tools::roster::ClaimsClass::Core, ToolsPolicy::Auto),
		)
		.expect("session tool");
		environment_registry(
			&mut registry,
			omp_tools::ast_grep::tool(
				PathBuf::from("."),
				production_search_policy(&ToolSettings::default()),
			),
			Presentation::Slot,
			omp_tools::roster::claims(omp_tools::roster::ClaimsClass::Core, ToolsPolicy::Auto),
		)
		.expect("environment tool");
		let remote = omp_tools::write::spec();
		let expected_description = remote.description.clone();
		registry
			.declare_remote(
				remote,
				Presentation::Slot,
				omp_tools::roster::claims(omp_tools::roster::ClaimsClass::Core, ToolsPolicy::Auto),
				ExecutionMode::Parallel,
			)
			.expect("remote environment declaration");

		assert_eq!(registry.locus("write").expect("write locus"), ToolLocus::Environment);
		assert_eq!(registry.locus("ast_grep").expect("ast_grep locus"), ToolLocus::Environment);
		assert_eq!(registry.locus("todo").expect("todo locus"), ToolLocus::Session);
		assert_eq!(registry.presentation("write").expect("write presentation"), Presentation::Slot);
		assert_eq!(
			&registry
				.live_spec("write")
				.expect("remote spec")
				.description,
			&expected_description
		);
		let names = environment_tool_names(&registry);
		assert!(names.contains("write"));
		assert!(names.contains("ast_grep"));
		assert!(!names.contains("todo"));
	}
	fn prelude_param(
		name: &str,
		kind: PreludeParamKind,
		default_json: Option<&'static [u8]>,
	) -> omp_proto::toolhost::v1::PreludeParam {
		omp_proto::toolhost::v1::PreludeParam {
			name:         name.to_owned(),
			kind:         kind as i32,
			default_json: default_json.map(bytes::Bytes::from_static),
			annotation:   None,
			props:        None,
		}
	}

	#[test]
	fn worker_tools_never_expand_the_auto_slot_roster() {
		assert_eq!(
			omp_tools::roster::presentation(
				omp_tools::roster::PresentationClass::LongTail,
				ToolsPolicy::Auto,
			),
			Presentation::Device,
		);
		assert_eq!(
			omp_tools::roster::presentation(
				omp_tools::roster::PresentationClass::LongTail,
				ToolsPolicy::ToolOnly,
			),
			Presentation::Slot,
		);
	}

	#[test]
	fn worker_constraint_preserves_registration_fallback() {
		let declaration = ToolDecl {
			constraint: Some(omp_proto::toolhost::v1::ToolConstraint {
				kind: Some(tool_constraint::Kind::Schema(omp_proto::toolhost::v1::SchemaConstraint {
					priority:       73,
					on_unsupported: omp_proto::inference::v1::Fallback::Error as i32,
				})),
			}),
			..ToolDecl::default()
		};
		assert_eq!(worker_constraint(&declaration).expect("constraint lowers"), Constraint::Schema {
			priority:       73,
			on_unsupported: omp_proto::inference::v1::Fallback::Error,
		});
	}

	#[test]
	fn prelude_parameter_metadata_rejects_invalid_signatures() {
		let cases = [
			(
				vec![
					prelude_param("value", PreludeParamKind::PositionalOrKeyword, None),
					prelude_param("value", PreludeParamKind::KeywordOnly, None),
				],
				"more than once",
			),
			(
				vec![
					prelude_param("mode", PreludeParamKind::KeywordOnly, None),
					prelude_param("value", PreludeParamKind::PositionalOrKeyword, None),
				],
				"after a keyword-only parameter",
			),
			(
				vec![
					prelude_param("optional", PreludeParamKind::PositionalOrKeyword, Some(b"null")),
					prelude_param("required", PreludeParamKind::PositionalOrKeyword, None),
				],
				"after a default",
			),
			(
				vec![prelude_param("not valid", PreludeParamKind::PositionalOrKeyword, None)],
				"invalid name",
			),
			(vec![prelude_param("é", PreludeParamKind::PositionalOrKeyword, None)], "invalid name"),
			(
				vec![prelude_param("class", PreludeParamKind::PositionalOrKeyword, None)],
				"Python keyword",
			),
		];
		for (params, expected) in cases {
			let declaration = ToolDecl { prelude_params: params, ..ToolDecl::default() };
			let error = prelude_params("contract_helper", &declaration)
				.expect_err("invalid prelude signature was accepted");
			assert!(error.to_string().contains(expected), "{error}");
		}
	}
}
