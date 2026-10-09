//! Typed TSP messages: verbs, the node vocabulary, frame ops, surface
//! lifecycle, queries, replies and events, as the Tern Surface Protocol v1
//! puts them on the wire.

use std::fmt;

use omp_core::Str;
use serde::{
	Deserialize, Deserializer, Serialize, Serializer,
	de::{self, SeqAccess, Visitor},
	ser::SerializeSeq,
};
use serde_json::{Map, Value};
use smallvec::SmallVec;
use strum::{EnumString, IntoStaticStr, VariantNames};

use super::{DEFAULT_APC_LIMIT, DEFAULT_CREDITS};

/// One-letter message verb.
#[derive(Clone, Copy, Debug, EnumString, Eq, Hash, IntoStaticStr, PartialEq)]
pub enum Verb {
	/// Program → terminal: a query (`hello`, `blobs`).
	#[strum(serialize = "q")]
	Query,
	/// Program → terminal: open a surface.
	#[strum(serialize = "o")]
	Open,
	/// Program → terminal: an atomic batch of document ops.
	#[strum(serialize = "f")]
	Frame,
	/// Program → terminal: a content-addressed blob.
	#[strum(serialize = "b")]
	Blob,
	/// Program → terminal: the program palette.
	#[strum(serialize = "t")]
	Palette,
	/// Program → terminal: a stylesheet.
	#[strum(serialize = "s")]
	Stylesheet,
	/// Program → terminal: close a surface.
	#[strum(serialize = "x")]
	Close,
	/// Terminal → program: the reply to a query.
	#[strum(serialize = "r")]
	Reply,
	/// Terminal → program: an event.
	#[strum(serialize = "e")]
	Event,
}

/// Every node kind of the v1 vocabulary.
#[derive(
	Clone,
	Copy,
	Debug,
	Deserialize,
	EnumString,
	Eq,
	Hash,
	IntoStaticStr,
	PartialEq,
	Serialize,
	strum::VariantArray,
)]
#[serde(rename_all = "lowercase")]
#[strum(serialize_all = "lowercase")]
#[allow(missing_docs, reason = "each variant is the wire kind of the same name")]
pub enum Kind {
	Col,
	Row,
	Card,
	Section,
	Rule,
	Spacer,
	Text,
	Md,
	Code,
	Diff,
	Ansi,
	Math,
	Image,
	Kv,
	Table,
	Tree,
	Badge,
	Kbd,
	Icon,
	Spinner,
	Shimmer,
	Elapsed,
	Progress,
	Rate,
	List,
	Item,
	Tabs,
	Editor,
	Input,
	Status,
	Seg,
	Overlay,
	Toast,
	Rows,
	Picker,
	Prefs,
	Tool,
	Checklist,
	Agent,
	Chart,
	Meter,
	Block,
	Effort,
	El,
}

impl Kind {
	/// Whether the kind keeps its primary text in the `text` prop, which the
	/// `text` and `splice` ops address.
	#[must_use]
	pub const fn has_primary_text(self) -> bool {
		matches!(
			self,
			Self::Text
				| Self::Md
				| Self::Code
				| Self::Ansi
				| Self::Math
				| Self::Editor
				| Self::Input
				| Self::Shimmer
				| Self::El
		)
	}
}

/// A node on the wire: `{id, k, p?, c?}`.
///
/// Props stay a JSON object here: the vocabulary has 44 kinds, each with its
/// own props beside the common ones, and Tern ignores fields it does not
/// know. Typed per-kind builders produce these maps; the document model only
/// stores and merges them.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct Node {
	/// Identity, unique within the surface.
	pub id: Str,
	/// Kind from the vocabulary.
	pub k:  Kind,
	/// Props; a `null` value means absent.
	#[serde(default, skip_serializing_if = "Map::is_empty")]
	pub p:  Map<String, Value>,
	/// Children, in order.
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub c:  Vec<Self>,
}

