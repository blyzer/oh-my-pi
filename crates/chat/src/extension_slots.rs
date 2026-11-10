//! Retained extension slot mounts for the interactive chat host.
//!
//! Extensions emit TML through `omp.ui.mount` / `patch` / `unmount` /
//! `focus_slot` / `blur_slot`. This module owns the observer-local registry
//! keyed by `(extension_id, key)`, sanitizes markup at the extension trust
//! boundary, and exposes placement projections the host paints (status
//! segments, header/footer bands, editor stack bands, and non-modal rails).

use std::collections::BTreeMap;

use omp_core::Str;
use omp_tui::{CellContent, Frame, Prop, PropValue, Size, Ui, UiContext};
use strum::{Display, EnumString, IntoStaticStr};
use xutf::IntoAnsiStripped as _;

/// Default rail width in cells (including the vertical rule), matching the
/// historical `Sidebar::WIDTH` cited by `docs/py/07-ui.md` §4.6.
pub const DEFAULT_RAIL_WIDTH: u16 = 30;
/// Default live-region width below which rails hide.
pub const DEFAULT_RAIL_MIN_WIDTH: u16 = 96;
/// Default viewport height below which rails hide.
pub const DEFAULT_RAIL_MIN_HEIGHT: u16 = 20;

/// Named placement for one extension-owned slot mount.
#[derive(
	Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Display, EnumString, IntoStaticStr,
)]
#[strum(serialize_all = "snake_case", ascii_case_insensitive)]
pub enum SlotPlacement {
	/// Left group of the composer status band.
	StatusLeft,
	/// Right group of the composer status band.
	StatusRight,
	/// Pinned band at the top of the live region.
	Header,
	/// Pinned band below the composer.
	Footer,
	/// Inside the composer stack, above the input.
	AboveEditor,
	/// Inside the composer stack, below the input.
	BelowEditor,
	/// Full-height non-modal rail on the left.
	SidebarLeft,
	/// Full-height non-modal rail on the right.
	SidebarRight,
}

impl SlotPlacement {
	/// Whether this placement is a full-height rail that reserves columns.
	#[must_use]
	pub const fn is_rail(self) -> bool {
		matches!(self, Self::SidebarLeft | Self::SidebarRight)
	}

	/// Whether this placement contributes a status-band segment.
	#[must_use]
	pub const fn is_status(self) -> bool {
		matches!(self, Self::StatusLeft | Self::StatusRight)
	}
}

/// Collapse policy when a mount does not fit the live region.
#[derive(
	Clone,
	Copy,
	Debug,
	Default,
	Eq,
	PartialEq,
	Ord,
	PartialOrd,
	Hash,
	Display,
	EnumString,
	IntoStaticStr,
)]
#[strum(serialize_all = "snake_case", ascii_case_insensitive)]
pub enum SlotCollapse {
	/// Remove the mount entirely.
	#[default]
	Hide,
	/// Keep the mount and clip content.
	Truncate,
	/// Rails only: shrink toward half width before hiding.
	Shrink,
}

/// Layout options carried with one mount.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SlotOptions {
	/// Sort seed among mounts in the same placement (lower first).
	pub order:      i32,
	/// Rail width in cells; ignored outside sidebar placements.
	pub width:      Option<u16>,
	/// Hide when the live region is narrower than this.
	pub min_width:  u16,
	/// Hide when the viewport is shorter than this.
	pub min_height: u16,
	/// Optional row cap for band placements.
	pub max_height: Option<u16>,
	/// Whether a rail may take keyboard focus.
	pub focusable:  bool,
	/// Behavior when space is short.
	pub collapse:   SlotCollapse,
	/// Optional titled box wrapper applied at mount time.
	pub title:      Option<Str>,
}

impl Default for SlotOptions {
	fn default() -> Self {
		Self {
			order:      100,
			width:      None,
			min_width:  0,
			min_height: 0,
			max_height: None,
			focusable:  false,
			collapse:   SlotCollapse::Hide,
			title:      None,
		}
	}
}

