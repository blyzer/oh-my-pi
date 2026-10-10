//! Native Tern Surface Protocol presentation for the terminal chat host.
//!
//! This is deliberately a presenter, not a second chat tree. It derives TSP
//! nodes from the existing chat projection and leaves all input ownership with
//! the ordinary terminal host.

use std::{collections::VecDeque, io, time::Duration};

use bytes::BytesMut;
use omp_core::{FastHashMap, Str};
use omp_tui::tsp::{
	DEFAULT_APC_LIMIT, DEFAULT_CREDITS,
	frame::{Encoder, Incoming},
	record::{Direction, Recorder},
	wire::{Close, Event, Frame, Hello, Kind, Mode, Node, Op, Open, Reply, TextMode, Verb},
};
use serde::Serialize;
use serde_json::{Map, Value};
use strum::{IntoStaticStr, VariantArray as _};

use crate::{
	chrome::PLACEHOLDER,
	project::{BlockKind, RenderedBlock},
	settings::TspStreamPacing,
	transcript::REVEAL_HORIZON,
};

const SURFACE_ID: &str = "omp-chat";
const MAIN_ID: &str = "main";
const DOCK_ID: &str = "dock";
const LAYER_ID: &str = "layer";
const COMPOSER_ID: &str = "composer";
const STATUS_ID: &str = "status";

#[derive(Clone, Debug)]
struct BlockState {
	id:        Str,
	text_id:   Str,
	text:      Str,
	finalized: bool,
}

#[derive(Serialize)]
struct Stylesheet {
	sf:   Str,
	name: Str,
	css:  Str,
}

/// One terminal-epoch TSP surface presenter.
pub struct Surface {
	id:               Str,
	hello:            Hello,
	encoder:          Encoder,
	recorder:         Option<Recorder>,
	next_seq:         u64,
	acked:            u64,
	blocks:           FastHashMap<u64, BlockState>,
	opened:           bool,
	adopting:         bool,
	suspended:        bool,
	dirty:            bool,
	last_paced:       Option<Duration>,
	composer_mounted: bool,
	composer:         Str,
	composer_cursor:  u32,
	status_mounted:   bool,
	status_text:      Str,
}
/// HTML-like element tags supported by Tern's `el` vocabulary.
#[derive(Clone, Copy, Debug, Eq, IntoStaticStr, PartialEq)]
#[strum(serialize_all = "lowercase")]
pub enum ElementTag {
	/// Form root.
	Form,
	/// Generic block.
	Div,
	/// Form label.
	Label,
	/// Generic input.
	Input,
	/// Action button.
	Button,
	/// Inline text.
	Span,
	/// Select control.
	Select,
	/// Select option.
	Option,
}

/// Builds a typed TSP `el` node.
#[must_use]
pub fn element_node(
	id: Str,
	tag: ElementTag,
	props: Map<String, Value>,
	children: Vec<Node>,
) -> Node {
	let tag: &'static str = tag.into();
	let mut props = props;
	props.insert("tag".into(), Value::String(tag.into()));
	Node { id, k: Kind::El, p: props, c: children }
}

impl Surface {
	/// Starts an inline surface for a terminal whose `hello` beat the DA1
	/// fence.
	///
	/// That reply reached the host through the startup probe rather than
	/// through [`Surface::incoming`], so it is recorded here: a recording
	/// `surface-play` replays must hold both directions of the handshake.
	pub fn new(hello: Hello, adopt: bool) -> Self {
		let mut surface = Self::with_hello(hello, adopt);
		surface.record_reply(&Reply::Hello(surface.hello.clone()));
		surface
	}

	/// Starts an optimistic v1 surface before a `hello` reply arrives.
	///
	/// The assumed capabilities (every kind, the default `apc` limit and
	/// credits) are the ADR's phase-1 contract; a later reply narrows them
	/// through [`Surface::replace_hello`].
	pub fn optimistic(adopt: bool) -> Self {
		Self::with_hello(
			Hello {
				v:             1,
				term:          Str::new_static("tern"),
				ver:           None,
				kinds:         Kind::VARIANTS
					.iter()
					.map(|kind| Str::new_static((*kind).into()))
					.collect(),
				features:      Vec::new(),
				apc:           Some(DEFAULT_APC_LIMIT as u32),
				credits:       Some(DEFAULT_CREDITS),
				cols:          None,
				cell:          None,
				dark:          None,
				reduce_motion: None,
				hour12:        None,
			},
			adopt,
		)
	}

