//! Registration failures.
//!
//! Routing failures travel as [`omp_ci::CiError`] because [`omp_ci::CiHost`]
//! returns that type. This enum is only for building the registry.

use omp_ci::IdError;
use omp_core::Str;
use thiserror::Error;

/// A host could not be added to the registry.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum RegisterError {
	/// The ASCII-lowercased host is already present.
	#[error("ci host {host} is already registered")]
	Duplicate {
		/// ASCII-lowercased host that collided.
		host: Str,
	},
	/// The host failed the contract identifier check.
	#[error(transparent)]
	Id(#[from] IdError),
}