/// How a `text` op changes the primary text.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum TextMode {
	/// Append to the text (the streaming path).
	Append,
	/// Replace the text.
	Replace,
}

/// Where a `reveal` op scrolls a node to.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Reveal {
	/// Top of the scroller.
	Start,
	/// Bottom of the scroller.
	End,
	/// Whichever edge is nearer.
	Nearest,
}

/// How far a `scroll` op moves the scroller holding a node.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
#[allow(missing_docs, reason = "each variant is the wire step of the same name")]
pub enum ScrollBy {
	LineUp,
	LineDown,
	PageUp,
	PageDown,
	Start,
	End,
}

/// One frame op, encoded as a JSON array whose first element names it.
#[derive(Clone, Debug, IntoStaticStr, PartialEq, VariantNames)]
#[strum(serialize_all = "lowercase")]
pub enum Op {
	/// `["add", id, parent, before|null, node]`.
	Add {
		/// Parent node id.
		parent: Str,
		/// Sibling to insert before, or last.
		before: Option<Str>,
		/// The subtree; its `id` is the op's id.
		node:   Node,
	},
	/// `["set", id, props]`: shallow merge, `null` deletes.
	Set {
		/// Target id.
		id:    Str,
		/// Props to merge.
		props: Map<String, Value>,
	},
	/// `["text", id, mode, text]`.
	Text {
		/// Target id.
		id:   Str,
		/// Append or replace.
		mode: TextMode,
		/// The text.
		text: Str,
	},
	/// `["splice", id, at, del, text]` in UTF-16 code units.
	Splice {
		/// Target id.
		id:   Str,
		/// Start offset.
		at:   u32,
		/// Units removed.
		del:  u32,
		/// Inserted text.
		text: Str,
	},
	/// `["move", id, parent, before|null]`.
	Move {
		/// Node to move.
		id:     Str,
		/// New parent.
		parent: Str,
		/// Sibling to move before, or last.
		before: Option<Str>,
	},
	/// `["del", id]`.
	Del(Str),
	/// `["settle", id]`.
	Settle(Str),
	/// `["focus", id|null]`.
	Focus(Option<Str>),
	/// `["reveal", id, where]`.
	Reveal(Str, Reveal),
	/// `["scroll", id, by]`.
	Scroll(Str, ScrollBy),
	/// `["suspend"]`.
	Suspend,
	/// `["resume"]`.
	Resume,
}

impl Op {
	/// The op's wire name.
	#[must_use]
	pub fn name(&self) -> &'static str {
		self.into()
	}
}

impl Serialize for Op {
	fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
		let len = match self {
			Self::Add { .. } | Self::Splice { .. } => 5,
			Self::Text { .. } | Self::Move { .. } => 4,
			Self::Set { .. } | Self::Reveal(..) | Self::Scroll(..) => 3,
			Self::Del(_) | Self::Settle(_) | Self::Focus(_) => 2,
			Self::Suspend | Self::Resume => 1,
		};
		let mut seq = serializer.serialize_seq(Some(len))?;
		seq.serialize_element(self.name())?;
		match self {
			Self::Add { parent, before, node } => {
				seq.serialize_element(&node.id)?;
				seq.serialize_element(parent)?;
				seq.serialize_element(before)?;
				seq.serialize_element(node)?;
			},
			Self::Set { id, props } => {
				seq.serialize_element(id)?;
				seq.serialize_element(props)?;
			},
			Self::Text { id, mode, text } => {
				seq.serialize_element(id)?;
				seq.serialize_element(mode)?;
				seq.serialize_element(text)?;
			},
			Self::Splice { id, at, del, text } => {
				seq.serialize_element(id)?;
				seq.serialize_element(at)?;
				seq.serialize_element(del)?;
				seq.serialize_element(text)?;
			},
			Self::Move { id, parent, before } => {
				seq.serialize_element(id)?;
				seq.serialize_element(parent)?;
				seq.serialize_element(before)?;
			},
			Self::Del(id) | Self::Settle(id) => seq.serialize_element(id)?,
			Self::Focus(id) => seq.serialize_element(id)?,
			Self::Reveal(id, at) => {
				seq.serialize_element(id)?;
				seq.serialize_element(at)?;
			},
			Self::Scroll(id, by) => {
				seq.serialize_element(id)?;
				seq.serialize_element(by)?;
			},
			Self::Suspend | Self::Resume => {},
		}
		seq.end()
	}
}

