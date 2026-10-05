//! Cross-crate runtime flags gate Director behavior at the kernel boundary.

use std::sync::{
	Arc,
	atomic::{AtomicBool, Ordering},
};

use omp_agent::{
	DirectorRegistry, DirectorStack, DispatchPolicy, Kernel, RunControl, RuntimeFlags, StaticPrompt,
	TurnInput, directors::goal::Goal,
};
use omp_core::{Str, sf};
use omp_journal::blob::BlobStore;

mod support;

use support::{
	ScriptedInference, fresh_session, registry, spec, spec_family, text_script, tool_script,
	tool_spec,
};

const INLINE_EDIT: &str = "<SM:EDIT path=\"src/a.rs\">\n<SM:FIND>\nlet x = \
                           1;\n</SM:FIND>\n<SM:PUT>\nlet x = 2;\n</SM:PUT>\n</SM:EDIT>";

fn flags(compaction: bool, goal: bool) -> RuntimeFlags {
	RuntimeFlags {
		automatic_compaction:     compaction,
		goal_enabled:             goal,
		autolearn_enabled:        false,
		autolearn_min_tool_calls: 5,
		recover_inline_edits:     true,
	}
}

#[tokio::test]
async fn automatic_compaction_flag_controls_director_engagement() {
	for (enabled, expected) in [(false, 0), (true, 1)] {
		let temp = tempfile::tempdir().expect("tempdir");
		let (inference, _) = ScriptedInference::new([text_script("done")]);
		let mut kernel = Kernel::new(
			inference,
			registry(std::iter::empty()),
			DispatchPolicy::new(BlobStore::open(temp.path().join("blobs")).expect("blobs")),
			StaticPrompt(sf!("system")),
		)
		.with_runtime_flags(flags(enabled, true));
		let mut session = fresh_session(&temp.path().join("compaction.oms"));
		kernel
			.run_turn(
				&mut session,
				TurnInput { text: sf!("run"), attachments: Vec::new() },
				RunControl::default(),
			)
			.await
			.expect("turn");
		assert_eq!(
			session
				.dom()
				.count("directors director[family=compaction]")
				.expect("selector"),
			expected
		);
	}
}

#[tokio::test]
async fn autolearn_flag_and_minimum_schedule_exactly_one_learn_call() {
	let temp = tempfile::tempdir().expect("tempdir");
	let (inference, requests) = ScriptedInference::new([
		tool_script("read-1", "read", serde_json::json!({})),
		text_script("candidate"),
		tool_script("learn-1", "learn", serde_json::json!({})),
		text_script("final"),
	]);
	let mut kernel = Kernel::new(
		inference,
		registry([spec("read", 1, "read"), spec("learn", 1, "learned")]),
		DispatchPolicy::new(BlobStore::open(temp.path().join("blobs")).expect("blobs")),
		StaticPrompt(sf!("system")),
	)
	.with_runtime_flags(RuntimeFlags {
		automatic_compaction:     false,
		goal_enabled:             true,
		autolearn_enabled:        true,
		autolearn_min_tool_calls: 1,
		recover_inline_edits:     true,
	});
	let mut session = fresh_session(&temp.path().join("autolearn.oms"));
	kernel
		.run_turn(
			&mut session,
			TurnInput { text: sf!("run"), attachments: Vec::new() },
			RunControl::default(),
		)
		.await
		.expect("turn");
	assert_eq!(requests.lock().len(), 4);
	assert_eq!(session.dom().count("body turn learn").expect("selector"), 1);
}

#[tokio::test]
async fn disabled_autolearn_never_schedules_learn_after_the_same_tool_count() {
	let temp = tempfile::tempdir().expect("tempdir");
	let (inference, requests) = ScriptedInference::new([
		tool_script("read-1", "read", serde_json::json!({})),
		text_script("candidate"),
	]);
	let mut kernel = Kernel::new(
		inference,
		registry([spec("read", 1, "read"), spec("learn", 1, "learned")]),
		DispatchPolicy::new(BlobStore::open(temp.path().join("blobs")).expect("blobs")),
		StaticPrompt(sf!("system")),
	)
	.with_runtime_flags(RuntimeFlags {
		automatic_compaction:     false,
		goal_enabled:             true,
		autolearn_enabled:        false,
		autolearn_min_tool_calls: 1,
		recover_inline_edits:     true,
	});
	let mut session = fresh_session(&temp.path().join("no-autolearn.oms"));
	kernel
		.run_turn(
			&mut session,
			TurnInput { text: sf!("run"), attachments: Vec::new() },
			RunControl::default(),
		)
		.await
		.expect("turn");
	assert_eq!(requests.lock().len(), 2);
	assert_eq!(session.dom().count("body turn learn").expect("selector"), 0);
}

