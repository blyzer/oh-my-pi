//! Host-local property visibility for read-only control observers.
//!
//! ADR 0039 A4 splits session facts into [`VisibilityClass`] buckets and
//! blanks the `HostLocal` ones in place for an observer whose grant lacks the
//! `observe_host_local` scope. [`ControlVisibility`] is the
//! [`PropPolicy`](omp_dom::PropPolicy) that makes that decision, so
//! [`Snapshot::redact`](omp_dom::Snapshot::redact) and
//! [`redact_event`](omp_dom::redact_event) keep handles, parents, child order,
//! and property keys aligned with the owner's unredacted stream.
//!
//! # Table syntax
//!
//! The table is a list of `<tag>/<prop>` entries carried by the
//! [`SV_CONTROL_HOST_LOCAL_PROPS`] convar, so the owner changes the rules
//! without a recompile:
//!
//! - `*/provider` blanks the `provider` property of every element.
//! - `assistant/route` blanks it only on `<assistant>` elements.
//! - `tool:grep/custom:root` names a tool element and a tool-defined property,
//!   using the same `tool:` and `custom:` prefixes the DOM wire format uses.
//!
//! `*` is accepted in the tag half only. A half written without a prefix must
//! name a [`KnownTag`] or a [`PropId`]. `Tag::from_str` and
//! `PropKey::from_str` are infallible and fall back to the custom form, so
//! without that requirement `*/pathh`, `*/call_id`, and `usr/path` would all
//! parse into rules that are reported valid and match nothing, silently
//! narrowing the policy. An entry that is malformed, empty on either side,
//! wildcards the property half, or names an unknown tag or property is
//! rejected as a [`VisibilityTableError`] naming the entry, rather than being
//! dropped (which would narrow the policy) or read as a literal element or
//! property name (which would widen it).
//!
//! The convar hook returns that error, but [`Ctx`] keeps only the fact of the
//! veto: an operator's console sees only that the variable rejected the
//! supplied value, not which entry was at fault. The named entry reaches any
//! caller of [`ControlVisibility::from_entries`] or
//! [`ControlVisibility::from_ctx`].
//!
//! # Default table
//!
//! The default blanks the raw provider metadata the fold writes onto
//! `<assistant>` and `<usage>` elements: the provider identity and the
//! resolved route.
//!
//! # Known gaps
//!
//! Two host-local facts A4 names stay visible in R1.
//!
//! Local filesystem paths never reach a property of their own. A tool call's
//! arguments are the *content* of its `<input>` child, and a tool result is
//! [`PropId::Text`] and [`PropId::Data`] on `<result>`, so a path arrives
//! embedded in a payload that a property-keyed table cannot separate from the
//! rest of it. Blanking those wholesale would empty the surface an observer
//! exists to watch, so this module does not, and paths remain visible.
//!
//! The advisor transcript card *is* identifiable in the DOM: the advisor
//! Director writes it as a [`KnownTag::Notice`] element carrying
//! `kind = advisor` beside a [`PropId::Data`] payload and a content body.
//! [`PropPolicy::redacts_content`] receives only a [`Tag`], so a rule
//! conditioned on a property value cannot be expressed, and widening to
//! `notice/data` would swallow every unrelated notice. Classifying the card
//! needs a wider [`PropPolicy`] signature, which is a separate unit, so R1
//! does not redact it.

use std::{collections::BTreeSet, str::FromStr};

use omp_collab::host::VisibilityClass;
use omp_con::Ctx;
use omp_core::{Str, sf};
use omp_dom::{KnownTag, PropId, PropKey, PropPolicy, Tag};
use thiserror::Error;

/// Separates the tag half of a table entry from its property half.
const ENTRY_SEPARATOR: char = '/';

/// Tag half that matches every element.
const ANY_TAG: &str = "*";

/// Prefix that names a tool element, as in the DOM wire format.
const TOOL_PREFIX: &str = "tool:";

/// Prefix that names a tool-defined property, as in the DOM wire format.
const CUSTOM_PREFIX: &str = "custom:";

/// Seed table: the provider metadata a tag-and-property rule can identify.
///
/// Local paths and the advisor card are known gaps; see the module
/// documentation.
const DEFAULT_HOST_LOCAL_PROPS: &[&str] = &["*/provider", "*/route"];

