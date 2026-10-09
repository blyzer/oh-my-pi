//! Journal, reasoning-level and event-filter projections for the RPC
//! commands that read session history (`get_entries`, `get_tree`), discover
//! reasoning levels (`get_available_thinking_levels`) and narrow session
//! event frames (`set_event_filter`).

use omp_catalog::{Catalog, ThinkingEffort};
use omp_core::{FastHashMap, FastHashSet};
use omp_journal::{Entry, EntryId, KindName};
use omp_session::Session;
use serde_json::{Map, Value, json};
use strum::{EnumString, IntoStaticStr};
use thiserror::Error;

/// `get_entries` names a `since` entry the journal does not hold.
#[derive(Debug, Error)]
#[error("unknown `since` entry id")]
pub(super) struct UnknownSince;

/// One journal entry as an RPC value: its kind, identity, branch parent
/// (`prior`, else the previous entry in the file), causing entry, label and
/// payload.
fn entry_value(entry: &Entry, parent: Option<EntryId>) -> Value {
	let data = serde_json::from_str::<Value>(entry.data.as_str())
		.unwrap_or_else(|_| Value::String(entry.data.as_str().to_owned()));
	let mut value = Map::new();
	value.insert("type".into(), Value::String(entry.kind.name.as_str().to_owned()));
	value.insert("rev".into(), entry.kind.rev.into());
	value.insert("id".into(), Value::String(entry.id.to_string()));
	value.insert(
		"parentId".into(),
		parent.map_or(Value::Null, |parent| Value::String(parent.to_string())),
	);
	if let Some(by) = entry.by {
		value.insert("causeId".into(), Value::String(by.to_string()));
	}
	if let Some(label) = &entry.label {
		value.insert("label".into(), Value::String(label.as_str().to_owned()));
	}
	value.insert("data".into(), data);
	Value::Object(value)
}

/// Every entry paired with its branch parent, in append order.
fn with_parents(session: &Session) -> impl Iterator<Item = (&Entry, Option<EntryId>)> + '_ {
	let mut previous = None;
	session.entries().map(move |entry| {
		let parent = entry.prior.or(previous);
		previous = Some(entry.id);
		(entry, parent)
	})
}

fn leaf_value(session: &Session) -> Value {
	session
		.head()
		.map_or(Value::Null, |head| Value::String(head.to_string()))
}

/// `get_entries`: the canonical append history (every branch), optionally
/// only the entries strictly after `since`, with the selected head.
pub(super) fn entries(session: &Session, since: Option<&str>) -> Result<Value, UnknownSince> {
	let skip = match since {
		None => 0,
		Some(since) => {
			let since = since.parse::<EntryId>().map_err(|_| UnknownSince)?;
			session
				.entries()
				.position(|entry| entry.id == since)
				.ok_or(UnknownSince)?
				+ 1
		},
	};
	let entries: Vec<Value> = with_parents(session)
		.skip(skip)
		.map(|(entry, parent)| entry_value(entry, parent))
		.collect();
	Ok(json!({ "entries": entries, "leafId": leaf_value(session) }))
}

/// Whether an entry is a node of the message tree: user and assistant
/// messages and compaction boundaries. Streamed text, tool progress, patches
/// and receipts belong to the message that caused them.
fn tree_kind(entry: &Entry) -> bool {
	entry.kind.rev == 1
		&& matches!(
			entry.kind.name.as_str().parse::<KindName>(),
			Ok(KindName::MsgUser | KindName::MsgAssistantStart | KindName::Compaction)
		)
}

