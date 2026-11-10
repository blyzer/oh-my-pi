//! Transport-neutral `env/v1` dispatch and owner-local UDS serving.
//!
//! Shared state stays in this module. Dispatch, control factories, device
//! bridges, diagnostics, the accept loop, connection state, and daemon
//! startup live in sibling modules. Crate paths and privacy match the
//! previous single file.

#[allow(
	unused_imports,
	reason = "shared import set for every server module and for tests via the parent glob; each \
	          name is used by only some of them"
)]
mod prelude {
	//! Shared imports for the `server` modules.
	pub(in crate::server) use std::{
		collections::{BTreeMap, BTreeSet, HashMap},
		fs, future, io, mem,
		ops::ControlFlow,
		path::{Path, PathBuf},
		pin, process, str, sync,
		sync::{
			Arc, Weak,
			atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering},
		},
		time::{Duration, SystemTime, UNIX_EPOCH},
	};

	pub(in crate::server) use bytes::{Bytes, BytesMut};
	pub(in crate::server) use flume::Receiver;
	pub(in crate::server) use futures::StreamExt as _;
	pub(in crate::server) use omp_cache::{
		forge_cache::ForgeCache, telemetry_cache::TelemetryIndex,
	};
	pub(in crate::server) use omp_con::Ctx;
	pub(in crate::server) use omp_contract::{
		ApprovalBook, ApprovalRoute, ApprovalSpec, HookGate, KernelSender, SessionRole, TicketState,
	};
	pub(in crate::server) use omp_core::{Hash32, Str, Ulid, sf};
	pub(in crate::server) use omp_env::{
		EnvClient, InProcessEnvTransport, partition::FramePipe, project_state::DaemonPolicy,
	};
	pub(in crate::server) use omp_exec::{ExecError, ExecEvent};
	pub(in crate::server) use omp_ext::workspace_trust::WorkspaceTrust;
	pub(in crate::server) use omp_journal::blob;
	pub(in crate::server) use omp_proto::{
		blob::v1 as blob_pb,
		document::v1::{self as document_pb, commit_transaction_response, document_target},
		env::v1::{
			self as pb, client_frame, data_event, data_request, data_response, document_result,
			exec_session_op, mcp_op, mcp_result, privileged_mutation_intent, send_input, server_frame,
			stdin_frame, workspace_result,
		},
		inference::v1::{Value, ValueMap, value},
		policy::v1 as policy_pb,
		prost::Message as _,
		thread::v1 as thread_pb,
		ui::v1::UiDispatchResult,
	};
	pub(in crate::server) use omp_tool::{
		Abort, ArgIssue, ArgPath, CallOutcome, CapsBase, Confinement, Effects, ErasedEv,
		ErasedOutcome, IncomingParams, Interrupt, InvocationPins, ModelClass, PromptCaps, Registry,
		RegistryError, ToolIdentity, ToolRoute, ToolTerminal,
	};
	pub(in crate::server) use omp_tools::{
		ask::PresenterSlot,
		device::{DeviceInvokeRequest, DeviceInvoker},
		eval::EvalSessionControl,
		read,
		read::{
			resolver::{self as resolver, ResolverTable, ResourceCapability, ResourceCompletion},
			selector::{self, ParsedUri},
		},
		staging::StagedProposalRegistry,
	};
	pub(in crate::server) use omp_walker::{
		CompiledWalkGlob, DirectoryErrorMode, FileType, FollowLinks, WalkDetail, WalkFilter,
		WalkOptions, WalkOrder, WalkRequest, WalkStatus,
	};
	pub(in crate::server) use parking_lot::Mutex;
	pub(in crate::server) use thiserror::Error;
	#[cfg(any(unix, windows))]
	pub(in crate::server) use tokio::sync::watch;
	pub(in crate::server) use tokio::{
		io::{AsyncRead, AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _, duplex, split},
		task::{self, JoinError, JoinHandle},
		time::{self, Instant, Sleep},
	};
	#[cfg(unix)]
	pub(in crate::server) use tokio::{
		net::{UnixListener, UnixStream},
		signal::{
			ctrl_c,
			unix::{SignalKind, signal},
		},
		task::JoinSet,
	};
	pub(in crate::server) use tokio_util::sync::CancellationToken;
	pub(in crate::server) use url::Url;

	#[cfg(feature = "cap-browser")]
	pub(in crate::server) use crate::browser_daemon::BrowserSettings;
	#[cfg(any(unix, windows))]
	pub(in crate::server) use crate::docserver::connection::ConnectionConfig;
	#[cfg(unix)]
	pub(in crate::server) use crate::docserver::daemon;
	#[cfg(windows)]
	pub(in crate::server) use crate::docserver::windows::OwnerPipeListener;
	pub(in crate::server) use crate::{
		EnvdConfig, RegistryBridges,
		admission::{
			AdmissionDecision, AdmissionGate, ApprovalPolicy, effects_narrow_or_refuse,
			widened_effects_denial,
		},
		approval_relay::{ConnectionApprovals, OwnedApprovals},
		authenticated_runtime_identity,
		blobs::{BlobError, BlobHost, BlobId, BlobRead},
		docs::{
			AcpDocumentBackend, DapRegistryEvent, DocumentError, DocumentEvents, DocumentHello,
			DocumentHost, DocumentLease, EditorCapabilities, EditorIoError, LspEvents,
			LspRegistryEvent, ServedWorkspaceTrust, WriteBack, WriteBackOutcome,
		},
		editor_base::{EditorRoute, EditorSession},
		eval::{
			BridgeHostError, ParentBindingLease, ParentSessionHost, PreludeInvoker, SessionBridgeHost,
		},
		exec::{ExecHost, ExecRun, ProcessEvent},
		exec_settings::{SandboxSettings, ShellSettings},
		exthost::{
			ControlAuthority, ControlAuthorityFactory, ControlCompositionError, ControlQuotaRuntime,
			EnvdControlAuthorities, ExtensionManifest, ExternalControlAuthorities,
			HostControlAuthorityFactory, PersistenceControlAuthorities, PolicyControlAuthorities,
			PresentationControlAuthorities, ProviderControlAuthorities, RegistryAvailabilitySink,
			RegistryControlAuthorities,
			control::{
				CompositeControlAuthority, ControlConnectionIdentity, ControlDispatch, ControlEffect,
				ControlProtocolError, ControlRequestContext, FixedControlAuthorityFactory,
			},
			dispatch::{CallbackDispatcher, CallbackDispatcherSlot, UiCallbackDispatch},
			extensions::SealedRegistryEvidence,
			lifecycle::EscapeCapability,
		},
		fault, fault_in,
		fetch_host::{FetchHostNamer, resolve_locators},
		github_url::GithubCredentialBridge,
		host_info::HostInfoHost,
		host_settings::HostSettings,
		http_egress::{HttpEgressError, HttpEgressHost},
		journal_runtime::DurableScheduleActor,
		lsp_settings::LspSettings,
		mcp::{
			McpConfigPaths, McpService, McpServiceError, ServiceSubscription, SubscriptionEvent,
			control::McpControl,
			manager::{McpManager, ProductionConnector},
			settings::McpSettings,
		},
		memory,
		memory::{ReflectionBridgeHost, RegisteredMemoryRuntime},
		policy::{
			AuthorityTable, DataAuthority, Grants, PolicyError, QuotaAccount, dap_command_capability,
			lsp_notification_tier, lsp_request_tier, lsp_tier_capability,
		},
		presence::{PresenceError, PresenceLease, PresenceRegistry},
		process_store::{ProcessStore, ShutdownAcknowledgement},
		reflection_relay::{ConnectionReflection, OwnedReflection},
		resource_materializer::{MaterializationError, ResourceMaterializer},
		schedules::{DurableScheduleError, ScheduleDeliveryBackend},
		search_backend::SearchBridgeHost,
		site::{SiteError, SiteMaterializer, record_modules},
		tool_document::{PrivilegedMutationFault, privileged_unlink, privileged_write},
		tool_settings::{ApprovalMode, ForgeCacheSettings, ToolSettings},
		tool_url::UrlResolver,
		tools::{
			AgentCheckpointControl, DeviceCatalogObserver, DeviceControlFactory,
			DeviceInvocationAdmission, DynamicDeviceCatalogEntry, HookControlFactory,
			InvocationAcpBackends, InvocationEditRepairContext, RegistryControlFactory,
			SessionRegistryBridges, build_environment_declaration_inputs, production_registry,
			session_registry, with_acp_scope, with_edit_repair_scope, with_invocation_approvals,
			with_invocation_reflection, with_invocation_scope, with_invocation_session_scope,
			with_output_request_scope,
		},
		vcs::{self, RepositoryAvailability},
		worker::{
			AgentsControlAuthorityBinding, DEFAULT_MAX_FRAME_BYTES, DomainControlSlot,
			ExtHostCompletion, ExtHostConfig, ExtHostError, ExtHostEvent, ExtHostInvocation,
			ExtHostOutcomeKind, ExtHostSpec, ExtHostSupervisor, ExtHostToolCall,
			ExternalControlAuthorityBinding, ExternalDomainControlBinding,
			ExternalDomainControlFactories, HostKey, worker_verdict_json,
		},
		worker_pool::{
			DEFAULT_MAX_CONCURRENT_SPAWNS, DEFAULT_WORKER_LAYER_CEILING, WorkerKey, WorkerRoute,
			WorkerSupervisor, WorkerUnavailable,
		},
		workspace::{
			WorkspaceError, WorkspaceHost, WorkspaceOperationError, WorkspaceOperations,
			WorkspaceSearchCase, WorkspaceSearchOptions,
		},
		workspace_roots::WorkspaceRootHost,
		write_scope::{self, WriteScope},
	};
}

