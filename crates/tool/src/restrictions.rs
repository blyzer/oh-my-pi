//! Per-request tool-roster restrictions enforced where a call starts.
//!
//! The advertised roster narrows the tools a model *sees*; these restrictions
//! decide which calls may *run*. The agent snapshots one [`ToolRestrictions`]
//! per model request (the same inputs the advertised list is derived from:
//! the `sv_tools` allowlist and the Director that bound it, the `turn_start`
//! hook filter, and the active plan file) and checks every call against it
//! before any preview or execution. A tool that hosts nested calls (the eval
//! bridge) receives the same snapshot and applies it to every nested call.
//!
//! Model-visible refusal text comes from one place: the [`RosterDenial`]
//! `Display` derive.

use std::{fmt, sync::Arc};

use omp_core::{Str, sf};
use omp_proto::env::v1 as wire;
use serde_json::value::RawValue;
use smallvec::SmallVec;

use crate::{Abort, PolicyDenied};

/// Stable denial code journaled for every roster refusal.
pub const ROSTER_RESTRICTED: &str = "tool.roster.restricted";

/// Tools whose target is confined to the plan file while plan mode is active.
const PLAN_SCOPED_TOOLS: &[&str] = &["write"];

/// Comma-separated tool names quoted in a refusal; `none` when empty.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ToolNameList(Arc<[Str]>);

impl ToolNameList {
	/// The names, in advertised order.
	#[must_use]
	pub fn names(&self) -> &[Str] {
		&self.0
	}
}

impl fmt::Display for ToolNameList {
	fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
		let mut names = self.0.iter();
		let Some(first) = names.next() else {
			return formatter.write_str("none");
		};
		formatter.write_str(first.as_str())?;
		for name in names {
			formatter.write_str(", ")?;
			formatter.write_str(name.as_str())?;
		}
		Ok(())
	}
}

/// Stable identifier of the restriction that refused a call, journaled in
/// [`PolicyDenied::rules`].
#[derive(Clone, Debug, Eq, PartialEq, strum::Display)]
pub enum RosterRule {
	/// A mode Director's `sv_tools` bind.
	#[strum(to_string = "director:{director}/sv_tools")]
	Director {
		/// Director id that owns the bind.
		director: Str,
	},
	/// The user's `sv_tools` allowlist (console, `--tools`, agent cfg).
	#[strum(to_string = "sv_tools")]
	Allowlist,
	/// A `turn_start` lifecycle hook's `enabled_tools`.
	#[strum(to_string = "hook:turn_start/enabled_tools")]
	Hook,
	/// The plan Director's plan-file write scope.
	#[strum(to_string = "director:plan/plan_file")]
	PlanFile,
}

/// Why one call was refused. `Display` is the model-visible text (projected
/// as `skipped: <text>`).
#[derive(Clone, Debug, Eq, PartialEq, strum::Display)]
pub enum RosterDenial {
	/// A Director's roster bind does not include the tool.
	#[strum(to_string = "`{tool}` is not available while {director} mode is active. No action was \
	                     taken. Available now: {available}.")]
	Director {
		/// Refused tool.
		tool:      Str,
		/// Director id that owns the roster.
		director:  Str,
		/// Tools the model may call under this roster.
		available: ToolNameList,
	},
	/// The user's `sv_tools` allowlist does not include the tool.
	#[strum(to_string = "`{tool}` is not in the `sv_tools` allowlist. No action was taken. \
	                     Available now: {available}.")]
	Allowlist {
		/// Refused tool.
		tool:      Str,
		/// Tools the model may call under this roster.
		available: ToolNameList,
	},
	/// A `turn_start` hook disabled the tool for this request.
	#[strum(to_string = "`{tool}` was disabled for this request by a turn_start hook. No action \
	                     was taken. Available now: {available}.")]
	Hook {
		/// Refused tool.
		tool:      Str,
		/// Tools the model may call under this roster.
		available: ToolNameList,
	},
	/// Plan mode confines the tool's target to the plan file.
	#[strum(to_string = "`{tool}` is limited to the plan file {plan_file} while plan mode is \
	                     active; no action was taken.")]
	PlanFile {
		/// Refused tool.
		tool:      Str,
		/// The only target the tool may change.
		plan_file: Str,
	},
}

