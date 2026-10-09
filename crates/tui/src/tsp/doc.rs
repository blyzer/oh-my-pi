//! Reference document applier for TSP frames.
//!
//! It applies frames to a node tree the way the specification says the
//! terminal does. Ops apply one by one; a rejected op is reported with its
//! index and skipped, and the rest of the frame still applies. `settle` is
//! recorded as a hint and restricts no later op.
//!
//! This is the test oracle for everything that produces frames: a presenter
//! is correct when the document its frames build here is the one it meant.

use std::ops;

use omp_core::{FastHashMap, FastHashSet, Str};
use serde_json::{Map, Value};
use thiserror::Error;

use super::wire::{Frame, Kind, Node, Op, TextMode};

/// Why the terminal would reject one op.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum OpError {
	/// The op names an id the document does not hold.
	#[error("unknown id {id}")]
	UnknownId {
		/// The id.
		id: Str,
	},
	/// An added subtree repeats an id already in the document or in itself.
	#[error("duplicate id {id}")]
	DuplicateId {
		/// The id.
		id: Str,
	},
	/// A node in an added subtree has an empty id.
	#[error("node without id")]
	EmptyId,
	/// A `text` or `splice` op targets a kind without primary text.
	#[error("{id} ({kind}) has no primary text")]
	NoPrimaryText {
		/// The node.
		id:   Str,
		/// Its kind.
		kind: &'static str,
	},
	/// A `splice` range falls outside the text or splits a surrogate pair.
	#[error("splice {at}+{del} outside text of {len} UTF-16 units or inside a surrogate pair")]
	SpliceRange {
		/// Start offset.
		at:  u32,
		/// Removed units.
		del: u32,
		/// Text length in UTF-16 units.
		len: usize,
	},
	/// `before` is not a child of the target parent.
	#[error("{before} is not a child of {parent}")]
	NotAChild {
		/// The sibling named.
		before: Str,
		/// The parent named.
		parent: Str,
	},
	/// The root is neither moved nor deleted.
	#[error("the surface root cannot be moved or deleted")]
	Root,
	/// A move would put a node inside its own subtree.
	#[error("cannot move {id} into its own subtree")]
	IntoOwnSubtree {
		/// The node moved.
		id: Str,
	},
	/// A move names the node itself as `before`.
	#[error("cannot move {id} before itself")]
	BeforeItself {
		/// The node moved.
		id: Str,
	},
}

/// One rejected op of a frame.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Rejected {
	/// Frame sequence number.
	pub s:     u64,
	/// Op index inside the frame.
	pub op:    usize,
	/// Why.
	pub error: OpError,
}

#[derive(Clone, Debug)]
struct Entry {
	kind:     Kind,
	props:    Map<String, Value>,
	children: Vec<Str>,
	parent:   Option<Str>,
}

/// A surface document: the root `col` (id = surface id) and an id index.
#[derive(Clone, Debug)]
pub struct Document {
	surface:   Str,
	nodes:     FastHashMap<Str, Entry>,
	settled:   FastHashSet<Str>,
	focus:     Option<Str>,
	suspended: bool,
}

impl Document {
	/// An empty surface document: only the root.
	#[must_use]
	pub fn new(surface: impl Into<Str>) -> Self {
		let surface = surface.into();
		let mut nodes = FastHashMap::default();
		nodes.insert(surface.clone(), Entry {
			kind:     Kind::Col,
			props:    Map::new(),
			children: Vec::new(),
			parent:   None,
		});
		Self { surface, nodes, settled: FastHashSet::default(), focus: None, suspended: false }
	}

	/// The surface id (the root's id).
	#[must_use]
	pub const fn surface(&self) -> &Str {
		&self.surface
	}

	/// Number of nodes, root included.
	#[must_use]
	pub fn len(&self) -> usize {
		self.nodes.len()
	}

	/// Whether only the root is left.
	#[must_use]
	pub fn is_empty(&self) -> bool {
		self.nodes.len() == 1
	}

	/// Whether the document holds `id`.
	#[must_use]
	pub fn contains(&self, id: &str) -> bool {
		self.nodes.contains_key(id)
	}

	/// The focused field.
	#[must_use]
	pub fn focus(&self) -> Option<&str> {
		self.focus.as_deref()
	}

