//! Loop-level stream redirects (ADR 0038 §1): a scripted Director's watcher
//! interrupts a streamed response, the loop journals one redirect
//! transaction and resamples in the same turn, the per-turn cap downgrades
//! further interrupts to notes, observations reach lifecycle hooks only after
//! commit, and replay reproduces the live tree.

use std::sync::Arc;

use bytes::Bytes;
use flume::Sender;
use omp_agent::{
	Director, DirectorCx, DirectorRegistry, DirectorStack, DispatchPolicy, HookGate, HookPhase,
	Kernel, KernelEvent, OnFailure, PartialOutput, RunControl, SourceRef, StaticPrompt,
	StreamEffect, StreamFragment, StreamInterrupt, StreamObservation, StreamVerdict, StreamWatch,
	TurnInput, TurnStop, Up, When,
};
use omp_ai::{ChatEvent, ChatRequest, ContentPart, OpaqueJson, Role, ToolCall, ToolCallId};
use omp_core::{Str, sf};
use omp_dom::{KnownTag, PropId, PropKey, Tag, Value};
use omp_journal::blob::BlobStore;
use omp_proto::toolhost::v1::HookEventId;
use omp_tool::Part;

mod support;

use support::{ScriptedInference, fresh_session, journal_entries, registry, spec, text_script};
const FAMILY: &str = "test-watch";
const INJECTION: &str = "Rewrite without the forbidden word.";

/// Interrupts every response whose visible text contains `forbidden`.
struct ForbiddenWord(PartialOutput);

impl Director for ForbiddenWord {
	fn id(&self) -> &str {
		FAMILY
	}

	fn watch_stream(&self, _: &DirectorCx<'_>, _: &ChatRequest) -> Option<Box<dyn StreamWatch>> {
		Some(Box::new(Watch(self.0)))
	}
}

