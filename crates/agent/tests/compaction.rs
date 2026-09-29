//! Compaction director integration proofs over the journal-derived session
//! tree.

use std::{collections::VecDeque, path::Path, sync::Arc};

use bytes::Bytes;
use futures::stream;
use omp_agent::{
	AI_COMPACT_THRESHOLD, GateDecision, HookGate, HookPatch, HookPhase, LifecycleHooks, OnFailure,
	SourceRef, When,
	director::{BoxFut, Director, ErasedInference, MutDirectorCx, Prepared, RouteFacts},
	directors::compaction::CompactionDirector,
};
use omp_ai::{
	ChatEvent, ChatRequest, ChatStream, ContentPart, Message, NegotiationPolicy, Role,
	SafetySetting, Sampling, Setting,
	settings::{AI_COMPACTION_KEEP_RECENT_TOKENS, AI_COMPACTION_THRESHOLD_TOKENS},
};
use omp_con::Ctx;
use omp_core::{Str, sf};
use omp_dom::{KnownTag, NodeSpec, Op, PropId, Txn, Value};
use omp_journal::{
	Journal,
	blob::BlobStore,
	data::{Compaction, TurnReceipt},
	kind,
};
use omp_proto::{thread::v1 as thread, toolhost::v1::HookEventId};
use omp_session::{ComponentRegistry, Session, projection::project_thread};
use parking_lot::Mutex;

struct FakeInference {
	replies:  VecDeque<Str>,
	requests: Vec<ChatRequest>,
}

impl FakeInference {
	fn with_reply(reply: &str) -> Self {
		Self { replies: VecDeque::from([Str::new(reply)]), requests: Vec::new() }
	}
}

impl ErasedInference for FakeInference {
	fn execute<'a>(
		&'a mut self,
		request: ChatRequest,
	) -> BoxFut<'a, Result<ChatStream, omp_ai::Error>> {
		self.requests.push(request);
		let reply = self
			.replies
			.pop_front()
			.expect("one summary reply configured");
		Box::pin(async move {
			Ok(ChatStream::ordinary(Box::pin(stream::iter([Ok(ChatEvent::TextDelta {
				index: 0,
				text:  reply,
			})]))))
		})
	}
}

fn request(text: &str) -> ChatRequest {
	ChatRequest {
		messages:          Arc::from([Message {
			role:    Role::User,
			content: Arc::from([ContentPart::Text { text: Str::new(text), proof: None }]),
			name:    None,
		}]),
		tools:             Arc::from([]),
		hosted_tools:      Arc::from([]),
		tool_choice:       Setting::Unset,
		output:            Setting::Unset,
		reasoning:         Setting::Unset,
		verbosity:         Setting::Unset,
		cache_retention:   Setting::Unset,
		service_tier:      Setting::Unset,
		sampling:          Sampling::default(),
		max_output_tokens: None,
		top_logprobs:      None,
		safety:            Arc::<[SafetySetting]>::from([]),
		negotiation:       NegotiationPolicy::default(),
		forced_call:       None,
	}
}

fn route(context_window: u64) -> RouteFacts {
	RouteFacts { context_window, ..Default::default() }
}

fn open(directory: &Path) -> (Session, BlobStore) {
	let blobs = BlobStore::open(directory).expect("blob store");
	let session = Session::create(&directory.join("session.oms"), ComponentRegistry::standard())
		.expect("session");
	(session, blobs)
}

/// A control plane that keeps nothing verbatim, so a tiny test history cuts
/// before the current turn.
fn keep_nothing() -> Ctx {
	let con = Ctx::new();
	AI_COMPACTION_KEEP_RECENT_TOKENS
		.set(&con, 0)
		.expect("keep recent");
	con
}

fn turn_handle(session: &Session) -> omp_dom::Handle {
	*session
		.dom()
		.children(session.dom().body())
		.last()
		.expect("turn is materialized")
}

fn set_compact_threshold(session: &mut Session, threshold: f64) {
	let con = session
		.dom()
		.select("con")
		.expect("con selector")
		.next()
		.expect("con component");
	let after = session.dom().children(con).last().copied();
	session
		.patch(Txn {
			cause: session.head().expect("journal head"),
			label: Some(Str::new_static("test.compaction.threshold")),
			ops:   vec![Op::Ins {
				parent: con,
				after,
				node: NodeSpec::new(KnownTag::Var)
					.with_prop(PropId::Name, Value::Str(Str::new_static(AI_COMPACT_THRESHOLD.name())))
					.with_prop(PropId::Value, Value::Float(threshold)),
			}],
		})
		.expect("threshold patch");
}

fn projected_texts(session: &Session) -> Vec<String> {
	project_thread(session.dom())
		.into_iter()
		.filter_map(|item| match item.kind? {
			thread::item::Kind::Message(message) => {
				message.parts.into_iter().find_map(|part| match part.kind? {
					thread::part::Kind::Text(text) => Some(text),
					_ => None,
				})
			},
			_ => None,
		})
		.collect()
}

fn message_text(message: &Message) -> &str {
	message
		.content
		.iter()
		.find_map(|part| match part {
			ContentPart::Text { text, .. } => Some(text.as_str()),
			_ => None,
		})
		.unwrap_or_default()
}

