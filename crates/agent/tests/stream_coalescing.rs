//! Streamed deltas reach the journal coalesced: a burst commits as one durable
//! `stream@1` append, stream switches keep arrival order, and text waiting in
//! the window commits on its own while the provider is silent.
//!
//! The paused clock makes the window deterministic: time only moves when every
//! task is idle, so an always-ready scripted stream fits in one window, and a
//! stalled one lets the commit timer fire.

use std::{
	future::{Future, ready},
	path::Path,
	time::{Duration, SystemTime},
};

use bytes::Bytes;
use omp_agent::{DispatchPolicy, Inference, Kernel, RunControl, StaticPrompt, TurnInput, TurnStop};
use omp_ai::{
	BlockKind, ChatEvent, ChatRequest, ChatStream, FinishReason, ProviderId, RequestId,
	ResponseMeta, RouteId, ToolCall, ToolCallId, call::OpaqueJson,
};
use omp_core::Str;
use omp_journal::{Entry, blob::BlobStore, kind};

mod support;
use support::{
	ScriptedInference, completed, fresh_session, journal_entries, registry, spec, text_script,
};

/// The coalescing window in `omp_agent::stream_coalesce`.
const WINDOW: Duration = Duration::from_millis(16);

fn input(text: &str) -> TurnInput {
	TurnInput { text: Str::new(text), attachments: Vec::new() }
}

fn policy(path: &Path) -> DispatchPolicy {
	DispatchPolicy::new(BlobStore::open(path).expect("blob store opens"))
}

/// Text of every `stream@1` append, in journal order.
fn stream_appends(entries: &[Entry]) -> Vec<String> {
	entries
		.iter()
		.filter(|entry| entry.kind.name.as_str() == kind::STREAM)
		.filter_map(|entry| {
			let data: serde_json::Value =
				serde_json::from_str(entry.data.as_str()).expect("stream entry is JSON");
			(data["op"] == "append").then(|| data["text"].as_str().expect("append text").to_owned())
		})
		.collect()
}

#[tokio::test(start_paused = true)]
async fn a_burst_of_text_deltas_commits_as_one_stream_append() {
	let directory = tempfile::tempdir().expect("temporary directory");
	let journal_path = directory.path().join("burst.oms");
	let deltas: Vec<String> = (0..512).map(|n| format!("{n} ")).collect();
	let expected = deltas.concat();
	let mut script = vec![ChatEvent::BlockStarted { index: 0, kind: BlockKind::Text }];
	script.extend(
		deltas
			.iter()
			.map(|delta| ChatEvent::TextDelta { index: 0, text: Str::new(delta) }),
	);
	script.push(completed(FinishReason::Stop, 1));
	let (inference, _) = ScriptedInference::new([script]);
	let mut kernel = Kernel::new(
		inference,
		registry(std::iter::empty()),
		policy(&directory.path().join("blobs")),
		StaticPrompt(Str::new_static("test system")),
	);
	let mut session = fresh_session(&journal_path);

	let outcome = kernel
		.run_turn(&mut session, input("stream a burst"), RunControl::default())
		.await
		.expect("turn completes");

	assert_eq!(outcome.stop, TurnStop::Completed);
	assert_eq!(outcome.assistant_text, expected);
	drop(session);
	let appends = stream_appends(&journal_entries(&journal_path));
	assert_eq!(appends, [expected], "512 deltas inside one window commit as one append");
}

#[tokio::test(start_paused = true)]
async fn switching_streams_commits_the_previous_one_first() {
	let directory = tempfile::tempdir().expect("temporary directory");
	let journal_path = directory.path().join("switch.oms");
	let script = vec![
		ChatEvent::BlockStarted { index: 0, kind: BlockKind::Thinking },
		ChatEvent::ThinkingDelta { index: 0, text: Str::new_static("weigh ") },
		ChatEvent::ThinkingDelta { index: 0, text: Str::new_static("it") },
		ChatEvent::BlockStarted { index: 1, kind: BlockKind::Text },
		ChatEvent::TextDelta { index: 1, text: Str::new_static("an") },
		ChatEvent::TextDelta { index: 1, text: Str::new_static("swer") },
		ChatEvent::ThinkingDelta { index: 0, text: Str::new_static(" again") },
		completed(FinishReason::Stop, 2),
	];
	let (inference, _) = ScriptedInference::new([script]);
	let mut kernel = Kernel::new(
		inference,
		registry(std::iter::empty()),
		policy(&directory.path().join("blobs")),
		StaticPrompt(Str::new_static("test system")),
	);
	let mut session = fresh_session(&journal_path);

	let outcome = kernel
		.run_turn(&mut session, input("think, then answer"), RunControl::default())
		.await
		.expect("turn completes");

	assert_eq!(outcome.assistant_text, "answer");
	drop(session);
	assert_eq!(
		stream_appends(&journal_entries(&journal_path)),
		["weigh it", "answer", " again"],
		"each run of one stream commits before the next stream's text"
	);
}

