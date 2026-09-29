//! Journal-first subagent composition and session-owned tools.

pub mod autoreply;
pub mod hub;
pub mod revive;
pub mod settings;
pub mod spawn;
pub mod workpool;
mod workpool_runtime;
pub mod workpool_scheduler;
mod yield_assembly;

omp_core::string_id!(
	/// Agent class a composed kernel runs as: [`MAIN_AGENT`] for a top-level
	/// session, the spawned class (`task`, `scout`, ...) for a child. It names
	/// the class cfg (`<agent>.cfg`, ADR 0013) and selects the rules whose
	/// `agents:` frontmatter admits it.
	AgentName
);

/// Agent class of the top-level session.
pub const MAIN_AGENT: &AgentName<str> = AgentName::from_ref("main");

/// The bundled default child class: it needs no `task.cfg` to exist.
pub const TASK_AGENT: &AgentName<str> = AgentName::from_ref("task");

/// `<meta>` prop recording the agent class a session's kernel runs as. Child
/// compositions journal it; a session without it is a [`MAIN_AGENT`] session.
const AGENT_PROP: &str = "agent";

/// `<meta>` prop recording the recursion depth a child session runs at
/// (`sv_task_recursion_depth`), journaled beside [`AGENT_PROP`].
const DEPTH_PROP: &str = "depth";

/// The agent class journaled on `dom`'s `<meta>` by [`journal_agent`], or
/// `None` for a session that never ran as a subagent.
#[must_use]
pub fn journaled_agent(dom: &omp_dom::Dom) -> Option<AgentName> {
	match dom
		.get(dom.meta())?
		.prop(&omp_dom::PropKey::Custom(omp_core::Str::new_static(AGENT_PROP)))?
	{
		omp_dom::Value::Str(agent) if !agent.is_empty() => Some(AgentName::new(agent.clone())),
		_ => None,
	}
}

/// The agent class a session runs as: its journaled class, else
/// [`MAIN_AGENT`].
#[must_use]
pub fn session_agent(dom: &omp_dom::Dom) -> AgentName {
	journaled_agent(dom).unwrap_or_else(|| MAIN_AGENT.to_owned())
}

/// The recursion depth journaled on `dom`'s `<meta>` by [`journal_agent`],
/// or `None` for a main session or a child journaled before depths were.
#[must_use]
pub fn journaled_depth(dom: &omp_dom::Dom) -> Option<u32> {
	match dom
		.get(dom.meta())?
		.prop(&omp_dom::PropKey::Custom(omp_core::Str::new_static(DEPTH_PROP)))?
	{
		omp_dom::Value::Int(depth) => u32::try_from(*depth).ok(),
		_ => None,
	}
}

/// Journals the agent class and recursion depth `session` runs as.
///
/// A later resume (from the main chat or `--resume`) then evaluates rule
/// `agents:` scopes with the class and presents the class configuration and
/// tool roster a spawn of `agent` at `depth` would. A session already
/// carrying both journals nothing.
pub fn journal_agent(
	session: &mut omp_session::Session,
	agent: &AgentName<str>,
	depth: u32,
) -> Result<(), omp_session::SessionError> {
	let dom = session.dom();
	let class_journaled = journaled_agent(dom).is_some_and(|journaled| journaled == *agent);
	let depth_journaled = journaled_depth(dom) == Some(depth);
	if class_journaled && depth_journaled {
		return Ok(());
	}
	let meta = dom.meta();
	let mut ops = Vec::with_capacity(2);
	if !class_journaled {
		ops.push(omp_dom::Op::Set {
			h:     meta,
			prop:  omp_dom::PropKey::Custom(omp_core::Str::new_static(AGENT_PROP)),
			value: omp_dom::Value::Str(omp_core::Str::new(agent.as_str())),
		});
	}
	if !depth_journaled {
		ops.push(omp_dom::Op::Set {
			h:     meta,
			prop:  omp_dom::PropKey::Custom(omp_core::Str::new_static(DEPTH_PROP)),
			value: omp_dom::Value::Int(i64::from(depth)),
		});
	}
	session.patch(omp_dom::Txn {
		cause: session
			.head()
			.ok_or(omp_session::SessionError::NoActiveTurn)?,
		label: Some(omp_core::Str::new_static("session.agent")),
		ops,
	})?;
	Ok(())
}