fn subscription(id: u32, phase: HookPhase) -> omp_agent::hooks::Subscription {
	omp_agent::hooks::Subscription {
		host: sf!("test"),
		source: SourceRef {
			layer:        0,
			publisher:    sf!("test"),
			extension_id: sf!("compaction"),
		},
		id,
		event: HookEventId::HookEventCompaction,
		phase,
		order: 0,
		on_failure: OnFailure::Deny,
		when: When::default(),
	}
}

#[tokio::test]
async fn under_threshold_skips_compaction() {
	let directory = tempfile::tempdir().expect("temporary directory");
	let (mut session, blobs) = open(directory.path());
	session.begin_turn().expect("turn");
	session.user("short", Vec::new()).expect("user");
	let mut inference = FakeInference::with_reply("unused");
	let route = route(16_000);
	let turn = turn_handle(&session);
	let mut cx = MutDirectorCx {
		session: &mut session,
		inference: &mut inference,
		blobs: &blobs,
		route: &route,
		turn,
		director: None,
		events: None,
		con: None,
		hooks: None,
	};
	let prepared = CompactionDirector::new()
		.before_inference(&mut cx, &request("short"))
		.await
		.expect("preparation");
	assert_eq!(prepared, Prepared::Unchanged);
	assert_eq!(session.dom().count("compaction").expect("selector"), 0);
	assert!(inference.requests.is_empty());
}

#[tokio::test]
async fn dom_ai_compact_threshold_controls_compaction() {
	let directory = tempfile::tempdir().expect("temporary directory");
	let (mut session, blobs) = open(directory.path());
	session.begin_turn().expect("turn");
	session.user("oldest history", Vec::new()).expect("user");
	// Without a console, the default retains 20k recent tokens verbatim, so
	// the turn before the prompt must exceed that to hide anything older.
	let recent = "small but configured ".repeat(4_000);
	session.begin_turn().expect("turn");
	session.user(recent.clone(), Vec::new()).expect("user");
	session.begin_turn().expect("turn");
	session.user("prompt", Vec::new()).expect("user");
	set_compact_threshold(&mut session, 0.10);
	let mut inference = FakeInference::with_reply("threshold summary");
	let route = route(512);
	let turn = turn_handle(&session);
	let mut cx = MutDirectorCx {
		session: &mut session,
		inference: &mut inference,
		blobs: &blobs,
		route: &route,
		turn,
		director: None,
		events: None,
		con: None,
		hooks: None,
	};
	assert_eq!(
		CompactionDirector::new()
			.before_inference(&mut cx, &request(&recent))
			.await
			.expect("configured preparation"),
		Prepared::Rebuild
	);
	assert_eq!(session.dom().count("compaction").expect("selector"), 1);
	assert_eq!(projected_texts(&session), vec![
		"threshold summary".to_owned(),
		recent,
		"prompt".to_owned()
	]);
	let summarised = inference.requests[0]
		.messages
		.iter()
		.map(message_text)
		.collect::<Vec<_>>();
	assert_eq!(summarised[1..], ["oldest history"]);
}

#[tokio::test]
async fn automatic_compaction_keeps_the_triggering_prompt_after_the_summary() {
	let directory = tempfile::tempdir().expect("temporary directory");
	let (mut session, blobs) = open(directory.path());
	session.begin_turn().expect("history turn");
	let history = "history ".repeat(100);
	let boundary = session.user(history.clone(), Vec::new()).expect("history");
	let turn_id = session.begin_turn().expect("turn");
	session.user("what next?", Vec::new()).expect("prompt");
	let con = keep_nothing();
	let mut inference = FakeInference::with_reply("durable compacted context");
	let route = route(128);
	let turn = turn_handle(&session);
	let mut cx = MutDirectorCx {
		session: &mut session,
		inference: &mut inference,
		blobs: &blobs,
		route: &route,
		turn,
		director: None,
		events: None,
		con: Some(&con),
		hooks: None,
	};
	let prepared = CompactionDirector::new()
		.before_inference(&mut cx, &request(&history))
		.await
		.expect("preparation");
	assert_eq!(prepared, Prepared::Rebuild);
	let repeated = CompactionDirector::new()
		.before_inference(&mut cx, &request(&history))
		.await
		.expect("re-entrant preparation");
	assert_eq!(repeated, Prepared::Unchanged);
	assert_eq!(session.dom().count("compaction").expect("selector"), 1);
	assert_eq!(inference.requests.len(), 1);
	// The summariser saw only the hidden history, never the pending prompt.
	let summarised = inference.requests[0]
		.messages
		.iter()
		.map(message_text)
		.collect::<Vec<_>>();
	assert_eq!(summarised.len(), 2);
	assert_eq!(summarised[1], history);
	// The model sees the summary and then the prompt that triggered the turn.
	let live_projection = project_thread(session.dom());
	assert_eq!(projected_texts(&session), vec![
		"durable compacted context".to_owned(),
		"what next?".to_owned()
	]);
	let path = session.journal_path().to_path_buf();
	drop(session);

	let entries = Journal::scan(&path).expect("journal reopens");
	let compact_entries = entries
		.iter()
		.filter(|entry| entry.kind.name == kind::COMPACTION && entry.kind.rev == 1)
		.collect::<Vec<_>>();
	assert_eq!(compact_entries.len(), 1);
	assert_eq!(compact_entries[0].by, Some(turn_id));
	let payload: Compaction =
		serde_json::from_str(compact_entries[0].data.as_str()).expect("compaction payload");
	assert_eq!(payload.boundary, boundary);
	assert_eq!(payload.method.as_deref(), Some("auto"));
	assert_eq!(
		blobs.get(&payload.summary).expect("summary blob"),
		b"durable compacted context".as_slice()
	);

	let reopened = Session::open(&path, ComponentRegistry::standard()).expect("session replays");
	assert_eq!(project_thread(reopened.dom()), live_projection);
}

