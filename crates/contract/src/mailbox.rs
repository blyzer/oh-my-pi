//! Sinks the environment and session routing use instead of the kernel mailbox.

use std::{any::Any, sync::Arc};

use omp_core::Str;

use crate::EnvEvent;

/// A mailbox refused one message.
#[derive(Debug)]
pub enum MailboxReject<T> {
	/// The mailbox was bounded and full. The message comes back.
	Full(T),
	/// The receiver is gone. The message comes back.
	Disconnected(T),
}

impl<T> MailboxReject<T> {
	fn from_flume(error: flume::TrySendError<T>) -> Self {
		match error {
			flume::TrySendError::Full(value) => Self::Full(value),
			flume::TrySendError::Disconnected(value) => Self::Disconnected(value),
		}
	}
}

/// Non-blocking sink for one environment observation.
///
/// Implementations forward onto an unbounded mailbox. `send` and `try_send`
/// fail only when the receiver is gone.
pub trait EnvEventSink: Send + Sync {
	/// Delivers `event`, waiting only for the mailbox mutex.
	fn send(&self, event: EnvEvent) -> Result<(), EnvEvent>;

	/// Delivers `event` or reports that the receiver is gone.
	fn try_send(&self, event: EnvEvent) -> Result<(), EnvEvent>;
}

impl SessionMailbox for flume::Sender<EnvEvent> {
	fn try_send_env(&self, event: EnvEvent) -> Result<(), MailboxReject<EnvEvent>> {
		self.try_send(event).map_err(MailboxReject::from_flume)
	}

	fn try_send_peer(&self, body: Str) -> Result<(), MailboxReject<Str>> {
		let _ = self;
		Err(MailboxReject::Disconnected(body))
	}

	fn same_mailbox(&self, other: &dyn SessionMailbox) -> bool {
		other
			.mailbox_any()
			.downcast_ref::<Self>()
			.is_some_and(|other| self.same_channel(other))
	}

	fn mailbox_any(&self) -> &(dyn Any + Send + Sync) {
		self
	}
}

impl EnvEventSink for flume::Sender<EnvEvent> {
	fn send(&self, event: EnvEvent) -> Result<(), EnvEvent> {
		flume::Sender::send(self, event).map_err(|error| error.into_inner())
	}

	fn try_send(&self, event: EnvEvent) -> Result<(), EnvEvent> {
		flume::Sender::try_send(self, event).map_err(|error| error.into_inner())
	}
}

/// Sending half of the environment observation mailbox.
///
/// The kernel adapts this onto its single upward channel. Environment code
/// never names that channel's other messages.
pub type KernelSender = Arc<dyn EnvEventSink>;

/// Messages a live session endpoint accepts from environment and peer routing.
///
/// The kernel mailbox stays the FIFO owner. This trait is the slice of it
/// that routing can name without the rest of the control enum.
pub trait SessionMailbox: Send + Sync {
	/// Delivers one environment observation.
	fn try_send_env(&self, event: EnvEvent) -> Result<(), MailboxReject<EnvEvent>>;

	/// Delivers one ordinary peer message.
	fn try_send_peer(&self, body: Str) -> Result<(), MailboxReject<Str>>;

	/// Whether `other` sends onto the same channel as `self`.
	fn same_mailbox(&self, other: &dyn SessionMailbox) -> bool;

	/// Downcast identity used by [`SessionMailbox::same_mailbox`].
	fn mailbox_any(&self) -> &(dyn Any + Send + Sync);
}