	/// Whether the surface is suspended (the grid shows instead).
	#[must_use]
	pub const fn suspended(&self) -> bool {
		self.suspended
	}

	/// Whether `id` got a `settle` hint and no later op at or below it.
	#[must_use]
	pub fn is_settled(&self, id: &str) -> bool {
		self.settled.contains(id)
	}

	/// `x keep:true`: drops the live-only regions (`dock`, `layer`) and the
	/// focus; `main` stays.
	pub fn close(&mut self) {
		for region in ["dock", "layer"] {
			if self
				.nodes
				.get(region)
				.is_some_and(|entry| entry.parent.as_deref() == Some(self.surface.as_str()))
			{
				self.detach(region);
				self.unregister(region);
			}
		}
		self.focus = None;
	}

	/// Applies a frame op by op and returns the rejected ops.
	pub fn apply(&mut self, frame: &Frame) -> Vec<Rejected> {
		frame
			.ops
			.iter()
			.enumerate()
			.filter_map(|(index, op)| {
				self
					.apply_op(op)
					.err()
					.map(|error| Rejected { s: frame.s, op: index, error })
			})
			.collect()
	}

	/// The whole document as a wire node, root included.
	#[must_use]
	pub fn snapshot(&self) -> Node {
		self.export(&self.surface)
	}

	/// One subtree as a wire node.
	#[must_use]
	pub fn get(&self, id: &str) -> Option<Node> {
		self.nodes.contains_key(id).then(|| self.export(id))
	}

	fn export(&self, id: &str) -> Node {
		let entry = &self.nodes[id];
		Node {
			id: self
				.nodes
				.get_key_value(id)
				.map_or_else(|| Str::new(id), |(key, _)| key.clone()),
			k:  entry.kind,
			p:  entry.props.clone(),
			c:  entry
				.children
				.iter()
				.map(|child| self.export(child))
				.collect(),
		}
	}

	fn entry(&self, id: &str) -> Result<&Entry, OpError> {
		self
			.nodes
			.get(id)
			.ok_or_else(|| OpError::UnknownId { id: Str::new(id) })
	}

	fn entry_mut(&mut self, id: &str) -> Result<&mut Entry, OpError> {
		self
			.nodes
			.get_mut(id)
			.ok_or_else(|| OpError::UnknownId { id: Str::new(id) })
	}

	/// Any op at or below a settled node brings it back.
	fn unsettle(&mut self, id: &str) {
		if self.settled.is_empty() {
			return;
		}
		let mut at = Some(Str::new(id));
		while let Some(current) = at {
			self.settled.remove(current.as_str());
			at = self
				.nodes
				.get(current.as_str())
				.and_then(|entry| entry.parent.clone());
		}
	}

	fn insert_index(&self, parent: &str, before: Option<&Str>) -> Result<usize, OpError> {
		let entry = self.entry(parent)?;
		let Some(before) = before else {
			return Ok(entry.children.len());
		};
		entry
			.children
			.iter()
			.position(|child| child == before)
			.ok_or_else(|| OpError::NotAChild { before: before.clone(), parent: Str::new(parent) })
	}

