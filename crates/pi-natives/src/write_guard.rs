//! Attempt-boundary write guard for AI developer workflow runs.
//!
//! # Overview
//! One [`TaskWriteGuard`] per run. The engine calls [`TaskWriteGuard::begin`]
//! before dispatching an attempt and [`TaskWriteGuard::settle`] on every exit —
//! acceptance, rejection, crash, cancel, or timeout. `settle` computes what the
//! attempt changed since `begin`, classifies each path against the writer's
//! declared scope and the protected set, restores unauthorized paths
//! byte-for-byte, and reports what it did. The guard is engine-agnostic: it
//! knows trees and globs, never phases or envelopes.
//!
//! # Snapshot mechanism
//! Content-addressed file snapshots keyed by `(xxh64, length)`, stored beside
//! the baseline file. Hashes — not sizes or mtimes — are the comparison, so a
//! same-size rewrite is caught. The object store is written with
//! [`std::fs::copy`], which already reaches `fclonefileat` on APFS and
//! `copy_file_range` on Linux, so on cloning filesystems a snapshot is a cheap
//! COW clone; identical content across attempts is stored once. `pi-iso`
//! backends were considered and rejected: they manage whole-tree mounts and
//! clone lifecycles, while an attempt boundary needs file-granular restores.
//! `git hash-object -w` was rejected because it writes into the user's own
//! object database and cannot represent ignored-mode metadata or symlinks.
//!
//! # Scan domain
//! The scan includes ignored, hidden, and untracked files: ignore rules must
//! not hide a protected path or bypass a writer's declared scope. Scope is
//! supplied at settle time, so the begin snapshot must cover all such paths.
//! Every `.git` entry is pruned at any depth (nested repository internals are
//! invisible, nested worktree files are not), and the guard's own state is
//! excluded from snapshots.
//!
//! # Baseline file format
//! JSON, versioned, written atomically:
//!
//! ```json
//! {
//!   "version": 1,
//!   "head": "<commit id or null>",
//!   "dirt": {
//!     "<repo-relative path>": {"kind": "file", "len": 12, "hash": 123, "mode": 420},
//!     "<deleted tracked path>": null
//!   }
//! }
//! ```
//!
//! `head` is the commit the working copy was on when the run started; `dirt`
//! maps every path `git status` reported dirty or untracked to its content
//! state at that moment (`null` for a tracked file deleted from the worktree).
//! When the file already exists, [`TaskWriteGuard::create`] loads it instead of
//! recapturing — a resumed run must not relabel interrupted work as user dirt.
//! Two sibling artifacts live next to it: `<baselineFile>.objects/` (the
//! content-addressed store) and `<baselineFile>.attempt` (the persisted
//! `begin()` manifest, same entry encoding), which is what makes `settle` after
//! a process crash restore against the original attempt boundary.

use std::{
	collections::{BTreeMap, HashSet},
	io::Read,
	path::{Path, PathBuf},
};

use globset::{Glob, GlobSet, GlobSetBuilder};
use napi::Result;
use napi_derive::napi;
use pi_vcs::types::{StatusOptions, UntrackedMode};
use pi_walker::{FileType, WalkOptions, collect_entries_without_heartbeat};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use xxhash_rust::xxh64::Xxh64;

use crate::tasks::changed_paths;

/// No phase may touch the workflow's own state, whether or not the operator
/// declared it. Protection beats authorization: declaring a protected path in
/// `writes` does not un-protect it.
const IMPLICIT_PROTECTED: &str = ".omp/adw/**";

const BASELINE_VERSION: u32 = 1;
// Version 1 omitted ignored paths; loading it with the expanded scan domain
// would misclassify pre-existing ignored files as creations and delete them.
const ATTEMPT_VERSION: u32 = 2;

/// One path's content identity. Hash and length together are the comparison
/// and the object-store key; mode is restored alongside bytes on Unix (0 on
/// Windows, where it carries no meaning).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum EntryState {
	File { len: u64, hash: u64, mode: u32 },
	Symlink { target: String },
}

/// The durable run baseline. See the module docs for the on-disk format.
#[derive(Debug, Serialize, Deserialize)]
struct Baseline {
	version: u32,
	head:    Option<String>,
	dirt:    BTreeMap<String, Option<EntryState>>,
}

/// The persisted `begin()` manifest: the attempt boundary itself.
#[derive(Debug, Serialize, Deserialize)]
struct AttemptManifest {
	version: u32,
	entries: BTreeMap<String, EntryState>,
}

#[napi(object)]
pub struct TaskWriteGuardOptions {
	/// Tree the guard watches; every reported path is relative to it.
	pub root:          String,
	/// Durable baseline location, outside `root` (in the run directory). Its
	/// `.objects/` and `.attempt` siblings are managed by the guard.
	pub baseline_file: String,
}

#[napi(object)]
pub struct TaskGuardSettleOptions {
	/// Repo-relative globs the attempt's writer declared. Omitted means
	/// unrestricted except protected paths; an explicit empty list denies
	/// every change.
	pub allowed:         Option<Vec<String>>,
	/// Repo-relative globs no attempt may change. `.omp/adw/**` is always
	/// added. A path matching both `allowed` and a protected glob is
	/// unauthorized: protection beats authorization.
	pub protected_globs: Vec<String>,
	/// Where the full unauthorized diff is preserved when a restoration
	/// cannot be performed safely.
	pub patch_dir:       String,
}

#[napi(object)]
#[derive(Debug)]
pub struct TaskGuardChange {
	pub path: String,
	/// `"modified"` · `"created"` · `"deleted"` · `"retargeted"`.
	pub kind: String,
}

#[napi(object)]
#[derive(Debug)]
pub struct TaskGuardReport {
	/// Every unauthorized change, whether it was restored or not.
	pub unauthorized:  Vec<TaskGuardChange>,
	/// Paths restored byte-for-byte to their `begin()` state.
	pub rolled_back:   Vec<String>,
	/// Paths that could not be restored safely. The tree is left as-is for
	/// them and the full unauthorized diff is preserved at `patchPath`; the
	/// caller must fail closed.
	pub unrecoverable: Vec<String>,
	/// Present exactly when `unrecoverable` is non-empty.
	pub patch_path:    Option<String>,
}

