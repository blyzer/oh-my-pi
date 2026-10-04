//! Loop-level stream redirects (ADR 0038 §1): a scripted Director's watcher
//! interrupts a streamed response, the loop journals one redirect
//! transaction and resamples in the same turn, the per-turn cap downgrades
//! further interrupts to notes, observations reach lifecycle hooks only after
//! commit, and replay reproduces the live tree.

use std::sync::Arc;

use omp_agent::{
	Director, DirectorCx, DirectorRegistry, DirectorStack, DispatchPolicy, HookGate, HookPhase,
	Kernel, KernelEvent, OnFailure, PartialOutput, RunControl, SourceRef, StaticPrompt,
	StreamEffect, StreamFragment, StreamInterrupt, StreamObservation, StreamVerdict, StreamWatch,
	TurnInput, TurnStop, When,
};
use omp_ai::{ChatRequest, ContentPart, Role};
use omp_core::{Str, sf};
use omp_dom::{PropKey, Value};
use omp_journal::blob::BlobStore;
use omp_proto::toolhost::v1::HookEventId;

mod support;

use support::{ScriptedInference, fresh_session, journal_entries, registry, text_script};

const FAMILY: &str = "test-watch";
const INJECTION: &str = "Rewrite without the forbidden word.";

/// Interrupts every response whose visible text contains `forbidden`.
struct ForbiddenWord;

impl Director for ForbiddenWord {
	fn id(&self) -> &str {
		FAMILY
	}

	fn watch_stream(&self, _: &DirectorCx<'_>, _: &ChatRequest) -> Option<Box<dyn StreamWatch>> {
		Some(Box::new(Watch))
	}
}

struct Watch;

impl StreamWatch for Watch {
	fn fragment(&mut self, fragment: StreamFragment<'_>) -> StreamVerdict {
		if !fragment
			.bytes
			.windows(9)
			.any(|window| window == b"forbidden")
		{
			return StreamVerdict::Pass;
		}
		let mut payload = serde_json::Map::new();
		payload.insert("rule".to_owned(), serde_json::Value::from("forbidden-word"));
		StreamVerdict::Interrupt(StreamInterrupt {
			culprit: None,
			partial: PartialOutput::Discard,
			label:   Str::new_static("stream rule forbidden-word"),
			effect:  StreamEffect {
				developer: Some(Str::new_static(INJECTION)),
				notice: Some(Str::new_static("forbidden word redirected")),
				notice_name: Some(Str::new_static("stream-rule")),
				observation: Some(StreamObservation {
					event: HookEventId::HookEventStreamRuleTriggered,
					payload,
				}),
				..StreamEffect::default()
			},
		})
	}
}

fn subscription() -> omp_agent::hooks::Subscription {
	omp_agent::hooks::Subscription {
		host:       sf!("test"),
		source:     SourceRef {
			layer:        0,
			publisher:    sf!("test"),
			extension_id: sf!("stream-watch"),
		},
		id:         1,
		event:      HookEventId::HookEventStreamRuleTriggered,
		phase:      HookPhase::Observe,
		order:      0,
		on_failure: OnFailure::Defer,
		when:       When::default(),
	}
}

fn system_texts(request: &ChatRequest) -> Vec<&str> {
	request
		.messages
		.iter()
		.filter(|message| message.role == Role::System)
		.flat_map(|message| message.content.iter())
		.filter_map(|part| match part {
			ContentPart::Text { text, .. } => Some(text.as_str()),
			_ => None,
		})
		.collect()
}

fn assistant_texts(request: &ChatRequest) -> Vec<&str> {
	request
		.messages
		.iter()
		.filter(|message| message.role == Role::Assistant)
		.flat_map(|message| message.content.iter())
		.filter_map(|part| match part {
			ContentPart::Text { text, .. } => Some(text.as_str()),
			_ => None,
		})
		.collect()
}

