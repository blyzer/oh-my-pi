//! Runtime-owned settings projections for environment execution.

mod async_jobs;
mod sandbox;
mod shell;

pub use async_jobs::AsyncJobSettings;
pub(crate) use sandbox::{
	EnvironmentInheritance, ReadMode, SandboxSettings, UnscopedWrites, valid_domain_name,
};
pub use sandbox::{
	ExecSandboxMode, NetworkConfinement, SV_SANDBOX_MODE, SV_SANDBOX_NETWORK_MODE,
	SandboxNetworkMode, network_confinement,
};
pub(crate) use shell::{DirenvMode, ShellSettings};
