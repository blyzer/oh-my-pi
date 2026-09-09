//! Test-only scratch directories. No dev-dependency for something this small.

use std::{
	path::{Path, PathBuf},
	sync::atomic::{AtomicU64, Ordering},
};

static COUNTER: AtomicU64 = AtomicU64::new(0);

pub struct TempDir(PathBuf);

impl TempDir {
	pub fn new(label: &str) -> Self {
		let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
		let path =
			std::env::temp_dir().join(format!("pi-tasks-{label}-{}-{unique}", std::process::id()));
		std::fs::create_dir_all(&path).expect("create temp dir");
		Self(path)
	}

	pub fn path(&self) -> &Path {
		&self.0
	}

	pub fn write(&self, rel: &str, contents: &str) {
		let path = self.0.join(rel);
		if let Some(parent) = path.parent() {
			std::fs::create_dir_all(parent).expect("create parent");
		}
		std::fs::write(path, contents).expect("write file");
	}
}

impl Drop for TempDir {
	fn drop(&mut self) {
		let _ = std::fs::remove_dir_all(&self.0);
	}
}