impl RosterDenial {
	/// The refused tool.
	#[must_use]
	pub const fn tool(&self) -> &Str {
		match self {
			Self::Director { tool, .. }
			| Self::Allowlist { tool, .. }
			| Self::Hook { tool, .. }
			| Self::PlanFile { tool, .. } => tool,
		}
	}

	/// The restriction that fired.
	#[must_use]
	pub fn rule(&self) -> RosterRule {
		match self {
			Self::Director { director, .. } => RosterRule::Director { director: director.clone() },
			Self::Allowlist { .. } => RosterRule::Allowlist,
			Self::Hook { .. } => RosterRule::Hook,
			Self::PlanFile { .. } => RosterRule::PlanFile,
		}
	}

	/// Renders the model-visible refusal once.
	#[must_use]
	pub fn reason(&self) -> Str {
		sf!("{self}")
	}

	/// The model-facing abort and durable policy evidence for a call settled
	/// by this refusal (see [`crate::CallOutcome::policy_denied`]); the text is
	/// rendered once and shared by both.
	#[must_use]
	pub fn verdict(&self, decision_id: Str) -> (Abort, PolicyDenied) {
		let reason = self.reason();
		let rule = sf!("{}", self.rule());
		(Abort::Skipped { reason: reason.clone() }, PolicyDenied {
			reason,
			code: Some(Str::new_static(ROSTER_RESTRICTED)),
			decision_id,
			rules: Arc::from([rule]),
		})
	}
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Allowlist {
	names:    Arc<[Str]>,
	director: Option<Str>,
}

/// One request's roster restrictions. The default value restricts nothing.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ToolRestrictions {
	allowlist: Option<Allowlist>,
	hook:      Option<Arc<[Str]>>,
	available: ToolNameList,
	plan_file: Option<Str>,
}

impl ToolRestrictions {
	/// Restricts calls to `names` (the effective `sv_tools` allowlist plus the
	/// hidden mounts advertised beside it). `director` names the Director whose
	/// bind supplied the allowlist; `None` for a user value.
	#[must_use]
	pub fn with_allowlist(mut self, names: impl Into<Arc<[Str]>>, director: Option<Str>) -> Self {
		self.allowlist = Some(Allowlist { names: names.into(), director });
		self
	}

	/// Confines plan-scoped tools (`write`) to `plan_file`.
	#[must_use]
	pub fn with_plan_file(mut self, plan_file: Str) -> Self {
		self.plan_file = Some(plan_file);
		self
	}

	/// Intersects calls with the names a `turn_start` hook left enabled.
	pub fn set_hook(&mut self, names: impl Into<Arc<[Str]>>) {
		self.hook = Some(names.into());
	}

	/// Records the names advertised to the model, quoted in refusals.
	pub fn set_available(&mut self, names: impl Into<Arc<[Str]>>) {
		self.available = ToolNameList(names.into());
	}

	/// Whether no restriction applies.
	#[must_use]
	pub const fn is_unrestricted(&self) -> bool {
		self.allowlist.is_none() && self.hook.is_none() && self.plan_file.is_none()
	}

	/// The plan file plan-scoped tools are confined to, while plan mode is
	/// active.
	#[must_use]
	pub const fn plan_file(&self) -> Option<&Str> {
		self.plan_file.as_ref()
	}

	/// The names a `turn_start` hook left enabled, when one filtered the
	/// request.
	#[must_use]
	pub const fn hook(&self) -> Option<&Arc<[Str]>> {
		self.hook.as_ref()
	}

	/// The names advertised to the model for the request.
	#[must_use]
	pub fn available(&self) -> &[Str] {
		self.available.names()
	}

	/// Checks a call by name alone. Allocation-free when the call is allowed.
	pub fn check_name(&self, tool: &str) -> Result<(), RosterDenial> {
		if let Some(allowlist) = &self.allowlist
			&& !contains(&allowlist.names, tool)
		{
			return Err(match &allowlist.director {
				Some(director) => RosterDenial::Director {
					tool:      Str::new(tool),
					director:  director.clone(),
					available: self.available.clone(),
				},
				None => RosterDenial::Allowlist {
					tool:      Str::new(tool),
					available: self.available.clone(),
				},
			});
		}
		if let Some(hook) = &self.hook
			&& !contains(hook, tool)
		{
			return Err(RosterDenial::Hook {
				tool:      Str::new(tool),
				available: self.available.clone(),
			});
		}
		Ok(())
	}

