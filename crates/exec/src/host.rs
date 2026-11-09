//! `ExecHost` contract (ADR 0045 ENVD-16, ADR 0049 D3.1).
//!
//! These are the types and traits `omp-exec-local` needs from the environment
//! process authority. envd keeps the session host and implements [`ExecHost`];
//! this module performs no I/O.

use std::{future::Future, io, mem::size_of, time::Duration};

use bytes::Bytes;
use omp_core::Str;
use omp_proto::{
	bounds::FrameBoundsError,
	env::v1::{
		CloseSessionResponse, ExecRequest, ExecStarted, ExitEvent, OpenSessionRequest,
		OpenSessionResponse, OutputFrame,
	},
};
use thiserror::Error;

/// Process identity inspection failure.
#[derive(Debug, Error)]
pub enum IdentityError {
	/// PID was invalid or no longer exists.
	#[error("process {pid} does not exist")]
	NotFound {
		/// Missing PID.
		pid: u32,
	},
	/// Operating-system identity query failed.
	#[error("failed to inspect process {pid}")]
	Inspect {
		/// Queried PID.
		pid:    u32,
		/// Platform I/O failure.
		#[source]
		source: io::Error,
	},
	/// The operating system returned an incomplete identity record.
	#[error("process {pid} returned incomplete identity metadata")]
	Incomplete {
		/// Queried PID.
		pid: u32,
	},
}

