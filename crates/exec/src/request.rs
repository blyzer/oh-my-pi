//! What a caller asks for and what an adapter turns it into.

use std::{sync::Arc, time::Duration};

use omp_adw::Requirement;
use omp_core::Str;
use strum::{Display, EnumString, IntoStaticStr};

use crate::{
	digest::Digest,
	ids::{CacheName, ExecutionId, ToolchainId},
	platform::{Platform, PlatformRequirement, ResourceRequirements},
};

/// What a command is for. Telemetry and reports group runs by it.
#[derive(
	Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Display, EnumString, IntoStaticStr,
)]
#[strum(serialize_all = "kebab-case", ascii_case_insensitive)]
pub enum Intent {
	/// Type checking.
	Check,
	/// Linting.
	Lint,
	/// Tests.
	Test,
	/// Building artifacts.
	Build,
	/// An end-to-end acceptance proof.
	Proof,
	/// Formatting.
	Format,
	/// Anything else.
	Other,
}

/// Where a request may run, derived by an adapter from the command and the
/// repository — never declared by the caller (ADR 0040, Part C.1).
#[derive(
	Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Display, EnumString, IntoStaticStr,
)]
#[strum(serialize_all = "kebab-case", ascii_case_insensitive)]
pub enum Eligibility {
	/// Must run on this host: it needs a host capability no remote backend
	/// can stand in for (Seatbelt, host timing, workspace writes).
	LocalRequired,
	/// Must run remotely: the requested platform does not exist on this host.
	/// Under every policy it is never placed locally.
	RemoteRequired,
	/// Valid on a remote Linux backend and worth offloading.
	RemotePreferred,
	/// Better run locally: short commands, unknown commands, or inputs that are
	/// not fully captured.
	LocalPreferred,
}

/// What a caller asks for, before any build-system knowledge is applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildCommand {
	/// Identity the caller minted for this run; a single-request plan reuses
	/// it as the request's id.
	pub id:          ExecutionId,
	/// Program and arguments. `argv[0]` is a bare program name, never a shell
	/// string.
	pub argv:        Arc<[Str]>,
	/// Working directory relative to the repository root; `None` is the root.
	pub cwd:         Option<Str>,
	/// What the command is for.
	pub intent:      Intent,
	/// The phase's posture ceiling.
	pub requirement: Requirement,
	/// The platform the caller wants the command verified on.
	pub target:      Platform,
	/// The platform of the host the caller runs on. A `target` different from
	/// it makes the request [`Eligibility::RemoteRequired`].
	pub host:        Platform,
}

/// One environment variable an adapter passes into the action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvEntry {
	/// Variable name.
	pub name:  Str,
	/// Variable value.
	pub value: Str,
}

/// A named persistent cache and the environment variable that points the
/// command at it, for example `cargo-target` → `CARGO_TARGET_DIR`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheDecl {
	/// Cache name the backend maps to a stable location.
	pub name: CacheName,
	/// Environment variable that receives that location.
	pub env:  Str,
}

/// How many files one exclusion rule kept out of an upload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Exclusion {
	/// The deny rule (a glob from [`crate::DEFAULT_UPLOAD_DENY`], an
	/// operator-added glob, or a content-scan rule id).
	pub rule:  Str,
	/// Files the rule excluded.
	pub files: u64,
}

/// Paths left out of a captured input set, counted per reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExclusionCounts {
	/// Untracked files excluded, per deny rule.
	pub rules:    Arc<[Exclusion]>,
	/// Untracked symlinks skipped.
	pub symlinks: u64,
}

/// A captured working tree: the manifest digest and what went into it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputSet {
	/// Digest of the capture manifest.
	pub manifest: Digest,
	/// Files in the input set.
	pub files:    u64,
	/// Total bytes of those files.
	pub bytes:    u64,
	/// What was left out, and why.
	pub excluded: ExclusionCounts,
}

/// Which outputs a run returns.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum OutputSpec {
	/// Exit code, timing, and digests only.
	Metadata,
	/// Adds stdout and stderr.
	#[default]
	Logs,
	/// Adds the declared output paths, relative to the input root.
	Paths(Arc<[Str]>),
	/// Every output; needs an explicit operator setting.
	All,
}

/// One unit of execution: a whole command over a captured working tree.
///
/// Cold, moved by value, and cloned into telemetry, so every collection is
/// `Arc`-backed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionRequest {
	/// Execution identity.
	pub id:          ExecutionId,
	/// What the command is for.
	pub intent:      Intent,
	/// Program and arguments; `argv[0]` is a bare name resolved on the
	/// action `PATH`.
	pub argv:        Arc<[Str]>,
	/// Working directory relative to the input root; the root itself is `""`
	/// (never `"."`).
	pub cwd:         Str,
	/// The adapter's environment allowlist. A backend profile adds its image
	/// environment.
	pub env:         Arc<[EnvEntry]>,
	/// The captured input set. `None` until capture runs: an adapter is pure
	/// and cannot capture, so the caller attaches it with
	/// [`ExecutionRequest::with_inputs`] before handing the request to a
	/// backend that needs it.
	pub inputs:      Option<InputSet>,
	/// Which outputs to return.
	pub outputs:     OutputSpec,
	/// Toolchain the backend maps to an image.
	pub toolchain:   ToolchainId,
	/// Named persistent caches.
	pub caches:      Arc<[CacheDecl]>,
	/// Platform and host needs.
	pub platform:    PlatformRequirement,
	/// Posture ceiling, checked against each candidate backend's enforced
	/// posture.
	pub requirement: Requirement,
	/// Declared resources.
	pub resources:   ResourceRequirements,
	/// Where the request may run.
	pub eligibility: Eligibility,
	/// Wall-clock limit; always set.
	pub timeout:     Duration,
}

impl ExecutionRequest {
	/// Attaches the captured input set.
	#[must_use]
	pub fn with_inputs(mut self, inputs: InputSet) -> Self {
		self.inputs = Some(inputs);
		self
	}
}

/// The requests one command expands to, with ordering edges.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionPlan {
	/// Requests in plan order.
	pub requests: Arc<[ExecutionRequest]>,
	/// `(before, after)` index pairs into `requests`.
	pub edges:    Arc<[(u16, u16)]>,
}

impl ExecutionPlan {
	/// A plan of exactly one request and no edges.
	pub fn single(request: ExecutionRequest) -> Self {
		Self { requests: Arc::new([request]), edges: Arc::new([]) }
	}
}
