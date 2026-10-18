//! What a run reports while it runs and when it settles.

use std::{collections::BTreeMap, sync::Arc};

use omp_core::{CowBytes, Str};
use strum::{Display, EnumString, IntoStaticStr};

use crate::{
	digest::Digest,
	error::{ExecutionError, ExecutionErrorKind},
	ids::{BackendId, ExecutionId},
};

/// How the command ended. A non-zero exit is "the command failed", not an
/// [`ExecutionError`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Outcome {
	/// The process exited with `code`.
	Exited {
		/// Exit code.
		code: i32,
	},
	/// The process was killed by `signal` (local backends only).
	Signalled {
		/// Signal number.
		signal: i32,
	},
}

impl Outcome {
	/// Whether the command succeeded (exit code 0).
	#[inline]
	pub const fn is_success(self) -> bool {
		matches!(self, Self::Exited { code: 0 })
	}
}

/// One output file of a run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Artifact {
	/// Path relative to the input root.
	pub path:       Str,
	/// Content digest.
	pub blob:       Digest,
	/// Whether the file is executable.
	pub executable: bool,
	/// Whether its bytes are already in the local store.
	pub fetched:    bool,
}

/// Where a run's wall time went, every field in milliseconds. Phases a backend
/// does not have stay zero. `u32` spans 49 days, far beyond any run's timeout,
/// and keeps [`ExecutionResult`] compact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Timing {
	/// Waiting for a worker.
	pub queued:    u32,
	/// Capturing and uploading inputs.
	pub upload:    u32,
	/// Running the command.
	pub execution: u32,
	/// Fetching outputs.
	pub download:  u32,
	/// End to end, as the caller observed it.
	pub total:     u32,
}

/// Content-addressed-storage traffic of one run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TransferCounts {
	/// Blobs the run referenced.
	pub blobs_total:               u32,
	/// Blobs the backend already held.
	pub cache_hits:                u32,
	/// Blobs the backend lacked.
	pub cache_misses:              u32,
	/// Blobs uploaded.
	pub blobs_uploaded:            u32,
	/// Bytes uploaded before compression.
	pub bytes_uploaded:            u64,
	/// Bytes uploaded on the wire.
	pub bytes_uploaded_compressed: u64,
	/// Bytes downloaded.
	pub bytes_downloaded:          u64,
}

/// Why a remote-placed request ran locally instead. The backend that failed
/// is the placement's; the record keeps only the class of its failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FallbackCause {
	/// The class of the remote failure.
	pub kind: ExecutionErrorKind,
}

/// Digests of a run's standard streams. Both or neither: every output mode
/// above `Metadata` fetches the pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Logs {
	/// Standard output.
	pub stdout: Digest,
	/// Standard error.
	pub stderr: Digest,
}

/// The settled record of one run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionResult {
	/// Execution identity.
	pub id:             ExecutionId,
	/// The backend that ran it.
	pub backend:        BackendId,
	/// How the command ended.
	pub outcome:        Outcome,
	/// Standard output and error, when the request asked for logs.
	pub logs:           Option<Logs>,
	/// Output files.
	pub artifacts:      Arc<[Artifact]>,
	/// Digests of declared output paths, populated by the Artifacts output mode.
	pub output_digests: BTreeMap<Str, Digest>,
	/// Where the time went.
	pub timing:         Timing,
	/// Storage traffic.
	pub transfer:       TransferCounts,
	/// Whether the backend answered from its action cache.
	pub cache_hit:      bool,
	/// Set when the run fell back to a local backend.
	pub fallback:       Option<FallbackCause>,
}

// Settled results ride every event stream and fan out to the journal and
// telemetry; keep the record flat (two ids, one optional digest pair,
// counters) so `ExecutionEvent::Settled` stays near its sibling variants.
const _: () = assert!(size_of::<ExecutionResult>() <= 256, "ExecutionResult must stay compact");

/// Which stream a live output frame came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Display, EnumString, IntoStaticStr)]
#[strum(serialize_all = "kebab-case")]
pub enum Stream {
	/// Standard output.
	Stdout,
	/// Standard error.
	Stderr,
	/// Combined vendor log with no stdout/stderr channel metadata.
	Combined,
}

/// A chunk of live output (local backends only; remote output arrives as
/// digests in the result).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputFrame {
	/// The stream.
	pub stream: Stream,
	/// The bytes, exactly as the process wrote them.
	pub bytes:  CowBytes<'static>,
}

/// Progress of one run, in order. [`ExecutionEvent::Settled`] is last.
#[derive(Debug, Clone)]
pub enum ExecutionEvent {
	/// Waiting for the backend.
	Queued,
	/// Uploading inputs.
	Uploading {
		/// Bytes uploaded so far.
		done:  u64,
		/// Bytes to upload.
		total: u64,
	},
	/// The backend accepted the work and named the operation. After this,
	/// fallback is forbidden ([`crate::Stage::AfterAcceptance`]).
	Accepted {
		/// The backend's operation name, used to reattach.
		operation: Str,
	},
	/// The command is running.
	Executing,
	/// Live output (local backends only).
	Output(OutputFrame),
	/// The run finished.
	Settled(Result<ExecutionResult, ExecutionError>),
}