	fn with_hello(hello: Hello, adopt: bool) -> Self {
		let encoder = Encoder::new(hello.apc_limit());
		Self {
			id: Str::new_static(SURFACE_ID),
			hello,
			encoder,
			recorder: Recorder::from_env(),
			next_seq: 1,
			acked: 0,
			blocks: FastHashMap::default(),
			opened: false,
			adopting: adopt,
			suspended: false,
			dirty: true,
			last_paced: None,
			composer_mounted: false,
			composer: Str::default(),
			composer_cursor: u32::MAX,
			status_mounted: false,
			status_text: Str::default(),
		}
	}

	/// Adopts the capabilities a `hello` reply announced.
	pub fn replace_hello(&mut self, hello: Hello) {
		self.encoder.set_limit(hello.apc_limit());
		self.hello = hello;
	}

	/// Installs or replaces a stylesheet scoped to this surface.
	///
	/// Styles are sent only when Tern advertises `styles`; otherwise the
	/// caller keeps the semantic `el` nodes and Tern's own theme/layout.
	///
	/// # Errors
	///
	/// Returns invalid input for an illegal sheet name or a CSS payload over
	/// Tern's 256 KiB per-surface budget, or the writer error.
	pub fn set_stylesheet(
		&mut self,
		writer: &mut impl io::Write,
		name: &str,
		css: &str,
	) -> io::Result<()> {
		if !self.hello.has_feature("styles") {
			return Ok(());
		}
		if name.is_empty()
			|| name.len() > 64
			|| !name
				.bytes()
				.all(|byte| byte.is_ascii_alphanumeric() || b"_-".contains(&byte))
		{
			return Err(io::Error::new(io::ErrorKind::InvalidInput, "invalid TSP stylesheet name"));
		}
		if css.len() > 256 * 1024 {
			return Err(io::Error::new(io::ErrorKind::InvalidInput, "TSP stylesheet exceeds 256 KiB"));
		}
		self.send(writer, Verb::Stylesheet, &Stylesheet {
			sf:   self.id.clone(),
			name: Str::new(name),
			css:  Str::new(css),
		})
	}

	/// Re-enters the surface for a new terminal epoch.
	pub fn reenter(&mut self, adopt: bool) {
		self.opened = false;
		self.adopting = adopt;
		if !adopt {
			self.blocks.clear();
		}
		self.composer_mounted = false;
		self.composer = Str::default();
		self.composer_cursor = u32::MAX;
		self.status_mounted = false;
		self.status_text = Str::default();
		self.dirty = true;
	}

	/// Earliest time a paced stream should be retried.
	pub fn next_wake(&self, pacing: TspStreamPacing) -> Option<Duration> {
		if pacing == TspStreamPacing::Paced {
			self
				.last_paced
				.map(|last| last.saturating_add(REVEAL_HORIZON))
		} else {
			None
		}
	}

	/// Emits pending native presentation updates if surface flow control allows.
	pub fn present(
		&mut self,
		writer: &mut impl io::Write,
		blocks: &[RenderedBlock],
		composer: &str,
		cursor_utf16: u32,
		status: &str,
		now: Duration,
		pacing: TspStreamPacing,
	) -> io::Result<()> {
		if self.suspended || !self.credit_available() {
			self.dirty = true;
			return Ok(());
		}
		let mut ops = Vec::new();
		if !self.opened {
			let was_adopting = self.adopting;
			self.send_open(writer)?;
			if was_adopting {
				ops.extend([
					add(DOCK_ID, self.id.as_str(), Kind::Col, Map::new()),
					add(LAYER_ID, self.id.as_str(), Kind::Col, Map::new()),
				]);
			} else {
				ops.extend([
					add(MAIN_ID, self.id.as_str(), Kind::Col, Map::new()),
					add(DOCK_ID, self.id.as_str(), Kind::Col, Map::new()),
					add(LAYER_ID, self.id.as_str(), Kind::Col, Map::new()),
				]);
			}
			self.opened = true;
		}
		self.blocks(blocks, &mut ops, now, pacing);
		self.composer(composer, cursor_utf16, &mut ops);
		self.status(status, &mut ops);
		if ops.is_empty() {
			self.dirty = false;
			return Ok(());
		}
		self.send_frame(writer, ops)?;
		self.dirty = false;
		Ok(())
	}