async fn inline_recovery(flags_enabled: bool, family: &str, text: &str) -> (usize, usize, String) {
	let temp = tempfile::tempdir().expect("tempdir");
	let scripts = if flags_enabled && family == "sloppy" && text.contains("</SM:EDIT>") {
		vec![text_script(text), text_script("done")]
	} else {
		vec![text_script(text)]
	};
	let (inference, requests) = ScriptedInference::new(scripts);
	let mut kernel = Kernel::new(
		inference,
		registry([spec_family("edit", family, 1, "edited")]),
		DispatchPolicy::new(BlobStore::open(temp.path().join("blobs")).expect("blobs")),
		StaticPrompt(sf!("system")),
	)
	.with_runtime_flags(RuntimeFlags {
		automatic_compaction:     false,
		goal_enabled:             true,
		autolearn_enabled:        false,
		autolearn_min_tool_calls: 5,
		recover_inline_edits:     flags_enabled,
	});
	let mut session = fresh_session(&temp.path().join("inline.oms"));
	kernel
		.run_turn(
			&mut session,
			TurnInput { text: sf!("run"), attachments: Vec::new() },
			RunControl::default(),
		)
		.await
		.expect("turn");
	let assistant = session
		.dom()
		.select("body turn assistant")
		.expect("selector")
		.next()
		.and_then(|handle| session.dom().get(handle))
		.and_then(|node| node.prop(&omp_dom::PropKey::from(omp_dom::PropId::Text)))
		.and_then(omp_dom::Value::as_str)
		.unwrap_or("")
		.to_owned();
	let request_count = requests.lock().len();
	(request_count, session.dom().count("body turn edit").expect("selector"), assistant)
}

#[tokio::test]
async fn inline_sloppy_edit_recovery_is_gated_and_rejects_malformed_or_non_sloppy_text() {
	let prose = format!("Fixing now.\n\n{INLINE_EDIT}\n\nDone.");
	let (requests, calls, assistant) = inline_recovery(true, "sloppy", &prose).await;
	assert_eq!((requests, calls), (2, 1));
	assert_eq!(assistant, "Fixing now.\n\n\n\nDone.");

	let (requests, calls, assistant) = inline_recovery(false, "sloppy", &prose).await;
	assert_eq!((requests, calls), (1, 0));
	assert!(assistant.contains("<SM:EDIT"));

	let (requests, calls, _) = inline_recovery(true, "test", &prose).await;
	assert_eq!((requests, calls), (1, 0));

	let malformed = "<SM:EDIT path=\"src/a.rs\">\n<SM:FIND>\nmissing close";
	let (requests, calls, assistant) = inline_recovery(true, "sloppy", malformed).await;
	assert_eq!((requests, calls), (1, 0));
	assert_eq!(assistant, malformed);
}

/// The tool names of request `index`, in wire order.
fn tool_names(requests: &support::Requests, index: usize) -> Vec<String> {
	requests.lock()[index]
		.tools
		.iter()
		.map(|tool| tool.name.to_string())
		.collect()
}

/// The `tools` array of request `index` as the codec would receive it,
/// rendered once so two requests compare byte for byte.
fn tool_bytes(requests: &support::Requests, index: usize) -> String {
	format!("{:?}", requests.lock()[index].tools)
}

#[allow(
	clippy::future_not_send,
	reason = "the kernel turn future is driven on the test's own task, never sent"
)]
async fn turn<C: omp_agent::Inference>(kernel: &mut Kernel<C>, session: &mut omp_session::Session) {
	kernel
		.run_turn(
			session,
			TurnInput { text: sf!("run"), attachments: Vec::new() },
			RunControl::default(),
		)
		.await
		.expect("turn");
}