	/// Whether [`Self::check_arguments`] can refuse `tool`: only a plan-scoped
	/// tool while plan mode is active inspects its arguments.
	#[must_use]
	pub fn scopes_arguments(&self, tool: &str) -> bool {
		self.plan_file.is_some() && PLAN_SCOPED_TOOLS.contains(&tool)
	}

	/// Checks a call's committed arguments: while plan mode is active,
	/// `write` may target only the plan file. A missing or unrecognized target
	/// fails closed.
	pub fn check_arguments(&self, tool: &str, args: &serde_json::Value) -> Result<(), RosterDenial> {
		let Some(plan_file) = self
			.plan_file
			.as_ref()
			.filter(|_| self.scopes_arguments(tool))
		else {
			return Ok(());
		};
		let target = args.get("path").and_then(serde_json::Value::as_str);
		if target.is_some_and(|target| plan_target_matches(plan_file, target)) {
			return Ok(());
		}
		Err(RosterDenial::PlanFile { tool: Str::new(tool), plan_file: plan_file.clone() })
	}

	/// Checks a call by name and then by its committed arguments.
	pub fn check_call(&self, tool: &str, args: &serde_json::Value) -> Result<(), RosterDenial> {
		self.check_name(tool)?;
		self.check_arguments(tool, args)
	}

	/// [`Self::check_call`] over raw committed arguments, parsed only when
	/// the tool's arguments are in scope. Unparseable arguments fail closed.
	pub fn check_raw(&self, tool: &str, args: &RawValue) -> Result<(), RosterDenial> {
		self.check_name(tool)?;
		if !self.scopes_arguments(tool) {
			return Ok(());
		}
		let value =
			serde_json::from_str::<serde_json::Value>(args.get()).unwrap_or(serde_json::Value::Null);
		self.check_arguments(tool, &value)
	}
}

fn contains(names: &[Str], tool: &str) -> bool {
	names.iter().any(|name| name.as_str() == tool)
}

/// Whether `target`, as a `write` tool would receive it, names exactly
/// `plan_file`.
///
/// The comparison is lexical and fails closed: both sides are reduced to a
/// scheme, an absolute flag, and their path components after the spellings the
/// write tool itself accepts (surrounding whitespace and quotes, a hashline
/// `[path#TAG]` header, `local:/x` for `local://x`, `.` and empty components).
/// Any `..` component, backslash, or percent escape refuses the target rather
/// than guessing how the filesystem resolves it, and a relative path never
/// equals an absolute one. A symlinked spelling therefore only matches when it
/// is the plan file's own spelling, which the write tool resolves to the plan
/// file by definition.
#[must_use]
pub fn plan_target_matches(plan_file: &str, target: &str) -> bool {
	match (PlanTarget::parse(plan_file), PlanTarget::parse(target)) {
		(Some(plan), Some(target)) => plan == target,
		_ => false,
	}
}

#[derive(Debug)]
struct PlanTarget<'a> {
	scheme:   Option<&'a str>,
	absolute: bool,
	parts:    SmallVec<&'a str, 8>,
}

impl PartialEq for PlanTarget<'_> {
	fn eq(&self, other: &Self) -> bool {
		let schemes = match (self.scheme, other.scheme) {
			(Some(left), Some(right)) => left.eq_ignore_ascii_case(right),
			(None, None) => true,
			_ => false,
		};
		schemes && self.absolute == other.absolute && self.parts == other.parts
	}
}

impl<'a> PlanTarget<'a> {
	fn parse(input: &'a str) -> Option<Self> {
		let path = unquote(unwrap_hashline_header(input.trim())).trim();
		if path.is_empty() || path.contains(['\\', '%']) {
			return None;
		}
		let (scheme, resource) = match path.split_once("://") {
			Some((scheme, rest)) if valid_scheme(scheme) => (Some(scheme), rest),
			Some(_) => return None,
			None => match path.strip_prefix("local:/") {
				Some(rest) => (Some("local"), rest),
				None if path.contains(':') => return None,
				None => (None, path),
			},
		};
		let absolute = resource.starts_with('/');
		if absolute && scheme.is_some_and(|scheme| !scheme.eq_ignore_ascii_case("file")) {
			// Session resources (`local:///x`) must be relative to their root.
			return None;
		}
		let mut parts = SmallVec::new();
		for part in resource.split('/') {
			match part {
				"" | "." => {},
				".." => return None,
				part => parts.push(part),
			}
		}
		if parts.is_empty() {
			return None;
		}
		// `file:///abs` is the absolute path `/abs`.
		let scheme = scheme.filter(|scheme| !scheme.eq_ignore_ascii_case("file"));
		Some(Self { scheme, absolute, parts })
	}
}

