//! Incremental stream-condition matching for discovered rule documents.
//!
//! [`StreamRuleSet`] is the one matcher stream rules run on. The
//! [`StreamRules`] Director watches live responses through it, and
//! [`StreamRuleSet::probe`] runs the same compiled automaton over a finished
//! text for inspection surfaces (`omp rules test|scan`), so an offline answer
//! never comes from a second matcher.

use std::{ops::ControlFlow, str::FromStr};

use omp_ai::{ChatRequest, ToolCall, ToolCallId};
use omp_core::{Str, sf};
use omp_proto::toolhost::v1::HookEventId;
use regex_automata::{Input, MatchKind, hybrid::dfa::DFA};

use crate::{
	director::{
		BindValue, Director, DirectorCx, PartialOutput, StateUpdate, StreamEffect, StreamFragment,
		StreamInterrupt, StreamObservation, StreamSource, StreamVerdict, StreamWatch,
	},
	vars::{
		AI_STREAM_RULES_CONTEXT, AI_STREAM_RULES_DISABLED, AI_STREAM_RULES_ENABLED,
		AI_STREAM_RULES_INTERRUPT, AI_STREAM_RULES_REPEAT, AI_STREAM_RULES_REPEAT_GAP,
		StreamRuleContext, StreamRuleInterrupt, StreamRuleRepeat,
	},
};

/// Registry family for the stream-rules Director.
pub const FAMILY: &str = "stream-rules";

/// `name` of the host-visible `<notice>` a stream-rule redirect journals.
pub const NOTICE_NAME: &str = "stream-rule";

/// The `stream_rule_triggered` lifecycle observation for one hit; the loop
/// stamps the session, turn, sequence, and `interrupted` facts after commit.
fn observation(
	rule: &RulePattern,
	source: StreamSource<'_>,
	call_id: Option<&ToolCallId>,
) -> StreamObservation {
	let mut payload = serde_json::Map::new();
	payload.insert("rule".to_owned(), serde_json::Value::from(rule.name.as_str()));
	payload.insert("matched".to_owned(), serde_json::Value::from(rule.pattern.as_str()));
	payload.insert("source".to_owned(), serde_json::Value::from(<&'static str>::from(source)));
	payload.insert(
		"call_id".to_owned(),
		call_id.map_or(serde_json::Value::Null, |id| serde_json::Value::from(id.as_str())),
	);
	StreamObservation { event: HookEventId::HookEventStreamRuleTriggered, payload }
}

/// One compiled condition and its rule body.
#[derive(Clone, Debug)]
pub struct RulePattern {
	/// Rule identifier shown in notices.
	pub name:           Str,
	/// Model-visible rule body.
	pub body:           Str,
	/// Regex source.
	pub pattern:        Str,
	/// Frontmatter stream-source selectors.
	pub scope:          Vec<Str>,
	/// Frontmatter path gates.
	pub globs:          Vec<Str>,
	/// Optional per-rule interrupt-policy override.
	pub interrupt_mode: Option<Str>,
}

/// Why compiling a rule set left a rule, a condition, or a selector out.
///
/// Every warning names the rule it came from; the rest of the set still
/// compiles.
#[derive(Debug, thiserror::Error)]
pub enum StreamRuleWarning {
	/// The streaming matcher rejects one condition (look-around beyond
	/// `^ $ \A \z \b`, backreferences, Unicode word boundaries, or invalid
	/// syntax); the rule's other conditions still load.
	#[error(
		"stream rule `{rule}` condition {index} `{pattern}` does not compile for the streaming \
		 matcher"
	)]
	Condition {
		/// Rule name.
		rule:    Str,
		/// Zero-based condition index within the rule.
		index:   usize,
		/// Rejected condition source.
		pattern: Str,
		/// Matcher build failure.
		#[source]
		source:  regex_automata::hybrid::BuildError,
	},
	/// A frontmatter `globs` entry is invalid, so no path can satisfy the
	/// rule's path gate.
	#[error(
		"stream rule `{rule}` path glob `{glob}` is invalid; the rule can never match a tool call"
	)]
	PathGlob {
		/// Rule name.
		rule:   Str,
		/// Rejected glob.
		glob:   Str,
		/// Glob parse failure.
		#[source]
		source: globset::Error,
	},
	/// A `tool:<name>(<glob>)` scope carries an invalid glob and was dropped.
	#[error("stream rule `{rule}` scope `{token}` has an invalid path glob and was ignored")]
	ScopeGlob {
		/// Rule name.
		rule:   Str,
		/// Rejected scope token.
		token:  Str,
		/// Glob parse failure.
		#[source]
		source: globset::Error,
	},
	/// A scope token names no stream source and was dropped.
	#[error(
		"stream rule `{rule}` scope `{token}` is not text, thinking, tool, or tool:<name>; it was \
		 ignored"
	)]
	UnknownScope {
		/// Rule name.
		rule:  Str,
		/// Rejected scope token.
		token: Str,
	},
	/// Every scope token was dropped, so the rule watches no stream.
	#[error("stream rule `{rule}` has no usable scope and can never match")]
	Unreachable {
		/// Rule name.
		rule: Str,
	},
	/// The `interruptMode` override is not a known policy; the session
	/// policy applies instead.
	#[error("stream rule `{rule}` interruptMode `{mode}` is invalid and was ignored")]
	InterruptMode {
		/// Rule name.
		rule:   Str,
		/// Rejected value.
		mode:   Str,
		/// Policy parse failure.
		#[source]
		source: strum::ParseError,
	},
	/// The combined automaton over every valid condition failed to build, so
	/// no stream rule is active.
	#[error("the stream rule matcher failed to compile; no stream rule is active")]
	Matcher {
		/// Matcher build failure.
		#[source]
		source: regex_automata::hybrid::BuildError,
	},
}

