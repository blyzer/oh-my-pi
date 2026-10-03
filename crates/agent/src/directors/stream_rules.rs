//! Incremental stream-condition matching for discovered rule documents.

use std::str::FromStr;

use omp_ai::{ChatRequest, ToolCall, ToolCallId};
use omp_core::Str;
use regex_automata::{Input, MatchKind, hybrid::dfa::DFA};

use crate::{
	director::{
		BindValue, Director, DirectorCx, PartialOutput, StateUpdate, StreamEffect, StreamFragment,
		StreamInterrupt, StreamSource, StreamVerdict, StreamWatch,
	},
	vars::{
		AI_STREAM_RULES_CONTEXT, AI_STREAM_RULES_DISABLED, AI_STREAM_RULES_ENABLED,
		AI_STREAM_RULES_INTERRUPT, AI_STREAM_RULES_REPEAT, AI_STREAM_RULES_REPEAT_GAP,
		StreamRuleContext, StreamRuleInterrupt, StreamRuleRepeat,
	},
};

/// Registry family for the stream-rules Director.
pub const FAMILY: &str = "stream-rules";

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

impl StreamRuleSet {
	/// Compiles valid conditions, silently skipping invalid patterns.
	#[must_use]
	pub fn compile(rules: Vec<RulePattern>) -> Option<Self> {
		let mut valid = Vec::with_capacity(rules.len());
		for (index, rule) in rules.into_iter().enumerate() {
			let scopes = parse_scopes(&rule.scope);
			let mut path_valid = true;
			let mut globs = Vec::with_capacity(rule.globs.len());
			for glob in &rule.globs {
				match globset::Glob::new(glob) {
					Ok(glob) => globs.push(glob.compile_matcher()),
					Err(error) => {
						path_valid = false;
						tracing::warn!(rule = %rule.name, %glob, %error, "invalid stream rule path glob makes the rule unreachable");
					},
				}
			}
			let interrupt = rule.interrupt_mode.as_deref().and_then(|mode| {
				StreamRuleInterrupt::from_str(mode).map_err(|error| tracing::warn!(rule = %rule.name, %mode, %error, "invalid stream rule interruptMode was ignored")).ok()
			});
			match DFA::builder().build(&rule.pattern) {
				Ok(_) => {
					valid.push(CompiledRule { pattern: rule, scopes, globs, path_valid, interrupt })
				},
				Err(_) => {
					tracing::warn!(rule = %rule.name, pattern_index = index, "invalid stream rule regex was skipped");
				},
			}
		}
		let patterns = valid
			.iter()
			.map(|rule| rule.pattern.pattern.as_str())
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

fn parse_scopes(scopes: &[Str]) -> Vec<Scope> {
	if scopes.is_empty() {
		return vec![Scope::Text, Scope::Tool(None, None)];
	}
	let mut parsed = Vec::new();
	let tokens = scopes
		.iter()
		.flat_map(|scope| scope.split(","))
		.map(|scope| scope.as_str().trim().to_owned())
		.collect::<Vec<_>>();
	for token in tokens.iter().map(String::as_str) {
		match token.to_ascii_lowercase().as_str() {
			"text" => parsed.push(Scope::Text),
			"thinking" => parsed.push(Scope::Thinking),
			"tool" | "toolcall" => parsed.push(Scope::Tool(None, None)),
			_ if token.to_ascii_lowercase().starts_with("tool:") => {
				let selector = &token[5..];
				if let Some((name, glob)) = selector.strip_suffix(')').and_then(|s| s.rsplit_once('('))
				{
					match globset::Glob::new(glob) {
						Ok(glob) => {
							parsed.push(Scope::Tool(Some(Str::new(name)), Some(glob.compile_matcher())))
						},
						Err(error) => {
							tracing::warn!(%token, %error, "invalid tool scope path glob was ignored")
						},
					}
				} else if !selector.is_empty() {
					parsed.push(Scope::Tool(Some(Str::new(selector)), None));
				}
			},
			_ => tracing::warn!(%token, "unknown stream rule scope was ignored"),
		}
	}
	parsed
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

struct Cursor {
	cache: regex_automata::hybrid::dfa::Cache,
	state: LazyState,
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
		let found = {
			let cursor = self.cursors.entry(key).or_insert_with(|| {
				let mut cache = self.set.dfa.create_cache();
				let state = start_state(&self.set.dfa, &mut cache);
				Cursor { cache, state }
			});
			let mut found = None;
			'bytes: for byte in bytes {
				let (state, matches) = advance(&self.set.dfa, &mut cursor.cache, cursor.state, *byte);
				cursor.state = state;
				for rule_index in matches {
					if !self.fired.contains(&rule_index)
						&& scope_allows(&self.set.rules[rule_index], source)
						&& match source {
							StreamSource::ToolArgs { tool, .. } => {
								has_path(&self.set.rules[rule_index], paths, tool)
							},
							_ => true,
						} {
						found = Some(rule_index);
						break 'bytes;
					}
				}
			}
			found
		};
		if let Some(rule_index) = found {
			if let Some(verdict) = self.trigger(rule_index, source, paths) {
				return verdict;
			}
		}
		StreamVerdict::Pass
	}

	#[define_opaque(LazyState)]
	fn finish(
		&mut self,
		key: (u8, u32, u32),
		source: StreamSource<'_>,
		paths: &[Str],
	) -> StreamVerdict {
		let matches = {
			let Some(cursor) = self.cursors.get_mut(&key) else {
				return StreamVerdict::Pass;
			};
			let Ok(state) = self.set.dfa.next_eoi_state(&mut cursor.cache, cursor.state) else {
				return StreamVerdict::Pass;
			};
			cursor.state = state;
			if !state.is_match() {
				return StreamVerdict::Pass;
			}
			(0..self.set.dfa.match_len(&cursor.cache, state))
				.map(|index| {
					self
						.set
						.dfa
						.match_pattern(&cursor.cache, state, index)
						.as_usize()
				})
				.collect::<smallvec::SmallVec<usize, 4>>()
		};
		for rule_index in matches {
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
		if self.fired.contains(&rule_index)
			|| !scope_allows(&self.set.rules[rule_index], source)
			|| !match source {
				StreamSource::ToolArgs { tool, .. } => {
					has_path(&self.set.rules[rule_index], paths, tool)
				},
				_ => true,
			} {
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
		let policy = compiled.interrupt.unwrap_or(self.interrupt);
		let tool_source = matches!(source, StreamSource::ToolArgs { .. });
		let should_interrupt = match policy {
			StreamRuleInterrupt::Always => true,
			StreamRuleInterrupt::ProseOnly => !tool_source,
			StreamRuleInterrupt::ToolOnly => tool_source,
			StreamRuleInterrupt::Never => false,
		};
		let update = StateUpdate::new(
			Str::new(format!("fired.{}", rule.name)),
			BindValue::Int(i64::from(self.response_ordinal)),
		);
		if !should_interrupt {
			let mut effect = StreamEffect { updates: vec![update], ..StreamEffect::default() };
			if tool_source {
				if let StreamSource::ToolArgs { call_id, .. } = source {
					effect
						.call_diags
						.push((ToolCallId::from(call_id), rule.body.clone()));
				}
			} else {
				effect.developer = Some(rule.body.clone());
			}
			return StreamVerdict::Note(effect);
		}
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
			partial: if self.context == StreamRuleContext::Keep {
				PartialOutput::Keep
			} else {
				PartialOutput::Discard
			},
			label: Str::new(format!("stream rule {}", rule.name)),
			effect,
		})
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
		let arguments = call.arguments.as_value();
		let mut paths = Vec::new();
		collect_paths(arguments, &mut paths);
		let mut values = Vec::new();
		collect_json_strings(arguments, &mut |value| values.push(value));
		for (value_index, value) in values.into_iter().enumerate() {
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
		let set =
			std::sync::Arc::new(StreamRuleSet::compile(vec![rule]).expect("valid rule compiles"));
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
}
