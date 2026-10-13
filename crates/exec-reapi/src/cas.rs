//! Content-addressed storage helpers: where blob bytes come from, how a blob
//! is named on ByteStream, and how a set of missing blobs is split between
//! batch calls and streamed uploads.
//!
//! Everything here is a pure function of its inputs. The gRPC calls that use
//! it live in the client, so the splitting rules and the resource-name grammar
//! can be tested without a server.

use std::{future::Future, io};

use bytes::Bytes;
use omp_exec::Digest;

use crate::capabilities::Capabilities;

/// REAPI's default batch ceiling when a server states none (ADR 0040, Part
/// D.4: "`0` is treated as 4 MiB").
pub const DEFAULT_BATCH_BYTES: u64 = 4 * 1024 * 1024;

/// Per-blob overhead inside a `BatchUpdateBlobs` request that counts against
/// the server's total, as a margin: a digest (64 hex + size) and framing.
const BATCH_ENTRY_OVERHEAD: u64 = 128;

/// Where the bytes of a blob come from.
///
/// The composing crate implements it over the environment's blob service, so
/// this crate holds no second copy of the content and never reads a path
/// itself (ADR 0040, PR 1b). It is never used as `dyn`: futures are unboxed.
pub trait BlobSource: Clone + Send + Sync + 'static {
	/// Reads one whole blob. Used for blobs small enough for a batch call.
	fn read(&self, digest: Digest) -> impl Future<Output = io::Result<Bytes>> + Send + '_;

	/// Reads `len` bytes starting at `offset`, for streaming a large blob in
	/// chunks without holding it whole.
	fn read_range(
		&self,
		digest: Digest,
		offset: u64,
		len: u64,
	) -> impl Future<Output = io::Result<Bytes>> + Send + '_;
}

/// Which compression a transfer uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Compression {
	/// Bytes as they are.
	Identity,
	/// zstd; only when the server advertised it.
	Zstd,
}

/// The ByteStream resource name for reading a blob.
///
/// `{instance}/blobs/{hash}/{size}` or, compressed,
/// `{instance}/compressed-blobs/zstd/{hash}/{size}`. An empty instance omits
/// the leading slash. The digest function segment is omitted because SHA-256
/// is the only function this crate speaks and REAPI says it MUST be omitted
/// for it.
pub fn read_resource(instance: &str, hash: &str, size: u64, compression: Compression) -> String {
	let prefix = prefix(instance);
	match compression {
		Compression::Identity => format!("{prefix}blobs/{hash}/{size}"),
		Compression::Zstd => format!("{prefix}compressed-blobs/zstd/{hash}/{size}"),
	}
}

/// The ByteStream resource name for writing a blob.
///
/// `uuid` is a client-chosen version 4 UUID that keeps concurrent uploads of
/// the same data apart.
pub fn write_resource(
	instance: &str,
	uuid: &str,
	hash: &str,
	size: u64,
	compression: Compression,
) -> String {
	let prefix = prefix(instance);
	match compression {
		Compression::Identity => format!("{prefix}uploads/{uuid}/blobs/{hash}/{size}"),
		Compression::Zstd => {
			format!("{prefix}uploads/{uuid}/compressed-blobs/zstd/{hash}/{size}")
		},
	}
}

fn prefix(instance: &str) -> String {
	if instance.is_empty() {
		String::new()
	} else {
		format!("{instance}/")
	}
}

/// How a set of blobs goes up.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct UploadPlan {
	/// Groups of blobs, each group sent as one `BatchUpdateBlobs` call.
	pub batches:  Vec<Vec<Digest>>,
	/// Blobs too large for a batch, each sent through ByteStream `Write`.
	pub streamed: Vec<Digest>,
}

/// Splits `missing` into batch calls and streamed uploads.
///
/// A blob whose size plus overhead exceeds the batch ceiling cannot be in a
/// batch and is streamed. The rest are packed in order into groups whose total
/// stays within the ceiling. A server that states no ceiling gets
/// [`DEFAULT_BATCH_BYTES`]. The input order is kept so the result is
/// deterministic.
pub fn plan_upload(missing: &[Digest], capabilities: &Capabilities) -> UploadPlan {
	let ceiling = capabilities.max_batch_bytes.unwrap_or(DEFAULT_BATCH_BYTES);
	let mut plan = UploadPlan::default();
	let mut current: Vec<Digest> = Vec::new();
	let mut current_bytes = 0_u64;
	for &digest in missing {
		let cost = digest.size.saturating_add(BATCH_ENTRY_OVERHEAD);
		if cost > ceiling {
			plan.streamed.push(digest);
			continue;
		}
		if current_bytes.saturating_add(cost) > ceiling && !current.is_empty() {
			plan.batches.push(std::mem::take(&mut current));
			current_bytes = 0;
		}
		current.push(digest);
		current_bytes = current_bytes.saturating_add(cost);
	}
	if !current.is_empty() {
		plan.batches.push(current);
	}
	plan
}

