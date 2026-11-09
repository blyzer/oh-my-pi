//! Environment observations carried to the session authority.

use std::sync::Arc;

use omp_core::Str;
use omp_journal::data::IrcTraffic;
use omp_proto::env::v1::{WorkspaceRestored, WorkspaceSnapshot};

/// Environment observation or control request for the session authority.
///
/// These messages are ephemeral transport. The kernel translates accepted
/// changes into session patches before they become authoritative.
#[derive(Clone, Debug)]
pub enum EnvEvent {
	/// The extension/device roster changed.
	DeviceAvailability {
		/// Canonical JSON projection of the available devices.
		payload: Str,
	},
	/// A workspace generation was captured before speculative exploration.
	CheckpointOpened {
		/// Opaque token scoped to the bound session controller.
		token:        Str,
		/// Human-readable unique branch label.
		label:        Str,
		/// Human-readable exploration goal.
		goal:         Str,
		/// Immediate checkpoint ancestor on the selected branch.
		parent_token: Option<Str>,
		/// Checkpoint creation time in epoch milliseconds.
		started_at:   u64,
		/// Typed environment-owned workspace generation.
		workspace:    WorkspaceSnapshot,
	},
	/// Workspace restoration completed and the matching journal branch may be
	/// selected.
	CheckpointRewind {
		/// Opaque token scoped to the bound session controller.
		token:      Str,
		/// Findings retained on the selected branch.
		report:     Str,
		/// Stable environment-issued operation receipt.
		receipt:    Str,
		/// Restoration result, including its durable undo generation.
		workspace:  WorkspaceRestored,
		/// Completion time in epoch milliseconds.
		rewound_at: u64,
	},
	/// A staged mutation requires a host-side resolution director.
	StagedPreview {
		/// Stable staged proposal identity.
		proposal_id: Str,
		/// Tool that produced the proposal.
		source_tool: Str,
	},
	/// Replay-stable incoming, autoreply, relay, or work-pool IRC traffic.
	IrcTraffic {
		/// Complete producer-attributed observation, shared across mailbox
		/// fan-out without cloning its message body or attribution strings.
		payload: Arc<IrcTraffic>,
	},
	/// Revision-fenced diagnostics that settled after a mutation tool returned.
	LateDiagnostics(omp_session::late_diagnostics::LateDiagnostics),
	/// A durable extension message projected into inference and the transcript.
	///
	/// Renderer output is optional presentation metadata: its semantic Markdown
	/// body remains authoritative for replay, copy, and fallback rendering.
	CustomMessage(omp_session::custom_message::CustomMessage),
	/// A hook or extension notice journaled as `<notice kind=… name=…>` under
	/// the current turn at the next mailbox drain.
	Notice {
		/// Notice kind (`hook`, `custom`, …).
		kind: Str,
		/// Producer-chosen name.
		name: Option<Str>,
		/// Notice body.
		body: Str,
	},
	/// A host notice whose identifying facts travel as a typed JSON payload,
	/// journaled as `<notice kind=… name=…>` carrying that payload in its data
	/// prop; `body` is only the fallback for projections that ignore it.
	TypedNotice {
		/// Notice kind (`warn`, …).
		kind: Str,
		/// Producer-chosen name.
		name: Str,
		/// Typed payload, serialized once by the producer.
		data: Box<serde_json::value::RawValue>,
		/// Fallback body.
		body: Str,
	},
}