/// `get_tree`: the message tree over every branch, as a flat list in append
/// order. Each node is `{ entry, children }`, where `entry.parentId` is the
/// nearest message ancestor and `children` lists the ids of its message
/// children; roots have a null `parentId`.
///
/// The tree is flat by design. A nested tree is as deep as the conversation
/// is long, and serializing nested JSON recurses once per level: a 1 000-level
/// tree overflowed a 2 MiB stack when measured, and decoders such as
/// Python's `json` refuse far less. Hosts rebuild nesting from `children`.
pub(super) fn tree(session: &Session) -> Value {
	// Nearest message ancestor of every entry, message or not.
	let mut anchor = FastHashMap::<EntryId, Option<EntryId>>::default();
	let mut nodes = Vec::<(&Entry, Option<EntryId>)>::new();
	for (entry, parent) in with_parents(session) {
		let inherited = parent.and_then(|parent| anchor.get(&parent).copied().flatten());
		if tree_kind(entry) {
			anchor.insert(entry.id, Some(entry.id));
			nodes.push((entry, inherited));
		} else {
			anchor.insert(entry.id, inherited);
		}
	}
	let mut children = FastHashMap::<EntryId, Vec<Value>>::default();
	for (entry, parent) in &nodes {
		if let Some(parent) = parent {
			children
				.entry(*parent)
				.or_default()
				.push(Value::String(entry.id.to_string()));
		}
	}
	let tree: Vec<Value> = nodes
		.iter()
		.map(|(entry, parent)| {
			json!({
				"entry": entry_value(entry, *parent),
				"children": children.remove(&entry.id).unwrap_or_default(),
			})
		})
		.collect();
	json!({ "tree": tree, "leafId": leaf_value(session) })
}

