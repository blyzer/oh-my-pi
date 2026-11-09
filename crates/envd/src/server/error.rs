//! Environment-daemon assembly and serving failures.

use super::prelude::*;

/// Environment-daemon assembly or serving failure.
#[derive(Debug, Error)]
pub enum EnvdError {
	/// A local filesystem, socket, or child-process operation failed.
	#[error(transparent)]
	Io(#[from] io::Error),
	/// The document authority could not be connected or verified.
	#[error("document authority failed: {0}")]
	Document(Str),
	/// The canonical workspace could not be opened.
	#[error("workspace host failed: {0}")]
	Workspace(Str),
	/// The content-addressed blob store could not be opened.
	#[error("blob host failed: {0}")]
	Blob(Str),
	/// The scoped exec materialization store could not be opened.
	#[error(transparent)]
	Materialization(#[from] MaterializationError),
	/// Durable named-process supervision could not be initialized.
	#[error(transparent)]
	Exec(#[from] ExecError),
	/// The non-session durable state authority could not be opened.
	#[error("state authority failed: {0}")]
	State(Str),
	/// The owner's `~/.o2` configuration root could not be resolved.
	#[error("user configuration root could not be resolved")]
	ConfigRoot(#[from] omp_core::dirs::DataDirError),
	/// The embedded Python runtime used by `eval` could not be initialized.
	#[error("eval runtime failed: {0}")]
	Eval(Str),
	/// A Python extension host could not be started or supervised.
	#[error(transparent)]
	ExtensionHost(#[from] ExtHostError),
	/// The durable schedule authority failed or was generation-fenced.
	#[error(transparent)]
	Schedule(#[from] DurableScheduleError),
	/// A native or worker tool declaration could not be registered.
	#[error(transparent)]
	Registry(#[from] RegistryError),
	/// An admitted extension setting could not be installed as a convar.
	#[error(transparent)]
	ExtensionConvar(#[from] crate::exthost::ExtensionConvarError),
	/// A worker advertised a declaration that cannot have a stable registry
	/// identity.
	#[error("invalid worker tool declaration: {0}")]
	WorkerDeclaration(Str),
	/// A worker advertised a malformed or incompatible protocol argument schema.
	#[error("invalid worker tool declaration schema")]
	WorkerProtocolSchema(#[from] omp_tool::ProtocolSchemaError),
	/// The selected edit dialect was not a registered built-in revision.
	#[error("invalid edit dialect: {0}")]
	EditDialect(Str),
	/// Production assembly encountered a second live declaration for one name.
	#[error("duplicate production tool name: {name}")]
	DuplicateToolName {
		/// The twice-declared tool name.
		name: Str,
	},
	/// The environment client could not complete its protocol handshake.
	#[error(transparent)]
	Client(#[from] omp_env::ClientError),
	/// A session host was opened before its owner completed the hello exchange.
	#[error("session host owner has not completed the environment hello handshake")]
	MissingOwnerHello,
	/// Daemon-owned client presence could not be updated.
	#[error(transparent)]
	Presence(#[from] PresenceError),
	/// A spawned environment connection task failed.
	#[error("environment connection task failed: {0}")]
	Task(#[from] JoinError),
	/// The embedded document authority exited before accepting a verified hello.
	#[error("embedded document authority exited before its hello handshake")]
	DocserverExited,
	/// Another live process still holds this project's document authority.
	///
	/// The holder is reachable only through its listening socket, which
	/// carries no owner identity, so the message names the recovery action
	/// instead of an identifier this process cannot observe.
	#[error(
		"project document authority for {path:?} is already held by another live omp process; close \
		 that session, or run this command from a different project root"
	)]
	DocumentAuthorityHeldBy {
		/// Canonical project path whose authority is held.
		path: PathBuf,
	},
	/// The project's document authority, which another live session started,
	/// loads the project's LSP and DAP configuration under a different
	/// workspace trust decision than this session's, so this session must not
	/// share its language- and debug-server roster.
	#[error(
		"another omp session holds the document authority for {path:?} under workspace trust \
		 `{served}`, not this session's `{expected}`; close that session or wait for it to idle out"
	)]
	DocumentAuthorityTrustMismatch {
		/// Canonical project path whose authority is held.
		path:     PathBuf,
		/// The workspace trust this session decided.
		expected: WorkspaceTrust,
		/// The workspace trust the running authority reported.
		served:   ServedWorkspaceTrust,
	},
	/// The project daemon on the environment socket enforces a different
	/// sandbox and approval policy than the client resolved, so the client
	/// must not run its tools there.
	#[error(
		"the project daemon at {socket:?} enforces sandbox and approval policy {served}, not this \
		 session's {expected}"
	)]
	DaemonPolicyMismatch {
		/// Environment socket the daemon answered on.
		socket:   PathBuf,
		/// Policy the client resolved from its own control context.
		expected: DaemonPolicy,
		/// Policy the daemon reported in its hello.
		served:   DaemonPolicy,
	},
	/// The project daemon on the environment socket reported no sandbox and
	/// approval policy, so the client cannot tell what it enforces.
	#[error("the project daemon at {socket:?} reports no sandbox and approval policy")]
	DaemonPolicyUnreported {
		/// Environment socket the daemon answered on.
		socket: PathBuf,
	},
	/// A project daemon spawned now would resolve another sandbox and approval
	/// policy from the configuration files than the client's own, which holds
	/// in-process settings the daemon never sees, so none was spawned.
	#[error(
		"a project daemon spawned for this session would enforce sandbox and approval policy \
		 {spawnable} from the configuration files, not this session's {expected}, which also holds \
		 settings made in this process (such as `--add-dir` roots, a cfg script or an agent class)"
	)]
	DaemonPolicyNotSpawnable {
		/// Policy the client resolved from its own control context.
		expected:  DaemonPolicy,
		/// Policy `omp envd` resolves from the configuration files alone.
		spawnable: DaemonPolicy,
	},
}

impl EnvdError {
	/// Creates a document-authority error, rendering the cause once at this
	/// model-facing fault boundary.
	pub(crate) fn document(cause: impl std::fmt::Display) -> Self {
		Self::Document(sf!("{cause}"))
	}

	/// Creates a state-authority error, rendering the cause once at this
	/// model-facing fault boundary.
	pub(crate) fn state(cause: impl std::fmt::Display) -> Self {
		Self::State(sf!("{cause}"))
	}

	/// Creates a blob-host error, rendering the cause once at this
	/// model-facing fault boundary.
	pub(crate) fn blob(cause: impl std::fmt::Display) -> Self {
		Self::Blob(sf!("{cause}"))
	}
}

impl From<DocumentError> for EnvdError {
	fn from(error: DocumentError) -> Self {
		Self::Document(Str::from(error.to_string()))
	}
}

impl From<WorkspaceError> for EnvdError {
	fn from(error: WorkspaceError) -> Self {
		Self::Workspace(Str::from(error.to_string()))
	}
}

impl From<BlobError> for EnvdError {
	fn from(error: BlobError) -> Self {
		Self::Blob(Str::from(error.to_string()))
	}
}

impl From<WorkspaceOperationError> for EnvdError {
	fn from(error: WorkspaceOperationError) -> Self {
		Self::Workspace(Str::from(error.to_string()))
	}
}
