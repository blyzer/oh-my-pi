//! How a gRPC failure becomes an [`ExecutionError`].
//!
//! The classification decides whether the caller may fall back to a local run
//! (`fallback_eligible`), so it is deliberately a total function with no
//! catch-all: a status code that is not listed here is infrastructure, not
//! something to retry or hide.

use std::{sync::Arc, time::Duration};

use omp_exec::{BackendId, CasOp, Digest, ExecutionError};
use tonic::Code;

/// Where in the exchange a status was returned. The same code means different
/// things at different points: `NOT_FOUND` during an upload query is a normal
/// answer, during execution it is a missing input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
	/// Asking which blobs the server lacks.
	FindMissing,
	/// Uploading blobs.
	Upload,
	/// Submitting or waiting on the execution.
	Execute,
	/// Fetching outputs after the command ran.
	Download,
}

/// Classifies a failed call.
///
/// `limit` is the action timeout, reported on `DEADLINE_EXCEEDED`. `blob` is
/// the digest concerned when the call was about one.
pub fn classify(
	status: tonic::Status,
	phase: Phase,
	backend: &BackendId<str>,
	limit: Duration,
	blob: Option<Digest>,
) -> ExecutionError {
	let backend = backend.to_owned();
	let cas = |op: CasOp| match blob {
		Some(digest) => ExecutionError::Cas { op, digest },
		// Without a digest the failure is the exchange itself: the transport.
		None => ExecutionError::Transport { source: Arc::new(status.clone()) },
	};
	match status.code() {
		Code::Unauthenticated | Code::PermissionDenied => ExecutionError::Auth { backend },
		Code::ResourceExhausted => ExecutionError::Capacity { backend },
		Code::DeadlineExceeded => ExecutionError::TimedOut { limit },
		Code::Cancelled => ExecutionError::Cancelled { remote: omp_exec::RemoteCancel::Requested },
		// A missing blob reported while executing means an input never reached
		// the CAS (or was evicted): it is a storage failure on the upload side.
		Code::FailedPrecondition if phase == Phase::Execute => cas(CasOp::Upload),
		Code::NotFound if matches!(phase, Phase::Execute | Phase::Upload) => cas(CasOp::Upload),
		Code::NotFound | Code::DataLoss if phase == Phase::Download => cas(CasOp::Download),
		Code::Unavailable | Code::Aborted => ExecutionError::Transport { source: Arc::new(status) },
		Code::Internal | Code::Unknown | Code::DataLoss | Code::Unimplemented => {
			ExecutionError::Infrastructure { source: Arc::new(status) }
		},
		// Anything else is the caller's request being wrong, which retrying or
		// falling back cannot fix.
		Code::Ok
		| Code::InvalidArgument
		| Code::AlreadyExists
		| Code::FailedPrecondition
		| Code::NotFound
		| Code::OutOfRange => ExecutionError::Config { source: Arc::new(status) },
	}
}

#[cfg(test)]
mod tests {
	use omp_core::Hash32;
	use omp_exec::{Stage, fallback_eligible};

	use super::*;

	fn backend() -> BackendId {
		BackendId::new("remote")
	}

	fn run(code: Code, phase: Phase, blob: Option<Digest>) -> ExecutionError {
		classify(tonic::Status::new(code, "x"), phase, &backend(), Duration::from_secs(60), blob)
	}

	fn digest() -> Option<Digest> {
		Some(Digest::new(Hash32::sum(b"x"), 1))
	}

	#[test]
	fn credential_problems_are_auth_and_fallback_eligible_only_before_acceptance() {
		for code in [Code::Unauthenticated, Code::PermissionDenied] {
			let error = run(code, Phase::Execute, None);
			assert!(matches!(error, ExecutionError::Auth { .. }), "{code:?}");
			assert!(fallback_eligible(&error, Stage::BeforeAcceptance));
			assert!(!fallback_eligible(&error, Stage::AfterAcceptance));
		}
	}

	#[test]
	fn exhaustion_is_capacity_and_a_deadline_is_a_timeout_that_never_falls_back() {
		assert!(matches!(
			run(Code::ResourceExhausted, Phase::Execute, None),
			ExecutionError::Capacity { .. }
		));
		let timeout = run(Code::DeadlineExceeded, Phase::Execute, None);
		assert!(
			matches!(timeout, ExecutionError::TimedOut { limit } if limit == Duration::from_secs(60))
		);
		assert!(!fallback_eligible(&timeout, Stage::BeforeAcceptance));
	}

	#[test]
	fn a_missing_input_at_execution_is_an_upload_side_storage_failure() {
		for code in [Code::FailedPrecondition, Code::NotFound] {
			let error = run(code, Phase::Execute, digest());
			assert!(
				matches!(error, ExecutionError::Cas { op: CasOp::Upload, .. }),
				"{code:?}: {error:?}"
			);
			assert!(fallback_eligible(&error, Stage::BeforeAcceptance));
		}
	}

	#[test]
	fn a_missing_output_is_a_download_failure_and_never_falls_back() {
		let error = run(Code::NotFound, Phase::Download, digest());
		assert!(matches!(error, ExecutionError::Cas { op: CasOp::Download, .. }));
		assert!(!fallback_eligible(&error, Stage::BeforeAcceptance));
	}

	#[test]
	fn unavailable_is_transport_and_internal_is_infrastructure() {
		assert!(matches!(
			run(Code::Unavailable, Phase::Execute, None),
			ExecutionError::Transport { .. }
		));
		assert!(matches!(
			run(Code::Internal, Phase::Execute, None),
			ExecutionError::Infrastructure { .. }
		));
	}

	#[test]
	fn a_bad_request_is_a_configuration_error_that_is_never_masked() {
		for code in [Code::InvalidArgument, Code::OutOfRange, Code::AlreadyExists] {
			let error = run(code, Phase::Upload, None);
			assert!(matches!(error, ExecutionError::Config { .. }), "{code:?}");
			assert!(!fallback_eligible(&error, Stage::BeforeAcceptance));
		}
	}

	#[test]
	fn a_cas_status_without_a_digest_degrades_to_transport_not_a_panic() {
		assert!(matches!(
			run(Code::NotFound, Phase::Execute, None),
			ExecutionError::Transport { .. }
		));
	}

	#[test]
	fn the_status_message_is_kept_as_the_typed_source() {
		use std::error::Error as _;
		let error = classify(
			tonic::Status::new(Code::Unavailable, "connection reset"),
			Phase::Execute,
			&backend(),
			Duration::from_secs(1),
			None,
		);
		assert!(
			error
				.source()
				.expect("typed source")
				.to_string()
				.contains("connection reset")
		);
	}
}
