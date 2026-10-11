//! Branded string identifiers for executions, backends, toolchains, and caches.

use omp_core::string_id;

string_id!(
	/// One execution, minted by the caller per run (a UUID); a remote backend
	/// also uses it as the REAPI `tool_invocation_id`.
	ExecutionId
);

string_id!(
	/// An operator-named backend: `local`, `remote`, `ssh:<alias>`.
	BackendId
);

string_id!(
	/// A toolchain derived by an adapter from the repository pins, for example
	/// `rust:nightly-2026-08-08+nextest-0.9.146+cpython-3.14.7-20260807`. A
	/// backend profile maps it to an image; an adapter never names one.
	ToolchainId
);

string_id!(
	/// A named persistent cache an adapter declares, for example
	/// `cargo-target`. A backend maps it to a stable location.
	CacheName
);