omp_con::var! {
	/// Host-local property table for control observers, as `<tag>/<prop>`
	/// entries; `*` in the tag half matches every element.
	pub static SV_CONTROL_HOST_LOCAL_PROPS = sv_control_host_local_props: Vec<Str> {
		default: DEFAULT_HOST_LOCAL_PROPS
			.iter()
			.copied()
			.map(Str::new_static)
			.collect(),
		validate: |_ctx, value| {
			ControlVisibility::from_entries(value.iter().map(Str::as_str))
				.map(|_| ())
				.map_err(|error| sf!("{error}"))
		},
		flags: archive,
	};
}

/// A table entry that could not be read as a tag-and-property rule.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum VisibilityTableError {
	/// The entry is not exactly one tag half, one separator, one property half.
	#[error("host-local entry `{entry}` must have the form `<tag>/<prop>`")]
	Malformed {
		/// The rejected entry, verbatim.
		entry: Str,
	},
	/// The tag half is empty.
	#[error("host-local entry `{entry}` has an empty tag; write `*` to match every element")]
	EmptyTag {
		/// The rejected entry, verbatim.
		entry: Str,
	},
	/// The property half is empty.
	#[error("host-local entry `{entry}` has an empty property")]
	EmptyProp {
		/// The rejected entry, verbatim.
		entry: Str,
	},
	/// The property half is `*`, which this table does not define.
	#[error("host-local entry `{entry}` wildcards the property; `*` is accepted in the tag only")]
	WildcardProp {
		/// The rejected entry, verbatim.
		entry: Str,
	},
	/// The tag half is neither `*`, a known tag, nor `tool:`-prefixed.
	#[error(
		"host-local entry `{entry}` names unknown element `{tag}`; write `tool:{tag}` for a tool \
		 element"
	)]
	UnknownTag {
		/// The rejected entry, verbatim.
		entry: Str,
		/// The tag half that named nothing.
		tag:   Str,
	},
	/// The property half is neither a known property nor `custom:`-prefixed.
	#[error(
		"host-local entry `{entry}` names unknown property `{prop}`; write `custom:{prop}` for a \
		 tool-defined property"
	)]
	UnknownProp {
		/// The rejected entry, verbatim.
		entry: Str,
		/// The property half that named nothing.
		prop:  Str,
	},
}

/// One parsed rule: a property, optionally narrowed to a single element.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum Rule {
	/// The property is host-local on every element.
	AnyTag(PropKey),
	/// The property is host-local on one element only.
	Exact(Tag, PropKey),
}

/// Classifies node properties for an observer without `observe_host_local`.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ControlVisibility {
	rules: BTreeSet<Rule>,
}

impl ControlVisibility {
	/// Reads the table from the [`SV_CONTROL_HOST_LOCAL_PROPS`] convar.
	///
	/// # Errors
	/// Returns the first entry that is not a tag-and-property rule.
	pub fn from_ctx(ctx: &Ctx) -> Result<Self, VisibilityTableError> {
		let entries = SV_CONTROL_HOST_LOCAL_PROPS.get(ctx);
		Self::from_entries(entries.iter().map(Str::as_str))
	}

	/// Builds the table from explicit `<tag>/<prop>` entries.
	///
	/// # Errors
	/// Returns the first entry that is not a tag-and-property rule.
	pub fn from_entries<'entry>(
		entries: impl IntoIterator<Item = &'entry str>,
	) -> Result<Self, VisibilityTableError> {
		entries
			.into_iter()
			.map(parse_rule)
			.collect::<Result<BTreeSet<_>, _>>()
			.map(|rules| Self { rules })
	}

	/// Classifies property `prop` of a `tag` element.
	#[must_use]
	pub fn classify(&self, tag: &Tag, prop: &PropKey) -> VisibilityClass {
		if self.rules.contains(&Rule::AnyTag(prop.clone()))
			|| self.rules.contains(&Rule::Exact(tag.clone(), prop.clone()))
		{
			VisibilityClass::HostLocal
		} else {
			VisibilityClass::PublicTranscript
		}
	}
}

impl PropPolicy for ControlVisibility {
	fn redacts(&self, tag: &Tag, prop: &PropKey) -> bool {
		self.classify(tag, prop) == VisibilityClass::HostLocal
	}