fn guard_fail(err: impl std::fmt::Display) -> napi::Error {
	napi::Error::from_reason(err.to_string())
}

/// Streaming xxh64 + exact length, so a multi-gigabyte artifact never has to
/// fit in memory.
fn hash_file(path: &Path) -> std::io::Result<(u64, u64)> {
	let mut file = std::fs::File::open(path)?;
	let mut hasher = Xxh64::new(0);
	let mut buf = [0u8; 64 * 1024];
	let mut len = 0u64;
	loop {
		let read = file.read(&mut buf)?;
		if read == 0 {
			break;
		}
		hasher.update(&buf[..read]);
		len += read as u64;
	}
	Ok((len, hasher.digest()))
}

#[cfg(unix)]
fn file_mode(meta: &std::fs::Metadata) -> u32 {
	use std::os::unix::fs::PermissionsExt;
	meta.permissions().mode() & 0o7777
}

#[cfg(not(unix))]
fn file_mode(_meta: &std::fs::Metadata) -> u32 {
	0
}

/// The state of one path right now, `None` when it is absent or a directory.
fn entry_state(root: &Path, rel: &str) -> std::io::Result<Option<EntryState>> {
	let path = root.join(rel);
	let meta = match std::fs::symlink_metadata(&path) {
		Ok(meta) => meta,
		Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
		Err(err) => return Err(err),
	};
	if meta.file_type().is_symlink() {
		let target = std::fs::read_link(&path)?;
		return Ok(Some(EntryState::Symlink { target: target.to_string_lossy().into_owned() }));
	}
	if !meta.is_file() {
		return Ok(None);
	}
	let (len, hash) = hash_file(&path)?;
	Ok(Some(EntryState::File { len, hash, mode: file_mode(&meta) }))
}

/// Hash every file and symlink under `root`, skipping the guard's own state.
///
/// Races with a live writer are the caller's contract to avoid — `begin` and
/// `settle` are attempt boundaries, where the tree is quiescent.
fn scan_tree(
	root: &Path,
	skip: &[String],
) -> std::result::Result<BTreeMap<String, EntryState>, String> {
	let options = WalkOptions {
		include_hidden: true,
		use_gitignore: false,
		skip_git: true,
		..WalkOptions::default()
	};
	let collected = collect_entries_without_heartbeat(root, options)
		.map_err(|err| format!("scan of {} failed: {err:?}", root.display()))?;
	let candidates: Vec<String> = collected
		.entries
		.into_iter()
		.filter(|entry| matches!(entry.file_type, FileType::File | FileType::Symlink))
		.filter(|entry| {
			!skip
				.iter()
				.any(|prefix| entry.path == *prefix || entry.path.starts_with(&format!("{prefix}/")))
		})
		.map(|entry| entry.path)
		.collect();
	let states: Vec<Option<(String, EntryState)>> = candidates
		.into_par_iter()
		.map(|rel| match entry_state(root, &rel) {
			// A path that vanished between the walk and the stat is simply
			// absent; blaming it on the attempt would invent a change.
			Ok(state) => Ok(state.map(|state| (rel, state))),
			Err(err) => Err(format!("cannot read {rel}: {err}")),
		})
		.collect::<std::result::Result<_, String>>()?;
	Ok(states.into_iter().flatten().collect())
}

/// HEAD plus every dirty/untracked path with its content state. An unreadable
/// or non-git tree yields an empty baseline rather than a failure: the guard's
/// change detection never depended on git, only the user-dirt attribution does.
fn capture_baseline(root: &Path) -> Baseline {
	let mut baseline =
		Baseline { version: BASELINE_VERSION, head: None, dirt: BTreeMap::new() };
	let Ok(Some(repo)) = pi_vcs::detect(root) else {
		return baseline;
	};
	baseline.head = repo.head_id().ok().flatten();
	let options = StatusOptions {
		untracked:      UntrackedMode::All,
		pathspecs:      Vec::new(),
		nul_terminated: true,
	};
	let Ok(porcelain) = repo.status_porcelain(&options) else {
		return baseline;
	};
	for rel in changed_paths(&porcelain) {
		if baseline.dirt.contains_key(&rel) {
			continue;
		}
		let state = entry_state(root, &rel).ok().flatten();
		baseline.dirt.insert(rel, state);
	}
	baseline
}

/// Temp-and-rename so a crash mid-write never leaves a torn state file.
fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
	if let Some(parent) = path.parent() {
		std::fs::create_dir_all(parent)?;
	}
	let mut tmp = path.as_os_str().to_owned();
	tmp.push(".tmp");
	let tmp = PathBuf::from(tmp);
	std::fs::write(&tmp, bytes)?;
	std::fs::rename(&tmp, path)
}

fn object_name(len: u64, hash: u64) -> String {
	format!("{hash:016x}-{len:x}")
}

/// Compile settle globs, naming the pattern that is wrong — argument
/// validation, so a bad scope fails before anything is compared or restored.
fn compile_globs(label: &str, patterns: &[String]) -> Result<GlobSet> {
	let mut builder = GlobSetBuilder::new();
	for pattern in patterns {
		let glob = Glob::new(pattern)
			.map_err(|err| guard_fail(format!("{label} glob {pattern:?}: {err}")))?;
		builder.add(glob);
	}
	builder.build().map_err(guard_fail)
}

fn change_kind(begin: Option<&EntryState>, now: Option<&EntryState>) -> &'static str {
	match (begin, now) {
		(None, Some(_)) => "created",
		(Some(_), None) => "deleted",
		(Some(EntryState::Symlink { .. }), Some(EntryState::Symlink { .. })) => "retargeted",
		_ => "modified",
	}
}

