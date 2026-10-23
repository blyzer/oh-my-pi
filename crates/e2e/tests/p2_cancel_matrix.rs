//! P2: cancellation reaches every runtime scope without corrupting durable
//! truth.

use std::sync::Arc;

use omp_agent::{
	CancelTree, DispatchPolicy, Kernel, RunControl, StaticPrompt, TurnInput, TurnStop, Up,
};
use omp_ai::{ChatEvent, Completion, ExecutionReceipt, FinishReason, Usage};
use omp_core::Str;
use omp_e2e::support::{
	ScriptedInference, assert_all_entries_caused, create_session, journal_entries,
};
use omp_journal::{blob::BlobStore, kind};
use omp_tool::Registry;

fn completion() -> Vec<ChatEvent> {
	vec![ChatEvent::Completed(Completion {
		reason:  FinishReason::Stop,
		blocks:  0,
		usage:   Usage::default(),
		receipt: ExecutionReceipt::default().into(),
	})]
}

fn input() -> TurnInput {
	TurnInput { text: Str::new_static("cancel me"), attachments: Vec::new() }
}

#[tokio::test]
async fn p2_cancel_tree_interrupts_read_and_background_but_not_foreground_mutation() {
	let tree = CancelTree::new();
	let turn = tree.begin_turn();
	let foreground = turn.foreground_mutation();
	let read = turn.read_only_tool();
	let background = turn.background_tool();
	turn.cancel_turn();
	assert!(!foreground.is_cancelled(), "authorized mutation remains session-scoped");
	assert!(read.is_cancelled());
	assert!(background.is_cancelled());
	tree.cancel_session();
	assert!(foreground.is_cancelled());
}

#[tokio::test]
async fn p2_kernel_interrupt_records_no_false_completion_and_replays() {
	let temp = tempfile::tempdir().expect("P2 scratch");
	let path = temp.path().join("interrupt.oms");
	let (inference, _) = ScriptedInference::new([completion()]);
	let mut kernel = Kernel::new(
		inference,
		Arc::new(Registry::new()),
		DispatchPolicy::new(BlobStore::open(temp.path().join("blobs")).expect("blob store")),
		StaticPrompt(Str::new_static("P2")),
	);
	kernel
		.mailbox()
		.send(Up::Interrupt)
		.expect("queue interrupt");
	let mut session = create_session(&path).expect("session");
	let outcome = kernel
		.run_turn(&mut session, input(), RunControl::default())
		.await
		.expect("turn");
	assert_eq!(outcome.stop, TurnStop::Cancelled);
	drop(session);
	let entries = journal_entries(&path);
	assert_all_entries_caused(&entries);
	assert!(
		!entries
			.iter()
			.any(|entry| entry.kind.name.as_str() == kind::MSG_ASSISTANT_END)
	);
	assert!(
		!entries
			.iter()
			.any(|entry| entry.kind.name.as_str() == kind::TURN_RECEIPT)
	);
	let replay = omp_session::Session::open(&path, omp_session::ComponentRegistry::standard())
		.expect("cancelled journal replays");
	assert_eq!(replay.dom().select("body turn").expect("selector").count(), 1);
	assert_eq!(
		replay
			.dom()
			.select("body turn assistant")
			.expect("selector")
			.count(),
		0
	);
}

#[tokio::test]
async fn p2_kernel_cancel_ends_session_and_future_turns() {
	let temp = tempfile::tempdir().expect("P2 scratch");
	let path = temp.path().join("cancel.oms");
	let (inference, _) = ScriptedInference::new([completion(), completion()]);
	let mut kernel = Kernel::new(
		inference,
		Arc::new(Registry::new()),
		DispatchPolicy::new(BlobStore::open(temp.path().join("blobs")).expect("blob store")),
		StaticPrompt(Str::new_static("P2")),
	);
	kernel.mailbox().send(Up::Cancel).expect("queue cancel");
	let mut session = create_session(&path).expect("session");
	let first = kernel
		.run_turn(&mut session, input(), RunControl::default())
		.await
		.expect("first");
	assert_eq!(first.stop, TurnStop::Cancelled);
	let second = kernel
		.run_turn(&mut session, input(), RunControl::default())
		.await
		.expect("second");
	assert_eq!(second.stop, TurnStop::Cancelled);
	drop(session);
	assert_all_entries_caused(&journal_entries(&path));
}