	/// Handles a terminal reply or event.
	pub fn incoming(&mut self, incoming: Incoming) {
		self.record_incoming(&incoming);
		match incoming {
			Incoming::Reply(Reply::Hello(hello)) => self.replace_hello(hello),
			Incoming::Event(Event::Ack { sf, s }) if sf == self.id => {
				self.acked = self.acked.max(s);
			},
			Incoming::Event(Event::Gone { sf, .. })
				if sf.as_ref().is_none_or(|surface| surface == &self.id) =>
			{
				self.opened = false;
				self.adopting = false;
				self.blocks.clear();
				self.composer_mounted = false;
				self.composer = Str::default();
				self.composer_cursor = u32::MAX;
				self.status_mounted = false;
				self.status_text = Str::default();
				self.dirty = true;
			},
			_ => {},
		}
	}

	/// Suspends native composition so the regular renderer can paint overlays.
	pub fn suspend(&mut self, writer: &mut impl io::Write) -> io::Result<()> {
		if !self.suspended {
			self.send_frame(writer, vec![Op::Suspend])?;
			self.suspended = true;
		}
		Ok(())
	}

	/// Resumes native composition after the cell overlay closes.
	pub fn resume(&mut self, writer: &mut impl io::Write) -> io::Result<()> {
		if self.suspended {
			self.send_frame(writer, vec![Op::Resume])?;
			self.suspended = false;
			self.dirty = true;
		}
		Ok(())
	}

	/// Closes the live epoch while retaining the inline main region for
	/// adoption.
	pub fn close(&mut self, writer: &mut impl io::Write, keep: bool) -> io::Result<()> {
		if self.opened {
			self.send(writer, Verb::Close, &Close { id: self.id.clone(), keep })?;
			self.opened = false;
		}
		Ok(())
	}

	/// Whether a blocked frame still needs replaying after an `ack`.
	pub const fn dirty(&self) -> bool {
		self.dirty
	}

	fn credit_available(&self) -> bool {
		let unacknowledged = (self.next_seq.saturating_sub(1)).saturating_sub(self.acked);
		unacknowledged < u64::from(self.hello.credit_limit())
	}

	fn send_open(&mut self, writer: &mut impl io::Write) -> io::Result<()> {
		let open = Open {
			id:     self.id.clone(),
			mode:   Mode::Inline,
			title:  Some(Str::new_static("omp")),
			role:   Some(Str::new_static("omp.session")),
			adopt:  self.adopting.then_some(true),
			listen: Some(true),
		};
		self.send(writer, Verb::Open, &open)?;
		self.adopting = false;
		Ok(())
	}

	fn send_frame(&mut self, writer: &mut impl io::Write, ops: Vec<Op>) -> io::Result<()> {
		let frame = Frame { sf: self.id.clone(), s: self.next_seq, ops };
		self.next_seq = self.next_seq.saturating_add(1);
		self.send(writer, Verb::Frame, &frame)
	}

	fn send<T: Serialize>(
		&mut self,
		writer: &mut impl io::Write,
		verb: Verb,
		value: &T,
	) -> io::Result<()> {
		let body = serde_json::to_vec(value).map_err(io::Error::other)?;
		if let Some(recorder) = &mut self.recorder {
			let _ = recorder.log(Direction::Out, verb.into(), &[], &body);
		}
		let mut out = BytesMut::new();
		self.encoder.message(verb, &[], &body, &mut out);
		writer.write_all(&out)?;
		writer.flush()
	}

