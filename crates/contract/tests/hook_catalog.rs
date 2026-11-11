//! Holds the hook-event descriptor table to the hand-kept catalogs it
//! describes (ADR 0046 item 3, ADR 0049 Phase 5b): the frozen `HookEventId`
//! enum in `toolhost.proto`, the Python `omp.events` spec table and
//! `omp.hooks` declaration list, and the Claude plugin `HookSeam` mapping in
//! `omp-ext`. The protobuf and Python sources are read as text, so no crate
//! gains a dependency edge.

use std::collections::{BTreeMap, BTreeSet};

use omp_contract::{
	AgentSettledEvent, GateEvent, HOOK_EVENTS, HookEvent, HookEventDescriptor, ProviderErrorEvent,
};
use omp_ext::claude_hooks::{ClaudeHookEvent, NotificationType};
use omp_proto::toolhost::v1::HookEventId;
use strum::VariantArray;

const TOOLHOST_PROTO: &str = include_str!("../../proto/proto/omp/toolhost/v1/toolhost.proto");
const EVENTS_PY: &str = include_str!("../../py/python/omp/events.py");
const HOOKS_PY: &str = include_str!("../../py/python/omp/hooks.py");

/// The stable event name of `id`: its protobuf name without `HOOK_EVENT_`,
/// lowercased.
fn event_name(id: HookEventId) -> String {
	id.as_str_name()
		.strip_prefix("HOOK_EVENT_")
		.unwrap_or_else(|| panic!("{id:?} has no HOOK_EVENT_ prefix"))
		.to_ascii_lowercase()
}

/// How many descriptors carry the event named `name`.
fn descriptors_named(name: &str) -> usize {
	HOOK_EVENTS
		.iter()
		.filter(|row| event_name(row.id) == name)
		.count()
}

