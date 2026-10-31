//! Backend-neutral execution resume vocabulary.

use omp_core::Str;
use serde::{Deserialize, Serialize};
use strum::{Display, EnumString, IntoStaticStr};

use crate::ids::{BackendId, ExecutionId};

/// How a backend can resume an accepted execution after the watcher stops.
#[derive(
	Debug,
	Clone,
	Copy,
	PartialEq,
	Eq,
	Hash,
	Display,
	EnumString,
	IntoStaticStr,
	Serialize,
	Deserialize,
)]
#[strum(serialize_all = "kebab-case")]
pub enum ResumeSupport {
	/// The backend cannot resume an accepted execution.
	Unsupported,
	/// The backend can resume by watching the opaque operation handle.
	Reattach,
	/// The backend can resume from a durable execution snapshot.
	Snapshot,
}

/// Version of the opaque reattach handle stored in the session journal.

#[derive(
	Debug,
	Clone,
	Copy,
	PartialEq,
	Eq,
	Hash,
	Display,
	EnumString,
	IntoStaticStr,
	Serialize,
	Deserialize,
)]
#[strum(serialize_all = "kebab-case")]
pub enum ReattachSchema {
	/// The first journaled handle shape.
	V1,
}

/// Backend-neutral identity for an accepted execution.
///
/// `operation` is opaque to `omp-exec`; provider-specific fields stay inside
/// the backend and are not added to this handle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReattachHandle {
	/// Handle schema version.
	pub schema:    ReattachSchema,
	/// Backend that owns the accepted work.
	pub backend:   BackendId,
	/// Opaque backend operation identity.
	pub operation: Str,
	/// Execution identity minted by the caller.
	pub id:        ExecutionId,
}

impl ReattachHandle {
	/// Creates the current handle schema.
	#[must_use]
	pub fn new(backend: BackendId, operation: Str, id: ExecutionId) -> Self {
		Self { schema: ReattachSchema::V1, backend, operation, id }
	}
}
