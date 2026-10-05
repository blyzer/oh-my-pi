//! Per-turn difficulty classification with bounded outputs and deterministic
//! fallbacks.

use std::collections::{HashMap, VecDeque};

use omp_core::Str;
use omp_proto::omp::inference::v1::Effort;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use strum::IntoStaticStr;

const INPUT_LIMIT: usize = 16 * 1024;
const MEMO_LIMIT: usize = 256;

/// Ordered reasoning difficulty selected for one turn.
#[derive(
	Clone,
	Copy,
	Debug,
	Default,
	Deserialize,
	Eq,
	Hash,
	IntoStaticStr,
	Ord,
	PartialEq,
	PartialOrd,
	Serialize,
)]
#[serde(rename_all = "snake_case")]
#[repr(u8)]
pub enum Difficulty {
	/// Trivial lookup or mechanical response.
	#[strum(serialize = "low")]
	Minimal = 0,
	/// Small, well-scoped task.
	#[strum(serialize = "low")]
	Low     = 1,
	/// Normal multi-step task.
	#[default]
	#[strum(serialize = "medium")]
	Medium  = 2,
	/// Difficult analysis or broad implementation.
	#[strum(serialize = "high")]
	High    = 3,
	/// Maximum supported reasoning effort.
	#[strum(serialize = "high")]
	Max     = 4,
}

impl Difficulty {
	const ONLINE_LABELS: [(&'static str, Self); 5] = [
		("minimal", Self::Minimal),
		("low", Self::Low),
		("medium", Self::Medium),
		("high", Self::High),
		("max", Self::Max),
	];

	/// Clamps this level to an explicit effort ceiling.
	pub fn clamped(self, ceiling: Self) -> Self {
		self.min(ceiling)
	}

	/// Clamps an `auto` provisional level below the maximum rung.
	pub fn provisional(self, ceiling: Self) -> Self {
		self.min(Self::High).min(ceiling)
	}

	/// Stable lowercase classifier label.
	pub const fn label(self) -> &'static str {
		Self::ONLINE_LABELS[self as usize].0
	}

	/// Converts this classifier rung to the canonical reasoning effort.
	pub const fn effort(self) -> Effort {
		match self {
			Self::Minimal => Effort::Minimal,
			Self::Low => Effort::Low,
			Self::Medium => Effort::Medium,
			Self::High => Effort::High,
			Self::Max => Effort::Max,
		}
	}
}

/// Backend selected for an automatic per-turn decision.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
#[repr(u8)]
pub enum DifficultyBackend {
	/// Small online chat model using the five-rung ladder.
	Online = 0,
	/// On-device classifier using a compact three-bucket ladder.
	Local  = 1,
}

/// Origin of an effective difficulty decision.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DifficultySource {
	/// A classifier backend selected the level.
	Backend,
	/// An identical sanitized prompt reused a bounded memo entry.
	Memo,
	/// The previous successful level survived a backend failure.
	Previous,
	/// The clamped provisional level was used.
	Provisional,
	/// Prewalk reported that the turn had no executable work.
	PrewalkNoop,
}

/// Effective reasoning level and its provenance.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DifficultyDecision {
	/// Selected level after ceiling enforcement.
	pub level:  Difficulty,
	/// Decision provenance.
	pub source: DifficultySource,
}

/// Immutable input to one automatic classification.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AutoDifficulty {
	/// Level used before the classifier settles.
	pub provisional:  Difficulty,
	/// Highest level this session permits.
	pub ceiling:      Difficulty,
	/// Whether the online ladder may return its maximum rung.
	pub allow_max:    bool,
	/// Prewalk's reason-to-execute hook found no executable work.
	pub prewalk_noop: bool,
}

impl Default for AutoDifficulty {
	fn default() -> Self {
		Self {
			provisional:  Difficulty::Medium,
			ceiling:      Difficulty::Max,
			allow_max:    false,
			prewalk_noop: false,
		}
	}
}

impl AutoDifficulty {
	/// Applies prewalk's reason-to-execute result to this turn.
	///
	/// A missing or blank reason is an explicit no-op hook and bypasses backend
	/// classification; a non-empty reason keeps normal classification active.
	pub fn with_prewalk_reason(mut self, reason: Option<&str>) -> Self {
		self.prewalk_noop = reason.is_none_or(|reason| reason.trim().is_empty());
		self
	}
}

#[derive(Default)]
struct MemoState {
	entries:  HashMap<[u8; 32], Difficulty>,
	order:    VecDeque<[u8; 32]>,
	previous: Option<Difficulty>,
}

/// Session-scoped classifier state with bounded prompt memoization.
#[derive(Default)]
pub struct DifficultyClassifier {
	state: Mutex<MemoState>,
}