	fn apply_op(&mut self, op: &Op) -> Result<(), OpError> {
		match op {
			Op::Add { parent, before, node } => {
				let index = self.insert_index(parent, before.as_ref())?;
				let mut seen = FastHashSet::default();
				self.check_new(node, &mut seen)?;
				self.insert(node, parent);
				self
					.entry_mut(parent)?
					.children
					.insert(index, node.id.clone());
				self.unsettle(parent);
			},
			Op::Set { id, props } => {
				let entry = self.entry_mut(id)?;
				for (key, value) in props {
					if value.is_null() {
						entry.props.remove(key);
					} else {
						entry.props.insert(key.clone(), value.clone());
					}
				}
				self.unsettle(id);
			},
			Op::Text { id, mode, text } => {
				let entry = self.text_entry(id)?;
				let next = match mode {
					TextMode::Append => {
						let mut current = primary_text(entry).to_owned();
						current.push_str(text);
						current
					},
					TextMode::Replace => text.as_str().to_owned(),
				};
				entry.props.insert("text".into(), Value::String(next));
				self.unsettle(id);
			},
			Op::Splice { id, at, del, text } => {
				let entry = self.text_entry(id)?;
				let current = primary_text(entry);
				let len = xutf::transcoded_len::<xutf::Utf8, xutf::Utf16Le>(current.as_bytes());
				let range = utf16_range(current, *at, *del).ok_or(OpError::SpliceRange {
					at: *at,
					del: *del,
					len,
				})?;
				let mut next = String::with_capacity(current.len() + text.len());
				next.push_str(&current[..range.start]);
				next.push_str(text);
				next.push_str(&current[range.end..]);
				entry.props.insert("text".into(), Value::String(next));
				self.unsettle(id);
			},
			Op::Move { id, parent, before } => {
				if id == &self.surface {
					return Err(OpError::Root);
				}
				self.entry(id)?;
				let mut at = Some(parent.clone());
				while let Some(current) = at.take() {
					if &current == id {
						return Err(OpError::IntoOwnSubtree { id: id.clone() });
					}
					at.clone_from(&self.entry(&current)?.parent);
				}
				if before.as_ref() == Some(id) {
					return Err(OpError::BeforeItself { id: id.clone() });
				}
				// Validate `before` first so a rejected move leaves the tree intact.
				self.insert_index(parent, before.as_ref())?;
				let old_parent = self.detach(id);
				let index = self.insert_index(parent, before.as_ref())?;
				self.entry_mut(parent)?.children.insert(index, id.clone());
				self.entry_mut(id)?.parent = Some(parent.clone());
				if let Some(old_parent) = old_parent {
					self.unsettle(&old_parent);
				}
				self.unsettle(id);
			},
			Op::Del(id) => {
				if id == &self.surface {
					return Err(OpError::Root);
				}
				self.entry(id)?;
				let parent = self.detach(id);
				self.unregister(id);
				if let Some(parent) = parent {
					self.unsettle(&parent);
				}
			},
			Op::Settle(id) => {
				self.entry(id)?;
				self.settled.insert(id.clone());
			},
			Op::Focus(id) => {
				if let Some(id) = id {
					self.entry(id)?;
				}
				self.focus.clone_from(id);
			},
			Op::Reveal(id, _) | Op::Scroll(id, _) => {
				self.entry(id)?;
			},
			Op::Suspend => self.suspended = true,
			Op::Resume => self.suspended = false,
		}
		Ok(())
	}

	fn text_entry(&mut self, id: &str) -> Result<&mut Entry, OpError> {
		let entry = self.entry_mut(id)?;
		if !entry.kind.has_primary_text() {
			return Err(OpError::NoPrimaryText { id: Str::new(id), kind: entry.kind.into() });
		}
		Ok(entry)
	}

	fn check_new(&self, node: &Node, seen: &mut FastHashSet<Str>) -> Result<(), OpError> {
		if node.id.is_empty() {
			return Err(OpError::EmptyId);
		}
		if self.nodes.contains_key(node.id.as_str()) || !seen.insert(node.id.clone()) {
			return Err(OpError::DuplicateId { id: node.id.clone() });
		}
		node
			.c
			.iter()
			.try_for_each(|child| self.check_new(child, seen))
	}

	fn insert(&mut self, node: &Node, parent: &Str) {
		let props = node
			.p
			.iter()
			.filter(|(_, value)| !value.is_null())
			.map(|(key, value)| (key.clone(), value.clone()))
			.collect();
		self.nodes.insert(node.id.clone(), Entry {
			kind: node.k,
			props,
			children: node.c.iter().map(|child| child.id.clone()).collect(),
			parent: Some(parent.clone()),
		});
		for child in &node.c {
			self.insert(child, &node.id);
		}
	}

	/// Removes `id` from its parent's children and returns the parent.
	fn detach(&mut self, id: &str) -> Option<Str> {
		let parent = self.nodes.get(id)?.parent.clone()?;
		if let Some(entry) = self.nodes.get_mut(parent.as_str()) {
			entry.children.retain(|child| child != id);
		}
		Some(parent)
	}

	fn unregister(&mut self, id: &str) {
		let Some(entry) = self.nodes.remove(id) else {
			return;
		};
		self.settled.remove(id);
		if self.focus.as_deref() == Some(id) {
			self.focus = None;
		}
		for child in entry.children {
			self.unregister(&child);
		}
	}
}