impl<'de> Deserialize<'de> for Op {
	fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
		deserializer.deserialize_seq(OpVisitor)
	}
}

struct OpVisitor;

fn element<'de, T: Deserialize<'de>, A: SeqAccess<'de>>(
	seq: &mut A,
	index: usize,
) -> Result<T, A::Error> {
	seq.next_element()?
		.ok_or_else(|| de::Error::invalid_length(index, &"a complete op"))
}

impl<'de> Visitor<'de> for OpVisitor {
	type Value = Op;

	fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
		formatter.write_str("a TSP op array")
	}

	fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Op, A::Error> {
		let name: Str = element(&mut seq, 0)?;
		let op = match name.as_str() {
			"add" => {
				let id: Str = element(&mut seq, 1)?;
				let parent = element(&mut seq, 2)?;
				let before = element(&mut seq, 3)?;
				let node: Node = element(&mut seq, 4)?;
				if node.id != id {
					return Err(de::Error::custom("add id does not match the node id"));
				}
				Op::Add { parent, before, node }
			},
			"set" => Op::Set { id: element(&mut seq, 1)?, props: element(&mut seq, 2)? },
			"text" => Op::Text {
				id:   element(&mut seq, 1)?,
				mode: element(&mut seq, 2)?,
				text: element(&mut seq, 3)?,
			},
			"splice" => Op::Splice {
				id:   element(&mut seq, 1)?,
				at:   element(&mut seq, 2)?,
				del:  element(&mut seq, 3)?,
				text: element(&mut seq, 4)?,
			},
			"move" => Op::Move {
				id:     element(&mut seq, 1)?,
				parent: element(&mut seq, 2)?,
				before: element(&mut seq, 3)?,
			},
			"del" => Op::Del(element(&mut seq, 1)?),
			"settle" => Op::Settle(element(&mut seq, 1)?),
			"focus" => Op::Focus(element(&mut seq, 1)?),
			"reveal" => Op::Reveal(element(&mut seq, 1)?, element(&mut seq, 2)?),
			"scroll" => Op::Scroll(element(&mut seq, 1)?, element(&mut seq, 2)?),
			"suspend" => Op::Suspend,
			"resume" => Op::Resume,
			other => return Err(de::Error::unknown_variant(other, Op::VARIANTS)),
		};
		if seq.next_element::<de::IgnoredAny>()?.is_some() {
			return Err(de::Error::custom("trailing op arguments"));
		}
		Ok(op)
	}
}

/// Verb `f`: an atomic batch of ops for one surface.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct Frame {
	/// Surface id.
	pub sf:  Str,
	/// Per-surface sequence number, echoed by `ack`.
	pub s:   u64,
	/// The ops, applied in order.
	pub ops: Vec<Op>,
}

/// How a surface sits in the pane.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
	/// At the cursor row, growing with the program's output; the program's
	/// session view.
	Inline,
	/// Covers the pane, like the alternate screen.
	Screen,
	/// Command output that stays in the scrollback.
	Flow,
}

/// Verb `o`: open (or adopt) a surface.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Open {
	/// Surface id.
	pub id:     Str,
	/// Placement.
	pub mode:   Mode,
	/// Tab title while the surface is live.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub title:  Option<Str>,
	/// Semantic role, such as `omp.session`.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub role:   Option<Str>,
	/// Reopen a surface closed with `keep:true` under the same id.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub adopt:  Option<bool>,
	/// `false`: the surface never hears events or acks.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub listen: Option<bool>,
}

/// Verb `x`: close a surface.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Close {
	/// Surface id.
	pub id:   Str,
	/// Keep `main` in the scrollback (true) or remove the surface (false).
	pub keep: bool,
}

