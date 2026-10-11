//! Typed execution failures and their fallback classification (ADR 0040,
//! Part C.4).
//!
//! A non-zero exit is not an error: it is [`crate::Outcome::Exited`], "the
//! command failed". Everything here is a failure to *run* the command.

use std::{error::Error, sync::Arc, time::Duration};

use omp_adw::PostureError;
use omp_core::Str;
use strum::{Display, EnumDiscriminants, EnumString, IntoStaticStr, VariantArray};

use crate::{
	digest::Digest,
	ids::BackendId,
	platform::{Need, Platform},
};

/// A cause raised by a foreign transport or host layer.
///
/// `omp-exec` is the pure domain: the remote-execution client, the
/// env-protocol client and the capture authority that produce these causes
/// live in crates it must not depend on, so no concrete source type is
/// nameable here. The cause stays a
/// typed error object — never a rendered string — and `Arc` keeps
/// [`ExecutionError`] `Clone` so one settled result can fan out to the journal,
/// telemetry, and the caller.
pub type ForeignError = Arc<dyn Error + Send + Sync + 'static>;

/// Which side of a content-addressed-storage exchange failed.
#[derive(
	Debug, Clone, Copy, PartialEq, Eq, Hash, Display, EnumString, IntoStaticStr, VariantArray,
)]
#[strum(serialize_all = "kebab-case")]
pub enum CasOp {
	/// Asking which input blobs the server lacks.
	FindMissing,
	/// Uploading input blobs.
	Upload,
	/// Downloading output blobs after the command ran.
	Download,
}

impl CasOp {
	/// Whether the operation happens before the command runs (inputs) rather
	/// than after (outputs).
	#[inline]
	pub const fn is_upload_side(self) -> bool {
		match self {
			Self::FindMissing | Self::Upload => true,
			Self::Download => false,
		}
	}
}

/// What became of the remote side of a cancelled run.
#[derive(
	Debug, Clone, Copy, PartialEq, Eq, Hash, Display, EnumString, IntoStaticStr, VariantArray,
)]
#[strum(serialize_all = "kebab-case")]
pub enum RemoteCancel {
	/// No remote work existed: the run was cancelled before acceptance or ran
	/// on a local backend.
	NotStarted,
	/// The backend confirmed the remote work stopped.
	Confirmed,
	/// A cancel was sent but not confirmed; the action timeout bounds any work
	/// that leaked.
	Requested,
	/// The cancel request itself failed; the action timeout bounds the work.
	Failed,
	/// The backend offers no cancel; the action timeout bounds the work.
	Unsupported,
}

/// Why one backend did not admit a request.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Rejection {
	/// The backend does not offer the requested platform.
	#[error("backend does not offer the requested platform")]
	Platform {
		/// The requested platform.
		platform: Platform,
	},
	/// The backend lacks a host capability the request needs.
	#[error("backend lacks a capability the request needs")]
	Need {
		/// The first missing need, in [`Need`] order.
		need: Need,
	},
	/// The request needs the network and the backend enforces none.
	#[error("request needs the network and the backend disables it")]
	Network,
	/// The backend's enforced posture is looser than the request allows.
	///
	/// Placement knows no phase, so the source's `phase` field carries the
	/// request's [`crate::ExecutionId`]; the caller maps it back to its phase.
	#[error("backend's enforced posture does not satisfy the request")]
	Posture {
		/// The failed posture check.
		#[source]
		error: PostureError,
	},
}

/// Why a request could not be placed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Unplaceable {
	/// The request must run on this host and the policy allows only remote
	/// placement.
	#[error("request must run locally and the policy allows only remote placement")]
	LocalRequired,
	/// The request must run remotely and the policy allows only local
	/// placement.
	#[error("request must run remotely and the policy allows only local placement")]
	RemoteRequired,
	/// The request must or should run remotely and no offered backend admits
	/// it.
	#[error("no backend admits the request")]
	NoBackend {
		/// One rejection per offered backend, in offer order; empty when none
		/// was offered.
		rejections: Arc<[Rejection]>,
	},
}

