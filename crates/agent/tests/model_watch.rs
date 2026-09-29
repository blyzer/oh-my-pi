//! The kernel's one `model_changed` emitter: a control-plane change is
//! reported the moment it commits (no request needed), a Director bind or
//! role selector reports `role`, a fallback the recovery middleware served is
//! reported when its answer starts, and nothing repeats an unchanged
//! selection.

use std::{
	future::{Future, ready},
	sync::Arc,
	time::SystemTime,
};

use omp_agent::{
	DispatchPolicy, HookGate, Inference, Kernel, ModelSelection, ModelSelector, NativeHookHost,
	NativeReply, RunControl, StaticPrompt, TurnInput,
};
use omp_ai::{ChatEvent, ChatRequest, ChatStream, ProviderId, RequestId, ResponseMeta, RouteId};
use omp_catalog::{ModelKey, ReasoningEffort};
use omp_con::{Origin, Value as ConValue};
use omp_core::Str;
use omp_journal::blob::BlobStore;
use omp_proto::toolhost::v1::HookEventId;
use parking_lot::Mutex;
use serde_json::Value;

mod support;

use support::{fresh_session, text_script};

/// Resolves `ai_model` as `provider/model` or `@role` (onto `role/<name>`)
/// and `ai_thinking` as the effort, the empty selector being `launch/model`.
struct TableSelector;

impl ModelSelector for TableSelector {
	fn select(&self, con: &omp_con::Ctx) -> Option<ModelSelection> {
		let selector = omp_agent::AI_MODEL.get(con);
		let (model, role) = match selector.as_str() {
			"" => (Str::new_static("launch/model"), None),
			"unknown" => return None,
			role if role.starts_with('@') => {
				(Str::new(format!("role/{}", &role[1..])), Some(Str::new(&role[1..])))
			},
			model => (Str::new(model), None),
		};
		let thinking = omp_agent::AI_THINKING
			.get(con)
			.parse::<ReasoningEffort>()
			.ok();
		Some(ModelSelection { model, role, thinking })
	}
}

/// Answers every request with text, reporting the model each answer was
/// served on.
struct Served {
	models: Vec<Option<&'static str>>,
}

impl Inference for Served {
	fn chat(
		&mut self,
		_: ChatRequest,
	) -> impl Future<Output = Result<ChatStream, omp_ai::Error>> + Send {
		let model = self.models.remove(0);
		let events = std::iter::once(ChatEvent::Started(ResponseMeta {
			request_id:          RequestId::from("served"),
			provider:            ProviderId::from("scripted"),
			route:               RouteId::from("scripted/test"),
			model:               model.map(ModelKey::from),
			provider_request_id: None,
			created_at:          SystemTime::UNIX_EPOCH,
		}))
		.chain(text_script("done"))
		.map(Ok);
		ready(Ok(ChatStream::ordinary(Box::pin(futures::stream::iter(events)))))
	}

	fn model_selector(&self) -> Option<Arc<dyn ModelSelector>> {
		Some(Arc::new(TableSelector))
	}
}

/// Records every `model_changed` observation.
#[derive(Default)]
struct Recorder {
	seen: Mutex<Vec<Value>>,
}

impl NativeHookHost for Recorder {
	fn decide<'a>(&'a self, _: HookEventId, _: &'a Value) -> omp_agent::BoxFut<'a, NativeReply> {
		Box::pin(ready(NativeReply::defer()))
	}

	fn observe(&self, event: HookEventId, payload: &Value) {
		assert_eq!(event, HookEventId::HookEventModelChanged);
		self.seen.lock().push(payload.clone());
	}
}

fn kernel(
	con: &Arc<omp_con::Ctx>,
	served: Vec<Option<&'static str>>,
	store: BlobStore,
) -> (Kernel<Served>, Arc<Recorder>) {
	let (gate, _receiver) = HookGate::channel();
	let gate = Arc::new(gate);
	let recorder = Arc::new(Recorder::default());
	gate.attach_native(Arc::clone(&recorder) as Arc<dyn NativeHookHost>, &[
		HookEventId::HookEventModelChanged,
	]);
	let kernel = Kernel::new(
		Served { models: served },
		support::registry([]),
		DispatchPolicy::new(store),
		StaticPrompt(Str::new_static("test")),
	)
	.with_hook_gate(gate)
	.with_con_context(Arc::clone(con));
	(kernel, recorder)
}

fn model_ref(provider: &str, model: &str) -> Value {
	serde_json::json!({"provider": provider, "api": "", "model": model})
}

fn take(recorder: &Recorder) -> Vec<Value> {
	std::mem::take(&mut *recorder.seen.lock())
}

