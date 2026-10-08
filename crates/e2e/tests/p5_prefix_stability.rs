//! P5: semantic prompt bands keep stable prefixes cacheable.

use std::sync::{Arc, atomic::Ordering};

use omp_agent::prompt::{
	PromptError, PromptOut, SlotAssembler, SlotClass, SlotDecl, SlotId, SlotRegistration, SlotSource,
};
use omp_core::Str;
use omp_scribe::Props;
use omp_session::{ComponentRegistry, Session};

struct Text(&'static str);

impl SlotSource for Text {
	fn render(
		&self,
		_dom: &omp_dom::Dom,
		_props: &Props,
		out: &mut dyn PromptOut,
	) -> Result<(), PromptError> {
		out.write_str(self.0);
		Ok(())
	}
}

fn source(
	slot: SlotId,
	class: SlotClass,
	owner: &'static str,
	text: &'static str,
) -> SlotRegistration {
	SlotRegistration {
		decl:   SlotDecl { slot, class, owner: Str::new_static(owner), priority: 0 },
		source: Arc::new(Text(text)),
	}
}

fn render(
	session: &Session,
	volatile: &'static str,
	dynamic: &'static str,
) -> omp_agent::prompt::RenderedPrompt {
	SlotAssembler::new(vec![
		source(SlotId::Conventions, SlotClass::Frozen, "frozen", "FROZEN\n"),
		source(SlotId::Tools, SlotClass::Stable, "stable", "STABLE\n"),
		source(SlotId::Memory, SlotClass::Dynamic, "dynamic", dynamic),
		source(SlotId::Status, SlotClass::Volatile, "volatile", volatile),
	])
	.render_banded(session.dom(), &Props::default())
	.expect("banded prompt")
}

#[test]
fn p5_volatile_changes_do_not_invalidate_stable_prompt_prefix_hashes() {
	let temp = tempfile::tempdir().expect("P5 scratch");
	let session = Session::create(temp.path().join("prefix.oms"), ComponentRegistry::standard())
		.expect("session");
	let before = render(&session, "STATUS A\n", "MEMORY A\n");
	let volatile_changed = render(&session, "STATUS B\n", "MEMORY A\n");
	assert_eq!(
		before.bands[SlotClass::Frozen as usize],
		volatile_changed.bands[SlotClass::Frozen as usize]
	);
	assert_eq!(
		before.bands[SlotClass::Stable as usize],
		volatile_changed.bands[SlotClass::Stable as usize]
	);
	assert_eq!(
		before.bands[SlotClass::Dynamic as usize],
		volatile_changed.bands[SlotClass::Dynamic as usize]
	);
	assert_ne!(
		before.bands[SlotClass::Volatile as usize],
		volatile_changed.bands[SlotClass::Volatile as usize]
	);
}

#[test]
fn p5_dynamic_changes_preserve_frozen_and_stable_band_hashes() {
	let temp = tempfile::tempdir().expect("P5 scratch");
	let session = Session::create(temp.path().join("dynamic.oms"), ComponentRegistry::standard())
		.expect("session");
	let before = render(&session, "STATUS\n", "MEMORY A\n");
	let after = render(&session, "STATUS\n", "MEMORY B\n");
	assert_eq!(before.bands[SlotClass::Frozen as usize], after.bands[SlotClass::Frozen as usize]);
	assert_eq!(before.bands[SlotClass::Stable as usize], after.bands[SlotClass::Stable as usize]);
	assert_ne!(before.bands[SlotClass::Dynamic as usize], after.bands[SlotClass::Dynamic as usize]);
	assert_eq!(
		before.bands[SlotClass::Volatile as usize],
		after.bands[SlotClass::Volatile as usize]
	);
	assert_eq!(before.items.len(), after.items.len());
}

/// Snapcompact never renders into the system prompt: an archive produced by
/// the real `CompactionDirector` changes the thread (the note plus frames),
/// while every canonical system band hash stays byte-identical, so the
/// provider prefix cache survives the compaction.
#[tokio::test]
async fn p5_snapcompact_archive_preserves_every_system_band_hash() {
	use omp_agent::{
		CompactionStrategy, RouteFacts,
		director::{BoxFut, Director, ErasedInference, MutDirectorCx, Prepared},
		directors::compaction::CompactionDirector,
		prompt::CanonicalPromptSource,
	};

	/// The archive renders locally; any inference call is a defect.
	struct NoInference;

	impl ErasedInference for NoInference {
		fn execute<'a>(
			&'a mut self,
			_request: omp_ai::ChatRequest,
		) -> BoxFut<'a, Result<omp_ai::ChatStream, omp_ai::Error>> {
			panic!("snapcompact must not call inference")
		}
	}

	let temp = tempfile::tempdir().expect("P5 scratch");
	let mut session =
		Session::create(temp.path().join("snapcompact.oms"), ComponentRegistry::standard())
			.expect("session");
	session.begin_turn().expect("history turn");
	session
		.user("Refactor the tokenizer so every keyword is interned.", Vec::new())
		.expect("history");
	session
		.receipt(omp_journal::data::TurnReceipt { tokens_in: 400_000, ..Default::default() })
		.expect("receipt");
	let (_, before) = CanonicalPromptSource
		.banded_render(session.dom())
		.expect("bands before");

	let con = omp_con::Ctx::new();
	omp_ai::settings::AI_COMPACTION_KEEP_RECENT_TOKENS
		.set(&con, 0)
		.expect("keep nothing verbatim");
	let blobs = omp_journal::blob::BlobStore::open(temp.path()).expect("blob store");
	let route = RouteFacts { context_window: 1_000_000, image_input: true, ..RouteFacts::default() };
	let turn = *session
		.dom()
		.children(session.dom().body())
		.last()
		.expect("turn");
	let request = omp_ai::ChatRequest {
		messages:          Arc::from([omp_ai::Message {
			role:    omp_ai::Role::User,
			content: Arc::from([omp_ai::ContentPart::Text {
				text:  Str::new_static("Refactor the tokenizer"),
				proof: None,
			}]),
			name:    None,
		}]),
		tools:             Arc::from([]),
		hosted_tools:      Arc::from([]),
		tool_choice:       omp_ai::Setting::Unset,
		output:            omp_ai::Setting::Unset,
		reasoning:         omp_ai::Setting::Unset,
		verbosity:         omp_ai::Setting::Unset,
		cache_retention:   omp_ai::Setting::Unset,
		service_tier:      omp_ai::Setting::Unset,
		sampling:          omp_ai::Sampling::default(),
		max_output_tokens: None,
		top_logprobs:      None,
		safety:            Arc::from([]),
		negotiation:       omp_ai::NegotiationPolicy::default(),
		forced_call:       None,
	};
	let mut inference = NoInference;
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
	let prepared = CompactionDirector::manual(None)
		.with_strategy(CompactionStrategy::Snapcompact)
		.before_inference(&mut cx, &request)
		.await
		.expect("snapcompact");
	assert_eq!(prepared, Prepared::Rebuild);
	assert_eq!(session.dom().count("compaction").expect("selector"), 1);

	let (_, after) = CanonicalPromptSource
		.banded_render(session.dom())
		.expect("bands after");
	assert_eq!(before, after, "an archive never reaches a system band");
	let thread = omp_agent::project_thread_with_attachments(session.dom(), session.blobs())
		.expect("frames resolve");
	let Some(omp_proto::thread::v1::item::Kind::Message(summary)) = thread[0].kind.as_ref() else {
		panic!("the archive projects as the leading thread message");
	};
	assert!(summary.parts.len() >= 2, "the note is followed by at least one frame");
}