/// Remove an existing non-directory occupant of `path`. A directory is never
/// removed — whatever grew there is not this path's history to erase.
fn clear_occupant(path: &Path) -> std::result::Result<(), String> {
	match std::fs::symlink_metadata(path) {
		Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
		Err(err) => Err(format!("cannot stat: {err}")),
		Ok(meta) if meta.is_dir() => Err("path is now a directory".to_owned()),
		Ok(_) => std::fs::remove_file(path).map_err(|err| format!("cannot remove: {err}")),
	}
}

/// Reject paths that resolve outside `root` (symlink traversal).
fn assert_within_root(root: &Path, rel: &str) -> std::result::Result<(), String> {
	let absolute = root.join(rel);
	// Resolve the deepest existing ancestor against a canonicalized root so a
	// symlinked directory (macOS /var -> /private/var, or a hostile symlink
	// planted inside the root) is resolved consistently — a nonexistent leaf
	// must not be compared unresolved against a resolved root, which would
	// false-positive on symlinked prefixes.
	let root_canonical = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
	let mut probe = absolute.as_path();
	loop {
		if probe == root {
			return Ok(());
		}
		match std::fs::canonicalize(probe) {
			Ok(canonical) if canonical.starts_with(&root_canonical) => return Ok(()),
			Ok(_) => return Err(format!("path escapes workspace root: {rel}")),
			Err(_) => match probe.parent() {
				Some(parent) if parent != probe => probe = parent,
				_ => return Ok(()),
			},
		}
	}
}

fn restore_file(
	root: &Path,
	objects: &Path,
	rel: &str,
	len: u64,
	hash: u64,
	mode: u32,
) -> std::result::Result<(), String> {
	let src = objects.join(object_name(len, hash));
	if !src.is_file() {
		return Err("attempt snapshot object is missing".to_owned());
	}
	assert_within_root(root, rel)?;
	let dst = root.join(rel);
	clear_occupant(&dst)?;
	if let Some(parent) = dst.parent() {
		std::fs::create_dir_all(parent).map_err(|err| format!("cannot recreate parent: {err}"))?;
	}
	std::fs::copy(&src, &dst).map_err(|err| format!("cannot restore bytes: {err}"))?;
	#[cfg(unix)]
	{
		use std::os::unix::fs::PermissionsExt;
		std::fs::set_permissions(&dst, std::fs::Permissions::from_mode(mode))
			.map_err(|err| format!("cannot restore mode: {err}"))?;
	}
	#[cfg(not(unix))]
	let _ = mode;
	Ok(())
}

fn restore_symlink(root: &Path, rel: &str, target: &str) -> std::result::Result<(), String> {
	assert_within_root(root, rel)?;
	let dst = root.join(rel);
	clear_occupant(&dst)?;
	if let Some(parent) = dst.parent() {
		std::fs::create_dir_all(parent).map_err(|err| format!("cannot recreate parent: {err}"))?;
	}
	#[cfg(unix)]
	return std::os::unix::fs::symlink(target, &dst)
		.map_err(|err| format!("cannot recreate symlink: {err}"));
	#[cfg(windows)]
	return std::os::windows::fs::symlink_file(target, &dst)
		.map_err(|err| format!("cannot recreate symlink: {err}"));
	#[cfg(not(any(unix, windows)))]
	Err("symlinks are not supported on this platform".to_owned())
}

/// Remove a created path, then prune ancestor directories the attempt must
/// have created — empty, and not an ancestor of anything that existed at
/// `begin()`. `remove_dir` refuses non-empty directories, so a shared parent
/// survives on its own.
fn remove_created(
	root: &Path,
	rel: &str,
	begin_dirs: &HashSet<String>,
) -> std::result::Result<(), String> {
	assert_within_root(root, rel)?;
	clear_occupant(&root.join(rel))?;
	let mut dir = rel;
	while let Some(cut) = dir.rfind('/') {
		dir = &dir[..cut];
		if begin_dirs.contains(dir) || std::fs::remove_dir(root.join(dir)).is_err() {
			break;
		}
	}
	Ok(())
}

fn is_text(bytes: &[u8]) -> bool {
	!bytes.contains(&0) && std::str::from_utf8(bytes).is_ok()
}

/// Append one file's full-replacement unified diff. Every old line is removed
/// and every new line added — larger than a minimal diff, but exact, dependency
/// free, and consumable by `git apply` / `patch -p1`. `old_exists`/`new_exists`
/// keep an empty-but-present file distinct from an absent one.
fn push_unified(
	out: &mut String,
	path: &str,
	old: &str,
	new: &str,
	old_exists: bool,
	new_exists: bool,
) {
	out.push_str(&format!("diff --git a/{path} b/{path}\n"));
	out.push_str(&if old_exists {
		format!("--- a/{path}\n")
	} else {
		"--- /dev/null\n".to_owned()
	});
	out.push_str(&if new_exists {
		format!("+++ b/{path}\n")
	} else {
		"+++ /dev/null\n".to_owned()
	});
	let old_lines: Vec<&str> = old.split_inclusive('\n').collect();
	let new_lines: Vec<&str> = new.split_inclusive('\n').collect();
	let old_start = if old_lines.is_empty() { 0 } else { 1 };
	let new_start = if new_lines.is_empty() { 0 } else { 1 };
	out.push_str(&format!(
		"@@ -{old_start},{} +{new_start},{} @@\n",
		old_lines.len(),
		new_lines.len()
	));
	for (sign, lines) in [('-', &old_lines), ('+', &new_lines)] {
		for line in lines {
			out.push(sign);
			out.push_str(line.strip_suffix('\n').unwrap_or(line));
			out.push('\n');
			if !line.ends_with('\n') {
				out.push_str("\\ No newline at end of file\n");
			}
		}
	}
}

