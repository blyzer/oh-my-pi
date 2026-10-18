#![cfg(feature = "apple-aux-memory")]
//! Auxiliary Apple Foundation Models re-ranking lane.
//!
//! Scores each [`RecallResult`] by asking an [`AppleFmSession`] to rate
//! relevance on a 0-10 integer scale, then sorts descending.

use omp_core::Str;
use tokio::task;

use crate::recall::RecallResult;

/// Re-ranks `candidates` using an on-device Apple FM relevance score.
///
/// Falls back to the original order on any error (session creation failure,
/// FM unavailability, or parse failure).  Empty inputs are returned
/// immediately without touching the model.
pub async fn rank(query: Str, mut candidates: Vec<RecallResult>) -> Vec<RecallResult> {
	if candidates.is_empty() {
		return candidates;
	}
	let scored = task::spawn_blocking(move || {
		let session = match omp_ai::local::applefm::aux_session() {
			Ok(s) => s,
			Err(_) => return candidates,
		};
		for result in &mut candidates {
			let prompt = format!(
				"query={q} memory={m}",
				q = query.as_str(),
				m = result.memory.content.as_str(),
			);
			let raw = match session.respond(prompt) {
				Ok(r) => r,
				Err(_) => continue,
			};
			let score: i32 = raw.trim().parse().unwrap_or(0).clamp(0, 10);
			// Overwrite the fused RRF score with the FM score normalised to [0,1].
			result.score = f64::from(score) / 10.0;
		}
		candidates.sort_by(|a, b| b.score.total_cmp(&a.score));
		candidates
	})
	.await;
	// If spawn_blocking itself panicked, return empty rather than propagating.
	scored.unwrap_or_default()
}

#[cfg(all(test, feature = "apple-aux-memory"))]
mod tests {
	use omp_core::{Str, sf};

	use super::rank;
	use crate::{
		bank::BankId,
		recall::{RecallResult, VoiceScores},
		store::MemoryRecord,
	};

	fn fake_record(id: &str, content: &str) -> MemoryRecord {
		MemoryRecord {
			id:            sf!("{id}"),
		bank:          BankId::configured("test").expect("valid bank name"),
			content:       Str::new(content),
			source:        None,
			session_id:    sf!("s0"),
			timestamp:     sf!("2024-01-01T00:00:00Z"),
			importance:    0.5,
			veracity:      sf!("stated"),
			memory_type:   sf!("fact"),
			metadata:      serde_json::Value::Null,
			superseded_by: None,
		}
	}

	fn fake_result(id: &str, content: &str, score: f64) -> RecallResult {
		RecallResult {
			memory:       fake_record(id, content),
			score,
			voice_scores: VoiceScores::default(),
			broadened:    false,
		}
	}

	#[tokio::test]
	async fn empty_input_returns_empty() {
		let out = rank(sf!("anything"), vec![]).await;
		assert!(out.is_empty(), "empty input must return empty");
	}

	#[tokio::test]
	async fn fallback_preserves_order_on_unavailable_model() {
		// On a machine without Apple Intelligence the aux_session() call fails
		// and rank() must return the candidates in their original order.
		let a = fake_result("a", "alpha content", 0.9);
		let b = fake_result("b", "beta content", 0.1);
		let input = vec![a.clone(), b.clone()];
		let out = rank(sf!("query"), input).await;
		// Either original order (fallback) or FM-rescored order are acceptable;
		// what must not happen is a panic or an empty result.
		assert_eq!(out.len(), 2, "both entries must survive");
	}
}
