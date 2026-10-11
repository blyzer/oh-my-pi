//! The build-system adapter contract and the repository pins it plans from.

use omp_core::Str;

use crate::{
	platform::Platform,
	request::{BuildCommand, ExecutionPlan},
};

/// Toolchain pins read from the repository by the caller and passed in as
/// data, so planning stays pure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoPins {
	/// `rust-toolchain.toml` `channel`, e.g. `nightly-2026-08-08`.
	pub rust_toolchain: Str,
	/// The cargo-nextest version CI installs, e.g. `0.9.146`.
	pub nextest:        Str,
	/// The python-build-standalone release tag from `fetch-python.sh`
	/// (`TAG`), e.g. `20260807`.
	pub cpython_tag:    Str,
	/// The Python version from `fetch-python.sh` (`VER`), e.g. `3.14.7`.
	pub cpython:        Str,
}

/// Why an adapter could not plan a command.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PlanError {
	/// The command has no program.
	#[error("command has no program")]
	EmptyArgv,
	/// The program is not one this adapter plans.
	#[error("command is not handled by this build-system adapter")]
	UnsupportedProgram {
		/// The program, `argv[0]`.
		program: Str,
	},
	/// The working directory is absolute or leaves the repository.
	#[error("working directory is not a relative path inside the repository")]
	InvalidCwd {
		/// The rejected working directory.
		cwd: Str,
	},
	/// The command must run on this host, and the caller asked to verify it on
	/// a different platform.
	#[error("command must run on this host and cannot target another platform")]
	HostOnly {
		/// The requested target platform.
		platform: Platform,
	},
}

/// Turns a build command into execution requests using knowledge of one build
/// system. Pure: no I/O, and never names a backend.
pub trait BuildSystemAdapter {
	/// Plans `command` against the repository `pins`.
	///
	/// # Errors
	///
	/// Returns [`PlanError`] when the command is not one this adapter can
	/// plan.
	fn plan(&self, command: &BuildCommand, pins: &RepoPins) -> Result<ExecutionPlan, PlanError>;
}
