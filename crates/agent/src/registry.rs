//! Host-owned registry seam for routing between live session kernels.

use std::sync::{
	Arc,
	atomic::{AtomicUsize, Ordering},
};

pub use omp_contract::session::{
	AutoreplyRequest, PeerAutoreply, SessionAuthority, SessionEndpoint, SessionRole, SessionTopology,
};

/// Recipient-side obligations that must settle before an ordinary turn can
/// release its controller.
///
/// The count is runtime coordination, not durable state. Each accepted
/// side-channel request owns one guard; dropping the guard wakes the turn
/// boundary even on inference failure or cancellation.
#[derive(Clone, Default)]
pub struct ReplyObligations {
	inner: Arc<ReplyObligationsInner>,
}

#[derive(Default)]
struct ReplyObligationsInner {
	pending: AtomicUsize,
	notify:  tokio::sync::Notify,
}

impl ReplyObligations {
	/// Registers one accepted side-channel reply.
	#[must_use]
	pub fn begin(&self) -> ReplyObligation {
		self.inner.pending.fetch_add(1, Ordering::AcqRel);
		ReplyObligation { obligations: self.clone() }
	}

	/// Whether any recipient-owned reply has not settled.
	#[must_use]
	pub fn is_pending(&self) -> bool {
		self.inner.pending.load(Ordering::Acquire) != 0
	}

	/// Resolves after every currently registered obligation settles.
	pub async fn wait(&self) {
		loop {
			let notified = self.inner.notify.notified();
			if !self.is_pending() {
				return;
			}
			notified.await;
		}
	}
}

/// Drop guard for one automatic peer reply obligation.
pub struct ReplyObligation {
	obligations: ReplyObligations,
}

impl Drop for ReplyObligation {
	fn drop(&mut self) {
		if self
			.obligations
			.inner
			.pending
			.fetch_sub(1, Ordering::AcqRel)
			== 1
		{
			self.obligations.inner.notify.notify_one();
		}
	}
}