impl DifficultyClassifier {
	/// Creates an empty classifier cache.
	pub fn new() -> Self {
		Self::default()
	}

	/// Applies the deterministic fallback path without dispatching a backend.
	///
	/// This is useful while an online or local classifier is unavailable and for
	/// callers that receive a prewalk no-op result before backend selection.
	pub fn fallback(
		&self,
		input: &str,
		backend: DifficultyBackend,
		auto: AutoDifficulty,
	) -> DifficultyDecision {
		let sanitized = sanitize_classifier_input(input);
		self
			.short_circuit(&sanitized, backend, auto)
			.unwrap_or_else(|| self.finish(&sanitized, backend, auto, None))
	}

	fn short_circuit(
		&self,
		sanitized: &Str,
		backend: DifficultyBackend,
		auto: AutoDifficulty,
	) -> Option<DifficultyDecision> {
		if auto.prewalk_noop || sanitized.is_empty() {
			return Some(DifficultyDecision {
				level:  auto.provisional.provisional(auto.ceiling),
				source: DifficultySource::PrewalkNoop,
			});
		}
		let key = memo_key(sanitized, backend, auto);
		self
			.state
			.lock()
			.entries
			.get(&key)
			.copied()
			.map(|level| DifficultyDecision {
				level:  level.clamped(backend_ceiling(backend, auto)),
				source: DifficultySource::Memo,
			})
	}

	fn finish(
		&self,
		sanitized: &Str,
		backend: DifficultyBackend,
		auto: AutoDifficulty,
		selected: Option<Difficulty>,
	) -> DifficultyDecision {
		let mut state = self.state.lock();
		let (level, source) = selected.map_or_else(
			|| match state.previous {
				Some(previous) => {
					(previous.clamped(backend_ceiling(backend, auto)), DifficultySource::Previous)
				},
				None => (auto.provisional.provisional(auto.ceiling), DifficultySource::Provisional),
			},
			|level| (level.clamped(backend_ceiling(backend, auto)), DifficultySource::Backend),
		);
		if selected.is_some() {
			state.previous = Some(level);
			let key = memo_key(sanitized, backend, auto);
			if !state.entries.contains_key(&key) {
				if state.order.len() == MEMO_LIMIT
					&& let Some(oldest) = state.order.pop_front()
				{
					state.entries.remove(&oldest);
				}
				state.order.push_back(key);
			}
			state.entries.insert(key, level);
		}
		DifficultyDecision { level, source }
	}
}

/// Removes control bytes, normalizes whitespace, and bounds classifier input.
pub fn sanitize_classifier_input(input: &str) -> Str {
	let mut output = String::with_capacity(input.len().min(INPUT_LIMIT));
	let mut pending_space = false;
	for character in input.chars() {
		if character.is_whitespace() {
			pending_space = !output.is_empty();
			continue;
		}
		if character.is_control() {
			continue;
		}
		let needed = character.len_utf8() + usize::from(pending_space);
		if output.len().saturating_add(needed) > INPUT_LIMIT {
			break;
		}
		if pending_space {
			output.push(' ');
			pending_space = false;
		}
		output.push(character);
	}
	output.into()
}

fn backend_ceiling(backend: DifficultyBackend, auto: AutoDifficulty) -> Difficulty {
	if backend == DifficultyBackend::Local || !auto.allow_max {
		auto.ceiling.min(Difficulty::High)
	} else {
		auto.ceiling
	}
}

fn memo_key(input: &Str, backend: DifficultyBackend, auto: AutoDifficulty) -> [u8; 32] {
	let mut context = Sha256::new();
	context.update(b"omp.difficulty/v1\0");
	context.update([backend as u8, auto.allow_max as u8, auto.ceiling as u8]);
	context.update(input.as_bytes());
	context.finalize().into()
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn sanitizer_normalizes_and_stays_on_utf8_boundaries() {
		let input = format!("  hello\0\nworld {}", "é".repeat(INPUT_LIMIT));
		let sanitized = sanitize_classifier_input(&input);
		assert!(sanitized.starts_with("hello world é"));
		assert!(sanitized.len() <= INPUT_LIMIT);
	}

	#[test]
	fn auto_provisional_and_noop_are_clamped_below_max() {
		let classifier = DifficultyClassifier::new();
		let decision = classifier.fallback(
			"ignored",
			DifficultyBackend::Online,
			AutoDifficulty { provisional: Difficulty::Max, ..AutoDifficulty::default() }
				.with_prewalk_reason(None),
		);
		assert_eq!(decision.level, Difficulty::High);
		assert_eq!(decision.source, DifficultySource::PrewalkNoop);
	}
}