#[tokio::test]
async fn compaction_trigger_uses_receipted_context_tokens_and_threshold_tokens_precedence() {
	// 1000-token window: the fraction path would trigger at 80% of the
	// post-reserve window (680); an explicit `thresholdTokens` of 300 wins.
	let con = keep_nothing();
	AI_COMPACTION_THRESHOLD_TOKENS
		.set(&con, 300)
		.expect("threshold tokens");
	let route = route(1_000);

	// A receipt above the explicit threshold triggers even though the byte
	// estimate of the request is tiny.
	let directory = tempfile::tempdir().expect("temporary directory");
	let (mut session, blobs) = open(directory.path());
	session.begin_turn().expect("history turn");
	session.user("history", Vec::new()).expect("history");
	session
		.receipt(TurnReceipt {
			tokens_in: 200,
			tokens_out: 50,
			cache_read: 100,
			..Default::default()
		})
		.expect("receipt");
	session.begin_turn().expect("turn");
	session.user("tiny", Vec::new()).expect("prompt");
	let mut inference = FakeInference::with_reply("receipted summary");
	let turn = turn_handle(&session);
	let mut cx = MutDirectorCx {
		session: &mut session,
		inference: &mut inference,
		blobs: &blobs,
		route: &route,
		turn,
		director: None,
		events: None,
		con: Some(&con),
		hooks: None,
	};
	assert_eq!(
		CompactionDirector::new()
			.before_inference(&mut cx, &request("tiny"))
			.await
			.expect("receipted preparation"),
		Prepared::Rebuild
	);
	let entries = Journal::scan(session.journal_path()).expect("journal");
	let payload: Compaction = entries
		.iter()
		.find(|entry| entry.kind.name == kind::COMPACTION)
		.map(|entry| serde_json::from_str(entry.data.as_str()).expect("compaction payload"))
		.expect("compaction journaled");
	assert_eq!(payload.tokens_before, Some(350));

	// A receipt at or below the explicit threshold does not trigger, even
	// though the fraction path (ai_compact_threshold 0.1 → 68 tokens) would.
	AI_COMPACT_THRESHOLD.set(&con, 0.1).expect("fraction");
	let directory = tempfile::tempdir().expect("temporary directory");
	let (mut session, blobs) = open(directory.path());
	session.begin_turn().expect("history turn");
	session.user("history", Vec::new()).expect("history");
	session
		.receipt(TurnReceipt { tokens_in: 250, tokens_out: 50, ..Default::default() })
		.expect("receipt");
	session.begin_turn().expect("turn");
	session.user("tiny", Vec::new()).expect("prompt");
	let mut inference = FakeInference::with_reply("unused");
	let turn = turn_handle(&session);
	let mut cx = MutDirectorCx {
		session: &mut session,
		inference: &mut inference,
		blobs: &blobs,
		route: &route,
		turn,
		director: None,
		events: None,
		con: Some(&con),
		hooks: None,
	};
	assert_eq!(
		CompactionDirector::new()
			.before_inference(&mut cx, &request("tiny"))
			.await
			.expect("under-threshold preparation"),
		Prepared::Unchanged
	);
	assert!(inference.requests.is_empty());

	// Without any receipt the byte estimate of the request stands in.
	let directory = tempfile::tempdir().expect("temporary directory");
	let (mut session, blobs) = open(directory.path());
	session.begin_turn().expect("history turn");
	session.user("history", Vec::new()).expect("history");
	session.begin_turn().expect("turn");
	session.user("tiny", Vec::new()).expect("prompt");
	let mut inference = FakeInference::with_reply("estimated summary");
	let turn = turn_handle(&session);
	let mut cx = MutDirectorCx {
		session: &mut session,
		inference: &mut inference,
		blobs: &blobs,
		route: &route,
		turn,
		director: None,
		events: None,
		con: Some(&con),
		hooks: None,
	};
	assert_eq!(
		CompactionDirector::new()
			.before_inference(&mut cx, &request(&"estimate ".repeat(200)))
			.await
			.expect("estimated preparation"),
		Prepared::Rebuild
	);
	assert_eq!(session.dom().count("compaction").expect("selector"), 1);
}