fn primary_text(entry: &Entry) -> &str {
	entry
		.props
		.get("text")
		.and_then(Value::as_str)
		.unwrap_or_default()
}

/// The byte range of UTF-16 units `[at, at + del)` in `text`, or `None` when
/// it runs past the end or either end splits a surrogate pair.
fn utf16_range(text: &str, at: u32, del: u32) -> Option<ops::Range<usize>> {
	let start = utf16_to_byte(text, at as usize)?;
	let end = utf16_to_byte(text, at as usize + del as usize)?;
	Some(start..end)
}

fn utf16_to_byte(text: &str, units: usize) -> Option<usize> {
	let mut seen = 0;
	for (byte, ch) in text.char_indices() {
		if seen == units {
			return Some(byte);
		}
		seen += ch.len_utf16();
		if seen > units {
			return None;
		}
	}
	(seen == units).then_some(text.len())
}

#[cfg(test)]
mod tests {
	use serde_json::json;

	use super::{Document, OpError};
	use crate::tsp::wire::{Frame, Kind};

	fn frame(value: serde_json::Value) -> Frame {
		serde_json::from_value(value).expect("frame")
	}

	/// The specification's own two-frame example.
	#[test]
	fn applies_the_specification_example() {
		let mut doc = Document::new("s1");
		let rejected = doc.apply(&frame(json!({"sf":"s1","s":1,"ops":[["add","main","s1",null,{"id":"main","k":"col","c":[
			{"id":"m1","k":"md","p":{"text":"Checking the build"}},
			{"id":"t9","k":"card","p":{"head":"cargo check","status":"running","collapsible":true,"collapsed":true},"c":[
				{"id":"o9","k":"code","p":{"lang":"text","text":"Checking stencil-term v0.1.0"}}]},
			{"id":"w1","k":"shimmer","p":{"text":"Working"}}]}]]})));
		assert!(rejected.is_empty(), "{rejected:?}");
		let rejected = doc.apply(&frame(json!({"sf":"s1","s":2,"ops":[
			["text","m1","append",", then the tests."],
			["set","t9",{"status":"done","collapsed":false}],
			["del","w1"],
			["add","m2","main",null,{"id":"m2","k":"md","p":{"text":"All **42** tests pass."}}]]})));
		assert!(rejected.is_empty(), "{rejected:?}");
		let main = doc.get("main").expect("main");
		let ids: Vec<&str> = main.c.iter().map(|node| node.id.as_str()).collect();
		assert_eq!(ids, ["m1", "t9", "m2"]);
		assert_eq!(main.c[0].p["text"], "Checking the build, then the tests.");
		assert_eq!(main.c[1].p["status"], "done");
		assert_eq!(main.c[1].p["collapsed"], false);
		assert!(!doc.contains("w1"));
	}

	#[test]
	fn a_rejected_op_is_reported_alone_and_the_rest_applies() {
		let mut doc = Document::new("s1");
		let rejected = doc.apply(&frame(json!({"sf":"s1","s":7,"ops":[
			["add","main","s1",null,{"id":"main","k":"col"}],
			["text","t7","append","x"],
			["add","r","main",null,{"id":"r","k":"rule"}],
			["text","r","append","x"]]})));
		assert_eq!(rejected.len(), 2);
		assert_eq!((rejected[0].s, rejected[0].op), (7, 1));
		assert_eq!(rejected[0].error, OpError::UnknownId { id: "t7".into() });
		assert_eq!(rejected[1].op, 3);
		assert!(matches!(rejected[1].error, OpError::NoPrimaryText { kind: "rule", .. }));
		assert!(doc.contains("r"));
	}

	#[test]
	fn an_add_with_a_duplicate_anywhere_in_the_subtree_adds_nothing() {
		let mut doc = Document::new("s1");
		doc.apply(&frame(
			json!({"sf":"s1","s":1,"ops":[["add","main","s1",null,{"id":"main","k":"col"}]]}),
		));
		let rejected = doc.apply(&frame(json!({"sf":"s1","s":2,"ops":[
			["add","a","main",null,{"id":"a","k":"col","c":[{"id":"b","k":"text"},{"id":"main","k":"text"}]}]]})));
		assert_eq!(rejected[0].error, OpError::DuplicateId { id: "main".into() });
		assert!(!doc.contains("a") && !doc.contains("b"));
	}

