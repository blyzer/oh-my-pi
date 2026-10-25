//! The `stream-rules` Director over the real loop (ADR 0038, test plan for
//! `directors/stream_rules`): repeat policies read their ages from the
//! journaled session, so they hold across a resume; a match on a response that
//! fails before the loop commits it is neither marked nor published; and a
//! match publishes its hook observation exactly once, after commit.

use std::{path::Path, sync::Arc};

use omp_agent::{
	DirectorRegistry, DirectorStack, DispatchPolicy, HookDispatch, HookGate, HookPhase, Inference,
	Kernel, KernelError, OnFailure, RunControl, SourceRef, StaticPrompt, TurnInput, TurnStop, When,
	directors::stream_rules::{FAMILY, RulePattern, StreamRuleSet, StreamRules},
};
use omp_ai::{
	BlockKind, ChatEvent, ChatRequest, ChatStream, Error, ErrorDetail, ErrorKind, ErrorPhase,
	ExecutionReceipt, ProviderId, ReasonId, RequestId, ResponseMeta, RetryAction, RouteId,
};
use omp_con::{Ctx, Source};
use omp_core::{Str, sf};
use omp_journal::blob::BlobStore;
use omp_proto::toolhost::v1::HookEventId;
use omp_session::{ComponentRegistry, Session};

mod support;

use support::{ScriptedInference, fresh_session, journal_entries, registry, text_script};

/// Registers the one rule these tests use: any text response containing
/// `danger`.
fn directors() -> DirectorRegistry {
	let rule = RulePattern {
		name:           Str::new_static("guard"),
		body:           Str::new_static("Avoid that word."),
		pattern:        Str::new_static("danger"),
		scope:          vec![Str::new_static("text")],
		globs:          Vec::new(),
		interrupt_mode: None,
	};
	let set = StreamRuleSet::compile(vec![rule])
		.set
		.expect("the rule compiles");
	let mut directors = DirectorRegistry::standard();
	directors.register_extension(Box::new(StreamRules::new(Arc::new(set))));
	directors
}

fn settings(lines: &[&str]) -> Arc<Ctx> {
	let ctx = Ctx::new();
	for line in lines {
		ctx.exec(line, Source::Console).expect("setting applies");
	}
	Arc::new(ctx)
}

fn subscription() -> omp_agent::hooks::Subscription {
	omp_agent::hooks::Subscription {
		host:       sf!("test"),
		source:     SourceRef {
			layer:        0,
			publisher:    sf!("test"),
			extension_id: sf!("stream-rules"),
		},
		id:         1,
		event:      HookEventId::HookEventStreamRuleTriggered,
		phase:      HookPhase::Observe,
		order:      0,
		on_failure: OnFailure::Defer,
		when:       When::default(),
	}
}

fn observer() -> (Arc<HookGate>, flume::Receiver<HookDispatch>) {
	let (gate, dispatches) = HookGate::channel();
	let gate = Arc::new(gate);
	gate
		.subscribe("test", [subscription()])
		.expect("observer subscription");
	(gate, dispatches)
}

fn kernel<C: Inference>(
	client: C,
	directory: &Path,
	directors: &DirectorRegistry,
	ctx: &Arc<Ctx>,
	gate: &Arc<HookGate>,
) -> Kernel<C> {
	Kernel::new(
		client,
		registry(std::iter::empty()),
		DispatchPolicy::new(BlobStore::open(directory.join("blobs")).expect("blobs")),
		StaticPrompt(Str::new_static("test system")),
	)
	.with_director_registry(directors.clone())
	.with_hook_gate(Arc::clone(gate))
	.with_con_context(Arc::clone(ctx))
}

const fn turn(text: &'static str) -> TurnInput {
	TurnInput { text: Str::new_static(text), attachments: Vec::new() }
}

/// The observations a rule match published to subscribed extensions.
fn triggered(dispatches: &flume::Receiver<HookDispatch>) -> Vec<serde_json::Value> {
	dispatches
		.try_iter()
		.filter(|dispatch| dispatch.event == HookEventId::HookEventStreamRuleTriggered)
		.map(|dispatch| serde_json::from_slice(&dispatch.payload).expect("JSON payload"))
		.collect()
}

