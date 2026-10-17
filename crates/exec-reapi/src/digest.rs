//! Conversion between the domain [`Digest`] and the wire `Digest` message.
//!
//! REAPI addresses a blob by the lowercase hexadecimal SHA-256 of its bytes
//! plus its length. The domain digest holds the raw 32 hash bytes, so the
//! conversion is a hex encode or a strict hex decode, and a wire digest that is
//! not a well-formed SHA-256 is refused rather than passed through.

use std::str::FromStr as _;

use omp_core::{Hash32, Hash32ParseError};
use omp_exec::Digest;

use crate::proto::build::bazel::remote::execution::v2 as wire;

/// A wire digest that cannot name a SHA-256 blob.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DigestError {
	/// The hash is not 64 lowercase hexadecimal characters.
	#[error("wire digest hash is not a lowercase SHA-256 hex string")]
	Hash(#[source] Hash32ParseError),
	/// The size is negative.
	#[error("wire digest size {size} is negative")]
	NegativeSize {
		/// The size the wire message carried.
		size: i64,
	},
}

/// Converts a domain digest to its wire form.
pub fn to_wire(digest: Digest) -> wire::Digest {
	wire::Digest {
		hash:       digest.hash.to_hex().as_str().to_owned(),
		// A blob larger than `i64::MAX` bytes cannot exist; saturate instead of
		// wrapping so an impossible size is rejected by the server, not aliased.
		size_bytes: i64::try_from(digest.size).unwrap_or(i64::MAX),
	}
}

/// Converts a wire digest to the domain form.
///
/// # Errors
///
/// [`DigestError`] when the hash is not a lowercase SHA-256 hex string or the
/// size is negative.
pub fn from_wire(digest: &wire::Digest) -> Result<Digest, DigestError> {
	let hash = Hash32::from_str(&digest.hash).map_err(DigestError::Hash)?;
	let size = u64::try_from(digest.size_bytes)
		.map_err(|_| DigestError::NegativeSize { size: digest.size_bytes })?;
	Ok(Digest::new(hash, size))
}

#[cfg(test)]
mod tests {
	use super::*;

	fn sample() -> Digest {
		Digest::new(Hash32::sum(b"hello"), 5)
	}

	#[test]
	fn a_digest_survives_the_round_trip() {
		let digest = sample();
		assert_eq!(from_wire(&to_wire(digest)).expect("round trip"), digest);
	}

	#[test]
	fn the_wire_hash_is_the_known_sha256_of_the_bytes() {
		assert_eq!(
			to_wire(sample()).hash,
			"2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
		);
	}

	#[test]
	fn the_empty_blob_has_the_well_known_empty_digest() {
		let empty = to_wire(Digest::new(Hash32::sum(b""), 0));
		assert_eq!(empty.hash, "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
		assert_eq!(empty.size_bytes, 0);
	}

	#[test]
	fn uppercase_short_and_non_hex_hashes_are_refused() {
		let good = to_wire(sample());
		for hash in
			[good.hash.to_uppercase(), good.hash[..63].to_owned(), "z".repeat(64), String::new()]
		{
			let wire = wire::Digest { hash, size_bytes: 5 };
			assert!(matches!(from_wire(&wire), Err(DigestError::Hash(_))), "{wire:?}");
		}
	}

	#[test]
	fn a_negative_size_is_refused() {
		let wire = wire::Digest { size_bytes: -1, ..to_wire(sample()) };
		assert_eq!(from_wire(&wire), Err(DigestError::NegativeSize { size: -1 }));
	}

	#[test]
	fn an_oversize_blob_saturates_instead_of_wrapping() {
		let huge = Digest::new(Hash32::sum(b"x"), u64::MAX);
		assert_eq!(to_wire(huge).size_bytes, i64::MAX);
	}
}