#[derive(Clone)]
enum Scope {
	Text,
	Thinking,
	Tool(Option<Str>, Option<globset::GlobMatcher>),
}

struct CompiledRule {
	pattern:    RulePattern,
	scopes:     Vec<Scope>,
	globs:      Vec<globset::GlobMatcher>,
	path_valid: bool,
	interrupt:  Option<StreamRuleInterrupt>,
}

/// Compiled multi-pattern matcher shared by request-scoped watchers.
pub struct StreamRuleSet {
	dfa:   DFA,
	rules: Vec<CompiledRule>,
}

/// The outcome of [`StreamRuleSet::compile`]: the set, when any condition
/// compiled, and every diagnostic compilation produced.
pub struct CompiledStreamRules {
	/// The compiled set; `None` when no condition compiled.
	pub set:      Option<StreamRuleSet>,
	/// Conditions, globs, and selectors left out, in input order.
	pub warnings: Vec<StreamRuleWarning>,
}

/// One rule a [`StreamRuleSet::probe`] would have triggered.
#[derive(Clone, Copy, Debug)]
pub struct ProbeHit<'s> {
	/// The first condition of the rule that matched.
	pub rule:       &'s RulePattern,
	/// Byte offset in the probed text where that match ended.
	pub end:        usize,
	/// Whether the effective interrupt policy redirects the response for
	/// this hit (`false`: the Director records a note instead).
	pub interrupts: bool,
}

/// Whether `pattern` compiles for the streaming matcher, which is what makes
/// a rule condition a stream condition.
#[must_use]
pub fn condition_compiles(pattern: &str) -> bool {
	DFA::builder().build(pattern).is_ok()
}

impl StreamRuleSet {
	/// Compiles every condition the streaming matcher accepts. Rejected
	/// conditions, globs, selectors, and overrides become typed warnings and
	/// the rest of the set still loads.
	#[must_use]
	pub fn compile(rules: Vec<RulePattern>) -> CompiledStreamRules {
		let mut warnings = Vec::new();
		let mut valid = Vec::with_capacity(rules.len());
		let mut condition_index = 0_usize;
		let mut previous: Option<Str> = None;
		for rule in rules {
			if previous.as_ref() == Some(&rule.name) {
				condition_index += 1;
			} else {
				condition_index = 0;
				previous = Some(rule.name.clone());
			}
			let report = condition_index == 0;
			let mut rule_warnings = Vec::new();
			let scopes = parse_scopes(&rule.name, &rule.scope, &mut rule_warnings);
			if scopes.is_empty() {
				rule_warnings.push(StreamRuleWarning::Unreachable { rule: rule.name.clone() });
			}
			let mut path_valid = true;
			let mut globs = Vec::with_capacity(rule.globs.len());
			for glob in &rule.globs {
				match globset::Glob::new(glob) {
					Ok(glob) => globs.push(glob.compile_matcher()),
					Err(source) => {
						path_valid = false;
						rule_warnings.push(StreamRuleWarning::PathGlob {
							rule: rule.name.clone(),
							glob: glob.clone(),
							source,
						});
					},
				}
			}
			let interrupt = rule.interrupt_mode.as_deref().and_then(|mode| {
				StreamRuleInterrupt::from_str(mode)
					.map_err(|source| {
						rule_warnings.push(StreamRuleWarning::InterruptMode {
							rule: rule.name.clone(),
							mode: Str::new(mode),
							source,
						});
					})
					.ok()
			});
			// Rule-level diagnostics repeat for every condition of the rule;
			// report them once, with its first condition.
			if report {
				warnings.append(&mut rule_warnings);
			}
			match DFA::builder().build(&rule.pattern) {
				Ok(_) => {
					valid.push(CompiledRule { pattern: rule, scopes, globs, path_valid, interrupt });
				},
				Err(source) => warnings.push(StreamRuleWarning::Condition {
					rule: rule.name.clone(),
					index: condition_index,
					pattern: rule.pattern.clone(),
					source,
				}),
			}
		}
		let patterns = valid
			.iter()
			.map(|rule| rule.pattern.pattern.as_str())
			.collect::<Vec<_>>();
		if patterns.is_empty() {
			return CompiledStreamRules { set: None, warnings };
		}
		let set = match DFA::builder()
			.configure(DFA::config().match_kind(MatchKind::All))
			.build_many(&patterns)
		{
			Ok(dfa) => Some(Self { dfa, rules: valid }),
			Err(source) => {
				warnings.push(StreamRuleWarning::Matcher { source });
				None
			},
		};
		CompiledStreamRules { set, warnings }
	}

