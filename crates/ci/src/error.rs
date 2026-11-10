//! Typed CI failures.
//!
//! Variants carry identifying facts. They do not carry a rendered reason
//! string. Display text lives on the `#[error]` attribute.

use std::mem::size_of;

use omp_core::Str;
use thiserror::Error;

use crate::{op::CiOp, route::RouteError};

/// Longest provider, pipeline, or job identifier accepted by a checked
/// constructor.
pub const MAX_ID_LEN: usize = 255;

/// A CI operation failed.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum CiError {
	/// No adapter is routed for this provider.
	///
	/// The host router returns this when a lookup has no operation to name:
	/// capability discovery, and a details URL resolved through
	/// [`crate::route`]. An operation the router was performing uses
	/// [`Self::Unregistered`].
	#[error("ci provider {provider} is not routed")]
	ProviderNotFound {
		/// Provider id. The host router ASCII-lowercases it.
		provider: Str,
	},
	/// The pipeline id does not exist on the provider.
	#[error("ci pipeline {pipeline} was not found on {provider}")]
	PipelineNotFound {
		/// Opaque provider id.
		provider: Str,
		/// Pipeline id that was requested.
		pipeline: Str,
	},
	/// The job id does not exist in the pipeline.
	#[error("ci job {job} was not found in pipeline {pipeline} on {provider}")]
	JobNotFound {
		/// Opaque provider id.
		provider: Str,
		/// Pipeline id that was requested.
		pipeline: Str,
		/// Job id that was requested.
		job:      Str,
	},
	/// The operation does not accept a job id.
	#[error("ci operation {op} does not take a job")]
	UnexpectedJob {
		/// The operation that rejected the job.
		op: CiOp,
	},
	/// The operation requires a job id.
	#[error("ci operation {op} requires a job")]
	MissingJob {
		/// The operation that required a job.
		op: CiOp,
	},
	/// This host does not implement the operation.
	#[error("ci operation {op} is not supported by this host")]
	Unsupported {
		/// The operation the host refused.
		op: CiOp,
	},
	/// The host rejected the operation with a numeric status.
	#[error("ci host rejected {op} with status {status}")]
	Rejected {
		/// The operation that was rejected.
		op:     CiOp,
		/// Host status code. The contract does not interpret it.
		status: u16,
	},
	/// The host could not be reached for the operation.
	#[error("ci host is unavailable for {op}")]
	Unavailable {
		/// The operation that could not be started.
		op: CiOp,
	},
	/// No adapter is registered for the provider host.
	///
	/// The in-core host router returns this when the request's host is
	/// absent from its registry. The provider string is ASCII-lowercased.
	/// The operation is the one the method names.
	#[error("ci provider {provider} has no adapter for {op}")]
	Unregistered {
		/// ASCII-lowercased provider host.
		provider: Str,
		/// Operation the router was performing.
		op:       CiOp,
	},
	/// A mutating operation was refused before it reached an adapter.
	///
	/// The router selects `op` from the method it implements. An adapter
	/// does not choose it.
	#[error("ci operation {op} on {provider} was not approved")]
	NotApproved {
		/// Operation that was refused.
		op:       CiOp,
		/// ASCII-lowercased provider host.
		provider: Str,
	},
	/// The capabilities registered for this provider do not allow the operation.
	///
	/// The in-core host router returns this before the adapter runs and
	/// before a write is submitted for approval. [`CiOp::Status`] is always
	/// allowed. Every other operation follows [`crate::Capabilities::allows`]
	/// on the flags stored at registration.
	#[error("ci operation {op} is outside the capabilities of {provider}")]
	NotCapable {
		/// Operation that was refused.
		op:       CiOp,
		/// ASCII-lowercased provider host.
		provider: Str,
	},
	/// A trigger carried more inputs than [`crate::MAX_INPUTS`].
	#[error("ci trigger has {len} inputs, above {max}")]
	TooManyInputs {
		/// How many inputs were offered, including the one that did not fit.
		len: usize,
		/// The cap that was exceeded.
		max: usize,
	},
	/// An input value exceeded [`crate::MAX_INPUT_VALUE`].
	#[error("ci input value is {len} bytes, above {max}")]
	InputTooLong {
		/// The length that was rejected.
		len: usize,
		/// The cap that was exceeded.
		max: usize,
	},
	/// An input value contained a control character.
	#[error("ci input value contains a character that is not allowed")]
	InputInvalid,
	/// An identifier failed validation.
	#[error(transparent)]
	Id(
		/// The identifier failure.
		#[from]
		IdError,
	),
	/// A details URL could not be classified.
	#[error(transparent)]
	Route(
		/// The route failure.
		#[from]
		RouteError,
	),
}

const _: () = assert!(size_of::<CiError>() <= 96, "CiError must stay compact");

/// A provider, pipeline, or job identifier failed validation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Error)]
pub enum IdError {
	/// The identifier was empty.
	#[error("ci identifier is empty")]
	Empty,
	/// The identifier exceeded [`MAX_ID_LEN`].
	#[error("ci identifier is {len} bytes, above {max}")]
	TooLong {
		/// The length that was rejected.
		len: usize,
		/// The cap that was exceeded.
		max: usize,
	},
	/// The identifier was `.`, `..`, or contained a separator or control.
	#[error("ci identifier contains a character that is not allowed")]
	Invalid,
}

/// Checks a provider, pipeline, or job id.
///
/// The charset is opaque text without spaces, separators (`/`, `\`, `?`,
/// `#`), or ASCII controls. `.` and `..` are rejected.
pub fn check_id(text: &str) -> Result<(), IdError> {
	if text.is_empty() {
		return Err(IdError::Empty);
	}
	if text == "." || text == ".." {
		return Err(IdError::Invalid);
	}
	if text.len() > MAX_ID_LEN {
		return Err(IdError::TooLong { len: text.len(), max: MAX_ID_LEN });
	}
	if !text.bytes().all(is_id_byte) {
		return Err(IdError::Invalid);
	}
	Ok(())
}

const fn is_id_byte(byte: u8) -> bool {
	!byte.is_ascii_control() && !matches!(byte, b' ' | b'/' | b'\\' | b'?' | b'#')
}
