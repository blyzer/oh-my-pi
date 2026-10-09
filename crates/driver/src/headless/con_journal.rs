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
//! child's configuration: the spawn path ([`configure_resumed_child`]) builds
//! it over the main scope's picture at the child's journaled recursion depth,
//! and [`Ctx::adopt_scope`] installs its inherited and class layers beneath
//! the child's journaled session writes, parking the main scope's session
//! layer so none of the main session's writes outrank the class or journal
//! into the child. Switching to a main session drops them again and restores
//! the parked layer ([`Ctx::drop_scope`]). Neither class layer is journaled
//! or persisted, so the child's definition is re-read on every resume.

use std::{path::Path, sync::Arc};

use omp_agent::{LiveComponent, LiveComponentError};
use omp_con::{CfgLoader, Ctx, Severity, Value};
use omp_core::{FastHashMap, Str};
use omp_dom::{Dom, Op, Txn};
use omp_env::project_state::DaemonPolicy;
use omp_journal::{Entry, Kind, KindName};
use omp_session::{
	ComponentRegistry, Session, SessionError,
	components::con::{ConWrite, con_remove_txn, con_write_txn, con_writes},
};
use parking_lot::Mutex;

use crate::subagent::{
	TASK_AGENT, journaled_agent, journaled_depth, spawn::configure_resumed_child,
};

/// Provenance recorded on journaled session writes.
const ORIGIN: &str = "session";

/// How a composition's console follows the agent class of its live session.
pub enum ClassScope {
	/// The composition configured the class itself (a spawned, revived, or
	/// workpool child built by
	/// [`configure_child`](crate::subagent::spawn::configure_child)); the
	/// console stays as composed.
	Composed,
	/// Follow the class journaled on the live session (the main chat,
	/// `--resume`): a child session presents its class configuration through
	/// the spawn path, reading class cfgs through this loader; a main session
	/// presents none.
	Journaled(Arc<dyn CfgLoader>),
}

