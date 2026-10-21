//! The Codex `app-server` [`DelegatedExecutor`]: probes the installed CLI and
//! spawns a private `codex app-server --listen stdio://` per session.
//!
//! Hard rules, enforced here rather than trusted:
//!
//! - the child is spawned with explicit arguments and working directory and
//!   with a [`ChildEnv`] environment: the ambient allowlist (`PATH`, `HOME`,
//!   locale, ...) and nothing secret-looking from OMP's own environment, plus
//!   whatever the operator names; this module never reads, copies or names any
//!   Codex configuration or credential path, and the contract gives it no way
//!   to receive one (the CLI finds its own login through `HOME`);
//! - the transport is stdio only: OMP owns the process, never attaches to a
//!   shared daemon or socket;
//! - process isolation and teardown come from [`ChildTree`].

use std::{ffi::OsString, process::Stdio, time::Duration};

use omp_agent::delegated::{
	AuthStatus, Capabilities, CliVersion, DelegatedExecutor, ExecutorError, ExecutorId, Feature,
	FeatureSet, ProbeReport, SessionInfo, SessionRequest, VersionRange,
};
use tokio::{process::Command, time::timeout};

use super::{
	policy::supported_policies,
	session::{CodexSession, SessionConfig, Transport, parse_version},
};
use crate::{child_env::ChildEnv, child_tree::ChildTree};

/// Largest inbound line accepted. Item payloads with command output can be
/// large; this bounds memory, not usefulness.
pub const MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;
/// How long `--version` may take.
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);
/// How long the opening exchange may take before the CLI counts as hung.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(30);
/// How long `terminate` waits for an interrupted turn before killing the tree.
const TERMINATE_GRACE: Duration = Duration::from_secs(2);

/// The Codex versions this adapter was written against: `0.160.x`.
pub const CODEX_VERSIONS: VersionRange = VersionRange {
	min_inclusive: CliVersion::new(0, 160, 0),
	max_exclusive: Some(CliVersion::new(0, 161, 0)),
};

/// Executor for the Codex CLI.
pub struct CodexExecutor {
	caps:    Capabilities,
	program: OsString,
	cfg:     SessionConfig,
	env:     ChildEnv,
}

impl CodexExecutor {
	/// An executor that runs `program` (normally `codex`).
	#[must_use]
	pub fn new(program: impl Into<OsString>) -> Self {
		Self {
			caps:    capabilities(),
			program: program.into(),
			cfg:     SessionConfig {
				max_frame_bytes:   MAX_FRAME_BYTES,
				terminate_grace:   TERMINATE_GRACE,
				handshake_timeout: HANDSHAKE_TIMEOUT,
				versions:          CODEX_VERSIONS,
			},
			env:     ChildEnv::ambient(),
		}
	}

	/// Overrides how long the opening exchange may take.
	#[must_use]
	pub const fn with_handshake_timeout(mut self, timeout: Duration) -> Self {
		self.cfg.handshake_timeout = timeout;
		self
	}

	/// Replaces the child's environment. The default is [`ChildEnv::ambient`].
	#[must_use]
	pub fn with_env(mut self, env: ChildEnv) -> Self {
		self.env = env;
		self
	}

	/// Identity of this executor.
	#[must_use]
	pub const fn executor_id() -> ExecutorId {
		ExecutorId::new_static("codex-app-server")
	}

	const fn missing() -> ExecutorError {
		ExecutorError::ExecutableMissing { executor: Self::executor_id() }
	}

	fn command(&self) -> Command {
		let mut command = Command::new(&self.program);
		command.stdin(Stdio::piped());
		self.env.apply_tokio(&mut command);
		command
	}
}

/// What Codex `app-server` can do. Every feature is backed by a protocol
/// message this adapter implements and the mock-peer tests exercise.
pub(super) fn capabilities() -> Capabilities {
	let features = FeatureSet::EMPTY
		.with(Feature::Resume)
		.with(Feature::Interrupt)
		.with(Feature::Permissions)
		.with(Feature::StreamingText)
		.with(Feature::ToolEvents)
		.with(Feature::UsageReporting)
		.with(Feature::WorkingDirectoryInheritance);
	Capabilities::new(CodexExecutor::executor_id(), CODEX_VERSIONS, features, supported_policies())
}

impl DelegatedExecutor for CodexExecutor {
	type Session = CodexSession;

	fn capabilities(&self) -> &Capabilities {
		&self.caps
	}

	async fn probe(&self) -> Result<ProbeReport, ExecutorError> {
		let mut command = self.command();
		command
			.arg("--version")
			.stdin(Stdio::null())
			.stderr(Stdio::null())
			.kill_on_drop(true);
		let output = match timeout(PROBE_TIMEOUT, command.output()).await {
			Ok(Ok(output)) if output.status.success() => output,
			Ok(_) | Err(_) => return Err(Self::missing()),
		};
		let text = String::from_utf8_lossy(&output.stdout);
		// `codex-cli 0.160.0`
		let version = text
			.split_whitespace()
			.find_map(parse_version)
			.ok_or_else(Self::missing)?;
		// Codex documents no side-effect-free login probe that is safe to run
		// without reading credential state, so the login is reported as it is:
		// unknown. The first turn surfaces `AuthRequired` if it is missing.
		Ok(ProbeReport { version, auth: AuthStatus::Unknown })
	}

	async fn open<'a>(
		&'a self,
		request: &'a SessionRequest,
	) -> Result<(Self::Session, SessionInfo), ExecutorError> {
		let mut command = self.command();
		command
			.args(["app-server", "--listen", "stdio://"])
			.current_dir(&request.workdir);
		let spawned = ChildTree::spawn(command).map_err(|_| Self::missing())?;
		let transport = Transport {
			reader: Box::new(spawned.stdout),
			writer: Box::new(spawned.stdin),
			stderr: Some(spawned.stderr),
			tree:   Some(spawned.tree),
		};
		let mut session = CodexSession::new(Self::executor_id(), transport, self.cfg);
		let opened = match timeout(self.cfg.handshake_timeout, session.handshake(request)).await {
			Ok(result) => result,
			Err(_) => Err(ExecutorError::Hung {
				executor: Self::executor_id(),
				idle:     self.cfg.handshake_timeout,
			}),
		};
		match opened {
			Ok(info) => Ok((session, info)),
			Err(error) => {
				// A failed handshake never leaks the child.
				let _ = omp_agent::delegated::DelegatedSession::terminate(&mut session).await;
				Err(error)
			},
		}
	}
}