	#[test]
	fn splice_counts_utf16_units_and_refuses_half_a_surrogate_pair() {
		let mut doc = Document::new("s1");
		doc.apply(&frame(json!({"sf":"s1","s":1,"ops":[
			["add","e","s1",null,{"id":"e","k":"editor","p":{"text":"a🙂b"}}]]})));
		// "a" is 1 unit, the emoji 2, "b" 1.
		let rejected = doc.apply(&frame(json!({"sf":"s1","s":2,"ops":[["splice","e",1,2,"é"]]})));
		assert!(rejected.is_empty(), "{rejected:?}");
		assert_eq!(doc.get("e").expect("editor").p["text"], "aéb");
		let rejected = doc.apply(&frame(json!({"sf":"s1","s":3,"ops":[
			["splice","e",0,9,""]]})));
		assert!(matches!(rejected[0].error, OpError::SpliceRange { len: 3, .. }));
		doc.apply(&frame(json!({"sf":"s1","s":4,"ops":[["text","e","replace","🙂"]]})));
		let rejected = doc.apply(&frame(json!({"sf":"s1","s":5,"ops":[["splice","e",1,0,"x"]]})));
		assert!(matches!(rejected[0].error, OpError::SpliceRange { .. }), "inside the pair");
	}

	#[test]
	fn moves_keep_the_tree_intact_when_refused() {
		let mut doc = Document::new("s1");
		doc.apply(&frame(
			json!({"sf":"s1","s":1,"ops":[["add","main","s1",null,{"id":"main","k":"col","c":[
			{"id":"a","k":"col","c":[{"id":"a1","k":"text"}]},{"id":"b","k":"text"}]}]]}),
		));
		let rejected = doc.apply(&frame(json!({"sf":"s1","s":2,"ops":[
			["move","a","a1",null],
			["move","a","main","a"],
			["move","a","main","zz"],
			["move","s1","main",null],
			["move","b","main","a"]]})));
		let errors: Vec<&OpError> = rejected.iter().map(|rejected| &rejected.error).collect();
		assert_eq!(errors, [
			&OpError::IntoOwnSubtree { id: "a".into() },
			&OpError::BeforeItself { id: "a".into() },
			&OpError::NotAChild { before: "zz".into(), parent: "main".into() },
			&OpError::Root,
		]);
		let main = doc.get("main").expect("main");
		let ids: Vec<&str> = main.c.iter().map(|node| node.id.as_str()).collect();
		assert_eq!(ids, ["b", "a"]);
	}

	#[test]
	fn settle_is_a_hint_any_later_op_below_it_revokes() {
		let mut doc = Document::new("s1");
		doc.apply(&frame(
			json!({"sf":"s1","s":1,"ops":[["add","main","s1",null,{"id":"main","k":"col","c":[
			{"id":"c","k":"card","c":[{"id":"t","k":"md","p":{"text":"x"}}]}]}],["settle","c"]]}),
		));
		assert!(doc.is_settled("c"));
		doc.apply(&frame(json!({"sf":"s1","s":2,"ops":[["text","t","append","y"]]})));
		assert!(!doc.is_settled("c"), "an op inside the subtree brings it back");
	}

	#[test]
	fn closing_keeps_main_and_drops_the_live_regions_and_focus() {
		let mut doc = Document::new("s1");
		doc.apply(&frame(json!({"sf":"s1","s":1,"ops":[
			["add","main","s1",null,{"id":"main","k":"col"}],
			["add","dock","s1",null,{"id":"dock","k":"col","c":[{"id":"ed","k":"editor"}]}],
			["add","layer","s1",null,{"id":"layer","k":"col"}],
			["focus","ed"]]})));
		assert_eq!(doc.focus(), Some("ed"));
		doc.close();
		assert!(doc.contains("main"));
		assert!(!doc.contains("dock") && !doc.contains("ed") && !doc.contains("layer"));
		assert_eq!(doc.focus(), None);
		assert_eq!(doc.snapshot().k, Kind::Col);
	}
}