#[allow(
	unused_imports,
	reason = "state types in this module and server::tests (`use super::*`) share the prelude; \
	          most names are used by only one of them"
)]
use prelude::*;

mod connection;
mod control_factories;
mod devices;
mod diagnostics;
mod dispatch;
mod error;
mod forward;
mod identity;
mod runtime;
mod session;

pub use diagnostics::AgentControlBinding;
#[cfg(all(test, unix))]
use diagnostics::LateDiagnosticsBatcher;
#[cfg(all(test, unix))]
use dispatch::requires_environment_host;
pub use error::EnvdError;
#[cfg(all(test, unix))]
use forward::{
	DEFAULT_RESULT_PROJECTION_BYTES, UNPROJECTED_NATIVE_RESULT, authorize_visibility_receipt,
	materialize_worker_outcome, project_wire_parts, project_wire_parts_with_visibility,
	projected_worker_completion_json, send_abort_verdict, send_native_outcome,
	send_policy_denied_verdict, server_frame, wire_text,
};
#[allow(
	unused_imports,
	reason = "preserves `server::ConnectionPolicy` for windows.rs and server tests"
)]
pub(crate) use identity::ConnectionPolicy;
#[cfg(all(test, unix))]
use identity::{AcceptedHello, approval_mode_from_wire};
pub use identity::{ExtensionDataBinding, ServerIdentity};
pub use runtime::document_user_config_root;
#[cfg(not(windows))]
pub use runtime::run;
#[cfg(unix)]
#[allow(
	unused_imports,
	reason = "preserves the crate path `server::run_with_registry` from the single-file module"
)]
pub use runtime::run_with_registry;
#[cfg(all(test, unix))]
use runtime::{
	DocumentConfigInputs, connect_or_start_docserver, ensure_document_socket_free,
	rehost_document_authority, validate_document_authority, wait_idle,
};
#[cfg(all(test, unix))]
use session::start_memory_runtime;