struct Watch(PartialOutput);

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
			culprit:       None,
			partial:       self.0,
			label:         Str::new_static("stream rule forbidden-word"),
			sibling_label: Str::new_static("interrupted by a stream rule on another tool call"),
			effect:        StreamEffect {
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

/// Redirects only the named tool call before execution can begin.
struct ToolCulprit;

impl Director for ToolCulprit {
	fn id(&self) -> &str {
		FAMILY
	}

	fn watch_stream(&self, _: &DirectorCx<'_>, _: &ChatRequest) -> Option<Box<dyn StreamWatch>> {
		Some(Box::new(ToolWatch))
	}
}

struct ToolWatch;

impl StreamWatch for ToolWatch {
	fn fragment(&mut self, _: StreamFragment<'_>) -> StreamVerdict {
		StreamVerdict::Pass
	}

	fn call_ready(&mut self, _: u32, call: &ToolCall) -> StreamVerdict {
		if call.id.as_str() != "culprit-edit" {
			return StreamVerdict::Pass;
		}
		StreamVerdict::Interrupt(StreamInterrupt {
			culprit:       Some(call.id.clone()),
			partial:       PartialOutput::Discard,
			label:         Str::new_static("stream rule no-unwrap"),
			sibling_label: Str::new_static("interrupted by a stream rule on another tool call"),
			effect:        StreamEffect {
				developer: Some(Str::new_static(INJECTION)),
				notice: Some(Str::new_static("tool call redirected")),
				notice_name: Some(Str::new_static("stream-rule")),
				..StreamEffect::default()
			},
		})
	}
}

/// Never interrupts: a text match records its body as a developer aside, a
/// tool match records a `<diag>` against the call.
struct NonInterrupting;

impl Director for NonInterrupting {
	fn id(&self) -> &str {
		FAMILY
	}

	fn watch_stream(&self, _: &DirectorCx<'_>, _: &ChatRequest) -> Option<Box<dyn StreamWatch>> {
		Some(Box::new(NoteWatch))
	}
}

struct NoteWatch;

impl StreamWatch for NoteWatch {
	fn fragment(&mut self, _: StreamFragment<'_>) -> StreamVerdict {
		StreamVerdict::Pass
	}

	fn block_end(&mut self, _: u32) -> StreamVerdict {
		StreamVerdict::Note(StreamEffect {
			developer: Some(Str::new_static(INJECTION)),
			notice: Some(Str::new_static("forbidden word noted")),
			notice_name: Some(Str::new_static("stream-rule")),
			..StreamEffect::default()
		})
	}

	fn call_ready(&mut self, _: u32, call: &ToolCall) -> StreamVerdict {
		StreamVerdict::Note(StreamEffect {
			call_diags: vec![(call.id.clone(), Str::new_static(INJECTION))],
			..StreamEffect::default()
		})
	}
}

/// A Director whose watcher never matches contributes nothing of its own to
/// the journal: the engaged run records the same entry sequence as an
/// unwatched run, plus the one `director.engage` patch that engaging it is.
struct Quiet;

impl Director for Quiet {
	fn id(&self) -> &str {
		FAMILY
	}

	fn watch_stream(&self, _: &DirectorCx<'_>, _: &ChatRequest) -> Option<Box<dyn StreamWatch>> {
		Some(Box::new(QuietWatch))
	}
}

struct QuietWatch;

impl StreamWatch for QuietWatch {
	fn fragment(&mut self, _: StreamFragment<'_>) -> StreamVerdict {
		StreamVerdict::Pass
	}
}

/// Queues a user interrupt with the redirect, exercising the next admission
/// boundary before a resample can start.
struct RedirectThenInterrupt(Sender<Up>);

impl Director for RedirectThenInterrupt {
	fn id(&self) -> &str {
		FAMILY
	}

	fn watch_stream(&self, _: &DirectorCx<'_>, _: &ChatRequest) -> Option<Box<dyn StreamWatch>> {
		Some(Box::new(InterruptWatch(self.0.clone())))
	}
}

struct InterruptWatch(Sender<Up>);

impl StreamWatch for InterruptWatch {
	fn fragment(&mut self, fragment: StreamFragment<'_>) -> StreamVerdict {
		if !fragment
			.bytes
			.windows(9)
			.any(|window| window == b"forbidden")
		{
			return StreamVerdict::Pass;
		}
		let _ = self.0.send(Up::Interrupt);
		StreamVerdict::Interrupt(StreamInterrupt {
			culprit:       None,
			partial:       PartialOutput::Discard,
			label:         Str::new_static("stream rule forbidden-word"),
			sibling_label: Str::new_static("interrupted by a stream rule on another tool call"),
			effect:        StreamEffect {
				developer: Some(Str::new_static(INJECTION)),
				notice: Some(Str::new_static("forbidden word redirected")),
				notice_name: Some(Str::new_static("stream-rule")),
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

fn two_tool_calls() -> Vec<ChatEvent> {
	let read = ToolCall {
		id:        ToolCallId::from("innocent-read"),
		name:      Str::new_static("read"),
		arguments: OpaqueJson::new(serde_json::json!({})),
	};
	let edit = ToolCall {
		id:        ToolCallId::from("culprit-edit"),
		name:      Str::new_static("edit"),
		arguments: OpaqueJson::new(serde_json::json!({})),
	};
	vec![
		ChatEvent::ToolCallStarted { index: 0, id: read.id.clone(), name: read.name.clone() },
		ChatEvent::ToolArgumentsDelta { index: 0, bytes: Bytes::from_static(b"{}") },
		ChatEvent::ToolCallReady { index: 0, call: read },
		ChatEvent::ToolCallStarted { index: 1, id: edit.id.clone(), name: edit.name.clone() },
		ChatEvent::ToolArgumentsDelta { index: 1, bytes: Bytes::from_static(b"{}") },
		ChatEvent::ToolCallReady { index: 1, call: edit },
	]
}

/// An aborted call's model-visible label lives on its terminal error `<diag>`.
/// A `Discard` redirect excludes the call from the resample projection.
fn settled_result_text(dom: &omp_dom::Dom, call_id: &str) -> String {
	let call = dom
		.select(&format!("body turn [id={call_id}]"))
		.expect("selector")
		.into_iter()
		.next()
		.expect("the settled call element journals");
	let diag = dom
		.children(call)
		.iter()
		.filter_map(|&handle| dom.get(handle))
		.find(|node| {
			node.tag == Tag::Known(KnownTag::Diag)
				&& node
					.prop(&PropKey::from(PropId::Severity))
					.and_then(Value::as_str)
					== Some("error")
		})
		.expect("the aborted call's error diagnostic journals");
	if let Some(Value::Json(data)) = diag.prop(&PropKey::from(PropId::Data)) {
		if let Ok(parts) = serde_json::from_str::<Vec<Part>>(data.get()) {
			return parts
				.iter()
				.map(|part| match part {
					Part::Text { text } => text.as_str().to_owned(),
					Part::Json { json } => String::from_utf8_lossy(&json[..]).into_owned(),
					Part::Blob { .. } => String::new(),
				})
				.collect::<Vec<_>>()
				.join("\n");
		}
	}
	diag
		.prop(&PropKey::from(PropId::Text))
		.and_then(Value::as_str)
		.unwrap_or_default()
		.to_owned()
}

#[tokio::test]
async fn a_watcher_interrupt_redirects_resamples_and_replays() {
	let directory = tempfile::tempdir().expect("temporary directory");
	let journal_path = directory.path().join("redirect.oms");
	let (inference, requests) =
		ScriptedInference::new([text_script("a forbidden answer"), text_script("a clean answer")]);
	let mut directors = DirectorRegistry::standard();
	directors.register_extension(Box::new(ForbiddenWord(PartialOutput::Discard)));
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
	directors.register_extension(Box::new(ForbiddenWord(PartialOutput::Discard)));
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

#[tokio::test]
async fn keep_projects_the_partial_assistant_and_the_redirect_placeholders() {
	let directory = tempfile::tempdir().expect("temporary directory");
	let journal_path = directory.path().join("keep.oms");
	let (inference, requests) =
		ScriptedInference::new([text_script("a forbidden answer"), text_script("a clean answer")]);
	let mut directors = DirectorRegistry::standard();
	directors.register_extension(Box::new(ForbiddenWord(PartialOutput::Keep)));
	let mut kernel = Kernel::new(
		inference,
		registry(std::iter::empty()),
		DispatchPolicy::new(BlobStore::open(directory.path().join("blobs")).expect("blobs")),
		StaticPrompt(Str::new_static("test system")),
	)
	.with_director_registry(directors.clone());
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
	let requests = requests.lock();
	assert_eq!(requests.len(), 2);
	assert!(
		assistant_texts(&requests[1])
			.iter()
			.any(|text| text.contains("forbidden")),
		"keep projects the interrupted assistant into the resample"
	);
	let interrupted = session
		.dom()
		.select(&format!("body turn assistant[interrupt={FAMILY}]"))
		.expect("selector")
		.next()
		.expect("interrupted assistant");
	assert_eq!(
		prop(session.dom().get(interrupted).expect("assistant"), "context"),
		None,
		"keep never excludes the partial assistant"
	);
}

#[tokio::test]
async fn tool_culprit_and_siblings_are_settled_then_discarded_before_resampling() {
	let directory = tempfile::tempdir().expect("temporary directory");
	let journal_path = directory.path().join("tool-redirect.oms");
	let (inference, requests) = ScriptedInference::new([two_tool_calls(), text_script("clean")]);
	let mut directors = DirectorRegistry::standard();
	directors.register_extension(Box::new(ToolCulprit));
	let mut kernel = Kernel::new(
		inference,
		registry([spec("read", 1, "unused"), spec("edit", 1, "unused")]),
		DispatchPolicy::new(BlobStore::open(directory.path().join("blobs")).expect("blobs")),
		StaticPrompt(Str::new_static("test system")),
	)
	.with_director_registry(directors.clone());
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
		.expect("tool redirect completes");

	assert_eq!(outcome.stop, TurnStop::Completed);
	assert_eq!(requests.lock().len(), 2, "a redirect resamples before tool execution");
	assert!(
		settled_result_text(session.dom(), "culprit-edit").contains("stream rule no-unwrap"),
		"the culprit receives its matching label"
	);
	assert!(
		settled_result_text(session.dom(), "innocent-read")
			.contains("interrupted by a stream rule on another tool call"),
		"siblings receive the neutral label"
	);
	let request = &requests.lock()[1];
	assert!(
		!request
			.messages
			.iter()
			.any(|message| message.role == Role::Tool),
		"discard drops every placeholder result with the interrupted assistant"
	);
}

#[tokio::test]
async fn interrupt_accepted_with_a_redirect_prevents_the_resample() {
	let directory = tempfile::tempdir().expect("temporary directory");
	let journal_path = directory.path().join("redirect-interrupt.oms");
	let (inference, requests) = ScriptedInference::new([text_script("a forbidden answer")]);
	let kernel = Kernel::new(
		inference,
		registry(std::iter::empty()),
		DispatchPolicy::new(BlobStore::open(directory.path().join("blobs")).expect("blobs")),
		StaticPrompt(Str::new_static("test system")),
	);
	let mailbox = kernel.mailbox();
	let mut directors = DirectorRegistry::standard();
	directors.register_extension(Box::new(RedirectThenInterrupt(mailbox)));
	let mut kernel = kernel.with_director_registry(directors.clone());
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
		.expect("interrupt settles the turn");

	assert_eq!(outcome.stop, TurnStop::Cancelled);
	assert_eq!(requests.lock().len(), 1, "no resample starts after the accepted interrupt");
	assert_eq!(
		session
			.dom()
			.select(&format!("body turn assistant[interrupt={FAMILY}]"))
			.expect("selector")
			.count(),
		1,
		"the committed redirect remains durable"
	);
}

#[tokio::test]
async fn redirect_consumes_the_request_budget_before_its_resample() {
	let directory = tempfile::tempdir().expect("temporary directory");
	let journal_path = directory.path().join("redirect-budget.oms");
	let (inference, requests) = ScriptedInference::new([text_script("a forbidden answer")]);
	let mut directors = DirectorRegistry::standard();
	directors.register_extension(Box::new(ForbiddenWord(PartialOutput::Discard)));
	let mut kernel = Kernel::new(
		inference,
		registry(std::iter::empty()),
		DispatchPolicy::new(BlobStore::open(directory.path().join("blobs")).expect("blobs")),
		StaticPrompt(Str::new_static("test system")),
	)
	.with_director_registry(directors.clone());
	let mut session = fresh_session(&journal_path);
	DirectorStack::from_dom(session.dom(), &directors)
		.engage_registered(&mut session, FAMILY)
		.expect("watcher engages");

	let outcome = kernel
		.run_turn(
			&mut session,
			TurnInput { text: Str::new_static("answer"), attachments: Vec::new() },
			RunControl::default()
				.with_request_budget(1)
				.with_request_budget_notice(false),
		)
		.await
		.expect("budget blocks the resample");

	assert_eq!(outcome.stop, TurnStop::Completed);
	assert_eq!(requests.lock().len(), 1, "the interrupted provider request consumes the budget");
	assert_eq!(
		session
			.dom()
			.select("body turn notice[custom:name=request-budget]")
			.expect("selector")
			.count(),
		1
	);
}

/// ADR 0038 test plan: "without a watcher, the journal is byte-identical to
/// today". Entry identities are ULIDs, so two runs never share bytes; the
/// observable is the entry sequence. A watcher that only ever passes adds no
/// entry of its own, and the engaged run differs from the unwatched one by
/// exactly the `director.engage` patch that engaging it is.
#[tokio::test]
async fn a_watcher_that_never_matches_adds_nothing_to_the_journal() {
	async fn run(engage: bool, path: &std::path::Path, blobs: &std::path::Path) -> Vec<String> {
		let (inference, requests) = ScriptedInference::new([text_script("a clean answer")]);
		let mut directors = DirectorRegistry::standard();
		directors.register_extension(Box::new(Quiet));
		let mut kernel = Kernel::new(
			inference,
			registry(std::iter::empty()),
			DispatchPolicy::new(BlobStore::open(blobs).expect("blobs")),
			StaticPrompt(Str::new_static("test system")),
		)
		.with_director_registry(directors.clone());
		let mut session = fresh_session(path);
		if engage {
			DirectorStack::from_dom(session.dom(), &directors)
				.engage_registered(&mut session, FAMILY)
				.expect("watcher engages");
		}
		kernel
			.run_turn(
				&mut session,
				TurnInput { text: Str::new_static("answer"), attachments: Vec::new() },
				RunControl::default(),
			)
			.await
			.expect("the unwatched turn completes");
		assert_eq!(requests.lock().len(), 1, "a passing watcher never resamples");
		drop(session);
		journal_entries(path)
			.into_iter()
			.map(|entry| match entry.label {
				Some(label) => format!("{}:{label}", entry.kind.name),
				None => entry.kind.name.to_string(),
			})
			.collect()
	}

	let directory = tempfile::tempdir().expect("temporary directory");
	let watched =
		run(true, &directory.path().join("watched.oms"), &directory.path().join("b1")).await;
	let plain = run(false, &directory.path().join("plain.oms"), &directory.path().join("b2")).await;
	// Both runs engage a standard Director mid-turn; only the first entry is
	// the explicit engagement under test.
	let mut engaged = watched.clone();
	if let Some(index) = engaged
		.iter()
		.position(|entry| entry == "patch:director.engage")
	{
		engaged.remove(index);
	}
	assert_eq!(engaged, plain, "a passing watcher journals nothing of its own:\n{watched:#?}");
}

/// ADR 0038 §6: a non-interrupting match uses the structured channels. A tool
/// source becomes a `<diag>` child of its call element; a text source becomes
/// the rule body as a developer aside. Neither resamples.
#[tokio::test]
async fn a_non_interrupting_match_records_a_diag_and_a_developer_aside() {
	let directory = tempfile::tempdir().expect("temporary directory");
	let journal_path = directory.path().join("note.oms");
	let (inference, requests) = ScriptedInference::new([two_tool_calls(), text_script("clean")]);
	let mut directors = DirectorRegistry::standard();
	directors.register_extension(Box::new(NonInterrupting));
	let mut kernel = Kernel::new(
		inference,
		registry([spec("read", 1, "unused"), spec("edit", 1, "unused")]),
		DispatchPolicy::new(BlobStore::open(directory.path().join("blobs")).expect("blobs")),
		StaticPrompt(Str::new_static("test system")),
	)
	.with_director_registry(directors.clone());
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
		.expect("the noted turn completes");

	assert_eq!(outcome.stop, TurnStop::Completed);
	assert!(requests.lock().len() >= 2, "the tool round trip still runs");
	let diags = session
		.dom()
		.select("body turn [id=culprit-edit] diag")
		.expect("selector")
		.count();
	assert!(diags >= 1, "a tool-source note lands as a diag on its call");
	let developers = session
		.dom()
		.select("body turn developer")
		.expect("selector")
		.count();
	assert!(developers >= 1, "a text-source note lands as a developer aside");
}
