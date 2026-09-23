//! Final-link arguments for binaries that embed `omp-py`'s static `CPython`.
//!
//! Cargo's `rustc-link-arg` applies to the crate being built, not to anything
//! downstream, so `omp-py`'s own build script cannot hand these to the
//! binaries that link it. Each consumer emits them itself, by calling
//! [`emit`] from its build script:
//!
//! ```no_run
//! fn main() {
//! 	omp_py_link::emit();
//! }
//! ```
//!
//! Two arguments are at stake. The ld64-to-lld shim, needed when the vendored
//! Python tree carries LLVM LTO bitcode that Xcode's `ld64` cannot read; and
//! the `CPython` export list, needed so native wheels resolve the C-API from
//! the host executable at `dlopen`.

use std::{
	env,
	path::{Path, PathBuf},
};

/// This crate's own directory, fixed at compile time.
///
/// Deliberately not the caller's `CARGO_MANIFEST_DIR`: every consumer must
/// resolve the same link inputs, wherever its own manifest sits.
const SELF_DIR: &str = env!("CARGO_MANIFEST_DIR");

/// `crates/py`, which owns the vendored interpreter and every link input
/// applied here. Built from this crate's sibling rather than by appending
/// `..`, so the paths that reach cargo and the linker stay normalized.
fn py_crate() -> PathBuf {
	workspace_root().join("crates/py")
}

/// The repository root: this crate sits two levels below it.
fn workspace_root() -> PathBuf {
	Path::new(SELF_DIR)
		.ancestors()
		.nth(2)
		.expect("omp-py-link sits two directories below the workspace root")
		.to_path_buf()
}

/// Emits the link arguments a binary embedding `omp-py`'s `CPython` needs.
///
/// Call this from a build script. It emits nothing when `PYO3_CONFIG_FILE` is
/// unset or does not resolve — `omp-py`'s own build script already fails that
/// case with the actionable message, and duplicating it here would only bury
/// it under a second panic.
///
/// # Panics
///
/// If a required link input is missing from the checkout: the export list for
/// the target's object format, or the ld64 shim when the vendored tree is
/// marked `needs-lld`. Both are tracked files, so absence means a damaged
/// checkout rather than a missing setup step, and a loud failure is better
/// than silently linking a binary whose extensions cannot resolve.
pub fn emit() {
	let py_crate = py_crate();
	let consumer = env::var("CARGO_PKG_NAME").unwrap_or_else(|_| "this crate".to_owned());

	// pyo3 reads this before any build script runs; tracking it keeps a vendor
	// swap from leaving a stale link behind.
	println!("cargo::rerun-if-env-changed=PYO3_CONFIG_FILE");
	if let Some(vendor) = vendor_dir() {
		// Vendor-tree swaps rewrite PYTHON.json; tracking it covers the
		// appearance of the `needs-lld` marker. The marker itself is tracked
		// only while present — cargo treats a missing `rerun-if-changed` path
		// as always changed, which would relink every consumer on each build.
		let python_manifest = vendor.join("PYTHON.json");
		if python_manifest.is_file() {
			println!("cargo::rerun-if-changed={}", python_manifest.display());
		}
		let marker = vendor.join("needs-lld");
		if marker.is_file() {
			println!("cargo::rerun-if-changed={}", marker.display());
			let shim = py_crate.join("scripts/ld64.lld");
			assert!(
				shim.is_file(),
				"{consumer} links a Python tree marked needs-lld, which requires omp-py's ld64.lld \
				 shim at {}; restore crates/py/scripts/ld64.lld",
				shim.display()
			);
			println!("cargo::rerun-if-changed={}", shim.display());
			println!("cargo::rustc-link-arg=--ld-path={}", shim.display());
		}
	}

	// Wheels' native extensions resolve the CPython C-API from the host
	// executable at dlopen, so those globals (code AND data, PyExc_* included)
	// must reach the dynamic symbol table and survive dead-strip. Export
	// exactly them: a blanket `--export-dynamic` also publishes every Rust
	// monomorphization, which measured 1,292,605 dynamic symbols and 275 MiB
	// of `.dynstr` per debug binary — 23% of the file, for symbols no
	// extension can name. The mechanism is per-linker: ld64 takes
	// `-exported_symbols_list` with Mach-O's leading underscore, ELF linkers
	// take `--dynamic-list`. Other object formats have no compatible flag.
	let exports = export_list(
		&env::var("CARGO_CFG_TARGET_VENDOR").unwrap_or_default(),
		&env::var("CARGO_CFG_TARGET_OS").unwrap_or_default(),
		&env::var("CARGO_CFG_TARGET_FAMILY").unwrap_or_default(),
		&py_crate,
	);
	if let Some((flag, list)) = exports {
		assert!(
			list.is_file(),
			"{consumer} needs omp-py's CPython export list at {}; restore crates/py/link/",
			list.display()
		);
		println!("cargo::rerun-if-changed={}", list.display());
		println!("cargo::rustc-link-arg={flag}{}", list.display());
	}
}