fn valid_scheme(scheme: &str) -> bool {
	let mut bytes = scheme.bytes();
	bytes
		.next()
		.is_some_and(|first| first.is_ascii_alphabetic())
		&& bytes.all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'-' | b'.'))
}

fn unquote(path: &str) -> &str {
	for quote in ['"', '\'', '`'] {
		if let Some(inner) = path
			.strip_prefix(quote)
			.and_then(|path| path.strip_suffix(quote))
		{
			return inner;
		}
	}
	path
}

/// Strips a strict `[path]` / `[path#XXXX]` hashline header, as the write
/// tool does before resolving its target.
fn unwrap_hashline_header(target: &str) -> &str {
	let Some(inner) = target
		.strip_prefix('[')
		.and_then(|target| target.strip_suffix(']'))
	else {
		return target;
	};
	let path = match inner.rsplit_once('#') {
		Some((path, tag)) if tag.len() == 4 && tag.bytes().all(|byte| byte.is_ascii_hexdigit()) => {
			path
		},
		Some(_) => return target,
		None => inner,
	};
	if path.is_empty() || path.contains('#') {
		target
	} else {
		path
	}
}

impl From<&ToolRestrictions> for wire::ToolRestrictions {
	fn from(restrictions: &ToolRestrictions) -> Self {
		let names = |names: &[Str]| names.iter().map(|name| name.to_string()).collect();
		Self {
			allowlist:          restrictions
				.allowlist
				.as_ref()
				.map(|allowlist| wire::ToolNames { names: names(&allowlist.names) }),
			allowlist_director: restrictions
				.allowlist
				.as_ref()
				.and_then(|allowlist| allowlist.director.as_ref())
				.map(ToString::to_string)
				.unwrap_or_default(),
			hook:               restrictions
				.hook
				.as_deref()
				.map(|hook| wire::ToolNames { names: names(hook) }),
			available:          names(restrictions.available.names()),
			plan_file:          restrictions.plan_file.as_ref().map(ToString::to_string),
		}
	}
}

impl From<wire::ToolRestrictions> for ToolRestrictions {
	fn from(wire: wire::ToolRestrictions) -> Self {
		let names = |names: Vec<String>| names.into_iter().map(Str::from).collect::<Arc<[Str]>>();
		Self {
			allowlist: wire.allowlist.map(|allowlist| Allowlist {
				names:    names(allowlist.names),
				director: (!wire.allowlist_director.is_empty())
					.then(|| Str::from(wire.allowlist_director)),
			}),
			hook:      wire.hook.map(|hook| names(hook.names)),
			available: ToolNameList(names(wire.available)),
			plan_file: wire.plan_file.map(Str::from),
		}
	}
}

#[cfg(test)]
mod tests {
	use serde_json::json;

	use super::*;