	fn record_incoming(&mut self, incoming: &Incoming) {
		match incoming {
			Incoming::Reply(reply) => self.record_reply(reply),
			Incoming::Event(event) => self.record(Direction::In, "e", event),
		}
	}

	fn record_reply(&mut self, reply: &Reply) {
		self.record(Direction::In, "r", reply);
	}

	fn record<T: Serialize>(&mut self, direction: Direction, verb: &str, value: &T) {
		let Some(recorder) = &mut self.recorder else {
			return;
		};
		if let Ok(body) = serde_json::to_vec(value) {
			let _ = recorder.log(direction, verb, &[], &body);
		}
	}

	fn blocks(
		&mut self,
		blocks: &[RenderedBlock],
		ops: &mut Vec<Op>,
		now: Duration,
		pacing: TspStreamPacing,
	) {
		let mut retained = VecDeque::new();
		for block in blocks {
			retained.push_back(block.view.key);
			let id = block_id(block.view.key);
			let (node, text_id) = node_for(block, &self.hello);
			let prior = self.blocks.get(&block.view.key).cloned();
			match prior {
				None => {
					if is_stream(block) && pacing == TspStreamPacing::Paced && self.last_paced.is_none()
					{
						self.last_paced = Some(now);
					}
					ops.push(add_node(node, MAIN_ID));
					self.blocks.insert(block.view.key, BlockState {
						id: id.clone(),
						text_id,
						text: block.view.text.clone(),
						finalized: block.view.finalized,
					});
					if block.view.finalized {
						ops.push(Op::Settle(id));
					}
				},
				Some(mut prior) => {
					let stream = is_stream(block);
					let grew = stream && block.view.text.starts_with(prior.text.as_str());
					let pacing_due = pacing != TspStreamPacing::Paced
						|| block.view.finalized
						|| self
							.last_paced
							.is_none_or(|last| now >= last.saturating_add(REVEAL_HORIZON));
					if grew && block.view.text.len() > prior.text.len() && pacing_due {
						ops.push(Op::Text {
							id:   prior.text_id.clone(),
							mode: TextMode::Append,
							text: Str::new(&block.view.text.as_str()[prior.text.len()..]),
						});
						prior.text.clone_from(&block.view.text);
						if pacing == TspStreamPacing::Paced {
							self.last_paced = Some(now);
						}
					} else if !grew && prior.text != block.view.text {
						ops.push(Op::Set {
							id:    prior.text_id.clone(),
							props: text_props(block.view.text.clone()),
						});
						prior.text.clone_from(&block.view.text);
					}
					if block.view.finalized && !prior.finalized {
						if prior.text != block.view.text {
							ops.push(Op::Text {
								id:   prior.text_id.clone(),
								mode: TextMode::Append,
								text: Str::new(&block.view.text.as_str()[prior.text.len()..]),
							});
							prior.text.clone_from(&block.view.text);
						}
						ops.push(Op::Settle(prior.id.clone()));
						prior.finalized = true;
					}
					self.blocks.insert(block.view.key, prior);
				},
			}
		}
		let stale = self
			.blocks
			.keys()
			.filter(|key| !retained.contains(key))
			.copied()
			.collect::<Vec<_>>();
		for key in stale {
			if let Some(block) = self.blocks.remove(&key) {
				ops.push(Op::Del(block.id));
			}
		}
	}

	fn composer(&mut self, text: &str, cursor_utf16: u32, ops: &mut Vec<Op>) {
		let id = Str::new_static(COMPOSER_ID);
		if !self.composer_mounted {
			let mut props = text_props(Str::new(text));
			props.insert("placeholder".into(), Value::String(PLACEHOLDER.into()));
			props.insert("cursor".into(), Value::from(cursor_utf16));
			ops.push(add(COMPOSER_ID, DOCK_ID, Kind::Editor, props));
			ops.push(Op::Focus(Some(id)));
			self.composer_mounted = true;
			self.composer = Str::new(text);
			self.composer_cursor = cursor_utf16;
			return;
		}
		if self.composer.as_str() != text {
			ops.push(Op::Text { id: id.clone(), mode: TextMode::Replace, text: Str::new(text) });
			self.composer = Str::new(text);
		}
		if self.composer_cursor != cursor_utf16 {
			let mut cursor = Map::new();
			cursor.insert("cursor".into(), Value::from(cursor_utf16));
			ops.push(Op::Set { id, props: cursor });
			self.composer_cursor = cursor_utf16;
		}
	}