/// Selects the export mechanism for one target's object format.
///
/// Returns the flag prefix and the list file it takes, or `None` for a format
/// with no compatible flag. Split out from [`emit`] because this is where the
/// per-linker spelling lives, and a wrong spelling here does not fail the
/// link: `ld64`'s `-export_dynamic` handed to an ELF linker parses as
/// `-e xport_dynamic`, which links a binary whose entry point is `0x0` and
/// segfaults inside the dynamic loader before `main`. That shipped once.
fn export_list(
	target_vendor: &str,
	target_os: &str,
	target_family: &str,
	py_crate: &Path,
) -> Option<(&'static str, PathBuf)> {
	if target_vendor == "apple" {
		Some(("-Wl,-exported_symbols_list,", py_crate.join("link/cpython.macho-list")))
	} else if target_os != "aix" && target_family.split(',').any(|family| family == "unix") {
		Some(("-Wl,--dynamic-list=", py_crate.join("link/cpython.dynamic-list")))
	} else {
		None
	}
}

/// Locates the vendored Python tree through `PYO3_CONFIG_FILE`.
///
/// The config file sits at the root of the generated tree, so its parent
/// directory is the vendor directory — one env var pins both pyo3 and this
/// crate to the same runtime. A relative value is resolved against the
/// workspace root, which is where `.cargo/config.toml` writes one.
fn vendor_dir() -> Option<PathBuf> {
	let workspace = workspace_root();
	env::var_os("PYO3_CONFIG_FILE")
		.map(PathBuf::from)
		.and_then(|path| {
			path
				.canonicalize()
				.ok()
				.or_else(|| workspace.join(&path).canonicalize().ok())
		})
		.and_then(|path| path.parent().map(Path::to_path_buf))
}

#[cfg(test)]
mod tests {
	use super::*;

	/// Every Apple target takes ld64's list flag and the Mach-O symbol
	/// spelling, whichever architecture or OS it is.
	#[test]
	fn apple_targets_take_the_macho_list() {
		for os in ["macos", "ios"] {
			let selected = export_list("apple", os, "unix", Path::new("/py"));
			let (flag, list) = selected.expect("an Apple target exports through ld64");
			assert_eq!(flag, "-Wl,-exported_symbols_list,");
			assert_eq!(list, Path::new("/py/link/cpython.macho-list"));
		}
	}

	/// Non-Apple unix targets take the ELF mechanism. Passing them ld64's
	/// spelling is the defect this crate exists to make unrepeatable, so the
	/// assertion is on the exact flag text.
	#[test]
	fn elf_targets_take_the_dynamic_list() {
		for (vendor, os) in [("unknown", "linux"), ("pc", "solaris"), ("unknown", "freebsd")] {
			let selected = export_list(vendor, os, "unix", Path::new("/py"));
			let (flag, list) = selected.expect("a unix target exports through its ELF linker");
			assert_eq!(flag, "-Wl,--dynamic-list=");
			assert_eq!(list, Path::new("/py/link/cpython.dynamic-list"));
		}
	}

	/// `CARGO_CFG_TARGET_FAMILY` is comma-joined when a target claims several
	/// families, so the unix test is membership, never equality.
	#[test]
	fn a_multi_family_target_is_still_unix() {
		let selected = export_list("unknown", "wasi", "unix,wasm", Path::new("/py"));
		assert!(selected.is_some(), "a target listing unix among its families exports");
	}

	/// AIX is unix but its XCOFF linker has neither flag, and a non-unix
	/// family has no dynamic symbol table to narrow.
	#[test]
	fn formats_without_a_compatible_flag_export_nothing() {
		assert!(export_list("ibm", "aix", "unix", Path::new("/py")).is_none());
		assert!(export_list("pc", "windows", "windows", Path::new("/py")).is_none());
		assert!(export_list("unknown", "unknown", "", Path::new("/py")).is_none());
	}
}