/// `get_available_thinking_levels`: `off` first, then the efforts the live
/// model's catalog policy supports, least to most intensive. A model without
/// a reasoning policy (or an unknown one) offers only `off`.
pub(super) fn thinking_levels(catalog: Option<&Catalog>, model: &str) -> Vec<&'static str> {
	let efforts = catalog
		.and_then(|catalog| {
			let spec = catalog
				.models()
				.iter()
				.find(|spec| spec.key.as_str() == model)?;
			catalog.thinking_policy(spec.thinking.as_ref()?)
		})
		.map(|policy| policy.efforts.as_slice())
		.unwrap_or_default();
	let mut levels = vec![<&'static str>::from(ThinkingEffort::Off)];
	levels.extend(
		efforts
			.iter()
			.filter(|effort| **effort != ThinkingEffort::Off)
			.map(|effort| <&'static str>::from(*effort)),
	);
	levels
}

/// `set_event_filter` projection of `message_update` frames.
#[derive(Clone, Copy, Debug, Default, EnumString, IntoStaticStr, PartialEq, Eq)]
#[strum(serialize_all = "lowercase")]
pub(super) enum MessageUpdates {
	/// Keep the accumulated `message` and `assistantMessageEvent.partial`.
	#[default]
	Full,
	/// Keep only the increment: `message` narrows to `{ role }` and
	/// `partial` is dropped.
	Delta,
}

/// An invalid `set_event_filter` request; neither setting changes.
#[derive(Debug, Error)]
pub(super) enum EventFilterError {
	/// `events` is neither null nor an array of non-empty strings.
	#[error("events must be null or an array of non-empty event type strings")]
	Events,
	/// `messageUpdates` is not `"full"` or `"delta"`.
	#[error("messageUpdates must be \"full\" or \"delta\"")]
	MessageUpdates,
}

/// Which session event frames reach the host, and how `message_update`
/// frames are projected. Only session events pass through it: responses,
/// requests, subagent frames and command side channels are never filtered.
#[derive(Debug, Default)]
pub(super) struct EventFilter {
	events:          Option<FastHashSet<String>>,
	message_updates: MessageUpdates,
}

impl EventFilter {
	/// Replaces the whole filter from a `set_event_filter` request and
	/// returns the response payload echoing the active selection.
	pub(super) fn replace(
		&mut self,
		params: &Map<String, Value>,
	) -> Result<Value, EventFilterError> {
		let events = match params.get("events") {
			None | Some(Value::Null) => None,
			Some(Value::Array(values)) => Some(
				values
					.iter()
					.map(|value| {
						value
							.as_str()
							.filter(|value| !value.is_empty())
							.map(str::to_owned)
							.ok_or(EventFilterError::Events)
					})
					.collect::<Result<FastHashSet<_>, _>>()?,
			),
			Some(_) => return Err(EventFilterError::Events),
		};
		let message_updates = match params.get("messageUpdates") {
			None => MessageUpdates::Full,
			Some(value) => value
				.as_str()
				.and_then(|value| value.parse().ok())
				.ok_or(EventFilterError::MessageUpdates)?,
		};
		let echoed = params
			.get("events")
			.filter(|events| !events.is_null())
			.cloned()
			.unwrap_or(Value::Null);
		self.events = events;
		self.message_updates = message_updates;
		Ok(json!({
			"events": echoed,
			"messageUpdates": <&'static str>::from(message_updates),
		}))
	}

	/// Projects one session event frame, or drops it.
	pub(super) fn apply(&self, mut frame: Value) -> Option<Value> {
		let kind = frame.get("type").and_then(Value::as_str);
		if let Some(events) = &self.events
			&& !kind.is_some_and(|kind| events.contains(kind))
		{
			return None;
		}
		if self.message_updates == MessageUpdates::Delta && kind == Some("message_update") {
			let role = frame
				.pointer("/message/role")
				.cloned()
				.unwrap_or(Value::Null);
			frame["message"] = json!({ "role": role });
			if let Some(event) = frame
				.get_mut("assistantMessageEvent")
				.and_then(Value::as_object_mut)
			{
				event.remove("partial");
			}
		}
		Some(frame)
	}
}

#[cfg(test)]
mod tests {
	use omp_catalog::Catalog;
	use omp_journal::EntryId;
	use omp_session::{ComponentRegistry, Session};
	use serde_json::{Value, json};

	use super::{EventFilter, EventFilterError, entries, thinking_levels, tree};

	/// `one`, then `two`, then a rewind to `one` and `three`: two branches.
	fn branched(temp: &tempfile::TempDir) -> (Session, [EntryId; 3]) {
		let mut session =
			Session::create(temp.path().join("history.oms"), ComponentRegistry::standard())
				.expect("session");
		session.begin_turn().expect("turn");
		let one = session.user("one", Vec::new()).expect("one");
		session.begin_turn().expect("turn");
		let two = session.user("two", Vec::new()).expect("two");
		session.rewind(one).expect("rewind");
		session.begin_turn().expect("turn");
		let three = session.user("three", Vec::new()).expect("three");
		(session, [one, two, three])
	}

	fn id(entry: EntryId) -> Value {
		Value::String(entry.to_string())
	}

	#[test]
	fn entries_cover_every_branch_with_parents_and_the_selected_head() {
		let temp = tempfile::tempdir().expect("tempdir");
		let (session, [one, two, three]) = branched(&temp);
		let all = entries(&session, None).expect("entries");
		let listed = all["entries"].as_array().expect("entries array");
		assert_eq!(listed.len(), session.entry_count(), "the whole append history");
		assert_eq!(listed[0]["type"], "journal");
		assert_eq!(listed[0]["parentId"], Value::Null);
		assert_eq!(all["leafId"], id(three));
		let user = |entry: EntryId| {
			listed
				.iter()
				.find(|value| value["id"] == id(entry))
				.expect("listed")
		};
		assert_eq!(user(two)["type"], "msg.user");
		assert_eq!(user(two)["data"]["text"], "two", "the payload is JSON, not a string");

		let after = entries(&session, Some(&two.to_string())).expect("entries after two");
		let after = after["entries"].as_array().expect("entries array");
		assert_eq!(after.len(), 2, "the rewound turn start and `three`: {after:#?}");
		assert_eq!(after[0]["type"], "turn.start");
		assert_eq!(after[0]["parentId"], id(one), "the rewind made `one` its branch parent");
		assert_eq!(after[1]["id"], id(three));

		assert!(entries(&session, Some("not-an-id")).is_err());
		assert!(entries(&session, Some(&EntryId::default().to_string())).is_err());
	}

	#[test]
	fn the_tree_links_messages_across_branches_without_nesting() {
		let temp = tempfile::tempdir().expect("tempdir");
		let (session, [one, two, three]) = branched(&temp);
		let value = tree(&session);
		assert_eq!(value["leafId"], id(three));
		let nodes = value["tree"].as_array().expect("tree array");
		let ids: Vec<&Value> = nodes.iter().map(|node| &node["entry"]["id"]).collect();
		assert_eq!(ids, [&id(one), &id(two), &id(three)], "messages only, in append order");
		assert_eq!(nodes[0]["entry"]["parentId"], Value::Null, "`one` is the root");
		assert_eq!(nodes[0]["children"], json!([id(two), id(three)]));
		assert_eq!(nodes[1]["entry"]["parentId"], id(one));
		assert_eq!(nodes[2]["entry"]["parentId"], id(one), "the branch hangs off `one`");
		assert_eq!(nodes[1]["children"], json!([]));
	}

	#[test]
	fn thinking_levels_follow_the_live_models_policy() {
		let catalog = Catalog::embedded();
		let (spec, policy) = catalog
			.models()
			.iter()
			.find_map(|spec| Some((spec, catalog.thinking_policy(spec.thinking.as_ref()?)?)))
			.expect("a reasoning model in the embedded catalog");
		let mut expected = vec!["off"];
		expected.extend(
			policy
				.efforts
				.iter()
				.filter(|effort| **effort != omp_catalog::ThinkingEffort::Off)
				.map(|effort| <&'static str>::from(*effort)),
		);
		assert_eq!(thinking_levels(Some(catalog), spec.key.as_str()), expected);
		assert!(expected.len() > 1, "{expected:?}");
		assert_eq!(thinking_levels(Some(catalog), "nobody/no-model"), ["off"]);
		assert_eq!(thinking_levels(None, spec.key.as_str()), ["off"]);
	}

	fn params(value: serde_json::Value) -> serde_json::Map<String, serde_json::Value> {
		value.as_object().expect("object").clone()
	}

	fn update() -> serde_json::Value {
		json!({
			"type": "message_update",
			"message": { "role": "assistant", "content": [{ "type": "text", "text": "hello" }] },
			"assistantMessageEvent": {
				"type": "text_delta",
				"contentIndex": 0,
				"delta": "lo",
				"partial": { "role": "assistant" },
			},
		})
	}

	#[test]
	fn the_default_filter_forwards_every_frame_unchanged() {
		let filter = EventFilter::default();
		assert_eq!(filter.apply(update()), Some(update()));
		assert_eq!(
			filter.apply(json!({ "type": "tool_execution_start" })),
			Some(json!({ "type": "tool_execution_start" }))
		);
	}

	#[test]
	fn an_event_list_forwards_only_the_named_types() {
		let mut filter = EventFilter::default();
		let echoed = filter
			.replace(&params(json!({ "events": ["message_end"] })))
			.expect("valid filter");
		assert_eq!(echoed, json!({ "events": ["message_end"], "messageUpdates": "full" }));
		assert_eq!(filter.apply(update()), None);
		assert_eq!(
			filter.apply(json!({ "type": "message_end" })),
			Some(json!({ "type": "message_end" }))
		);
		assert_eq!(filter.apply(json!({ "untyped": true })), None);
	}

	#[test]
	fn delta_updates_keep_only_the_increment() {
		let mut filter = EventFilter::default();
		let echoed = filter
			.replace(&params(json!({ "events": null, "messageUpdates": "delta" })))
			.expect("valid filter");
		assert_eq!(echoed, json!({ "events": null, "messageUpdates": "delta" }));
		assert_eq!(
			filter.apply(update()),
			Some(json!({
				"type": "message_update",
				"message": { "role": "assistant" },
				"assistantMessageEvent": { "type": "text_delta", "contentIndex": 0, "delta": "lo" },
			}))
		);
		let end = json!({ "type": "message_end", "message": { "role": "assistant", "content": [] } });
		assert_eq!(filter.apply(end.clone()), Some(end));
	}

	#[test]
	fn each_request_replaces_the_whole_filter_and_invalid_ones_change_nothing() {
		let mut filter = EventFilter::default();
		filter
			.replace(&params(json!({ "events": ["message_end"], "messageUpdates": "delta" })))
			.expect("valid filter");
		assert!(matches!(
			filter.replace(&params(json!({ "events": [""] }))),
			Err(EventFilterError::Events)
		));
		assert!(matches!(
			filter.replace(&params(json!({ "events": "message_end" }))),
			Err(EventFilterError::Events)
		));
		assert!(matches!(
			filter.replace(&params(json!({ "events": null, "messageUpdates": "partial" }))),
			Err(EventFilterError::MessageUpdates)
		));
		assert_eq!(
			filter.apply(json!({ "type": "agent_start" })),
			None,
			"the refused requests left the list"
		);
		filter
			.replace(&params(json!({ "events": null })))
			.expect("valid filter");
		assert_eq!(
			filter.apply(update()),
			Some(update()),
			"omitting messageUpdates resets it to full"
		);
	}
}
