//! Remote commands a host submits to a running kernel under a client key.
//!
//! [`journal`] records each command through [`Session::patch_keyed`], the
//! one journal-backed key index that also covers keyed `msg.user@1` entries
//! and approval decisions. A retry of the same key therefore replays the entry
//! the first attempt created, also after the session is reopened, and a key
//! reused for any other mutation is a [`SessionError::KeyConflict`].
//!
//! The receipt says whether this submission journaled the command, never how
//! it was routed. That is sound only while the arm alone decides where a
//! command lands: [`crate::CallControl`] routes steer and queue with no
//! degradation path, and the keyed patch records the arm's label and request,
//! so a replay always describes an original with the same arm. The
//! `receipt_without_disposition` tests in [`crate::dispatch`] pin that; a
//! routing change that breaks them needs a disposition on the receipt first.

use omp_core::Str;
use omp_dom::Txn;
use omp_journal::{EntryId, idempotency::IdempotencyKey};
use omp_session::{AppendOutcome, Session, SessionError};

/// An authenticated remote command accepted by a running kernel.
///
/// Its serde form, with the author, is the typed request the journal compares
/// on a retry of the same key.
#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SubmitCommand {
	/// Queue a steering aside for the current turn.
	Steer(Str),
	/// Queue a follow-up for the next turn.
	Queue(Str),
	/// Pause (`true`) or resume (`false`) the session.
	Pause(bool),
	/// Act on the running turn without writing a node.
	Turn(TurnControl),
}

/// A remote command that acts on the running turn and writes no node.
///
/// It is still journaled, as an empty keyed patch labelled with its strum
/// name, so a retried key replays instead of acting on a later turn.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, strum::IntoStaticStr)]
#[serde(rename_all = "snake_case")]
pub enum TurnControl {
	/// Interrupt the current turn.
	#[strum(serialize = "control.interrupt")]
	Interrupt,
	/// Abort every tool running in the current turn; the turn continues.
	#[strum(serialize = "control.abort_tools")]
	AbortTools,
	/// Cancel the session.
	#[strum(serialize = "control.cancel")]
	Cancel,
}

/// Durable outcome of a remote command the kernel handled.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SubmitReceipt {
	/// This submission journaled the command.
	Applied {
		/// The entry that carries the command.
		entry: EntryId,
		/// One-based position of that entry in the journal.
		seq:   u64,
	},
	/// The key already named this command, so nothing was journaled.
	Duplicate {
		/// The entry the original submission created.
		entry: EntryId,
	},
}

impl SubmitReceipt {
	/// The entry that carries the command, new or original.
	#[must_use]
	pub const fn entry(self) -> EntryId {
		match self {
			Self::Applied { entry, .. } | Self::Duplicate { entry } => entry,
		}
	}
}

/// Journals `command` under `key`, or finds the entry an earlier submission
/// of the same key and the same command created.
pub(crate) fn journal(
	session: &mut Session,
	command: &SubmitCommand,
	author: &Str,
	key: IdempotencyKey,
) -> Result<SubmitReceipt, SessionError> {
	let txn = match command {
		SubmitCommand::Steer(text) => {
			crate::steering::steering_txn(session, text.clone(), &[], Some(author.clone()))?
		},
		SubmitCommand::Queue(text) => {
			crate::steering::prompt_txn(session, text.clone(), &[], Some(author.clone()))?
		},
		SubmitCommand::Pause(active) => crate::pause::pause_txn(session, *active)?,
		SubmitCommand::Turn(control) => Txn {
			cause: session.head().ok_or(SessionError::NoActiveTurn)?,
			label: Some(Str::new_static(control.into())),
			ops:   Vec::new(),
		},
	};
	Ok(match session.patch_keyed(txn, key, &(command, author))? {
		AppendOutcome::Appended(entry) => {
			SubmitReceipt::Applied { entry, seq: session.entry_count() as u64 }
		},
		AppendOutcome::Replayed(entry) => SubmitReceipt::Duplicate { entry },
	})
}