const MIN_SCHEMA_REV: u32 = 4;
const FRAME_LIMIT: usize = 64 * 1024 * 1024;
const BLOB_CHUNK_BYTES: usize = 64 * 1024;
const DEFAULT_TOOL_DEADLINE: Duration = Duration::from_secs(300);
const PRELUDE_CALL_DEADLINE: Duration = Duration::from_secs(600);
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
const NATIVE_CANCEL_GRACE: Duration = Duration::from_millis(250);
const INVOCATION_RESPONSE_SEND_GRACE: Duration = Duration::from_millis(250);
const MAX_RESOURCE_URI_BYTES: usize = 8 * 1024;
const MAX_RESOURCE_READ_BYTES: usize = 8 * 1024 * 1024;
const MAX_RESOURCE_LIST_BYTES: usize = 2 * 1024 * 1024;
const MAX_RESOURCE_ENTRIES: usize = 4_096;
const MAX_RESOURCE_COMPLETIONS: usize = 100;
static NEXT_AGENT_CONTROL_BINDING: AtomicU64 = AtomicU64::new(1);
fn unknown_tool_message(name: &str) -> &'static str {
	if name == "eval" {
		"eval is disabled; restart the project daemon with --py-eval (omp envd)"
	} else {
		"tool name and revision are not registered; project daemon settings differ; restart omp envd \
		 after changing tool settings"
	}
}

