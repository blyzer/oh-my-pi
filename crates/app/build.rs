//! Applies final-link requirements that cannot propagate from `omp-py`'s
//! library build script to `omp-app` binaries, examples, and test executables.
//!
//! `omp_py_link::emit` supplies the CPython link arguments; this script adds
//! the embedded changelog, which is generated from the checkout's tags.

use std::{
	env, fs,
	path::{Path, PathBuf},
	process::Command,
};

fn main() {
	omp_py_link::emit();
	write_changelog(&PathBuf::from(env!("CARGO_MANIFEST_DIR")));
}

fn write_changelog(manifest: &Path) {
	let workspace = manifest.join("../..");
	// Track only ref paths that exist: a missing `rerun-if-changed` path makes
	// cargo rerun this script (and relink omp) on every build. A tag created
	// on a tree where neither path existed refreshes the changelog on the next
	// ordinary rebuild instead.
	for refs in [workspace.join(".git/packed-refs"), workspace.join(".git/refs/tags")] {
		if refs.exists() {
			println!("cargo::rerun-if-changed={}", refs.display());
		}
	}
	let generated = Command::new("git")
		.arg("-C")
		.arg(&workspace)
		.args([
			"for-each-ref",
			"--sort=-version:refname",
			"--format=## %(refname:short) — %(creatordate:short)%0a%0a%(contents:subject)%0a",
			"refs/tags",
		])
		.output()
		.ok()
		.filter(|output| output.status.success() && !output.stdout.is_empty())
		.map_or_else(
			|| {
				format!(
					"## v{}\n\nCurrent release.\n",
					env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "unknown".to_owned())
				)
				.into_bytes()
			},
			|output| output.stdout,
		);
	let output =
		PathBuf::from(env::var_os("OUT_DIR").expect("Cargo provides OUT_DIR")).join("changelog.md");
	fs::write(output, generated).expect("write embedded changelog");
}
