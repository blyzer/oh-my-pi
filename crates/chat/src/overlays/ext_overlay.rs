//! Extension-authored retained overlays (`omp.ui.overlay`).
//!
//! The chat host opens one [`ExtOverlay`] panel per request. Content is
//! extension-origin TML (core chrome tags degrade to custom elements). The
//! panel never writes terminal escapes: the host composites its frame like
//! every other [`Panel`].

use std::{
	collections::BTreeMap,
	sync::{Arc, Mutex},
	time::Duration,
};

use omp_core::Str;
use omp_tui::{Frame, Key, Size, Ui, UiContext, UiEvent};

use super::{Panel, PanelAnchor, PanelEvent};

/// Shared settlement cell for one overlay: waiters observe close / submit.
#[derive(Clone, Debug, Default)]
pub struct OverlayState {
	inner: Arc<Mutex<OverlayInner>>,
}

#[derive(Clone, Debug, Default)]
struct OverlayInner {
	closed:  bool,
	values:  BTreeMap<String, serde_json::Value>,
	events:  Vec<serde_json::Value>,
	outcome: Option<OverlayOutcome>,
}

/// Total result published when an overlay settles.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OverlayOutcome {
	/// Whether the user cancelled (Esc / close) rather than submitting.
	pub cancelled: bool,
	/// Optional reason string (`dismissed`, …).
	pub reason:    Option<Str>,
}

impl OverlayState {
	/// Creates an open overlay state.
	#[must_use]
	pub fn new() -> Self {
		Self::default()
	}

	/// Current field values snapshot.
	#[must_use]
	pub fn values(&self) -> serde_json::Map<String, serde_json::Value> {
		self
			.inner
			.lock()
			.expect("overlay state")
			.values
			.iter()
			.map(|(k, v)| (k.clone(), v.clone()))
			.collect()
	}

	/// Watched events drained so far.
	#[must_use]
	pub fn events(&self) -> Vec<serde_json::Value> {
		self.inner.lock().expect("overlay state").events.clone()
	}

	/// Whether the overlay has been closed.
	#[must_use]
	pub fn closed(&self) -> bool {
		self.inner.lock().expect("overlay state").closed
	}

	/// Settlement outcome once closed.
	#[must_use]
	pub fn outcome(&self) -> Option<OverlayOutcome> {
		self.inner.lock().expect("overlay state").outcome.clone()
	}

	/// Marks the overlay closed from the host (idempotent close request).
	pub fn close_for_host(&self) {
		self.close(true, Some(Str::new_static("dismissed")));
	}

	fn close(&self, cancelled: bool, reason: Option<Str>) {
		let mut inner = self.inner.lock().expect("overlay state");
		if inner.closed {
			return;
		}
		inner.closed = true;
		inner.outcome = Some(OverlayOutcome { cancelled, reason });
	}

	fn record_event(&self, event: serde_json::Value) {
		self.inner.lock().expect("overlay state").events.push(event);
	}

	fn capture_values(&self, values: serde_json::Value) {
		let mut inner = self.inner.lock().expect("overlay state");
		inner.values = match values {
			serde_json::Value::Object(map) => map.into_iter().collect(),
			_ => BTreeMap::new(),
		};
	}
}

/// Retained extension overlay panel.
pub struct ExtOverlay {
	id:     &'static str,
	ui:     Ui,
	ctx:    UiContext,
	width:  u16,
	state:  OverlayState,
	watch:  Vec<Str>,
	anchor: PanelAnchor,
}

impl ExtOverlay {
	/// Opens an overlay for app-issued `id` with extension-origin `source`.
	///
	/// `id` is leaked into a `'static` slot identity so [`Panel::id`] can name
	/// it; overlay ids are few and session-scoped.
	#[must_use]
	pub fn open(
		id: Str,
		source: &str,
		viewport: Size,
		ctx: &UiContext,
		state: OverlayState,
		watch: Vec<Str>,
		modal: bool,
	) -> Self {
		let width = viewport.width.saturating_mul(80) / 100;
		let width = width.max(20).min(viewport.width);
		let ui = Ui::from_extension_markup(source, width, ctx.clone())
			.unwrap_or_else(|_| Ui::from_root(omp_tui::dom! { <col/> }, width, ctx.clone()));
		let id = Box::leak(id.to_string().into_boxed_str());
		Self {
			id,
			ui,
			ctx: ctx.clone(),
			width,
			state,
			watch,
			anchor: if modal {
				PanelAnchor::Center
			} else {
				PanelAnchor::Side
			},
		}
	}