/// Verb `q`.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "q", rename_all = "lowercase")]
pub enum Query {
	/// Detection and capability negotiation.
	Hello {
		/// Protocol versions the program speaks.
		v:        SmallVec<u32, 2>,
		/// Program name, shown as the pane's program.
		app:      Str,
		/// Program version.
		#[serde(default, skip_serializing_if = "Option::is_none")]
		ver:      Option<Str>,
		/// Program features beyond v1 (`edit`, `undo`, `send`).
		#[serde(default, skip_serializing_if = "Vec::is_empty")]
		features: Vec<Str>,
	},
	/// Which of these blobs the terminal already holds.
	Blobs {
		/// SHA-256 hex ids.
		ids: Vec<Str>,
	},
}

/// A cell size in pixels.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Cell {
	/// Width.
	pub w: u32,
	/// Height.
	pub h: u32,
}

/// The terminal's `hello` reply.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Hello {
	/// Version picked from the query's list.
	pub v:             u32,
	/// Terminal name (`tern`).
	pub term:          Str,
	/// Terminal version.
	#[serde(default)]
	pub ver:           Option<Str>,
	/// Every kind it draws, by wire name; unknown names are kept as text.
	pub kinds:         Vec<Str>,
	/// Terminal features (`blobs`, `settle`, `adopt`, `dock`, `flow`, ...).
	#[serde(default)]
	pub features:      Vec<Str>,
	/// Largest body per message before chunking.
	#[serde(default)]
	pub apc:           Option<u32>,
	/// Frames the program may have unacknowledged.
	#[serde(default)]
	pub credits:       Option<u32>,
	/// Pane width in columns.
	#[serde(default)]
	pub cols:          Option<u32>,
	/// Cell size in pixels.
	#[serde(default)]
	pub cell:          Option<Cell>,
	/// Dark appearance.
	#[serde(default)]
	pub dark:          Option<bool>,
	/// Reduce Motion is on.
	#[serde(default)]
	pub reduce_motion: Option<bool>,
	/// The user's clock reads 12-hour.
	#[serde(default)]
	pub hour12:        Option<bool>,
}

impl Hello {
	/// Whether the terminal draws `kind`.
	#[must_use]
	pub fn draws(&self, kind: Kind) -> bool {
		let name: &'static str = kind.into();
		self.kinds.iter().any(|known| known.as_str() == name)
	}

	/// Whether the terminal advertises `feature`.
	#[must_use]
	pub fn has_feature(&self, feature: &str) -> bool {
		self.features.iter().any(|known| known.as_str() == feature)
	}

	/// The `apc` limit, defaulted.
	#[must_use]
	pub fn apc_limit(&self) -> usize {
		self.apc.map_or(DEFAULT_APC_LIMIT, |limit| limit as usize)
	}

	/// The `credits`, defaulted.
	#[must_use]
	pub fn credit_limit(&self) -> u32 {
		self.credits.unwrap_or(DEFAULT_CREDITS)
	}
}

/// Verb `r`.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "r", rename_all = "lowercase")]
pub enum Reply {
	/// The `hello` reply.
	Hello(Hello),
	/// The `blobs` reply.
	Blobs {
		/// The asked ids the terminal holds, in asking order.
		have: Vec<Str>,
	},
}