/// Runs the first turn of a session: the rule fires on the first response and
/// the redirected resample is clean. Returns the journal path.
#[expect(
	clippy::future_not_send,
	reason = "the helper holds a Session, whose boxed Send-only stream is not Sync"
)]
async fn first_turn_fires(
	directory: &Path,
	directors: &DirectorRegistry,
	ctx: &Arc<Ctx>,
) -> std::path::PathBuf {
	let path = directory.join("session.oms");
	let (inference, requests) =
		ScriptedInference::new([text_script("a danger answer"), text_script("a clean answer")]);
	let (gate, dispatches) = observer();
	let mut kernel = kernel(inference, directory, directors, ctx, &gate);
	let mut session = fresh_session(&path);
	DirectorStack::from_dom(session.dom(), directors)
		.engage_registered(&mut session, FAMILY)
		.expect("the director engages");
	let outcome = kernel
		.run_turn(&mut session, turn("first"), RunControl::default())
		.await
		.expect("the redirected turn completes");
	assert_eq!(outcome.stop, TurnStop::Completed);
	assert_eq!(outcome.assistant_text, "a clean answer");
	assert_eq!(requests.lock().len(), 2, "the match redirects within the turn");
	assert_eq!(triggered(&dispatches).len(), 1, "one match, one observation");
	path
}

/// Reopens the journal in a fresh kernel and runs a second turn whose first
/// response matches the rule again. Returns how many requests that turn made:
/// 1 when the rule stayed quiet, 2 when it fired and redirected.
#[expect(
	clippy::future_not_send,
	reason = "the helper holds a Session, whose boxed Send-only stream is not Sync"
)]
async fn resumed_turn_requests(
	path: &Path,
	directory: &Path,
	directors: &DirectorRegistry,
	ctx: &Arc<Ctx>,
) -> usize {
	let mut session = Session::open(path, ComponentRegistry::standard()).expect("journal reopens");
	let (inference, requests) = ScriptedInference::new([
		text_script("a danger answer again"),
		text_script("a clean answer again"),
	]);
	let (gate, _dispatches) = observer();
	let mut kernel = kernel(inference, directory, directors, ctx, &gate);
	kernel
		.run_turn(&mut session, turn("second"), RunControl::default())
		.await
		.expect("the resumed turn completes");
	requests.lock().len()
}

/// `once` is the default repeat policy: a rule that fired stays fired after
/// the session resumes in a new kernel, because the mark is in the journal.
#[tokio::test]
async fn a_once_rule_stays_fired_after_the_session_resumes() {
	let directory = tempfile::tempdir().expect("temporary directory");
	let directors = directors();
	let ctx = settings(&[]);
	let path = first_turn_fires(directory.path(), &directors, &ctx).await;
	assert_eq!(
		resumed_turn_requests(&path, directory.path(), &directors, &ctx).await,
		1,
		"a fired `once` rule does not redirect again after a resume"
	);
}

/// `after-gap` derives a rule's age from assistant ordinals: the first turn
/// ended with two responses, so on the resumed turn's first response the rule
/// is two responses old. A gap of 3 keeps it quiet; a gap of 2 lets it fire.
#[tokio::test]
async fn an_after_gap_rule_reopens_by_assistant_ordinal_across_a_resume() {
	for (gap, expected_requests) in [("3", 1), ("2", 2)] {
		let directory = tempfile::tempdir().expect("temporary directory");
		let directors = directors();
		let ctx = settings(&[
			"ai_stream_rules_repeat after-gap",
			&format!("ai_stream_rules_repeat_gap {gap}"),
		]);
		let path = first_turn_fires(directory.path(), &directors, &ctx).await;
		assert_eq!(
			resumed_turn_requests(&path, directory.path(), &directors, &ctx).await,
			expected_requests,
			"gap {gap}"
		);
	}
}

/// Fails mid-stream after text that matches the rule.
struct FailsAfterAMatch;

