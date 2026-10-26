//! Policy-driven redaction of session snapshots and subscription events.
//!
//! Redaction blanks values and never changes shape: no property pair is
//! dropped and no node is pruned, so handles, parents, child order, and
//! property keys stay byte-aligned with the unredacted source. An observer of
//! a redacted stream therefore applies the same patches against the same
//! handles as the owner.

use omp_core::Str;

use crate::{Event, Handle, NodeSpec, Op, Patch, PropKey, Snapshot, Tag, Value};

/// Decides which node properties and text content a bridge must blank.
pub trait PropPolicy {
	/// Returns whether property `prop` of a `tag` node must be blanked.
	fn redacts(&self, tag: &Tag, prop: &PropKey) -> bool;

	/// Returns whether the text content of a `tag` node must be blanked.
	fn redacts_content(&self, tag: &Tag) -> bool;
}

impl Snapshot {
	/// Returns this snapshot with every policy-selected property value and
	/// text content replaced by an empty string.
	///
	/// Handles, parents, child order, property keys, and the allocator
	/// high-water mark are preserved exactly.
	#[must_use]
	pub fn redact(&self, policy: &dyn PropPolicy) -> Self {
		let streams = self
			.streams
			.iter()
			.cloned()
			.map(|mut stream| {
				if stream_target_redacted(self, stream.node, &stream.prop, policy) {
					stream.text = Str::empty();
				}
				stream
			})
			.collect();
		let mut nodes = self.nodes.clone();
		for node in nodes.iter_mut().flatten() {
			blank(&node.tag, &mut node.props, node.content.as_mut(), policy);
		}
		Self::build(self.high_water, self.next_sid, nodes, self.parents.clone(), streams)
	}
}

/// Returns `event` with every policy-selected value blanked, resolving node
/// tags against `base`.
///
/// `base` is the replica state the event applies to. A target the base cannot
/// resolve is treated as redacted, so an unknown handle or stream never leaks.
#[must_use]
pub fn redact_event(event: Event, base: &Snapshot, policy: &dyn PropPolicy) -> Event {
	match event {
		Event::Patch(Patch { cause, prior, label, ops }) => {
			Event::Patch(Patch { cause, prior, label, ops: redact_ops(ops, base, policy) })
		},
		Event::Reset { snapshot } => Event::Reset { snapshot: snapshot.redact(policy) },
		Event::Stream { cause, sid, op, node, prop, text } => {
			let blanked = text.map(|text| {
				if stream_redacted(base, sid, node, prop.as_ref(), policy) {
					Str::empty()
				} else {
					text
				}
			});
			Event::Stream { cause, sid, op, node, prop, text: blanked }
		},
	}
}

/// Blanks the selected property values and text content of one node shape.
fn blank(
	tag: &Tag,
	props: &mut [(PropKey, Value)],
	content: Option<&mut Str>,
	policy: &dyn PropPolicy,
) {
	for (key, value) in props {
		if policy.redacts(tag, key) {
			*value = Value::Str(Str::empty());
		}
	}
	if let Some(content) = content
		&& policy.redacts_content(tag)
	{
		*content = Str::empty();
	}
}

/// Rewrites one patch's operations, tracking the handles the patch mints so a
/// later `Set` on a freshly inserted node resolves its tag.
fn redact_ops(ops: Vec<Op>, base: &Snapshot, policy: &dyn PropPolicy) -> Vec<Op> {
	let mut minted: Vec<(Handle, Tag)> = Vec::new();
	let mut high_water = base.high_water();
	ops.into_iter()
		.map(|op| match op {
			Op::Ins { parent, after, mut node } => {
				high_water = high_water.saturating_add(1);
				if let Some(handle) = Handle::new(high_water) {
					minted.push((handle, node.tag.clone()));
				}
				blank_spec(&mut node, policy);
				Op::Ins { parent, after, node }
			},
			Op::Set { h, prop, value } => {
				let tag = base
					.get(h)
					.map(|node| &node.tag)
					.or_else(|| tag_of(&minted, h));
				let value = match tag {
					Some(tag) if !policy.redacts(tag, &prop) => value,
					_ => Value::Str(Str::empty()),
				};
				Op::Set { h, prop, value }
			},
			op @ (Op::Rm(_) | Op::Mv { .. }) => op,
		})
		.collect()
}

