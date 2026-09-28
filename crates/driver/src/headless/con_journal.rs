//! Bridge between the process console and the journal-backed `<meta><con>`
//! component (ADR 0012 `SESSION` flag, ADR 0003 one session tree).
//!
//! Composition hydrates the console's session layer from the opened
//! journal, then every committed `SESSION` write the console publishes is
//! folded back into the journal as a `patch@1` on `<meta><con>`: at the
//! next turn boundary through a kernel [`LiveComponent`], and at process
//! exit through [`ConJournal::flush`]. Resume restores the values, rewind
//! re-derives them from the live chain, and nothing keeps convar state
//! outside the tree. A `reset` that removes a session override removes the
//! journaled `<var>` too, so replay leaves the variable inherited.
//!
//! When the composition follows the live session's class
//! ([`ClassScope::Journaled`]: the main chat, `--resume`), a session journaled
//! as a child class ([`crate::subagent::journaled_agent`]) presents that
//! child's configuration: the spawn path ([`configure_child`]) builds it over
//! the console's own picture, and [`Ctx::adopt_scope`] installs its inherited
//! and class layers beneath the journaled session writes. Switching to a main
//! session drops them again ([`Ctx::drop_scope`]). Neither layer is journaled
//! or persisted, so the child's definition is re-read on every resume.

use std::sync::Arc;

use omp_agent::{LiveComponent, LiveComponentError};
use omp_con::{CfgLoader, Ctx, Severity, Value};
use omp_core::{FastHashMap, Str};
use omp_dom::{Dom, Op, Txn};
use omp_journal::{Entry, Kind, KindName};
use omp_session::{
	Session, SessionError,
	components::con::{ConWrite, con_remove_txn, con_write_txn, con_writes},
};
use parking_lot::Mutex;

use crate::subagent::{TASK_AGENT, journaled_agent, spawn::configure_child};

/// Provenance recorded on journaled session writes.
const ORIGIN: &str = "session";

/// How a composition's console follows the agent class of its live session.
pub enum ClassScope {
	/// The composition configured the class itself (a spawned, revived, or
	/// workpool child built by [`configure_child`]); the console stays as
	/// composed.
	Composed,
	/// Follow the class journaled on the live session (the main chat,
	/// `--resume`): a child session presents its class configuration through
	/// the spawn path, reading class cfgs through this loader; a main session
	/// presents none.
	Journaled(Arc<dyn CfgLoader>),
}

/// The one console-to-journal channel for a composed session.
pub struct ConJournal {
	ctx:     Arc<Ctx>,
	scope:   ClassScope,
	writes:  flume::Receiver<omp_con::SessionWrite>,
	/// Writes observed but not yet journaled, last value per name; `None`
	/// drops the journaled value.
	pending: Mutex<FastHashMap<Str, Option<Value>>>,
}

impl ConJournal {
	/// Presents the session's class configuration (per `scope`), restores the
	/// journal's `<meta><con>` values into `ctx`'s session layer, then
	/// subscribes to the console's later `SESSION` writes.
	///
	/// A journaled name this build no longer registers is skipped with a
	/// warning rather than aborting composition: cfg and journal data from
	/// older builds is user data (ADR 0013).
	pub fn attach(ctx: Arc<Ctx>, dom: &Dom, scope: ClassScope) -> Self {
		apply_class(&ctx, &scope, dom);
		hydrate(&ctx, dom);
		let writes = ctx.subscribe_session_writes();
		Self { ctx, scope, writes, pending: Mutex::new(FastHashMap::default()) }
	}

	/// Re-derives the session layer after the tree changed underneath the
	/// console (rewind, session switch): names no longer on the live chain
	/// are cleared, the class configuration follows the session's journaled
	/// class, the rest are restored, and pending writes are dropped because
	/// they described the abandoned branch.
	pub fn resync(&self, dom: &Dom) {
		let live = con_writes(dom);
		let stale = self
			.ctx
			.session_writes()
			.filter(|(name, _)| !live.iter().any(|write| write.name == *name))
			.map(|(name, _)| name)
			.collect::<Vec<_>>();
		for name in stale {
			if let Err(error) = self.ctx.clear_session_write(name.as_str()) {
				tracing::warn!(%name, %error, "session convar could not be cleared on resync");
			}
		}
		apply_class(&self.ctx, &self.scope, dom);
		hydrate(&self.ctx, dom);
		self.pending.lock().clear();
		while self.writes.try_recv().is_ok() {}
	}

