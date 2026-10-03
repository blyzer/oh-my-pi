//! Incremental stream-condition matching for discovered rule documents.

use omp_ai::{ChatRequest, ToolCall, ToolCallId};
use omp_core::Str;
use regex_automata::{Input, MatchKind, hybrid::dfa::DFA};

use crate::director::{
	BindValue, Director, DirectorCx, PartialOutput, StateUpdate, StreamEffect, StreamFragment,
	StreamInterrupt, StreamSource, StreamVerdict, StreamWatch,
};

/// Registry family for the stream-rules Director.
pub const FAMILY: &str = "stream-rules";

/// One compiled condition and its rule body.
#[derive(Clone, Debug)]
pub struct RulePattern {
	/// Rule identifier shown in notices.
	pub name:    Str,
	/// Model-visible rule body.
	pub body:    Str,
	/// Regex source.
	pub pattern: Str,
}

/// Compiled multi-pattern matcher shared by request-scoped watchers.
pub struct StreamRuleSet {
	dfa:   DFA,
	rules: Vec<RulePattern>,
}

impl StreamRuleSet {
	/// Compiles valid conditions, silently skipping invalid patterns.
	#[must_use]
	pub fn compile(rules: Vec<RulePattern>) -> Option<Self> {
		let mut valid = Vec::with_capacity(rules.len());
		for (index, rule) in rules.into_iter().enumerate() {
			if DFA::builder().build(&rule.pattern).is_ok() {
				valid.push(rule);
			} else {
				tracing::warn!(rule = %rule.name, pattern_index = index, "invalid stream rule regex was skipped");
			}
		}
		let patterns = valid
			.iter()
			.map(|rule| rule.pattern.as_str())
			.collect::<Vec<_>>();
		if patterns.is_empty() {
			return None;
		}
		let dfa = DFA::builder()
			.configure(DFA::config().match_kind(MatchKind::All))
			.build_many(&patterns)
			.inspect_err(|error| tracing::warn!(%error, "stream rule matcher compilation failed"))
			.ok()?;
		Some(Self { dfa, rules: valid })
	}
}

/// Director backed by the active session's compiled rule set.
pub struct StreamRules {
	set: Option<std::sync::Arc<StreamRuleSet>>,
}

impl StreamRules {
	/// Creates a Director over one immutable compiled set.
	#[must_use]
	pub const fn new(set: std::sync::Arc<StreamRuleSet>) -> Self {
		Self { set: Some(set) }
	}
}

impl Director for StreamRules {
	fn id(&self) -> &'static str {
		FAMILY
	}

	fn state(&self) -> Vec<(Str, BindValue)> {
		vec![(Str::new_static("compiled"), BindValue::Bool(self.set.is_some()))]
	}

	fn watch_stream(&self, cx: &DirectorCx<'_>, _req: &ChatRequest) -> Option<Box<dyn StreamWatch>> {
		let set = self.set.as_ref()?.clone();
		let fired = set
			.rules
			.iter()
			.enumerate()
			.filter_map(|(index, rule)| {
				matches!(cx.state(&format!("fired.{}", rule.name)), Some(omp_dom::Value::Bool(true)))
					.then_some(index)
			})
			.collect();
		Some(Box::new(Watch::new(set, fired)))
	}
}

type LazyState = impl Copy;

#[define_opaque(LazyState)]
fn start_state(dfa: &DFA, cache: &mut regex_automata::hybrid::dfa::Cache) -> LazyState {
	dfa.start_state_forward(cache, &Input::new(&[]))
		.expect("empty start state")
}

#[define_opaque(LazyState)]
fn advance(
	dfa: &DFA,
	cache: &mut regex_automata::hybrid::dfa::Cache,
	state: LazyState,
	byte: u8,
) -> (LazyState, Option<usize>) {
	let state = dfa
		.next_state(cache, state, byte)
		.expect("DFA cache can grow for each byte");
	let matched = state
		.is_match()
		.then(|| dfa.match_pattern(cache, state, 0).as_usize());
	(state, matched)
}

struct Cursor {
	cache: regex_automata::hybrid::dfa::Cache,
	state: LazyState,
}

struct Watch {
	set:     std::sync::Arc<StreamRuleSet>,
	cursors: omp_core::FastHashMap<(u8, u32), Cursor>,
	fired:   omp_core::FastHashSet<usize>,
}

impl Watch {
	fn new(set: std::sync::Arc<StreamRuleSet>, fired: omp_core::FastHashSet<usize>) -> Self {
		Self { cursors: omp_core::FastHashMap::default(), fired, set }
	}

	fn scan(&mut self, key: (u8, u32), bytes: &[u8], source: StreamSource<'_>) -> StreamVerdict {
		let cursor = self.cursors.entry(key).or_insert_with(|| {
			let mut cache = self.set.dfa.create_cache();
			let state = start_state(&self.set.dfa, &mut cache);
			Cursor { cache, state }
		});
		for byte in bytes {
			let (state, matches) = advance(&self.set.dfa, &mut cursor.cache, cursor.state, *byte);
			cursor.state = state;
			if let Some(rule_index) = matches {
				if self.fired.insert(rule_index) {
					return self.hit(rule_index, source);
				}
			}
		}
		StreamVerdict::Pass
	}

	fn hit(&self, index: usize, source: StreamSource<'_>) -> StreamVerdict {
		let rule = &self.set.rules[index];
		let update =
			StateUpdate::new(Str::new(format!("fired.{}", rule.name)), BindValue::Bool(true));
		let effect = StreamEffect {
			updates:     vec![update],
			developer:   Some(rule.body.clone()),
			notice:      None,
			notice_name: None,
			call_diags:  Vec::new(),
		};
		let culprit = match source {
			StreamSource::ToolArgs { call_id, .. } => Some(ToolCallId::from(call_id)),
			_ => None,
		};
		StreamVerdict::Interrupt(StreamInterrupt {
			culprit,
			partial: PartialOutput::Discard,
			label: Str::new(format!("stream rule {}", rule.name)),
			effect,
		})
	}
}

impl StreamWatch for Watch {
	fn fragment(&mut self, fragment: StreamFragment<'_>) -> StreamVerdict {
		if matches!(fragment.source, StreamSource::Thinking) {
			return StreamVerdict::Pass;
		}
		let key = match fragment.source {
			StreamSource::Text => (0, fragment.index),
			StreamSource::Thinking => (1, fragment.index),
			StreamSource::ToolArgs { .. } => (2, fragment.index),
		};
		self.scan(key, fragment.bytes, fragment.source)
	}

	fn call_ready(&mut self, _index: u32, _call: &ToolCall) -> StreamVerdict {
		StreamVerdict::Pass
	}
}
