//! Applies omp-py's final-link requirements to the executable acceptance host.

use std::{
	env,
	path::{Path, PathBuf},
};

fn main() {
	println!("cargo::rerun-if-env-changed=PYO3_CONFIG_FILE");

	let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
	let vendor = env::var_os("PYO3_CONFIG_FILE")
		.map(PathBuf::from)
		.and_then(|p| {
			p.canonicalize()
				.ok()
				.or_else(|| manifest.join("../..").join(&p).canonicalize().ok())
		})
		.and_then(|p| p.parent().map(Path::to_path_buf));

	if let Some(vendor_dir) = &vendor {
		// Vendor-tree swaps rewrite PYTHON.json; tracking it covers the
		// appearance of the `needs-lld` marker. The marker itself is tracked
		// only while present — cargo treats a missing `rerun-if-changed` path
		// as always changed, which would relink the e2e host per build.
		let python_manifest = vendor_dir.join("PYTHON.json");
		if python_manifest.is_file() {
			println!("cargo::rerun-if-changed={}", python_manifest.display());
		}
		let marker = vendor_dir.join("needs-lld");
		if marker.is_file() {
			println!("cargo::rerun-if-changed={}", marker.display());
			let shim = manifest.join("../py/scripts/ld64.lld");
			println!("cargo::rerun-if-changed={}", shim.display());
			assert!(
				shim.is_file(),
				"omp-e2e's release macOS link requires omp-py's ld64.lld shim at {}; restore \
				 crates/py/scripts/ld64.lld",
				shim.display()
			);
			println!("cargo::rustc-link-arg=--ld-path={}", shim.display());
		}
	}
	// Wheels' native extensions resolve the CPython C-API from this executable
	// at dlopen, so those globals (code AND data, PyExc_* included) must reach
	// the dynamic symbol table and survive dead-strip. Export exactly them, via
	// the list in crates/py/link: a blanket `--export-dynamic` also publishes
	// every Rust monomorphization, which measured 1,292,605 dynamic symbols and
	// 275 MiB of `.dynstr` per debug binary — 23% of the file, for symbols no
	// extension can name. The mechanism is per-linker: ld64 takes
	// `-exported_symbols_list` with Mach-O's leading underscore, ELF linkers
	// take `--dynamic-list`. Other object formats have no compatible flag.
	let target_vendor = env::var("CARGO_CFG_TARGET_VENDOR").unwrap_or_default();
	let target_family = env::var("CARGO_CFG_TARGET_FAMILY").unwrap_or_default();
	let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
	let link_dir = manifest.join("../py/link");
	let exports = if target_vendor == "apple" {
		Some(("-Wl,-exported_symbols_list,", link_dir.join("cpython.macho-list")))
	} else if target_os != "aix" && target_family.split(',').any(|family| family == "unix") {
		Some(("-Wl,--dynamic-list=", link_dir.join("cpython.dynamic-list")))
	} else {
		None
	};
	if let Some((flag, list)) = exports {
		assert!(
			list.is_file(),
			"omp-e2e needs omp-py's CPython export list at {}; restore crates/py/link/",
			list.display()
		);
		println!("cargo::rerun-if-changed={}", list.display());
		println!("cargo::rustc-link-arg={flag}{}", list.display());
	}
}
