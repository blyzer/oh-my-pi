//! P5: semantic prompt bands keep stable prefixes cacheable.

use std::sync::Arc;

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