impl SlotOptions {
	/// Applies rail defaults when the placement is a sidebar and the
	/// extension left width / min thresholds unset.
	#[must_use]
	pub fn with_rail_defaults(mut self, placement: SlotPlacement) -> Self {
		if placement.is_rail() {
			if self.width.is_none() {
				self.width = Some(DEFAULT_RAIL_WIDTH);
			}
			if self.min_width == 0 {
				self.min_width = DEFAULT_RAIL_MIN_WIDTH;
			}
			if self.min_height == 0 {
				self.min_height = DEFAULT_RAIL_MIN_HEIGHT;
			}
		}
		self
	}
}

/// One typed slot update delivered to the chat actor.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ExtensionSlot {
	/// Insert or replace the mount for `(extension, key)`.
	Mount {
		/// Extension identity that owns this mount.
		extension: Str,
		/// Extension-local mount key.
		key:       Str,
		/// Where the host paints this mount.
		placement: SlotPlacement,
		/// Validated extension-origin TML source.
		source:    Str,
		/// Layout options for this mount.
		options:   SlotOptions,
	},
	/// Remove one mount.
	Unmount {
		/// Extension identity that owns the mount.
		extension: Str,
		/// Extension-local mount key.
		key:       Str,
	},
	/// Retained mutation of one node inside an existing mount.
	Patch {
		/// Extension identity that owns the mount.
		extension: Str,
		/// Extension-local mount key.
		key:       Str,
		/// Target component id inside the retained tree.
		id:        Str,
		/// Replacement text when the node is text-bearing.
		text:      Option<Str>,
		/// Prop name → string value pairs applied through the typed prop table.
		props:     Vec<(Str, Str)>,
	},
	/// Hand keyboard focus to a focusable rail.
	Focus {
		/// Extension identity that owns the mount.
		extension: Str,
		/// Extension-local mount key.
		key:       Str,
	},
	/// Return focus to the composer.
	Blur,
	/// Drop every mount when the actor adopts another session.
	Reset,
}

/// Stable identity for one mount in the registry.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
struct SlotKey {
	extension: Str,
	key:       Str,
}

/// One retained mount after sanitization.
struct MountedSlot {
	placement: SlotPlacement,
	options:   SlotOptions,
	source:    Str,
	ui:        Ui,
	/// Flattened single-line text for status placements and tests.
	text:      Str,
	visible:   bool,
}

/// Columns reserved by visible rails for one viewport.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RailReservation {
	/// Cells reserved on the left edge.
	pub left:  u16,
	/// Cells reserved on the right edge.
	pub right: u16,
}

impl RailReservation {
	/// Total columns reserved from the live region.
	#[must_use]
	pub const fn total(self) -> u16 {
		self.left.saturating_add(self.right)
	}
}

/// One visible mount projection the host paints.
#[derive(Clone)]
pub struct SlotView<'a> {
	/// Extension identity.
	pub extension: &'a Str,
	/// Mount key.
	pub key:       &'a Str,
	/// Placement.
	pub placement: SlotPlacement,
	/// Effective options (rail defaults applied).
	pub options:   &'a SlotOptions,
	/// Retained UI tree.
	pub ui:        &'a Ui,
	/// Flattened text (status placements).
	pub text:      &'a Str,
}

/// Observer-local extension slot registry.
#[derive(Default)]
pub struct ExtensionSlots {
	by_key:  BTreeMap<SlotKey, MountedSlot>,
	focused: Option<SlotKey>,
}

