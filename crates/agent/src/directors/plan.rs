//! Plan composition and its write/decision gates.

use omp_core::Str;
use omp_dom::{Dom, Node};

use crate::director::{
	BindValue, Director, DirectorCx, DirectorEffect, ForceUntil, Slot, StateUpdate, TurnView,
	Verdict, state_bool, state_int, state_str, turn_call_inputs, turn_called,
};

const CLAIMS: &[Slot] = &[Slot::Mode, Slot::Worktree];
/// Tools a planning turn may use: read-only discovery plus the plan file
/// write and the decision request.
///
/// The wire roster is latched for the session (ADR 0024), so the bind never
/// narrows what is advertised; the kernel enforces it at dispatch: every model,
/// session, or host tool call is checked against the request's
/// [`omp_tool::ToolRestrictions`] snapshot before any preview or execution, and
/// a call outside this list settles as a journaled `tool.roster.restricted`
/// policy denial. While plan mode is active the same snapshot confines `write`
/// to [`Plan`]'s plan file ([`omp_tool::plan_target_matches`]) and keeps `hub`
/// off processes; nested `tool.<name>()` calls from an eval cell obey the
/// identical snapshot in the environment's bridge, and the environment refuses
/// every write outside the plan file.
pub const PLAN_TOOLS: &[&str] = &[
	"read",
	"grep",
	"glob",
	"ast_grep",
	"lsp",
	"web_search",
	"think",
	"todo",
	"write",
	"ask",
	"task",
	"hub",
	"yield",
];

/// The read-only ceiling of a subagent spawned under plan mode
/// ([`crate::SV_TOOLS_READ_ONLY`]): [`PLAN_TOOLS`] without the plan-file
/// `write`, which belongs to the planning session alone.
pub const PLAN_READ_ONLY_TOOLS: &[&str] = &[
	"read",
	"grep",
	"glob",
	"ast_grep",
	"lsp",
	"web_search",
	"think",
	"todo",
	"ask",
	"task",
	"hub",
	"yield",
];

/// Whether a subagent spawned from `con` must be read-only.
///
/// It must when `con` runs under plan mode (the plan Director owns its
/// `sv_tools` engagement) or already carries the read-only ceiling.
/// Inheritance is a ceiling, like the `task` recursion limit: a child's own
/// cfg cannot lift it.
#[must_use]
pub fn spawns_read_only(con: &omp_con::Ctx) -> bool {
	crate::SV_TOOLS_READ_ONLY.get(con)
		|| con
			.engagement_owner(crate::SV_TOOLS.name())
			.is_some_and(|owner| owner.split_once('#').map_or(owner.as_str(), |(id, _)| id) == "plan")
}

/// Requires a durable plan followed by an explicit user decision request.
pub struct Plan {
	plan_file:         Str,
	plan_written:      bool,
	decision_made:     bool,
	write_attempts:    u32,
	decision_attempts: u32,
	/// Once the plan is written and presented, hand off to this model and keep
	/// going instead of yielding for approval.
	yolo_into:         Option<Str>,
	yolo_thinking:     Option<Str>,
	binds:             Vec<(Str, BindValue)>,
}

impl Plan {
	/// Creates a plan engagement for one local artifact.
	#[must_use]
	pub fn new(plan_file: impl Into<Str>) -> Self {
		Self {
			plan_file:         plan_file.into(),
			plan_written:      false,
			decision_made:     false,
			write_attempts:    0,
			decision_attempts: 0,
			yolo_into:         None,
			yolo_thinking:     None,
			binds:             plan_binds(),
		}
	}

	/// Enables the yolo handoff: after the plan is written and presented the
	/// Director exits, re-targets `ai_model` (and `ai_thinking` when given),
	/// and continues implementing.
	#[must_use]
	pub fn with_yolo(mut self, target: impl Into<Str>, thinking: Option<Str>) -> Self {
		self.yolo_into = Some(target.into());
		self.yolo_thinking = thinking;
		self
	}

	/// Reconstructs plan state from its DOM element.
	#[must_use]
	pub fn from_node(node: &Node) -> Self {
		Self {
			plan_file:         state_str(node, "plan_file")
				.unwrap_or_else(|| Str::new_static("local://plans/current.md")),
			plan_written:      state_bool(node, "plan_written").unwrap_or(false),
			decision_made:     state_bool(node, "decision_made").unwrap_or(false),
			write_attempts:    u32_value(state_int(node, "write_attempts")),
			decision_attempts: u32_value(state_int(node, "decision_attempts")),
			yolo_into:         state_str(node, "yolo_into").filter(|value| !value.is_empty()),
			yolo_thinking:     state_str(node, "yolo_thinking").filter(|value| !value.is_empty()),
			binds:             plan_binds(),
		}
	}

	fn yolo_handoff(&self) -> Option<DirectorEffect> {
		let target = self.yolo_into.as_ref()?;
		let mut effect = DirectorEffect::new(Verdict::Done)
			.with_aside(format!("Plan approved. Implementing now with {target}."))
			.with_write("ai_model", BindValue::Str(target.clone()))
			.continuing_after_exit();
		if let Some(thinking) = &self.yolo_thinking {
			effect = effect.with_write("ai_thinking", BindValue::Str(thinking.clone()));
		}
		Some(effect)
	}
}

