//! Client-chosen keys that make one remote mutation exactly-once.

use omp_core::{Str, string_id};
use thiserror::Error;

string_id!(
	/// Client-chosen key that makes one remote mutation exactly-once.
	///
	/// The key is committed in the same journal entry as the message it
	/// names, so replay can never separate them. [`IdempotencyKey::parse`]
	/// validates untrusted input; [`IdempotencyKey::validate`] re-checks a key
	/// that arrived through the infallible constructors every branded id has.
	IdempotencyKey
);

/// Why a key is not an acceptable idempotency key.
#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum IdempotencyKeyError {
	/// The key has no bytes.
	#[error("idempotency key is empty")]
	Empty,
	/// The key is longer than [`IdempotencyKey::MAX_BYTES`].
	#[error("idempotency key is {actual} bytes; maximum is {maximum}")]
	TooLong {
		/// Supplied length in bytes.
		actual:  usize,
		/// Longest accepted length in bytes.
		maximum: usize,
	},
}

impl IdempotencyKey<Str> {
	/// Longest accepted key, in bytes. Keys are journaled and indexed per
	/// session, so the bound keeps one hostile client from inflating either.
	pub const MAX_BYTES: usize = 128;

	/// Wraps client-supplied key text after checking it.
	pub fn parse(text: &str) -> Result<Self, IdempotencyKeyError> {
		Self::check(text)?;
		Ok(Self::new(text))
	}

	/// Checks a key built through the unvalidated constructors.
	pub fn validate(&self) -> Result<(), IdempotencyKeyError> {
		Self::check(self.as_str())
	}

	fn check(text: &str) -> Result<(), IdempotencyKeyError> {
		match text.len() {
			0 => Err(IdempotencyKeyError::Empty),
			actual if actual > Self::MAX_BYTES => {
				Err(IdempotencyKeyError::TooLong { actual, maximum: Self::MAX_BYTES })
			},
			_ => Ok(()),
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn parse_accepts_one_byte_through_the_bound() {
		assert_eq!(IdempotencyKey::parse("k").unwrap().as_str(), "k");
		let longest = "k".repeat(IdempotencyKey::MAX_BYTES);
		assert_eq!(IdempotencyKey::parse(&longest).unwrap().as_str(), longest);
	}

	#[test]
	fn parse_refuses_empty_and_oversized_keys() {
		assert_eq!(IdempotencyKey::parse(""), Err(IdempotencyKeyError::Empty));
		let oversized = "k".repeat(IdempotencyKey::MAX_BYTES + 1);
		assert_eq!(
			IdempotencyKey::parse(&oversized),
			Err(IdempotencyKeyError::TooLong {
				actual:  IdempotencyKey::MAX_BYTES + 1,
				maximum: IdempotencyKey::MAX_BYTES,
			})
		);
	}

	#[test]
	fn validate_rechecks_a_key_built_without_parse() {
		assert_eq!(IdempotencyKey::new("").validate(), Err(IdempotencyKeyError::Empty));
		assert_eq!(IdempotencyKey::new_static("k").validate(), Ok(()));
	}

	#[test]
	fn key_serializes_as_its_bare_text() {
		let key = IdempotencyKey::parse("submit-7").unwrap();
		let json = serde_json::to_string(&key).unwrap();
		assert_eq!(json, r#""submit-7""#);
		assert_eq!(serde_json::from_str::<IdempotencyKey>(&json).unwrap(), key);
	}
}