/// `goal` is not in the roster until a goal is first engaged; the engagement
/// mounts it at the next turn boundary and it stays for the rest of the
/// session, byte for byte, through pause, completion, and removal of the goal
/// (the `goal` tool answers an inactive goal with a typed fault instead).
#[tokio::test]
async fn goal_tool_mounts_at_the_first_engagement_and_stays_for_the_session() {
	let temp = tempfile::tempdir().expect("tempdir");
	let directors = DirectorRegistry::standard();
	let mut session = fresh_session(&temp.path().join("goal-roster.oms"));
	let (inference, requests) = ScriptedInference::new([
		text_script("before"),
		text_script("engaged"),
		text_script("paused"),
		text_script("removed"),
		text_script("replaced"),
	]);
	let mut kernel = Kernel::new(
		inference,
		registry([spec("goal", 1, "goal"), spec("read", 1, "read")]),
		DispatchPolicy::new(BlobStore::open(temp.path().join("blobs")).expect("blobs")),
		StaticPrompt(Str::new_static("system")),
	)
	.with_director_registry(DirectorRegistry::standard())
	.with_runtime_flags(flags(false, true));

	turn(&mut kernel, &mut session).await;
	assert_eq!(tool_names(&requests, 0), ["read"], "no goal has been engaged yet");

	let mut stack = DirectorStack::from_dom(session.dom(), &directors);
	stack
		.engage(&mut session, Box::new(Goal::new("finish", None)))
		.expect("goal engages");
	turn(&mut kernel, &mut session).await;
	assert_eq!(tool_names(&requests, 1), ["read", "goal"], "the engagement mounts goal");

	let mut stack = DirectorStack::from_dom(session.dom(), &directors);
	stack.pause(&mut session, "goal").expect("goal pauses");
	turn(&mut kernel, &mut session).await;
	let mut stack = DirectorStack::from_dom(session.dom(), &directors);
	stack.exit(&mut session, "goal").expect("goal exits");
	turn(&mut kernel, &mut session).await;
	let mut stack = DirectorStack::from_dom(session.dom(), &directors);
	stack
		.engage(&mut session, Box::new(Goal::new("again", None)))
		.expect("a replacement goal engages");
	turn(&mut kernel, &mut session).await;
	for index in 2..5 {
		assert_eq!(
			tool_bytes(&requests, index),
			tool_bytes(&requests, 1),
			"request {index}: pause, removal, and replacement never change the roster"
		);
	}
}

/// A session opened on an existing goal (any status) mounts `goal` from its
/// first request: the engagement predates the kernel.
#[tokio::test]
async fn an_existing_goal_mounts_the_tool_from_the_first_request_whatever_its_status() {
	for paused in [false, true] {
		let temp = tempfile::tempdir().expect("tempdir");
		let directors = DirectorRegistry::standard();
		let mut session = fresh_session(&temp.path().join("goal-roster.oms"));
		let mut stack = DirectorStack::from_dom(session.dom(), &directors);
		stack
			.engage(&mut session, Box::new(Goal::new("finish", None)))
			.expect("goal engages");
		if paused {
			stack.pause(&mut session, "goal").expect("goal pauses");
		}
		let (inference, requests) = ScriptedInference::new([text_script("candidate")]);
		let mut kernel = Kernel::new(
			inference,
			registry([spec("goal", 1, "goal")]),
			DispatchPolicy::new(BlobStore::open(temp.path().join("blobs")).expect("blobs")),
			StaticPrompt(Str::new_static("system")),
		)
		.with_director_registry(directors)
		.with_runtime_flags(flags(false, true));
		turn(&mut kernel, &mut session).await;
		assert_eq!(tool_names(&requests, 0), ["goal"], "paused: {paused}");
	}
}

/// Only a goal engagement mounts `goal`: a model that names the tool before
/// any engagement is refused at dispatch (typed, journaled), the director it
/// asked for never exists, and the next request's roster is unchanged.
#[tokio::test]
async fn the_model_cannot_mount_goal_by_calling_it() {
	let temp = tempfile::tempdir().expect("tempdir");
	let mut session = fresh_session(&temp.path().join("model-goal.oms"));
	let (inference, requests) = ScriptedInference::new([
		tool_script("goal-1", "goal", serde_json::json!({"op": "create", "objective": "mine"})),
		text_script("refused"),
		text_script("next turn"),
	]);
	let mut kernel = Kernel::new(
		inference,
		registry([spec("goal", 1, "goal ran"), spec("read", 1, "read")]),
		DispatchPolicy::new(BlobStore::open(temp.path().join("blobs")).expect("blobs")),
		StaticPrompt(Str::new_static("system")),
	)
	.with_director_registry(DirectorRegistry::standard())
	.with_runtime_flags(flags(false, true));
	turn(&mut kernel, &mut session).await;
	assert_eq!(support::result_text(&session, "goal-1"), ["skipped: `goal` is not mounted in \
	                                                       this session. No action was taken. \
	                                                       Available now: read."]);
	let journal = std::fs::read_to_string(session.journal_path()).expect("journal reads");
	assert!(journal.contains("tool.roster.restricted"), "{journal}");
	assert_eq!(
		session
			.dom()
			.count("directors director[family=goal]")
			.expect("selector"),
		0
	);
	turn(&mut kernel, &mut session).await;
	for index in 0..3 {
		assert_eq!(tool_names(&requests, index), ["read"], "request {index}");
	}
}