	fn status(&mut self, status: &str, ops: &mut Vec<Op>) {
		let text = Str::new(status);
		if !self.status_mounted {
			ops.push(add_node(status_node(text.as_str()), DOCK_ID));
			self.status_mounted = true;
			self.status_text = text;
		} else if self.status_text != text {
			ops.extend([
				Op::Del(Str::new_static(STATUS_ID)),
				add_node(status_node(text.as_str()), DOCK_ID),
			]);
			self.status_text = text;
		}
	}
}

/// Whether a block's text grows by append, so deltas ride `text append`.
const fn is_stream(block: &RenderedBlock) -> bool {
	matches!(block.view.kind, BlockKind::Assistant | BlockKind::Thinking)
}

fn status_node(text: &str) -> Node {
	let children = text
		.split('·')
		.enumerate()
		.map(|(index, segment)| Node {
			id: Str::from(format!("{STATUS_ID}-{index}")),
			k:  Kind::Seg,
			p:  text_props(Str::new(segment.trim())),
			c:  Vec::new(),
		})
		.collect();
	Node {
		id: Str::new_static(STATUS_ID),
		k:  Kind::Status,
		p:  text_props(Str::new(text)),
		c:  children,
	}
}

fn block_id(key: u64) -> Str {
	Str::from(format!("b-{key:x}"))
}

fn text_props(text: Str) -> Map<String, Value> {
	let mut props = Map::new();
	props.insert("text".into(), Value::String(text.into()));
	props
}

fn add(id: &str, parent: &str, kind: Kind, props: Map<String, Value>) -> Op {
	Op::Add {
		parent: Str::new(parent),
		before: None,
		node:   Node { id: Str::new(id), k: kind, p: props, c: Vec::new() },
	}
}

fn add_node(node: Node, parent: &str) -> Op {
	Op::Add { parent: Str::new(parent), before: None, node }
}

fn node_for(block: &RenderedBlock, hello: &Hello) -> (Node, Str) {
	let id = block_id(block.view.key);
	if let Some(node) = block
		.description
		.as_ref()
		.filter(|node| hello.draws(node.k))
	{
		return (node.clone(), node.id.clone());
	}
	let kind = match block.view.kind {
		BlockKind::User => Kind::Card,
		BlockKind::Assistant if hello.draws(Kind::Md) => Kind::Md,
		BlockKind::Thinking if hello.draws(Kind::Section) => Kind::Section,
		BlockKind::Notice if hello.draws(Kind::Toast) => Kind::Toast,
		BlockKind::Tool if hello.draws(Kind::Tool) => Kind::Tool,
		BlockKind::Tool if hello.draws(Kind::Card) => Kind::Card,
		_ => Kind::Rows,
	};
	match kind {
		Kind::Md | Kind::Toast => (
			Node { id: id.clone(), k: kind, p: text_props(block.view.text.clone()), c: Vec::new() },
			id,
		),
		Kind::Rows => {
			let mut props = text_props(block.view.text.clone());
			props.insert(
				"lines".into(),
				Value::Array(
					block
						.view
						.text
						.lines()
						.map(|l| Value::String(l.to_string()))
						.collect(),
				),
			);
			(Node { id: id.clone(), k: kind, p: props, c: Vec::new() }, id)
		},
		Kind::Card | Kind::Section | Kind::Tool => {
			let child_id = Str::from(format!("{id}-text"));
			(
				Node {
					id,
					k: kind,
					p: Map::new(),
					c: vec![Node {
						id: child_id.clone(),
						k:  if matches!(block.view.kind, BlockKind::Assistant | BlockKind::Thinking) {
							Kind::Md
						} else {
							Kind::Text
						},
						p:  text_props(block.view.text.clone()),
						c:  Vec::new(),
					}],
				},
				child_id,
			)
		},
		_ => unreachable!("phase-one node mapping is closed"),
	}
}