/// The full unauthorized diff, rendered before anything is restored — once a
/// rollback starts, the "current" side of already-restored paths is gone.
/// Binary content cannot ride in a text patch; its current bytes are returned
/// as blobs to be preserved verbatim next to it.
fn render_patch(
	root: &Path,
	objects: &Path,
	unauthorized: &[(String, Option<EntryState>, Option<EntryState>)],
) -> (String, Vec<(String, Vec<u8>)>) {
	let mut text = String::new();
	let mut blobs: Vec<(String, Vec<u8>)> = Vec::new();
	for (rel, begin, now) in unauthorized {
		let old_bytes: Vec<u8> = match begin {
			None => Vec::new(),
			Some(EntryState::Symlink { target }) => format!("symlink -> {target}\n").into_bytes(),
			Some(EntryState::File { len, hash, .. }) => {
				std::fs::read(objects.join(object_name(*len, *hash))).unwrap_or_else(|_| {
					text.push_str(&format!("# begin snapshot missing for {rel}\n"));
					Vec::new()
				})
			},
		};
		let new_bytes: Vec<u8> = match now {
			None => Vec::new(),
			Some(EntryState::Symlink { .. }) => std::fs::read_link(root.join(rel))
				.map(|target| format!("symlink -> {}\n", target.to_string_lossy()).into_bytes())
				.unwrap_or_default(),
			Some(EntryState::File { .. }) => std::fs::read(root.join(rel)).unwrap_or_default(),
		};
		if is_text(&old_bytes) && is_text(&new_bytes) {
			push_unified(
				&mut text,
				rel,
				std::str::from_utf8(&old_bytes).unwrap_or_default(),
				std::str::from_utf8(&new_bytes).unwrap_or_default(),
				begin.is_some(),
				now.is_some(),
			);
		} else {
			text.push_str(&format!(
				"diff --git a/{rel} b/{rel}\nBinary files a/{rel} and b/{rel} differ (current bytes \
				 preserved at blobs/{rel})\n"
			));
			blobs.push((rel.clone(), new_bytes));
		}
	}
	(text, blobs)
}

/// Attempt-boundary write guard. `create` establishes (or reloads) the durable
/// run baseline, `begin` snapshots the tree at an attempt boundary, and
/// `settle` detects, classifies, and rolls back unauthorized changes.
#[napi]
pub struct TaskWriteGuard {
	root:         PathBuf,
	baseline:     Baseline,
	objects_dir:  PathBuf,
	attempt_file: PathBuf,
	/// Repo-relative locations of the guard's own state, pruned from every
	/// scan so the guard can never observe itself.
	state_rel:    Vec<String>,
	begin_state:  Option<BTreeMap<String, EntryState>>,
}

#[napi]
impl TaskWriteGuard {
	/// Captures the durable run baseline — HEAD plus dirty/untracked content
	/// states — or, when `baselineFile` already exists, loads it so a resumed
	/// run keeps the original attribution instead of relabeling interrupted
	/// work as user dirt. Also reloads a persisted `begin()` manifest, so
	/// `settle` after a process crash restores against the real boundary.
	#[napi(factory)]
	pub fn create(options: TaskWriteGuardOptions) -> Result<Self> {
		let root = PathBuf::from(&options.root);
		if !root.is_dir() {
			return Err(guard_fail(format!("root {} is not a directory", root.display())));
		}
		let baseline_file = PathBuf::from(&options.baseline_file);
		let mut objects_dir = baseline_file.as_os_str().to_owned();
		objects_dir.push(".objects");
		let objects_dir = PathBuf::from(objects_dir);
		let mut attempt_file = baseline_file.as_os_str().to_owned();
		attempt_file.push(".attempt");
		let attempt_file = PathBuf::from(attempt_file);

		let baseline = match std::fs::read(&baseline_file) {
			Ok(bytes) => {
				let baseline: Baseline = serde_json::from_slice(&bytes).map_err(|err| {
					guard_fail(format!("baseline {} is corrupt: {err}", baseline_file.display()))
				})?;
				if baseline.version != BASELINE_VERSION {
					return Err(guard_fail(format!(
						"baseline {} has version {}, expected {BASELINE_VERSION}",
						baseline_file.display(),
						baseline.version
					)));
				}
				baseline
			},
			Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
				let baseline = capture_baseline(&root);
				let bytes = serde_json::to_vec_pretty(&baseline).map_err(guard_fail)?;
				write_atomic(&baseline_file, &bytes).map_err(|err| {
					guard_fail(format!("cannot persist baseline {}: {err}", baseline_file.display()))
				})?;
				baseline
			},
			Err(err) => {
				return Err(guard_fail(format!(
					"cannot read baseline {}: {err}",
					baseline_file.display()
				)));
			},
		};

		let begin_state = match std::fs::read(&attempt_file) {
			Ok(bytes) => {
				let manifest: AttemptManifest = serde_json::from_slice(&bytes).map_err(|err| {
					guard_fail(format!("attempt manifest {} is corrupt: {err}", attempt_file.display()))
				})?;
				if manifest.version != ATTEMPT_VERSION {
					return Err(guard_fail(format!(
						"attempt manifest {} has version {}, expected {ATTEMPT_VERSION}",
						attempt_file.display(),
						manifest.version
					)));
				}
				Some(manifest.entries)
			},
			Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
			Err(err) => {
				return Err(guard_fail(format!(
					"cannot read attempt manifest {}: {err}",
					attempt_file.display()
				)));
			},
		};

		// The guard's state belongs in the run directory, outside the tree; if
		// a caller puts it inside anyway, the guard must never scan itself.
		let state_rel = [&baseline_file, &objects_dir, &attempt_file]
			.into_iter()
			.filter_map(|path| path.strip_prefix(&root).ok())
			.map(|rel| rel.to_string_lossy().replace('\\', "/"))
			.collect();