fn blank_spec(node: &mut NodeSpec, policy: &dyn PropPolicy) {
	blank(&node.tag, &mut node.props, node.content.as_mut(), policy);
}

fn tag_of(minted: &[(Handle, Tag)], handle: Handle) -> Option<&Tag> {
	minted
		.iter()
		.find(|(candidate, _)| *candidate == handle)
		.map(|(_, tag)| tag)
}

/// Resolves a stream delta's target, preferring the event's own binding and
/// falling back to the open stream recorded in `base`.
fn stream_redacted(
	base: &Snapshot,
	sid: crate::Sid,
	node: Option<Handle>,
	prop: Option<&PropKey>,
	policy: &dyn PropPolicy,
) -> bool {
	if let (Some(node), Some(prop)) = (node, prop) {
		return stream_target_redacted(base, node, prop, policy);
	}
	let Some(stream) = base.streams.iter().find(|stream| stream.sid == sid) else {
		return true;
	};
	stream_target_redacted(base, stream.node, &stream.prop, policy)
}

fn stream_target_redacted(
	base: &Snapshot,
	node: Handle,
	prop: &PropKey,
	policy: &dyn PropPolicy,
) -> bool {
	base
		.get(node)
		.is_none_or(|node| policy.redacts(&node.tag, prop))
}

#[cfg(test)]
fn prop_text<'a>(node: &'a crate::Node, prop: &PropKey) -> Option<&'a str> {
	node.prop(prop).and_then(Value::as_str)
}

#[cfg(test)]
mod tests {
	use omp_journal::EntryId;
	use omp_vocab::{KnownTag, PropId};
	use pretty_assertions::assert_eq;

	use super::*;
	use crate::{Dom, StreamOp, Txn, wire};

	const TURN: Handle = Handle::new(5).expect("five is nonzero");
	const USER: Handle = Handle::new(6).expect("six is nonzero");
	const ASSISTANT: Handle = Handle::new(7).expect("seven is nonzero");

	/// Blanks `text` and `thinking` everywhere, plus the content of
	/// `<assistant>` nodes.
	struct TextPolicy;

	impl PropPolicy for TextPolicy {
		fn redacts(&self, _tag: &Tag, prop: &PropKey) -> bool {
			matches!(prop, PropKey::Known(PropId::Text | PropId::Thinking))
		}

		fn redacts_content(&self, tag: &Tag) -> bool {
			tag == &Tag::Known(KnownTag::Assistant)
		}
	}

	fn seeded() -> Dom {
		let mut dom = Dom::new();
		let body = dom.body();
		dom.apply(&Txn {
			cause: EntryId::default(),
			label: None,
			ops:   vec![
				Op::Ins { parent: body, after: None, node: NodeSpec::new(KnownTag::Turn) },
				Op::Ins {
					parent: TURN,
					after:  None,
					node:   NodeSpec::new(KnownTag::User)
						.with_prop(PropId::Text, Value::Str(Str::new_static("secret question")))
						.with_prop(PropId::Status, Value::Str(Str::new_static("sent")))
						.with_content("visible"),
				},
				Op::Ins {
					parent: TURN,
					after:  Some(USER),
					node:   NodeSpec::new(KnownTag::Assistant)
						.with_prop(PropId::Text, Value::Str(Str::new_static("secret answer")))
						.with_prop(PropId::TokensIn, Value::Int(12))
						.with_content("secret content"),
				},
			],
		})
		.expect("seed transaction applies");
		dom
	}

