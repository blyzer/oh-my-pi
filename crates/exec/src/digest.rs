//! Content digests for inputs, logs, and artifacts.

use omp_core::Hash32;
use serde::{Deserialize, Serialize};

/// A content-addressed blob: SHA-256 of its bytes plus its length.
///
/// Field-for-field the same shape as `omp_journal::BlobRef` and REAPI's
/// `Digest`. It is a local type because `omp-exec` stays a pure domain crate:
/// depending on `omp-journal` would pull its blob store, archive codecs, and
/// filesystem capabilities into every consumer of these types. When a second
/// crate needs the shared shape, `BlobRef` moves into `omp-core` (ADR 0040,
/// Part B, "Digests") and this type is replaced by it, never kept beside it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Digest {
	/// SHA-256 of the blob contents.
	pub hash: Hash32,
	/// Blob length in bytes.
	pub size: u64,
}

impl Digest {
	/// Pairs a content hash with the length of the bytes it hashes.
	#[inline]
	pub const fn new(hash: Hash32, size: u64) -> Self {
		Self { hash, size }
	}
}