#[tokio::test]
async fn compaction_hook_verdict_can_cancel_or_replace_the_summary() {
	let (gate, receiver) = HookGate::channel();
	let gate = Arc::new(gate);
	let observed = Arc::new(Mutex::new(Vec::new()));
	let replacement = Arc::new(Mutex::new(None::<Str>));
	let responder = {
		let gate = Arc::clone(&gate);
		let observed = Arc::clone(&observed);
		let replacement = Arc::clone(&replacement);
		tokio::spawn(async move {
			while let Ok(dispatch) = receiver.recv_async().await {
				let payload: serde_json::Value =
					serde_json::from_slice(&dispatch.payload).expect("hook payload");
				observed.lock().push((dispatch.event, payload.clone()));
				if dispatch.event != HookEventId::HookEventCompaction {
					continue;
				}
				let decision = match replacement.lock().clone() {
					None => GateDecision::Deny(sf!("extension keeps the context")),
					Some(summary) => {
						let mut transformed = payload;
						transformed["summary"] = serde_json::Value::String(summary.to_string());
						GateDecision::Modify(HookPatch {
							target: None,
							args:   Some(Bytes::from(
								serde_json::to_vec(&transformed).expect("transform"),
							)),
						})
					},
				};
				gate
					.answer(dispatch.dispatch_id, vec![(1, decision)])
					.expect("hook answer");
			}
		})
	};
	let hooks = LifecycleHooks::new(Arc::clone(&gate));
	let con = keep_nothing();
	let route = route(128);
	let history = "history ".repeat(100);

	// A denial cancels this run: nothing is summarised or journaled.
	gate
		.subscribe("test", [subscription(1, HookPhase::Precheck)])
		.expect("precheck subscription");
	let directory = tempfile::tempdir().expect("temporary directory");
	let (mut session, blobs) = open(directory.path());
	session.begin_turn().expect("history turn");
	session.user(history.clone(), Vec::new()).expect("history");
	session.begin_turn().expect("turn");
	session.user("what next?", Vec::new()).expect("prompt");
	let mut inference = FakeInference::with_reply("unused");
	let turn = turn_handle(&session);
	let mut cx = MutDirectorCx {
		session: &mut session,
		inference: &mut inference,
		blobs: &blobs,
		route: &route,
		turn,
		director: None,
		events: None,
		con: Some(&con),
		hooks: Some(&hooks),
	};
	assert_eq!(
		CompactionDirector::new()
			.before_inference(&mut cx, &request(&history))
			.await
			.expect("cancelled preparation"),
		Prepared::Unchanged
	);
	assert!(inference.requests.is_empty());
	assert_eq!(session.dom().count("compaction").expect("selector"), 0);
	let event = observed
		.lock()
		.iter()
		.find(|(event, _)| *event == HookEventId::HookEventCompaction)
		.map(|(_, payload)| payload.clone())
		.expect("compaction gate payload");
	for key in [
		"preparation_id",
		"tier",
		"reason",
		"epoch",
		"tokens_before",
		"target_tokens",
		"suggested_first_kept",
		"to_summarize",
		"to_retain",
		"split_turn",
		"previous_summary",
		"previous_preserve",
		"custom_instructions",
		"deadline",
	] {
		assert!(event.get(key).is_some(), "missing CompactionEvent key {key}");
	}
	assert_eq!(event["reason"], "threshold");
	assert_eq!(event["to_summarize"].as_array().map(Vec::len), Some(1));
	assert_eq!(event["to_summarize"][0]["role"], "user");
	assert_eq!(event["to_retain"].as_array().map(Vec::len), Some(1));
	assert_eq!(event["to_retain"][0]["preview"], "what next?");

	// A transform supplying `summary` replaces the summariser call.
	*replacement.lock() = Some(sf!("extension-authored summary"));
	gate
		.subscribe("test", [subscription(1, HookPhase::Transform)])
		.expect("transform subscription");
	observed.lock().clear();
	let mut inference = FakeInference::with_reply("unused");
	let turn = turn_handle(&session);
	let mut cx = MutDirectorCx {
		session: &mut session,
		inference: &mut inference,
		blobs: &blobs,
		route: &route,
		turn,
		director: None,
		events: None,
		con: Some(&con),
		hooks: Some(&hooks),
	};
	assert_eq!(
		CompactionDirector::new()
			.before_inference(&mut cx, &request(&history))
			.await
			.expect("replaced preparation"),
		Prepared::Rebuild
	);
	drop(cx);
	assert!(inference.requests.is_empty(), "the extension summary skips the summariser");
	assert_eq!(projected_texts(&session), vec![
		"extension-authored summary".to_owned(),
		"what next?".to_owned()
	]);
	responder.abort();
}

