//! What a REAPI server says it can do, reduced to the facts this client acts
//! on.
//!
//! Capabilities decide three things before any work is sent: whether the
//! server speaks SHA-256 (the only digest the domain uses), whether it will
//! execute at all, and which compressors and batch sizes the CAS accepts. A
//! server that fails one of these is refused up front with a typed reason
//! instead of failing halfway through an upload.

use omp_exec::{CacheSupport, CancelSupport};

use crate::proto::build::bazel::remote::execution::v2 as wire;

/// REAPI's `DigestFunction.Value::SHA256`.
const SHA256: i32 = wire::digest_function::Value::Sha256 as i32;
/// REAPI's `Compressor.Value::ZSTD`.
const ZSTD: i32 = wire::compressor::Value::Zstd as i32;

/// The facts the client acts on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Capabilities {
	/// Whether the server accepts zstd-compressed blobs on ByteStream.
	pub zstd:            bool,
	/// Whether the server accepts zstd inside `BatchUpdateBlobs`.
	pub zstd_batch:      bool,
	/// The largest total payload of one batch call; `None` when the server
	/// states no limit.
	pub max_batch_bytes: Option<u64>,
	/// The largest single blob; `None` when unbounded.
	pub max_blob_bytes:  Option<u64>,
	/// The action cache can be written.
	pub action_cache:    CacheSupport,
	/// The server executes actions.
	pub execution:       bool,
	/// The server states `CancelOperation` is not offered. REAPI has no
	/// capability field for it, so this is always `Unknown` until a call is
	/// answered `UNIMPLEMENTED`.
	pub cancel:          CancelSupport,
}

/// Why a server cannot be used.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum CapabilityError {
	/// The server did not state its cache capabilities.
	#[error("server stated no cache capabilities")]
	NoCache,
	/// The server does not support SHA-256, the only digest function used.
	#[error("server does not support the SHA-256 digest function")]
	NoSha256,
	/// The server does not execute actions.
	#[error("server does not execute actions")]
	NoExecution,
}

/// Interprets a server's `GetCapabilities` reply.
///
/// # Errors
///
/// [`CapabilityError`] when the server cannot serve this client at all.
pub fn interpret(server: &wire::ServerCapabilities) -> Result<Capabilities, CapabilityError> {
	let cache = server
		.cache_capabilities
		.as_ref()
		.ok_or(CapabilityError::NoCache)?;
	if !cache.digest_functions.contains(&SHA256) {
		return Err(CapabilityError::NoSha256);
	}
	let execution = server
		.execution_capabilities
		.as_ref()
		.is_some_and(|execution| {
			execution.exec_enabled
				&& (execution.digest_function == SHA256 || execution.digest_functions.contains(&SHA256))
		});
	if !execution {
		return Err(CapabilityError::NoExecution);
	}
	let update = cache
		.action_cache_update_capabilities
		.as_ref()
		.is_some_and(|update| update.update_enabled);
	Ok(Capabilities {
		zstd: cache.supported_compressors.contains(&ZSTD),
		zstd_batch: cache.supported_batch_update_compressors.contains(&ZSTD),
		max_batch_bytes: positive(cache.max_batch_total_size_bytes),
		max_blob_bytes: positive(cache.max_cas_blob_size_bytes),
		action_cache: if update {
			CacheSupport::ReadWrite
		} else {
			CacheSupport::Read
		},
		execution,
		cancel: CancelSupport::BestEffort,
	})
}

/// REAPI states "no limit" as zero.
fn positive(value: i64) -> Option<u64> {
	u64::try_from(value).ok().filter(|value| *value > 0)
}

#[cfg(test)]
mod tests {
	use super::*;

	fn server() -> wire::ServerCapabilities {
		wire::ServerCapabilities {
			cache_capabilities: Some(wire::CacheCapabilities {
				digest_functions: vec![SHA256],
				action_cache_update_capabilities: Some(wire::ActionCacheUpdateCapabilities {
					update_enabled: true,
				}),
				max_batch_total_size_bytes: 4 * 1024 * 1024,
				supported_compressors: vec![ZSTD],
				supported_batch_update_compressors: vec![],
				..Default::default()
			}),
			execution_capabilities: Some(wire::ExecutionCapabilities {
				digest_function: SHA256,
				exec_enabled: true,
				..Default::default()
			}),
			..Default::default()
		}
	}

	#[test]
	fn a_full_featured_server_is_read_in_full() {
		let caps = interpret(&server()).expect("usable");
		assert!(caps.zstd);
		assert!(!caps.zstd_batch, "batch compression is a separate capability");
		assert_eq!(caps.max_batch_bytes, Some(4 * 1024 * 1024));
		assert_eq!(caps.max_blob_bytes, None, "zero means unbounded");
		assert_eq!(caps.action_cache, CacheSupport::ReadWrite);
		assert!(caps.execution);
	}

	#[test]
	fn a_read_only_action_cache_is_not_writable() {
		let mut server = server();
		server
			.cache_capabilities
			.as_mut()
			.expect("cache")
			.action_cache_update_capabilities = None;
		assert_eq!(interpret(&server).expect("usable").action_cache, CacheSupport::Read);
	}

	#[test]
	fn a_server_without_sha256_is_refused() {
		let mut server = server();
		server
			.cache_capabilities
			.as_mut()
			.expect("cache")
			.digest_functions = vec![wire::digest_function::Value::Sha1 as i32];
		assert_eq!(interpret(&server), Err(CapabilityError::NoSha256));
	}

	#[test]
	fn a_server_that_does_not_execute_is_refused() {
		let mut disabled = server();
		disabled
			.execution_capabilities
			.as_mut()
			.expect("exec")
			.exec_enabled = false;
		assert_eq!(interpret(&disabled), Err(CapabilityError::NoExecution));
		let mut absent = server();
		absent.execution_capabilities = None;
		assert_eq!(interpret(&absent), Err(CapabilityError::NoExecution));
	}

	#[test]
	fn execution_with_only_the_list_form_of_the_digest_function_is_accepted() {
		let mut server = server();
		let exec = server.execution_capabilities.as_mut().expect("exec");
		exec.digest_function = 0;
		exec.digest_functions = vec![SHA256];
		assert!(interpret(&server).expect("usable").execution);
	}

	#[test]
	fn a_server_with_no_cache_capabilities_is_refused() {
		let mut server = server();
		server.cache_capabilities = None;
		assert_eq!(interpret(&server), Err(CapabilityError::NoCache));
	}
}