/// Tool-roster stability (ADR 0024 rule 2).
///
/// The `tools` array is the first thing in every provider's cached prefix, so
/// a roster that changes mid-session misses the cache from the first byte.
/// These proofs run a real kernel over a scripted inference service and
/// compare the `tools` array the service received, request by request.
mod roster {
	use std::{
		collections::VecDeque,
		future::Future,
		ops::Range,
		sync::{
			Arc,
			atomic::{AtomicBool, Ordering},
		},
	};

	use async_stream::stream;
	use bytes::Bytes;
	use omp_agent::{
		DirectorRegistry, DirectorStack, DispatchPolicy, GateDecision, HookGate, HookPatch,
		HookPhase, Inference, Kernel, OnFailure, RouteFacts, RunControl, RuntimeFlags, SessionTool,
		SessionToolCx, SessionToolFuture, SourceRef, StaticPrompt, TurnInput, When,
	};
	pub use omp_agent::{
		SV_TOOLS,
		directors::{goal::Goal, plan::Plan, vibe::Vibe},
	};
	pub use omp_ai::settings::AI_EXTERNAL_THINKING as THINK;
	use omp_ai::{
		BlockKind, ChatEvent, ChatRequest, ChatStream, Completion, ExecutionReceipt, FinishReason,
		ToolCall, Usage, call::OpaqueJson,
	};
	use omp_con::Ctx;
	use omp_core::{Hash32, Str};
	use omp_e2e::support::{CapturedRequests, create_session, scripted_stream};
	use omp_journal::blob::BlobStore;
	use omp_proto::toolhost::v1::HookEventId;
	use omp_session::Session;
	use omp_tool::{
		Claims, Constraint, Effects, Ev, Fallback, IncomingParams, Part, Precedence, Presentation,
		PromptCaps, Registry, Rev, Tool, ToolSpec, ToolTerminal,
	};
	use parking_lot::Mutex;
	use serde_json::Value;