#[cfg(test)]
mod tests {
	use omp_tui::{
		IntoComponent as _,
		components::TextLeaf,
		slots::Mode,
		tsp::{doc::Document, frame::split},
	};

	use super::*;
	use crate::project::BlockView;

	fn make_block(key: u64, kind: BlockKind, text: &str, finalized: bool) -> RenderedBlock {
		RenderedBlock {
			view:        BlockView {
				key,
				kind,
				text: Str::new(text),
				mode: Mode::AppendOnly,
				finalized,
			},
			component:   TextLeaf::new().text(text).into_component(),
			description: None,
			stream:      Some(Str::new(text)),
		}
	}

	fn apply_output_to_doc(doc: &mut Document, output: &[u8]) {
		let mut rest = output;
		while let Some(start) = rest.windows(2).position(|w| w == b"\x1b_") {
			let after = &rest[start + 2..];
			let end = after
				.windows(2)
				.position(|w| w == b"\x1b\\")
				.expect("terminated message");
			let payload = &after[..end];
			if let Some(raw) = split(payload)
				&& raw.verb == "f"
			{
				let frame: Frame = serde_json::from_slice(raw.body).expect("valid frame JSON");
				let rejected = doc.apply(&frame);
				assert!(rejected.is_empty(), "frame had rejected ops: {rejected:?}");
			}
			rest = &after[end + 2..];
		}
	}

	#[test]
	fn optimistic_surface_emits_valid_regions_and_document_applies() {
		let mut surface = Surface::optimistic(false);
		let mut out = Vec::new();
		let mut doc = Document::new(SURFACE_ID);

		let blocks = [make_block(1, BlockKind::User, "Hello assistant", true)];
		surface
			.present(
				&mut out,
				&blocks,
				"my draft",
				8,
				"model-v1",
				Duration::ZERO,
				TspStreamPacing::Raw,
			)
			.expect("present succeeds");

		apply_output_to_doc(&mut doc, &out);
		assert!(doc.contains(MAIN_ID));
		assert!(doc.contains(DOCK_ID));
		assert!(doc.contains(LAYER_ID));
		assert!(doc.contains(COMPOSER_ID));
		assert_eq!(doc.focus(), Some(COMPOSER_ID));
		assert!(doc.contains(&block_id(1)));
		assert!(doc.is_settled(&block_id(1)));
	}

	#[test]
	fn streaming_assistant_uses_text_append_and_settles() {
		let mut surface = Surface::optimistic(false);
		let mut doc = Document::new(SURFACE_ID);

		// Frame 1: streaming delta 1
		let mut out1 = Vec::new();
		let blocks1 = [make_block(2, BlockKind::Assistant, "The answer is", false)];
		surface
			.present(&mut out1, &blocks1, "", 0, "status", Duration::ZERO, TspStreamPacing::Raw)
			.unwrap();
		apply_output_to_doc(&mut doc, &out1);
		assert!(!doc.is_settled(&block_id(2)));

		// Frame 2: streaming delta 2
		let mut out2 = Vec::new();
		let blocks2 = [make_block(2, BlockKind::Assistant, "The answer is 42.", false)];
		surface
			.present(
				&mut out2,
				&blocks2,
				"",
				0,
				"status",
				Duration::from_millis(50),
				TspStreamPacing::Raw,
			)
			.unwrap();
		apply_output_to_doc(&mut doc, &out2);
		assert!(!doc.is_settled(&block_id(2)));
		// Acknowledge frame 1 to grant credit for frame 3
		surface.incoming(Incoming::Event(Event::Ack { sf: Str::new(SURFACE_ID), s: 1 }));

		// Frame 3: finalized
		let mut out3 = Vec::new();
		let blocks3 = [make_block(2, BlockKind::Assistant, "The answer is 42.", true)];
		surface
			.present(
				&mut out3,
				&blocks3,
				"",
				0,
				"status",
				Duration::from_millis(100),
				TspStreamPacing::Raw,
			)
			.unwrap();
		apply_output_to_doc(&mut doc, &out3);
		assert!(doc.is_settled(&block_id(2)));
	}

