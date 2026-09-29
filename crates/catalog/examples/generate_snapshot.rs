//! Regenerates the checked-in catalog artifacts from the offline oracle
//! fixtures.
//!
//! `--relock` first rewrites `data/sources.lock.json` from the bytes of the
//! files it lists (an edited fixture or KDL source changes its hash), then
//! compiles the snapshot against the fresh lock. Without it a stale lock is an
//! error, so an edit can never be snapshotted against the wrong provenance.

use std::{
	env, error, fs,
	path::{Path, PathBuf},
};

use omp_catalog::{Catalog, SnapshotProvenance, compile_oracle};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SourceLock {
	schema_version: u32,
	source_digest:  String,
	inputs:         Vec<SourceInput>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SourceInput {
	id:     String,
	path:   String,
	sha256: String,
	source: String,
}

fn main() -> Result<(), Box<dyn error::Error>> {
	let crate_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
	let workspace = crate_dir
		.parent()
		.and_then(Path::parent)
		.expect("catalog crate is in workspace/crates");
	let lock_path = crate_dir.join("data/sources.lock.json");
	let mut lock: SourceLock = serde_json::from_slice(&fs::read(&lock_path)?)?;
	if lock.schema_version != 2 {
		return Err(format!("unsupported source-lock schema {}", lock.schema_version).into());
	}
	if env::args().skip(1).any(|argument| argument == "--relock") {
		relock(workspace, &mut lock)?;
		let mut encoded = serde_json::to_string_pretty(&lock)?;
		encoded.push('\n');
		fs::write(&lock_path, encoded)?;
		println!("relocked {} sources", lock.inputs.len());
	}
	let source_digest = verify_sources(workspace, &lock)?;
	let providers =
		fs::read_to_string(workspace.join("fixtures/llm-oracle/catalog/providers.toml"))?;
	let models = fs::read(workspace.join("fixtures/llm-oracle/catalog/models.json.zst"))?;
	let oauth = fs::read_to_string(workspace.join("fixtures/llm-oracle/catalog/oauth.toml"))?;
	let catalog = compile_oracle(&providers, &models, &oauth)?;
	let artifacts = Catalog::encode(catalog, SnapshotProvenance { source_digest })?;
	fs::write(crate_dir.join("data/catalog.postcard"), artifacts.postcard)?;
	// The normalized JSON is a review artifact only: reproducible from the
	// postcard (its hash rides the snapshot header), so it stays out of git.
	let target =
		env::var_os("CARGO_TARGET_DIR").map_or_else(|| workspace.join("target"), PathBuf::from);
	fs::create_dir_all(&target)?;
	let review = target.join("catalog.normalized.json");
	fs::write(&review, artifacts.normalized_json)?;
	println!("review artifact: {}", review.display());
	Ok(())
}

/// Recomputes every input hash and the aggregate digest from disk.
fn relock(workspace: &Path, lock: &mut SourceLock) -> Result<(), Box<dyn error::Error>> {
	let mut source_hasher = Sha256::new();
	for input in &mut lock.inputs {
		let bytes = fs::read(workspace.join(&input.path))?;
		input.sha256 = hex(&Sha256::digest(bytes).into());
		source_hasher.update(input.id.as_bytes());
		source_hasher.update([0]);
		source_hasher.update(input.path.as_bytes());
		source_hasher.update([0]);
		source_hasher.update(input.sha256.as_bytes());
		source_hasher.update([0]);
	}
	lock.source_digest = hex(&source_hasher.finalize().into());
	Ok(())
}

fn verify_sources(workspace: &Path, lock: &SourceLock) -> Result<[u8; 32], Box<dyn error::Error>> {
	let mut source_hasher = Sha256::new();
	let mut previous: Option<&str> = None;
	for input in &lock.inputs {
		if previous.is_some_and(|prior| prior >= input.id.as_str()) {
			return Err("source-lock inputs are not uniquely sorted".into());
		}
		if input.source.is_empty() {
			return Err(format!("source {} has no provenance", input.id).into());
		}
		let bytes = fs::read(workspace.join(&input.path))?;
		let actual = hex(&Sha256::digest(bytes).into());
		if actual != input.sha256 {
			return Err(format!("source {} hash mismatch", input.id).into());
		}
		source_hasher.update(input.id.as_bytes());
		source_hasher.update([0]);
		source_hasher.update(input.path.as_bytes());
		source_hasher.update([0]);
		source_hasher.update(input.sha256.as_bytes());
		source_hasher.update([0]);
		previous = Some(&input.id);
	}
	let digest: [u8; 32] = source_hasher.finalize().into();
	if hex(&digest) != lock.source_digest {
		return Err("source-lock aggregate digest mismatch".into());
	}
	Ok(digest)
}

fn hex(bytes: &[u8; 32]) -> String {
	use std::fmt::Write as _;
	let mut output = String::with_capacity(64);
	for byte in bytes {
		write!(&mut output, "{byte:02x}").expect("writing to String cannot fail");
	}
	output
}