impl Inference for FailsAfterAMatch {
	fn chat(
		&mut self,
		_request: ChatRequest,
	) -> impl Future<Output = Result<ChatStream, Error>> + Send {
		let events = vec![
			Ok(ChatEvent::Started(ResponseMeta {
				request_id:          RequestId::from("scripted-request"),
				provider:            ProviderId::from("scripted"),
				route:               RouteId::from("scripted/test"),
				model:               None,
				provider_request_id: None,
				created_at:          std::time::SystemTime::UNIX_EPOCH,
			})),
			Ok(ChatEvent::BlockStarted { index: 0, kind: BlockKind::Text }),
			Ok(ChatEvent::TextDelta { index: 0, text: Str::new_static("a danger answer") }),
			Err(
				Error::new(
					ErrorKind::Connectivity,
					ErrorPhase::Streaming,
					RetryAction::Never,
					ExecutionReceipt::default(),
				)
				.committed(true)
				.detail(ErrorDetail::protocol(ReasonId::new_static("http-response-body"))),
			),
		];
		std::future::ready(Ok(ChatStream::ordinary(Box::pin(futures::stream::iter(events)))))
	}
}

/// A non-interrupting match is only an effect the loop holds until the
/// response completes. When the stream fails first, the effect is dropped: the
/// rule is not marked fired and its observation is never published, so the
/// same text on the next attempt still matches.
#[tokio::test]
async fn a_match_on_a_response_that_fails_is_neither_marked_nor_published() {
	let directory = tempfile::tempdir().expect("temporary directory");
	let directors = directors();
	let ctx = settings(&["ai_stream_rules_interrupt never"]);
	let (gate, dispatches) = observer();
	let path = directory.path().join("session.oms");
	let mut session = fresh_session(&path);
	DirectorStack::from_dom(session.dom(), &directors)
		.engage_registered(&mut session, FAMILY)
		.expect("the director engages");

	let mut failing = kernel(FailsAfterAMatch, directory.path(), &directors, &ctx, &gate);
	let error = failing
		.run_turn(&mut session, turn("first"), RunControl::default())
		.await
		.expect_err("the stream fails after the match");
	assert!(matches!(error, KernelError::Inference(_)), "{error:?}");
	assert!(
		triggered(&dispatches).is_empty(),
		"a response that never committed publishes no observation"
	);

	let (inference, _requests) = ScriptedInference::new([
		text_script("a danger answer"),
		text_script("a clean answer"),
		text_script("a clean answer"),
	]);
	let mut retry = kernel(inference, directory.path(), &directors, &ctx, &gate);
	retry
		.run_turn(&mut session, turn("retry"), RunControl::default())
		.await
		.expect("the retry completes");
	assert_eq!(
		triggered(&dispatches).len(),
		1,
		"the failed attempt left the rule unmarked, so the retry's match still fires"
	);
}

/// A match publishes its observation to subscribed extensions exactly once,
/// after the loop committed it: a response that mentions the word twice is one
/// match, and a `once` rule does not publish again on the next turn.
#[tokio::test]
async fn a_match_publishes_its_observation_exactly_once() {
	for (mode, interrupted) in [("always", true), ("never", false)] {
		let directory = tempfile::tempdir().expect("temporary directory");
		let directors = directors();
		let ctx = settings(&[&format!("ai_stream_rules_interrupt {mode}")]);
		let (gate, dispatches) = observer();
		let path = directory.path().join("session.oms");
		let mut session = fresh_session(&path);
		DirectorStack::from_dom(session.dom(), &directors)
			.engage_registered(&mut session, FAMILY)
			.expect("the director engages");

		let (inference, _requests) = ScriptedInference::new([
			text_script("danger here and danger there"),
			text_script("a clean answer"),
			text_script("a clean answer"),
		]);
		let mut first = kernel(inference, directory.path(), &directors, &ctx, &gate);
		first
			.run_turn(&mut session, turn("first"), RunControl::default())
			.await
			.expect("the turn completes");
		let observed = triggered(&dispatches);
		assert_eq!(observed.len(), 1, "{mode}: one match, one observation");
		assert_eq!(observed[0]["rule"], "guard", "{mode}");
		assert_eq!(observed[0]["interrupted"], interrupted, "{mode}");

		let (inference, _requests) = ScriptedInference::new([
			text_script("danger once more"),
			text_script("a clean answer"),
			text_script("a clean answer"),
		]);
		let mut second = kernel(inference, directory.path(), &directors, &ctx, &gate);
		second
			.run_turn(&mut session, turn("second"), RunControl::default())
			.await
			.expect("the second turn completes");
		assert!(
			triggered(&dispatches).is_empty(),
			"{mode}: a fired `once` rule publishes nothing more"
		);
	}
}