/// The one console-to-journal channel for a composed session.
pub struct ConJournal {
	ctx:      Arc<Ctx>,
	scope:    ClassScope,
	writes:   flume::Receiver<omp_con::SessionWrite>,
	/// Writes observed but not yet journaled, last value per name; `None`
	/// drops the journaled value.
	pending:  Mutex<FastHashMap<Str, Option<Value>>>,
	/// The sandbox and approval policy the console presented when the session
	/// was attached, which the composition's environment enforces for its
	/// whole life (`compose_kernel` refuses a session that presents another).
	composed: DaemonPolicy,
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
		apply_class(&ctx, &scope, dom, Report::Console);
		hydrate(&ctx, dom);
		let writes = ctx.subscribe_session_writes();
		let composed = omp_envd::daemon_policy::from_con(&ctx);
		Self { ctx, scope, writes, pending: Mutex::new(FastHashMap::default()), composed }
	}

	/// Re-derives the session layer after the tree changed underneath the
	/// console (rewind, session switch): names no longer on the live chain
	/// are cleared, the class configuration follows the session's journaled
	/// class, the rest are restored, and pending writes are dropped because
	/// they described the abandoned branch.
	///
	/// A switch to a session whose class sets another sandbox and approval
	/// policy than the composition's environment enforces is reported through
	/// the console: the environment keeps the policy it started with.
	pub fn resync(&self, dom: &Dom) {
		// Only a journaled class moves the policy: no sandbox or approval
		// convar is `SESSION`-flagged, so hydration never does.
		let before = matches!(self.scope, ClassScope::Journaled(_))
			.then(|| omp_envd::daemon_policy::from_con(&self.ctx));
		// The class goes first: returning to a main session restores its
		// parked session layer, which the stale sweep then aligns with the
		// journal of the session actually presented.
		apply_class(&self.ctx, &self.scope, dom, Report::Console);
		if let Some(before) = before {
			self.report_policy_drift(before);
		}
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
		hydrate(&self.ctx, dom);
		self.pending.lock().clear();
		while self.writes.try_recv().is_ok() {}
	}

	/// Reports a class presentation that moved the console's sandbox and
	/// approval policy from `before` to one the environment does not enforce.
	/// A presentation that keeps the policy, such as the resync at every
	/// command boundary, or that returns to the composed one says nothing.
	fn report_policy_drift(&self, before: DaemonPolicy) {
		let presented = omp_envd::daemon_policy::from_con(&self.ctx);
		if presented == before || presented == self.composed {
			return;
		}
		let composed = self.composed;
		tracing::warn!(
			%presented,
			%composed,
			"the presented session's class sets another sandbox and approval policy than its \
			 environment enforces"
		);
		self.ctx.reply_fmt(
			Severity::Warn,
			format_args!(
				"this session's agent class sets sandbox and approval policy {presented}, but its \
				 tools keep running in this process's environment, which enforces {composed}; resume \
				 the session in a new process (`omp --resume`) to run them under its own policy"
			),
		);
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

/// Presents the class journaled on the session at `journal` before the
/// composition opens that session for good, when the composition follows the
/// session's class.
///
/// A composition attaches its project environment before it opens the
/// session, and a project daemon or an embedded host fixes its sandbox and
/// approval policy when it starts. Without this a resumed child's environment
/// would enforce the main configuration while the session presents its class
/// configuration. The journal is materialized once with the standard
/// components (the class is a plain `<meta>` property) and closed again;
/// [`ConJournal::attach`] then presents the same class and reports what this
/// leaves unsaid. A journal that cannot be read here is left to that open.
pub fn present_journaled_class(ctx: &Ctx, scope: &ClassScope, journal: &Path) {
	if !matches!(scope, ClassScope::Journaled(_)) || !journal.exists() {
		return;
	}
	match Session::open(journal, ComponentRegistry::standard()) {
		Ok(session) => apply_class(ctx, scope, session.dom(), Report::Quiet),
		Err(error) => tracing::warn!(
			%error,
			journal = %journal.display(),
			"the journaled agent class could not be read before the environment started"
		),
	}
}

/// Whether presenting a class reports what it could not apply.
#[derive(Clone, Copy, Eq, PartialEq)]
enum Report {
	/// Through the console sink.
	Console,
	/// Not at all: a later presentation of the same session reports it.
	Quiet,
}

/// Presents the class configuration of the session `dom` journals, when the
/// composition follows it. For a child session it builds the child exactly
/// as the spawn path does ([`configure_resumed_child`]) — over the main
/// scope's picture, at the journaled recursion depth — and adopts its
/// inherited and class layers; the first adoption parks the main scope's
/// session layer, so the main session's writes sit beneath the class as the
/// inherited seed rather than above it. For a main session it drops the
/// adopted scope, restoring the parked layer. A class whose definition
/// (`<agent>.cfg`) is gone, or that fails to configure, is reported through
/// the console sink, never skipped silently.
fn apply_class(ctx: &Ctx, scope: &ClassScope, dom: &Dom, report: Report) {
	let ClassScope::Journaled(cfg) = scope else {
		return;
	};
	let Some(agent) = journaled_agent(dom) else {
		ctx.drop_scope();
		return;
	};
	if report == Report::Console && agent.as_str() != TASK_AGENT.as_str() {
		let mut file = agent.as_str().to_owned();
		file.push_str(".cfg");
		match cfg
			.load(&file)
			.and_then(|user| Ok(user.is_some() || cfg.load_project(&file)?.is_some()))
		{
			Ok(true) => {},
			Ok(false) => ctx.reply_fmt(
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
	// A child journaled before depths were is resumed as a direct child of
	// the main session.
	let depth = journaled_depth(dom).unwrap_or(1);
	match configure_resumed_child(ctx, cfg.as_ref(), agent.as_str(), depth) {
		Ok((child, _)) => ctx.adopt_scope(&child),
		Err(error) => {
			ctx.drop_scope();
			if report == Report::Quiet {
				return;
			}
			ctx.reply_fmt(
				Severity::Warn,
				format_args!(
					"agent `{agent}` configuration could not be applied: {error}; this session resumes \
					 with the main session's configuration"
				),
			);
		},
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

	/// A session account pin is journaled as an opaque digest (no account id or
	/// principal), restored on reopen so the pin survives the session, and
	/// removed from the journal when it is cleared.
	#[test]
	fn account_pin_survives_reopen_as_an_opaque_digest() {
		use std::collections::BTreeSet;

		use omp_ai::{
			AccountId, AccountRoutingContext, PrincipalId,
			account::{
				AI_ACCOUNT_PINS, AccountPin, AccountPool, AccountRecord, with_pin, without_pin,
			},
			auth::CredentialAffinityResolver,
		};
		use omp_catalog::ProviderId;

		let provider = ProviderId::from("provider");
		let record = AccountRecord {
			account:               AccountId::from("raw-account-uuid"),
			principal:             PrincipalId::from("person@example.test"),
			provider:              provider.clone(),
			routes:                BTreeSet::new(),
			enabled:               true,
			credential_generation: 1,
			routing:               AccountRoutingContext::default(),
		};
		let pool = AccountPool::new();
		pool.upsert(record.clone()).expect("account registers");
		let resolver = CredentialAffinityResolver::new([5; 32]);
		let directory = tempfile::tempdir().expect("tempdir");
		let path = directory.path().join("pin.oms");
		let ctx = Arc::new(Ctx::new());
		let mut session = open(&path);
		let journal = ConJournal::attach(Arc::clone(&ctx), session.dom(), ClassScope::Composed);
		let pinned = with_pin(&omp_con::Kv::new(), &provider, &resolver.digest(&record));
		AI_ACCOUNT_PINS
			.set(&ctx, pinned.clone())
			.expect("pin writes");
		journal.flush(&mut session).expect("flush journals the pin");
		let writes = con_writes(session.dom());
		let write = writes
			.iter()
			.find(|write| write.name == "ai_account_pins")
			.expect("the pin is journaled");
		assert!(!write.value.contains("raw-account-uuid"), "{}", write.value);
		assert!(!write.value.contains("person@example.test"), "{}", write.value);
		drop(session);

		let restored = Arc::new(Ctx::new());
		let mut reopened = open(&path);
		let rejournal =
			ConJournal::attach(Arc::clone(&restored), reopened.dom(), ClassScope::Composed);
		let recorded = AI_ACCOUNT_PINS.get(&restored);
		assert_eq!(
			resolver
				.session_pins(&pool, &recorded)
				.for_provider(&provider),
			Some(&AccountPin::Account(record.account.clone())),
			"the reopened session resolves the same account"
		);

		AI_ACCOUNT_PINS
			.set(&restored, without_pin(&recorded, &provider))
			.expect("pin clears");
		rejournal
			.flush(&mut reopened)
			.expect("flush journals the unpin");
		assert!(
			resolver
				.session_pins(&pool, &AI_ACCOUNT_PINS.get(&restored))
				.is_empty()
		);
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

	fn child_session(path: &std::path::Path, agent: &str, depth: u32) -> Session {
		let mut session = open(path);
		crate::subagent::journal_agent(
			&mut session,
			crate::subagent::AgentName::from_ref(agent),
			depth,
		)
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
		let mut child = child_session(&child_path, "scout", 1);
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
		let child = child_session(&directory.path().join("gone.oms"), "retired", 1);
		let _journal = ConJournal::attach(Arc::clone(&ctx), child.dom(), ClassScope::Journaled(cfg));
		assert!(omp_agent::AI_FASTMODE.get(&ctx), "the default subagent configuration applies");
		assert_eq!(crate::subagent::settings::SV_TASK_RECURSION_DEPTH.get(&ctx), 1);
		let replies = replies.lock().join("\n");
		assert!(replies.contains("agent `retired` has no definition (retired.cfg)"), "{replies}");

		// The bundled `task` class needs no cfg and is not reported.
		let (task_ctx, task_replies) = console();
		let empty = tempfile::tempdir().expect("tempdir");
		let task = child_session(&empty.path().join("task.oms"), "task", 1);
		let _journal = ConJournal::attach(
			Arc::clone(&task_ctx),
			task.dom(),
			ClassScope::Journaled(class_files(empty.path(), &[])),
		);
		assert!(task_replies.lock().is_empty(), "{:?}", task_replies.lock());
		assert_eq!(crate::subagent::settings::SV_TASK_RECURSION_DEPTH.get(&task_ctx), 1);
	}

	fn bool_var(ctx: &Ctx, name: &str) -> bool {
		match ctx.get(name) {
			Some(omp_con::Value::Bool(value)) => value,
			other => panic!("`{name}` is not a boolean: {other:?}"),
		}
	}

	/// Resuming a child parks the main session's writes: neither its
	/// journaled values nor its unjournaled console and host writes outrank
	/// the child's class. The unjournaled ones reach the child only as the
	/// seed it inherits, nothing of the main session is journaled into the
	/// child, and switching back restores them.
	#[test]
	fn resuming_a_child_parks_the_main_session_writes_beneath_its_class() {
		let directory = tempfile::tempdir().expect("tempdir");
		let cfg =
			class_files(directory.path(), &[("scout.cfg", "ai_thinking low\nsv_lsp_enabled 1\n")]);
		let (ctx, _) = console();
		ctx.exec(
			"ai_model main/model",
			omp_con::Source::Config(omp_core::Str::new_static("config.cfg")),
		)
		.expect("user cfg");

		// The main chat, on a main session: one journaled write, two the
		// journal does not keep (variables without `SESSION`).
		let mut main = open(&directory.path().join("main.oms"));
		let journal =
			ConJournal::attach(Arc::clone(&ctx), main.dom(), ClassScope::Journaled(Arc::clone(&cfg)));
		ctx.run("ai_thinking xhigh").expect("journaled write");
		ctx.run("sv_lsp_enabled 0").expect("unjournaled write");
		ctx.run("ai_external_thinking 1")
			.expect("unjournaled write");
		journal.flush(&mut main).expect("flush");

		// `/resume` of a `scout` child.
		let child_path = directory.path().join("child.oms");
		let mut child = child_session(&child_path, "scout", 1);
		journal.resync(child.dom());
		let assert_child = |ctx: &Ctx| {
			assert_eq!(omp_agent::AI_THINKING.get(ctx), "low", "the class outranks a journaled write");
			assert!(bool_var(ctx, "sv_lsp_enabled"), "the class outranks an unjournaled write");
			assert!(bool_var(ctx, "ai_external_thinking"), "inherited beneath the class");
			assert_eq!(omp_agent::AI_MODEL.get(ctx), "main/model");
		};
		assert_child(&ctx);
		assert!(ctx.session_writes().next().is_none(), "the child's session layer is its own");
		// The resync at every command boundary presents the same picture.
		journal.flush(&mut child).expect("flush");
		journal.resync(child.dom());
		assert_child(&ctx);
		assert!(con_writes(child.dom()).is_empty(), "nothing of the main session is journaled");

		// The child's own writes outrank its class and last across command
		// boundaries.
		ctx.run("ai_thinking medium").expect("child write");
		ctx.run("sv_lsp_enabled 0").expect("child write");
		journal.flush(&mut child).expect("flush");
		journal.resync(child.dom());
		assert_eq!(omp_agent::AI_THINKING.get(&ctx), "medium");
		assert!(!bool_var(&ctx, "sv_lsp_enabled"));
		assert_eq!(
			con_writes(child.dom())
				.into_iter()
				.map(|write| write.name)
				.collect::<Vec<_>>(),
			["ai_thinking"]
		);

		// Switching back restores the main session's writes.
		ctx.run("sv_lsp_enabled 1").expect("child write");
		journal.flush(&mut child).expect("flush");
		journal.resync(main.dom());
		assert_eq!(omp_agent::AI_THINKING.get(&ctx), "xhigh");
		assert!(!bool_var(&ctx, "sv_lsp_enabled"), "the main session's write is back");
		assert!(bool_var(&ctx, "ai_external_thinking"));
		assert_eq!(omp_agent::AI_MODEL.get(&ctx), "main/model");

		// From the child straight to a new main session: the main scope's
		// unjournaled writes return, the previous main session's journaled
		// ones stay in its journal.
		journal.resync(child.dom());
		assert_eq!(omp_agent::AI_THINKING.get(&ctx), "medium");
		let fresh = open(&directory.path().join("fresh.oms"));
		journal.resync(fresh.dom());
		assert_eq!(omp_agent::AI_THINKING.get(&ctx), omp_agent::AI_THINKING.get(&Ctx::new()));
		assert!(!bool_var(&ctx, "sv_lsp_enabled"));
		assert!(bool_var(&ctx, "ai_external_thinking"));
	}

	fn task_tool(
		ctx: &Arc<Ctx>,
		cfg: &Arc<dyn omp_con::CfgLoader>,
	) -> crate::subagent::spawn::TaskSessionTool {
		let scratch = std::env::temp_dir();
		let (env, _transport) = omp_env::EnvClient::in_process(0);
		crate::subagent::spawn::TaskSessionTool::new(
			scratch.clone(),
			scratch.clone(),
			scratch,
			Arc::new(crate::sessions::SessionRegistry::new()),
			Arc::clone(ctx),
			Arc::clone(cfg),
			env,
			omp_core::Str::new_static("main"),
			omp_core::Str::new_static("provider/model"),
		)
	}

	/// The `task` tool's place in the roster follows the class and recursion
	/// depth of the session the console presents, as a freshly spawned child
	/// of that class and depth has it: a resumed child at the recursion
	/// ceiling is never advertised `task`, and switching back to the main
	/// session advertises it again.
	#[test]
	fn resumed_child_roster_matches_a_spawned_child_at_its_depth() {
		use omp_agent::SessionTool as _;

		use crate::subagent::spawn::configure_child;

		let directory = tempfile::tempdir().expect("tempdir");
		let cfg = class_files(directory.path(), &[("scout.cfg", "ai_thinking low\n")]);
		let (ctx, _) = console();
		ctx.exec(
			"sv_task_max_recursion_depth 2",
			omp_con::Source::Config(omp_core::Str::new_static("config.cfg")),
		)
		.expect("user cfg");
		let tool = task_tool(&ctx, &cfg);
		let main = open(&directory.path().join("main.oms"));
		let journal =
			ConJournal::attach(Arc::clone(&ctx), main.dom(), ClassScope::Journaled(Arc::clone(&cfg)));
		assert!(tool.advertised(), "the main session may delegate");

		// Children spawned from here: `scout` at depth 1, its own `scout` at 2.
		let (spawned, _) = configure_child(&ctx, cfg.as_ref(), "scout", None).expect("child");
		let spawned = Arc::new(spawned);
		let (grandchild, _) =
			configure_child(&spawned, cfg.as_ref(), "scout", None).expect("grandchild");
		let grandchild = Arc::new(grandchild);
		let spawned_tool = task_tool(&spawned, &cfg);
		let grandchild_tool = task_tool(&grandchild, &cfg);
		assert!(spawned_tool.advertised());
		assert!(!grandchild_tool.advertised(), "the ceiling withholds `task`");

		// Resuming each from the main chat presents the same roster.
		let child = child_session(&directory.path().join("child.oms"), "scout", 1);
		journal.resync(child.dom());
		assert_eq!(tool.advertised(), spawned_tool.advertised());
		let deep = child_session(&directory.path().join("grandchild.oms"), "scout", 2);
		journal.resync(deep.dom());
		assert_eq!(crate::subagent::settings::SV_TASK_RECURSION_DEPTH.get(&ctx), 2);
		assert_eq!(tool.advertised(), grandchild_tool.advertised());
		// Every command boundary re-derives the same depth.
		journal.resync(deep.dom());
		assert!(!tool.advertised());

		// Switching back restores the main session's roster.
		journal.resync(main.dom());
		assert!(tool.advertised());
		assert_eq!(crate::subagent::settings::SV_TASK_RECURSION_DEPTH.get(&ctx), 0);
	}

	/// A composition attaches its environment before it opens the session,
	/// and the environment fixes its sandbox and approval policy when it
	/// starts. The early presentation puts a resumed child's class policy in
	/// force first, without a word, and attaching the session afterwards
	/// presents the same policy and reports what could not be applied once.
	#[test]
	fn the_journaled_class_is_presented_before_the_environment_starts() {
		use omp_envd::{
			daemon_policy,
			exec_settings::{ExecSandboxMode, SV_SANDBOX_MODE},
		};

		let directory = tempfile::tempdir().expect("tempdir");
		let cfg = class_files(directory.path(), &[("reviewer.cfg", "sv_sandbox_mode read-only\n")]);
		let child_path = directory.path().join("child.oms");
		drop(child_session(&child_path, "reviewer", 1));
		let (ctx, replies) = console();
		let main = daemon_policy::from_con(&ctx);
		let scope = ClassScope::Journaled(Arc::clone(&cfg));

		super::present_journaled_class(&ctx, &scope, &child_path);
		assert_eq!(SV_SANDBOX_MODE.get(&ctx), ExecSandboxMode::ReadOnly);
		let early = daemon_policy::from_con(&ctx);
		assert_ne!(early, main, "the class policy is in force before the environment starts");
		let child = open(&child_path);
		let _journal = ConJournal::attach(Arc::clone(&ctx), child.dom(), scope);
		assert_eq!(daemon_policy::from_con(&ctx), early, "the session presents the same policy");
		assert!(replies.lock().is_empty(), "{:?}", replies.lock());

		// A new session, a main session and a composed child keep the console.
		let (fresh, _) = console();
		let journaled = ClassScope::Journaled(Arc::clone(&cfg));
		super::present_journaled_class(&fresh, &journaled, &directory.path().join("new.oms"));
		assert_eq!(daemon_policy::from_con(&fresh), main);
		let main_path = directory.path().join("main.oms");
		drop(open(&main_path));
		super::present_journaled_class(&fresh, &journaled, &main_path);
		assert_eq!(daemon_policy::from_con(&fresh), main);
		super::present_journaled_class(&fresh, &ClassScope::Composed, &child_path);
		assert_eq!(daemon_policy::from_con(&fresh), main);

		// A class gone from its definitions is reported once, by the attach.
		let retired_path = directory.path().join("retired.oms");
		drop(child_session(&retired_path, "retired", 1));
		let (retired, replies) = console();
		super::present_journaled_class(&retired, &journaled, &retired_path);
		assert!(replies.lock().is_empty(), "the early presentation is quiet");
		let session = open(&retired_path);
		let _journal = ConJournal::attach(Arc::clone(&retired), session.dom(), journaled);
		let reported = replies.lock().join("\n");
		assert_eq!(
			reported
				.matches("agent `retired` has no definition")
				.count(),
			1,
			"{reported}"
		);
	}

	/// A chat keeps the environment it was composed with, so switching to a
	/// child whose class sets another sandbox policy (`/resume` from the main
	/// chat) says so once, naming both policies. The resync at every later
	/// command boundary and the switch back to the main session say nothing.
	#[test]
	fn switching_to_a_class_of_another_policy_is_reported_once() {
		use omp_envd::daemon_policy;

		let directory = tempfile::tempdir().expect("tempdir");
		let cfg = class_files(directory.path(), &[
			("reviewer.cfg", "sv_sandbox_mode read-only\n"),
			("scout.cfg", "ai_thinking low\n"),
		]);
		let (ctx, replies) = console();
		let main = open(&directory.path().join("main.oms"));
		let journal =
			ConJournal::attach(Arc::clone(&ctx), main.dom(), ClassScope::Journaled(Arc::clone(&cfg)));
		let composed = daemon_policy::from_con(&ctx);

		// A class that keeps the policy says nothing.
		let scout = child_session(&directory.path().join("scout.oms"), "scout", 1);
		journal.resync(scout.dom());
		assert!(replies.lock().is_empty(), "{:?}", replies.lock());

		let reviewer = child_session(&directory.path().join("reviewer.oms"), "reviewer", 1);
		journal.resync(reviewer.dom());
		let presented = daemon_policy::from_con(&ctx);
		assert_ne!(presented, composed);
		journal.resync(reviewer.dom());
		journal.resync(main.dom());
		assert_eq!(daemon_policy::from_con(&ctx), composed);
		let reported = replies.lock().join("\n");
		assert_eq!(
			reported
				.matches(&format!(
					"sets sandbox and approval policy {presented}, but its tools keep running in this \
					 process's environment, which enforces {composed}"
				))
				.count(),
			1,
			"{reported}"
		);
		assert_eq!(replies.lock().len(), 1, "{reported}");
	}

	/// The reported gap: `--resume` of a child whose class sets
	/// `sv_sandbox_mode read-only` composed its environment from the main
	/// configuration and joined that configuration's project daemon, where
	/// `bash` may write the workspace, while the session presented the class.
	/// Presented first, the class keys the environment: the session never
	/// joins that daemon, spawns none it could not join, and runs embedded
	/// under the class policy.
	#[cfg(unix)]
	#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
	async fn a_resumed_child_never_joins_the_main_configurations_daemon() {
		use std::time::Duration;

		use omp_core::{Principal, sf};
		use omp_env::project_state::{DaemonPolicy, document_socket, environment_socket};
		use omp_envd::{
			AttachOptions, EnvServer, ProjectEnvironment, RegistryBridges, daemon_policy,
			exthost::ConvarControlFactory, worker::ExtHostConfig,
		};
		use tokio_util::sync::CancellationToken;

		let scratch = tempfile::tempdir().expect("scratch");
		let root = scratch.path().join("workspace");
		let state = scratch.path().join("state");
		let classes = scratch.path().join("o2");
		for directory in [&root, &state, &classes] {
			std::fs::create_dir_all(directory).expect("directory");
		}
		let root = std::fs::canonicalize(&root).expect("canonical workspace");
		let cfg = class_files(&classes, &[("reviewer.cfg", "sv_sandbox_mode read-only\n")]);
		let child_path = scratch.path().join("child.oms");
		drop(child_session(&child_path, "reviewer", 1));

		// A daemon started under the main configuration serves the project.
		let main = Arc::new(Ctx::new());
		let main_policy = daemon_policy::from_con(&main);
		let socket = environment_socket(&state, &main_policy).expect("environment socket");
		let server = EnvServer::open_project(
			&root,
			&state,
			&document_socket(&state),
			omp_tool::Registry::new(),
			ExtHostConfig::current(
				Principal::new(sf!("daemon-tester"), sf!("Daemon Tester")),
				sf!("daemon-session"),
				1,
			)
			.expect("daemon host configuration"),
			None,
			false,
			None,
			&main,
			Arc::new(ConvarControlFactory::new(Arc::clone(&main))),
			RegistryBridges::default(),
		)
		.await
		.expect("project daemon");
		let shutdown = CancellationToken::new();
		let serving = tokio::spawn({
			let server = Arc::new(server);
			let socket = socket.clone();
			let shutdown = shutdown.clone();
			async move { server.serve_uds(&socket, shutdown, None).await }
		});
		tokio::time::timeout(Duration::from_secs(30), async {
			while tokio::net::UnixStream::connect(&socket).await.is_err() {
				tokio::time::sleep(Duration::from_millis(10)).await;
			}
		})
		.await
		.expect("the project daemon never listened");
		let attach = |con: Arc<Ctx>| {
			ProjectEnvironment::attach(&root, &state, AttachOptions {
				py_eval: false,
				approval_mode: None,
				trusted_extensions: Vec::new(),
				contributed_values: Vec::new(),
				con,
				bridges: RegistryBridges::default(),
				spawn_idle_timeout: Some(2),
				spawn_policy: Some(main_policy),
			})
		};

		// The main configuration joins its daemon.
		let joined = attach(Arc::clone(&main)).await.expect("environment");
		assert!(joined.fallback_notice.is_none(), "{:?}", joined.fallback_notice);
		drop(joined);

		let (session, _) = console();
		super::present_journaled_class(&session, &ClassScope::Journaled(cfg), &child_path);
		let environment = attach(Arc::clone(&session)).await.expect("environment");
		let notice = environment
			.fallback_notice
			.clone()
			.expect("the resumed child joined the main configuration's daemon");
		let class_policy = daemon_policy::from_con(&session);
		assert_ne!(class_policy, main_policy);
		assert!(
			notice.contains(&format!(
				"policy {main_policy} from the configuration files, not this session's {class_policy}"
			)),
			"the notice names both policies: {notice}"
		);
		let hello = environment.client().info().expect("hello");
		assert_eq!(
			DaemonPolicy::from_wire(&hello.policy_digest),
			Some(class_policy),
			"the embedded environment enforces the class policy"
		);
		assert!(
			!state.join("envd.log").exists(),
			"no daemon the session could never join was spawned"
		);
		drop(environment);
		shutdown.cancel();
		serving.abort();
	}
}