		Ok(Self { root, baseline, objects_dir, attempt_file, state_rel, begin_state })
	}

	/// Snapshot the tree at an attempt boundary: hash every watched file,
	/// store content not already in the object store, and persist the manifest
	/// so a crashed process can still settle against it.
	#[napi]
	pub fn begin(&mut self) -> Result<()> {
		let state = scan_tree(&self.root, &self.state_rel).map_err(guard_fail)?;
		// Object name -> one source path holding that content. First writer
		// wins; identical content elsewhere in the tree lands on the same
		// object, and content already stored by an earlier attempt is skipped.
		let mut to_store: BTreeMap<String, &str> = BTreeMap::new();
		for (rel, entry) in &state {
			if let EntryState::File { len, hash, .. } = entry {
				let name = object_name(*len, *hash);
				if !self.objects_dir.join(&name).is_file() {
					to_store.entry(name).or_insert(rel.as_str());
				}
			}
		}
		if !to_store.is_empty() {
			std::fs::create_dir_all(&self.objects_dir).map_err(guard_fail)?;
			to_store
				.par_iter()
				.try_for_each(|(name, rel)| {
					let tmp = self.objects_dir.join(format!(".tmp-{name}"));
					std::fs::copy(self.root.join(rel), &tmp)
						.map_err(|err| format!("cannot snapshot {rel}: {err}"))?;
					std::fs::rename(&tmp, self.objects_dir.join(name))
						.map_err(|err| format!("cannot commit snapshot of {rel}: {err}"))?;
					Ok(())
				})
				.map_err(|err: String| guard_fail(err))?;
		}
		let manifest = AttemptManifest { version: ATTEMPT_VERSION, entries: state };
		let bytes = serde_json::to_vec(&manifest).map_err(guard_fail)?;
		write_atomic(&self.attempt_file, &bytes).map_err(|err| {
			guard_fail(format!(
				"cannot persist attempt manifest {}: {err}",
				self.attempt_file.display()
			))
		})?;
		self.begin_state = Some(manifest.entries);
		Ok(())
	}

	/// Detect, classify, and roll back. Safe to call after a crash, cancel, or
	/// timeout with no submission — it only needs the persisted `begin()`
	/// state — and idempotent: a second settle over a restored tree finds
	/// nothing left to do.
	#[napi]
	pub fn settle(&mut self, options: TaskGuardSettleOptions) -> Result<TaskGuardReport> {
		// Argument validation first: a malformed scope must fail before
		// anything is compared or restored.
		let allowed = options
			.allowed
			.as_deref()
			.map(|patterns| compile_globs("writes", patterns))
			.transpose()?;
		let mut protected_patterns = options.protected_globs.clone();
		protected_patterns.push(IMPLICIT_PROTECTED.to_owned());
		let protected = compile_globs("protected", &protected_patterns)?;
		if options.patch_dir.is_empty() {
			return Err(guard_fail("patchDir must not be empty"));
		}
		let Some(begin) = self.begin_state.as_ref() else {
			return Err(guard_fail("settle() before begin(): no attempt snapshot exists"));
		};

		let mut skip = self.state_rel.clone();
		if let Ok(rel) = Path::new(&options.patch_dir).strip_prefix(&self.root) {
			skip.push(rel.to_string_lossy().replace('\\', "/"));
		}
		let now = scan_tree(&self.root, &skip).map_err(guard_fail)?;

		// Union of both key sets, in path order. A path untouched since
		// `begin()` — including pre-existing user dirt — never appears.
		let mut unauthorized: Vec<(String, Option<EntryState>, Option<EntryState>)> = Vec::new();
		let mut report = TaskGuardReport {
			unauthorized:  Vec::new(),
			rolled_back:   Vec::new(),
			unrecoverable: Vec::new(),
			patch_path:    None,
		};
		let mut paths: Vec<&String> = begin.keys().chain(now.keys()).collect();
		paths.sort_unstable();
		paths.dedup();
		for rel in paths {
			let before = begin.get(rel);
			let after = now.get(rel);
			if before == after {
				continue;
			}
			let authorized =
				allowed.as_ref().is_none_or(|scope| scope.is_match(rel)) && !protected.is_match(rel);
			if authorized {
				continue;
			}
			report.unauthorized.push(TaskGuardChange {
				path: rel.clone(),
				kind: change_kind(before, after).to_owned(),
			});
			unauthorized.push((rel.clone(), before.cloned(), after.cloned()));
		}
		unauthorized.sort_by(|a, b| a.0.cmp(&b.0));
		report.unauthorized.sort_by(|a, b| a.path.cmp(&b.path));

		// Rendered before any restore: rolling back destroys the "current"
		// side this evidence needs.
		let (patch_text, patch_blobs) = render_patch(&self.root, &self.objects_dir, &unauthorized);

		let begin_dirs: HashSet<String> = begin
			.keys()
			.flat_map(|rel| {
				let mut dirs = Vec::new();
				let mut dir = rel.as_str();
				while let Some(cut) = dir.rfind('/') {
					dir = &dir[..cut];
					dirs.push(dir.to_owned());
				}
				dirs
			})
			.collect();

		// Creations first, contents before parents, so a file that displaced a
		// directory tree is removable before the original file is recreated.
		let mut ordered: Vec<&(String, Option<EntryState>, Option<EntryState>)> =
			unauthorized.iter().collect();
		ordered.sort_by(|a, b| {
			let rank = |change: &(String, Option<EntryState>, Option<EntryState>)| {
				u8::from(change.1.is_some())
			};
			rank(a)
				.cmp(&rank(b))
				.then_with(|| pi_walker::compare_depth_first_paths(&a.0, &b.0))
		});
		let mut failures: Vec<(String, String)> = Vec::new();
		for (rel, before, _) in ordered {
			let outcome = match before {
				None => remove_created(&self.root, rel, &begin_dirs),
				Some(EntryState::File { len, hash, mode }) => {
					restore_file(&self.root, &self.objects_dir, rel, *len, *hash, *mode)
				},
				Some(EntryState::Symlink { target }) => restore_symlink(&self.root, rel, target),
			};
			match outcome {
				Ok(()) => report.rolled_back.push(rel.clone()),
				Err(reason) => failures.push((rel.clone(), reason)),
			}
		}
		report.rolled_back.sort();

		if !failures.is_empty() {
			// Fail-closed evidence is not optional: if the diff cannot be
			// preserved, settle itself fails and the caller must treat the
			// attempt as poisoned.
			let patch_dir = PathBuf::from(&options.patch_dir);
			std::fs::create_dir_all(&patch_dir).map_err(|err| {
				guard_fail(format!("cannot create patch dir {}: {err}", patch_dir.display()))
			})?;
			let millis = std::time::SystemTime::now()
				.duration_since(std::time::UNIX_EPOCH)
				.map_or(0, |d| d.as_millis());
			let patch_path = patch_dir.join(format!("unauthorized-{millis}.patch"));
			let mut full = String::new();
			for (rel, reason) in &failures {
				full.push_str(&format!("# unrecoverable {rel}: {reason}\n"));
			}
			full.push_str(&patch_text);
			write_atomic(&patch_path, full.as_bytes()).map_err(|err| {
				guard_fail(format!("cannot preserve patch {}: {err}", patch_path.display()))
			})?;
			for (rel, bytes) in &patch_blobs {
				write_atomic(&patch_dir.join("blobs").join(rel), bytes)
					.map_err(|err| guard_fail(format!("cannot preserve blob for {rel}: {err}")))?;
			}
			report.patch_path = Some(patch_path.to_string_lossy().into_owned());
			report.unrecoverable = failures.into_iter().map(|(rel, _)| rel).collect();
			report.unrecoverable.sort();
		}
		Ok(report)
	}

	/// The run baseline's dirty/untracked paths — the user's work, loaded from
	/// the durable baseline on resume. Report-relevant only: an untouched
	/// dirty file never appears in a settle report and is never restored.
	#[napi(getter)]
	pub fn baseline_dirt(&self) -> Vec<String> {
		self.baseline.dirt.keys().cloned().collect()
	}

	/// The commit the working copy was on when the run started, if any.
	#[napi(getter)]
	pub fn baseline_head(&self) -> Option<String> {
		self.baseline.head.clone()
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	struct Tree {
		base: PathBuf,
	}

	impl Tree {
		fn new(label: &str) -> Self {
			let base = std::env::temp_dir().join(format!(
				"pi-guard-{label}-{}-{:?}",
				std::process::id(),
				std::thread::current().id()
			));
			let _ = std::fs::remove_dir_all(&base);
			std::fs::create_dir_all(base.join("repo")).expect("repo dir");
			std::fs::create_dir_all(base.join("run")).expect("run dir");
			Self { base }
		}

		fn root(&self) -> PathBuf {
			self.base.join("repo")
		}

		fn guard(&self) -> TaskWriteGuard {
			TaskWriteGuard::create(TaskWriteGuardOptions {
				root:          self.root().to_string_lossy().into_owned(),
				baseline_file: self
					.base
					.join("run/baseline.json")
					.to_string_lossy()
					.into_owned(),
			})
			.expect("guard")
		}

		fn write(&self, rel: &str, content: &str) {
			let path = self.root().join(rel);
			std::fs::create_dir_all(path.parent().expect("parent")).expect("parents");
			std::fs::write(path, content).expect("write");
		}

		fn read(&self, rel: &str) -> String {
			std::fs::read_to_string(self.root().join(rel)).expect("read")
		}

		fn settle_opts(&self, allowed: &[&str], protected: &[&str]) -> TaskGuardSettleOptions {
			TaskGuardSettleOptions {
				allowed:         Some(allowed.iter().map(|s| (*s).to_owned()).collect()),
				protected_globs: protected.iter().map(|s| (*s).to_owned()).collect(),
				patch_dir:       self.base.join("run/patches").to_string_lossy().into_owned(),
			}
		}
	}

	impl Drop for Tree {
		fn drop(&mut self) {
			let _ = std::fs::remove_dir_all(&self.base);
		}
	}

	fn kinds(report: &TaskGuardReport) -> Vec<(String, String)> {
		report
			.unauthorized
			.iter()
			.map(|change| (change.path.clone(), change.kind.clone()))
			.collect()
	}

	fn git_init(root: &Path) {
		let ok = std::process::Command::new("git")
			.args(["init", "-q"])
			.current_dir(root)
			.status()
			.expect("git runs")
			.success();
		assert!(ok, "git init");
	}

	#[test]
	fn a_same_size_unauthorized_rewrite_is_caught_and_restored_byte_for_byte() {
		let tree = Tree::new("same-size");
		tree.write("src/a.txt", "aaaa");
		let mut guard = tree.guard();
		guard.begin().expect("begin");
		// Same length and a fresh mtime either way: only content hashes can
		// tell these apart, which is the point of the snapshot design.
		tree.write("src/a.txt", "bbbb");
		tree.write("src/ok.txt", "fine");
		let report = guard
			.settle(tree.settle_opts(&["src/ok.txt"], &[]))
			.expect("settle");
		assert_eq!(kinds(&report), vec![("src/a.txt".to_owned(), "modified".to_owned())]);
		assert_eq!(report.rolled_back, vec!["src/a.txt"]);
		assert!(report.unrecoverable.is_empty());
		assert!(report.patch_path.is_none());
		assert_eq!(tree.read("src/a.txt"), "aaaa");
		assert_eq!(tree.read("src/ok.txt"), "fine", "the declared creation must survive");
	}

	#[test]
	fn an_unauthorized_creation_is_deleted_and_a_deletion_restored() {
		let tree = Tree::new("create-delete");
		tree.write("src/a.txt", "alpha");
		let mut guard = tree.guard();
		guard.begin().expect("begin");
		std::fs::remove_file(tree.root().join("src/a.txt")).expect("delete");
		tree.write("src/new/n.txt", "n");
		let report = guard
			.settle(tree.settle_opts(&["docs/**"], &[]))
			.expect("settle");
		assert_eq!(kinds(&report), vec![
			("src/a.txt".to_owned(), "deleted".to_owned()),
			("src/new/n.txt".to_owned(), "created".to_owned()),
		]);
		assert_eq!(tree.read("src/a.txt"), "alpha");
		assert!(!tree.root().join("src/new/n.txt").exists());
		assert!(
			!tree.root().join("src/new").exists(),
			"the directory the attempt created is pruned with its file"
		);
	}

	#[test]
	fn protection_beats_authorization_even_when_declared_in_writes() {
		let tree = Tree::new("protected");
		tree.write("secrets/key.pem", "k1");
		let mut guard = tree.guard();
		guard.begin().expect("begin");
		tree.write("secrets/key.pem", "k2");
		let report = guard
			.settle(tree.settle_opts(&["secrets/**"], &["secrets/**"]))
			.expect("settle");
		assert_eq!(kinds(&report), vec![("secrets/key.pem".to_owned(), "modified".to_owned())]);
		assert_eq!(
			tree.read("secrets/key.pem"),
			"k1",
			"declaring a forbidden path authorizes nothing"
		);
	}

	#[test]
	fn omitted_writes_is_unrestricted_but_the_implicit_protection_still_blocks() {
		let tree = Tree::new("unrestricted");
		tree.write("a.txt", "one");
		tree.write(".omp/adw/state.json", "{}");
		let mut guard = tree.guard();
		guard.begin().expect("begin");
		tree.write("a.txt", "two");
		tree.write(".omp/adw/state.json", "{\"evil\":1}");
		let mut options = tree.settle_opts(&[], &[]);
		options.allowed = None;
		let report = guard.settle(options).expect("settle");
		assert_eq!(kinds(&report), vec![(".omp/adw/state.json".to_owned(), "modified".to_owned())]);
		assert_eq!(tree.read("a.txt"), "two", "omitted writes authorizes ordinary paths");
		assert_eq!(tree.read(".omp/adw/state.json"), "{}");
	}

	#[test]
	fn explicit_empty_writes_restores_edits_and_deletions_and_removes_ignored_creations() {
		let tree = Tree::new("deny-all");
		git_init(&tree.root());
		tree.write(".gitignore", "generated/\n");
		tree.write("a.txt", "user edit");
		tree.write("b.txt", "user bytes");
		let mut guard = tree.guard();
		guard.begin().expect("begin");
		tree.write("a.txt", "writer edit");
		std::fs::remove_file(tree.root().join("b.txt")).expect("delete");
		tree.write("generated/new.txt", "writer output");
		let report = guard.settle(tree.settle_opts(&[], &[])).expect("settle");
		assert_eq!(kinds(&report), vec![
			("a.txt".to_owned(), "modified".to_owned()),
			("b.txt".to_owned(), "deleted".to_owned()),
			("generated/new.txt".to_owned(), "created".to_owned()),
		]);
		assert!(report.unrecoverable.is_empty());
		assert_eq!(tree.read("a.txt"), "user edit");
		assert_eq!(tree.read("b.txt"), "user bytes");
		assert!(!tree.root().join("generated/new.txt").exists());
	}

	#[test]
	fn ignored_protected_paths_are_restored_while_scoped_output_and_git_metadata_survive() {
		let tree = Tree::new("ignored-protection");
		git_init(&tree.root());
		tree.write(".gitignore", "evaluators/\n.omp/\n");
		tree.write(".ignore", "generated/\n");
		tree.write("evaluators/check.txt", "original evaluator");
		tree.write(".omp/adw/state.json", "{}");
		tree.write("generated/allowed.txt", "original output");
		let mut guard = tree.guard();
		guard.begin().expect("begin");
		tree.write("evaluators/check.txt", "weakened evaluator");
		tree.write(".omp/adw/state.json", "{\"evil\":1}");
		tree.write("generated/allowed.txt", "accepted output");
		tree.write("generated/denied.txt", "unauthorized output");
		tree.write(".git/guard-marker", "metadata");
		tree.write("nested/.git", "gitdir: elsewhere\n");
		let report = guard
			.settle(tree.settle_opts(
				&["evaluators/**", ".omp/**", "generated/allowed.txt"],
				&["evaluators/**"],
			))
			.expect("settle");
		assert_eq!(kinds(&report), vec![
			(".omp/adw/state.json".to_owned(), "modified".to_owned()),
			("evaluators/check.txt".to_owned(), "modified".to_owned()),
			("generated/denied.txt".to_owned(), "created".to_owned()),
		]);
		assert!(report.unrecoverable.is_empty());
		assert_eq!(tree.read("evaluators/check.txt"), "original evaluator");
		assert_eq!(tree.read(".omp/adw/state.json"), "{}");
		assert_eq!(tree.read("generated/allowed.txt"), "accepted output");
		assert!(!tree.root().join("generated/denied.txt").exists());
		assert_eq!(tree.read(".git/guard-marker"), "metadata");
		assert_eq!(tree.read("nested/.git"), "gitdir: elsewhere\n");
	}

	#[test]
	fn legacy_attempt_manifests_refuse_resume_without_deleting_unsnapshotted_ignored_files() {
		let tree = Tree::new("legacy-manifest");
		tree.write(".gitignore", "evaluators/\n");
		tree.write("evaluators/check.txt", "user evaluator");
		drop(tree.guard());
		std::fs::write(
			tree.base.join("run/baseline.json.attempt"),
			br#"{"version":1,"entries":{}}"#,
		)
		.expect("legacy manifest");
		let resumed = TaskWriteGuard::create(TaskWriteGuardOptions {
			root:          tree.root().to_string_lossy().into_owned(),
			baseline_file: tree.base.join("run/baseline.json").to_string_lossy().into_owned(),
		});
		assert!(resumed.is_err(), "an incomplete legacy snapshot cannot safely restore ignored paths");
		assert_eq!(tree.read("evaluators/check.txt"), "user evaluator");
	}

	#[test]
	fn untouched_user_dirt_survives_settle_and_is_never_reported() {
		let tree = Tree::new("dirt");
		git_init(&tree.root());
		tree.write("dirty.txt", "user bytes");
		let mut guard = tree.guard();
		assert_eq!(guard.baseline_dirt(), vec!["dirty.txt"]);
		guard.begin().expect("begin");
		tree.write("out.txt", "phase output");
		let report = guard
			.settle(tree.settle_opts(&["out.txt"], &[]))
			.expect("settle");
		assert!(report.unauthorized.is_empty(), "{:?}", kinds(&report));
		assert!(report.rolled_back.is_empty());
		assert_eq!(tree.read("dirty.txt"), "user bytes");
	}

	#[test]
	fn a_reloaded_baseline_and_attempt_manifest_restore_after_a_simulated_crash() {
		let tree = Tree::new("resume");
		git_init(&tree.root());
		tree.write("dirty.txt", "user bytes");
		tree.write("a.txt", "orig");
		let mut guard = tree.guard();
		guard.begin().expect("begin");
		tree.write("a.txt", "evil");
		std::fs::remove_file(tree.root().join("dirty.txt")).expect("delete dirt");
		// Crash: the guard is dropped with no settle and no submission.
		drop(guard);

		let mut resumed = tree.guard();
		// Loaded from disk, not recaptured — a recapture would see the
		// interrupted attempt's tree (no dirty.txt, a.txt = "evil") and
		// relabel that work as user dirt.
		assert_eq!(resumed.baseline_dirt(), vec!["a.txt".to_owned(), "dirty.txt".to_owned()]);
		let report = resumed
			.settle(tree.settle_opts(&["none/**"], &[]))
			.expect("settle");
		assert_eq!(kinds(&report), vec![
			("a.txt".to_owned(), "modified".to_owned()),
			("dirty.txt".to_owned(), "deleted".to_owned()),
		]);
		assert_eq!(tree.read("a.txt"), "orig");
		assert_eq!(tree.read("dirty.txt"), "user bytes");
	}

	#[test]
	fn a_failed_restore_is_reported_unrecoverable_with_the_diff_preserved() {
		let tree = Tree::new("unrecoverable");
		tree.write("a.txt", "one\n");
		let mut guard = tree.guard();
		guard.begin().expect("begin");
		tree.write("a.txt", "two\n");
		// Corrupt the snapshot store: the restoration cannot be performed.
		std::fs::remove_dir_all(tree.base.join("run/baseline.json.objects")).expect("corrupt");
		let report = guard
			.settle(tree.settle_opts(&["none/**"], &[]))
			.expect("settle");
		assert_eq!(report.unrecoverable, vec!["a.txt"]);
		assert!(report.rolled_back.is_empty());
		assert_eq!(tree.read("a.txt"), "two\n", "the tree is left as-is for the path");
		let patch_path = report.patch_path.expect("patch preserved");
		let patch = std::fs::read_to_string(&patch_path).expect("patch readable");
		assert!(patch.contains("+two"), "{patch}");
		assert!(patch.contains("unrecoverable a.txt"), "{patch}");
	}

	#[test]
	fn a_malformed_settle_glob_fails_naming_the_pattern() {
		let tree = Tree::new("bad-glob");
		let mut guard = tree.guard();
		guard.begin().expect("begin");
		let err = guard
			.settle(tree.settle_opts(&["src/**/["], &[]))
			.expect_err("must refuse");
		assert!(err.reason.contains("src/**/["), "{err}");
		let err = guard
			.settle(tree.settle_opts(&[], &["docs/["]))
			.expect_err("must refuse");
		assert!(err.reason.contains("docs/["), "{err}");
	}

	#[test]
	fn settle_without_begin_refuses_instead_of_guessing_a_boundary() {
		let tree = Tree::new("no-begin");
		let mut guard = tree.guard();
		let err = guard
			.settle(tree.settle_opts(&[], &[]))
			.expect_err("must refuse");
		assert!(err.reason.contains("begin"), "{err}");
	}

	#[cfg(unix)]
	#[test]
	fn an_unauthorized_symlink_retarget_is_restored() {
		let tree = Tree::new("symlink");
		tree.write("a.txt", "a");
		tree.write("b.txt", "b");
		std::os::unix::fs::symlink("a.txt", tree.root().join("link")).expect("link");
		let mut guard = tree.guard();
		guard.begin().expect("begin");
		std::fs::remove_file(tree.root().join("link")).expect("unlink");
		std::os::unix::fs::symlink("b.txt", tree.root().join("link")).expect("relink");
		let report = guard
			.settle(tree.settle_opts(&["none/**"], &[]))
			.expect("settle");
		assert_eq!(kinds(&report), vec![("link".to_owned(), "retargeted".to_owned())]);
		let target = std::fs::read_link(tree.root().join("link")).expect("target");
		assert_eq!(target, Path::new("a.txt"));
	}
	#[cfg(unix)]
	#[test]
	fn a_replaced_directory_symlink_escape_is_refused_after_begin() {
		// The agent deletes the real `src` directory and replaces it with a
		// symlink pointing outside the worktree, then writes an allowed path
		// through it. Restore must not follow the link out of the root — it
		// refuses rather than writing the snapshot object outside the repo.
		let tree = Tree::new("escape");
		tree.write("src/a.txt", "aaa");
		let outside = tree.base.join("outside");
		std::fs::create_dir_all(&outside).expect("outside dir");
		let mut guard = tree.guard();
		guard.begin().expect("begin");
		std::fs::remove_dir_all(tree.root().join("src")).expect("remove real src");
		std::os::unix::fs::symlink(&outside, tree.root().join("src")).expect("alias");
		std::fs::write(outside.join("a.txt"), b"changed").expect("write through link");
		let report = guard
			.settle(tree.settle_opts(&["src/a.txt"], &[]))
			.expect("settle");
		// Restore must not follow the link out of the root: the replaced
		// directory is reported unrecoverable rather than written through.
		assert!(
			report.unrecoverable.iter().any(|p| p == "src" || p == "src/a.txt"),
			"restore must refuse the escape, got unrecoverable: {:?}",
			report.unrecoverable
		);
		assert!(
			report.rolled_back.is_empty(),
			"nothing may be restored through the escape: {:?}",
			report.rolled_back
		);
		// The outside file is whatever the attempt left; the worktree must not
		// be able to exfiltrate the snapshot object.
		assert_eq!(std::fs::read_to_string(outside.join("a.txt")).expect("outside"), "changed");
	}
}