/// ADR 0038: an interrupt that lands while the redirect's resample streams.
mod redirect {
	use std::{
		future::{Future, ready},
		sync::Arc,
	};

	use omp_agent::{
		DirectorStack, DispatchPolicy, Inference, Kernel, RunControl, StaticPrompt, TurnInput,
		TurnStop, Up, directors::stream_rules::FAMILY,
	};
	use omp_ai::{BlockKind, ChatEvent, ChatRequest, ChatStream};
	use omp_con::Ctx;
	use omp_core::Str;
	use omp_e2e::support::{
		CapturedRequests, assert_all_entries_caused, create_session, guard_directors, hanging_stream,
		journal_entries, scripted_stream, text_response,
	};
	use omp_journal::{blob::BlobStore, kind};
	use omp_tool::Registry;
	use parking_lot::Mutex;

	/// The first response matches the guard rule; the resample never finishes
	/// and queues an interrupt as it opens.
	struct InterruptsTheResample {
		requests:  CapturedRequests,
		interrupt: Arc<Mutex<Option<flume::Sender<Up>>>>,
	}

	impl Inference for InterruptsTheResample {
		fn chat(
			&mut self,
			request: ChatRequest,
		) -> impl Future<Output = Result<ChatStream, omp_ai::Error>> + Send {
			let seen = {
				let mut requests = self.requests.lock();
				requests.push(request);
				requests.len()
			};
			let stream = if seen == 1 {
				scripted_stream(text_response("a danger answer"))
			} else {
				self
					.interrupt
					.lock()
					.as_ref()
					.expect("the kernel mailbox is wired")
					.send(Up::Interrupt)
					.expect("queue the interrupt");
				hanging_stream(vec![
					ChatEvent::BlockStarted { index: 0, kind: BlockKind::Text },
					ChatEvent::TextDelta { index: 0, text: Str::new_static("a clean") },
				])
			};
			ready(Ok(stream))
		}
	}

	#[tokio::test]
	async fn p2_interrupt_during_a_redirect_resample_keeps_the_redirect_and_records_no_completion() {
		let temp = tempfile::tempdir().expect("P2 scratch");
		let path = temp.path().join("redirect-interrupt.oms");
		let requests = CapturedRequests::default();
		let interrupt = Arc::new(Mutex::new(None));
		let mut kernel = Kernel::new(
			InterruptsTheResample {
				requests:  Arc::clone(&requests),
				interrupt: Arc::clone(&interrupt),
			},
			Arc::new(Registry::new()),
			DispatchPolicy::new(BlobStore::open(temp.path().join("blobs")).expect("blob store")),
			StaticPrompt(Str::new_static("P2")),
		)
		.with_director_registry(guard_directors())
		.with_con_context(Arc::new(Ctx::new()));
		*interrupt.lock() = Some(kernel.mailbox());
		let mut session = create_session(&path).expect("session");
		DirectorStack::from_dom(session.dom(), &guard_directors())
			.engage_registered(&mut session, FAMILY)
			.expect("the director engages");
		let outcome = kernel
			.run_turn(
				&mut session,
				TurnInput { text: Str::new_static("go"), attachments: Vec::new() },
				RunControl::default(),
			)
			.await
			.expect("the interrupted turn settles");
		assert_eq!(outcome.stop, TurnStop::Cancelled);
		assert_eq!(requests.lock().len(), 2, "the match redirected into a resample");
		drop(session);
		let entries = journal_entries(&path);
		assert_all_entries_caused(&entries);
		let count = |kind: &str| {
			entries
				.iter()
				.filter(|entry| entry.kind.name.as_str() == kind)
				.count()
		};
		assert_eq!(
			entries
				.iter()
				.filter(|entry| entry
					.label
					.as_ref()
					.is_some_and(|label| label == "director.stream-interrupt"))
				.count(),
			1,
			"the interrupt keeps the one committed redirect"
		);
		assert_eq!(count(kind::MSG_ASSISTANT_START), 1, "the cancelled resample opens no assistant");
		assert_eq!(count(kind::MSG_ASSISTANT_END), 1, "only the redirected response ended");
		assert_eq!(count(kind::TURN_RECEIPT), 1, "the cancelled resample records no completion");
		let replay = omp_session::Session::open(&path, omp_session::ComponentRegistry::standard())
			.expect("the interrupted journal replays");
		assert_eq!(
			replay
				.dom()
				.select("body turn assistant[interrupt=stream-rules]")
				.expect("selector")
				.count(),
			1
		);
	}
}