	/// Element text content is never blanked in R1.
	///
	/// The content-bearing host-local fact A4 names is the advisor transcript
	/// card, and it is identifiable in the DOM: a [`KnownTag::Notice`] element
	/// carrying `kind = advisor` beside its [`PropId::Data`] payload. This
	/// signature receives only the [`Tag`], so a rule conditioned on a property
	/// *value* cannot be expressed here, and widening to every `<notice>` would
	/// blank unrelated notices. Classifying the card needs a wider
	/// [`PropPolicy`] signature, which is a separate unit.
	fn redacts_content(&self, _tag: &Tag) -> bool {
		false
	}
}

/// Reads one `<tag>/<prop>` entry.
fn parse_rule(entry: &str) -> Result<Rule, VisibilityTableError> {
	let Some((tag, prop)) = entry.split_once(ENTRY_SEPARATOR) else {
		return Err(VisibilityTableError::Malformed { entry: Str::new(entry) });
	};
	if prop.contains(ENTRY_SEPARATOR) {
		return Err(VisibilityTableError::Malformed { entry: Str::new(entry) });
	}
	if tag.is_empty() {
		return Err(VisibilityTableError::EmptyTag { entry: Str::new(entry) });
	}
	if prop.is_empty() {
		return Err(VisibilityTableError::EmptyProp { entry: Str::new(entry) });
	}
	if prop == ANY_TAG {
		return Err(VisibilityTableError::WildcardProp { entry: Str::new(entry) });
	}
	let prop = parse_prop(entry, prop)?;
	if tag == ANY_TAG {
		return Ok(Rule::AnyTag(prop));
	}
	Ok(Rule::Exact(parse_tag(entry, tag)?, prop))
}

/// Reads the tag half, which must be `tool:`-prefixed or name a [`KnownTag`].
fn parse_tag(entry: &str, tag: &str) -> Result<Tag, VisibilityTableError> {
	if let Some(name) = tag.strip_prefix(TOOL_PREFIX) {
		if name.is_empty() {
			return Err(VisibilityTableError::EmptyTag { entry: Str::new(entry) });
		}
		return Ok(Tag::Custom(Str::new(name)));
	}
	KnownTag::from_str(tag)
		.map(Tag::Known)
		.map_err(|_| VisibilityTableError::UnknownTag {
			entry: Str::new(entry),
			tag:   Str::new(tag),
		})
}

/// Reads the property half, which must be `custom:`-prefixed or name a
/// [`PropId`].
fn parse_prop(entry: &str, prop: &str) -> Result<PropKey, VisibilityTableError> {
	if let Some(name) = prop.strip_prefix(CUSTOM_PREFIX) {
		if name.is_empty() {
			return Err(VisibilityTableError::EmptyProp { entry: Str::new(entry) });
		}
		return Ok(PropKey::Custom(Str::new(name)));
	}
	PropId::from_str(prop)
		.map(PropKey::Known)
		.map_err(|_| VisibilityTableError::UnknownProp {
			entry: Str::new(entry),
			prop:  Str::new(prop),
		})
}

#[cfg(test)]
mod tests {
	use omp_dom::{Dom, Handle, NodeSpec, Op, Txn, Value};
	use omp_journal::EntryId;
	use serde_json::value::RawValue;

	use super::*;

	/// The owner's local path, as it reaches the DOM inside tool payloads.
	const LOCAL_PATH: &str = "/home/owner/secret";

	/// Tool arguments, which the fold writes as `<input>` content.
	const INPUT_CONTENT: &str = r#"{"path":"/home/owner/secret"}"#;

	/// Tool output, which the fold writes as `<result>` text.
	const RESULT_TEXT: &str = "read 3 lines of /home/owner/secret";

	/// The advisor card body the advisor Director writes onto its notice.
	const ADVISOR_NOTE: &str = "check the deadline";

	fn tool(name: &'static str) -> Tag {
		Tag::Custom(Str::new_static(name))
	}

	/// Handles into the turn [`seeded`] builds.
	struct Seeded {
		assistant: Handle,
		notice:    Handle,
		input:     Handle,
		result:    Handle,
	}

	fn insert(dom: &mut Dom, parent: Handle, node: NodeSpec) -> Handle {
		let after = dom.children(parent).last().copied();
		let applied = dom
			.apply(&Txn {
				cause: EntryId::default(),
				label: None,
				ops:   vec![Op::Ins { parent, after, node }],
			})
			.expect("the insert applies");
		applied.minted[0]
	}