	pub const PLAN_FILE: &str = "local://PLAN.md";

	/// A tool that answers every call.
	struct Stub(ToolSpec);

	impl Stub {
		fn new(name: &'static str, strict: bool) -> Self {
			let schema = serde_json::to_vec(&serde_json::json!({
				"type": "object",
				"properties": { "path": { "type": "string" } },
				"required": [],
				"additionalProperties": false,
			}))
			.expect("schema");
			Self(ToolSpec {
				name:            Str::new_static(name),
				rev:             Rev { family: Str::new_static("e2e"), n: 1 },
				description:     Str::new_static("roster stability proof"),
				schema:          Bytes::from(schema),
				constraint:      if strict {
					Constraint::Schema { priority: 100, on_unsupported: Fallback::Unspecified }
				} else {
					Constraint::None
				},
				effects:         Effects::empty(),
				confinement:     omp_tool::Confinement::Host,
				projection_code: [1; 32],
			})
		}
	}

	impl Tool for Stub {
		type Fault = Value;
		type Params = Value;
		type Payload = Value;
		type Update = Value;

		fn spec(&self) -> &ToolSpec {
			&self.0
		}

		fn call<'c>(
			&'c self,
			mut params: IncomingParams<'c>,
		) -> impl futures::Stream<Item = Ev<Value, Value, Value>> + Send + 'c {
			stream! {
				let _ = params.committed().await;
				yield Ev::Done(ToolTerminal::Done { result: Ok(serde_json::json!({"ok": true})), useless: false });
			}
		}

		fn prompt(&self, _view: Result<&Value, &Value>, _caps: &PromptCaps) -> Vec<Part> {
			vec![Part::Text { text: Str::new_static("ok") }]
		}
	}

	/// `task` at the recursion ceiling: the session tool whose declaration the
	/// session may withhold.
	struct Withholding {
		spec:       ToolSpec,
		advertised: Arc<AtomicBool>,
	}

	impl SessionTool for Withholding {
		fn spec(&self) -> &ToolSpec {
			&self.spec
		}

		fn call<'a>(
			&'a self,
			_cx: SessionToolCx<'a>,
			_args: Box<serde_json::value::RawValue>,
		) -> SessionToolFuture<'a> {
			Box::pin(async move {
				Ok(omp_tool::CallOutcome::Ok(serde_json::value::to_raw_value("ok").expect("raw")))
			})
		}

		fn advertised(&self) -> bool {
			self.advertised.load(Ordering::SeqCst)
		}
	}

	type Scripts = Arc<Mutex<VecDeque<Vec<ChatEvent>>>>;
	type Before = Arc<Mutex<VecDeque<Box<dyn FnOnce() + Send>>>>;

	/// Scripted inference whose route can change and whose requests can be
	/// followed by a host write that lands while the request streams.
	pub struct RigInference {
		scripts:  Scripts,
		requests: CapturedRequests,
		route:    Arc<Mutex<RouteFacts>>,
		before:   Before,
	}

	impl Inference for RigInference {
		fn chat(
			&mut self,
			request: ChatRequest,
		) -> impl Future<Output = Result<ChatStream, omp_ai::Error>> + Send {
			self.requests.lock().push(request);
			if let Some(write) = self.before.lock().pop_front() {
				write();
			}
			let events = self
				.scripts
				.lock()
				.pop_front()
				.expect("one script per inference request");
			std::future::ready(Ok(scripted_stream(events)))
		}

		fn route_facts(&self) -> Option<RouteFacts> {
			Some(self.route.lock().clone())
		}
	}

	pub fn completed(reason: FinishReason, blocks: u32) -> ChatEvent {
		ChatEvent::Completed(Completion {
			reason,
			blocks,
			usage: Usage::default(),
			receipt: ExecutionReceipt::default().into(),
		})
	}

	pub fn text(text: &'static str) -> Vec<ChatEvent> {
		vec![
			ChatEvent::BlockStarted { index: 0, kind: BlockKind::Text },
			ChatEvent::TextDelta { index: 0, text: Str::new_static(text) },
			completed(FinishReason::Stop, 1),
		]
	}

	pub fn call(id: &'static str, name: &'static str, arguments: Value) -> Vec<ChatEvent> {
		let call = ToolCall {
			id:        id.into(),
			name:      Str::new_static(name),
			arguments: OpaqueJson::new(arguments.clone()),
		};
		vec![
			ChatEvent::BlockStarted { index: 0, kind: BlockKind::ToolCall },
			ChatEvent::ToolCallStarted { index: 0, id: call.id.clone(), name: call.name.clone() },
			ChatEvent::ToolArgumentsDelta {
				index: 0,
				bytes: Bytes::from(serde_json::to_vec(&arguments).expect("args encode")),
			},
			ChatEvent::ToolCallReady { index: 0, call },
			completed(FinishReason::ToolCalls, 1),
		]
	}

	/// What a rig is composed with.
	#[derive(Default)]
	pub struct Setup {
		/// `ai_external_thinking` at composition.
		pub think:      bool,
		/// `sv_tools` at composition (`--tools`, agent cfg).
		pub sv_tools:   Vec<&'static str>,
		/// Plan already engaged when the first request leaves.
		pub plan_first: bool,
	}

	/// A real kernel over the scripted inference, plus the knobs a session
	/// has: the control plane, the director stack, the route, and `task`.
	pub struct Rig {
		pub kernel:          Kernel<RigInference>,
		pub session:         Session,
		pub con:             Arc<Ctx>,
		pub requests:        CapturedRequests,
		scripts:             Scripts,
		pub before:          Before,
		pub route:           Arc<Mutex<RouteFacts>>,
		pub task_advertised: Arc<AtomicBool>,
		pub hook_narrows:    Arc<AtomicBool>,
		/// `toolset_changed` the `turn_start` hook was told, per request.
		pub toolset_changed: Arc<Mutex<Vec<bool>>>,
		pub trace:           Vec<(&'static str, Range<usize>)>,
		_temp:               tempfile::TempDir,
		responder:           tokio::task::JoinHandle<()>,
	}

	impl Drop for Rig {
		fn drop(&mut self) {
			self.responder.abort();
		}
	}

	fn turn_start_subscription() -> omp_agent::hooks::Subscription {
		omp_agent::hooks::Subscription {
			host:       Str::new_static("e2e"),
			source:     SourceRef {
				layer:        0,
				publisher:    Str::new_static("e2e"),
				extension_id: Str::new_static("roster"),
			},
			id:         1,
			event:      HookEventId::HookEventTurnStart,
			phase:      HookPhase::Transform,
			order:      0,
			on_failure: OnFailure::Deny,
			when:       When::default(),
		}
	}

	fn strict_route(strict: bool) -> RouteFacts {
		RouteFacts { strict_schema: strict, ..RouteFacts::default() }
	}

	impl Rig {
		pub fn new(setup: Setup) -> Self {
			let temp = tempfile::tempdir().expect("roster scratch");
			let claims = || Claims {
				precedence: Precedence::CORE,
				claimant:   Str::new_static("omp-e2e"),
				replaces:   None,
			};
			let mut registry = Registry::new();
			for (name, strict) in [
				("read", true),
				("grep", false),
				("edit", false),
				("write", false),
				("ask", false),
				("task", false),
			] {
				registry
					.register(Stub::new(name, strict), Presentation::Slot, claims())
					.expect("tool registers");
			}
			for name in ["think", "goal"] {
				registry
					.register(Stub::new(name, false), Presentation::Hidden, claims())
					.expect("hidden tool registers");
			}
			let con = Arc::new(Ctx::new());
			if setup.think {
				omp_ai::settings::AI_EXTERNAL_THINKING
					.set(&con, true)
					.expect("think on at composition");
			}
			if !setup.sv_tools.is_empty() {
				omp_agent::SV_TOOLS
					.set(
						&con,
						setup
							.sv_tools
							.iter()
							.copied()
							.map(Str::new_static)
							.collect(),
					)
					.expect("composition allowlist");
			}
			let requests = CapturedRequests::default();
			let scripts = Scripts::default();
			let before = Before::default();
			let route = Arc::new(Mutex::new(strict_route(true)));
			let inference = RigInference {
				scripts:  Arc::clone(&scripts),
				requests: Arc::clone(&requests),
				route:    Arc::clone(&route),
				before:   Arc::clone(&before),
			};
			let (gate, receiver) = HookGate::channel();
			let gate = Arc::new(gate);
			gate
				.subscribe("e2e", [turn_start_subscription()])
				.expect("subscription");
			let hook_narrows = Arc::new(AtomicBool::new(false));
			let toolset_changed = Arc::new(Mutex::new(Vec::new()));
			let responder = {
				let gate = Arc::clone(&gate);
				let narrows = Arc::clone(&hook_narrows);
				let changed = Arc::clone(&toolset_changed);
				tokio::spawn(async move {
					while let Ok(dispatch) = receiver.recv_async().await {
						// The receiver also carries lossy Observe notifications (the
						// echo of this very event, and every other lifecycle event);
						// only the gate's Transform dispatch is one `turn_start` per
						// request, and only it takes an answer.
						if dispatch.event != HookEventId::HookEventTurnStart
							|| dispatch.phase != HookPhase::Transform
						{
							continue;
						}
						let mut payload: Value =
							serde_json::from_slice(&dispatch.payload).expect("hook payload");
						changed.lock().push(
							payload["toolset_changed"]
								.as_bool()
								.expect("toolset_changed"),
						);
						let decision = if narrows.load(Ordering::SeqCst) {
							payload["enabled_tools"] = serde_json::json!(["read"]);
							GateDecision::Modify(HookPatch {
								target: None,
								args:   Some(Bytes::from(serde_json::to_vec(&payload).expect("patch"))),
							})
						} else {
							GateDecision::Defer
						};
						let _ = gate.answer(dispatch.dispatch_id, vec![(1, decision)]);
					}
				})
			};
			let task_advertised = Arc::new(AtomicBool::new(true));
			let kernel = Kernel::new(
				inference,
				Arc::new(registry),
				DispatchPolicy::new(BlobStore::open(temp.path().join("blobs")).expect("blob store")),
				StaticPrompt(Str::new_static("roster")),
			)
			.with_director_registry(DirectorRegistry::standard())
			.with_con_context(Arc::clone(&con))
			.with_hook_gate(gate)
			.with_runtime_flags(RuntimeFlags {
				automatic_compaction:     false,
				goal_enabled:             true,
				autolearn_enabled:        false,
				autolearn_min_tool_calls: 5,
				recover_inline_edits:     false,
			})
			.with_session_tool(Arc::new(Withholding {
				spec:       Stub::new("task", false).0,
				advertised: Arc::clone(&task_advertised),
			}));
			let session = create_session(&temp.path().join("roster.oms")).expect("session");
			let mut rig = Self {
				kernel,
				session,
				con,
				requests,
				scripts,
				before,
				route,
				task_advertised,
				hook_narrows,
				toolset_changed,
				trace: Vec::new(),
				_temp: temp,
				responder,
			};
			if setup.plan_first {
				rig.engage(Box::new(Plan::new(PLAN_FILE)));
			}
			rig
		}

		fn stack(&self) -> DirectorStack {
			DirectorStack::from_dom(self.session.dom(), &DirectorRegistry::standard())
		}

		pub fn engage(&mut self, director: Box<dyn omp_agent::Director>) {
			self
				.stack()
				.engage(&mut self.session, director)
				.expect("director engages");
		}

		pub fn exit(&mut self, family: &str) {
			self
				.stack()
				.exit(&mut self.session, family)
				.expect("director exits");
		}

		pub fn pause(&mut self, family: &str) {
			self
				.stack()
				.pause(&mut self.session, family)
				.expect("director pauses");
		}

		pub fn resume(&mut self, family: &str) {
			self
				.stack()
				.resume(&mut self.session, family)
				.expect("director resumes");
		}

		/// Runs one turn over `scripts` (one per request) and records which
		/// requests it produced under `label`.
		pub async fn turn(&mut self, label: &'static str, scripts: Vec<Vec<ChatEvent>>) {
			let start = self.requests.lock().len();
			self.scripts.lock().extend(scripts);
			self
				.kernel
				.run_turn(
					&mut self.session,
					TurnInput { text: Str::new_static("go"), attachments: Vec::new() },
					RunControl::default(),
				)
				.await
				.expect("turn");
			let end = self.requests.lock().len();
			self.trace.push((label, start..end));
		}

		/// Registers a host write that lands while the next request streams.
		pub fn write_during_next_request(&self, write: impl FnOnce() + Send + 'static) {
			self.before.lock().push_back(Box::new(write));
		}

		/// The fingerprint of request `index`'s `tools` array, as sent.
		pub fn fingerprint(&self, index: usize) -> Hash32 {
			Hash32::sum(format!("{:?}", self.requests.lock()[index].tools))
		}

		pub fn names(&self, index: usize) -> Vec<String> {
			self.requests.lock()[index]
				.tools
				.iter()
				.map(|tool| tool.name.to_string())
				.collect()
		}

		/// Asserts every request of the turns labelled `labels` carried exactly
		/// the `tools` bytes of `reference`.
		pub fn assert_constant(&self, reference: usize, labels: &[&str]) {
			let expected = self.fingerprint(reference);
			for (label, range) in &self.trace {
				if !labels.contains(label) {
					continue;
				}
				assert!(!range.is_empty(), "turn `{label}` sent no request");
				for index in range.clone() {
					assert_eq!(
						self.fingerprint(index),
						expected,
						"turn `{label}` request {index} changed the tools array (names {:?}, reference \
						 names {:?})",
						self.names(index),
						self.names(reference),
					);
				}
			}
		}

		/// The first request of the turn labelled `label`.
		pub fn first_request(&self, label: &str) -> usize {
			self
				.trace
				.iter()
				.find(|(name, _)| *name == label)
				.map(|(_, range)| range.start)
				.unwrap_or_else(|| panic!("no turn labelled `{label}`"))
		}

		/// Asserts the `turn_start` hook's `toolset_changed` told the truth about
		/// every request: true exactly when the `tools` array differs from the
		/// previous request's (always true for the first request).
		pub fn assert_toolset_changed_is_truthful(&self) {
			let told = self.toolset_changed.lock().clone();
			let total = self.requests.lock().len();
			assert_eq!(told.len(), total, "one turn_start per request");
			for index in 0..total {
				let changed = index == 0 || self.fingerprint(index) != self.fingerprint(index - 1);
				assert_eq!(told[index], changed, "toolset_changed at request {index}");
			}
		}

		/// The text the model received as tool results, after request `index`.
		pub fn messages_after(&self, index: usize) -> String {
			format!("{:?}", self.requests.lock()[index].messages)
		}
	}

	pub fn plan_turn() -> Vec<Vec<ChatEvent>> {
		vec![
			call("edit-1", "edit", serde_json::json!({"path": "src/lib.rs"})),
			call("write-plan", "write", serde_json::json!({"path": PLAN_FILE})),
			call("read-1", "read", serde_json::json!({})),
			call("ask-1", "ask", serde_json::json!({})),
			text("presented"),
		]
	}

	pub fn goal_op(id: &'static str, op: &str) -> Vec<ChatEvent> {
		call(id, "goal", serde_json::json!({"op": op}))
	}

	pub fn sv_tools(names: &[&'static str]) -> Vec<Str> {
		names.iter().copied().map(Str::new_static).collect()
	}
}

/// Every transition ADR 0024 lists leaves the `tools` array byte-identical:
/// Plan engage and exit, Vibe, `sv_tools` writes (between turns and between two
/// requests of one turn), `think` toggles, the `task` ceiling, and a
/// `turn_start` hook that narrows `enabled_tools`. A model switch re-lowers the
/// same roster once (the sanctioned boundary) and is byte-stable again after
/// it; the first user-initiated goal engagement mounts `goal` once and the
/// roster stays that way through completion, drop, pause, and resume.
#[tokio::test]
async fn p5_tool_roster_bytes_are_constant_across_session_transitions() {
	use roster::*;

	let mut rig = Rig::new(Setup::default());
	rig.turn("normal", vec![call("read-0", "read", serde_json::json!({})), text("done")])
		.await;
	let reference = 0;
	let wire = rig.names(reference);
	for expected in ["read", "grep", "edit", "write", "ask", "task"] {
		assert!(wire.iter().any(|name| name == expected), "{expected} is on the wire: {wire:?}");
	}
	for hidden in ["goal", "think"] {
		assert!(!wire.iter().any(|name| name == hidden), "{hidden} is not mounted: {wire:?}");
	}

	// Plan engages (its bind narrows `sv_tools` to the planning roster) and the
	// model calls `edit` anyway: the call is refused at dispatch and the refusal
	// reaches the model; `read` still runs.
	rig.engage(Box::new(Plan::new(PLAN_FILE)));
	rig.turn("plan", plan_turn()).await;
	let plan_requests = rig.trace.last().expect("plan turn").1.clone();
	assert!(
		rig.messages_after(plan_requests.start + 1)
			.contains("is not available while plan mode is active"),
		"the refusal reaches the model"
	);
	rig.exit("plan");
	rig.turn("after-plan", vec![text("back to normal")]).await;

	rig.engage(Box::new(Vibe::new()));
	rig.turn("vibe", vec![call("write-v", "write", serde_json::json!({})), text("coordinated")])
		.await;
	rig.exit("vibe");

	// A user `sv_tools` write between turns, then one landing between two
	// requests of a single turn.
	SV_TOOLS
		.set(&rig.con, sv_tools(&["read"]))
		.expect("user allowlist");
	rig.turn("sv-tools", vec![call("grep-1", "grep", serde_json::json!({})), text("narrow")])
		.await;
	SV_TOOLS
		.set(&rig.con, Vec::new())
		.expect("allowlist cleared");
	let con = Arc::clone(&rig.con);
	rig.write_during_next_request(move || {
		SV_TOOLS
			.set(&con, sv_tools(&["read", "grep"]))
			.expect("mid-turn allowlist");
	});
	rig.turn("sv-tools-mid-turn", vec![
		call("read-2", "read", serde_json::json!({})),
		call("edit-2", "edit", serde_json::json!({})),
		text("narrowed mid-turn"),
	])
	.await;
	SV_TOOLS
		.set(&rig.con, Vec::new())
		.expect("allowlist cleared");

	// `think` toggles on and off; the `task` ceiling flips; a hook narrows
	// `enabled_tools`.
	THINK.set(&rig.con, true).expect("think on");
	rig.turn("think-on", vec![text("thinking")]).await;
	THINK.set(&rig.con, false).expect("think off");
	rig.turn("think-off", vec![text("not thinking")]).await;
	rig.task_advertised.store(false, Ordering::SeqCst);
	rig.turn("task-ceiling", vec![text("ceiling")]).await;
	rig.hook_narrows.store(true, Ordering::SeqCst);
	rig.turn("hook", vec![call("grep-h", "grep", serde_json::json!({})), text("filtered")])
		.await;
	rig.hook_narrows.store(false, Ordering::SeqCst);
	rig.turn("settled", vec![text("settled")]).await;

	rig.assert_constant(reference, &[
		"normal",
		"plan",
		"after-plan",
		"vibe",
		"sv-tools",
		"sv-tools-mid-turn",
		"think-on",
		"think-off",
		"task-ceiling",
		"hook",
		"settled",
	]);

	// The model switch: the same names lower to different bytes on a route
	// without strict schemas, exactly once, and the new bytes then hold.
	rig.route.lock().strict_schema = false;
	rig.turn("switched", vec![
		call("read-3", "read", serde_json::json!({})),
		text("on the other model"),
	])
	.await;
	rig.turn("switched-again", vec![text("still there")]).await;
	let switched = rig.first_request("switched");
	assert_ne!(
		rig.fingerprint(switched),
		rig.fingerprint(reference),
		"a route with different capabilities re-lowers the roster"
	);
	let mut before = rig.names(reference);
	let mut after = rig.names(switched);
	before.sort_unstable();
	after.sort_unstable();
	assert_eq!(before, after, "a model switch changes the lowering, never the membership");
	rig.assert_constant(switched, &["switched", "switched-again"]);
	rig.route.lock().strict_schema = true;
	rig.turn("switched-back", vec![text("home again")]).await;
	rig.assert_constant(reference, &["switched-back"]);

	// The first user-initiated goal engagement mounts `goal` at the next turn
	// boundary; after that the roster never changes again.
	rig.engage(Box::new(Goal::new("finish", None)));
	rig.turn("goal-engaged", vec![goal_op("goal-complete", "complete"), text("done")])
		.await;
	let mounted = rig.first_request("goal-engaged");
	assert_ne!(rig.fingerprint(mounted), rig.fingerprint(reference), "goal mounts");
	assert_eq!(rig.names(mounted).last().map(String::as_str), Some("goal"));
	rig.turn("goal-completed", vec![text("after completion")])
		.await;
	rig.exit("goal");
	rig.engage(Box::new(Goal::new("second", None)));
	rig.turn("goal-dropped", vec![goal_op("goal-drop", "drop"), text("dropped")])
		.await;
	rig.engage(Box::new(Goal::new("third", None)));
	rig.pause("goal");
	rig.turn("goal-paused", vec![text("paused")]).await;
	rig.resume("goal");
	rig.turn("goal-resumed", vec![goal_op("goal-get", "get"), text("resumed")])
		.await;
	rig.engage(Box::new(Plan::new(PLAN_FILE)));
	rig.turn("goal-then-plan", plan_turn()).await;
	rig.exit("plan");
	rig.assert_constant(mounted, &[
		"goal-engaged",
		"goal-completed",
		"goal-dropped",
		"goal-paused",
		"goal-resumed",
		"goal-then-plan",
	]);

	rig.assert_toolset_changed_is_truthful();
}

/// Mounts and withholding decided at composition are latched: `think` on at
/// composition stays advertised after the setting is turned off, `task`
/// withheld at composition stays withheld after the ceiling moves, and a
/// session composed while Plan is already engaged still latches the full
/// roster (the Director's bind narrows dispatch, never the wire).
#[tokio::test]
async fn p5_composition_latched_mounts_survive_later_changes() {
	use roster::*;

	let mut rig = Rig::new(Setup { think: true, plan_first: true, ..Setup::default() });
	rig.task_advertised.store(false, Ordering::SeqCst);
	rig.turn("plan-first", plan_turn()).await;
	let wire = rig.names(0);
	assert!(
		wire.iter().any(|name| name == "edit"),
		"Plan's bind does not narrow the wire: {wire:?}"
	);
	assert!(wire.iter().any(|name| name == "think"), "think latched on at composition: {wire:?}");
	assert!(!wire.iter().any(|name| name == "task"), "task withheld at composition: {wire:?}");
	THINK.set(&rig.con, false).expect("think off");
	rig.task_advertised.store(true, Ordering::SeqCst);
	rig.exit("plan");
	rig.turn("later", vec![text("later")]).await;
	rig.assert_constant(0, &["plan-first", "later"]);
	rig.assert_toolset_changed_is_truthful();
}

/// A composition-time `sv_tools` (`--tools`, agent cfg) narrows the wire roster
/// and is final: later user writes narrow or widen only what dispatch allows.
#[tokio::test]
async fn p5_composition_sv_tools_narrowing_is_final() {
	use roster::*;

	let mut rig = Rig::new(Setup { sv_tools: vec!["read", "grep"], ..Setup::default() });
	rig.turn("narrow", vec![text("one")]).await;
	let mut wire = rig.names(0);
	wire.sort_unstable();
	assert_eq!(wire, ["grep", "read"]);
	SV_TOOLS
		.set(&rig.con, sv_tools(&["read", "grep", "edit", "write"]))
		.expect("widening write");
	rig.turn("widened", vec![text("two")]).await;
	rig.assert_constant(0, &["narrow", "widened"]);
}