	#[test]
	fn snapshot_redaction_preserves_shape_and_blanks_only_values() {
		let snapshot = seeded().snapshot();
		let redacted = snapshot.redact(&TextPolicy);

		assert_eq!(redacted.high_water(), snapshot.high_water());
		assert_eq!(redacted.next_sid(), snapshot.next_sid());
		assert_eq!(redacted.handles().collect::<Vec<_>>(), snapshot.handles().collect::<Vec<_>>());
		for handle in snapshot.handles() {
			let before = snapshot.get(handle).expect("live handle");
			let after = redacted.get(handle).expect("redaction keeps every node");
			assert_eq!(after.tag, before.tag);
			assert_eq!(after.kids, before.kids);
			assert_eq!(redacted.parent(handle), snapshot.parent(handle));
			assert_eq!(
				after.props.iter().map(|(key, _)| key).collect::<Vec<_>>(),
				before.props.iter().map(|(key, _)| key).collect::<Vec<_>>()
			);
		}

		let user = redacted.get(USER).expect("user node survives");
		assert_eq!(prop_text(user, &PropId::Text.into()), Some(""));
		assert_eq!(prop_text(user, &PropId::Status.into()), Some("sent"));
		assert_eq!(user.content.as_deref(), Some("visible"));

		let assistant = redacted.get(ASSISTANT).expect("assistant node survives");
		assert_eq!(prop_text(assistant, &PropId::Text.into()), Some(""));
		assert_eq!(assistant.prop(&PropId::TokensIn.into()), Some(&Value::Int(12)));
		assert_eq!(assistant.content.as_deref(), Some(""));
	}

	#[test]
	fn redacted_snapshot_decodes_as_a_canonical_snapshot() {
		let redacted = seeded().snapshot().redact(&TextPolicy);
		let decoded = Snapshot::from_bytes(redacted.as_bytes()).expect("redaction stays canonical");
		assert_eq!(decoded.as_bytes(), redacted.as_bytes());
		assert_eq!(decoded, redacted);
	}

	#[test]
	fn redacted_snapshot_survives_the_wire_codec() {
		let redacted = seeded().snapshot().redact(&TextPolicy);
		let bytes = wire::encode_event(&Event::Reset { snapshot: redacted.clone() })
			.expect("redacted reset encodes");
		let Event::Reset { snapshot } = wire::decode_event(&bytes).expect("redacted reset decodes")
		else {
			panic!("snapshot@1 decodes as a reset");
		};
		assert_eq!(snapshot.as_bytes(), redacted.as_bytes());
	}

	#[test]
	fn open_stream_text_is_blanked_in_the_snapshot() {
		let mut dom = seeded();
		let sid = dom
			.stream_open(EntryId::default(), ASSISTANT, PropId::Text.into())
			.expect("assistant text accepts a stream");
		dom.stream_append(EntryId::default(), sid, "streamed secret")
			.expect("open stream accepts a delta");
		let redacted = dom.snapshot().redact(&TextPolicy);
		let assistant = redacted.get(ASSISTANT).expect("assistant node survives");
		assert_eq!(prop_text(assistant, &PropId::Text.into()), Some(""));
		assert!(!redacted.as_bytes().windows(6).any(|w| w == b"secret"));
	}

	#[test]
	fn patch_sets_and_inserts_are_blanked_and_moves_are_untouched() {
		let base = seeded().snapshot();
		let patch = Event::Patch(Patch {
			cause: EntryId::default(),
			prior: None,
			label: Some(Str::new_static("turn")),
			ops:   vec![
				Op::Set {
					h:     USER,
					prop:  PropId::Text.into(),
					value: Value::Str(Str::new_static("secret edit")),
				},
				Op::Set {
					h:     USER,
					prop:  PropId::Status.into(),
					value: Value::Str(Str::new_static("edited")),
				},
				Op::Ins {
					parent: TURN,
					after:  Some(ASSISTANT),
					node:   NodeSpec::new(KnownTag::Assistant)
						.with_prop(PropId::Thinking, Value::Str(Str::new_static("secret plan")))
						.with_prop(PropId::Status, Value::Str(Str::new_static("live")))
						.with_content("secret body"),
				},
				Op::Mv { h: USER, parent: TURN, after: None },
				Op::Rm(ASSISTANT),
			],
		});

		let Event::Patch(redacted) = redact_event(patch.clone(), &base, &TextPolicy) else {
			panic!("a patch stays a patch");
		};
		let Event::Patch(original) = patch else {
			panic!("a patch stays a patch");
		};
		assert_eq!(redacted.cause, original.cause);
		assert_eq!(redacted.label, original.label);

		assert_eq!(redacted.ops[0], Op::Set {
			h:     USER,
			prop:  PropId::Text.into(),
			value: Value::Str(Str::empty()),
		});
		assert_eq!(redacted.ops[1], original.ops[1]);
		let Op::Ins { node, .. } = &redacted.ops[2] else {
			panic!("the third operation is an insert");
		};
		assert_eq!(node.props[0].1, Value::Str(Str::empty()));
		assert_eq!(node.props[1].1, Value::Str(Str::new_static("live")));
		assert_eq!(node.content.as_deref(), Some(""));
		assert_eq!(redacted.ops[3], original.ops[3]);
		assert_eq!(redacted.ops[4], original.ops[4]);
	}

