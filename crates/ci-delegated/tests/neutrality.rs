//! The library names no CI vendor. The check lives outside `src` so the
//! forbidden spellings are not themselves part of the router.

use std::{fs, path::Path};

#[test]
fn library_sources_name_no_ci_vendor() {
	let root = Path::new(env!("CARGO_MANIFEST_DIR"));
	let mut files = vec![root.join("README.md"), root.join("Cargo.toml")];
	collect(&root.join("src"), &mut files);
	for path in files {
		let text = fs::read_to_string(&path).unwrap().to_ascii_lowercase();
		for vendor in [
			"github",
			"gitlab",
			"gitea",
			"bitbucket",
			"jenkins",
			"buildkite",
			"circle",
			"travis",
			"actions",
			"buildbuddy",
		] {
			assert!(
				!text.contains(vendor),
				"{} names {vendor}",
				path.strip_prefix(root).unwrap_or(&path).display()
			);
		}
	}
}

fn collect(dir: &Path, files: &mut Vec<std::path::PathBuf>) {
	for entry in fs::read_dir(dir).unwrap() {
		let path = entry.unwrap().path();
		if path.is_dir() {
			collect(&path, files);
		} else {
			files.push(path);
		}
	}
}