	/// Drains the console channel into the pending map.
	fn collect(&self) {
		let mut pending = self.pending.lock();
		while let Ok((name, value)) = self.writes.try_recv() {
			pending.insert(name, value);
		}
	}

	/// DOM operations that bring `<meta><con>` up to date with every write
	/// committed since the last flush; writes already reflected in the tree
	/// (a restore echo, an idempotent set) produce nothing.
	#[must_use]
	pub fn pending_ops(&self, dom: &Dom, cause: omp_journal::EntryId) -> Vec<Op> {
		self.collect();
		let mut pending = self.pending.lock();
		if pending.is_empty() {
			return Vec::new();
		}
		let current = con_writes(dom);
		let mut ops = Vec::new();
		for (name, value) in pending.drain() {
			let Some(value) = value else {
				match con_remove_txn(dom, cause, name.as_str()) {
					Ok(Some(txn)) => ops.extend(txn.ops),
					Ok(None) => {},
					Err(error) => {
						tracing::warn!(%name, %error, "session convar reset could not be journaled");
					},
				}
				continue;
			};
			let write = ConWrite {
				name:   name.clone(),
				value:  Str::new(value.to_string()),
				origin: Str::new_static(ORIGIN),
			};
			if current
				.iter()
				.any(|existing| existing.name == write.name && existing.value == write.value)
			{
				continue;
			}
			match con_write_txn(dom, cause, &write) {
				Ok(txn) => ops.extend(txn.ops),
				Err(error) => {
					tracing::warn!(%name, %error, "session convar write could not be journaled");
				},
			}
		}
		ops
	}

	/// Journals every pending write now (process exit, session switch).
	pub fn flush(&self, session: &mut Session) -> Result<(), SessionError> {
		let Some(cause) = session.head() else {
			return Ok(());
		};
		let ops = self.pending_ops(session.dom(), cause);
		if ops.is_empty() {
			return Ok(());
		}
		session.patch(Txn { cause, label: Some(Str::new_static("con.session")), ops })?;
		Ok(())
	}

	/// The kernel-side reducer that journals pending writes at every turn
	/// boundary (before the request is projected, so the journaled value
	/// and the value the kernel reads agree).
	#[must_use]
	pub fn live_component(self: &Arc<Self>) -> Box<dyn LiveComponent> {
		Box::new(TurnBoundary(Arc::clone(self)))
	}
}

/// Presents the class configuration of the session `dom` journals, when the
/// composition follows it: drops the previous session's, then — for a child
/// session — builds the child exactly as the spawn path does over the
/// console's own picture and adopts its inherited and class layers. A class
/// whose definition (`<agent>.cfg`) is gone, or that fails to configure, is
/// reported through the console sink, never skipped silently.
fn apply_class(ctx: &Ctx, scope: &ClassScope, dom: &Dom) {
	let ClassScope::Journaled(cfg) = scope else {
		return;
	};
	ctx.drop_scope();
	let Some(agent) = journaled_agent(dom) else {
		return;
	};
	if agent.as_str() != TASK_AGENT.as_str() {
		let mut file = agent.as_str().to_owned();
		file.push_str(".cfg");
		match cfg.load(&file) {
			Ok(Some(_)) => {},
			Ok(None) => ctx.reply_fmt(
				Severity::Warn,
				format_args!(
					"agent `{agent}` has no definition ({file}) anymore; this session resumes with the \
					 default subagent configuration"
				),
			),
			Err(error) => ctx.reply_fmt(
				Severity::Warn,
				format_args!(
					"agent `{agent}` definition ({file}) could not be read: {error}; this session \
					 resumes with the default subagent configuration"
				),
			),
		}
	}
	match configure_child(ctx, cfg.as_ref(), agent.as_str(), None) {
		Ok((child, _)) => ctx.adopt_scope(&child),
		Err(error) => ctx.reply_fmt(
			Severity::Warn,
			format_args!(
				"agent `{agent}` configuration could not be applied: {error}; this session resumes \
				 with the main session's configuration"
			),
		),
	}
}