impl Director for Plan {
	fn id(&self) -> &'static str {
		"plan"
	}

	fn claims(&self) -> &'static [Slot] {
		CLAIMS
	}

	fn binds(&self) -> &[(Str, BindValue)] {
		&self.binds
	}

	fn state(&self) -> Vec<(Str, BindValue)> {
		vec![
			(Str::new_static("plan_file"), BindValue::Str(self.plan_file.clone())),
			(Str::new_static("plan_written"), BindValue::Bool(self.plan_written)),
			(Str::new_static("decision_made"), BindValue::Bool(self.decision_made)),
			(Str::new_static("write_attempts"), BindValue::Int(i64::from(self.write_attempts))),
			(Str::new_static("decision_attempts"), BindValue::Int(i64::from(self.decision_attempts))),
			(Str::new_static("tools"), BindValue::Str(Str::new_static("write,ask"))),
			(Str::new_static("yolo_into"), BindValue::Str(self.yolo_into.clone().unwrap_or_default())),
			(
				Str::new_static("yolo_thinking"),
				BindValue::Str(self.yolo_thinking.clone().unwrap_or_default()),
			),
		]
	}

	fn observe_turn(&self, dom: &Dom, _cx: &DirectorCx<'_>, turn: &TurnView) -> Vec<StateUpdate> {
		let wrote_plan = call_wrote_path(dom, turn.turn, self.plan_file.as_str());
		let proposed = turn_called(dom, turn.turn, "ask");
		let mut updates = Vec::with_capacity(2);
		if wrote_plan && !self.plan_written {
			updates.push(StateUpdate::new("plan_written", BindValue::Bool(true)));
		}
		if proposed && !self.decision_made {
			updates.push(StateUpdate::new("decision_made", BindValue::Bool(true)));
		}
		updates
	}

	fn evaluate(&self, _dom: &Dom, cx: &DirectorCx<'_>, _turn: &TurnView) -> DirectorEffect {
		if !self.plan_written {
			if self.write_attempts >= 3 {
				return DirectorEffect::new(Verdict::Yield);
			}
			let verdict = cx.force_tool(
				"write",
				ForceUntil::ToolCalled(Str::new_static("write")),
				Some(Str::new(format!(
					"Write the plan to {} before asking for approval.",
					self.plan_file
				))),
				3,
			);
			return DirectorEffect::new(verdict)
				.with_update("write_attempts", BindValue::Int(i64::from(self.write_attempts + 1)))
				.with_aside(format!(
					"Write the plan to {} before asking for approval.",
					self.plan_file
				));
		}
		if self.decision_made {
			return self
				.yolo_handoff()
				.unwrap_or_else(|| DirectorEffect::new(Verdict::Yield));
		}
		if self.decision_attempts >= 3 {
			return DirectorEffect::new(Verdict::Yield);
		}
		DirectorEffect::new(cx.force_tool(
			"required",
			ForceUntil::AnyToolCall,
			Some(Str::new_static(
				"Present the completed plan for an explicit decision before yielding.",
			)),
			3,
		))
		.with_update("decision_attempts", BindValue::Int(i64::from(self.decision_attempts + 1)))
	}
}

/// The engagement layer plan mode installs (ADR 0012/0015): the mode prompt
/// slot, the `@plan` role route, and the planning tool roster.
fn plan_binds() -> Vec<(Str, BindValue)> {
	vec![
		(Str::new_static("ai_prompt_mode"), BindValue::Str(Str::new_static("plan"))),
		(Str::new_static("ai_model"), BindValue::Str(Str::new_static("@plan"))),
		(Str::new_static("sv_tools"), BindValue::list(PLAN_TOOLS)),
	]
}

/// The plan file of the active plan engagement, which the dispatch roster
/// check confines `write` to; `None` when plan mode is not active.
#[must_use]
pub fn active_plan_file(dom: &Dom) -> Option<Str> {
	let (_, node) = crate::find_director(dom, "plan")?;
	(crate::director_status(node) == Some("active")).then(|| Plan::from_node(node).plan_file)
}

/// Whether a `write` in `turn` targeted the plan file, compared exactly as
/// the dispatch check compares it.
fn call_wrote_path(dom: &Dom, turn: omp_dom::Handle, expected: &str) -> bool {
	turn_call_inputs(dom, turn, "write").any(|input| {
		serde_json::from_str::<serde_json::Value>(input)
			.ok()
			.as_ref()
			.and_then(|value| value.get("path"))
			.and_then(serde_json::Value::as_str)
			.is_some_and(|path| omp_tool::plan_target_matches(expected, path))
	})
}

fn u32_value(value: Option<i64>) -> u32 {
	value
		.and_then(|value| u32::try_from(value).ok())
		.unwrap_or(0)
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn the_read_only_ceiling_is_the_plan_roster_without_its_write() {
		let expected = PLAN_TOOLS
			.iter()
			.copied()
			.filter(|name| *name != "write")
			.collect::<Vec<_>>();
		assert_eq!(PLAN_READ_ONLY_TOOLS, expected.as_slice());
	}
}