/// Execution facts of one invocation that the environment enforces itself.
#[derive(Clone, Debug, Default)]
struct InvocationExecutionPolicy {
	tool:           Str,
	/// Plan mode, or a read-only subagent of a plan-mode session, derived from
	/// the invocation's typed roster restrictions (`InvokeTool.restrictions`).
	/// The native executor runs inside it, so the environment's writers
	/// refuse every change it does not admit.
	write_scope:    Option<Arc<WriteScope>>,
	core_admission: bool,
}

/// An invocation refused at the environment boundary because its tool could
/// write around the writers its write scope confines.
#[derive(Clone, Debug, Eq, PartialEq, Error)]
enum WriteBoundaryDenied {
	/// Plan mode is active.
	#[error(
		"plan mode is active: the environment refused `{tool}`, which can change files outside the \
		 plan file; no action was taken"
	)]
	Plan {
		/// Refused tool.
		tool: Str,
	},
	/// The caller is a read-only subagent of a plan-mode session.
	#[error(
		"this agent is a read-only subagent of a plan-mode session: the environment refused \
		 `{tool}`, which can change files; no action was taken"
	)]
	ReadOnly {
		/// Refused tool.
		tool: Str,
	},
}

/// An invocation refused once its committed arguments are judged, before its
/// executor sees them.
#[derive(Debug, Error)]
enum InvocationRefused {
	/// The call could write around the writers its write scope confines.
	#[error(transparent)]
	WriteBoundary {
		/// The boundary's refusal.
		source: WriteBoundaryDenied,
	},
	/// The tool judged the call's effects to exceed its declared maximum.
	#[error(transparent)]
	Effects {
		/// The registry's refusal.
		source: RegistryError,
	},
}

impl InvocationExecutionPolicy {
	fn from_request(
		request: &pb::InvokeTool,
		restrictions: Option<&omp_tool::ToolRestrictions>,
		workspace_root: &Path,
		sessions_dir: &Path,
		principal: Option<&str>,
	) -> Self {
		let core_admission = request
			.props
			.as_ref()
			.and_then(|props| props.fields.get("omp/core-admission"))
			.and_then(|value| value.kind.as_ref())
			.is_some_and(|kind| matches!(kind, value::Kind::Bool(true)));
		Self {
			tool: Str::from(request.name.as_str()),
			write_scope: restrictions
				.and_then(|restrictions| {
					WriteScope::for_invocation(restrictions, workspace_root, sessions_dir, principal)
				})
				.map(Arc::new),
			core_admission,
		}
	}

	/// Refuses, before execution, a call that could write around the
	/// environment's scoped writers while a write scope applies: its tool runs
	/// processes under the exec sandbox ([`Confinement::ExecSandbox`], whose
	/// declaration leaves those effects out), or the call's envelope (its
	/// argument-scoped effects, else the declared maximum) runs commands,
	/// spawns subagents, or writes documents without
	/// [`Confinement::DocumentAuthority`]. Network-only and read-only calls
	/// pass, and so do document-authority tools (`write`, `edit`, `lsp`),
	/// whose writes the scope confines one by one.
	fn denial(&self, effects: &Effects, confinement: Confinement) -> Option<WriteBoundaryDenied> {
		let scope = self.write_scope.as_deref()?;
		let writes_around = confinement.sandboxed()
			|| effects
				.exec
				.as_ref()
				.is_some_and(|exec| !exec.commands.is_empty())
			|| effects.subagents != 0
			|| (effects
				.documents
				.as_ref()
				.is_some_and(|documents| !documents.write_globs.is_empty())
				&& confinement != Confinement::DocumentAuthority);
		writes_around.then(|| match scope {
			WriteScope::PlanFile { .. } => WriteBoundaryDenied::Plan { tool: self.tool.clone() },
			WriteScope::ReadOnly => WriteBoundaryDenied::ReadOnly { tool: self.tool.clone() },
		})
	}
}

struct DocumentAuthority {
	shutdown: CancellationToken,
	#[cfg(unix)]
	task:     Option<JoinHandle<daemon::Result>>,
	#[cfg(windows)]
	task:     Option<JoinHandle<Result<(), crate::docserver::windows::WindowsTransportError>>>,
}