#[tokio::test]
async fn compaction_done_reports_the_outcome_to_observers() {
	let (gate, receiver) = HookGate::channel();
	let gate = Arc::new(gate);
	gate
		.subscribe("test", [omp_agent::hooks::Subscription {
			event: HookEventId::HookEventCompactionDone,
			..subscription(1, HookPhase::Observe)
		}])
		.expect("observer subscription");
	let hooks = LifecycleHooks::new(Arc::clone(&gate));
	let con = keep_nothing();
	let route = route(128);
	let history = "history ".repeat(100);
	let directory = tempfile::tempdir().expect("temporary directory");
	let (mut session, blobs) = open(directory.path());
	session.begin_turn().expect("history turn");
	let boundary = session.user(history.clone(), Vec::new()).expect("history");
	let turn_id = session.begin_turn().expect("turn");
	session.user("what next?", Vec::new()).expect("prompt");
	let mut inference = FakeInference::with_reply("observed summary");
	let turn = turn_handle(&session);
	let mut cx = MutDirectorCx {
		session: &mut session,
		inference: &mut inference,
		blobs: &blobs,
		route: &route,
		turn,
		director: None,
		events: None,
		con: Some(&con),
		hooks: Some(&hooks),
	};
	assert_eq!(
		CompactionDirector::new()
			.before_inference(&mut cx, &request(&history))
			.await
			.expect("preparation"),
		Prepared::Rebuild
	);
	let dispatch = receiver.try_recv().expect("compaction_done observation");
	assert_eq!(dispatch.event, HookEventId::HookEventCompactionDone);
	let outcome: serde_json::Value = serde_json::from_slice(&dispatch.payload).expect("payload");
	for key in [
		"preparation_id",
		"tiers_run",
		"from_extension",
		"tokens_before",
		"tokens_after",
		"first_kept_id",
		"epoch",
		"summary_bytes",
		"warning",
	] {
		assert!(outcome.get(key).is_some(), "missing CompactionOutcome key {key}");
	}
	assert_eq!(outcome["preparation_id"], boundary.to_string());
	assert_eq!(outcome["first_kept_id"], turn_id.to_string());
	assert_eq!(outcome["tiers_run"], serde_json::json!(["local"]));
	assert_eq!(outcome["from_extension"], serde_json::Value::Null);
	assert_eq!(outcome["summary_bytes"], "observed summary".len());
	assert_eq!(outcome["epoch"], 0);
	// What plugin `PostCompact` hooks read: the trigger and the summary.
	assert_eq!(outcome["reason"], "threshold");
	assert_eq!(outcome["summary"], "observed summary");
}

#[tokio::test]
async fn manual_compaction_carries_focus_and_ignores_threshold() {
	let directory = tempfile::tempdir().expect("temporary directory");
	let (mut session, blobs) = open(directory.path());
	session.begin_turn().expect("turn");
	session.user("small history", Vec::new()).expect("user");
	session.begin_turn().expect("turn");
	session.user("later", Vec::new()).expect("user");
	let con = keep_nothing();
	let mut inference = FakeInference::with_reply("focused context");
	let route = route(1_000_000);
	let turn = turn_handle(&session);
	let mut cx = MutDirectorCx {
		session: &mut session,
		inference: &mut inference,
		blobs: &blobs,
		route: &route,
		turn,
		director: None,
		events: None,
		con: Some(&con),
		hooks: None,
	};
	let prepared = CompactionDirector::manual(Some(Str::new_static("database migration")))
		.with_method("handoff")
		.before_inference(&mut cx, &request("small history"))
		.await
		.expect("manual compaction");
	assert_eq!(prepared, Prepared::Rebuild);
	assert_eq!(session.dom().count("compaction").expect("selector"), 1);
	// Between turns nothing is pending, so a manual run hides the whole
	// (tiny) history: only the summary remains.
	assert_eq!(projected_texts(&session), vec!["focused context".to_owned()]);
	let summary_request = inference.requests.first().expect("summary request");
	assert!(summary_request.tools.is_empty());
	assert!(summary_request.hosted_tools.is_empty());
	// The summariser ships no tools, so it must state no tool-choice intent at
	// all: any Setting other than `Unset` emits a `chat.tools.choice`
	// requirement, and a route carrying no tool-choice evidence then fails
	// compaction closed — on exactly the long-context sessions it exists to
	// serve.
	assert!(
		matches!(summary_request.tool_choice, omp_ai::Setting::Unset),
		"summary tool_choice must be unset, got {:?}",
		summary_request.tool_choice,
	);
	assert!(message_text(&summary_request.messages[0]).contains("database migration"));
	assert_eq!(
		summary_request.messages[1..]
			.iter()
			.map(message_text)
			.collect::<Vec<_>>(),
		["small history", "later"]
	);
}

/// A route that reads images, with a window large enough that only a manual
/// or explicitly receipted run compacts.
fn vision_route(context_window: u64) -> RouteFacts {
	RouteFacts { context_window, image_input: true, ..Default::default() }
}

/// An inference client that must never be called: the snapcompact path
/// renders locally.
const fn no_inference() -> FakeInference {
	FakeInference { replies: VecDeque::new(), requests: Vec::new() }
}

fn newest_compaction(session: &Session) -> &omp_dom::Node {
	let dom = session.dom();
	dom.children(dom.meta())
		.iter()
		.filter_map(|handle| dom.get(*handle))
		.rfind(|node| node.tag.as_str() == "compaction")
		.expect("compaction marker")
}

fn compaction_method(session: &Session) -> Option<&str> {
	newest_compaction(session)
		.prop(&PropId::Method.into())
		.and_then(Value::as_str)
}

/// Every `<notice name=snapcompact>` body, oldest first.
fn snapcompact_notices(session: &Session) -> Vec<String> {
	let dom = session.dom();
	dom.children(dom.body())
		.iter()
		.flat_map(|turn| dom.children(*turn).iter())
		.filter_map(|handle| dom.get(*handle))
		.filter(|node| {
			node.tag == omp_dom::Tag::Known(KnownTag::Notice)
				&& node
					.prop(&omp_dom::PropKey::Custom(Str::new_static("name")))
					.and_then(Value::as_str)
					== Some("snapcompact")
		})
		.map(|node| node.content.as_deref().unwrap_or_default().to_owned())
		.collect()
}