	fn names(names: &[&'static str]) -> Arc<[Str]> {
		names.iter().copied().map(Str::new_static).collect()
	}

	fn plan() -> ToolRestrictions {
		let mut restrictions = ToolRestrictions::default()
			.with_allowlist(names(&["read", "write"]), Some(Str::new_static("plan")))
			.with_plan_file(Str::new_static("local://PLAN.md"));
		restrictions.set_available(names(&["read", "write"]));
		restrictions
	}

	#[test]
	fn default_restricts_nothing() {
		let restrictions = ToolRestrictions::default();
		assert!(restrictions.is_unrestricted());
		assert!(
			restrictions
				.check_call("edit", &json!({"path": "src/lib.rs"}))
				.is_ok()
		);
	}

	#[test]
	fn director_allowlist_refusal_names_the_mode_and_the_roster() {
		let denial = plan()
			.check_name("edit")
			.expect_err("edit is outside the plan roster");
		assert_eq!(
			denial.to_string(),
			"`edit` is not available while plan mode is active. No action was taken. Available now: \
			 read, write."
		);
		assert_eq!(denial.rule().to_string(), "director:plan/sv_tools");
		let (abort, policy) = denial.verdict(Str::new_static("call-1"));
		assert_eq!(abort.render().as_str(), format!("skipped: {denial}"));
		assert_eq!(policy.code.as_deref(), Some(ROSTER_RESTRICTED));
		assert_eq!(policy.decision_id.as_str(), "call-1");
		assert_eq!(policy.rules.as_ref(), [Str::new_static("director:plan/sv_tools")]);
	}

	#[test]
	fn user_allowlist_and_hook_have_their_own_reasons() {
		let mut restrictions =
			ToolRestrictions::default().with_allowlist(names(&["read", "grep"]), None);
		restrictions.set_hook(names(&["read"]));
		restrictions.set_available(names(&["read"]));
		let allowlist = restrictions
			.check_name("bash")
			.expect_err("not allowlisted");
		assert_eq!(allowlist.rule(), RosterRule::Allowlist);
		assert!(allowlist.to_string().contains("`sv_tools` allowlist"));
		let hook = restrictions
			.check_name("grep")
			.expect_err("hook disabled grep");
		assert_eq!(hook.rule().to_string(), "hook:turn_start/enabled_tools");
		assert!(restrictions.check_name("read").is_ok());
	}

	#[test]
	fn plan_write_is_confined_to_the_plan_file() {
		let restrictions = plan();
		for allowed in [
			"local://PLAN.md",
			"local:/PLAN.md",
			"local://./PLAN.md",
			" \"local://PLAN.md\" ",
			"[local://PLAN.md#1A2B]",
			"LOCAL://PLAN.md",
		] {
			assert!(
				restrictions
					.check_call("write", &json!({"path": allowed, "content": "x"}))
					.is_ok(),
				"{allowed} names the plan file"
			);
		}
		for denied in [
			"src/lib.rs",
			"PLAN.md",
			"local://plan.md",
			"local://notes/PLAN.md",
			"local://x/../PLAN.md",
			"local://../PLAN.md",
			"local:///PLAN.md",
			"/etc/PLAN.md",
			"file:///tmp/PLAN.md",
			"local://PLAN.md%00",
			"local:\\\\PLAN.md",
			"vault://PLAN.md",
		] {
			let denial = restrictions
				.check_call("write", &json!({"path": denied, "content": "x"}))
				.expect_err(denied);
			assert_eq!(
				denial.to_string(),
				"`write` is limited to the plan file local://PLAN.md while plan mode is active; no \
				 action was taken."
			);
			assert_eq!(denial.rule(), RosterRule::PlanFile);
		}
		assert!(
			restrictions
				.check_call("write", &json!({"content": "x"}))
				.is_err(),
			"a missing target fails closed"
		);
		assert!(
			restrictions
				.check_call("read", &json!({"path": "src/lib.rs"}))
				.is_ok()
		);
		let raw = RawValue::from_string(r#"{"path":"local://PLAN.md"}"#.to_owned()).unwrap();
		assert!(restrictions.check_raw("write", &raw).is_ok());
		let raw = RawValue::from_string("[\"local://PLAN.md\"]".to_owned()).unwrap();
		assert!(
			restrictions.check_raw("write", &raw).is_err(),
			"arguments without a target fail closed"
		);
	}

	#[test]
	fn relative_plan_files_compare_lexically_and_never_through_traversal() {
		assert!(plan_target_matches("docs/PLAN.md", "./docs/PLAN.md"));
		assert!(plan_target_matches("docs/PLAN.md", "docs//PLAN.md"));
		assert!(!plan_target_matches("docs/PLAN.md", "docs/../docs/PLAN.md"));
		assert!(!plan_target_matches("docs/PLAN.md", "/repo/docs/PLAN.md"));
		assert!(!plan_target_matches("docs/PLAN.md", "link/PLAN.md"));
		assert!(plan_target_matches("/repo/PLAN.md", "file:///repo/PLAN.md"));
		assert!(!plan_target_matches("/repo/PLAN.md", "/repo/sub/../PLAN.md"));
	}

	#[test]
	fn wire_round_trip_preserves_every_restriction() {
		let mut restrictions = plan();
		restrictions.set_hook(names(&["read"]));
		let wire = wire::ToolRestrictions::from(&restrictions);
		assert_eq!(ToolRestrictions::from(wire), restrictions);
		let user = ToolRestrictions::default().with_allowlist(names(&["read"]), None);
		assert_eq!(ToolRestrictions::from(wire::ToolRestrictions::from(&user)), user);
	}
}