impl ExtensionSlots {
	/// Applies one update. Returns whether presentation facts changed.
	pub fn apply(&mut self, event: ExtensionSlot, ctx: &UiContext) -> bool {
		match event {
			ExtensionSlot::Mount { extension, key, placement, source, options } => {
				let options = options.with_rail_defaults(placement);
				let source = wrap_title(source, options.title.as_ref());
				let Ok((ui, text)) = parse_mount(&source, placement, ctx) else {
					return false;
				};
				let slot_key = SlotKey { extension, key };
				let changed = match self.by_key.get(&slot_key) {
					Some(existing) => {
						existing.placement != placement
							|| existing.source != source
							|| existing.options != options
					},
					None => true,
				};
				self.by_key.insert(slot_key, MountedSlot {
					placement,
					options,
					source,
					ui,
					text,
					visible: true,
				});
				changed
			},
			ExtensionSlot::Unmount { extension, key } => {
				let slot_key = SlotKey { extension, key };
				if self.focused.as_ref() == Some(&slot_key) {
					self.focused = None;
				}
				self.by_key.remove(&slot_key).is_some()
			},
			ExtensionSlot::Patch { extension, key, id, text, props } => {
				let Some(mount) = self.by_key.get_mut(&SlotKey { extension, key }) else {
					return false;
				};
				let mut changed = false;
				if let Some(text) = text {
					changed |= mount.ui.set_text(id.as_str(), text);
				}
				for (name, value) in props {
					let Ok(prop) = name.as_str().parse::<Prop>() else {
						continue;
					};
					changed |= mount.ui.set_prop(id.as_str(), prop, PropValue::from(value));
				}
				if changed && mount.placement.is_status() {
					mount.text = flatten_ui(&mount.ui);
				}
				changed
			},
			ExtensionSlot::Focus { extension, key } => {
				let slot_key = SlotKey { extension, key };
				let Some(mount) = self.by_key.get(&slot_key) else {
					return false;
				};
				if !mount.placement.is_rail() || !mount.options.focusable {
					return false;
				}
				if self.focused.as_ref() == Some(&slot_key) {
					return false;
				}
				self.focused = Some(slot_key);
				true
			},
			ExtensionSlot::Blur => {
				if self.focused.take().is_some() {
					true
				} else {
					false
				}
			},
			ExtensionSlot::Reset => {
				if self.by_key.is_empty() && self.focused.is_none() {
					false
				} else {
					self.by_key.clear();
					self.focused = None;
					true
				}
			},
		}
	}

	/// Visible mounts in a placement, sorted by `(order, extension, key)`.
	pub fn visible(
		&self,
		placement: SlotPlacement,
		viewport: Size,
	) -> impl Iterator<Item = SlotView<'_>> + '_ {
		let mut rows = self
			.by_key
			.iter()
			.filter(|(_, mount)| {
				mount.placement == placement && mount.visible && fits(mount, viewport)
			})
			.map(|(key, mount)| (mount.options.order, key, mount))
			.collect::<Vec<_>>();
		rows.sort_by(|(order_a, key_a, _), (order_b, key_b, _)| {
			order_a
				.cmp(order_b)
				.then_with(|| key_a.extension.cmp(&key_b.extension))
				.then_with(|| key_a.key.cmp(&key_b.key))
		});
		rows.into_iter().map(|(_, key, mount)| SlotView {
			extension: &key.extension,
			key:       &key.key,
			placement: mount.placement,
			options:   &mount.options,
			ui:        &mount.ui,
			text:      &mount.text,
		})
	}

	/// Flattened status-band contributions (left then right, key-sorted within
	/// each side by the same order used for visible mounts).
	#[must_use]
	pub fn status_texts(&self, viewport: Size) -> Vec<Str> {
		let mut texts = Vec::new();
		for placement in [SlotPlacement::StatusLeft, SlotPlacement::StatusRight] {
			for view in self.visible(placement, viewport) {
				if !view.text.is_empty() {
					texts.push(view.text.clone());
				}
			}
		}
		texts
	}

	/// Columns reserved by visible left/right rails for `viewport`.
	#[must_use]
	pub fn reserved(&self, viewport: Size) -> RailReservation {
		let mut left = 0u16;
		let mut right = 0u16;
		for view in self.visible(SlotPlacement::SidebarLeft, viewport) {
			left = left.saturating_add(rail_width(view.options, viewport.width));
		}
		for view in self.visible(SlotPlacement::SidebarRight, viewport) {
			right = right.saturating_add(rail_width(view.options, viewport.width));
		}
		RailReservation { left, right }
	}

	/// Whether any mount is currently retained.
	#[must_use]
	pub fn is_empty(&self) -> bool {
		self.by_key.is_empty()
	}