#[tokio::test(start_paused = true)]
async fn streamed_tool_arguments_commit_before_the_call_is_ready() {
	let directory = tempfile::tempdir().expect("temporary directory");
	let journal_path = directory.path().join("arguments.oms");
	let arguments = serde_json::json!({ "value": "coalesced" });
	let encoded = serde_json::to_vec(&arguments).expect("arguments encode");
	let (head, tail) = encoded.split_at(encoded.len() / 2);
	let call = ToolCall {
		id:        ToolCallId::from("call-1"),
		name:      Str::new_static("echo"),
		arguments: OpaqueJson::new(arguments),
	};
	let tool_turn = vec![
		ChatEvent::ToolCallStarted { index: 0, id: call.id.clone(), name: call.name.clone() },
		ChatEvent::ToolArgumentsDelta { index: 0, bytes: Bytes::copy_from_slice(head) },
		ChatEvent::ToolArgumentsDelta { index: 0, bytes: Bytes::copy_from_slice(tail) },
		ChatEvent::ToolCallReady { index: 0, call },
		completed(FinishReason::ToolCalls, 1),
	];
	let (inference, _) = ScriptedInference::new([tool_turn, text_script("done")]);
	let mut kernel = Kernel::new(
		inference,
		registry([spec("echo", 1, "echoed")]),
		policy(&directory.path().join("blobs")),
		StaticPrompt(Str::new_static("test system")),
	);
	let mut session = fresh_session(&journal_path);

	kernel
		.run_turn(&mut session, input("call echo"), RunControl::default())
		.await
		.expect("turn completes");

	drop(session);
	let entries = journal_entries(&journal_path);
	let appends = stream_appends(&entries);
	assert_eq!(
		appends.first().map(String::as_bytes),
		Some(encoded.as_slice()),
		"both argument fragments commit as one append"
	);
	let arguments_committed = entries
		.iter()
		.position(|entry| {
			entry.kind.name.as_str() == kind::STREAM && entry.data.as_str().contains("\"append\"")
		})
		.expect("argument append");
	let call_ready = entries
		.iter()
		.position(|entry| {
			entry.kind.name.as_str() == kind::TOOL_UPDATE
				&& entry.data.as_str().contains("\"kernel\":\"ready\"")
		})
		.expect("ready update");
	assert!(
		arguments_committed < call_ready,
		"the streamed arguments are durable before the ready call that settles them"
	);
}

/// Streams one delta and then never answers again.
struct SilentAfterPrefix;

impl Inference for SilentAfterPrefix {
	fn chat(
		&mut self,
		_: ChatRequest,
	) -> impl Future<Output = Result<ChatStream, omp_ai::Error>> + Send {
		ready(Ok(ChatStream::ordinary(Box::pin(async_stream::stream! {
			yield Ok(ChatEvent::Started(ResponseMeta {
				request_id: RequestId::from("silent"),
				provider: ProviderId::from("scripted"),
				route: RouteId::from("scripted/test"),
				model: None,
				provider_request_id: None,
				created_at: SystemTime::UNIX_EPOCH,
			}));
			yield Ok(ChatEvent::BlockStarted { index: 0, kind: BlockKind::Text });
			yield Ok(ChatEvent::TextDelta { index: 0, text: Str::new_static("prefix") });
			std::future::pending::<()>().await;
		}))))
	}
}

#[tokio::test(start_paused = true)]
async fn text_waiting_in_the_window_commits_while_the_provider_is_silent() {
	let directory = tempfile::tempdir().expect("temporary directory");
	let journal_path = directory.path().join("silent.oms");
	let mut kernel = Kernel::new(
		SilentAfterPrefix,
		registry(std::iter::empty()),
		policy(&directory.path().join("blobs")),
		StaticPrompt(Str::new_static("test system")),
	);
	let mut session = fresh_session(&journal_path);
	let started = tokio::time::Instant::now();

	let turn = kernel.run_turn(&mut session, input("then go quiet"), RunControl::default());
	let committed = async {
		loop {
			if stream_appends(&journal_entries(&journal_path)) == ["prefix"] {
				return tokio::time::Instant::now();
			}
			tokio::time::sleep(Duration::from_millis(1)).await;
		}
	};
	// Bounded in virtual time: without the commit timer the prefix would
	// wait for an event that never comes.
	let committed = tokio::time::timeout(Duration::from_secs(5), committed);
	let committed_at = tokio::select! {
		_ = turn => panic!("a silent provider never completes the turn"),
		at = committed => at.expect("buffered text commits without a further provider event"),
	};

	assert!(
		committed_at.duration_since(started) >= WINDOW,
		"the delta waited out its window before committing"
	);
	let entries = journal_entries(&journal_path);
	assert!(
		!entries
			.iter()
			.any(|entry| entry.kind.name.as_str() == kind::MSG_ASSISTANT_END),
		"the stream is still open: nothing was invented to close it"
	);
}