	#[test]
	fn a_set_on_a_handle_minted_by_the_same_patch_resolves_its_tag() {
		let base = seeded().snapshot();
		let minted = Handle::new(base.high_water() + 1).expect("the next handle is nonzero");
		let Event::Patch(redacted) = redact_event(
			Event::Patch(Patch {
				cause: EntryId::default(),
				prior: None,
				label: None,
				ops:   vec![
					Op::Ins { parent: TURN, after: None, node: NodeSpec::new(KnownTag::User) },
					Op::Set {
						h:     minted,
						prop:  PropId::Status.into(),
						value: Value::Str(Str::new_static("sent")),
					},
					Op::Set {
						h:     minted,
						prop:  PropId::Text.into(),
						value: Value::Str(Str::new_static("secret")),
					},
				],
			}),
			&base,
			&TextPolicy,
		) else {
			panic!("a patch stays a patch");
		};
		assert_eq!(redacted.ops[1], Op::Set {
			h:     minted,
			prop:  PropId::Status.into(),
			value: Value::Str(Str::new_static("sent")),
		});
		assert_eq!(redacted.ops[2], Op::Set {
			h:     minted,
			prop:  PropId::Text.into(),
			value: Value::Str(Str::empty()),
		});
	}

	#[test]
	fn stream_deltas_are_blanked_by_their_bound_property() {
		let mut dom = seeded();
		let secret = dom
			.stream_open(EntryId::default(), ASSISTANT, PropId::Text.into())
			.expect("assistant text accepts a stream");
		let public = dom
			.stream_open(EntryId::default(), ASSISTANT, PropId::Status.into())
			.expect("assistant status accepts a stream");
		let base = dom.snapshot();

		let append = |sid| Event::Stream {
			cause: EntryId::default(),
			sid,
			op: StreamOp::Append,
			node: None,
			prop: None,
			text: Some(Str::new_static("delta")),
		};
		assert_eq!(redact_event(append(secret), &base, &TextPolicy), Event::Stream {
			cause: EntryId::default(),
			sid:   secret,
			op:    StreamOp::Append,
			node:  None,
			prop:  None,
			text:  Some(Str::empty()),
		});
		assert_eq!(redact_event(append(public), &base, &TextPolicy), append(public));

		let open = Event::Stream {
			cause: EntryId::default(),
			sid:   99,
			op:    StreamOp::Open,
			node:  Some(ASSISTANT),
			prop:  Some(PropId::Text.into()),
			text:  None,
		};
		assert_eq!(redact_event(open.clone(), &base, &TextPolicy), open);
	}

	#[test]
	fn an_unresolvable_stream_delta_is_blanked() {
		let base = seeded().snapshot();
		assert_eq!(
			redact_event(
				Event::Stream {
					cause: EntryId::default(),
					sid:   404,
					op:    StreamOp::Append,
					node:  None,
					prop:  None,
					text:  Some(Str::new_static("secret")),
				},
				&base,
				&TextPolicy,
			),
			Event::Stream {
				cause: EntryId::default(),
				sid:   404,
				op:    StreamOp::Append,
				node:  None,
				prop:  None,
				text:  Some(Str::empty()),
			}
		);
	}
}