	/// The compiled conditions, one per accepted condition, in input order.
	pub fn patterns(
		&self,
	) -> impl ExactSizeIterator<Item = &RulePattern>
	+ DoubleEndedIterator
	+ std::iter::FusedIterator
	+ Clone
	+ '_ {
		self.rules.iter().map(|rule| &rule.pattern)
	}

	/// Runs `text` through the same automaton, scope, and path checks the
	/// Director's watcher uses, as one complete block from `source` (for a
	/// tool source: one authored-text segment written to `paths`).
	///
	/// Returns at most one hit per rule name, in match order, skipping rules
	/// named in `disabled`. `policy` is the session interrupt policy; each
	/// rule's `interruptMode` override wins over it, as it does live.
	#[must_use]
	pub fn probe(
		&self,
		source: StreamSource<'_>,
		paths: &[Str],
		text: &[u8],
		policy: StreamRuleInterrupt,
		disabled: &[Str],
	) -> Vec<ProbeHit<'_>> {
		let mut hits = Vec::<ProbeHit<'_>>::new();
		let mut record = |rule_index: usize, end: usize| {
			let rule = &self.rules[rule_index];
			if admits(rule, source, paths)
				&& !disabled.contains(&rule.pattern.name)
				&& !hits.iter().any(|hit| hit.rule.name == rule.pattern.name)
			{
				hits.push(ProbeHit {
					rule: &rule.pattern,
					end,
					interrupts: interrupts(rule.interrupt.unwrap_or(policy), source),
				});
			}
			ControlFlow::<()>::Continue(())
		};
		let mut cursor = Cursor::start(&self.dfa);
		let _ = feed(&self.dfa, &mut cursor, text, &mut record);
		for rule_index in finish_state(&self.dfa, &mut cursor) {
			let _ = record(rule_index, text.len());
		}
		hits
	}
}

/// Whether `policy` redirects the response for a hit from `source`.
const fn interrupts(policy: StreamRuleInterrupt, source: StreamSource<'_>) -> bool {
	let tool_source = matches!(source, StreamSource::ToolArgs { .. });
	match policy {
		StreamRuleInterrupt::Always => true,
		StreamRuleInterrupt::ProseOnly => !tool_source,
		StreamRuleInterrupt::ToolOnly => tool_source,
		StreamRuleInterrupt::Never => false,
	}
}

fn parse_scopes(rule: &Str, scopes: &[Str], warnings: &mut Vec<StreamRuleWarning>) -> Vec<Scope> {
	if scopes.is_empty() {
		return vec![Scope::Text, Scope::Tool(None, None)];
	}
	let mut parsed = Vec::new();
	let tokens = scopes
		.iter()
		.flat_map(|scope| scope.split(","))
		.map(|scope| scope.trim())
		.filter(|scope| !scope.is_empty());
	for token in tokens {
		let lower = token.to_ascii_lowercase();
		match lower.as_str() {
			"text" => parsed.push(Scope::Text),
			"thinking" => parsed.push(Scope::Thinking),
			"tool" | "toolcall" => parsed.push(Scope::Tool(None, None)),
			_ if lower.starts_with("tool:") => {
				let selector = &token.as_str()[5..];
				if let Some((name, glob)) = selector.strip_suffix(')').and_then(|s| s.rsplit_once('('))
				{
					match globset::Glob::new(glob) {
						Ok(glob) => {
							parsed.push(Scope::Tool(Some(Str::new(name)), Some(glob.compile_matcher())));
						},
						Err(source) => warnings.push(StreamRuleWarning::ScopeGlob {
							rule: rule.clone(),
							token,
							source,
						}),
					}
				} else if !selector.is_empty() {
					parsed.push(Scope::Tool(Some(Str::new(selector)), None));
				} else {
					warnings.push(StreamRuleWarning::UnknownScope { rule: rule.clone(), token });
				}
			},
			_ => warnings.push(StreamRuleWarning::UnknownScope { rule: rule.clone(), token }),
		}
	}
	parsed
}

/// Scope and path gates for one hit; the shared eligibility test of the
/// live watcher and [`StreamRuleSet::probe`].
fn admits(rule: &CompiledRule, source: StreamSource<'_>, paths: &[Str]) -> bool {
	scope_allows(rule, source)
		&& match source {
			StreamSource::ToolArgs { tool, .. } => has_path(rule, paths, tool),
			_ => true,
		}
}

fn scope_allows(rule: &CompiledRule, source: StreamSource<'_>) -> bool {
	if rule.scopes.is_empty() {
		return false;
	}
	match source {
		StreamSource::Text => rule.scopes.iter().any(|scope| matches!(scope, Scope::Text)),
		StreamSource::Thinking => rule
			.scopes
			.iter()
			.any(|scope| matches!(scope, Scope::Thinking)),
		StreamSource::ToolArgs { tool, .. } => rule.scopes.iter().any(|scope| match scope {
			Scope::Tool(name, _) => name
				.as_ref()
				.is_none_or(|name| name.eq_ignore_ascii_case(tool)),
			_ => false,
		}),
	}
}