	#[test]
	fn flow_control_credit_limit_and_ack_unblocking() {
		let mut surface = Surface::optimistic(false);
		let mut out = Vec::new();

		// Frame 1
		let blocks = [make_block(1, BlockKind::Assistant, "chunk 1", false)];
		surface
			.present(&mut out, &blocks, "", 0, "", Duration::ZERO, TspStreamPacing::Raw)
			.unwrap();
		assert!(!out.is_empty());
		out.clear();

		// Frame 2
		let blocks = [make_block(1, BlockKind::Assistant, "chunk 1 and 2", false)];
		surface
			.present(&mut out, &blocks, "", 0, "", Duration::from_millis(10), TspStreamPacing::Raw)
			.unwrap();
		assert!(!out.is_empty());
		out.clear();

		// Frame 3 should be blocked because credits = 2 and unacknowledged = 2
		let blocks = [make_block(1, BlockKind::Assistant, "chunk 1 and 2 and 3", false)];
		surface
			.present(&mut out, &blocks, "", 0, "", Duration::from_millis(20), TspStreamPacing::Raw)
			.unwrap();
		assert!(out.is_empty(), "frame 3 must be blocked by credit limit");
		assert!(surface.dirty());

		// Acknowledge frame 1
		surface.incoming(Incoming::Event(Event::Ack { sf: Str::new(SURFACE_ID), s: 1 }));

		// Now frame 3 can be sent
		surface
			.present(&mut out, &blocks, "", 0, "", Duration::from_millis(30), TspStreamPacing::Raw)
			.unwrap();
		assert!(!out.is_empty(), "frame 3 must be sent after ack 1");
	}

	#[test]
	fn overlay_suspends_and_resumes() {
		let mut surface = Surface::optimistic(false);
		let mut out = Vec::new();
		let mut doc = Document::new(SURFACE_ID);

		let blocks = [make_block(1, BlockKind::User, "hi", true)];
		surface
			.present(&mut out, &blocks, "", 0, "", Duration::ZERO, TspStreamPacing::Raw)
			.unwrap();
		apply_output_to_doc(&mut doc, &out);
		assert!(!doc.suspended());

		out.clear();
		surface.suspend(&mut out).unwrap();
		apply_output_to_doc(&mut doc, &out);
		assert!(doc.suspended());

		out.clear();
		surface.resume(&mut out).unwrap();
		apply_output_to_doc(&mut doc, &out);
		assert!(!doc.suspended());
	}

	#[test]
	fn close_with_keep_and_adopt() {
		let mut surface = Surface::optimistic(false);
		let mut out = Vec::new();
		let mut doc = Document::new(SURFACE_ID);

		let blocks = [make_block(1, BlockKind::User, "historical user", true)];
		surface
			.present(&mut out, &blocks, "draft", 5, "ok", Duration::ZERO, TspStreamPacing::Raw)
			.unwrap();
		apply_output_to_doc(&mut doc, &out);

		// Close with keep: true
		out.clear();
		surface.close(&mut out, true).unwrap();
		doc.close();
		assert!(doc.contains(MAIN_ID));
		assert!(!doc.contains(DOCK_ID));

		// Adopt in next epoch
		surface.reenter(true);
		out.clear();
		let new_blocks = [
			make_block(1, BlockKind::User, "historical user", true),
			make_block(2, BlockKind::Assistant, "new reply", true),
		];
		surface
			.present(&mut out, &new_blocks, "draft 2", 7, "ok", Duration::ZERO, TspStreamPacing::Raw)
			.unwrap();
		apply_output_to_doc(&mut doc, &out);
		assert!(doc.contains(MAIN_ID));
		assert!(doc.contains(DOCK_ID));
		assert!(doc.contains(&block_id(1)));
		assert!(doc.contains(&block_id(2)));
	}

