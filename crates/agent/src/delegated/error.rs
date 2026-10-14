//! Typed failures of the delegated-executor contract.
//!
//! Every variant names the executor and the identifying facts as fields;
//! causes ride `#[source]`. Nothing here carries a string payload.

use std::{io, path::PathBuf, time::Duration};

use omp_core::Hash32;
use thiserror::Error;

use super::{
	CliSessionId, CliVersion, DelegatedPolicy, ExecutionMode, ExecutorId, Feature, RemoteFailure,
	TurnId, VersionRange,
};

/// How a delegated process ended when it ended on its own.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExitFact {
	/// The process exited with a status code.
	Code(i32),
	/// A signal terminated the process.
	Signal(i32),
	/// The process vanished without a recoverable status.
	Unknown,
}

/// Why one frame of the CLI's output could not be decoded.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum FrameFault {
	/// The stream ended inside a frame.
	#[error("frame truncated")]
	Truncated,
	/// The frame is not valid JSON.
	#[error("frame is not valid JSON")]
	NotJson,
	/// The frame names a message this contract has no meaning for.
	#[error("frame names an unknown message")]
	UnknownMessage,
	/// A required field is absent or has the wrong type.
	#[error("frame is missing required field `{field}`")]
	MissingField {
		/// Name of the field in the CLI's protocol.
		field: &'static str,
	},
	/// The frame exceeds the size the host is willing to buffer.
	#[error("frame exceeds {limit} bytes")]
	Oversize {
		/// The enforced limit in bytes.
		limit: usize,
	},
}

/// A well-formed stream that breaks the turn protocol.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum ProtocolViolation {
	/// An event belongs to a turn other than the one in flight.
	#[error("event belongs to turn {found}, but turn {expected} is in flight")]
	TurnMismatch {
		/// The turn the host started.
		expected: TurnId,
		/// The turn the event named.
		found:    TurnId,
	},
	/// The CLI ended the session while a turn was in flight.
	#[error("session closed while a turn was in flight")]
	SessionClosedMidTurn,
	/// The CLI claims a resumed session although none was requested.
	#[error("session reported as resumed although no resume was requested")]
	UnrequestedResume,
	/// The CLI rejected a request the host had every reason to expect to
	/// succeed.
	#[error("request rejected by the CLI (code {code})")]
	RequestRejected {
		/// The CLI's error code.
		code: i64,
	},
	/// An answer named a prompt that is not pending.
	#[error("answer names a prompt that is not pending")]
	UnknownPrompt,
}

