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

/// `<meta>` prop recording the agent class a session's kernel runs as. Child
/// compositions journal it; a session without it is a [`MAIN_AGENT`] session.
const AGENT_PROP: &str = "agent";

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

/// Journals `agent` as the class `session` runs as, so a later resume (from
/// the main chat or `--resume`) evaluates rule `agents:` scopes with it. A
/// session already carrying `agent` journals nothing.
pub fn journal_agent(
	session: &mut omp_session::Session,
	agent: &AgentName<str>,
) -> Result<(), omp_session::SessionError> {
	if journaled_agent(session.dom()).is_some_and(|journaled| journaled == *agent) {
		return Ok(());
	}
	let meta = session.dom().meta();
	session.patch(omp_dom::Txn {
		cause: session
			.head()
			.ok_or(omp_session::SessionError::NoActiveTurn)?,
		label: Some(omp_core::Str::new_static("session.agent")),
		ops:   vec![omp_dom::Op::Set {
			h:     meta,
			prop:  omp_dom::PropKey::Custom(omp_core::Str::new_static(AGENT_PROP)),
			value: omp_dom::Value::Str(omp_core::Str::new(agent.as_str())),
		}],
	})?;
	Ok(())
}