fn has_path(rule: &CompiledRule, paths: &[Str], tool: &str) -> bool {
	if !rule.path_valid {
		return false;
	}
	let scope_globs = rule.scopes.iter().filter_map(|scope| match scope {
		Scope::Tool(name, glob)
			if name
				.as_ref()
				.is_none_or(|name| name.eq_ignore_ascii_case(tool)) =>
		{
			glob.as_ref()
		},
		_ => None,
	});
	let scope_globs = scope_globs.collect::<Vec<_>>();
	if rule.globs.is_empty() && scope_globs.is_empty() {
		return true;
	}
	paths.iter().any(|path| {
		(rule.globs.is_empty() || rule.globs.iter().any(|glob| glob.is_match(path.as_str())))
			&& (scope_globs.is_empty() || scope_globs.iter().any(|glob| glob.is_match(path.as_str())))
	})
}

fn collect_json_strings<'a>(value: &'a serde_json::Value, visit: &mut impl FnMut(&'a str)) {
	match value {
		serde_json::Value::String(text) => visit(text),
		serde_json::Value::Array(values) => values
			.iter()
			.for_each(|value| collect_json_strings(value, visit)),
		serde_json::Value::Object(values) => values
			.values()
			.for_each(|value| collect_json_strings(value, visit)),
		_ => {},
	}
}