	/// Focused rail identity, when one owns keyboard focus.
	#[must_use]
	pub fn focused(&self) -> Option<(&Str, &Str)> {
		self.focused.as_ref().map(|key| (&key.extension, &key.key))
	}

	/// Relays each visible mount in `placement` at `viewport` width and
	/// returns a stacked frame, or `None` when nothing is visible.
	pub fn frame(&mut self, placement: SlotPlacement, viewport: Size) -> Option<Frame> {
		let keys = self
			.by_key
			.iter()
			.filter(|(_, mount)| {
				mount.placement == placement && mount.visible && fits(mount, viewport)
			})
			.map(|(key, mount)| {
				(
					mount.options.order,
					key.clone(),
					mount.options.width,
					mount.options.max_height,
					mount.options.collapse,
					mount.options.min_width,
				)
			})
			.collect::<Vec<_>>();
		if keys.is_empty() {
			return None;
		}
		let mut ordered = keys;
		ordered.sort_by(|a, b| {
			a.0.cmp(&b.0)
				.then_with(|| a.1.extension.cmp(&b.1.extension))
				.then_with(|| a.1.key.cmp(&b.1.key))
		});
		if placement.is_rail() {
			let width = ordered
				.iter()
				.map(|(_, _, width, _, collapse, min_width)| {
					let desired = width.unwrap_or(DEFAULT_RAIL_WIDTH);
					match collapse {
						SlotCollapse::Shrink if viewport.width < *min_width => {
							desired.saturating_div(2).max(1).min(viewport.width)
						},
						_ => desired.min(viewport.width),
					}
				})
				.max()
				.unwrap_or(DEFAULT_RAIL_WIDTH)
				.min(viewport.width);
			let mut frame = Frame::new(Size::new(width, viewport.height));
			let mut y = 0u16;
			for (_, key, ..) in &ordered {
				let Some(mount) = self.by_key.get_mut(key) else {
					continue;
				};
				mount.ui.resize(width);
				let rendered = mount.ui.frame();
				let rows = rendered
					.size()
					.height
					.min(viewport.height.saturating_sub(y));
				if rows == 0 {
					break;
				}
				frame.blit(rendered, 0, rows, 0, y);
				y = y.saturating_add(rows);
			}
			return Some(frame);
		}
		let width = viewport.width;
		let mut parts = Vec::new();
		for (_, key, _, max_height, ..) in &ordered {
			let Some(mount) = self.by_key.get_mut(key) else {
				continue;
			};
			mount.ui.resize(width);
			let rendered = mount.ui.frame().clone();
			let rows = match *max_height {
				Some(max) => rendered.size().height.min(max),
				None => rendered.size().height,
			};
			parts.push((rendered, rows));
		}
		let height = parts
			.iter()
			.map(|(_, rows)| *rows)
			.fold(0u16, u16::saturating_add);
		if height == 0 {
			return None;
		}
		let mut frame = Frame::new(Size::new(width, height));
		let mut y = 0u16;
		for (rendered, rows) in parts {
			frame.blit(&rendered, 0, rows, 0, y);
			y = y.saturating_add(rows);
		}
		Some(frame)
	}
}

fn fits(mount: &MountedSlot, viewport: Size) -> bool {
	let too_narrow = viewport.width < mount.options.min_width;
	let too_short = viewport.height < mount.options.min_height;
	if !(too_narrow || too_short) {
		return true;
	}
	match mount.options.collapse {
		SlotCollapse::Truncate => true,
		SlotCollapse::Shrink if mount.placement.is_rail() && !too_short => true,
		SlotCollapse::Hide | SlotCollapse::Shrink => false,
	}
}

fn rail_width(options: &SlotOptions, viewport_width: u16) -> u16 {
	let desired = options.width.unwrap_or(DEFAULT_RAIL_WIDTH);
	match options.collapse {
		SlotCollapse::Shrink if viewport_width < options.min_width => {
			desired.saturating_div(2).max(1).min(viewport_width)
		},
		_ => desired.min(viewport_width),
	}
}

