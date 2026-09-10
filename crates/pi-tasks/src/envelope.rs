//! Typed agent handoff envelopes.
//!
//! An agent's only structured output channel is the last complete top-level
//! JSON object in its final turn. Everything before it (prose, fences,
//! reasoning) is ignored, so an agent that narrates before answering still
//! produces a parseable envelope.

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EnvelopeStatus {
	Success,
	Fail,
}

/// The contract every phase must return. Unknown fields land in `payload`, so
/// a phase-specific schema (`changed_files`, `commit_message`, …) rides along
/// without a distinct Rust type per phase.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Envelope<T = Value> {
	pub status:               EnvelopeStatus,
	#[serde(default)]
	pub summary:              String,
	#[serde(default)]
	pub artifacts:            Vec<String>,
	#[serde(default)]
	pub notes_for_next_agent: String,
	#[serde(flatten)]
	pub payload:              T,
}

#[derive(Debug, thiserror::Error)]
pub enum EnvelopeError {
	#[error("no JSON object found in agent output")]
	NoJson,
	#[error("envelope JSON did not match the contract: {0}")]
	Invalid(#[from] serde_json::Error),
}

impl<T> Envelope<T> {
	pub const fn is_success(&self) -> bool {
		matches!(self.status, EnvelopeStatus::Success)
	}
}

impl Envelope<Value> {
	/// Parse the trailing JSON object of an agent turn.
	pub fn from_agent_text(text: &str) -> Result<Self, EnvelopeError> {
		let raw = last_top_level_object(text).ok_or(EnvelopeError::NoJson)?;
		Ok(serde_json::from_str(raw)?)
	}

	/// Envelope for a deterministic (`kind = code`) step. Code phases have no
	/// model to ask, so the executor reports the outcome directly and it flows
	/// through the same gates as an agent's.
	pub fn code(ok: bool, summary: impl Into<String>) -> Self {
		Self {
			status:               if ok {
				EnvelopeStatus::Success
			} else {
				EnvelopeStatus::Fail
			},
			summary:              summary.into(),
			artifacts:            Vec::new(),
			notes_for_next_agent: String::new(),
			payload:              Value::Object(serde_json::Map::new()),
		}
	}
}

/// The envelope text inside an agent's turn.
///
/// The last complete top-level JSON object, or `None` when the turn contained
/// none. Spans are the last balanced `{…}` at nesting depth zero, string- and
/// escape-aware so braces inside JSON strings (or prose quoting them) never
/// split one.
///
/// Public so a caller that must inspect the payload before submitting — schema
/// validation, which needs a type system this crate does not have — uses this
/// rule rather than reimplementing "the last JSON object" and drifting from it.
pub fn envelope_text(text: &str) -> Option<&str> {
	last_top_level_object(text)
}

fn last_top_level_object(text: &str) -> Option<&str> {
	let mut depth = 0usize;
	let mut start = None;
	let mut last = None;
	let mut in_string = false;
	let mut escaped = false;

	for (i, byte) in text.bytes().enumerate() {
		if in_string {
			if escaped {
				escaped = false;
			} else if byte == b'\\' {
				escaped = true;
			} else if byte == b'"' {
				in_string = false;
			}
			continue;
		}
		match byte {
			b'"' => in_string = true,
			b'{' => {
				if depth == 0 {
					start = Some(i);
				}
				depth += 1;
			},
			b'}' if depth > 0 => {
				depth -= 1;
				if depth == 0
					&& let Some(s) = start.take()
				{
					last = Some(&text[s..=i]);
				}
			},
			_ => {},
		}
	}
	last
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn parses_the_last_object_after_prose_and_fences() {
		let text = r#"Here is my thinking.

```json
{"status": "fail", "summary": "draft"}
```

Final answer:
{"status": "success", "summary": "done", "artifacts": ["specs/plan.md"], "changed_files": ["a.rs"]}
"#;
		let env = Envelope::from_agent_text(text).expect("parses");
		assert!(env.is_success());
		assert_eq!(env.summary, "done");
		assert_eq!(env.artifacts, vec!["specs/plan.md"]);
		assert_eq!(env.payload["changed_files"][0], "a.rs");
	}

	#[test]
	fn braces_inside_strings_do_not_split_the_span() {
		let text = r#"{"status":"success","summary":"used a { literal } brace"}"#;
		let env = Envelope::from_agent_text(text).expect("parses");
		assert_eq!(env.summary, "used a { literal } brace");
	}

	#[test]
	fn nested_objects_stay_one_span() {
		let text = r#"prose {"status":"success","summary":"s","plan":{"steps":[{"id":1}]}}"#;
		let env = Envelope::from_agent_text(text).expect("parses");
		assert_eq!(env.payload["plan"]["steps"][0]["id"], 1);
	}

	#[test]
	fn unterminated_object_is_not_a_candidate() {
		let text = r#"{"status":"success","summary":"ok"} then {"status":"fail""#;
		let env = Envelope::from_agent_text(text).expect("parses the complete one");
		assert!(env.is_success());
	}

	#[test]
	fn prose_only_output_is_rejected() {
		let err = Envelope::from_agent_text("I finished the task.").unwrap_err();
		assert!(matches!(err, EnvelopeError::NoJson));
	}

	#[test]
	fn unknown_status_is_a_contract_violation() {
		let err = Envelope::from_agent_text(r#"{"status":"done"}"#).unwrap_err();
		assert!(matches!(err, EnvelopeError::Invalid(_)));
	}
}