fn collect_paths(value: &serde_json::Value, out: &mut Vec<Str>) {
	let Some(object) = value.as_object() else {
		return;
	};
	if let Some(path) = object.get("path").and_then(serde_json::Value::as_str) {
		out.push(Str::new(path));
	}
	if let Some(paths) = object.get("paths") {
		match paths {
			serde_json::Value::String(path) => out.push(Str::new(path)),
			serde_json::Value::Array(paths) => out.extend(
				paths
					.iter()
					.filter_map(serde_json::Value::as_str)
					.map(Str::new),
			),
			_ => {},
		}
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

	fn observe_turn(
		&self,
		dom: &omp_dom::Dom,
		cx: &DirectorCx<'_>,
		turn: &crate::TurnView,
	) -> Vec<StateUpdate> {
		let previous = match cx.state("responses") {
			Some(omp_dom::Value::Int(count)) => *count,
			_ => 0,
		};
		let current = dom
			.children(turn.turn)
			.iter()
			.filter(|handle| {
				dom.get(**handle)
					.is_some_and(|node| node.tag == omp_dom::KnownTag::Assistant.into())
			})
			.count() as i64;
		vec![StateUpdate::new("responses", BindValue::Int(previous.saturating_add(current)))]
	}

	fn watch_stream(&self, cx: &DirectorCx<'_>, _req: &ChatRequest) -> Option<Box<dyn StreamWatch>> {
		let set = self.set.as_ref()?.clone();
		let con = cx.con?;
		if !AI_STREAM_RULES_ENABLED.get(con) {
			return None;
		}
		let disabled = AI_STREAM_RULES_DISABLED.get(con);
		let interrupt = AI_STREAM_RULES_INTERRUPT.get(con);
		let context = AI_STREAM_RULES_CONTEXT.get(con);
		let repeat = AI_STREAM_RULES_REPEAT.get(con);
		let repeat_gap = AI_STREAM_RULES_REPEAT_GAP.get(con);
		let completed = match cx.state("responses") {
			Some(omp_dom::Value::Int(count)) => u32::try_from(*count).unwrap_or(u32::MAX),
			_ => 0,
		};
		let exhausted = set
			.rules
			.iter()
			.enumerate()
			.filter_map(|(index, rule)| {
				let name = rule.pattern.name.as_str();
				let disabled = disabled.iter().any(|item| item.as_str() == name);
				let fired_at = match cx.state(&format!("fired.{name}")) {
					Some(omp_dom::Value::Int(count)) => u32::try_from(*count).ok(),
					Some(omp_dom::Value::Bool(true)) => Some(0),
					_ => None,
				};
				let exhausted = fired_at.is_some_and(|fired_at| match repeat {
					StreamRuleRepeat::Once => true,
					StreamRuleRepeat::AfterGap => {
						completed
							.saturating_add(cx.response_ordinal)
							.saturating_sub(fired_at)
							< repeat_gap
					},
				});
				(disabled || exhausted).then_some(index)
			})
			.collect::<omp_core::FastHashSet<_>>();
		Some(Box::new(Watch::new(
			set,
			exhausted,
			interrupt,
			context,
			completed.saturating_add(cx.response_ordinal),
		)))
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
) -> (LazyState, smallvec::SmallVec<usize, 4>) {
	let state = dfa
		.next_state(cache, state, byte)
		.expect("DFA cache can grow for each byte");
	let mut matched = smallvec::SmallVec::new();
	if state.is_match() {
		for index in 0..dfa.match_len(cache, state) {
			matched.push(dfa.match_pattern(cache, state, index).as_usize());
		}
	}
	(state, matched)
}

/// Feeds the end-of-input transition (end of block counts as end of input
/// for `$`/`\z`) and returns the patterns matching there.
#[define_opaque(LazyState)]
fn finish_state(dfa: &DFA, cursor: &mut Cursor) -> smallvec::SmallVec<usize, 4> {
	let Ok(state) = dfa.next_eoi_state(&mut cursor.cache, cursor.state) else {
		return smallvec::SmallVec::new();
	};
	cursor.state = state;
	if !state.is_match() {
		return smallvec::SmallVec::new();
	}
	(0..dfa.match_len(&cursor.cache, state))
		.map(|index| dfa.match_pattern(&cursor.cache, state, index).as_usize())
		.collect()
}

/// Advances `cursor` over `bytes`, handing every matching pattern and the
/// byte offset its match ended at (the lazy DFA reports one byte late) to
/// `on_match` until it breaks.
fn feed(
	dfa: &DFA,
	cursor: &mut Cursor,
	bytes: &[u8],
	on_match: &mut impl FnMut(usize, usize) -> ControlFlow<()>,
) -> ControlFlow<()> {
	for (offset, byte) in bytes.iter().enumerate() {
		let (state, matches) = advance(dfa, &mut cursor.cache, cursor.state, *byte);
		cursor.state = state;
		for rule_index in matches {
			on_match(rule_index, offset)?;
		}
	}
	ControlFlow::Continue(())
}

struct Cursor {
	cache: regex_automata::hybrid::dfa::Cache,
	state: LazyState,
}

impl Cursor {
	fn start(dfa: &DFA) -> Self {
		let mut cache = dfa.create_cache();
		let state = start_state(dfa, &mut cache);
		Self { cache, state }
	}
}

struct Watch {
	set:              std::sync::Arc<StreamRuleSet>,
	cursors:          omp_core::FastHashMap<(u8, u32, u32), Cursor>,
	fired:            omp_core::FastHashSet<usize>,
	interrupt:        StreamRuleInterrupt,
	context:          StreamRuleContext,
	response_ordinal: u32,
}

impl Watch {
	fn new(
		set: std::sync::Arc<StreamRuleSet>,
		fired: omp_core::FastHashSet<usize>,
		interrupt: StreamRuleInterrupt,
		context: StreamRuleContext,
		response_ordinal: u32,
	) -> Self {
		Self {
			cursors: omp_core::FastHashMap::default(),
			fired,
			set,
			interrupt,
			context,
			response_ordinal,
		}
	}

	fn scan(
		&mut self,
		key: (u8, u32, u32),
		bytes: &[u8],
		source: StreamSource<'_>,
		paths: &[Str],
	) -> StreamVerdict {
		let set = &self.set;
		let fired = &self.fired;
		let cursor = self
			.cursors
			.entry(key)
			.or_insert_with(|| Cursor::start(&set.dfa));
		let mut found = None;
		let _ = feed(&set.dfa, cursor, bytes, &mut |rule_index, _| {
			if !fired.contains(&rule_index) && admits(&set.rules[rule_index], source, paths) {
				found = Some(rule_index);
				ControlFlow::Break(())
			} else {
				ControlFlow::Continue(())
			}
		});
		if let Some(rule_index) = found
			&& let Some(verdict) = self.trigger(rule_index, source, paths)
		{
			return verdict;
		}
		StreamVerdict::Pass
	}

	fn finish(
		&mut self,
		key: (u8, u32, u32),
		source: StreamSource<'_>,
		paths: &[Str],
	) -> StreamVerdict {
		let Some(cursor) = self.cursors.get_mut(&key) else {
			return StreamVerdict::Pass;
		};
		for rule_index in finish_state(&self.set.dfa, cursor) {
			if let Some(verdict) = self.trigger(rule_index, source, paths) {
				return verdict;
			}
		}
		StreamVerdict::Pass
	}

	fn trigger(
		&mut self,
		rule_index: usize,
		source: StreamSource<'_>,
		paths: &[Str],
	) -> Option<StreamVerdict> {
		if self.fired.contains(&rule_index) || !admits(&self.set.rules[rule_index], source, paths) {
			return None;
		}
		self.fired.insert(rule_index);
		let name = &self.set.rules[rule_index].pattern.name;
		for (index, rule) in self.set.rules.iter().enumerate() {
			if &rule.pattern.name == name {
				self.fired.insert(index);
			}
		}
		Some(self.hit(rule_index, source))
	}

	fn hit(&self, index: usize, source: StreamSource<'_>) -> StreamVerdict {
		let compiled = &self.set.rules[index];
		let rule = &compiled.pattern;
		let should_interrupt = interrupts(compiled.interrupt.unwrap_or(self.interrupt), source);
		let update = StateUpdate::new(
			sf!("fired.{}", rule.name),
			BindValue::Int(i64::from(self.response_ordinal)),
		);
		let call_id = match source {
			StreamSource::ToolArgs { call_id, .. } => Some(ToolCallId::from(call_id)),
			StreamSource::Text | StreamSource::Thinking => None,
		};
		let observation = observation(rule, source, call_id.as_ref());
		if !should_interrupt {
			let mut effect = StreamEffect {
				updates: vec![update],
				observation: Some(observation),
				..StreamEffect::default()
			};
			if let Some(call_id) = call_id {
				effect.call_diags.push((call_id, rule.body.clone()));
			} else {
				effect.developer = Some(rule.body.clone());
			}
			return StreamVerdict::Note(effect);
		}
		let source_name = <&'static str>::from(source);
		let notice = match (&call_id, source) {
			(Some(call_id), StreamSource::ToolArgs { tool, .. }) => sf!(
				"Stream rule {} matched the {tool} call {call_id}; the response was redirected.",
				rule.name
			),
			_ => sf!(
				"Stream rule {} matched the {source_name} output; the response was redirected.",
				rule.name
			),
		};
		let effect = StreamEffect {
			updates:     vec![update],
			developer:   Some(rule.body.clone()),
			notice:      Some(notice),
			notice_name: Some(Str::new_static(NOTICE_NAME)),
			call_diags:  Vec::new(),
			observation: Some(observation),
		};
		StreamVerdict::Interrupt(StreamInterrupt {
			culprit: call_id,
			partial: if self.context == StreamRuleContext::Keep {
				PartialOutput::Keep
			} else {
				PartialOutput::Discard
			},
			label: sf!("stream rule {}", rule.name),
			effect,
		})
	}

	fn call_ready_values(&mut self, index: u32, call: &ToolCall, values: &[&str]) -> StreamVerdict {
		let arguments = call.arguments.as_value();
		let mut paths = Vec::new();
		collect_paths(arguments, &mut paths);
		for (value_index, value) in values.iter().enumerate() {
			let Ok(value_index) = u32::try_from(value_index) else {
				break;
			};
			let source =
				StreamSource::ToolArgs { call_id: call.id.as_str(), tool: call.name.as_str() };
			let key = (2, index, value_index);
			let scanned = self.scan(key, value.as_bytes(), source, &paths);
			let verdict = match scanned {
				StreamVerdict::Pass => self.finish(key, source, &paths),
				verdict => verdict,
			};
			match verdict {
				StreamVerdict::Pass => {},
				verdict => return verdict,
			}
		}
		StreamVerdict::Pass
	}

	fn call_ready_segments(
		&mut self,
		index: u32,
		call: &ToolCall,
		segments: &[omp_tool::StreamMatchText],
	) -> StreamVerdict {
		let mut argument_paths = Vec::new();
		collect_paths(call.arguments.as_value(), &mut argument_paths);
		let source =
			StreamSource::ToolArgs { call_id: call.id.as_str(), tool: call.name.as_str() };
		for (segment_index, segment) in segments.iter().enumerate() {
			let Ok(segment_index) = u32::try_from(segment_index) else {
				break;
			};
			let segment_paths = segment
				.path
				.as_ref()
				.map(std::slice::from_ref)
				.unwrap_or(&argument_paths);
			let key = (2, index, segment_index);
			let scanned = self.scan(key, segment.text.as_bytes(), source, segment_paths);
			let verdict = match scanned {
				StreamVerdict::Pass => self.finish(key, source, segment_paths),
				verdict => verdict,
			};
			match verdict {
				StreamVerdict::Pass => {},
				verdict => return verdict,
			}
		}
		StreamVerdict::Pass
	}
}

impl StreamWatch for Watch {
	fn fragment(&mut self, fragment: StreamFragment<'_>) -> StreamVerdict {
		if matches!(fragment.source, StreamSource::ToolArgs { .. }) {
			return StreamVerdict::Pass;
		}
		let key = match fragment.source {
			StreamSource::Text => (0, fragment.index, 0),
			StreamSource::Thinking => (1, fragment.index, 0),
			StreamSource::ToolArgs { .. } => unreachable!(),
		};
		self.scan(key, fragment.bytes, fragment.source, &[])
	}

	fn block_end(&mut self, index: u32) -> StreamVerdict {
		match self.finish((0, index, 0), StreamSource::Text, &[]) {
			StreamVerdict::Pass => {},
			verdict => return verdict,
		}
		self.finish((1, index, 0), StreamSource::Thinking, &[])
	}

	fn call_ready(&mut self, index: u32, call: &ToolCall) -> StreamVerdict {
		let mut values = Vec::new();
		collect_json_strings(call.arguments.as_value(), &mut |value| values.push(value));
		self.call_ready_values(index, call, &values)
	}

	fn call_ready_with_match_text(
		&mut self,
		index: u32,
		call: &ToolCall,
		match_text: Option<&[omp_tool::StreamMatchText]>,
	) -> StreamVerdict {
		let Some(match_text) = match_text else {
			return self.call_ready(index, call);
		};
		self.call_ready_segments(index, call, match_text)
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn make_watch(
		pattern: &str,
		scope: &[&str],
		globs: &[&str],
		interrupt_mode: Option<&str>,
		context: StreamRuleContext,
	) -> Watch {
		let rule = RulePattern {
			name:           Str::new_static("guard"),
			body:           Str::new_static("Follow the repository rule."),
			pattern:        Str::new(pattern),
			scope:          scope.iter().map(|item| Str::new(*item)).collect(),
			globs:          globs.iter().map(|item| Str::new(*item)).collect(),
			interrupt_mode: interrupt_mode.map(Str::new),
		};
		let set = std::sync::Arc::new(
			StreamRuleSet::compile(vec![rule])
				.set
				.expect("valid rule compiles"),
		);
		Watch::new(set, omp_core::FastHashSet::default(), StreamRuleInterrupt::Always, context, 1)
	}

	fn fragment<'a>(index: u32, source: StreamSource<'a>, text: &'a [u8]) -> StreamFragment<'a> {
		StreamFragment { index, source, bytes: text }
	}

	#[test]
	fn matches_across_text_fragments_and_interrupts_with_context_policy() {
		let mut watch = make_watch("danger", &["text"], &[], None, StreamRuleContext::Keep);
		assert!(matches!(
			watch.fragment(fragment(0, StreamSource::Text, b"first dan")),
			StreamVerdict::Pass
		));
		let verdict = watch.fragment(fragment(0, StreamSource::Text, b"gerous line"));
		let StreamVerdict::Interrupt(interrupt) = verdict else {
			panic!("expected stream redirect")
		};
		assert_eq!(interrupt.partial, PartialOutput::Keep);
		assert_eq!(interrupt.effect.developer.as_deref(), Some("Follow the repository rule."));
	}

	#[test]
	fn default_scope_skips_thinking_while_explicit_scope_matches_it() {
		let mut default_watch = make_watch("secret", &[], &[], None, StreamRuleContext::Discard);
		let thinking = StreamSource::Thinking;
		assert!(matches!(
			default_watch.fragment(fragment(0, thinking, b"secret")),
			StreamVerdict::Pass
		));
		assert!(matches!(default_watch.block_end(0), StreamVerdict::Pass));
		assert!(matches!(
			default_watch.fragment(fragment(0, StreamSource::Text, b"secret")),
			StreamVerdict::Pass
		));
		assert!(matches!(default_watch.block_end(0), StreamVerdict::Interrupt(_)));

		let mut thinking_watch =
			make_watch("secret", &["thinking"], &[], None, StreamRuleContext::Discard);
		assert!(matches!(
			thinking_watch.fragment(fragment(0, thinking, b"secret")),
			StreamVerdict::Pass
		));
		assert!(matches!(thinking_watch.block_end(0), StreamVerdict::Interrupt(_)));
		assert!(matches!(
			thinking_watch.fragment(fragment(0, StreamSource::Text, b"secret")),
			StreamVerdict::Pass
		));
	}

	#[test]
	fn tool_scope_and_path_gate_use_decoded_json_string_values() {
		let mut watch = make_watch(
			"remove this file",
			&["tool:edit(src/**)"],
			&["src/**"],
			None,
			StreamRuleContext::Discard,
		);
		let args = serde_json::from_str(r#"{"path":"src/main.rs","content":"remove this file"}"#)
			.expect("valid tool JSON");
		let call = ToolCall {
			id:        ToolCallId::from("call-1"),
			name:      Str::new_static("edit"),
			arguments: omp_ai::OpaqueJson::new(args),
		};
		let verdict = watch.call_ready(0, &call);
		let StreamVerdict::Interrupt(interrupt) = verdict else {
			panic!("expected matching tool rule")
		};
		assert_eq!(interrupt.culprit, Some(ToolCallId::from("call-1")));

		let mut gated = make_watch(
			"remove this file",
			&["tool:edit(src/**)"],
			&["src/**"],
			None,
			StreamRuleContext::Discard,
		);
		let args = serde_json::from_str(r#"{"path":"docs/guide.md","content":"remove this file"}"#)
			.expect("valid tool JSON");
		let call = ToolCall {
			id:        ToolCallId::from("call-2"),
			name:      Str::new_static("edit"),
			arguments: omp_ai::OpaqueJson::new(args),
		};
		assert!(matches!(gated.call_ready(0, &call), StreamVerdict::Pass));
	}

	#[test]
	fn tool_projection_matches_only_newly_authored_text() {
		let mut watch = make_watch(
			"removed text",
			&["tool:edit(src/**)"],
			&["src/**"],
			None,
			StreamRuleContext::Discard,
		);
		let args = serde_json::from_str(r#"{"path":"src/main.rs","content":"removed text"}"#)
			.expect("valid tool JSON");
		let call = ToolCall {
			id:        ToolCallId::from("call-projected"),
			name:      Str::new_static("edit"),
			arguments: omp_ai::OpaqueJson::new(args),
		};
		assert!(matches!(
			watch.call_ready_with_match_text(
				0,
				&call,
				Some(&[omp_tool::StreamMatchText {
					path: Some(Str::new_static("src/main.rs")),
					text: Str::new_static("new code"),
				}]),
			),
			StreamVerdict::Pass
		));
		let mut matching_watch = make_watch(
			"removed text",
			&["tool:edit(src/**)"],
			&["src/**"],
			None,
			StreamRuleContext::Discard,
		);
		let StreamVerdict::Interrupt(interrupt) = matching_watch.call_ready_with_match_text(
			0,
			&call,
			Some(&[omp_tool::StreamMatchText {
				path: Some(Str::new_static("src/main.rs")),
				text: Str::new_static("removed text"),
			}]),
		) else {
			panic!("newly authored text should match")
		};
		assert_eq!(interrupt.culprit, Some(ToolCallId::from("call-projected")));
	}

	#[test]
	fn interrupt_mode_override_delivers_note_and_tool_diagnostics() {
		let mut prose =
			make_watch("forbidden", &["text"], &[], Some("never"), StreamRuleContext::Discard);
		assert!(matches!(
			prose.fragment(fragment(0, StreamSource::Text, b"forbidden")),
			StreamVerdict::Pass
		));
		let StreamVerdict::Note(note) = prose.block_end(0) else {
			panic!("never policy should record a note")
		};
		assert_eq!(note.developer.as_deref(), Some("Follow the repository rule."));

		let mut tool =
			make_watch("forbidden", &["tool:edit"], &[], Some("never"), StreamRuleContext::Discard);
		let args = serde_json::from_str(r#"{"content":"forbidden"}"#).expect("valid tool JSON");
		let call = ToolCall {
			id:        ToolCallId::from("call-3"),
			name:      Str::new_static("edit"),
			arguments: omp_ai::OpaqueJson::new(args),
		};
		let StreamVerdict::Note(note) = tool.call_ready(0, &call) else {
			panic!("expected non-interrupting note")
		};
		assert_eq!(note.call_diags.len(), 1);
		assert_eq!(note.call_diags[0].0, call.id);
	}

	#[test]
	fn invalid_path_glob_makes_rule_unreachable() {
		let mut watch =
			make_watch("forbidden", &["tool:edit"], &["["], None, StreamRuleContext::Discard);
		let args = serde_json::from_str(r#"{"path":"src/main.rs","content":"forbidden"}"#)
			.expect("valid tool JSON");
		let call = ToolCall {
			id:        ToolCallId::from("call-4"),
			name:      Str::new_static("edit"),
			arguments: omp_ai::OpaqueJson::new(args),
		};
		assert!(matches!(watch.call_ready(0, &call), StreamVerdict::Pass));
	}

	fn pattern(name: &'static str, pattern: &str, scope: &[&str]) -> RulePattern {
		RulePattern {
			name:           Str::new_static(name),
			body:           Str::new_static("Follow the repository rule."),
			pattern:        Str::new(pattern),
			scope:          scope.iter().map(|item| Str::new(*item)).collect(),
			globs:          Vec::new(),
			interrupt_mode: None,
		}
	}

	#[test]
	fn compile_reports_typed_warnings_and_keeps_valid_conditions() {
		let mut bad_mode = pattern("mode", "ok", &["text"]);
		bad_mode.interrupt_mode = Some(Str::new_static("sometimes"));
		let mut bad_glob = pattern("glob", "ok", &["tool"]);
		bad_glob.globs = vec![Str::new_static("[")];
		let compiled = StreamRuleSet::compile(vec![
			pattern("multi", "first", &["text"]),
			pattern("multi", "a(?=b)", &["text"]),
			pattern("scopes", "x", &["bogus", "tool:edit([)"]),
			bad_mode,
			bad_glob,
		]);
		let set = compiled.set.expect("valid conditions compile");
		assert_eq!(
			set.patterns()
				.map(|rule| rule.name.as_str())
				.collect::<Vec<_>>(),
			["multi", "scopes", "mode", "glob"]
		);
		let kinds = compiled
			.warnings
			.iter()
			.map(|warning| match warning {
				StreamRuleWarning::Condition { rule, index, .. } => format!("condition {rule} {index}"),
				StreamRuleWarning::UnknownScope { token, .. } => format!("scope {token}"),
				StreamRuleWarning::ScopeGlob { token, .. } => format!("scope-glob {token}"),
				StreamRuleWarning::Unreachable { rule } => format!("unreachable {rule}"),
				StreamRuleWarning::InterruptMode { mode, .. } => format!("mode {mode}"),
				StreamRuleWarning::PathGlob { glob, .. } => format!("glob {glob}"),
				StreamRuleWarning::Matcher { .. } => "matcher".to_owned(),
			})
			.collect::<Vec<_>>();
		assert_eq!(kinds, [
			"condition multi 1",
			"scope bogus",
			"scope-glob tool:edit([)",
			"unreachable scopes",
			"mode sometimes",
			"glob [",
		]);
		assert!(condition_compiles("plain"));
		assert!(!condition_compiles("a(?=b)"));
	}

	#[test]
	fn probe_agrees_with_the_live_watch_and_reports_match_ends() {
		let set = StreamRuleSet::compile(vec![
			pattern("danger", "danger", &["text"]),
			pattern("tail", "end$", &["text"]),
			pattern("thought", "secret", &["thinking"]),
		])
		.set
		.expect("compiles");
		let text = b"a danger then the end";
		let hits = set.probe(StreamSource::Text, &[], text, StreamRuleInterrupt::Always, &[]);
		assert_eq!(
			hits
				.iter()
				.map(|hit| (hit.rule.name.as_str(), hit.end, hit.interrupts))
				.collect::<Vec<_>>(),
			[("danger", 8, true), ("tail", text.len(), true)]
		);
		let disabled = [Str::new_static("danger")];
		let hits = set.probe(StreamSource::Text, &[], text, StreamRuleInterrupt::ToolOnly, &disabled);
		assert_eq!(
			hits
				.iter()
				.map(|hit| (hit.rule.name.as_str(), hit.interrupts))
				.collect::<Vec<_>>(),
			[("tail", false)]
		);
		assert!(
			set.probe(StreamSource::Text, &[], b"secret", StreamRuleInterrupt::Always, &[])
				.is_empty()
		);

		let set = std::sync::Arc::new(set);
		let mut watch = Watch::new(
			std::sync::Arc::clone(&set),
			omp_core::FastHashSet::default(),
			StreamRuleInterrupt::Always,
			StreamRuleContext::Discard,
			1,
		);
		assert!(matches!(
			watch.fragment(fragment(0, StreamSource::Text, b"a dan")),
			StreamVerdict::Pass
		));
		let StreamVerdict::Interrupt(live) = watch.fragment(fragment(0, StreamSource::Text, b"ger!"))
		else {
			panic!("the live watch interrupts where the probe hits")
		};
		assert_eq!(live.label.as_str(), "stream rule danger");
	}
}