/// Owner-local `env/v1` dispatch state serving one project environment.
struct EnvironmentAuthorities {
	documents:           DocumentHost,
	_document_authority: Option<DocumentAuthority>,
	workspace_ops:       WorkspaceOperations,
	lsp_settings:        LspSettings,
	process_store:       ProcessStore,
}

/// Owner-local `env/v1` dispatch state serving one project environment.
pub struct EnvServer {
	identity:                ServerIdentity,
	environment:             Option<EnvironmentAuthorities>,
	tool_settings:           ToolSettings,
	exec:                    ExecHost,
	approvals:               ApprovalAuthoritySlot,
	http_egress:             HttpEgressHost,
	workspace:               WorkspaceHost,
	mcp:                     Arc<McpService>,
	mcp_manager:             Arc<McpManager>,
	host_info:               HostInfoHost,
	workspace_roots:         WorkspaceRootHost,
	resources:               Arc<ResolverTable<UrlResolver>>,
	_memory_runtime:         RegisteredMemoryRuntime,
	blobs:                   BlobHost,
	sites:                   SiteMaterializer,
	materializations:        ResourceMaterializer,
	registry:                Arc<Registry>,
	ask_presenter:           PresenterSlot,
	ext_hosts:               Arc<ExtHostSupervisor>,
	eval_bridge:             Arc<SessionBridgeHost>,
	reflection_bridge:       Arc<ReflectionBridgeHost>,
	eval_control:            EvalSessionControl,
	search_bridge:           Arc<SearchBridgeHost>,
	github_credentials:      Arc<GithubCredentialBridge>,
	usage_fetchers:          omp_ai::operation::usage::UsageFetcherRegistry,
	provider_response_hooks: omp_ai::ProviderResponseHooks,
	admission_gate:          Arc<HookGate>,
	checkpoint_control:      AgentCheckpointControl,
	previews:                StagedProposalRegistry,
	schedules:               DurableScheduleActor,
	workers:                 Arc<WorkerSupervisor>,
	authority:               Arc<AuthorityTable>,
	repository_revision:     AtomicU64,
	presence:                PresenceRegistry,
	state_dir:               PathBuf,
}

#[derive(Clone)]
struct ApprovalAuthority {
	route: ApprovalRoute,
}

#[derive(Clone, Default)]
struct ApprovalAuthoritySlot(Arc<parking_lot::RwLock<Option<ApprovalAuthority>>>);

const EDIT_REPAIR_TIMEOUT: Duration = Duration::from_secs(60);

struct PendingEditRepair {
	generation: u64,
	reply:      flume::Sender<Result<Str, omp_tools::edit::observer::EditRepairError>>,
}

#[derive(Clone)]
struct ConnectionEditRepairRoute {
	request_id:    u64,
	invocation_id: Str,
	responses:     flume::Sender<pb::ServerFrame>,
	next_query:    Arc<AtomicU64>,
	pending:       Arc<Mutex<Option<PendingEditRepair>>>,
}

const ACP_MAX_PENDING: usize = 64;

/// Editor requests one write-back may wait behind and make: the write-back
/// already running for the path, then its own pre-write re-read, write and
/// read-back, each bounded by the editor's deadline.
const WRITE_BACK_REQUESTS: u32 = 6;

type AcpDocumentReply = flume::Sender<Result<pb::acp_document_answer::Body, EditorIoError>>;

struct ConnectionAcpDocumentRoute {
	request_id:    u64,
	invocation_id: Str,
	responses:     flume::Sender<pb::ServerFrame>,
	next_query:    Arc<AtomicU64>,
	pending:       Arc<Mutex<HashMap<u64, AcpDocumentReply>>>,
	/// The binding's editor deadline (`AcpBind.fs_timeout_ms`).
	deadline:      Duration,
	/// The binding's editor capabilities (`AcpBind.read_text`/`write_text`).
	capabilities:  EditorCapabilities,
}

struct InvocationAcpRoutes {
	documents: Option<(Arc<ConnectionAcpDocumentRoute>, Arc<EditorSession>)>,
}

