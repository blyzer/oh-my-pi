//! [`AcpExecutor`]: spawns an ACP agent process and opens sessions over stdio.
//!
//! Hard rules:
//! - The child is spawned with explicit program and arguments supplied by the
//!   caller; this module never reads configuration or credentials.
//! - Transport is stdio only: OMP owns the process.
//! - Process isolation and teardown come from [`ChildTree`].
//! - ACP has no sandbox parameter; the only mappable policy is
//!   [`DelegatedPolicy::STRICTEST`].

use std::{ffi::OsString, time::Duration};

use omp_agent::delegated::{
	AuthStatus, Capabilities, DelegatedExecutor, ExecutorError, ExecutorId, Feature, FeatureSet,
	ProbeReport, SessionInfo, SessionRequest,
};
use tokio::process::Command;

use super::{
	delegated_session::{AcpDelegatedSession, AcpSessionConfig},
	policy::supported_policies,
};
use crate::child_tree::ChildTree;

/// Largest inbound line accepted. Same cap as the Codex adapter.
pub const MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;

/// Default timeout for the opening handshake.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(30);

/// Grace period between `session/cancel` and `SIGKILL` on `terminate`.
const TERMINATE_GRACE: Duration = Duration::from_secs(2);

/// Identity string for this executor.
const EXECUTOR_NAME: &str = "acp-executor";

/// A [`DelegatedExecutor`] that spawns an ACP v1 agent over stdio.
///
/// The caller supplies the program and arguments; this executor never reads
/// from a project configuration file or from any credential store.
pub struct AcpExecutor {
	caps:    Capabilities,
	program: OsString,
	args:    Vec<OsString>,
	cfg:     AcpSessionConfig,
}

impl AcpExecutor {
	/// Creates an executor that runs `program` with `args`.
	///
	/// The executor id is fixed (`acp-executor`); the version range is open
	/// because ACP versioning is negotiated per-session rather than via a
	/// `--version` flag.
	#[must_use]
	pub fn new(
		program: impl Into<OsString>,
		args: impl IntoIterator<Item = impl Into<OsString>>,
	) -> Self {
		Self {
			caps:    capabilities(),
			program: program.into(),
			args:    args.into_iter().map(Into::into).collect(),
			cfg:     AcpSessionConfig {
				handshake_timeout: HANDSHAKE_TIMEOUT,
				terminate_grace:   TERMINATE_GRACE,
				max_frame_bytes:   MAX_FRAME_BYTES,
			},
		}
	}

	/// Overrides the handshake timeout.
	#[must_use]
	pub const fn with_handshake_timeout(mut self, timeout: Duration) -> Self {
		self.cfg.handshake_timeout = timeout;
		self
	}

	/// Identity of this executor.
	#[must_use]
	pub const fn executor_id() -> ExecutorId {
		ExecutorId::new_static(EXECUTOR_NAME)
	}
}

/// The features every ACP session exposes.
pub(super) fn capabilities() -> Capabilities {
	let features = FeatureSet::EMPTY
		.with(Feature::Interrupt)
		.with(Feature::Permissions)
		.with(Feature::StreamingText)
		.with(Feature::ToolEvents)
		.with(Feature::WorkingDirectoryInheritance);
	// ACP v1 does not expose a separate session/resume in the wire (it uses
	// session/load, gated by `loadSession` capability). We advertise Resume
	// so the host can request it; the executor checks `loadSession` at open
	// time and returns ResumeFailed if it is absent.
	let features = features.with(Feature::Resume);
	Capabilities::new(
		AcpExecutor::executor_id(),
		open_version_range(),
		features,
		supported_policies(),
	)
}

/// An open version range: ACP versioning is protocol-level, not CLI-level.
fn open_version_range() -> omp_agent::delegated::VersionRange {
	omp_agent::delegated::VersionRange {
		min_inclusive: omp_agent::delegated::CliVersion::new(0, 0, 0),
		max_exclusive: None,
	}
}

impl DelegatedExecutor for AcpExecutor {
	type Session = AcpDelegatedSession;

	fn capabilities(&self) -> &Capabilities {
		&self.caps
	}

	/// ACP has no version flag; the version is negotiated inside `initialize`.
	/// We report `AuthStatus::Unknown` because there is no credential-free
	/// auth probe.
	async fn probe(&self) -> Result<ProbeReport, ExecutorError> {
		// Check that the program exists by attempting a zero-argument spawn
		// and immediately killing it.
		let mut cmd = Command::new(&self.program);
		cmd.args(&self.args).kill_on_drop(true);
		match cmd.spawn() {
			Ok(mut child) => {
				let _ = child.kill().await;
				Ok(ProbeReport {
					version: omp_agent::delegated::CliVersion::new(0, 0, 0),
					auth:    AuthStatus::Unknown,
				})
			},
			Err(_) => Err(ExecutorError::ExecutableMissing { executor: Self::executor_id() }),
		}
	}

	async fn open<'a>(
		&'a self,
		request: &'a SessionRequest,
	) -> Result<(Self::Session, SessionInfo), ExecutorError> {
		let mut cmd = Command::new(&self.program);
		cmd.args(&self.args);
		if !request.workdir.as_os_str().is_empty() {
			cmd.current_dir(&request.workdir);
		}

		let spawned = ChildTree::spawn(cmd)
			.map_err(|_| ExecutorError::ExecutableMissing { executor: Self::executor_id() })?;

		let (session, info) = AcpDelegatedSession::handshake(
			Self::executor_id(),
			spawned.stdout,
			spawned.stdin,
			Some(spawned.tree),
			request,
			self.cfg,
		)
		.await?;

		Ok((session, info))
	}
}
