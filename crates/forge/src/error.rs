//! Typed forge failures.
//!
//! Variants carry identifying facts. They do not carry a rendered reason
//! string. Display text lives on the `#[error]` attribute.

use std::mem::size_of;

use omp_core::Str;
use thiserror::Error;

use crate::{component::ComponentError, op::ForgeOp, scheme::SchemeError};

/// A forge operation failed.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ForgeError {
	/// The repository does not exist on the host.
	#[error("forge repository {owner}/{name} was not found on {host}")]
	RepoNotFound {
		/// Opaque host.
		host:  Str,
		/// Owner path.
		owner: Str,
		/// Repository name.
		name:  Str,
	},
	/// The change request number does not exist in the repository.
	#[error("forge change request {number} was not found in {owner}/{name} on {host}")]
	ChangeRequestNotFound {
		/// Opaque host.
		host:   Str,
		/// Owner path.
		owner:  Str,
		/// Repository name.
		name:   Str,
		/// Change-request number.
		number: u64,
	},
	/// The issue number does not exist in the repository.
	#[error("forge issue {number} was not found in {owner}/{name} on {host}")]
	IssueNotFound {
		/// Opaque host.
		host:   Str,
		/// Owner path.
		owner:  Str,
		/// Repository name.
		name:   Str,
		/// Issue number.
		number: u64,
	},
	/// The commit identity does not exist in the repository.
	#[error("forge commit {commit} was not found in {owner}/{name} on {host}")]
	CommitNotFound {
		/// Opaque host.
		host:   Str,
		/// Owner path.
		owner:  Str,
		/// Repository name.
		name:   Str,
		/// Commit identity that was requested.
		commit: Str,
	},
	/// The file path does not exist at the requested revision.
	#[error("forge file {path} was not found in {owner}/{name} on {host}")]
	FileNotFound {
		/// Opaque host.
		host:  Str,
		/// Owner path.
		owner: Str,
		/// Repository name.
		name:  Str,
		/// Repository-relative path.
		path:  Str,
	},
	/// This host does not implement the operation.
	#[error("forge operation {op} is not supported by this host")]
	Unsupported {
		/// The operation the host refused.
		op: ForgeOp,
	},
	/// The host rejected the operation with a numeric status.
	#[error("forge host rejected {op} with status {status}")]
	Rejected {
		/// The operation that was rejected.
		op:     ForgeOp,
		/// Host status code. The contract does not interpret it.
		status: u16,
	},
	/// The host could not be reached for the operation.
	#[error("forge host is unavailable for {op}")]
	Unavailable {
		/// The operation that could not be started.
		op: ForgeOp,
	},
	/// No adapter is registered for the host.
	///
	/// The in-core host router returns this when the request's host is
	/// absent from its registry. The host string is ASCII-lowercased.
	#[error("forge host {host} has no adapter for {op}")]
	Unregistered {
		/// ASCII-lowercased host that was requested.
		host: Str,
		/// Operation the router was performing.
		op:   ForgeOp,
	},
	/// A mutating operation was refused before it reached an adapter.
	///
	/// The router selects `op` from the method it implements. An adapter
	/// does not choose it.
	#[error("forge operation {op} on {host} was not approved")]
	NotApproved {
		/// Operation that was refused.
		op:   ForgeOp,
		/// ASCII-lowercased host the operation targeted.
		host: Str,
	},
	/// The operation named no repository, so a host could not be selected.
	#[error("forge operation {op} has no repository host")]
	Unscoped {
		/// Operation that could not be routed.
		op: ForgeOp,
	},
	/// A `pr://` or `issue://` URL could not be parsed.
	#[error(transparent)]
	Scheme(#[from] SchemeError),
	/// A ref component failed validation.
	#[error(transparent)]
	Component(#[from] ComponentError),
}

const _: () = assert!(size_of::<ForgeError>() <= 160, "ForgeError must stay compact");
