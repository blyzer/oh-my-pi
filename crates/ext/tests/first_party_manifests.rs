//! The first-party extensions in `extensions/` must load through the product's
//! own manifest parser.
//!
//! `omp ext link` and native admission both read `omp.toml` with
//! `DeploymentManifest::parse`, then `validate`, and refuse a manifest with no
//! `id` or `entry`. The extension conformance script parses with Python's
//! `tomllib` and its own capability vocabulary, so it cannot see a manifest
//! that this parser rejects. All seventeen were in that state: each wrote a
//! `[requires]` table where the parser expects a list, and six wrote
//! `[settings]` as bare values where it expects one schema per setting.

use std::{fs, path::Path};

use omp_ext::config::DeploymentManifest;

/// Manifests the parser still rejects, and why. The list can only shrink: a
/// manifest that is not here must load, and one that is here and now loads must
/// be removed.
///
/// `SettingType` has `string`, `number`, `boolean` and `enum`, so a setting whose
/// default is a table or a list cannot be written. Either the type grows an
/// object and an array kind, or these extensions restructure their settings.
const NOT_YET_LOADABLE: [(&str, &str); 3] = [
	("green-loop", "`settings.affected` defaults to a table of tables"),
	("pr-review", "`settings.areas` defaults to a list of tables"),
	("welcome-chrome", "`settings.tips` defaults to a list of strings"),
];

#[test]
fn every_first_party_manifest_parses_and_validates() {
	let extensions = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../extensions");
	let mut checked = 0_usize;
	let mut unexpected = Vec::new();
	let mut loads_now = Vec::new();
	let mut seen_known = 0_usize;
	for entry in fs::read_dir(&extensions)
		.expect("the in-tree extensions directory")
		.flatten()
	{
		let Ok(text) = fs::read_to_string(entry.path().join("omp.toml")) else {
			continue;
		};
		checked += 1;
		let name = entry.file_name().to_string_lossy().into_owned();
		let known = NOT_YET_LOADABLE.iter().any(|(known, _)| *known == name);
		seen_known += usize::from(known);
		let refusal = match DeploymentManifest::parse(&text).and_then(|manifest| {
			manifest.validate()?;
			Ok(manifest)
		}) {
			Ok(manifest) if manifest.id.is_empty() || manifest.entry.is_empty() => {
				Some("no `id` or no `entry`".to_owned())
			},
			Ok(_) => None,
			Err(error) => Some(format!("{error:?}")),
		};
		match (refusal, known) {
			(Some(refusal), false) => unexpected.push(format!("{name}: {refusal}")),
			(None, true) => loads_now.push(name),
			_ => {},
		}
	}
	assert!(checked > 0, "no first-party manifest was found: this test is stale");
	assert_eq!(
		seen_known,
		NOT_YET_LOADABLE.len(),
		"NOT_YET_LOADABLE names an extension that is not in extensions/"
	);
	unexpected.sort();
	loads_now.sort();
	assert!(
		unexpected.is_empty(),
		"{} of {checked} first-party manifests are rejected by the parser `omp ext link` uses:\n{}",
		unexpected.len(),
		unexpected.join("\n")
	);
	assert!(
		loads_now.is_empty(),
		"these load now; remove them from NOT_YET_LOADABLE: {loads_now:?}"
	);
}