struct ConnectionState {
	owner:            Str,
	requests:         HashMap<u64, RequestState>,
	invocation_ids:   HashMap<Str, u64>,
	tool_settings:    ToolSettings,
	exec_host:        ExecHost,
	document_leases:  HashMap<Bytes, DocumentLease>,
	grants:           Grants,
	capabilities:     BTreeSet<Str>,
	hello_props:      Option<ValueMap>,
	/// The editor binding this connection serves document queries for; a
	/// fresh binding per `AcpBind` so anchors never outlive a session switch.
	acp_documents:    Option<Arc<EditorSession>>,
	/// Relays the approval prompts of the commands this connection issues;
	/// present only for an application connection to an environment host
	/// that advertised the capability.
	approvals:        Option<ConnectionApprovals>,
	/// Relays the memory reflections of the `reflect` calls this connection
	/// issues, directly or through its commands' `dyn`; present under the
	/// same conditions for its own capability.
	reflection:       Option<ConnectionReflection>,
	host:             Option<HostKey>,
	authority:        Arc<AuthorityTable>,
	connection_owner: u64,
	quotas:           QuotaAccount,
	presence:         Option<PresenceLease>,
}

enum RequestState {
	Invocation(InvocationState),
	InvocationFinishing,
	Exec { exec: Bytes, cancel: CancellationToken },
	ProcessAttach { cancel: CancellationToken },
	BlobPut(BlobUpload),
	BlobGet { cancel: CancellationToken },
	DataStream { cancel: CancellationToken },
	DocumentEvents { lease_id: Bytes, cancel: CancellationToken },
	LspEvents { cancel: CancellationToken },
}

/// One open tool invocation. `effects` is the envelope it is admitted under:
/// the declared maximum until its arguments commit, then the call's
/// argument-scoped effects ([`ConnectionState::scope_committed`]). A native
/// call of a tool judged by its arguments is judged and run inside `pins`, so
/// what its judgment resolved from live state is what its executor reaches.
enum InvocationState {
	Native {
		id:             Str,
		feed:           omp_tool::InvocationFeed,
		lifecycle:      Arc<NativeLifecycle>,
		admission:      AdmissionGate,
		pending_commit: Option<pb::ArgsCommitted>,
		effects:        Effects,
		pins:           Option<Arc<InvocationPins>>,
		confinement:    Confinement,
		execution:      InvocationExecutionPolicy,
		request_scope:  Option<bool>,
		edit_repair:    Option<ConnectionEditRepairRoute>,
		acp:            InvocationAcpRoutes,
		delivery:       VerdictDelivery,
		cancel:         CancellationToken,
	},
	Worker {
		id:             Str,
		owner:          HostKey,
		invocation:     Option<ExtHostInvocation>,
		committed:      bool,
		admission:      AdmissionGate,
		pending_commit: Option<pb::ArgsCommitted>,
		effects:        Effects,
		confinement:    Confinement,
		execution:      InvocationExecutionPolicy,
		request_scope:  Option<bool>,
		delivery:       VerdictDelivery,
		interrupt:      flume::Sender<pb::Interrupt>,
		interrupts:     Option<Receiver<pb::Interrupt>>,
		cancel:         CancellationToken,
	},
}

const NATIVE_COMMITTED: u8 = 1;
const NATIVE_TERMINAL: u8 = 2;

#[derive(Default)]
struct NativeLifecycle {
	state: AtomicU8,
}

enum NativeCommitError {
	AlreadyCommitted,
	Terminal,
}

#[derive(Default)]
struct BlobUpload {
	data:          BytesMut,
	expected_hash: Option<Bytes>,
	expected_size: Option<u64>,
	chunks:        usize,
}

struct Finished {
	request_id:    u64,
	invocation_id: Option<Str>,
}

enum LoopEvent {
	/// Boxes the foreign generated protobuf frame to keep this local event enum
	/// compact.
	Frame(Box<pb::ClientFrame>),
	Finished(Finished),
	/// The env-owned deadline of at least one pending admission elapsed.
	AdmissionDeadline,
}

/// What a terminal verdict needs to honor the `Verdict` contract: the CAS
/// that holds the whole serialized `CallOutcome`, the session whose delivery
/// lease retains it, and the projection the client asked for.
#[derive(Clone, Debug)]
struct VerdictDelivery {
	blobs:             BlobHost,
	retention_session: Option<Str>,
	output_request:    omp_tool::OutputRequest,
}

#[cfg(all(test, unix))]
mod tests;
