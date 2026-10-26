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
//! - `*/path` blanks the `path` property of every element.
//! - `user/path` blanks it only on `<user>` elements.
//! - `tool:grep/path` names a tool element, using the same `tool:` prefix the
//!   DOM wire format uses; `custom:` names a tool-defined property.
//!
//! `*` is accepted in the tag half only. An entry that is malformed, empty on
//! either side, or wildcards the property half is rejected as a
//! [`VisibilityTableError`] naming the entry, rather than being dropped (which
//! would narrow the policy) or read as a literal element or property name
//! (which would widen it).
//!
//! # Default table
//!
//! The default covers the host-local facts A4 names that the DOM can actually
//! identify from a tag and a property: filesystem paths (A4's `cwd` and local
//! resources) and raw provider metadata (the provider and the resolved route).
//! The model-facing advisor aside is a bare `<developer>` element with no
//! marker, so a tag-and-property table cannot separate it from any other
//! developer message; per the ADR amendment of 2026-10-26 it is out of R1 and
//! no marker is invented here.

use std::{collections::BTreeSet, str::FromStr};

use omp_collab::host::VisibilityClass;
use omp_con::Ctx;
use omp_core::{Str, sf};
use omp_dom::{PropKey, PropPolicy, Tag};
use thiserror::Error;

/// Separates the tag half of a table entry from its property half.
const ENTRY_SEPARATOR: char = '/';

/// Tag half that matches every element.
const ANY_TAG: &str = "*";

/// Seed table: the A4 host-local facts a tag-and-property rule can identify.
const DEFAULT_HOST_LOCAL_PROPS: &[&str] = &["*/path", "*/provider", "*/route"];

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

	/// Element text content is never host-local in R1: the only content-bearing
	/// host-local fact A4 names is the advisor aside, which carries no marker
	/// the DOM can key on (see the module documentation).
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
	let prop = PropKey::from_str(prop).expect("property parsing is infallible");
	if tag == ANY_TAG {
		return Ok(Rule::AnyTag(prop));
	}
	let tag = Tag::from_str(tag).expect("tag parsing is infallible");
	Ok(Rule::Exact(tag, prop))
}

#[cfg(test)]
mod tests {
	use omp_dom::{Dom, Handle, KnownTag, NodeSpec, Op, PropId, Txn, Value};
	use omp_journal::EntryId;

	use super::*;

	fn tool(name: &'static str) -> Tag {
		Tag::Custom(Str::new_static(name))
	}

	/// Seeds a turn whose user and tool nodes carry one host-local property
	/// from the default table and one ordinary property beside it.
	fn seeded() -> (Dom, Handle, Handle) {
		let mut dom = Dom::new();
		let body = dom.body();
		let applied = dom
			.apply(&Txn {
				cause: EntryId::default(),
				label: None,
				ops:   vec![Op::Ins {
					parent: body,
					after:  None,
					node:   NodeSpec::new(KnownTag::Turn),
				}],
			})
			.expect("the turn insert applies");
		let turn = applied.minted[0];
		let applied = dom
			.apply(&Txn {
				cause: EntryId::default(),
				label: None,
				ops:   vec![
					Op::Ins {
						parent: turn,
						after:  None,
						node:   NodeSpec::new(KnownTag::User)
							.with_prop(PropId::Text, Value::Str(Str::new_static("read the file")))
							.with_prop(PropId::Status, Value::Str(Str::new_static("sent"))),
					},
					Op::Ins {
						parent: turn,
						after:  None,
						node:   NodeSpec::new(tool("read"))
							.with_prop(PropId::Path, Value::Str(Str::new_static("/home/owner/secret")))
							.with_prop(PropId::Provider, Value::Str(Str::new_static("anthropic")))
							.with_prop(PropId::Lines, Value::Int(42))
							.with_content("file body"),
					},
				],
			})
			.expect("the message inserts apply");
		let user = applied.minted[0];
		let call = applied.minted[1];
		(dom, user, call)
	}

	#[test]
	fn default_table_blanks_paths_and_provider_metadata_only() {
		let policy = ControlVisibility::from_entries(DEFAULT_HOST_LOCAL_PROPS.iter().copied())
			.expect("the seeded default table parses");

		assert_eq!(policy.classify(&tool("read"), &PropId::Path.into()), VisibilityClass::HostLocal);
		assert_eq!(
			policy.classify(&tool("read"), &PropId::Provider.into()),
			VisibilityClass::HostLocal
		);
		assert_eq!(policy.classify(&tool("read"), &PropId::Route.into()), VisibilityClass::HostLocal);
		assert_eq!(
			policy.classify(&KnownTag::User.into(), &PropId::Text.into()),
			VisibilityClass::PublicTranscript
		);
		assert!(policy.redacts(&tool("read"), &PropId::Path.into()));
		assert!(!policy.redacts(&tool("read"), &PropId::Lines.into()));
	}

	#[test]
	fn a_wildcard_entry_crosses_tags_and_an_exact_entry_does_not() {
		let policy =
			ControlVisibility::from_entries(["*/path", "user/status"]).expect("both forms parse");

		assert!(policy.redacts(&tool("read"), &PropId::Path.into()));
		assert!(policy.redacts(&KnownTag::User.into(), &PropId::Path.into()));
		assert!(policy.redacts(&KnownTag::User.into(), &PropId::Status.into()));
		assert!(!policy.redacts(&KnownTag::Assistant.into(), &PropId::Status.into()));
		assert!(!policy.redacts(&tool("read"), &PropId::Status.into()));
	}

	#[test]
	fn an_unparseable_entry_is_rejected_by_name() {
		assert_eq!(
			ControlVisibility::from_entries(["*/path", "path"]),
			Err(VisibilityTableError::Malformed { entry: Str::new_static("path") })
		);
		assert_eq!(
			ControlVisibility::from_entries(["user/custom/path"]),
			Err(VisibilityTableError::Malformed { entry: Str::new_static("user/custom/path") })
		);
		assert_eq!(
			ControlVisibility::from_entries(["/path"]),
			Err(VisibilityTableError::EmptyTag { entry: Str::new_static("/path") })
		);
		assert_eq!(
			ControlVisibility::from_entries(["user/"]),
			Err(VisibilityTableError::EmptyProp { entry: Str::new_static("user/") })
		);
		assert_eq!(
			ControlVisibility::from_entries(["user/*"]),
			Err(VisibilityTableError::WildcardProp { entry: Str::new_static("user/*") })
		);
	}

	#[test]
	fn redacting_a_snapshot_blanks_the_seeded_properties_and_keeps_the_shape() {
		let (dom, user, call) = seeded();
		let policy = ControlVisibility::from_entries(DEFAULT_HOST_LOCAL_PROPS.iter().copied())
			.expect("the seeded default table parses");
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
		let call = redacted.get(call).expect("the tool node survives");
		assert_eq!(call.prop(&PropId::Path.into()), Some(&blanked));
		assert_eq!(call.prop(&PropId::Provider.into()), Some(&blanked));
		assert_eq!(call.prop(&PropId::Lines.into()), Some(&Value::Int(42)));
		assert_eq!(call.content.as_deref(), Some("file body"));

		let user = redacted.get(user).expect("the user node survives");
		assert_eq!(
			user.prop(&PropId::Text.into()),
			Some(&Value::Str(Str::new_static("read the file")))
		);
		assert_eq!(user.prop(&PropId::Status.into()), Some(&Value::Str(Str::new_static("sent"))));
		assert!(
			!redacted
				.as_bytes()
				.windows(6)
				.any(|window| window == b"secret")
		);
	}
}