/// One history turn followed by a receipt large enough that imaging the
/// hidden run clears the renderer's savings margin.
fn archived_history(session: &mut Session) {
	session.begin_turn().expect("history turn");
	session
		.user("Refactor the tokenizer so\n\n  every keyword is interned.", Vec::new())
		.expect("history");
	session
		.receipt(TurnReceipt { tokens_in: 400_000, ..Default::default() })
		.expect("receipt");
}

/// A manual snapcompact that keeps nothing verbatim (between turns the whole
/// history since the previous marker is hidden).
async fn snapcompact_manually(
	session: &mut Session,
	blobs: &BlobStore,
	route: &RouteFacts,
	inference: &mut FakeInference,
) -> Prepared {
	let con = keep_nothing();
	let turn = turn_handle(session);
	let mut cx = MutDirectorCx {
		session,
		inference,
		blobs,
		route,
		turn,
		director: None,
		events: None,
		con: Some(&con),
		hooks: None,
	};
	CompactionDirector::manual(None)
		.with_strategy(omp_agent::CompactionStrategy::Snapcompact)
		.before_inference(&mut cx, &request("Refactor the tokenizer"))
		.await
		.expect("snapcompact preparation")
}

fn frame_hashes(session: &Session) -> Vec<omp_core::Hash32> {
	omp_session::compaction_frames(newest_compaction(session))
		.into_iter()
		.map(|frame| frame.blob.hash)
		.collect()
}

#[tokio::test]
async fn snapcompact_journals_png_frames_that_the_prompt_inlines_after_the_note() {
	let directory = tempfile::tempdir().expect("temporary directory");
	let (mut session, blobs) = open(directory.path());
	archived_history(&mut session);
	let mut inference = no_inference();
	let prepared =
		snapcompact_manually(&mut session, &blobs, &vision_route(1_000_000), &mut inference).await;
	assert_eq!(prepared, Prepared::Rebuild);
	assert!(inference.requests.is_empty(), "snapcompact renders locally");
	assert_eq!(compaction_method(&session), Some("snapcompact"));
	assert!(snapcompact_notices(&session).is_empty(), "no fallback notice");

	// The journal entry carries the ordered frame refs; the CAS holds PNGs.
	let entries = Journal::scan(session.journal_path()).expect("journal");
	let payload: Compaction = entries
		.iter()
		.find(|entry| entry.kind.name == kind::COMPACTION)
		.map(|entry| serde_json::from_str(entry.data.as_str()).expect("compaction payload"))
		.expect("compaction journaled");
	assert_eq!(payload.method.as_deref(), Some("snapcompact"));
	assert_eq!(payload.frame_count(), 1, "a short history fits one frame");
	assert!(payload.tokens_after < payload.tokens_before);
	let frame = &payload.frames[0];
	assert_eq!(frame.mime.as_str(), "image/png");
	let png = session
		.blobs()
		.get(&frame.blob)
		.expect("frame retained in the session CAS");
	assert!(png.starts_with(b"\x89PNG\r\n\x1a\n"));

	// The prompt projection leads with the note and inlines the frame.
	let items = omp_agent::project_thread_with_attachments(session.dom(), session.blobs())
		.expect("frames resolve");
	let Some(thread::item::Kind::Message(message)) = items[0].kind.as_ref() else {
		panic!("the compaction projects as a message");
	};
	assert_eq!(message.synthetic, Some(true));
	assert_eq!(message.parts.len(), 2);
	let Some(thread::part::Kind::Text(note)) = message.parts[0].kind.as_ref() else {
		panic!("the note leads");
	};
	assert!(note.starts_with("The earlier conversation is archived verbatim"));
	let Some(thread::part::Kind::Blob(blob)) = message.parts[1].kind.as_ref() else {
		panic!("the frame follows the note");
	};
	assert_eq!(blob.mime, "image/png");
	assert_eq!(blob.inline.as_ref(), png.as_ref());
	assert_eq!(items.len(), 1, "a manual run between turns hides the whole history");
}

#[tokio::test]
async fn snapcompact_replays_and_rerenders_deterministically() {
	let first_directory = tempfile::tempdir().expect("temporary directory");
	let (mut first, first_blobs) = open(first_directory.path());
	archived_history(&mut first);
	let route = vision_route(1_000_000);
	snapcompact_manually(&mut first, &first_blobs, &route, &mut no_inference()).await;
	let projected = project_thread(first.dom());
	let hashes = frame_hashes(&first);
	let path = first.journal_path().to_path_buf();
	drop(first);

	// Replay folds the same marker and projection from the journal alone.
	let replayed = Session::open(&path, ComponentRegistry::standard()).expect("replay");
	assert_eq!(project_thread(replayed.dom()), projected);
	assert_eq!(frame_hashes(&replayed), hashes);

	// The same history renders byte-identical frames in another session.
	let second_directory = tempfile::tempdir().expect("temporary directory");
	let (mut second, second_blobs) = open(second_directory.path());
	archived_history(&mut second);
	snapcompact_manually(&mut second, &second_blobs, &route, &mut no_inference()).await;
	assert_eq!(frame_hashes(&second), hashes);
}