#[cfg(test)]
mod tests {
	use omp_core::Hash32;
	use omp_exec::{CacheSupport, CancelSupport};

	use super::*;

	fn caps(max_batch_bytes: Option<u64>) -> Capabilities {
		Capabilities {
			zstd: false,
			zstd_batch: false,
			max_batch_bytes,
			max_blob_bytes: None,
			action_cache: CacheSupport::Read,
			execution: true,
			cancel: CancelSupport::BestEffort,
		}
	}

	fn blob(tag: u8, size: u64) -> Digest {
		Digest::new(Hash32::new([tag; 32]), size)
	}

	#[test]
	fn resource_names_follow_the_reapi_grammar() {
		let hash = "ab".repeat(32);
		assert_eq!(
			read_resource("main", &hash, 7, Compression::Identity),
			format!("main/blobs/{hash}/7")
		);
		assert_eq!(
			read_resource("", &hash, 7, Compression::Zstd),
			format!("compressed-blobs/zstd/{hash}/7"),
			"an empty instance omits the leading slash"
		);
		assert_eq!(
			write_resource("main", "u-1", &hash, 7, Compression::Identity),
			format!("main/uploads/u-1/blobs/{hash}/7")
		);
		assert_eq!(
			write_resource("", "u-1", &hash, 7, Compression::Zstd),
			format!("uploads/u-1/compressed-blobs/zstd/{hash}/7")
		);
	}

	#[test]
	fn small_blobs_are_packed_in_order_within_the_ceiling() {
		let ceiling = 1000;
		let missing = [blob(1, 300), blob(2, 300), blob(3, 300), blob(4, 300)];
		let plan = plan_upload(&missing, &caps(Some(ceiling)));
		assert!(plan.streamed.is_empty());
		let flat: Vec<Digest> = plan.batches.iter().flatten().copied().collect();
		assert_eq!(flat, missing, "order is kept and nothing is lost");
		for batch in &plan.batches {
			let total: u64 = batch.iter().map(|d| d.size + BATCH_ENTRY_OVERHEAD).sum();
			assert!(total <= ceiling, "{total} exceeds {ceiling}");
		}
		assert!(plan.batches.len() >= 2, "four 300-byte blobs cannot share a 1000-byte batch");
	}

	#[test]
	fn a_blob_over_the_ceiling_is_streamed_not_batched() {
		let plan = plan_upload(&[blob(1, 10), blob(2, 5000), blob(3, 10)], &caps(Some(1000)));
		assert_eq!(plan.streamed, [blob(2, 5000)]);
		assert_eq!(plan.batches, [vec![blob(1, 10), blob(3, 10)]]);
	}

	#[test]
	fn a_blob_that_only_fits_without_its_overhead_is_streamed() {
		let plan = plan_upload(&[blob(1, 1000)], &caps(Some(1000)));
		assert_eq!(plan.streamed, [blob(1, 1000)]);
		assert!(plan.batches.is_empty());
	}

	#[test]
	fn a_server_with_no_stated_ceiling_gets_four_mebibytes() {
		let just_under = DEFAULT_BATCH_BYTES - BATCH_ENTRY_OVERHEAD;
		let plan = plan_upload(&[blob(1, just_under), blob(2, just_under + 1)], &caps(None));
		assert_eq!(plan.batches, [vec![blob(1, just_under)]]);
		assert_eq!(plan.streamed, [blob(2, just_under + 1)]);
	}

	#[test]
	fn nothing_missing_plans_nothing() {
		assert_eq!(plan_upload(&[], &caps(Some(1000))), UploadPlan::default());
	}

	#[test]
	fn a_huge_size_does_not_overflow_the_arithmetic() {
		let plan = plan_upload(&[blob(1, u64::MAX)], &caps(Some(1000)));
		assert_eq!(plan.streamed, [blob(1, u64::MAX)]);
	}
}