fn wrap_title(source: Str, title: Option<&Str>) -> Str {
	match title {
		Some(title) if !title.is_empty() => {
			Str::new(format!("<box border=round title=\"{}\">{source}</box>", escape_attr(title)))
		},
		_ => source,
	}
}

fn escape_attr(value: &str) -> String {
	value.replace('&', "&amp;").replace('"', "&quot;")
}

fn parse_mount(
	source: &str,
	placement: SlotPlacement,
	ctx: &UiContext,
) -> Result<(Ui, Str), omp_tui::ParseError> {
	let wrapped = if placement.is_status() && looks_like_segment_fragment(source) {
		Str::new(format!("<status>{source}</status>"))
	} else {
		Str::new(source)
	};
	let width = wrapped.len().clamp(1, usize::from(u16::MAX)) as u16;
	let ui = Ui::from_extension_markup(wrapped.as_str(), width, ctx.clone())?;
	let text = flatten_ui(&ui);
	Ok((ui, text))
}

fn looks_like_segment_fragment(source: &str) -> bool {
	let trimmed = source.trim_start();
	trimmed.starts_with("<segment") || !trimmed.starts_with('<')
}

fn flatten_ui(ui: &Ui) -> Str {
	let frame = ui.frame();
	let mut text = String::new();
	for y in 0..frame.size().height {
		let row_start = text.len();
		for x in 0..frame.size().width {
			match frame.cell(x, y).content() {
				CellContent::Blank => text.push(' '),
				CellContent::Grapheme { text: glyph, .. } => text.push_str(glyph),
				CellContent::Continuation | CellContent::Image { .. } => {},
			}
		}
		let row_end = text.trim_end_matches(' ').len();
		text.truncate(row_end);
		if text.len() > row_start {
			text.push(' ');
		}
	}
	sanitize_line(text.trim_end())
}

fn sanitize_line(value: &str) -> Str {
	let stripped = value.to_owned().into_ansi_stripped();
	let mut clean = String::with_capacity(stripped.len());
	let mut ascii_space = true;
	for ch in stripped.chars() {
		let ch = if ch.is_control() { ' ' } else { ch };
		if ch == ' ' {
			if !ascii_space {
				clean.push(' ');
			}
			ascii_space = true;
		} else {
			clean.push(ch);
			ascii_space = false;
		}
	}
	if clean.ends_with(' ') {
		clean.pop();
	}
	Str::new(clean)
}

/// Parses a mount effect body into a typed [`ExtensionSlot::Mount`].
pub fn mount_from_effect(
	extension: &str,
	body: &serde_json::Map<String, serde_json::Value>,
) -> Result<ExtensionSlot, Str> {
	let key = body
		.get("key")
		.and_then(serde_json::Value::as_str)
		.filter(|key| !key.is_empty())
		.ok_or_else(|| Str::new_static("mount requires a non-empty body.key"))?;
	let placement = body
		.get("placement")
		.and_then(serde_json::Value::as_str)
		.ok_or_else(|| Str::new_static("mount requires body.placement"))?
		.parse::<SlotPlacement>()
		.map_err(|_| Str::new_static("mount body.placement is not a known slot"))?;
	let source = body
		.get("content")
		.and_then(|content| content.get("source"))
		.and_then(serde_json::Value::as_str)
		.ok_or_else(|| Str::new_static("mount body.content.source must be a string"))?;
	let options = options_from_value(body.get("options")).unwrap_or_default();
	// Reject at the bridge before the actor sees it (mirrors set_status).
	let _ = parse_mount(source, placement, &UiContext::default())
		.map_err(|_| Str::new_static("mount body.content.source is invalid TML"))?;
	Ok(ExtensionSlot::Mount {
		extension: Str::new(extension),
		key: Str::new(key),
		placement,
		source: Str::new(source),
		options,
	})
}