#[tokio::test]
async fn snapcompact_on_a_model_without_image_input_falls_back_to_soft_with_a_notice() {
	let directory = tempfile::tempdir().expect("temporary directory");
	let (mut session, blobs) = open(directory.path());
	archived_history(&mut session);
	let mut inference = FakeInference::with_reply("soft fallback summary");
	let prepared = snapcompact_manually(
		&mut session,
		&blobs,
		&RouteFacts { context_window: 1_000_000, ..Default::default() },
		&mut inference,
	)
	.await;
	assert_eq!(prepared, Prepared::Rebuild, "the fallback never fails the run");
	assert_eq!(inference.requests.len(), 1, "the soft summariser ran");
	assert_eq!(compaction_method(&session), Some("manual"));
	assert!(omp_session::compaction_frames(newest_compaction(&session)).is_empty());
	assert_eq!(projected_texts(&session), vec!["soft fallback summary".to_owned()]);
	assert_eq!(snapcompact_notices(&session), vec![
		omp_agent::directors::snapcompact::Fallback::NoImageInput
			.notice()
			.to_owned()
	]);
}

#[tokio::test]
async fn snapcompact_over_the_provider_frame_budget_falls_back_with_a_notice() {
	let directory = tempfile::tempdir().expect("temporary directory");
	let (mut session, blobs) = open(directory.path());
	session.begin_turn().expect("history turn");
	// An unknown provider admits five frames; this history needs more.
	session
		.user("x".repeat(120_000), Vec::new())
		.expect("history");
	session
		.receipt(TurnReceipt { tokens_in: 4_000_000, ..Default::default() })
		.expect("receipt");
	let mut inference = FakeInference::with_reply("budget fallback summary");
	snapcompact_manually(&mut session, &blobs, &vision_route(1_000_000), &mut inference).await;
	assert_eq!(compaction_method(&session), Some("manual"));
	assert_eq!(snapcompact_notices(&session), vec![
		omp_agent::directors::snapcompact::Fallback::FrameBudget
			.notice()
			.to_owned()
	]);
}

#[tokio::test]
async fn snapcompact_without_measured_savings_falls_back_with_a_notice() {
	let directory = tempfile::tempdir().expect("temporary directory");
	let (mut session, blobs) = open(directory.path());
	session.begin_turn().expect("history turn");
	// No receipt: the byte estimate of a short history is far below one
	// frame's image cost, so imaging would not save tokens.
	session.user("short history", Vec::new()).expect("history");
	let mut inference = FakeInference::with_reply("savings fallback summary");
	snapcompact_manually(&mut session, &blobs, &vision_route(1_000_000), &mut inference).await;
	assert_eq!(compaction_method(&session), Some("manual"));
	assert_eq!(snapcompact_notices(&session), vec![
		omp_agent::directors::snapcompact::Fallback::NoSavings
			.notice()
			.to_owned()
	]);
}

#[tokio::test]
async fn ai_compaction_strategy_selects_snapcompact_for_automatic_compaction() {
	let directory = tempfile::tempdir().expect("temporary directory");
	let (mut session, blobs) = open(directory.path());
	archived_history(&mut session);
	session.begin_turn().expect("turn");
	session.user("what next?", Vec::new()).expect("prompt");
	let con = keep_nothing();
	omp_agent::AI_COMPACTION_STRATEGY
		.set(&con, omp_agent::CompactionStrategy::Snapcompact)
		.expect("strategy");
	let mut inference = no_inference();
	let route = vision_route(128);
	let turn = turn_handle(&session);
	let mut cx = MutDirectorCx {
		session: &mut session,
		inference: &mut inference,
		blobs: &blobs,
		route: &route,
		turn,
		director: None,
		events: None,
		con: Some(&con),
		hooks: None,
	};
	assert_eq!(
		CompactionDirector::new()
			.before_inference(&mut cx, &request("Refactor the tokenizer"))
			.await
			.expect("automatic snapcompact"),
		Prepared::Rebuild
	);
	assert!(inference.requests.is_empty());
	assert_eq!(compaction_method(&session), Some("snapcompact"));
	// The triggering prompt stays verbatim after the archived history.
	let texts = projected_texts(&session);
	assert_eq!(texts.len(), 2);
	assert!(texts[0].starts_with("The earlier conversation is archived verbatim"));
	assert_eq!(texts[1], "what next?");
}

#[tokio::test]
async fn journaled_ai_compaction_strategy_is_honored_without_a_console() {
	let directory = tempfile::tempdir().expect("temporary directory");
	let (mut session, blobs) = open(directory.path());
	session.begin_turn().expect("hidden turn");
	session
		.user("Refactor the tokenizer so every keyword is interned.", Vec::new())
		.expect("history");
	// Without a console 20k recent tokens stay verbatim, so this turn is
	// kept and only the one before it is archived.
	let recent = "small but configured ".repeat(4_000);
	session.begin_turn().expect("recent turn");
	session.user(recent.clone(), Vec::new()).expect("recent");
	session
		.receipt(TurnReceipt { tokens_in: 400_000, ..Default::default() })
		.expect("receipt");
	let con = session
		.dom()
		.select("con")
		.expect("con selector")
		.next()
		.expect("con component");
	let after = session.dom().children(con).last().copied();
	session
		.patch(Txn {
			cause: session.head().expect("journal head"),
			label: Some(Str::new_static("test.compaction.strategy")),
			ops:   vec![Op::Ins {
				parent: con,
				after,
				node: NodeSpec::new(KnownTag::Var)
					.with_prop(
						PropId::Name,
						Value::Str(Str::new_static(omp_agent::AI_COMPACTION_STRATEGY.name())),
					)
					.with_prop(PropId::Value, Value::Str(Str::new_static("snapcompact"))),
			}],
		})
		.expect("strategy patch");
	let mut inference = no_inference();
	let route = vision_route(1_000_000);
	let turn = turn_handle(&session);
	let mut cx = MutDirectorCx {
		session: &mut session,
		inference: &mut inference,
		blobs: &blobs,
		route: &route,
		turn,
		director: None,
		events: None,
		con: None,
		hooks: None,
	};
	assert_eq!(
		CompactionDirector::manual(None)
			.before_inference(&mut cx, &request("Refactor the tokenizer"))
			.await
			.expect("journaled snapcompact"),
		Prepared::Rebuild
	);
	assert!(inference.requests.is_empty());
	assert_eq!(compaction_method(&session), Some("snapcompact"));
	let texts = projected_texts(&session);
	assert!(texts[0].starts_with("The earlier conversation is archived verbatim"));
	assert_eq!(texts[1..], [recent]);
}

