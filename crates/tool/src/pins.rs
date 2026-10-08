//! Resolutions a call's judgment fixes for its execution.
//!
//! An argument-scoped tool is judged before it runs:
//! [`Tool::invocation_effects`] decides the envelope approval and the write
//! boundary hold the call to, and [`Tool::fetch_locators`] the hosts a fetch
//! approval names. Where that judgment reads live state, the state can change
//! before the executor acts on it: the mounted server answering an `mcp://`
//! resource, a local process when the call was judged, may be a remote one by
//! the time the read runs, after a prompt the user answered for the first.
//!
//! A dispatcher that judges a call and then runs it gives the call one
//! [`InvocationPins`]. The judgment runs inside them
//! ([`InvocationPins::judge`]) and so does the execution
//! ([`InvocationPins::scope`]). A resolver whose answer depends on live state
//! resolves through [`InvocationPins::pin`], which fixes the first answer of a
//! call for the rest of it, and its executor reaches
//! only what [`InvocationPins::pinned`] reports, refusing when that can no
//! longer be reached as judged. A call no dispatcher judged has no pins and
//! resolves live: it was admitted on its tool's declared maximum.
//!
//! [`Tool::invocation_effects`]: crate::Tool::invocation_effects
//! [`Tool::fetch_locators`]: crate::Tool::fetch_locators

use std::{future::Future, sync::Arc};

use omp_core::Str;
use parking_lot::Mutex;
use smallvec::SmallVec;
use tokio::task::futures::TaskLocalFuture;

use crate::FetchEffects;

tokio::task_local! {
	static PINS: Option<Arc<InvocationPins>>;
}

/// What one judgment resolved a live-state target to.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolutionPin {
	/// What answers the target, named by its resolver (the mounted MCP server
	/// advertising a resource); `None` when nothing did, so the call was judged
	/// on whatever may answer it by the time it runs.
	pub target: Option<Str>,
	/// The fetch reaching the target performs, as judged; `None` when it stays
	/// in local or environment-owned state.
	pub fetch:  Option<FetchEffects>,
}

impl ResolutionPin {
	/// Whether reaching the pinned target with `fetch` stays within what was
	/// judged: no fetch always does, and a fetch only when one was judged, a
	/// credentialed one only when the judged one was credentialed.
	#[must_use]
	pub const fn admits(&self, fetch: Option<FetchEffects>) -> bool {
		match (fetch, self.fetch) {
			(None, _) => true,
			(Some(_), None) => false,
			(Some(now), Some(judged)) => !now.credentials || judged.credentials,
		}
	}
}

/// One pinned resolution: the resolver that made it, the resource it
/// resolved, and its answer.
#[derive(Debug)]
struct Pinned {
	resolver: &'static str,
	resource: Str,
	pin:      ResolutionPin,
}

/// The resolutions one call's judgment fixed, which its execution reaches.
///
/// A call names a handful of targets, so the pins are a short list searched
/// in order.
#[derive(Debug, Default)]
pub struct InvocationPins {
	pins: Mutex<SmallVec<Pinned, 2>>,
}

impl InvocationPins {
	/// Runs `judgment` (the call's [`crate::Registry::invocation_effects`],
	/// [`crate::Registry::fetch_locators`] and the naming of the hosts they
	/// reach) inside `pins`; `None` runs it unpinned.
	pub fn judge<R>(pins: Option<&Arc<Self>>, judgment: impl FnOnce() -> R) -> R {
		PINS.sync_scope(pins.cloned(), judgment)
	}

	/// Runs `execution` (the call's executor) inside `pins`; `None` runs it
	/// unpinned. One wrapper either way.
	pub fn scope<F: Future>(
		pins: Option<Arc<Self>>,
		execution: F,
	) -> TaskLocalFuture<Option<Arc<Self>>, F> {
		PINS.scope(pins, execution)
	}

	/// Resolves `resource` through `resolver` once per judged call: inside a
	/// call's pins the first resolution is kept and returned to every later
	/// one; outside any pins (a call no dispatcher judged) `resolve` answers
	/// live each time.
	pub fn pin(
		resolver: &'static str,
		resource: &str,
		resolve: impl FnOnce() -> ResolutionPin,
	) -> ResolutionPin {
		match current() {
			Some(pins) => pins.pin_with(resolver, resource, resolve),
			None => resolve(),
		}
	}

	/// What the current call's judgment pinned for `resource` through
	/// `resolver`; `None` when it pinned nothing (no dispatcher judged the call,
	/// or its judgment never resolved the target), so the executor resolves
	/// live.
	#[must_use]
	pub fn pinned(resolver: &str, resource: &str) -> Option<ResolutionPin> {
		current()?.get(resolver, resource)
	}

