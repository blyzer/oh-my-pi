//! Adapters from the kernel mailbox onto the shared contract sinks.

use std::sync::Arc;

use omp_ai::{ChatRequest, ChatStream};
use omp_contract::{
	ApprovalMailbox, ApprovalPublisher, ApprovalRequest, DirectorNotify, EnvEvent, EnvEventSink,
	ErasedInference, MailboxReject, SessionMailbox,
};
use omp_core::Str;

use crate::{ApprovalRoute, HookGate, Inference, KernelEvent, KernelEvents, Up};

/// Kernel mailbox adapted onto the environment and session sinks.
#[derive(Clone)]
pub struct UpMailbox(pub flume::Sender<Up>);

impl EnvEventSink for UpMailbox {
	fn send(&self, event: EnvEvent) -> Result<(), EnvEvent> {
		self
			.0
			.send(Up::Env(event))
			.map_err(|error| match error.into_inner() {
				Up::Env(event) => event,
				_ => unreachable!("the sink only forwards environment events"),
			})
	}

	fn try_send(&self, event: EnvEvent) -> Result<(), EnvEvent> {
		self
			.0
			.try_send(Up::Env(event))
			.map_err(|error| match error.into_inner() {
				Up::Env(event) => event,
				_ => unreachable!("the sink only forwards environment events"),
			})
	}
}

impl SessionMailbox for UpMailbox {
	fn try_send_env(&self, event: EnvEvent) -> Result<(), MailboxReject<EnvEvent>> {
		self
			.0
			.try_send(Up::Env(event))
			.map_err(|error| match error {
				flume::TrySendError::Full(Up::Env(event)) => MailboxReject::Full(event),
				flume::TrySendError::Disconnected(Up::Env(event)) => MailboxReject::Disconnected(event),
				flume::TrySendError::Full(_) | flume::TrySendError::Disconnected(_) => {
					unreachable!("the mailbox only forwards environment events")
				},
			})
	}

	fn try_send_peer(&self, body: Str) -> Result<(), MailboxReject<Str>> {
		self
			.0
			.try_send(Up::Peer(body))
			.map_err(|error| match error {
				flume::TrySendError::Full(Up::Peer(body)) => MailboxReject::Full(body),
				flume::TrySendError::Disconnected(Up::Peer(body)) => MailboxReject::Disconnected(body),
				flume::TrySendError::Full(_) | flume::TrySendError::Disconnected(_) => {
					unreachable!("the mailbox only forwards peer text")
				},
			})
	}

	fn send_env<'a>(&'a self, event: EnvEvent) -> omp_contract::MailboxSendFut<'a, EnvEvent> {
		let sender = self.0.clone();
		Box::pin(async move {
			sender
				.send_async(Up::Env(event))
				.await
				.map_err(|error| match error.into_inner() {
					Up::Env(event) => MailboxReject::Disconnected(event),
					_ => unreachable!("the mailbox only forwards environment events"),
				})
		})
	}

	fn send_peer<'a>(&'a self, body: Str) -> omp_contract::MailboxSendFut<'a, Str> {
		let sender = self.0.clone();
		Box::pin(async move {
			sender
				.send_async(Up::Peer(body))
				.await
				.map_err(|error| match error.into_inner() {
					Up::Peer(body) => MailboxReject::Disconnected(body),
					_ => unreachable!("the mailbox only forwards peer text"),
				})
		})
	}

	fn same_mailbox(&self, other: &dyn SessionMailbox) -> bool {
		other
			.mailbox_any()
			.downcast_ref::<Self>()
			.is_some_and(|other| self.0.same_channel(&other.0))
	}

	fn mailbox_any(&self) -> &(dyn std::any::Any + Send + Sync) {
		self
	}
}

/// Whether `live` and the endpoint mailbox send onto the same kernel channel.
#[must_use]
pub fn up_same_channel(live: &flume::Sender<Up>, mailbox: &dyn SessionMailbox) -> bool {
	mailbox
		.mailbox_any()
		.downcast_ref::<UpMailbox>()
		.is_some_and(|endpoint| live.same_channel(&endpoint.0))
}

struct KernelApprovalMailbox(flume::Sender<Up>);

impl ApprovalMailbox for KernelApprovalMailbox {
	fn deliver(&self, request: ApprovalRequest) -> Result<(), ApprovalRequest> {
		self
			.0
			.send(Up::Approval(request))
			.map_err(|error| match error.into_inner() {
				Up::Approval(request) => request,
				_ => unreachable!("the mailbox only forwards approval requests"),
			})
	}
}

/// Binds an approval route to this kernel's upward mailbox.
#[must_use]
pub fn kernel_approval_route(
	mailbox: flume::Sender<Up>,
	hook_gate: Option<Arc<HookGate>>,
) -> ApprovalRoute {
	ApprovalRoute::to_mailbox(Arc::new(KernelApprovalMailbox(mailbox)), hook_gate)
}

impl ApprovalPublisher for KernelEvents {
	fn publish_requested(&self, ticket: crate::ApprovalTicket) {
		self.publish(KernelEvent::ApprovalRequested(ticket));
	}
}

impl DirectorNotify for KernelEvents {
	fn compaction_speculating(&self, percent: u8) {
		self.publish(KernelEvent::CompactionSpeculating { percent });
	}

	fn compaction_settled(&self, applied: bool) {
		self.publish(KernelEvent::CompactionSettled { applied });
	}
}

/// Adapts the kernel inference capability onto [`ErasedInference`].
pub struct InferenceErase<'a, T: ?Sized>(pub &'a mut T);

impl<T: Inference + ?Sized> ErasedInference for InferenceErase<'_, T> {
	fn execute(
		&mut self,
		request: ChatRequest,
	) -> crate::BoxFut<'_, Result<ChatStream, omp_ai::Error>> {
		Box::pin(self.0.chat(request))
	}

	fn execute_on<'a>(
		&'a mut self,
		selector: &str,
		request: ChatRequest,
	) -> crate::BoxFut<'a, Result<ChatStream, omp_ai::Error>> {
		let selector = Str::new(selector);
		Box::pin(async move { self.0.chat_on(selector.as_str(), request).await })
	}
}