/// Parses a patch effect body into a typed [`ExtensionSlot::Patch`].
pub fn patch_from_effect(
	extension: &str,
	body: &serde_json::Map<String, serde_json::Value>,
) -> Result<ExtensionSlot, Str> {
	let key = body
		.get("key")
		.and_then(serde_json::Value::as_str)
		.filter(|key| !key.is_empty())
		.ok_or_else(|| Str::new_static("patch requires a non-empty body.key"))?;
	let id = body
		.get("id")
		.and_then(serde_json::Value::as_str)
		.filter(|id| !id.is_empty())
		.ok_or_else(|| Str::new_static("patch requires a non-empty body.id"))?;
	let text = match body.get("text") {
		None | Some(serde_json::Value::Null) => None,
		Some(serde_json::Value::String(text)) => Some(Str::new(text.as_str())),
		Some(serde_json::Value::Object(object)) => object
			.get("source")
			.and_then(serde_json::Value::as_str)
			.map(|source| {
				// `<text>…</text>` or bare text — strip a trivial text wrapper.
				let trimmed = source.trim();
				if let Some(inner) = trimmed
					.strip_prefix("<text>")
					.and_then(|rest| rest.strip_suffix("</text>"))
				{
					Str::new(inner)
				} else {
					Str::new(source)
				}
			}),
		_ => return Err(Str::new_static("patch body.text must be a string, Tml object, or null")),
	};
	let props = match body.get("props") {
		None | Some(serde_json::Value::Null) => Vec::new(),
		Some(serde_json::Value::Object(object)) => object
			.iter()
			.filter_map(|(name, value)| {
				value
					.as_str()
					.map(|text| (Str::new(name.as_str()), Str::new(text)))
			})
			.collect(),
		_ => return Err(Str::new_static("patch body.props must be an object")),
	};
	Ok(ExtensionSlot::Patch {
		extension: Str::new(extension),
		key: Str::new(key),
		id: Str::new(id),
		text,
		props,
	})
}

fn options_from_value(value: Option<&serde_json::Value>) -> Option<SlotOptions> {
	let object = value?.as_object()?;
	let mut options = SlotOptions::default();
	if let Some(order) = object.get("order").and_then(serde_json::Value::as_i64) {
		options.order = i32::try_from(order).unwrap_or(options.order);
	}
	if let Some(width) = object.get("width").and_then(serde_json::Value::as_u64) {
		options.width = Some(u16::try_from(width).unwrap_or(u16::MAX));
	}
	if let Some(min_width) = object.get("min_width").and_then(serde_json::Value::as_u64) {
		options.min_width = u16::try_from(min_width).unwrap_or(u16::MAX);
	}
	if let Some(min_height) = object.get("min_height").and_then(serde_json::Value::as_u64) {
		options.min_height = u16::try_from(min_height).unwrap_or(u16::MAX);
	}
	if let Some(max_height) = object.get("max_height").and_then(serde_json::Value::as_u64) {
		options.max_height = Some(u16::try_from(max_height).unwrap_or(u16::MAX));
	}
	if let Some(focusable) = object.get("focusable").and_then(serde_json::Value::as_bool) {
		options.focusable = focusable;
	}
	if let Some(collapse) = object
		.get("collapse")
		.and_then(serde_json::Value::as_str)
		.and_then(|value| value.parse().ok())
	{
		options.collapse = collapse;
	}
	if let Some(title) = object.get("title").and_then(serde_json::Value::as_str) {
		options.title = Some(Str::new(title));
	}
	Some(options)
}

#[cfg(test)]
mod tests {
	use omp_core::Str;
	use omp_tui::{Size, UiContext};

	use super::*;

	fn viewport() -> Size {
		Size { width: 120, height: 40 }
	}

	fn mount(placement: SlotPlacement, key: &str, source: &str) -> ExtensionSlot {
		ExtensionSlot::Mount {
			extension: Str::new_static("demo"),
			key: Str::new(key),
			placement,
			source: Str::new(source),
			options: SlotOptions::default(),
		}
	}