	/// Seeds the turn shape the engine actually emits: an `<assistant>`
	/// carrying provider metadata, the advisor `<notice>` card, and a tool
	/// element whose local path lives in `<input>` content and `<result>` text.
	fn seeded() -> (Dom, Seeded) {
		let mut dom = Dom::new();
		let body = dom.body();
		let turn = insert(&mut dom, body, NodeSpec::new(KnownTag::Turn));
		let assistant = insert(
			&mut dom,
			turn,
			NodeSpec::new(KnownTag::Assistant)
				.with_prop(PropId::Model, Value::Str(Str::new_static("opus")))
				.with_prop(PropId::Provider, Value::Str(Str::new_static("anthropic")))
				.with_prop(PropId::Route, Value::Str(Str::new_static("http://127.0.0.1:8787/v1")))
				.with_prop(PropId::Text, Value::Str(Str::new_static("reading it now"))),
		);
		let notice = insert(
			&mut dom,
			turn,
			NodeSpec::new(KnownTag::Notice)
				.with_prop(PropId::Kind, Value::Str(Str::new_static("advisor")))
				.with_prop(
					PropId::Data,
					Value::Json(
						RawValue::from_string(r#"{"notes":[{"note":"check the deadline"}]}"#.to_owned())
							.expect("the advisor payload is valid JSON"),
					),
				)
				.with_content(ADVISOR_NOTE),
		);
		let call = insert(
			&mut dom,
			turn,
			NodeSpec::new(tool("read"))
				.with_prop(PropId::Id, Value::Str(Str::new_static("call-1")))
				.with_prop(PropId::Status, Value::Str(Str::new_static("ok"))),
		);
		let input =
			insert(&mut dom, call, NodeSpec::new(KnownTag::Input).with_content(INPUT_CONTENT));
		let result = insert(
			&mut dom,
			call,
			NodeSpec::new(KnownTag::Result)
				.with_prop(PropId::Text, Value::Str(Str::new_static(RESULT_TEXT))),
		);
		(dom, Seeded { assistant, notice, input, result })
	}

	fn default_policy() -> ControlVisibility {
		ControlVisibility::from_entries(DEFAULT_HOST_LOCAL_PROPS.iter().copied())
			.expect("the seeded default table parses")
	}

	#[test]
	fn default_table_blanks_provider_metadata_only() {
		let policy = default_policy();

		assert_eq!(
			policy.classify(&KnownTag::Assistant.into(), &PropId::Provider.into()),
			VisibilityClass::HostLocal
		);
		assert_eq!(
			policy.classify(&KnownTag::Assistant.into(), &PropId::Route.into()),
			VisibilityClass::HostLocal
		);
		assert_eq!(
			policy.classify(&KnownTag::Assistant.into(), &PropId::Model.into()),
			VisibilityClass::PublicTranscript
		);
		assert_eq!(
			policy.classify(&KnownTag::User.into(), &PropId::Text.into()),
			VisibilityClass::PublicTranscript
		);
		// `path` is in no rule: the engine never writes that property, so a
		// `*/path` entry would match nothing. See the module documentation.
		assert_eq!(
			policy.classify(&tool("read"), &PropId::Path.into()),
			VisibilityClass::PublicTranscript
		);
		assert!(policy.redacts(&KnownTag::Usage.into(), &PropId::Provider.into()));
		assert!(!policy.redacts(&KnownTag::Usage.into(), &PropId::Model.into()));
	}

	#[test]
	fn a_wildcard_entry_crosses_tags_and_an_exact_entry_does_not() {
		let policy = ControlVisibility::from_entries(["*/provider", "assistant/model"])
			.expect("both forms parse");

		assert!(policy.redacts(&tool("read"), &PropId::Provider.into()));
		assert!(policy.redacts(&KnownTag::Assistant.into(), &PropId::Provider.into()));
		assert!(policy.redacts(&KnownTag::Assistant.into(), &PropId::Model.into()));
		assert!(!policy.redacts(&KnownTag::Usage.into(), &PropId::Model.into()));
		assert!(!policy.redacts(&tool("read"), &PropId::Model.into()));
	}

	#[test]
	fn an_unparseable_entry_is_rejected_by_name() {
		assert_eq!(
			ControlVisibility::from_entries(["*/provider", "provider"]),
			Err(VisibilityTableError::Malformed { entry: Str::new_static("provider") })
		);
		assert_eq!(
			ControlVisibility::from_entries(["assistant/custom/root"]),
			Err(VisibilityTableError::Malformed { entry: Str::new_static("assistant/custom/root") })
		);
		assert_eq!(
			ControlVisibility::from_entries(["/provider"]),
			Err(VisibilityTableError::EmptyTag { entry: Str::new_static("/provider") })
		);
		assert_eq!(
			ControlVisibility::from_entries(["assistant/"]),
			Err(VisibilityTableError::EmptyProp { entry: Str::new_static("assistant/") })
		);
		assert_eq!(
			ControlVisibility::from_entries(["assistant/*"]),
			Err(VisibilityTableError::WildcardProp { entry: Str::new_static("assistant/*") })
		);
	}

	#[test]
	fn a_misspelled_half_is_rejected_instead_of_narrowing_the_policy() {
		assert_eq!(
			ControlVisibility::from_entries(["*/pathh"]),
			Err(VisibilityTableError::UnknownProp {
				entry: Str::new_static("*/pathh"),
				prop:  Str::new_static("pathh"),
			})
		);
		assert_eq!(
			ControlVisibility::from_entries(["*/call_id"]),
			Err(VisibilityTableError::UnknownProp {
				entry: Str::new_static("*/call_id"),
				prop:  Str::new_static("call_id"),
			})
		);
		assert_eq!(
			ControlVisibility::from_entries(["usr/path"]),
			Err(VisibilityTableError::UnknownTag {
				entry: Str::new_static("usr/path"),
				tag:   Str::new_static("usr"),
			})
		);
		assert_eq!(
			ControlVisibility::from_entries(["read/path"]),
			Err(VisibilityTableError::UnknownTag {
				entry: Str::new_static("read/path"),
				tag:   Str::new_static("read"),
			})
		);

		let policy = ControlVisibility::from_entries(["tool:read/custom:root", "*/call-id"])
			.expect("the prefixed forms stay nameable");

		assert!(policy.redacts(&tool("read"), &PropKey::Custom(Str::new_static("root"))));
		assert!(!policy.redacts(&tool("grep"), &PropKey::Custom(Str::new_static("root"))));
		assert!(policy.redacts(&KnownTag::Assistant.into(), &PropId::CallId.into()));
	}

	#[test]
	fn redacting_a_snapshot_blanks_provider_metadata_and_keeps_the_shape() {
		let (dom, seeded) = seeded();
		let policy = default_policy();
		let snapshot = dom.snapshot();
		let redacted = snapshot.redact(&policy);

		assert_eq!(redacted.high_water(), snapshot.high_water());
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

		let blanked = Value::Str(Str::empty());
		let assistant = redacted
			.get(seeded.assistant)
			.expect("the assistant node survives");
		assert_eq!(assistant.prop(&PropId::Provider.into()), Some(&blanked));
		assert_eq!(assistant.prop(&PropId::Route.into()), Some(&blanked));
		assert_eq!(assistant.prop(&PropId::Model.into()), Some(&Value::Str(Str::new_static("opus"))));
		assert_eq!(
			assistant.prop(&PropId::Text.into()),
			Some(&Value::Str(Str::new_static("reading it now")))
		);
		assert!(
			!redacted
				.as_bytes()
				.windows(b"anthropic".len())
				.any(|window| window == b"anthropic")
		);
	}

	/// Pins the two gaps the module documents, so widening or narrowing either
	/// one has to update the documentation with it.
	#[test]
	fn r1_leaves_tool_payload_paths_and_the_advisor_card_visible() {
		let (dom, seeded) = seeded();
		let redacted = dom.snapshot().redact(&default_policy());

		let input = redacted.get(seeded.input).expect("the tool input survives");
		assert_eq!(input.content.as_deref(), Some(INPUT_CONTENT));
		assert!(INPUT_CONTENT.contains(LOCAL_PATH));

		let result = redacted
			.get(seeded.result)
			.expect("the tool result survives");
		assert_eq!(
			result.prop(&PropId::Text.into()),
			Some(&Value::Str(Str::new_static(RESULT_TEXT)))
		);
		assert!(RESULT_TEXT.contains(LOCAL_PATH));

		let notice = redacted
			.get(seeded.notice)
			.expect("the advisor card survives");
		assert_eq!(notice.prop(&PropId::Kind.into()), Some(&Value::Str(Str::new_static("advisor"))));
		assert!(matches!(notice.prop(&PropId::Data.into()), Some(Value::Json(_))));
		assert_eq!(notice.content.as_deref(), Some(ADVISOR_NOTE));
	}
}