/// Rewinding past the redirect transaction removes the exclusion, the
/// injection and the fired mark together (ADR 0038 §4, 0004). The mark is
/// proved by behavior: a `once` rule that fired stays quiet without a rewind
/// (see `a_once_rule_stays_fired_after_the_session_resumes`), and fires again
/// after one.
#[tokio::test]
async fn rewinding_past_a_redirect_removes_its_exclusion_injection_and_mark() {
	let directory = tempfile::tempdir().expect("temporary directory");
	let directors = directors();
	let ctx = settings(&[]);
	let (gate, _dispatches) = observer();
	let path = directory.path().join("session.oms");
	let mut session = fresh_session(&path);
	DirectorStack::from_dom(session.dom(), &directors)
		.engage_registered(&mut session, FAMILY)
		.expect("the director engages");

	let (inference, _requests) =
		ScriptedInference::new([text_script("a danger answer"), text_script("a clean answer")]);
	let mut first = kernel(inference, directory.path(), &directors, &ctx, &gate);
	first
		.run_turn(&mut session, turn("first"), RunControl::default())
		.await
		.expect("the redirected turn completes");
	let interrupted = |session: &Session| {
		session
			.dom()
			.select(&format!("body turn assistant[interrupt={FAMILY}]"))
			.expect("selector")
			.count()
	};
	let injections = |session: &Session| {
		session
			.dom()
			.select("body turn developer")
			.expect("selector")
			.count()
	};
	assert_eq!(interrupted(&session), 1, "the redirect marks the interrupted assistant");
	assert!(injections(&session) >= 1, "the redirect journals its injection");

	// Control: the fired mark is what keeps the same text quiet until the
	// rewind removes it.
	let (inference, requests) = ScriptedInference::new([text_script("a danger answer again")]);
	let mut quiet = kernel(inference, directory.path(), &directors, &ctx, &gate);
	quiet
		.run_turn(&mut session, turn("second"), RunControl::default())
		.await
		.expect("the second turn completes");
	assert_eq!(requests.lock().len(), 1, "before the rewind the mark keeps the rule quiet");

	let user = journal_entries(&path)
		.into_iter()
		.find(|entry| entry.kind.name.as_str().starts_with("msg.user"))
		.expect("the turn's user message is journaled")
		.id;
	session.rewind(user).expect("rewinds past the redirect");
	assert_eq!(interrupted(&session), 0, "the exclusion is gone");
	assert_eq!(injections(&session), 0, "the injection is gone");

	let (inference, requests) =
		ScriptedInference::new([text_script("a danger answer"), text_script("a clean answer")]);
	let mut again = kernel(inference, directory.path(), &directors, &ctx, &gate);
	again
		.run_turn(&mut session, turn("again"), RunControl::default())
		.await
		.expect("the turn after the rewind completes");
	assert_eq!(
		requests.lock().len(),
		2,
		"the fired mark was removed with the redirect, so the same text redirects again"
	);
}

/// Registers the same rule with `interruptMode: never`: prose matches defer.
fn directors_never() -> DirectorRegistry {
	let rule = RulePattern {
		name:           Str::new_static("guard"),
		body:           Str::new_static("Avoid that word."),
		pattern:        Str::new_static("danger"),
		scope:          vec![Str::new_static("text")],
		globs:          Vec::new(),
		interrupt_mode: Some(Str::new_static("never")),
	};
	let set = StreamRuleSet::compile(vec![rule])
		.set
		.expect("the rule compiles");
	let mut directors = DirectorRegistry::standard();
	directors.register_extension(Box::new(StreamRules::new(Arc::new(set))));
	directors
}

/// Collects `(tag, content)` for every turn child in the journaled session.
fn node_texts(path: &Path) -> Vec<(omp_dom::Tag, Str)> {
	let session = Session::open(path, ComponentRegistry::standard()).expect("journal reopens");
	let dom = session.dom();
	dom.children(dom.body())
		.iter()
		.flat_map(|turn| dom.children(*turn).iter())
		.filter_map(|handle| {
			let node = dom.get(*handle)?;
			Some((node.tag.clone(), node.content.clone()?))
		})
		.collect()
}

