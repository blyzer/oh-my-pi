//! Compiles the vendored Remote Execution API v2 protos with protox +
//! tonic-prost-build, the same pipeline `omp-proto` uses, so no system
//! `protoc` is needed.
//!
//! Two roots are compiled separately:
//! - `proto/`: REAPI v2, `google.longrunning`, `google.bytestream`,
//!   `google.rpc` and the annotations they import.
//! - `proto-buildbuddy/`: BuildBuddy's `BuildBuddyService`, only when the
//!   `buildbuddy-cancel` feature is on.
//!
//! Well-known types (`Any`, `Duration`, `Timestamp`, wrappers) map to
//! `prost-types` through prost's default extern paths.

use std::{
	env, fs,
	path::{Path, PathBuf},
};

fn main() {
	let manifest = PathBuf::from(
		env::var_os("CARGO_MANIFEST_DIR").expect("Cargo did not set CARGO_MANIFEST_DIR"),
	);
	compile(&manifest.join("proto"), true, false);
	if env::var_os("CARGO_FEATURE_BUILDBUDDY_CANCEL").is_some() {
		compile(&manifest.join("proto-buildbuddy"), false, true);
	}
}

fn compile(root: &Path, with_services: bool, client_only: bool) {
	println!("cargo::rerun-if-changed={}", root.display());
	let mut protos = Vec::new();
	collect(root, &mut protos);
	protos.sort();
	for proto in &protos {
		println!("cargo::rerun-if-changed={}", proto.display());
	}
	let fds = protox::compile(&protos, [root]).expect("protox failed to compile .proto sources");
	let out = PathBuf::from(env::var_os("OUT_DIR").expect("Cargo did not set OUT_DIR"))
		.join(root.file_name().expect("proto root has a name"));
	fs::create_dir_all(&out).expect("create generated-code directory");
	tonic_prost_build::configure()
		.build_client(with_services || client_only)
		.build_server(false)
		.out_dir(&out)
		.compile_fds(fds)
		.expect("tonic-prost-build failed to generate Rust code");
}

/// Recursively gathers `.proto` files under `dir`.
fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
	for entry in fs::read_dir(dir).expect("proto directory missing") {
		let path = entry.expect("unreadable dir entry").path();
		if path.is_dir() {
			collect(&path, out);
		} else if path.extension().is_some_and(|ext| ext == "proto") {
			out.push(path);
		}
	}
}