/// Atomic process metadata failure.
#[derive(Debug, Error)]
pub enum StoreError {
	/// Filesystem operation failed.
	#[error("process metadata filesystem operation failed")]
	Io(#[source] io::Error),
	/// Snapshot serialization failed.
	#[error("process metadata serialization failed")]
	Encode(#[source] serde_json::Error),
	/// Snapshot decoding failed.
	#[error("process metadata decoding failed")]
	Decode(#[source] serde_json::Error),
	/// Current daemon identity could not be captured for a new store.
	#[error("process metadata owner identity could not be established")]
	Identity(#[source] IdentityError),
	/// On-disk schema is not supported by this binary.
	#[error("process metadata version {actual} is unsupported; expected {supported}")]
	UnsupportedVersion {
		/// On-disk version.
		actual:    u32,
		/// Current version.
		supported: u32,
	},
}

/// Exclusive durable-owner lease failure.
#[derive(Debug, Error)]
pub enum LeaseError {
	/// Another live envd holds the operating-system lease.
	#[error("another environment daemon owns the durable process lease")]
	AlreadyOwned,
	/// Lease filesystem operation failed.
	#[error("durable process lease filesystem operation failed")]
	Io(#[source] io::Error),
	/// Current process identity could not be established.
	#[error("durable process owner identity could not be established")]
	Identity(#[from] IdentityError),
	/// Lease identity serialization failed.
	#[error("durable process owner identity serialization failed")]
	Encode(#[source] serde_json::Error),
}

/// Errors returned by the environment execution host.
#[derive(Debug, Error)]
pub enum ExecError {
	/// The requested session does not exist.
	#[error("exec session was not found")]
	SessionNotFound,
	/// The requested execution does not exist or has already finished.
	#[error("execution was not found")]
	RunNotFound,
	/// Final working-directory metadata is not retained for this execution.
	#[error("execution final working directory was not found")]
	FinalCwdNotFound,
	/// Final working-directory metadata did not match the caller's fence.
	#[error("execution final working-directory revision is stale")]
	StaleFinalCwdRevision,
	/// An exec-session request used an incompatible schema revision.
	#[error("exec-session wire revision does not match the Environment schema")]
	WireRevision,
	/// A requested shell profile is not advertised by this backend.
	#[error("shell profile {profile:?} is not supported by this backend")]
	UnsupportedShellProfile {
		/// Requested profile name.
		profile: Str,
	},
	/// The control operation was outside the declared vocabulary.
	#[error("exec control kind is invalid")]
	InvalidControl,
	/// A command-local environment delta used an invalid wire value.
	#[error("command-local environment delta is invalid")]
	InvalidRunEnvironment,
	/// The requested named process does not exist.
	#[error("named process {name:?} was not found")]
	ProcessNotFound {
		/// The missing process name.
		name: Str,
	},
	/// A process with this name is already registered.
	#[error("named process {name:?} already exists")]
	ProcessExists {
		/// The already-registered process name.
		name: Str,
	},
	/// A named-process request targeted a retired generation.
	#[error(
		"named process {name:?} generation {requested} is stale; current generation is {current}"
	)]
	StaleProcessGeneration {
		/// Stable process name.
		name:      Str,
		/// Generation carried by the request.
		requested: u64,
		/// Current retained generation.
		current:   u64,
	},
	/// A request contained an unsupported signal name.
	#[error("unsupported signal {signal:?}")]
	UnsupportedSignal {
		/// The unsupported signal name.
		signal: Str,
	},
	/// A URI could not be used as a local working directory.
	#[error("invalid working-directory URI {uri:?}")]
	InvalidCwd {
		/// The unusable working-directory URI.
		uri: Str,
	},
	/// The shell engine rejected the requested operation.
	#[error("shell execution failed: {0}")]
	Shell(Str),
	/// A named-process readiness probe was invalid or did not pass.
	#[error("process readiness failed: {0}")]
	Readiness(Str),
	/// A readiness-probe reply frame failed the protobuf preflight, so it was
	/// not decoded.
	#[error("readiness probe reply frame rejected before decoding")]
	ProbeFrame {
		/// Preflight failure.
		#[source]
		source: FrameBoundsError,
	},
	/// A readiness-probe reply frame passed the preflight but is not a valid
	/// `WorkerFrame`.
	#[error("readiness probe reply frame is not a valid worker frame")]
	ProbeDecode {
		/// Protobuf decode failure.
		#[source]
		source: prost::DecodeError,
	},
	/// An operating-system process primitive failed.
	#[error("process I/O failed: {0}")]
	Io(#[from] io::Error),
	/// The target actor has stopped.
	#[error("exec session has closed")]
	SessionClosed,
	/// Shell output could not enter the content-addressed artifact store.
	#[error(transparent)]
	OutputStore(#[from] omp_journal::blob::Error),
	/// A named process used an invalid durable name.
	#[error("process name must be 1-48 ASCII letters, digits, dot, underscore, or hyphen")]
	InvalidProcessName,
	/// Detached processes cannot retain a pseudo-terminal.
	#[error("detached processes cannot use a PTY")]
	DetachedPty,
	/// Durable process metadata could not be committed.
	#[error(transparent)]
	ProcessStore(#[from] StoreError),
	/// A durable operating-system identity could not be captured.
	#[error(transparent)]
	ProcessIdentity(#[from] IdentityError),
	/// Another daemon already owns the durable process namespace.
	#[error(transparent)]
	ProcessLease(#[from] LeaseError),
	/// The configured command sandbox could not be compiled.
	#[error("failed to enable sandbox mode {mode}")]
	Sandbox {
		/// Requested user-facing posture.
		mode:   &'static str,
		/// Typed sandbox compiler failure.
		#[source]
		source: omp_sandbox::SandboxError,
	},
}

/// One ordered event emitted by an execution.
#[derive(Clone, Debug)]
pub enum ExecEvent {
	/// The session dequeued this command and entered execution.
	Started {
		/// Opaque execution identifier assigned when the command was queued.
		exec_id: Bytes,
	},
	/// Bytes written by stdout, stderr, or the PTY.
	Output(OutputFrame),
	/// The terminal execution status. No output follows it.
	///
	/// `ExitEvent` is a generated prost message. Boxing that one field keeps
	/// this enum the size of an output frame.
	Exit(Box<ExitEvent>),
}

const _: () = assert!(size_of::<ExecEvent>() <= 128, "ExecEvent must stay compact");

/// RAII ownership of one command invocation.
///
/// The environment host owns the process groups. Dropping the concrete run
/// requests teardown unless the host retained that exact run as a named
/// process.
pub trait ExecRun: Send + Sync {
	/// Returns the opaque wire execution identifier.
	fn id(&self) -> &[u8];

	/// Waits for the next output or terminal event.
	fn next_event(&self) -> impl Future<Output = Option<ExecEvent>> + Send;

	/// Requests cancellation without dropping the event stream.
	fn cancel(&self);
}

/// Process authority a local execution backend drives.
///
/// envd's session host implements this. The trait is the contract;
/// session state, sandbox, and kill behavior stay in the implementor.
pub trait ExecHost: Clone + Send + Sync + 'static {
	/// One live command owned by this host.
	type Run: ExecRun;

	/// Opens a persistent shell carrying its own cwd and environment state.
	fn open_session(
		&self,
		request: OpenSessionRequest,
	) -> impl Future<Output = Result<OpenSessionResponse, ExecError>> + Send;

	/// Starts a script in a session. A session serializes its scripts.
	fn exec(
		&self,
		request: ExecRequest,
		timeout: Option<Duration>,
	) -> impl Future<Output = Result<(ExecStarted, Self::Run), ExecError>> + Send;

	/// Closes a session and the shell-owned background jobs it still holds.
	fn close_session(&self, session: &[u8]) -> Result<CloseSessionResponse, ExecError>;
}