	fn get(&self, resolver: &str, resource: &str) -> Option<ResolutionPin> {
		self
			.pins
			.lock()
			.iter()
			.find(|pinned| pinned.resolver == resolver && pinned.resource == *resource)
			.map(|pinned| pinned.pin.clone())
	}

	fn pin_with(
		&self,
		resolver: &'static str,
		resource: &str,
		resolve: impl FnOnce() -> ResolutionPin,
	) -> ResolutionPin {
		if let Some(pin) = self.get(resolver, resource) {
			return pin;
		}
		// Resolved outside the lock, which the resolver's own state never
		// waits on; a racing judgment of the same target keeps the first pin.
		let pin = resolve();
		let mut pins = self.pins.lock();
		if let Some(pinned) = pins
			.iter()
			.find(|pinned| pinned.resolver == resolver && pinned.resource == *resource)
		{
			return pinned.pin.clone();
		}
		pins.push(Pinned { resolver, resource: Str::from(resource), pin: pin.clone() });
		pin
	}
}

fn current() -> Option<Arc<InvocationPins>> {
	PINS.try_with(Option::clone).ok().flatten()
}

#[cfg(test)]
mod tests {
	use std::sync::atomic::{AtomicUsize, Ordering};

	use omp_core::sf;

	use super::*;

	fn answered_by(server: &'static str, fetch: Option<FetchEffects>) -> ResolutionPin {
		ResolutionPin { target: Some(Str::new_static(server)), fetch }
	}

	/// Inside a call's pins the first resolution of a target holds for the
	/// rest of the call, its judgment and its execution alike, whatever later
	/// resolutions would answer; each resolver's targets are its own, and a
	/// call's pins are never another call's.
	#[tokio::test]
	async fn the_first_resolution_of_a_judged_call_holds_for_its_execution() {
		let resolved = AtomicUsize::new(0);
		let live = |server: &'static str| {
			resolved.fetch_add(1, Ordering::Relaxed);
			answered_by(server, None)
		};
		let pins = Arc::new(InvocationPins::default());
		let judged = InvocationPins::judge(Some(&pins), || {
			let first = InvocationPins::pin("mcp", "doc://a", || live("local"));
			let again = InvocationPins::pin("mcp", "doc://a", || live("remote"));
			let other = InvocationPins::pin("other", "doc://a", || live("other"));
			(first, again, other)
		});
		assert_eq!(
			judged,
			(answered_by("local", None), answered_by("local", None), answered_by("other", None))
		);
		assert_eq!(resolved.load(Ordering::Relaxed), 2, "a pinned target is not resolved again");

		let executed = InvocationPins::scope(Some(Arc::clone(&pins)), async {
			(
				InvocationPins::pinned("mcp", "doc://a"),
				InvocationPins::pin("mcp", "doc://a", || live("remote")),
				InvocationPins::pinned("mcp", "doc://b"),
			)
		})
		.await;
		assert_eq!(executed, (Some(answered_by("local", None)), answered_by("local", None), None));

		let elsewhere = Arc::new(InvocationPins::default());
		let unpinned =
			InvocationPins::scope(Some(elsewhere), async { InvocationPins::pinned("mcp", "doc://a") })
				.await;
		assert_eq!(unpinned, None, "another call's pins never answer");
	}

	/// A call no dispatcher judged resolves live every time and keeps nothing.
	#[tokio::test]
	async fn an_unjudged_call_resolves_live() {
		assert_eq!(
			InvocationPins::pin("mcp", "doc://a", || answered_by("local", None)).target,
			Some(sf!("local"))
		);
		assert_eq!(
			InvocationPins::pin("mcp", "doc://a", || answered_by("remote", None)).target,
			Some(sf!("remote"))
		);
		assert_eq!(InvocationPins::pinned("mcp", "doc://a"), None);
		let unscoped = InvocationPins::scope(None, async {
			InvocationPins::pin("mcp", "doc://a", || answered_by("remote", None));
			InvocationPins::pinned("mcp", "doc://a")
		})
		.await;
		assert_eq!(unscoped, None);
		assert_eq!(InvocationPins::judge(None, || InvocationPins::pinned("mcp", "doc://a")), None);
	}

	/// A pinned target admits reaching it with no more fetch than was judged.
	#[test]
	fn a_pin_admits_no_more_fetch_than_was_judged() {
		let anonymous = Some(FetchEffects { credentials: false });
		let credentialed = Some(FetchEffects { credentials: true });
		let local = answered_by("local", None);
		assert!(local.admits(None));
		assert!(!local.admits(anonymous));
		assert!(!local.admits(credentialed));
		let open = answered_by("remote", anonymous);
		assert!(open.admits(None));
		assert!(open.admits(anonymous));
		assert!(!open.admits(credentialed));
		let keyed = answered_by("remote", credentialed);
		assert!(keyed.admits(None) && keyed.admits(anonymous) && keyed.admits(credentialed));
	}
}