#[tokio::test]
async fn disabled_goal_is_removed_before_inference_and_never_mounted() {
	for (enabled, expected) in [(false, 0), (true, 1)] {
		let temp = tempfile::tempdir().expect("tempdir");
		let directors = DirectorRegistry::standard();
		let mut session = fresh_session(&temp.path().join("goal.oms"));
		DirectorStack::from_dom(session.dom(), &directors)
			.engage(&mut session, Box::new(Goal::new("finish", None)))
			.expect("goal engages");
		let (inference, requests) = ScriptedInference::new([text_script("candidate")]);
		let mut kernel = Kernel::new(
			inference,
			registry([spec("goal", 1, "goal")]),
			DispatchPolicy::new(BlobStore::open(temp.path().join("blobs")).expect("blobs")),
			StaticPrompt(Str::new_static("system")),
		)
		.with_director_registry(directors)
		.with_runtime_flags(flags(false, enabled));
		turn(&mut kernel, &mut session).await;
		assert_eq!(
			session
				.dom()
				.count("directors director[family=goal]")
				.expect("selector"),
			expected
		);
		assert_eq!(requests.lock().len(), 1, "one provider request per prose-only turn");
		assert_eq!(
			tool_names(&requests, 0).iter().any(|name| name == "goal"),
			enabled,
			"a disabled goal mounts nothing; an enabled one mounts the tool"
		);
	}
}

/// A session tool whose availability follows session-scoped state (`task` at
/// the recursion ceiling of the session the host presents).
struct Withholding {
	spec:       omp_tool::ToolSpec,
	advertised: Arc<AtomicBool>,
}

impl omp_agent::SessionTool for Withholding {
	fn spec(&self) -> &omp_tool::ToolSpec {
		&self.spec
	}

	fn call<'a>(
		&'a self,
		_cx: omp_agent::SessionToolCx<'a>,
		_args: Box<serde_json::value::RawValue>,
	) -> omp_agent::SessionToolFuture<'a> {
		Box::pin(async move {
			Ok(omp_tool::CallOutcome::Ok(
				serde_json::value::to_raw_value("refused").expect("raw payload"),
			))
		})
	}

	fn advertised(&self) -> bool {
		self.advertised.load(Ordering::SeqCst)
	}
}

/// A session tool's withholding is read once, when the roster is latched at
/// the first request: a later change of the state it follows (the `task`
/// recursion ceiling) cannot add or remove the declaration mid-session.
#[tokio::test]
async fn session_tool_withholding_is_latched_at_the_first_request() {
	for admitted_first in [false, true] {
		let temp = tempfile::tempdir().expect("tempdir");
		let advertised = Arc::new(AtomicBool::new(admitted_first));
		let (inference, requests) =
			ScriptedInference::new([text_script("first"), text_script("second")]);
		let mut kernel = Kernel::new(
			inference,
			registry([spec("task", 1, "declaration"), spec("read", 1, "read")]),
			DispatchPolicy::new(BlobStore::open(temp.path().join("blobs")).expect("blobs")),
			StaticPrompt(Str::new_static("system")),
		)
		.with_runtime_flags(flags(false, true))
		.with_session_tool(Arc::new(Withholding {
			spec:       tool_spec("task", 1),
			advertised: Arc::clone(&advertised),
		}));
		let mut session = fresh_session(&temp.path().join("withheld.oms"));
		for _ in 0..2 {
			turn(&mut kernel, &mut session).await;
			advertised.store(!admitted_first, Ordering::SeqCst);
		}
		let mut names = tool_names(&requests, 0);
		names.sort_unstable();
		let expected: &[&str] = if admitted_first {
			&["read", "task"]
		} else {
			&["read"]
		};
		assert_eq!(names, expected, "latched by the state at the first request");
		assert_eq!(
			tool_bytes(&requests, 1),
			tool_bytes(&requests, 0),
			"flipping the state afterwards never changes the roster"
		);
	}
}