/// Why a process could not be torn down.
#[derive(Debug, Error)]
pub enum TeardownFault {
	/// Signalling the process group failed.
	#[error("signalling the process group failed")]
	Signal(#[source] io::Error),
	/// The process survived the final kill.
	#[error("process survived the final kill")]
	Unreaped,
}

/// Failure of a delegated executor or of the host driving it.
#[derive(Debug, Error)]
pub enum ExecutorError {
	/// The CLI binary is not installed or not executable.
	#[error("executable for `{executor}` is missing")]
	ExecutableMissing {
		/// The executor.
		executor: ExecutorId,
	},
	/// The installed CLI version is outside the supported range.
	#[error("`{executor}` {found:?} is outside the supported range")]
	UnsupportedVersion {
		/// The executor.
		executor:  ExecutorId,
		/// The version the CLI reported.
		found:     CliVersion,
		/// The range the executor supports.
		supported: VersionRange,
	},
	/// The CLI reports that its own login is missing or expired.
	#[error("`{executor}` requires login with its own CLI")]
	AuthRequired {
		/// The executor.
		executor: ExecutorId,
	},
	/// The CLI cannot say whether it is logged in, and the request did not
	/// opt in to proceeding.
	#[error("`{executor}` auth status is unknown and the request did not allow that")]
	AuthUnknown {
		/// The executor.
		executor: ExecutorId,
	},
	/// The requested execution mode is not the one this executor offers.
	#[error("`{executor}` offers {offered}, not {requested}")]
	ModeUnsupported {
		/// The executor.
		executor:  ExecutorId,
		/// The mode the caller asked for.
		requested: ExecutionMode,
		/// The only mode the executor offers.
		offered:   ExecutionMode,
	},
	/// The executor cannot enforce the requested policy exactly.
	#[error("`{executor}` cannot enforce the requested policy exactly")]
	PolicyUnmappable {
		/// The executor.
		executor:  ExecutorId,
		/// The policy that was requested.
		requested: DelegatedPolicy,
	},
	/// The executor applied a different policy than the one requested.
	#[error("`{executor}` applied a different policy than requested")]
	PolicyDrift {
		/// The executor.
		executor:  ExecutorId,
		/// The policy that was requested.
		requested: DelegatedPolicy,
		/// The policy the executor reported applying.
		applied:   DelegatedPolicy,
	},
	/// A feature the request depends on is not advertised.
	#[error("`{executor}` does not advertise {feature}")]
	FeatureUnavailable {
		/// The executor.
		executor: ExecutorId,
		/// The missing feature.
		feature:  Feature,
	},
	/// The executor emitted an event that needs a feature it never advertised.
	#[error("`{executor}` emitted an event that requires unadvertised {feature}")]
	UndeclaredEvent {
		/// The executor.
		executor: ExecutorId,
		/// The feature the event needs.
		feature:  Feature,
	},
	/// The working directory is not an absolute path.
	#[error("working directory {path:?} is not absolute")]
	WorkdirNotAbsolute {
		/// The rejected path.
		path: PathBuf,
	},
	/// The working directory cannot be handed to the CLI as text.
	#[error("working directory {path:?} is not valid UTF-8")]
	WorkdirNotUtf8 {
		/// The rejected path.
		path: PathBuf,
	},
	/// The executor reports a different working directory than requested.
	#[error("`{executor}` reported working directory {reported:?}, expected {requested:?}")]
	WorkdirMismatch {
		/// The executor.
		executor:  ExecutorId,
		/// The directory the host asked for.
		requested: PathBuf,
		/// The directory the executor reported.
		reported:  PathBuf,
	},
	/// A resume request was answered with a different or fresh session.
	#[error("`{executor}` did not resume session {requested}")]
	ResumeFailed {
		/// The executor.
		executor:  ExecutorId,
		/// The session the host asked to resume.
		requested: CliSessionId,
	},
	/// The CLI output could not be decoded.
	#[error("`{executor}` produced an undecodable frame (#{frame})")]
	MalformedStream {
		/// The executor.
		executor: ExecutorId,
		/// Zero-based index of the offending frame.
		frame:    u64,
		/// What was wrong with it.
		#[source]
		fault:    FrameFault,
	},
	/// The CLI output decoded but broke the turn protocol.
	#[error("`{executor}` broke the turn protocol")]
	Protocol {
		/// The executor.
		executor:  ExecutorId,
		/// What it did.
		#[source]
		violation: ProtocolViolation,
	},
	/// The CLI process died.
	#[error("`{executor}` process ended unexpectedly ({exit:?})")]
	Crashed {
		/// The executor.
		executor: ExecutorId,
		/// How it ended.
		exit:     ExitFact,
	},
	/// The CLI went silent for longer than the idle limit.
	#[error("`{executor}` produced no event for {idle:?}")]
	Hung {
		/// The executor.
		executor: ExecutorId,
		/// The idle limit that elapsed.
		idle:     Duration,
	},
	/// The turn exceeded its total time limit.
	#[error("`{executor}` turn exceeded {limit:?}")]
	TimedOut {
		/// The executor.
		executor: ExecutorId,
		/// The total limit that elapsed.
		limit:    Duration,
	},
	/// The host cancelled the turn.
	#[error("`{executor}` turn cancelled")]
	Cancelled {
		/// The executor.
		executor: ExecutorId,
	},
	/// The CLI reported that the turn failed.
	#[error("`{executor}` turn {turn} failed: {failure}")]
	Remote {
		/// The executor.
		executor: ExecutorId,
		/// The failed turn.
		turn:     TurnId,
		/// The typed reason.
		failure:  RemoteFailure,
	},
	/// The backend delivered different prompt bytes than it was given.
	#[error("`{executor}` forwarded a different prompt than it was given")]
	PromptRewritten {
		/// The executor.
		executor:  ExecutorId,
		/// Digest of the prompt the host supplied.
		supplied:  Hash32,
		/// Digest of what the backend reports having delivered.
		forwarded: Hash32,
	},
	/// The session was already closed.
	#[error("`{executor}` session is closed")]
	SessionClosed {
		/// The executor.
		executor: ExecutorId,
	},
	/// The process could not be torn down. This is the most serious outcome
	/// because it can leak a child.
	#[error("`{executor}` process could not be torn down")]
	TeardownFailed {
		/// The executor.
		executor: ExecutorId,
		/// Why teardown failed.
		#[source]
		fault:    TeardownFault,
	},
}

const _: () = assert!(size_of::<ExecutorError>() <= 128, "ExecutorError must stay compact");

impl ExecutorError {
	/// Whether the failure belongs to the turn rather than to the process or
	/// the contract.
	///
	/// This is a classification, not a promise that the session survives: a
	/// timed-out or hung process is turn-local but is still torn down. Use
	/// [`ActiveSession::is_open`](super::ActiveSession::is_open) to learn
	/// whether another turn can run. A crash, a malformed stream or a contract
	/// violation is never turn-local.
	#[must_use]
	pub const fn is_turn_local(&self) -> bool {
		matches!(
			self,
			Self::Remote { .. } | Self::Cancelled { .. } | Self::TimedOut { .. } | Self::Hung { .. }
		)
	}
}
