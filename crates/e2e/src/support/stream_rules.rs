//! The one stream rule the redirect proofs (ADR 0038) share.

use std::sync::Arc;

use omp_agent::{
	DirectorRegistry,
	directors::stream_rules::{RulePattern, StreamRuleSet, StreamRules},
};
use omp_ai::{BlockKind, ChatEvent, Completion, ExecutionReceipt, FinishReason, Usage};
use omp_core::Str;

/// The standard Director registry plus a rule that fires on any text response
/// containing `danger`. The rule only acts once a session engages the
/// `stream-rules` family.
#[must_use]
pub fn guard_directors() -> DirectorRegistry {
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
		.expect("the guard rule compiles");
	let mut directors = DirectorRegistry::standard();
	directors.register_extension(Box::new(StreamRules::new(Arc::new(set))));
	directors
}

/// One complete text response, as a canonical event script.
#[must_use]
pub fn text_response(text: &'static str) -> Vec<ChatEvent> {
	vec![
		ChatEvent::BlockStarted { index: 0, kind: BlockKind::Text },
		ChatEvent::TextDelta { index: 0, text: Str::new_static(text) },
		ChatEvent::Completed(Completion {
			reason:  FinishReason::Stop,
			blocks:  1,
			usage:   Usage::default(),
			receipt: ExecutionReceipt::default().into(),
		}),
	]
}