fn hydrate(ctx: &Ctx, dom: &Dom) {
	for write in con_writes(dom) {
		if let Err(error) = ctx.restore_session_write(write.name.as_str(), write.value.as_str()) {
			tracing::warn!(name = %write.name, %error, "journaled session convar not restored");
		}
	}
}

impl omp_agent::SessionStateBridge for ConJournal {
	fn flush(&self, session: &mut Session) -> Result<(), SessionError> {
		ConJournal::flush(self, session)
	}

	fn resync(&self, dom: &Dom) {
		ConJournal::resync(self, dom);
	}
}

struct TurnBoundary(Arc<ConJournal>);

impl LiveComponent for TurnBoundary {
	fn id(&self) -> &str {
		"con"
	}

	fn interested(&self, kind: &Kind) -> bool {
		*kind == Kind::known(KindName::TurnStart)
	}

	fn reduce(&self, entry: &Entry, dom: &Dom) -> Result<Vec<Op>, LiveComponentError> {
		Ok(self.0.pending_ops(dom, entry.id))
	}
}

#[cfg(test)]
mod tests {
	use std::sync::Arc;

	use omp_con::Ctx;
	use omp_session::{ComponentRegistry, Session, components::con::con_writes};

	use super::{ClassScope, ConJournal};

	fn open(path: &std::path::Path) -> Session {
		if path.exists() {
			Session::open(path, ComponentRegistry::standard()).expect("session opens")
		} else {
			Session::create(path, ComponentRegistry::standard()).expect("session creates")
		}
	}

	/// ADR 0012 `SESSION`: a value set on the console is journaled, survives
	/// a reopen into a fresh console, and never touches the archive layer.
	#[test]
	fn session_writes_round_trip_through_the_journal() {
		let directory = tempfile::tempdir().expect("tempdir");
		let path = directory.path().join("con.oms");
		let ctx = Arc::new(Ctx::new());
		let mut session = open(&path);
		let journal =
			Arc::new(ConJournal::attach(Arc::clone(&ctx), session.dom(), ClassScope::Composed));
		ctx.run("ai_fastmode 1").expect("session write");
		ctx.run("ai_thinking low").expect("session write");
		assert!(con_writes(session.dom()).is_empty(), "nothing journaled before a flush");
		journal.flush(&mut session).expect("flush journals");
		let names = con_writes(session.dom())
			.into_iter()
			.map(|write| (write.name, write.value))
			.collect::<Vec<_>>();
		assert!(names.contains(&("ai_fastmode".into(), "true".into())), "{names:?}");
		assert!(names.contains(&("ai_thinking".into(), "low".into())), "{names:?}");
		// An idempotent second flush journals nothing.
		let head = session.head();
		journal.flush(&mut session).expect("no-op flush");
		assert_eq!(session.head(), head);
		drop(session);

		let restored = Arc::new(Ctx::new());
		let reopened = open(&path);
		let rejournal =
			ConJournal::attach(Arc::clone(&restored), reopened.dom(), ClassScope::Composed);
		assert!(omp_agent::AI_FASTMODE.get(&restored));
		assert_eq!(omp_agent::AI_THINKING.get(&restored), "low");
		assert!(
			restored
				.session_writes()
				.any(|(name, _)| name == "ai_fastmode"),
			"restored into the session layer, not the archive"
		);
		// The restore echo is not a new write.
		let cause = reopened.head().expect("head");
		assert!(rejournal.pending_ops(reopened.dom(), cause).is_empty());
	}

	/// ADR 0004: rewinding past the write re-derives the console from the
	/// live chain, so the value falls off with the branch.
	#[test]
	fn resync_after_rewind_drops_values_that_left_the_live_chain() {
		let directory = tempfile::tempdir().expect("tempdir");
		let path = directory.path().join("rewind.oms");
		let ctx = Arc::new(Ctx::new());
		let mut session = open(&path);
		let journal = ConJournal::attach(Arc::clone(&ctx), session.dom(), ClassScope::Composed);
		let before = session.head().expect("genesis head");
		ctx.run("ai_fastmode 1").expect("session write");
		journal.flush(&mut session).expect("flush");
		assert!(omp_agent::AI_FASTMODE.get(&ctx));
		session.rewind(before).expect("rewind to genesis");
		omp_agent::SessionStateBridge::resync(&journal, session.dom());
		assert!(!omp_agent::AI_FASTMODE.get(&ctx), "the rewound write is gone");
		assert!(!ctx.session_writes().any(|(name, _)| name == "ai_fastmode"));
	}