/// A failure to run a command.
#[derive(Debug, Clone, thiserror::Error, EnumDiscriminants)]
#[strum_discriminants(
	name(ExecutionErrorKind),
	doc = "The class of an [`ExecutionError`], for telemetry and fallback records.",
	derive(Hash, Display, EnumString, IntoStaticStr, VariantArray),
	strum(serialize_all = "kebab-case")
)]
pub enum ExecutionError {
	/// No backend the policy allows can run the request.
	#[error("request cannot be placed")]
	Unplaceable {
		/// What placement could not satisfy.
		#[source]
		need: Unplaceable,
	},
	/// The backend refused the credential (`UNAUTHENTICATED`,
	/// `PERMISSION_DENIED`).
	#[error("backend refused the credential")]
	Auth {
		/// The refusing backend.
		backend: BackendId,
	},
	/// A content-addressed-storage exchange failed, including a missing input
	/// blob reported at execution.
	#[error("content-addressed storage exchange failed")]
	Cas {
		/// Which exchange failed.
		op:     CasOp,
		/// The blob concerned.
		digest: Digest,
	},
	/// The backend has no capacity (`RESOURCE_EXHAUSTED`, a quota, persistent
	/// `UNAVAILABLE`).
	#[error("backend has no capacity")]
	Capacity {
		/// The saturated backend.
		backend: BackendId,
	},
	/// The command exceeded its time limit (`DEADLINE_EXCEEDED`).
	#[error("execution timed out")]
	TimedOut {
		/// The limit that was exceeded.
		limit: Duration,
	},
	/// The run was cancelled.
	#[error("execution was cancelled")]
	Cancelled {
		/// What became of the remote side.
		remote: RemoteCancel,
	},
	/// The connection to the backend failed.
	#[error("backend transport failed")]
	Transport {
		/// The transport cause.
		#[source]
		source: ForeignError,
	},
	/// The backend failed internally (`INTERNAL`, a lost worker).
	#[error("backend infrastructure failed")]
	Infrastructure {
		/// The infrastructure cause.
		#[source]
		source: ForeignError,
	},
	/// Capturing the working tree failed.
	#[error("input capture failed")]
	InputCapture {
		/// The capture cause.
		#[source]
		source: ForeignError,
	},
	/// A tracked file matches an upload deny rule, so the run is refused
	/// rather than silently differing from the repository.
	#[error("tracked file matches an upload deny rule")]
	InputRefused {
		/// The file, relative to the input root.
		path: Str,
		/// The rule it matched.
		rule: Str,
	},
	/// Fetching outputs after the command ran failed.
	#[error("output retrieval failed")]
	OutputRetrieval {
		/// The retrieval cause.
		#[source]
		source: ForeignError,
	},
	/// The execution configuration is invalid.
	#[error("execution configuration is invalid")]
	Config {
		/// The configuration cause.
		#[source]
		source: ForeignError,
	},
}

/// When a failure happened relative to the backend accepting the work.
///
/// "Accepted" means the backend returned an operation name
/// ([`crate::ExecutionEvent::Accepted`]); after it the command may have run, so
/// a local rerun could duplicate its effects.
#[derive(
	Debug, Clone, Copy, PartialEq, Eq, Hash, Display, EnumString, IntoStaticStr, VariantArray,
)]
#[strum(serialize_all = "kebab-case")]
pub enum Stage {
	/// Before the backend accepted the work.
	BeforeAcceptance,
	/// After the backend accepted the work.
	AfterAcceptance,
}

/// Whether `error` at `stage` permits running the request locally instead.
///
/// This classifies the failure only; the policy decides whether a fallback
/// happens at all (ADR 0040, Part C.3). Eligible, and only before acceptance:
/// `Transport`, `Capacity`, `Infrastructure`, upload-side `Cas`, and `Auth`
/// (which the caller reports loudly). Never eligible: every failure after
/// acceptance, `TimedOut`, `Cancelled`, `InputRefused`, `InputCapture`,
/// `Config` (a misconfiguration is reported, never masked), `OutputRetrieval`
/// and download-side `Cas` (the command already ran), and `Unplaceable`
/// (placement already weighed every allowed backend; a local alternative is a
/// placement decision, not a fallback).
///
/// One exhaustive match with no wildcard arm: a new variant does not compile
/// until it is classified here.
pub const fn fallback_eligible(error: &ExecutionError, stage: Stage) -> bool {
	match (error, stage) {
		(
			ExecutionError::Transport { .. }
			| ExecutionError::Capacity { .. }
			| ExecutionError::Infrastructure { .. }
			| ExecutionError::Auth { .. },
			Stage::BeforeAcceptance,
		) => true,
		(ExecutionError::Cas { op, .. }, Stage::BeforeAcceptance) => op.is_upload_side(),
		(
			ExecutionError::Transport { .. }
			| ExecutionError::Capacity { .. }
			| ExecutionError::Infrastructure { .. }
			| ExecutionError::Auth { .. }
			| ExecutionError::Cas { .. },
			Stage::AfterAcceptance,
		)
		| (
			ExecutionError::Unplaceable { .. }
			| ExecutionError::TimedOut { .. }
			| ExecutionError::Cancelled { .. }
			| ExecutionError::InputCapture { .. }
			| ExecutionError::InputRefused { .. }
			| ExecutionError::OutputRetrieval { .. }
			| ExecutionError::Config { .. },
			Stage::BeforeAcceptance | Stage::AfterAcceptance,
		) => false,
	}
}

