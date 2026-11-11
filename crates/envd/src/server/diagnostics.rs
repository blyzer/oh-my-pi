//! Late-diagnostic batching and the agent CONTROL binding.

use super::{EnvServer, prelude::*};

pub(in crate::server) struct LateDiagnosticsBatcher {
	pub(in crate::server) pending:
		parking_lot::Mutex<Vec<omp_session::late_diagnostics::LateDiagnosticsFile>>,
	pub(in crate::server) scheduled: AtomicBool,
	pub(in crate::server) active:    AtomicBool,
	pub(in crate::server) sender:    KernelSender,
}

impl LateDiagnosticsBatcher {
	pub(in crate::server) fn reset(&self) {
		self.pending.lock().clear();
	}

	pub(in crate::server) fn push(
		self: &Arc<Self>,
		diagnostics: omp_session::late_diagnostics::LateDiagnostics,
	) {
		if !self.active.load(Ordering::Acquire) {
			return;
		}
		let mut pending = self.pending.lock();
		for file in diagnostics.files {
			if let Some(existing) = pending.iter_mut().find(|known| known.path == file.path) {
				existing.messages.extend(file.messages);
				existing.recount();
			} else {
				pending.push(file);
			}
		}
		drop(pending);
		if !self.scheduled.swap(true, Ordering::AcqRel) {
			let batcher = Arc::clone(self);
			tokio::spawn(async move { batcher.flush().await });
		}
	}

	pub(in crate::server) async fn flush(self: Arc<Self>) {
		loop {
			tokio::time::sleep(Duration::from_millis(25)).await;
			let files = {
				let mut pending = self.pending.lock();
				if !self.active.load(Ordering::Acquire) {
					pending.clear();
					self.scheduled.store(false, Ordering::Release);
					return;
				}
				if pending.is_empty() {
					self.scheduled.store(false, Ordering::Release);
					return;
				}
				mem::take(&mut *pending)
			};
			let _ = self.sender.send(omp_contract::EnvEvent::LateDiagnostics(
				omp_session::late_diagnostics::LateDiagnostics { files },
			));
			let pending = self.pending.lock();
			if pending.is_empty() {
				self.scheduled.store(false, Ordering::Release);
				return;
			}
		}
	}
}

/// Sole-owner lease for Agent CONTROL routes installed in one environment.
#[must_use]
pub struct AgentControlBinding {
	pub(in crate::server) server:      Arc<EnvServer>,
	pub(in crate::server) id:          u64,
	pub(in crate::server) diagnostics: Option<Arc<LateDiagnosticsBatcher>>,
}

impl AgentControlBinding {
	/// Re-derives checkpoint state and drops obsolete deferred diagnostics when
	/// the controller selects another journal with the same authenticated
	/// mailbox.
	pub fn refresh_session(&self, dom: &omp_dom::Dom) {
		if let Some(diagnostics) = &self.diagnostics {
			diagnostics.reset();
		}
		if let Some(environment) = &self.server.environment {
			environment.documents.reset_late_diagnostics(self.id);
		}
		self.server.checkpoint_control.restore_session(self.id, dom);
	}
}

impl Drop for AgentControlBinding {
	fn drop(&mut self) {
		if let Some(diagnostics) = &self.diagnostics {
			diagnostics.active.store(false, Ordering::Release);
		}
		self.server.release_agent_control(self.id);
	}
}