#[test]
fn control_plane_changes_are_reported_as_they_commit() {
	let directory = tempfile::tempdir().expect("temporary directory");
	let con = Arc::new(omp_con::Ctx::new());
	omp_agent::AI_THINKING
		.set(&con, Str::new_static("high"))
		.unwrap();
	let store = BlobStore::open(directory.path()).expect("blob store");
	let (_kernel, recorder) = kernel(&con, Vec::new(), store);
	assert!(take(&recorder).is_empty(), "the launch selection is the baseline");

	omp_agent::AI_MODEL
		.set(&con, Str::new_static("p/b"))
		.unwrap();
	assert_eq!(take(&recorder), [serde_json::json!({
		"from_model": model_ref("launch", "model"),
		"to_model": model_ref("p", "b"),
		"role": "default",
		"reason": "user",
		"previous_thinking": "high",
		"thinking": "high",
	})]);

	// Rewriting the same selection is no change.
	omp_agent::AI_MODEL
		.set(&con, Str::new_static("p/b"))
		.unwrap();
	assert!(take(&recorder).is_empty());

	// A thinking-only change repeats the model.
	omp_agent::AI_THINKING
		.set(&con, Str::new_static("low"))
		.unwrap();
	assert_eq!(take(&recorder), [serde_json::json!({
		"from_model": model_ref("p", "b"),
		"to_model": model_ref("p", "b"),
		"role": "default",
		"reason": "user",
		"previous_thinking": "high",
		"thinking": "low",
	})]);

	// A role selector is role routing.
	omp_agent::AI_MODEL
		.set(&con, Str::new_static("@plan"))
		.unwrap();
	let seen = take(&recorder);
	assert_eq!(seen.len(), 1, "{seen:?}");
	assert_eq!(seen[0]["reason"], "role");
	assert_eq!(seen[0]["role"], "plan");
	assert_eq!(seen[0]["to_model"], model_ref("role", "plan"));

	// An unresolvable selector reports nothing; the next resolvable one
	// reports from the last reported model.
	omp_agent::AI_MODEL
		.set(&con, Str::new_static("unknown"))
		.unwrap();
	assert!(take(&recorder).is_empty());
	omp_agent::AI_MODEL
		.set(&con, Str::new_static("p/c"))
		.unwrap();
	let seen = take(&recorder);
	assert_eq!(seen[0]["from_model"], model_ref("role", "plan"));
	assert_eq!(seen[0]["reason"], "user");
}

#[test]
fn a_director_bind_is_role_routing_and_a_dropped_kernel_goes_quiet() {
	let directory = tempfile::tempdir().expect("temporary directory");
	let con = Arc::new(omp_con::Ctx::new());
	let store = BlobStore::open(directory.path()).expect("blob store");
	let (kernel, recorder) = kernel(&con, Vec::new(), store);
	con.derive_layers(&[(Str::new_static("plan#1"), vec![(
		Str::new_static("ai_model"),
		ConValue::Str(Str::new_static("p/planner")),
	)])]);
	let seen = take(&recorder);
	assert_eq!(seen.len(), 1, "{seen:?}");
	assert_eq!(seen[0]["reason"], "role", "a Director bind is role routing");
	assert_eq!(seen[0]["to_model"], model_ref("p", "planner"));
	// Leaving the engagement restores the session's own selection: the
	// role routing ends.
	con.derive_layers(&[]);
	let seen = take(&recorder);
	assert_eq!(seen[0]["to_model"], model_ref("launch", "model"));
	assert_eq!(seen[0]["reason"], "role");
	// The user's own next pick is theirs.
	omp_agent::AI_MODEL
		.set(&con, Str::new_static("p/mine"))
		.unwrap();
	assert_eq!(take(&recorder)[0]["reason"], "user");

	drop(kernel);
	con.set("ai_model", ConValue::Str(Str::new_static("p/after")), Origin::Session)
		.unwrap();
	assert!(take(&recorder).is_empty(), "the watch ends with its kernel");
}

#[tokio::test]
async fn a_fallback_the_middleware_served_is_reported_when_its_answer_starts() {
	let directory = tempfile::tempdir().expect("temporary directory");
	let mut session = fresh_session(&directory.path().join("watch.oms"));
	let con = Arc::new(omp_con::Ctx::new());
	omp_agent::AI_THINKING
		.set(&con, Str::new_static("medium"))
		.unwrap();
	let (mut kernel, recorder) = kernel(
		&con,
		vec![Some("launch/model"), Some("backup/model"), Some("backup/model"), Some("launch/model")],
		session.blobs().clone(),
	);
	let control = || RunControl::new(tokio_util::sync::CancellationToken::new(), None);
	let prompt = || TurnInput { text: Str::new_static("go"), attachments: Vec::new() };

	kernel
		.run_turn(&mut session, prompt(), control())
		.await
		.unwrap();
	assert!(take(&recorder).is_empty(), "served on the selection: no change");

	kernel
		.run_turn(&mut session, prompt(), control())
		.await
		.unwrap();
	assert_eq!(take(&recorder), [serde_json::json!({
		"from_model": model_ref("launch", "model"),
		"to_model": model_ref("backup", "model"),
		"role": "default",
		"reason": "fallback",
		"previous_thinking": "medium",
		"thinking": "medium",
	})]);

	kernel
		.run_turn(&mut session, prompt(), control())
		.await
		.unwrap();
	assert!(take(&recorder).is_empty(), "the fallback persists: no change");

	kernel
		.run_turn(&mut session, prompt(), control())
		.await
		.unwrap();
	let seen = take(&recorder);
	assert_eq!(seen.len(), 1, "{seen:?}");
	assert_eq!(seen[0]["reason"], "fallback", "the revert is the middleware's too");
	assert_eq!(seen[0]["to_model"], model_ref("launch", "model"));
}