#[cfg(test)]
mod tests {
	use std::{sync::Arc, time::Duration};

	use omp_core::{Hash32, Str};
	use strum::VariantArray;

	use super::*;

	#[derive(Debug, thiserror::Error)]
	#[error("cause")]
	struct Cause;

	fn cause() -> ForeignError {
		Arc::new(Cause)
	}

	fn digest() -> Digest {
		Digest::new(Hash32::default(), 0)
	}

	/// One sample per variant (and per `CasOp`), with its expected
	/// classification `(before acceptance, after acceptance)`.
	fn table() -> Vec<(ExecutionError, bool, bool)> {
		let backend = || BackendId::new_static("remote");
		vec![
			(ExecutionError::Unplaceable { need: Unplaceable::RemoteRequired }, false, false),
			(ExecutionError::Unplaceable { need: Unplaceable::LocalRequired }, false, false),
			(
				ExecutionError::Unplaceable {
					need: Unplaceable::NoBackend { rejections: Arc::new([]) },
				},
				false,
				false,
			),
			(ExecutionError::Auth { backend: backend() }, true, false),
			(ExecutionError::Cas { op: CasOp::FindMissing, digest: digest() }, true, false),
			(ExecutionError::Cas { op: CasOp::Upload, digest: digest() }, true, false),
			(ExecutionError::Cas { op: CasOp::Download, digest: digest() }, false, false),
			(ExecutionError::Capacity { backend: backend() }, true, false),
			(ExecutionError::TimedOut { limit: Duration::from_secs(1) }, false, false),
			(ExecutionError::Cancelled { remote: RemoteCancel::Confirmed }, false, false),
			(ExecutionError::Cancelled { remote: RemoteCancel::NotStarted }, false, false),
			(ExecutionError::Transport { source: cause() }, true, false),
			(ExecutionError::Infrastructure { source: cause() }, true, false),
			(ExecutionError::InputCapture { source: cause() }, false, false),
			(
				ExecutionError::InputRefused {
					path: Str::new_static(".env"),
					rule: Str::new_static(".env"),
				},
				false,
				false,
			),
			(ExecutionError::OutputRetrieval { source: cause() }, false, false),
			(ExecutionError::Config { source: cause() }, false, false),
		]
	}

	#[test]
	fn classification_table_covers_every_variant_at_both_stages() {
		let table = table();
		for kind in ExecutionErrorKind::VARIANTS {
			assert!(
				table
					.iter()
					.any(|(error, ..)| ExecutionErrorKind::from(error) == *kind),
				"classification table misses {kind}"
			);
		}
		for op in CasOp::VARIANTS {
			assert!(
				table.iter().any(
					|(error, ..)| matches!(error, ExecutionError::Cas { op: sample, .. } if sample == op)
				),
				"classification table misses Cas {op}"
			);
		}
		for (error, before, after) in &table {
			assert_eq!(
				fallback_eligible(error, Stage::BeforeAcceptance),
				*before,
				"{error:?} before acceptance"
			);
			assert_eq!(
				fallback_eligible(error, Stage::AfterAcceptance),
				*after,
				"{error:?} after acceptance"
			);
		}
	}

	#[test]
	fn nothing_falls_back_after_acceptance() {
		for (error, ..) in table() {
			assert!(!fallback_eligible(&error, Stage::AfterAcceptance), "{error:?}");
		}
	}

	#[test]
	fn kinds_have_stable_telemetry_names() {
		let kind: &'static str = ExecutionErrorKind::OutputRetrieval.into();
		assert_eq!(kind, "output-retrieval");
		assert_eq!(
			"input-refused".parse::<ExecutionErrorKind>(),
			Ok(ExecutionErrorKind::InputRefused)
		);
	}
}