	/// The kernel reducer journals pending writes at the turn boundary.
	#[test]
	fn live_component_journals_at_turn_start() {
		let directory = tempfile::tempdir().expect("tempdir");
		let path = directory.path().join("turn.oms");
		let ctx = Arc::new(Ctx::new());
		let mut session = open(&path);
		let journal =
			Arc::new(ConJournal::attach(Arc::clone(&ctx), session.dom(), ClassScope::Composed));
		let component = journal.live_component();
		ctx.run("ai_fastmode 1").expect("session write");
		let turn = session.begin_turn().expect("turn");
		let entry = session.entry(turn).cloned().expect("turn entry");
		assert!(component.interested(&entry.kind));
		let ops = component.reduce(&entry, session.dom()).expect("reduce");
		assert!(!ops.is_empty());
		session
			.patch(omp_dom::Txn { cause: turn, label: None, ops })
			.expect("patch");
		assert!(
			con_writes(session.dom())
				.iter()
				.any(|write| write.name == "ai_fastmode" && write.value == "true")
		);
	}

	/// A `reset` removes the journaled value, so a reopen leaves the variable
	/// inherited instead of restoring the old override or pinning the
	/// inherited value.
	#[test]
	fn reset_removes_the_journaled_value() {
		let directory = tempfile::tempdir().expect("tempdir");
		let path = directory.path().join("reset.oms");
		let ctx = Arc::new(Ctx::new());
		let mut session = open(&path);
		let journal = ConJournal::attach(Arc::clone(&ctx), session.dom(), ClassScope::Composed);
		ctx.run("ai_thinking low").expect("session write");
		journal.flush(&mut session).expect("flush");
		assert!(
			con_writes(session.dom())
				.iter()
				.any(|write| write.name == "ai_thinking")
		);
		ctx.run("reset ai_thinking").expect("reset");
		journal.flush(&mut session).expect("flush the removal");
		assert!(
			!con_writes(session.dom())
				.iter()
				.any(|write| write.name == "ai_thinking")
		);
		drop(session);

		let restored = Arc::new(Ctx::new());
		let reopened = open(&path);
		let _journal =
			ConJournal::attach(Arc::clone(&restored), reopened.dom(), ClassScope::Composed);
		assert_eq!(omp_agent::AI_THINKING.get(&restored), omp_agent::AI_THINKING.get(&Ctx::new()));
	}

	type Replies = Arc<parking_lot::Mutex<Vec<String>>>;

	fn console() -> (Arc<Ctx>, Replies) {
		let replies = Replies::default();
		let sink = Arc::clone(&replies);
		let ctx = Ctx::builder()
			.sink(move |_, text| sink.lock().push(text.to_owned()))
			.build();
		(Arc::new(ctx), replies)
	}

	fn class_files(root: &std::path::Path, files: &[(&str, &str)]) -> Arc<dyn omp_con::CfgLoader> {
		for (name, text) in files {
			std::fs::write(root.join(name), text).expect("cfg");
		}
		Arc::new(crate::cfg::CfgFiles::with_roots(root.to_path_buf(), None))
	}

	fn child_session(path: &std::path::Path, agent: &str) -> Session {
		let mut session = open(path);
		crate::subagent::journal_agent(&mut session, crate::subagent::AgentName::from_ref(agent))
			.expect("journal agent");
		session
	}