/// ADR 0038 §3: a non-interrupting prose match defers its body. The yield
/// returns `Continue { reminder }`, so the loop journals the body as a
/// `<developer>` aside and resamples once; the cleared pending state keeps the
/// second yield quiet.
#[tokio::test]
async fn a_never_prose_match_defers_its_reminder_to_the_yield() {
	let directory = tempfile::tempdir().expect("temporary directory");
	let directors = directors_never();
	let ctx = settings(&[]);
	let path = directory.path().join("session.oms");
	let (inference, requests) =
		ScriptedInference::new([text_script("a danger answer"), text_script("a clean answer")]);
	let (gate, _dispatches) = observer();
	let mut kernel = kernel(inference, directory.path(), &directors, &ctx, &gate);
	let mut session = fresh_session(&path);
	DirectorStack::from_dom(session.dom(), &directors)
		.engage_registered(&mut session, FAMILY)
		.expect("the director engages");
	let outcome = kernel
		.run_turn(&mut session, turn("first"), RunControl::default())
		.await
		.expect("the deferred turn completes");
	assert_eq!(outcome.stop, TurnStop::Completed);
	// A non-interrupting match keeps the first assistant in context, so the
	// turn text carries both responses.
	assert_eq!(outcome.assistant_text, "a danger answera clean answer");
	assert_eq!(requests.lock().len(), 2, "the reminder continues the turn once");
	drop(session);
	let developers = node_texts(&path)
		.into_iter()
		.filter(|(tag, content)| {
			*tag == omp_dom::Tag::Known(omp_dom::KnownTag::Developer)
				&& content.as_str() == "Avoid that word."
		})
		.count();
	assert_eq!(developers, 1, "the deferred body lands exactly once");
}

/// ADR 0038 §4: a redirect notice names the match end offset and carries a
/// bounded excerpt recovered from the journaled stream text.
#[tokio::test]
async fn a_redirect_notice_carries_the_offset_and_journal_excerpt() {
	let directory = tempfile::tempdir().expect("temporary directory");
	let directors = directors();
	let ctx = settings(&[]);
	let path = first_turn_fires(directory.path(), &directors, &ctx).await;
	let notices = node_texts(&path)
		.into_iter()
		.filter(|(tag, _)| *tag == omp_dom::Tag::Known(omp_dom::KnownTag::Notice))
		.map(|(_, content)| content)
		.collect::<Vec<_>>();
	let notice = notices
		.iter()
		.find(|content| content.as_str().contains("Stream rule guard matched"))
		.expect("the redirect notice is journaled");
	assert!(notice.as_str().contains("at byte"), "offset named: {notice}");
	assert!(notice.as_str().contains("\nexcerpt: "), "excerpt attached: {notice}");
	assert!(notice.as_str().contains("danger"), "the excerpt window covers the match: {notice}");
}

/// ADR 0038 §6: a hidden context opens no stream watchers, by construction at
/// the stack walker rather than by hidden work merely not reaching the loop.
#[tokio::test]
async fn a_hidden_context_opens_no_stream_watchers() {
	let directory = tempfile::tempdir().expect("temporary directory");
	let directors = directors();
	let ctx = settings(&[]);
	let path = directory.path().join("session.oms");
	let mut session = fresh_session(&path);
	let mut stack = DirectorStack::from_dom(session.dom(), &directors);
	stack
		.engage_registered(&mut session, FAMILY)
		.expect("the director engages");
	let stack = DirectorStack::from_dom(session.dom(), &directors);
	let turn_handle = session.dom().body();
	let route = omp_agent::RouteFacts::default();
	let request = support::empty_request();
	let visible = omp_agent::DirectorCx::new(turn_handle, &route).with_con(Some(&ctx));
	assert_eq!(
		stack.watch_stream(session.dom(), &visible, &request).len(),
		1,
		"the engaged rule set watches a visible response"
	);
	let hidden = omp_agent::DirectorCx::new(turn_handle, &route)
		.with_con(Some(&ctx))
		.with_hidden(true);
	assert!(
		stack
			.watch_stream(session.dom(), &hidden, &request)
			.is_empty(),
		"hidden work is never watched"
	);
}