fn prop<'a>(node: &'a omp_dom::Node, name: &'static str) -> Option<&'a str> {
	node
		.prop(&PropKey::Custom(Str::new_static(name)))
		.and_then(Value::as_str)
}

#[tokio::test]
async fn a_watcher_interrupt_redirects_resamples_and_replays() {
	let directory = tempfile::tempdir().expect("temporary directory");
	let journal_path = directory.path().join("redirect.oms");
	let (inference, requests) =
		ScriptedInference::new([text_script("a forbidden answer"), text_script("a clean answer")]);
	let mut directors = DirectorRegistry::standard();
	directors.register_extension(Box::new(ForbiddenWord));
	let (gate, dispatches) = HookGate::channel();
	let gate = Arc::new(gate);
	gate
		.subscribe("test", [subscription()])
		.expect("observer subscription");
	let mut kernel = Kernel::new(
		inference,
		registry(std::iter::empty()),
		DispatchPolicy::new(BlobStore::open(directory.path().join("blobs")).expect("blobs")),
		StaticPrompt(Str::new_static("test system")),
	)
	.with_director_registry(directors.clone())
	.with_hook_gate(Arc::clone(&gate));
	let events = kernel.subscribe();
	let mut session = fresh_session(&journal_path);
	DirectorStack::from_dom(session.dom(), &directors)
		.engage_registered(&mut session, FAMILY)
		.expect("watcher engages");

	let outcome = kernel
		.run_turn(
			&mut session,
			TurnInput { text: Str::new_static("answer"), attachments: Vec::new() },
			RunControl::default(),
		)
		.await
		.expect("redirected turn completes");

	assert_eq!(outcome.stop, TurnStop::Completed);
	assert_eq!(outcome.assistant_text, "a clean answer");
	{
		let requests = requests.lock();
		assert_eq!(requests.len(), 2, "the redirect resamples within the same turn");
		assert_eq!(
			system_texts(&requests[1]).last().copied(),
			Some(INJECTION),
			"the injection is the resample's trailing developer item"
		);
		assert!(
			!assistant_texts(&requests[1])
				.iter()
				.any(|text| text.contains("forbidden")),
			"a discarded partial response is excluded from the resample"
		);
	}

	let assistants = session
		.dom()
		.select("body turn assistant")
		.expect("selector")
		.collect::<Vec<_>>();
	assert_eq!(assistants.len(), 2);
	let interrupted = session.dom().get(assistants[0]).expect("first assistant");
	assert_eq!(prop(interrupted, "interrupt"), Some(FAMILY));
	assert_eq!(prop(interrupted, "context"), Some("excluded"));
	let notice = session
		.dom()
		.select("body turn notice")
		.expect("selector")
		.filter_map(|handle| session.dom().get(handle))
		.find(|node| prop(node, "name") == Some("stream-rule"))
		.expect("the redirect journals its notice");
	assert_eq!(notice.content.as_deref(), Some("forbidden word redirected"));
	assert!(
		journal_entries(&journal_path)
			.iter()
			.any(|entry| entry.label.as_deref() == Some("director.stream-interrupt"))
	);

	let kernel_events = events.try_iter().collect::<Vec<_>>();
	assert!(kernel_events.contains(&KernelEvent::StreamRedirected {
		director: Str::new_static(FAMILY),
		label:    Str::new_static("stream rule forbidden-word"),
		reason:   Some(Str::new_static("forbidden word redirected")),
	}));
	let observed = kernel_events
		.iter()
		.find_map(|event| match event {
			KernelEvent::StreamObserved { director, event, payload } => {
				Some((director.clone(), *event, payload.clone()))
			},
			_ => None,
		})
		.expect("hosts see the committed observation");
	assert_eq!(observed.0.as_str(), FAMILY);
	assert_eq!(observed.1, HookEventId::HookEventStreamRuleTriggered);

	let dispatch = dispatches
		.try_recv()
		.expect("extensions see the committed observation");
	assert_eq!(dispatch.event, HookEventId::HookEventStreamRuleTriggered);
	let payload: serde_json::Value =
		serde_json::from_slice(&dispatch.payload).expect("JSON payload");
	assert_eq!(payload["rule"], "forbidden-word");
	assert_eq!(payload["interrupted"], true, "the loop stamps the redirect outcome");
	assert!(payload["session_id"].is_string() && payload["turn_id"].is_string());
	assert!(
		payload["sequence"]
			.as_u64()
			.is_some_and(|sequence| sequence > 0)
	);
	assert_eq!(serde_json::from_str::<serde_json::Value>(&observed.2).expect("json"), payload);

	let live = session.dom().snapshot();
	drop(session);
	let replayed =
		omp_session::Session::open(&journal_path, omp_session::ComponentRegistry::standard())
			.expect("journal replays");
	assert_eq!(replayed.dom().snapshot(), live, "replay never re-runs the matcher");
}