	/// PR #91 left resuming a child from the main chat on the main console's
	/// configuration. A console following the journaled class presents the
	/// child's class exactly as the spawn path builds it — `subagent.cfg`,
	/// `<agent>.cfg`, the next recursion depth — beneath the child's own
	/// journaled writes, and drops it again on switching to a main session.
	#[test]
	fn resuming_a_child_applies_its_class_configuration_through_the_spawn_path() {
		let directory = tempfile::tempdir().expect("tempdir");
		let cfg = class_files(directory.path(), &[
			("subagent.cfg", "ai_fastmode 1\n"),
			("scout.cfg", "ai_model scout/model\nai_thinking low\nsv_tools [read grep]\n"),
		]);
		let (ctx, _) = console();
		ctx.exec(
			"ai_model main/model; ai_thinking xhigh",
			omp_con::Source::Config(omp_core::Str::new_static("config.cfg")),
		)
		.expect("main values");

		// `--resume child.oms`: the composition follows the journaled class.
		let child_path = directory.path().join("child.oms");
		let mut child = child_session(&child_path, "scout");
		let journal =
			ConJournal::attach(Arc::clone(&ctx), child.dom(), ClassScope::Journaled(Arc::clone(&cfg)));
		assert_eq!(omp_agent::AI_MODEL.get(&ctx), "scout/model");
		assert_eq!(omp_agent::AI_THINKING.get(&ctx), "low");
		assert!(omp_agent::AI_FASTMODE.get(&ctx));
		assert_eq!(omp_agent::SV_TOOLS.get(&ctx), ["read", "grep"]);
		assert_eq!(crate::subagent::settings::SV_TASK_RECURSION_DEPTH.get(&ctx), 1);

		// The child's own journaled choice outranks its class; `reset` returns
		// to the class value and the journal forgets the override.
		ctx.run("ai_thinking medium").expect("child write");
		journal.flush(&mut child).expect("flush");
		journal.resync(child.dom());
		assert_eq!(omp_agent::AI_THINKING.get(&ctx), "medium");
		ctx.run("reset ai_thinking").expect("reset");
		assert_eq!(omp_agent::AI_THINKING.get(&ctx), "low", "back to the class value");
		journal.flush(&mut child).expect("flush");
		assert!(
			!con_writes(child.dom())
				.iter()
				.any(|write| write.name == "ai_thinking")
		);

		// Switching to a main session drops the class configuration.
		let main = open(&directory.path().join("main.oms"));
		journal.resync(main.dom());
		assert_eq!(omp_agent::AI_MODEL.get(&ctx), "main/model");
		assert_eq!(omp_agent::AI_THINKING.get(&ctx), "xhigh");
		assert!(!omp_agent::AI_FASTMODE.get(&ctx));
		assert!(omp_agent::SV_TOOLS.get(&ctx).is_empty());
		assert_eq!(crate::subagent::settings::SV_TASK_RECURSION_DEPTH.get(&ctx), 0);

		// And back: the class is re-read from its definition on every resume.
		std::fs::write(directory.path().join("scout.cfg"), "ai_thinking minimal\n").expect("edit");
		journal.resync(child.dom());
		assert_eq!(omp_agent::AI_THINKING.get(&ctx), "minimal");

		// A spawned child's composition configured itself: nothing is applied.
		let (composed, _) = console();
		let _spawned = ConJournal::attach(Arc::clone(&composed), child.dom(), ClassScope::Composed);
		assert_eq!(omp_agent::AI_THINKING.get(&composed), omp_agent::AI_THINKING.get(&Ctx::new()));
	}

	/// A child whose class definition is gone resumes on the default subagent
	/// configuration, and the console says so instead of falling back silently.
	#[test]
	fn resuming_a_child_without_its_definition_reports_the_fallback() {
		let directory = tempfile::tempdir().expect("tempdir");
		let cfg = class_files(directory.path(), &[("subagent.cfg", "ai_fastmode 1\n")]);
		let (ctx, replies) = console();
		let child = child_session(&directory.path().join("gone.oms"), "retired");
		let _journal = ConJournal::attach(Arc::clone(&ctx), child.dom(), ClassScope::Journaled(cfg));
		assert!(omp_agent::AI_FASTMODE.get(&ctx), "the default subagent configuration applies");
		assert_eq!(crate::subagent::settings::SV_TASK_RECURSION_DEPTH.get(&ctx), 1);
		let replies = replies.lock().join("\n");
		assert!(replies.contains("agent `retired` has no definition (retired.cfg)"), "{replies}");

		// The bundled `task` class needs no cfg and is not reported.
		let (task_ctx, task_replies) = console();
		let empty = tempfile::tempdir().expect("tempdir");
		let task = child_session(&empty.path().join("task.oms"), "task");
		let _journal = ConJournal::attach(
			Arc::clone(&task_ctx),
			task.dom(),
			ClassScope::Journaled(class_files(empty.path(), &[])),
		);
		assert!(task_replies.lock().is_empty(), "{:?}", task_replies.lock());
		assert_eq!(crate::subagent::settings::SV_TASK_RECURSION_DEPTH.get(&task_ctx), 1);
	}
}
