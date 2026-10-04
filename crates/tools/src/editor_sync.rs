//! The editor write-back a durable commit hands its tool (ADR 0037 §4.3–§4.6).
//!
//! When an ACP editor is bound, a document host starts one write-back per
//! committed text document once the commit is durable. The tool builds its
//! result from the commit, then waits for those write-backs before it settles,
//! so a later call in the same turn observes a synced editor. A write-back
//! never fails the tool: its failures, conflicts and client reformatting
//! arrive as diags. An interrupt after the commit settles the tool at once
//! with `editor_sync_pending`; the write-back itself continues on the
//! session's queue.

use std::{future::Future, sync::Arc};

use futures::{FutureExt as _, pin_mut, select_biased};
use omp_core::{Str, sf};
use omp_tool::{Diag, DiagKind};

/// One committed document being written back.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EditorSyncTarget {
	/// Canonical path of the document.
	pub path:     Str,
	/// The committed revision being written back.
	pub revision: Str,
}

/// The notices of one finished write-back.
#[derive(Clone, Debug)]
pub struct EditorSyncReport {
	/// Index of the write-back's [`EditorSyncTarget`].
	pub target: usize,
	/// Notices for the tool element; empty for a clean write-back.
	pub diags:  Vec<Diag>,
}

/// The write-backs following one commit; empty without a bound editor.
///
/// A cheap handle: clones share the same write-backs and compare equal, and
/// one of them settles.
#[derive(Clone, Debug, Default)]
pub struct EditorSync(Option<Arc<Pending>>);

#[derive(Debug)]
struct Pending {
	targets: Vec<EditorSyncTarget>,
	reports: flume::Receiver<EditorSyncReport>,
}

impl PartialEq for EditorSync {
	fn eq(&self, other: &Self) -> bool {
		match (&self.0, &other.0) {
			(Some(this), Some(other)) => Arc::ptr_eq(this, other),
			(None, None) => true,
			_ => false,
		}
	}
}

impl Eq for EditorSync {}

impl EditorSync {
	/// Write-backs of `targets`, each reporting once on `reports`.
	#[must_use]
	pub fn new(targets: Vec<EditorSyncTarget>, reports: flume::Receiver<EditorSyncReport>) -> Self {
		Self((!targets.is_empty()).then(|| Arc::new(Pending { targets, reports })))
	}

	/// Whether no write-back follows the commit.
	#[must_use]
	pub const fn is_empty(&self) -> bool {
		self.0.is_none()
	}

	/// The documents being written back.
	#[must_use]
	pub fn targets(&self) -> &[EditorSyncTarget] {
		self.0.as_ref().map_or(&[], |pending| &pending.targets)
	}

	/// Waits until every write-back reported, or until `interrupt` resolves,
	/// and returns the notices for the tool element. Each write-back that has
	/// not reported by then is named by one `editor_sync_pending` notice; it
	/// continues without the tool.
	pub async fn settle<T>(self, interrupt: impl Future<Output = T>) -> Vec<Diag> {
		let Some(pending) = self.0 else {
			return Vec::new();
		};
		let Pending { targets, reports } = &*pending;
		let mut reported = vec![false; targets.len()];
		let mut remaining = targets.len();
		let mut diags = Vec::new();
		let interrupt = interrupt.fuse();
		pin_mut!(interrupt);
		while remaining > 0 {
			let report = reports.recv_async().fuse();
			pin_mut!(report);
			select_biased! {
				report = report => {
					let Ok(report) = report else { break };
					if let Some(slot) = reported.get_mut(report.target)
						&& !*slot
					{
						*slot = true;
						remaining -= 1;
						diags.extend(report.diags);
					}
				},
				_ = interrupt => break,
			}
		}
		for (target, reported) in targets.iter().zip(reported) {
			if !reported {
				diags.push(Diag::info(
					DiagKind::EditorSyncPending,
					sf!(
						"{} is committed at revision {}; writing it back to the editor continues after \
						 this call",
						target.path,
						target.revision
					),
				));
			}
		}
		diags
	}
}

#[cfg(test)]
mod tests {
	use std::future;

	use super::*;

	fn target(path: &'static str) -> EditorSyncTarget {
		EditorSyncTarget { path: Str::new_static(path), revision: Str::new_static("1:ab") }
	}

	#[tokio::test]
	async fn settling_waits_for_every_report() {
		let (reports, received) = flume::unbounded();
		let sync = EditorSync::new(vec![target("/a"), target("/b")], received);
		reports
			.send(EditorSyncReport {
				target: 1,
				diags:  vec![Diag::warn(DiagKind::EditorSyncFailed, "b failed")],
			})
			.expect("report b");
		reports
			.send(EditorSyncReport { target: 0, diags: Vec::new() })
			.expect("report a");
		let diags = sync.settle(future::pending::<()>()).await;
		assert_eq!(diags.len(), 1);
		assert_eq!(diags[0].native_kind(), Some(DiagKind::EditorSyncFailed));
	}

	#[tokio::test]
	async fn an_interrupt_settles_with_the_unreported_write_backs_pending() {
		let (reports, received) = flume::unbounded();
		let sync = EditorSync::new(vec![target("/a"), target("/b")], received);
		reports
			.send(EditorSyncReport { target: 0, diags: Vec::new() })
			.expect("report a");
		let diags = sync.settle(future::ready(())).await;
		assert_eq!(diags.len(), 1, "{diags:?}");
		assert_eq!(diags[0].native_kind(), Some(DiagKind::EditorSyncPending));
		assert!(diags[0].text.contains("/b"), "{}", diags[0].text);
		drop(reports);
	}

	#[tokio::test]
	async fn an_empty_sync_settles_at_once() {
		assert!(EditorSync::default().is_empty());
		assert!(
			EditorSync::default()
				.settle(future::pending::<()>())
				.await
				.is_empty()
		);
	}
}