#[tokio::test]
async fn the_per_turn_cap_downgrades_further_interrupts_to_notes() {
	let directory = tempfile::tempdir().expect("temporary directory");
	let journal_path = directory.path().join("cap.oms");
	let (inference, requests) = ScriptedInference::new([
		text_script("forbidden one"),
		text_script("forbidden two"),
		text_script("forbidden three"),
		text_script("forbidden four"),
	]);
	let mut directors = DirectorRegistry::standard();
	directors.register_extension(Box::new(ForbiddenWord));
	let (gate, dispatches) = HookGate::channel();
	let gate = Arc::new(gate);
	gate
		.subscribe("test", [subscription()])
		.expect("observer subscription");
	let mut kernel = Kernel::new(
		inference,
		registry(std::iter::empty()),
		DispatchPolicy::new(BlobStore::open(directory.path().join("blobs")).expect("blobs")),
		StaticPrompt(Str::new_static("test system")),
	)
	.with_director_registry(directors.clone())
	.with_hook_gate(gate);
	let mut session = fresh_session(&journal_path);
	DirectorStack::from_dom(session.dom(), &directors)
		.engage_registered(&mut session, FAMILY)
		.expect("watcher engages");

	let outcome = kernel
		.run_turn(
			&mut session,
			TurnInput { text: Str::new_static("answer"), attachments: Vec::new() },
			RunControl::default(),
		)
		.await
		.expect("capped turn completes");

	assert_eq!(outcome.stop, TurnStop::Completed);
	assert_eq!(outcome.assistant_text, "forbidden four", "past the cap the response stands");
	assert_eq!(requests.lock().len(), 4, "three redirects, then the capped response");
	let interrupted = session
		.dom()
		.select("body turn assistant")
		.expect("selector")
		.filter_map(|handle| session.dom().get(handle))
		.filter(|node| prop(node, "interrupt").is_some())
		.count();
	assert_eq!(interrupted, 3);
	let caps = session
		.dom()
		.select("body turn notice")
		.expect("selector")
		.filter_map(|handle| session.dom().get(handle))
		.filter(|node| prop(node, "name") == Some("stream-redirect-cap"))
		.count();
	assert_eq!(caps, 1, "the cap notice is journaled once");
	let outcomes = dispatches
		.try_iter()
		.map(|dispatch| {
			serde_json::from_slice::<serde_json::Value>(&dispatch.payload).expect("payload")
				["interrupted"]
				.clone()
		})
		.collect::<Vec<_>>();
	assert_eq!(outcomes, [true, true, true, false], "a capped interrupt is reported as a note");

	let live = session.dom().snapshot();
	drop(session);
	let replayed =
		omp_session::Session::open(&journal_path, omp_session::ComponentRegistry::standard())
			.expect("journal replays");
	assert_eq!(replayed.dom().snapshot(), live, "the cap count survives replay");
}
