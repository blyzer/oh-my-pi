//! The normalized event model of a delegated agent.
//!
//! A delegated agent is not a model: it runs its own loop, so the stream is a
//! sequence of *agent activity* — text, command and tool activity, permission
//! prompts, usage, lifecycle and failure — never a flat token stream.
//!
//! Tool events are **observation only**. They report what the delegated CLI
//! did on its own authority; nothing in this vocabulary authorizes OMP to run
//! anything (contrast `omp_ai::ChatEvent::ToolCallReady`, which does).

use omp_ai::Usage;
use omp_core::Str;

use super::{CallId, CliSessionId, Feature, PermissionRequest, TurnId};

/// How a turn ended when the CLI ended it normally.
#[derive(
	Clone, Copy, Debug, Eq, Hash, PartialEq, strum::Display, strum::IntoStaticStr,
)]
#[strum(serialize_all = "snake_case")]
pub enum TurnEnd {
	/// The agent finished its work.
	Completed,
	/// The turn stopped because it was interrupted.
	Interrupted,
}

/// Why the CLI reported that a turn failed.
///
/// Typed on purpose: the host and the UI need to distinguish "log in again"
/// from "retry later" without parsing prose.
#[derive(
	Clone, Copy, Debug, Eq, Hash, PartialEq, strum::Display, strum::IntoStaticStr,
)]
#[strum(serialize_all = "snake_case")]
pub enum RemoteFailure {
	/// The CLI's own login is missing or expired.
	AuthRequired,
	/// The provider throttled the CLI.
	RateLimited,
	/// The conversation no longer fits the model's context.
	ContextOverflow,
	/// The provider or the CLI refused the request.
	Refused,
	/// Any other failure inside the CLI.
	Internal,
}

/// What kind of activity a tool event describes.
#[derive(
	Clone, Copy, Debug, Eq, Hash, PartialEq, strum::Display, strum::IntoStaticStr,
)]
#[strum(serialize_all = "snake_case")]
pub enum ToolKind {
	/// A shell or process command.
	Command,
	/// A file edit or write.
	FileChange,
	/// A call into an MCP server or other external tool.
	External,
	/// Anything the CLI reports that fits no other kind.
	Other,
}

/// How one tool call ended.
#[derive(
	Clone, Copy, Debug, Eq, Hash, PartialEq, strum::Display, strum::IntoStaticStr,
)]
#[strum(serialize_all = "snake_case")]
pub enum ToolOutcome {
	/// The call ran and succeeded.
	Succeeded,
	/// The call ran and failed.
	Failed,
	/// The call did not run because a permission was denied.
	Denied,
	/// The call was cut short by an interrupt.
	Interrupted,
}

/// Session and turn lifecycle.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LifecycleEvent {
	/// The CLI began working on a turn.
	TurnStarted {
		/// The turn.
		turn: TurnId,
	},
	/// The CLI finished a turn. Terminal for that turn.
	TurnCompleted {
		/// The turn.
		turn:    TurnId,
		/// How it ended.
		outcome: TurnEnd,
	},
	/// The CLI ended the session by itself. A turn still in flight is lost.
	SessionClosed {
		/// The session.
		session: CliSessionId,
	},
}

/// Command and tool activity performed by the delegated agent.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ToolEvent {
	/// The agent started a call.
	Started {
		/// The turn.
		turn:  TurnId,
		/// The call.
		call:  CallId,
		/// What kind of activity it is.
		kind:  ToolKind,
		/// Display label: the command line or the tool name.
		label: Str,
	},
	/// The call ended.
	Finished {
		/// The turn.
		turn:    TurnId,
		/// The call.
		call:    CallId,
		/// How it ended.
		outcome: ToolOutcome,
	},
}

/// One normalized event of a delegated session.
///
/// Every variant except [`LifecycleEvent::SessionClosed`] belongs to a turn,
/// and the host rejects events of any turn other than the one in flight.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DelegatedEvent {
	/// Session or turn lifecycle.
	Lifecycle(LifecycleEvent),
	/// Incremental agent text. Requires [`Feature::StreamingText`].
	AgentTextDelta {
		/// The turn.
		turn: TurnId,
		/// The new text.
		text: Str,
	},
	/// A complete agent message. Always allowed, so a CLI that only reports
	/// final text is representable without pretending to stream.
	AgentMessage {
		/// The turn.
		turn: TurnId,
		/// The full message text.
		text: Str,
	},
	/// Command or tool activity. Requires [`Feature::ToolEvents`].
	Tool(ToolEvent),
	/// The agent asks permission to act. Requires [`Feature::Permissions`].
	/// The agent is blocked until the host answers.
	Permission(PermissionRequest),
	/// Cumulative usage for the turn so far; the last one wins. Requires
	/// [`Feature::UsageReporting`]. Adapters mark it `UsageSource::Provider`.
	Usage {
		/// The turn.
		turn:  TurnId,
		/// Usage consumed by the turn so far.
		usage: Usage,
	},
	/// The turn failed. Terminal for that turn; no further event of the turn
	/// may follow it.
	Failure {
		/// The turn.
		turn:    TurnId,
		/// The typed reason.
		failure: RemoteFailure,
	},
}

impl DelegatedEvent {
	/// The turn this event belongs to, if it belongs to one.
	#[must_use]
	pub const fn turn(&self) -> Option<&TurnId> {
		match self {
			Self::Lifecycle(
				LifecycleEvent::TurnStarted { turn } | LifecycleEvent::TurnCompleted { turn, .. },
			)
			| Self::AgentTextDelta { turn, .. }
			| Self::AgentMessage { turn, .. }
			| Self::Tool(ToolEvent::Started { turn, .. } | ToolEvent::Finished { turn, .. })
			| Self::Usage { turn, .. }
			| Self::Failure { turn, .. } => Some(turn),
			Self::Permission(request) => Some(&request.turn),
			Self::Lifecycle(LifecycleEvent::SessionClosed { .. }) => None,
		}
	}

	/// The optional feature an executor must have advertised to emit this
	/// event.
	#[must_use]
	pub const fn requires(&self) -> Option<Feature> {
		match self {
			Self::AgentTextDelta { .. } => Some(Feature::StreamingText),
			Self::Tool(_) => Some(Feature::ToolEvents),
			Self::Permission(_) => Some(Feature::Permissions),
			Self::Usage { .. } => Some(Feature::UsageReporting),
			Self::Lifecycle(_) | Self::AgentMessage { .. } | Self::Failure { .. } => None,
		}
	}
}