	#[test]
	fn stream_pacing_holds_deltas_until_horizon() {
		let mut surface = Surface::optimistic(false);
		let mut out = Vec::new();

		let blocks1 = [make_block(1, BlockKind::Assistant, "A", false)];
		surface
			.present(&mut out, &blocks1, "", 0, "", Duration::from_millis(100), TspStreamPacing::Paced)
			.unwrap();
		assert!(!out.is_empty(), "initial block must be added");
		out.clear();

		// Delta arrived too quickly before REVEAL_HORIZON
		let blocks2 = [make_block(1, BlockKind::Assistant, "AB", false)];
		surface
			.present(&mut out, &blocks2, "", 0, "", Duration::from_millis(150), TspStreamPacing::Paced)
			.unwrap();
		assert!(out.is_empty(), "delta must be held under paced mode before horizon");

		// Delta released once REVEAL_HORIZON passes
		surface
			.present(
				&mut out,
				&blocks2,
				"",
				0,
				"",
				Duration::from_millis(100) + REVEAL_HORIZON,
				TspStreamPacing::Paced,
			)
			.unwrap();
		assert!(!out.is_empty(), "delta must be emitted after horizon");
	}

	#[test]
	fn tool_kind_fallback_to_card_when_tool_absent() {
		let hello_without_tool = Hello {
			v:             1,
			term:          Str::new_static("tern"),
			ver:           None,
			kinds:         vec![
				Str::new_static("col"),
				Str::new_static("card"),
				Str::new_static("md"),
			],
			features:      Vec::new(),
			apc:           None,
			credits:       None,
			cols:          None,
			cell:          None,
			dark:          None,
			reduce_motion: None,
			hour12:        None,
		};
		assert!(!hello_without_tool.draws(Kind::Tool));

		let block = make_block(5, BlockKind::Tool, "bash exit 0", true);
		let (node, _) = node_for(&block, &hello_without_tool);
		assert_eq!(node.k, Kind::Card, "tool must fall back to card when tool kind is missing");
	}

	#[test]
	fn mutation_detects_unsettled_blocks() {
		let mut doc = Document::new(SURFACE_ID);
		let frame = Frame {
			sf:  Str::new_static(SURFACE_ID),
			s:   1,
			ops: vec![
				add(MAIN_ID, SURFACE_ID, Kind::Col, Map::new()),
				add_node(
					Node { id: Str::new_static("b-1"), k: Kind::Card, p: Map::new(), c: vec![] },
					MAIN_ID,
				),
				// Intentionally omitted Op::Settle("b-1")
			],
		};
		doc.apply(&frame);
		assert!(!doc.is_settled("b-1"), "omitted settle must leave block unsettled");
	}
	#[test]
	fn element_builder_is_typed_and_surface_scoped() {
		let mut props = Map::new();
		props.insert("text".into(), Value::String(Str::new_static("Approve").into()));
		let node = element_node(Str::new_static("approve"), ElementTag::Button, props, Vec::new());
		assert_eq!(node.k, Kind::El);
		assert_eq!(node.p.get("tag"), Some(&Value::String("button".into())));
	}

	#[test]
	fn stylesheet_requires_feature_and_rejects_invalid_names() {
		let hello = Hello {
			v:             1,
			term:          Str::new_static("tern"),
			ver:           None,
			kinds:         vec![Str::new_static("col")],
			features:      vec![Str::new_static("styles")],
			apc:           None,
			credits:       None,
			cols:          None,
			cell:          None,
			dark:          None,
			reduce_motion: None,
			hour12:        None,
		};
		let mut surface = Surface::new(hello, false);
		let mut out = Vec::new();
		surface
			.set_stylesheet(&mut out, "forms", ".ask { display: flex }")
			.unwrap();
		assert!(out.windows(6).any(|window| window == b"tsp;s;"));
		assert!(surface.set_stylesheet(&mut out, "bad/name", "").is_err());
	}
}