	#[test]
	fn mount_is_visible_and_remount_replaces() {
		let mut slots = ExtensionSlots::default();
		let ctx = UiContext::default();
		assert!(slots.apply(mount(SlotPlacement::Header, "welcome", "<text>hello</text>"), &ctx,));
		let texts = slots
			.visible(SlotPlacement::Header, viewport())
			.map(|view| view.text.as_str().to_owned())
			.collect::<Vec<_>>();
		assert_eq!(texts, ["hello"]);
		assert!(slots.apply(mount(SlotPlacement::Header, "welcome", "<text>replaced</text>"), &ctx,));
		let texts = slots
			.visible(SlotPlacement::Header, viewport())
			.map(|view| view.text.as_str().to_owned())
			.collect::<Vec<_>>();
		assert_eq!(texts, ["replaced"]);
	}

	#[test]
	fn unmount_clears_and_invalid_tml_is_rejected_at_parse() {
		let mut slots = ExtensionSlots::default();
		let ctx = UiContext::default();
		slots.apply(mount(SlotPlacement::Footer, "f", "<text>x</text>"), &ctx);
		assert!(slots.apply(
			ExtensionSlot::Unmount {
				extension: Str::new_static("demo"),
				key:       Str::new_static("f"),
			},
			&ctx,
		));
		assert!(slots.is_empty());
		assert!(
			parse_mount(
				"<md><button id=unsafe when=active>interactive</button></md>",
				SlotPlacement::Header,
				&ctx,
			)
			.is_err()
		);
	}

	#[test]
	fn status_left_and_right_flatten_cleanly() {
		let mut slots = ExtensionSlots::default();
		let ctx = UiContext::default();
		assert!(slots.apply(
			mount(
				SlotPlacement::StatusLeft,
				"build",
				"<row><text fg=error>failed</text><text> safely</text></row>",
			),
			&ctx,
		));
		assert!(slots.apply(mount(SlotPlacement::StatusRight, "hook", "<text>ready</text>"), &ctx,));
		assert_eq!(
			slots
				.status_texts(viewport())
				.iter()
				.map(Str::as_str)
				.collect::<Vec<_>>(),
			["failed safely", "ready"]
		);
	}

	#[test]
	fn rails_reserve_columns_and_hide_under_min_width() {
		let mut slots = ExtensionSlots::default();
		let ctx = UiContext::default();
		let mut options =
			SlotOptions { width: Some(34), min_width: 120, min_height: 20, ..SlotOptions::default() };
		assert!(slots.apply(
			ExtensionSlot::Mount {
				extension: Str::new_static("demo"),
				key:       Str::new_static("rail"),
				placement: SlotPlacement::SidebarRight,
				source:    Str::new_static("<text>rail</text>"),
				options:   options.clone(),
			},
			&ctx,
		));
		assert_eq!(slots.reserved(Size { width: 130, height: 40 }), RailReservation {
			left:  0,
			right: 34,
		});
		assert_eq!(
			slots.reserved(Size { width: 80, height: 40 }).total(),
			0,
			"rails hide below min_width"
		);
		options.collapse = SlotCollapse::Shrink;
		slots.apply(
			ExtensionSlot::Mount {
				extension: Str::new_static("demo"),
				key: Str::new_static("rail"),
				placement: SlotPlacement::SidebarRight,
				source: Str::new_static("<text>rail</text>"),
				options,
			},
			&ctx,
		);
		assert_eq!(
			slots.reserved(Size { width: 80, height: 40 }).right,
			17,
			"shrink keeps a half-width rail"
		);
	}

	#[test]
	fn focus_requires_focusable_rail() {
		let mut slots = ExtensionSlots::default();
		let ctx = UiContext::default();
		slots.apply(
			ExtensionSlot::Mount {
				extension: Str::new_static("demo"),
				key:       Str::new_static("rail"),
				placement: SlotPlacement::SidebarLeft,
				source:    Str::new_static("<text>rail</text>"),
				options:   SlotOptions { focusable: true, ..SlotOptions::default() },
			},
			&ctx,
		);
		assert!(slots.apply(
			ExtensionSlot::Focus {
				extension: Str::new_static("demo"),
				key:       Str::new_static("rail"),
			},
			&ctx,
		));
		assert_eq!(slots.focused().map(|(e, k)| (e.as_str(), k.as_str())), Some(("demo", "rail")));
		assert!(slots.apply(ExtensionSlot::Blur, &ctx));
		assert!(slots.focused().is_none());
	}
}