/// Every `NAME = number;` value of `enum HookEventId` and its `reserved`
/// numbers, read from the `.proto` source the generated enum is built from.
fn proto_hook_event_ids() -> (Vec<(&'static str, i32)>, Vec<i32>) {
	let (_, rest) = TOOLHOST_PROTO
		.split_once("enum HookEventId {")
		.expect("toolhost.proto declares enum HookEventId");
	let (body, _) = rest.split_once('}').expect("enum HookEventId is closed");
	let number = |text: &str, line: &str| -> i32 {
		text
			.trim()
			.parse()
			.unwrap_or_else(|_| panic!("unparsed HookEventId line {line:?}"))
	};
	let mut values = Vec::new();
	let mut reserved = Vec::new();
	for line in body.lines() {
		let statement = line.split_once("//").map_or(line, |(code, _)| code).trim();
		if statement.is_empty() {
			continue;
		}
		let statement = statement
			.strip_suffix(';')
			.unwrap_or_else(|| panic!("unparsed HookEventId line {line:?}"));
		if let Some(numbers) = statement.strip_prefix("reserved ") {
			reserved.extend(numbers.split(',').map(|text| number(text, line)));
			continue;
		}
		let (name, value) = statement
			.split_once('=')
			.unwrap_or_else(|| panic!("unparsed HookEventId line {line:?}"));
		values.push((name.trim(), number(value, line)));
	}
	(values, reserved)
}

/// The contents of a Python string literal.
fn unquote(literal: &str) -> &str {
	literal
		.strip_prefix('"')
		.and_then(|inner| inner.strip_suffix('"'))
		.unwrap_or_else(|| panic!("{literal:?} is not a string literal"))
}

/// The items of the module-level Python tuple `name = (...)`: string
/// literals as `Some`, `None` as `None`.
fn python_tuple(source: &'static str, name: &str) -> Vec<Option<&'static str>> {
	let (_, rest) = source
		.split_once(&format!("\n{name} = (\n"))
		.unwrap_or_else(|| panic!("{name} is not a module-level tuple"));
	let (body, _) = rest
		.split_once("\n)")
		.unwrap_or_else(|| panic!("{name} is not closed"));
	body
		.split(',')
		.map(str::trim)
		.filter(|item| !item.is_empty())
		.map(|item| (item != "None").then(|| unquote(item)))
		.collect()
}

/// The one-line `"key": value,` rows of the Python dict literal between
/// `opening` and `closing`, comment lines skipped and keys unquoted.
fn python_dict(
	source: &'static str,
	opening: &str,
	closing: &str,
) -> Vec<(&'static str, &'static str)> {
	let (_, rest) = source
		.split_once(opening)
		.unwrap_or_else(|| panic!("{opening:?} not found"));
	let (body, _) = rest
		.split_once(closing)
		.unwrap_or_else(|| panic!("{opening:?} is not closed"));
	body
		.lines()
		.map(str::trim)
		.filter(|line| !line.is_empty() && !line.starts_with('#'))
		.map(|line| {
			let (key, value) = line
				.split_once(": ")
				.unwrap_or_else(|| panic!("unparsed dict row {line:?}"));
			(unquote(key), value.strip_suffix(',').unwrap_or(value))
		})
		.collect()
}

/// The payload revision `omp.events._make_spec` assigns `event`: its `rev=`
/// argument is `<n>` or `<n> if name == "<event>" else <default>`.
fn python_revision(event: &str) -> u32 {
	let expression = EVENTS_PY
		.lines()
		.find_map(|line| line.trim().strip_prefix("rev="))
		.and_then(|expression| expression.strip_suffix(','))
		.expect("omp.events._make_spec passes rev=");
	let number = |text: &str| -> u32 {
		text
			.trim()
			.parse()
			.unwrap_or_else(|_| panic!("unparsed rev={expression}"))
	};
	let Some((special, rest)) = expression.split_once(" if name == ") else {
		return number(expression);
	};
	let (name, default) = rest
		.split_once(" else ")
		.unwrap_or_else(|| panic!("unparsed rev={expression}"));
	number(if unquote(name.trim()) == event {
		special
	} else {
		default
	})
}

/// The catalog revision of the event `E` encodes.
fn catalog_revision<E: HookEvent>() -> Option<u32> {
	HookEventDescriptor::of(E::ID).map(|row| row.revision)
}

#[test]
fn every_hook_event_id_has_one_descriptor_and_keeps_its_wire_id() {
	let (values, reserved) = proto_hook_event_ids();
	let mut described = 0;
	for (name, number) in values {
		let id = HookEventId::from_str_name(name)
			.unwrap_or_else(|| panic!("the generated HookEventId lacks {name}"));
		assert_eq!(i32::from(id), number, "{name} disagrees with the generated wire id");
		let rows = HOOK_EVENTS.iter().filter(|row| row.id == id).count();
		if id == HookEventId::HookEventUnspecified {
			assert_eq!(rows, 0, "{name} is not an event");
			assert!(HookEventDescriptor::of(id).is_none(), "{name} is not an event");
		} else {
			assert_eq!(rows, 1, "{name} needs exactly one descriptor");
			assert_eq!(HookEventDescriptor::of(id).map(|row| row.id), Some(id));
			described += 1;
		}
	}
	assert_eq!(described, HOOK_EVENTS.len(), "a descriptor has no declared HookEventId");
	for number in reserved {
		assert!(HookEventId::try_from(number).is_err(), "reserved {number} is a live variant");
	}
}

#[test]
fn descriptors_ascend_by_wire_id_and_name_a_typed_payload() {
	for pair in HOOK_EVENTS.windows(2) {
		assert!(
			i32::from(pair[0].id) < i32::from(pair[1].id),
			"{:?} is out of wire order",
			pair[1].id,
		);
	}
	for row in HOOK_EVENTS {
		assert!(row.revision >= 1, "{:?} has no schema revision", row.id);
		let mut characters = row.payload.chars();
		assert!(
			characters
				.next()
				.is_some_and(|first| first.is_ascii_uppercase())
				&& characters.all(|character| character.is_ascii_alphanumeric()),
			"{:?} payload {:?} is not a type name",
			row.id,
			row.payload,
		);
	}
}

#[test]
fn omp_events_catalog_matches_every_descriptor() {
	let payloads: BTreeMap<_, _> = python_dict(EVENTS_PY, "\n_EVENT_METADATA = {\n", "\n}")
		.into_iter()
		.map(|(name, row)| {
			let payload = row
				.strip_prefix('(')
				.and_then(|row| row.split_once(','))
				.map(|(payload, _)| payload.trim())
				.unwrap_or_else(|| panic!("unparsed _EVENT_METADATA row for {name}"));
			(name, payload)
		})
		.collect();
	assert_eq!(payloads.len(), HOOK_EVENTS.len(), "_EVENT_METADATA rows");
	let mut matched = 0;
	for (index, name) in python_tuple(EVENTS_PY, "_EVENT_NAMES_BY_ID")
		.into_iter()
		.enumerate()
	{
		let wire = i32::try_from(index + 1).expect("wire ids fit in i32");
		let row = HookEventId::try_from(wire)
			.ok()
			.and_then(HookEventDescriptor::of);
		match (name, row) {
			(Some(name), Some(row)) => {
				assert_eq!(event_name(row.id), name, "omp.events wire id {wire}");
				assert_eq!(payloads.get(name), Some(&row.payload), "{name} payload type");
				assert_eq!(python_revision(name), row.revision, "{name} schema revision");
				matched += 1;
			},
			// A tombstone on both sides: no Python name, no protobuf variant.
			(None, None) => {},
			(name, row) => panic!("wire id {wire}: omp.events has {name:?}, the catalog {row:?}"),
		}
	}
	assert_eq!(matched, HOOK_EVENTS.len(), "omp.events lacks a descriptor's event");
}

#[test]
fn omp_hooks_routes_every_descriptor_and_rejects_only_events_without_one() {
	let rejected: BTreeMap<_, _> =
		python_dict(HOOKS_PY, "\n_REJECTED_EVENTS = types.MappingProxyType(\n    {\n", "\n    }\n)")
			.into_iter()
			.map(|(name, reason)| (name, unquote(reason)))
			.collect();
	let mut routed = BTreeSet::new();
	let mut unemitted = BTreeSet::new();
	for name in python_tuple(HOOKS_PY, "_EVENT_NAMES") {
		let name = name.expect("omp.hooks._EVENT_NAMES holds only names");
		match (descriptors_named(name), rejected.get(name)) {
			(1, None) => assert!(routed.insert(name), "omp.hooks declares {name} twice"),
			// An explicit state, not an error: `omp.hooks` declares the name but
			// refuses its registration until a `HookEventId` and a production
			// emitter exist.
			(0, Some(reason)) => {
				assert!(reason.starts_with("no HookEventId"), "{name} is rejected for {reason:?}");
				assert!(unemitted.insert(name), "omp.hooks declares {name} twice");
			},
			(rows, reason) => panic!("{name}: {rows} descriptors, rejection {reason:?}"),
		}
	}
	assert_eq!(routed.len(), HOOK_EVENTS.len(), "omp.hooks lacks a descriptor's event");
	assert_eq!(unemitted, rejected.keys().copied().collect::<BTreeSet<_>>());
}

#[test]
fn every_claude_hook_seam_runs_on_one_described_event() {
	let seams: BTreeSet<&'static str> = ClaudeHookEvent::VARIANTS
		.iter()
		.copied()
		.filter_map(ClaudeHookEvent::seam)
		.chain(
			NotificationType::VARIANTS
				.iter()
				.copied()
				.filter_map(NotificationType::seam),
		)
		.map(<&'static str>::from)
		.collect();
	assert!(!seams.is_empty(), "no Claude hook event maps onto an omp seam");
	for seam in seams {
		assert_eq!(descriptors_named(seam), 1, "the {seam} seam needs exactly one descriptor");
	}
}

#[test]
fn contract_hook_events_match_their_catalog_revision() {
	assert_eq!(catalog_revision::<GateEvent>(), Some(GateEvent::REV));
	assert_eq!(catalog_revision::<ProviderErrorEvent>(), Some(ProviderErrorEvent::REV));
	// A recorded divergence this slice leaves unchanged: `omp.events` and
	// `docs/py/05-hooks.md` declare `agent_settled` revision 2, which Python
	// hook declarations carry to envd, while the in-process producer still
	// stamps revision 1 on its dispatch.
	assert_eq!(catalog_revision::<AgentSettledEvent>(), Some(2));
	assert_eq!(AgentSettledEvent::REV, 1);
}