/// Verb `e`: a terminal → program event. Unknown events decode as
/// [`Event::Unknown`]; unknown fields are ignored.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "ev", rename_all = "lowercase")]
pub enum Event {
	/// A frame was applied and drawn.
	Ack {
		/// Surface id.
		sf: Str,
		/// Highest frame drawn.
		s:  u64,
	},
	/// The live surface was first drawn, or its width or cell size changed.
	Resize {
		/// Surface id.
		#[serde(default)]
		sf:      Option<Str>,
		/// Width in columns.
		cols:    u32,
		/// Cell size.
		#[serde(default)]
		cell:    Option<Cell>,
		/// Whether the pane is visible.
		#[serde(default)]
		visible: Option<bool>,
	},
	/// The appearance switched.
	Theme {
		/// Dark appearance.
		dark: bool,
	},
	/// Reduce Motion was toggled.
	Motion {
		/// Reduce Motion is on.
		reduce: bool,
	},
	/// The pane was hidden or shown.
	Visible {
		/// Surface id.
		#[serde(default)]
		sf:      Option<Str>,
		/// Whether it is visible now.
		visible: bool,
	},
	/// A node was folded or unfolded.
	Toggle {
		/// Surface id.
		sf:        Str,
		/// Node id.
		id:        Str,
		/// The node's `key` prop.
		#[serde(default)]
		key:       Option<Str>,
		/// Folded now.
		collapsed: bool,
	},
	/// An item was selected.
	Select {
		/// Surface id.
		sf:     Str,
		/// Node (or list) id.
		id:     Str,
		/// Item id.
		item:   Str,
		/// Form values, inside an `el` form.
		#[serde(default)]
		values: Option<Map<String, Value>>,
	},
	/// An item was activated.
	Activate {
		/// Surface id.
		sf:     Str,
		/// Node (or list) id.
		id:     Str,
		/// Item id.
		item:   Str,
		/// Form values, inside an `el` form.
		#[serde(default)]
		values: Option<Map<String, Value>>,
	},
	/// A program-defined action ran.
	Action {
		/// Surface id.
		sf:     Str,
		/// Node id.
		id:     Str,
		/// Action name.
		act:    Str,
		/// The part after `=` in `name=value`.
		#[serde(default)]
		value:  Option<Str>,
		/// Held modifiers.
		#[serde(default)]
		mods:   Vec<Str>,
		/// Form values, inside an `el` form.
		#[serde(default)]
		values: Option<Map<String, Value>>,
	},
	/// A control's value changed.
	Change {
		/// Surface id.
		sf:      Str,
		/// Node id.
		id:      Str,
		/// `prefs` row.
		#[serde(default)]
		item:    Option<Str>,
		/// New value (`null` resets to the default).
		#[serde(default)]
		value:   Value,
		/// Checked state of an `el` control.
		#[serde(default)]
		checked: Option<bool>,
		/// Control name.
		#[serde(default)]
		name:    Option<Str>,
		/// Form values, inside an `el` form.
		#[serde(default)]
		values:  Option<Map<String, Value>>,
	},
	/// A native edit over the terminal's selection (UTF-16 offsets).
	Edit {
		/// Surface id.
		sf:     Str,
		/// Field id.
		id:     Str,
		/// Replaced range start.
		from:   u32,
		/// Replaced range end.
		to:     u32,
		/// Inserted text.
		text:   Str,
		/// Caret afterwards.
		cursor: u32,
		/// Text length the terminal saw; a mismatch makes the edit stale.
		len:    u32,
	},
	/// Undo the last change to a field.
	Undo {
		/// Surface id.
		sf: Str,
		/// Field id.
		id: Str,
	},
	/// Submit text through a composer.
	Send {
		/// Surface id.
		sf:   Str,
		/// Composer id.
		id:   Str,
		/// The prompt.
		text: Str,
	},
	/// A click asked for the keys.
	Focus {
		/// Surface id.
		sf: Str,
		/// Field id.
		id: Str,
	},
	/// A rejected op, message or dropped chunk.
	Error {
		/// Surface id.
		#[serde(default)]
		sf:  Option<Str>,
		/// Frame sequence number.
		#[serde(default)]
		s:   Option<u64>,
		/// Op index in the frame.
		#[serde(default)]
		op:  Option<u32>,
		/// Description.
		msg: Str,
	},
	/// Nodes (or a surface) the terminal dropped.
	Gone {
		/// Surface id.
		#[serde(default)]
		sf:  Option<Str>,
		/// Dropped ids.
		ids: Vec<Str>,
	},
	/// An event this build does not know.
	#[serde(other)]
	Unknown,
}