	/// App-issued overlay id.
	#[must_use]
	pub fn overlay_id(&self) -> &str {
		self.id
	}

	/// Shared settlement state.
	#[must_use]
	pub fn state(&self) -> &OverlayState {
		&self.state
	}

	/// Replaces the overlay document with new extension-origin TML.
	pub fn set_content(&mut self, source: &str) -> bool {
		match Ui::from_extension_markup(source, self.width, self.ctx.clone()) {
			Ok(ui) => {
				self.ui = ui;
				true
			},
			Err(_) => false,
		}
	}

	fn settle(&mut self, cancelled: bool) -> PanelEvent {
		self.state.capture_values(self.ui.values());
		self
			.state
			.close(cancelled, cancelled.then_some(Str::new_static("dismissed")));
		PanelEvent::Close
	}
}

impl Panel for ExtOverlay {
	fn id(&self) -> &'static str {
		self.id
	}

	fn anchor(&self) -> PanelAnchor {
		self.anchor
	}

	fn key(&mut self, key: Key) -> PanelEvent {
		match key {
			Key::Esc => return self.settle(true),
			Key::Enter => {
				if matches!(self.ui.handle_key(key), UiEvent::Submit | UiEvent::Changed { .. }) {
					return self.settle(false);
				}
				return self.settle(false);
			},
			other => {
				let _ = self.ui.handle_key(other);
				if !self.watch.is_empty() {
					self.state.record_event(serde_json::json!({
						"kind": "key",
						"id": null,
						"value": format!("{other:?}"),
					}));
				}
				PanelEvent::Consumed
			},
		}
	}

	fn paste(&mut self, text: &str) -> PanelEvent {
		let _ = self.ui.handle_paste(text);
		PanelEvent::Consumed
	}

	fn frame(&mut self, viewport: Size) -> &Frame {
		let width = match self.anchor {
			PanelAnchor::Center | PanelAnchor::BottomCenter => viewport.width.saturating_mul(80) / 100,
			_ => viewport.width,
		}
		.max(20)
		.min(viewport.width);
		if width != self.width {
			self.width = width;
			self.ui.resize(width);
		}
		self.ui.frame()
	}

	fn tick(&mut self, _now: Duration) -> bool {
		false
	}
}

#[cfg(test)]
mod tests {
	use omp_tui::{Size, UiContext};

	use super::*;

	#[test]
	fn open_escape_closes_as_cancelled() {
		let state = OverlayState::new();
		let mut panel = ExtOverlay::open(
			Str::new_static("ov-1"),
			"<box title='Hi'><text>body</text></box>",
			Size { width: 80, height: 24 },
			&UiContext::default(),
			state.clone(),
			Vec::new(),
			true,
		);
		assert_eq!(panel.key(Key::Esc), PanelEvent::Close);
		assert!(state.closed());
		assert_eq!(
			state.outcome(),
			Some(OverlayOutcome { cancelled: true, reason: Some(Str::new_static("dismissed")) })
		);
	}

	#[test]
	fn enter_settles_without_cancel() {
		let state = OverlayState::new();
		let mut panel = ExtOverlay::open(
			Str::new_static("ov-2"),
			"<text>ok</text>",
			Size { width: 80, height: 24 },
			&UiContext::default(),
			state.clone(),
			Vec::new(),
			true,
		);
		assert_eq!(panel.key(Key::Enter), PanelEvent::Close);
		let outcome = state.outcome().expect("settled");
		assert!(!outcome.cancelled);
	}
}