#[tokio::test]
async fn a_pinned_soft_strategy_wins_and_its_summary_stays_text_ahead_of_a_later_archive() {
	let directory = tempfile::tempdir().expect("temporary directory");
	let (mut session, blobs) = open(directory.path());
	archived_history(&mut session);
	let con = keep_nothing();
	omp_agent::AI_COMPACTION_STRATEGY
		.set(&con, omp_agent::CompactionStrategy::Snapcompact)
		.expect("strategy");
	let route = vision_route(1_000_000);
	// `/compact soft` and `/handoff` pin `soft` over the convar.
	let mut soft = FakeInference::with_reply("pinned soft summary");
	{
		let turn = turn_handle(&session);
		let mut cx = MutDirectorCx {
			session: &mut session,
			inference: &mut soft,
			blobs: &blobs,
			route: &route,
			turn,
			director: None,
			events: None,
			con: Some(&con),
			hooks: None,
		};
		CompactionDirector::manual(None)
			.with_strategy(omp_agent::CompactionStrategy::Soft)
			.before_inference(&mut cx, &request("Refactor the tokenizer"))
			.await
			.expect("pinned soft");
	}
	assert_eq!(soft.requests.len(), 1);
	assert_eq!(compaction_method(&session), Some("manual"));

	session.begin_turn().expect("next turn");
	session
		.user("Now intern the operators as well.", Vec::new())
		.expect("history");
	session
		.receipt(TurnReceipt { tokens_in: 400_000, ..Default::default() })
		.expect("receipt");
	snapcompact_manually(&mut session, &blobs, &route, &mut no_inference()).await;
	assert_eq!(compaction_method(&session), Some("snapcompact"));
	assert!(
		projected_texts(&session)[0]
			.starts_with("pinned soft summary\n\nThe earlier conversation is archived verbatim")
	);
}

#[tokio::test]
async fn chained_snapcompact_carries_earlier_frames_and_a_later_soft_run_reads_them() {
	let directory = tempfile::tempdir().expect("temporary directory");
	let (mut session, blobs) = open(directory.path());
	archived_history(&mut session);
	let route = vision_route(1_000_000);
	snapcompact_manually(&mut session, &blobs, &route, &mut no_inference()).await;
	let first = frame_hashes(&session);
	let first_note = projected_texts(&session)[0].clone();

	session.begin_turn().expect("next turn");
	session
		.user("Now intern the operators as well.", Vec::new())
		.expect("history");
	session
		.receipt(TurnReceipt { tokens_in: 400_000, ..Default::default() })
		.expect("receipt");
	snapcompact_manually(&mut session, &blobs, &route, &mut no_inference()).await;
	let chained = frame_hashes(&session);
	assert_eq!(chained.len(), 2, "the new frame follows the carried one");
	assert_eq!(chained[0], first[0]);
	assert_eq!(projected_texts(&session)[0], first_note, "the note is not repeated");

	// A soft run after an archive hands the frames to the summariser.
	session.begin_turn().expect("soft turn");
	session
		.user("And the literals.", Vec::new())
		.expect("history");
	let frames = omp_agent::project_thread_with_attachments(session.dom(), session.blobs())
		.expect("frames resolve");
	let request = ChatRequest {
		messages: Message::from_thread_items(&frames).expect("request").into(),
		..request("unused")
	};
	let mut inference = FakeInference::with_reply("soft after archive");
	let con = keep_nothing();
	let turn = turn_handle(&session);
	let mut cx = MutDirectorCx {
		session: &mut session,
		inference: &mut inference,
		blobs: &blobs,
		route: &route,
		turn,
		director: None,
		events: None,
		con: Some(&con),
		hooks: None,
	};
	CompactionDirector::manual(None)
		.with_strategy(omp_agent::CompactionStrategy::Soft)
		.before_inference(&mut cx, &request)
		.await
		.expect("soft after archive");
	let previous = &inference.requests[0].messages[1];
	assert_eq!(
		previous
			.content
			.iter()
			.filter(|part| matches!(part, ContentPart::Image(_)))
			.count(),
		2,
		"the summariser reads both archive frames"
	);
	assert!(omp_session::compaction_frames(newest_compaction(&session)).is_empty());
}
