//! Registration failures.
//!
//! Routing failures travel as [`omp_forge::ForgeError`] because
//! [`omp_forge::ForgeHost`] returns that type. This enum is only for
//! building the registry.

use omp_core::Str;
use omp_forge::ComponentError;
use thiserror::Error;

/// A host could not be added to the registry.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum RegisterError {
	/// The ASCII-lowercased host is already present.
	#[error("forge host {host} is already registered")]
	Duplicate {
		/// ASCII-lowercased host that collided.
		host: Str,
	},
	/// The host failed the contract charset check.
	#[error(transparent)]
	Component(#[from] ComponentError),
}
