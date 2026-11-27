//! The first-party extensions in `extensions/` must load through the product's
//! own manifest parser.
//!
//! `omp ext link` and native admission both read `omp.toml` with
//! `DeploymentManifest::parse`, then `validate`, and refuse a manifest with no
//! `id` or `entry`. The extension conformance script parses with Python's
//! `tomllib` and its own capability vocabulary, so it cannot see a manifest
//! that this parser rejects. Seventeen manifests were in that state, each with
//! a `[requires]` table where the parser expects a list.

use std::{fs, path::Path};

use omp_ext::config::DeploymentManifest;

#[test]
fn every_first_party_manifest_parses_and_validates() {
	let extensions = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../extensions");
	let mut checked = 0_usize;
	let mut rejected = Vec::new();
	for entry in fs::read_dir(&extensions)
		.expect("the in-tree extensions directory")
		.flatten()
	{
		let Ok(text) = fs::read_to_string(entry.path().join("omp.toml")) else {
			continue;
		};
		checked += 1;
		let name = entry.file_name().to_string_lossy().into_owned();
		match DeploymentManifest::parse(&text).and_then(|manifest| {
			manifest.validate()?;
			Ok(manifest)
		}) {
			Ok(manifest) if manifest.id.is_empty() || manifest.entry.is_empty() => {
				rejected.push(format!("{name}: no `id` or no `entry`"));
			},
			Ok(_) => {},
			Err(error) => rejected.push(format!("{name}: {error:?}")),
		}
	}
	assert!(checked > 0, "no first-party manifest was found: this test is stale");
	rejected.sort();
	assert!(
		rejected.is_empty(),
		"{} of {checked} first-party manifests are rejected by the parser `omp ext link` uses:\n{}",
		rejected.len(),
		rejected.join("\n")
	);
}
