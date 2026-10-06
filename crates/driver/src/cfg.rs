//! Production cfg script files: the loader behind `exec <name>` /
//! `exec_configs` and the saver behind `writecfg`.
//!
//! Every cfg name resolves to at most two files, user first and project
//! overlay second (ADR 0013: `config.cfg`, `subagent.cfg`, `<agent>.cfg`,
//! and any user profile follow one layout):
//!
//! - `<profile config root>/<name>.cfg` — `~/.o2` by default,
//!   `~/.o2/profiles/<profile>` under `--profile`/`OMP_PROFILE`
//!   ([`omp_core::dirs::profile_config_dir`]);
//! - `<project>/.omp/<name>.cfg`.
//!
//! The two texts are never merged. [`CfgFiles::load`] returns the user text,
//! which runs with user authority; [`CfgFiles::load_project`] returns the
//! overlay, which `omp-con` runs after it with project authority: only `set`
//! and `reset` of convars that opt in with `VarFlags::PROJECT`, into a layer
//! `writecfg` never persists. A repository's `.omp/*.cfg` is content the user
//! did not write, so it must not run with the user's own power.
//! [`CfgFiles::save`] always writes the user file atomically (the project
//! overlay is edited by `omp config set --scope project`).

use std::{
	fs::{self, File, OpenOptions},
	io::{self, Write as _},
	path::{Path, PathBuf},
	sync::{
		Arc,
		atomic::{AtomicU64, Ordering},
	},
};

use omp_con::{
	CFG_HEADER_PREFIX, CFG_SCHEMA_VERSION, CfgLoader, CfgSaver, ConError, ConResult, ConfigIoError,
	ConfigOperation,
};
use omp_core::{
	FastHashMap, Str,
	dirs::DataDirError,
	project_file::{self, Containment},
};
use parking_lot::Mutex;

/// Largest project cfg overlay the reader accepts: a handful of `set` lines,
/// never a document.
pub const PROJECT_CFG_LIMIT: u64 = 256 * 1024;

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Cfg script resolution for one process: user root plus optional project.
#[derive(Clone, Debug)]
pub struct CfgFiles {
	user:     PathBuf,
	project:  Option<PathBuf>,
	observed: Arc<Mutex<FastHashMap<Str, Option<Str>>>>,
}

impl CfgFiles {
	/// Resolves the user root for the selected profile and the project's
	/// `.omp` overlay directory.
	///
	/// # Errors
	///
	/// [`DataDirError::HomeUnset`] when no home directory is set.
	pub fn new(project_root: Option<&Path>) -> Result<Self, DataDirError> {
		let user = omp_core::dirs::user_config_root()?;
		Ok(Self::with_roots(user, project_root.map(|root| root.join(".omp"))))
	}

	/// Explicit roots (tests, migration into a scratch directory).
	#[must_use]
	pub fn with_roots(user: PathBuf, project: Option<PathBuf>) -> Self {
		Self { user, project, observed: Arc::default() }
	}

	/// The user (profile) root every save lands in.
	#[must_use]
	pub fn user_root(&self) -> &Path {
		&self.user
	}

	/// The project overlay directory (`<project>/.omp`), when present.
	#[must_use]
	pub fn project_root(&self) -> Option<&Path> {
		self.project.as_deref()
	}

	/// User-file path for a cfg name (`config` and `config.cfg` are the same
	/// file).
	#[must_use]
	pub fn user_path(&self, name: &str) -> PathBuf {
		self.user.join(file_name(name))
	}

	/// Project-overlay path for a cfg name, when a project is attached.
	#[must_use]
	pub fn project_path(&self, name: &str) -> Option<PathBuf> {
		self.project.as_ref().map(|root| root.join(file_name(name)))
	}

	/// The user's script text, `None` when the file is absent. It runs with
	/// user authority. Generated files are migrated to the current schema in
	/// memory before execution.
	pub fn load(&self, name: &str) -> ConResult<Option<Str>> {
		validate_name(name)?;
		let user_path = self.user_path(name);
		let user = read_config(&user_path)?;
		self
			.observed
			.lock()
			.insert(Str::new(file_name(name)), user.as_deref().map(Str::new));
		Ok(user.filter(|script| !script.is_empty()).map(Str::new))
	}

	/// The project overlay's script text, `None` when no project is attached
	/// or the file is absent. It runs with project authority, after the user
	/// text, and is never written back by [`CfgFiles::save`].
	pub fn load_project(&self, name: &str) -> ConResult<Option<Str>> {
		validate_name(name)?;
		let Some(path) = self.project_path(name) else {
			return Ok(None);
		};
		Ok(read_project_config(&path)?
			.filter(|script| !script.is_empty())
			.map(Str::new))
	}

	/// Writes `contents` to the user file for `name` with a cross-process lock,
	/// a synchronized same-directory temporary, atomic replacement, and parent
	/// directory synchronization.
	pub fn save(&self, name: &str, contents: &str) -> ConResult<()> {
		validate_name(name)?;
		let path = self.user_path(name);
		let transaction = ConfigFileLock::acquire(path.clone())?;
		let current = transaction
			.read()?
			.map(|text| migrate_config_script(&path, &text))
			.transpose()?;
		let key = Str::new(file_name(name));
		let observed = self.observed.lock().get(&key).cloned();
		let changed = match observed {
			Some(observed) => current.as_deref() != observed.as_deref(),
			None => current.is_some(),
		};
		if changed {
			return Err(ConError::ConfigChanged { path });
		}
		let stored = migrate_config_script(&path, contents)?;
		transaction.replace_raw(stored.as_bytes())?;
		self.observed.lock().insert(key, Some(Str::new(stored)));
		Ok(())
	}
}

impl CfgLoader for CfgFiles {
	fn load(&self, name: &str) -> ConResult<Option<Str>> {
		Self::load(self, name)
	}

	fn load_project(&self, name: &str) -> ConResult<Option<Str>> {
		Self::load_project(self, name)
	}
}

impl CfgSaver for CfgFiles {
	fn save(&self, name: &str, contents: &str) -> ConResult<()> {
		Self::save(self, name, contents)
	}
}

/// `<name>.cfg`, tolerating an explicit `.cfg` suffix.
fn file_name(name: &str) -> String {
	format!("{}.cfg", name.trim_end_matches(".cfg"))
}

pub(crate) fn validate_name(name: &str) -> ConResult<()> {
	let stem = name.trim_end_matches(".cfg");
	let valid = !stem.is_empty()
		&& stem != "."
		&& stem != ".."
		&& stem
			.bytes()
			.all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'));
	if valid {
		Ok(())
	} else {
		Err(ConError::InvalidCfgName { name: Str::new(name) })
	}
}

fn io_error(operation: ConfigOperation, path: &Path, source: io::Error) -> ConError {
	ConfigIoError::new(operation, path.to_path_buf(), source).into()
}

fn read_optional(path: &Path) -> ConResult<Option<String>> {
	match fs::read_to_string(path) {
		Ok(text) => Ok(Some(text)),
		Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(None),
		Err(source) => Err(io_error(ConfigOperation::Read, path, source)),
	}
}

/// Reads one cfg with typed filesystem attribution and in-memory schema
/// migration. Absence remains distinct from a read failure.
pub fn read_config(path: &Path) -> ConResult<Option<String>> {
	read_optional(path)?
		.map(|text| migrate_config_script(path, &text))
		.transpose()
}

/// The project directory above a project cfg's `<project>/.omp/` overlay
/// directory: the root every project cfg read and write stays inside.
fn project_root_of(cfg_path: &Path) -> &Path {
	cfg_path
		.parent()
		.and_then(Path::parent)
		.filter(|root| !root.as_os_str().is_empty())
		.unwrap_or_else(|| Path::new("."))
}

/// Reads a project cfg overlay (`<project>/.omp/<name>.cfg`) through the
/// contained project-file reader.
///
/// The file must be a regular file of at most [`PROJECT_CFG_LIMIT`] bytes
/// whose canonical path stays inside the project. A repository's file is never
/// trusted to be a symlink to somewhere else, a FIFO, or a device. Absence is
/// `Ok(None)`.
///
/// # Errors
///
/// [`ConError::ProjectFile`] when the file is refused or unreadable, and the
/// errors of [`migrate_config_script`].
pub fn read_project_config(path: &Path) -> ConResult<Option<String>> {
	project_file::read_text(path, Containment::Within(project_root_of(path)), PROJECT_CFG_LIMIT)?
		.map(|text| migrate_config_script(path, &text))
		.transpose()
}

/// A held cross-process transaction lock for one cfg path.
///
/// Read-modify-write callers keep this value alive across both operations so
/// independent processes cannot overwrite one another's updates.
pub struct ConfigFileLock {
	path:    PathBuf,
	/// Project cfgs are read and written without following links outside the
	/// project; user cfgs may be symlinked by their owner.
	project: bool,
	_lock:   File,
}

impl ConfigFileLock {
	/// Locks a user-owned `path`, creating its parent and stable sibling lock
	/// file. Writes follow a symlinked `path`, so a dotfile manager keeps
	/// working.
	pub fn acquire(path: PathBuf) -> ConResult<Self> {
		Self::lock(path, false)
	}

	/// Locks a project cfg (`<project>/.omp/<name>.cfg`). Reads are contained
	/// like [`read_project_config`], and the lock, and every later write,
	/// refuses a symlink or non-regular `path` or a directory that resolves
	/// outside the project, since the repository supplied it.
	///
	/// # Errors
	///
	/// [`ConError::ProjectFile`] when the target is refused, plus the errors
	/// of [`ConfigFileLock::acquire`].
	pub fn acquire_project(path: PathBuf) -> ConResult<Self> {
		project_file::check_write_target(&path, project_root_of(&path))?;
		Self::lock(path, true)
	}

	fn lock(path: PathBuf, project: bool) -> ConResult<Self> {
		let parent = path.parent().unwrap_or_else(|| Path::new("."));
		fs::create_dir_all(parent)
			.map_err(|source| io_error(ConfigOperation::Create, parent, source))?;
		let lock_path = path.with_extension("cfg.lock");
		let lock = OpenOptions::new()
			.read(true)
			.write(true)
			.create(true)
			.open(&lock_path)
			.map_err(|source| io_error(ConfigOperation::Lock, &lock_path, source))?;
		lock_exclusive(&lock)
			.map_err(|source| io_error(ConfigOperation::Lock, &lock_path, source))?;
		Ok(Self { path, project, _lock: lock })
	}

	/// Logical cfg path protected by this transaction.
	#[must_use]
	pub fn path(&self) -> &Path {
		&self.path
	}

	/// Reads the current cfg while retaining the transaction lock.
	pub fn read(&self) -> ConResult<Option<String>> {
		if self.project {
			return Ok(project_file::read_text(
				&self.path,
				Containment::Within(project_root_of(&self.path)),
				PROJECT_CFG_LIMIT,
			)?);
		}
		read_optional(&self.path)
	}

	/// Crash-safely replaces the cfg while retaining the transaction lock.
	pub fn replace(&self, contents: &str) -> ConResult<()> {
		let contents = migrate_config_script(&self.path, contents)?;
		self.replace_raw(contents.as_bytes())
	}

	/// Crash-safely replaces non-cfg text protected by the same transaction
	/// primitive (for example a shell profile alias block).
	pub fn replace_raw(&self, contents: &[u8]) -> ConResult<()> {
		atomic_replace(&self.path, contents, self.project)
	}
}

fn atomic_replace(logical_path: &Path, contents: &[u8], project: bool) -> ConResult<()> {
	let path = if project {
		project_file::check_write_target(logical_path, project_root_of(logical_path))?;
		logical_path.to_path_buf()
	} else {
		resolve_write_target(logical_path)?
	};
	let parent = path.parent().unwrap_or_else(|| Path::new("."));
	fs::create_dir_all(parent)
		.map_err(|source| io_error(ConfigOperation::Create, parent, source))?;
	let file_name = path
		.file_name()
		.and_then(|name| name.to_str())
		.unwrap_or("config.cfg");
	let (temporary, mut file) = loop {
		let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
		let temporary = parent.join(format!(".{file_name}.tmp.{}.{}", std::process::id(), sequence));
		match create_private(&temporary) {
			Ok(file) => break (temporary, file),
			Err(source) if source.kind() == io::ErrorKind::AlreadyExists && sequence != u64::MAX => {},
			Err(source) => return Err(io_error(ConfigOperation::Write, &temporary, source)),
		}
	};
	let result = (|| {
		if let Ok(metadata) = fs::metadata(&path) {
			fs::set_permissions(&temporary, metadata.permissions())
				.map_err(|source| io_error(ConfigOperation::Write, &temporary, source))?;
		}
		file
			.write_all(contents)
			.map_err(|source| io_error(ConfigOperation::Write, &temporary, source))?;
		file
			.sync_all()
			.map_err(|source| io_error(ConfigOperation::Sync, &temporary, source))?;
		drop(file);
		replace_file(&temporary, &path)
			.map_err(|source| io_error(ConfigOperation::Replace, &path, source))?;
		sync_parent(parent).map_err(|source| io_error(ConfigOperation::Sync, parent, source))
	})();
	if result.is_err() {
		let _ = fs::remove_file(&temporary);
	}
	result
}

fn resolve_write_target(path: &Path) -> ConResult<PathBuf> {
	let mut current = path.to_path_buf();
	for _ in 0..40 {
		match fs::symlink_metadata(&current) {
			Ok(metadata) if metadata.file_type().is_symlink() => {
				let target = fs::read_link(&current)
					.map_err(|source| io_error(ConfigOperation::Read, &current, source))?;
				current = if target.is_absolute() {
					target
				} else {
					current
						.parent()
						.unwrap_or_else(|| Path::new("."))
						.join(target)
				};
			},
			Ok(_) => return Ok(current),
			Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(current),
			Err(source) => return Err(io_error(ConfigOperation::Read, &current, source)),
		}
	}
	Err(io_error(
		ConfigOperation::Read,
		&current,
		io::Error::from_raw_os_error(too_many_links_code()),
	))
}

#[cfg(unix)]
fn create_private(path: &Path) -> io::Result<File> {
	use std::os::unix::fs::OpenOptionsExt as _;
	OpenOptions::new()
		.write(true)
		.create_new(true)
		.mode(0o600)
		.open(path)
}

#[cfg(windows)]
fn create_private(path: &Path) -> io::Result<File> {
	OpenOptions::new().write(true).create_new(true).open(path)
}

#[cfg(unix)]
fn lock_exclusive(file: &File) -> io::Result<()> {
	use std::os::fd::AsRawFd as _;
	loop {
		// SAFETY: `file` owns a valid descriptor and flock retains no pointer.
		let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
		if result == 0 {
			return Ok(());
		}
		let source = io::Error::last_os_error();
		if source.kind() != io::ErrorKind::Interrupted {
			return Err(source);
		}
	}
}

#[cfg(windows)]
fn lock_exclusive(file: &File) -> io::Result<()> {
	use std::{mem::zeroed, os::windows::io::AsRawHandle as _};

	use windows_sys::Win32::{
		Storage::FileSystem::{LOCKFILE_EXCLUSIVE_LOCK, LockFileEx},
		System::IO::OVERLAPPED,
	};
	// SAFETY: OVERLAPPED is an integer/handle record valid when zeroed.
	let mut overlapped = unsafe { zeroed::<OVERLAPPED>() };
	// SAFETY: the handle and OVERLAPPED remain valid for this synchronous call.
	let result = unsafe {
		LockFileEx(
			file.as_raw_handle(),
			LOCKFILE_EXCLUSIVE_LOCK,
			0,
			u32::MAX,
			u32::MAX,
			&raw mut overlapped,
		)
	};
	if result != 0 {
		Ok(())
	} else {
		Err(io::Error::last_os_error())
	}
}

#[cfg(unix)]
fn replace_file(source: &Path, destination: &Path) -> io::Result<()> {
	fs::rename(source, destination)
}

#[cfg(windows)]
fn replace_file(source: &Path, destination: &Path) -> io::Result<()> {
	use std::os::windows::ffi::OsStrExt as _;

	use windows_sys::Win32::Storage::FileSystem::{
		MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
	};
	let source = source
		.as_os_str()
		.encode_wide()
		.chain(Some(0))
		.collect::<Vec<_>>();
	let destination = destination
		.as_os_str()
		.encode_wide()
		.chain(Some(0))
		.collect::<Vec<_>>();
	// SAFETY: both buffers are live, terminated UTF-16 strings.
	let result = unsafe {
		MoveFileExW(
			source.as_ptr(),
			destination.as_ptr(),
			MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
		)
	};
	if result != 0 {
		Ok(())
	} else {
		Err(io::Error::last_os_error())
	}
}

#[cfg(unix)]
fn sync_parent(parent: &Path) -> io::Result<()> {
	File::open(parent).and_then(|directory| directory.sync_all())
}

#[cfg(windows)]
fn sync_parent(_parent: &Path) -> io::Result<()> {
	Ok(())
}

#[cfg(unix)]
const fn too_many_links_code() -> i32 {
	libc::ELOOP
}

#[cfg(windows)]
const fn too_many_links_code() -> i32 {
	1921 // ERROR_CANT_RESOLVE_FILENAME
}

/// Drops the bare `unbindall` reset that the pre-baseline `dump` wrote at the
/// top of every generated cfg. It was a serializer preamble, never a user's
/// intent, and with defaults now seeded from the default bind cfg it would
/// erase them. A hand-written `unbindall` followed by `bind` lines is kept.
pub fn migrate_config_script(path: &Path, script: &str) -> ConResult<String> {
	let first = script.lines().next().unwrap_or_default();
	if !first.starts_with(CFG_HEADER_PREFIX) {
		return validate_config_script(path, script.to_owned());
	}
	let schema = first
		.split(';')
		.find_map(|part| part.trim().strip_prefix("schema="))
		.map(|version| {
			version
				.parse::<u32>()
				.map_err(|_| ConError::InvalidCfgSchema { path: path.to_path_buf() })
		})
		.transpose()?
		.unwrap_or(0);
	if schema > CFG_SCHEMA_VERSION {
		return Err(ConError::UnsupportedCfgSchema {
			path:      path.to_path_buf(),
			found:     schema,
			supported: CFG_SCHEMA_VERSION,
		});
	}
	let mut lines = script.lines().peekable();
	let _ = lines.next();
	let mut out = String::with_capacity(script.len());
	out.push_str("// generated by omp-con dump; schema=1; replay with `exec`\n");
	while let Some(line) = lines.next() {
		if line.trim() == "unbindall"
			&& !lines
				.peek()
				.is_some_and(|next| next.trim_start().starts_with("bind "))
		{
			continue;
		}
		out.push_str(line);
		out.push('\n');
	}
	validate_config_script(path, out)
}

fn validate_config_script(path: &Path, script: String) -> ConResult<String> {
	omp_con::parse(&Str::new(&script))
		.map_err(|source| ConError::ConfigParse { path: path.to_path_buf(), source })?;
	Ok(script)
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn names_resolve_to_user_and_project_files_that_stay_separate() {
		let dir = tempfile::tempdir().unwrap();
		let user = dir.path().join("o2");
		let project = dir.path().join("proj/.omp");
		fs::create_dir_all(&user).unwrap();
		fs::create_dir_all(&project).unwrap();
		let files = CfgFiles::with_roots(user.clone(), Some(project.clone()));
		assert_eq!(files.user_path("config"), user.join("config.cfg"));
		assert_eq!(files.user_path("sonic.cfg"), user.join("sonic.cfg"));
		assert_eq!(files.project_path("subagent"), Some(project.join("subagent.cfg")));
		assert_eq!(files.load("subagent").unwrap(), None);
		assert_eq!(files.load_project("subagent").unwrap(), None);
		fs::write(user.join("subagent.cfg"), "ai_fastmode 0").unwrap();
		assert_eq!(files.load("subagent").unwrap().unwrap().as_str(), "ai_fastmode 0");
		assert_eq!(files.load_project("subagent").unwrap(), None);
		fs::write(project.join("subagent.cfg"), "ai_thinking low\n").unwrap();
		assert_eq!(files.load("subagent.cfg").unwrap().unwrap().as_str(), "ai_fastmode 0");
		assert_eq!(
			files
				.load_project("subagent.cfg")
				.unwrap()
				.unwrap()
				.as_str(),
			"ai_thinking low\n",
			"the overlay is never merged into the user text"
		);
		let detached = CfgFiles::with_roots(user, None);
		assert_eq!(detached.load_project("subagent").unwrap(), None);
	}

	/// The subagent spawn path installs [`CfgFiles`] as its loader (kernel
	/// `TaskSessionTool`), so `~/.o2/subagent.cfg` and `~/.o2/<agent>.cfg`
	/// reach every child in ADR 0013 order — user script, project overlay,
	/// then the agent class — with `config.cfg` never re-read over the seed.
	#[test]
	fn spawn_configs_run_user_then_project_subagent_and_agent_cfgs() {
		let dir = tempfile::tempdir().unwrap();
		let user = dir.path().join("o2");
		let project = dir.path().join("proj/.omp");
		fs::create_dir_all(&user).unwrap();
		fs::create_dir_all(&project).unwrap();
		fs::write(user.join("config.cfg"), "ai_fastmode 1\nai_thinking low\n").unwrap();
		fs::write(user.join("subagent.cfg"), "ai_fastmode 0\nai_thinking medium\n").unwrap();
		fs::write(project.join("subagent.cfg"), "ai_thinking high\n").unwrap();
		fs::write(user.join("scout.cfg"), "ai_model @smol\n").unwrap();
		let files = CfgFiles::with_roots(user, Some(project));
		let child = omp_con::Ctx::new();
		// The parent's seed is the child's inherited layer, beneath the cfgs.
		omp_agent::AI_FASTMODE
			.set_in(&child, true, omp_con::Origin::Inherited)
			.unwrap();
		let outcome = child.exec_spawn_configs(&files, "scout").unwrap();
		assert_eq!(outcome.failed, 0);
		assert!(!omp_agent::AI_FASTMODE.get(&child), "user subagent.cfg ran");
		assert_eq!(omp_agent::AI_THINKING.get(&child).as_str(), "high", "project overlay ran last");
		assert_eq!(omp_agent::AI_MODEL.get(&child).as_str(), "@smol", "user <agent>.cfg ran");
	}

	#[test]
	fn save_writes_the_user_file_atomically_and_creates_the_root() {
		let dir = tempfile::tempdir().unwrap();
		let user = dir.path().join("missing/o2");
		let files = CfgFiles::with_roots(user.clone(), None);
		CfgSaver::save(&files, "profile", "ai_model @smol\n").unwrap();
		assert_eq!(fs::read_to_string(user.join("profile.cfg")).unwrap(), "ai_model @smol\n");
		assert!(fs::read_dir(&user).unwrap().all(|entry| {
			!entry
				.unwrap()
				.file_name()
				.to_string_lossy()
				.contains(".tmp.")
		}));
		assert_eq!(files.load("profile").unwrap().unwrap().as_str(), "ai_model @smol\n");
	}

	#[test]
	fn rejects_cfg_names_that_escape_the_profile_root() {
		let dir = tempfile::tempdir().unwrap();
		let files = CfgFiles::with_roots(dir.path().join(".o2"), None);
		assert!(matches!(files.load("../outside"), Err(ConError::InvalidCfgName { .. })));
		assert!(matches!(files.save("nested/config", ""), Err(ConError::InvalidCfgName { .. })));
	}

	#[test]
	fn generated_schema_migrates_and_future_schema_is_rejected() {
		let path = Path::new("config.cfg");
		let legacy = "// generated by omp-con dump; replay with `exec`\nunbindall\nai_model @smol\n";
		let migrated = migrate_config_script(path, legacy).unwrap();
		assert!(migrated.starts_with("// generated by omp-con dump; schema=1;"));
		assert!(!migrated.contains("unbindall"));
		let future = "// generated by omp-con dump; schema=999; replay with `exec`\n";
		assert!(matches!(
			migrate_config_script(path, future),
			Err(ConError::UnsupportedCfgSchema { found: 999, .. })
		));
		let malformed = "// generated by omp-con dump; schema=next; replay with `exec`\n";
		assert!(matches!(
			migrate_config_script(path, malformed),
			Err(ConError::InvalidCfgSchema { .. })
		));
	}

	#[test]
	fn filesystem_errors_keep_operation_path_and_source() {
		let dir = tempfile::tempdir().unwrap();
		let root = dir.path().join(".o2");
		fs::write(&root, "not a directory").unwrap();
		let files = CfgFiles::with_roots(root.clone(), None);
		assert!(matches!(
			files.save("config", ""),
			Err(ConError::ConfigIo(ConfigIoError {
				operation: ConfigOperation::Create,
				path,
				..
			})) if path == root
		));
	}

	#[test]
	fn stale_context_cannot_overwrite_a_concurrent_update() {
		let dir = tempfile::tempdir().unwrap();
		let root = dir.path().join(".o2");
		fs::create_dir_all(&root).unwrap();
		let path = root.join("config.cfg");
		fs::write(&path, "ai_model first\n").unwrap();
		let files = CfgFiles::with_roots(root, None);
		files.load("config").unwrap();
		fs::write(&path, "ai_model concurrent\n").unwrap();
		assert!(matches!(
			files.save("config", "ai_model stale\n"),
			Err(ConError::ConfigChanged { .. })
		));
		assert_eq!(fs::read_to_string(path).unwrap(), "ai_model concurrent\n");
	}

	#[test]
	fn malformed_config_reports_its_path_and_parse_source() {
		let dir = tempfile::tempdir().unwrap();
		let path = dir.path().join("config.cfg");
		fs::write(&path, "ai_model \"unterminated").unwrap();
		assert!(matches!(
			read_config(&path),
			Err(ConError::ConfigParse { path: failed, .. }) if failed == path
		));
	}

	#[test]
	fn locked_read_modify_write_preserves_concurrent_updates() {
		let dir = tempfile::tempdir().unwrap();
		let path = dir.path().join(".o2/config.cfg");
		let files = CfgFiles::with_roots(dir.path().join(".o2"), None);
		let workers = (0..8)
			.map(|index| {
				let files = files.clone();
				std::thread::spawn(move || {
					let transaction = ConfigFileLock::acquire(files.user_path("config")).unwrap();
					let mut text = transaction.read().unwrap().unwrap_or_default();
					text.push_str(&format!("line-{index}\n"));
					transaction.replace(&text).unwrap();
				})
			})
			.collect::<Vec<_>>();
		for worker in workers {
			worker.join().unwrap();
		}
		let text = fs::read_to_string(path).unwrap();
		for index in 0..8 {
			assert!(text.contains(&format!("line-{index}\n")));
		}
	}

	#[cfg(unix)]
	#[test]
	fn atomic_replace_preserves_a_dangling_config_symlink() {
		use std::os::unix::fs::symlink;

		let dir = tempfile::tempdir().unwrap();
		let root = dir.path().join(".o2");
		fs::create_dir_all(root.join("managed")).unwrap();
		symlink("managed/config.cfg", root.join("config.cfg")).unwrap();
		let files = CfgFiles::with_roots(root.clone(), None);
		files.save("config", "ai_model @smol\n").unwrap();
		assert!(
			fs::symlink_metadata(root.join("config.cfg"))
				.unwrap()
				.file_type()
				.is_symlink()
		);
		assert_eq!(fs::read_to_string(root.join("managed/config.cfg")).unwrap(), "ai_model @smol\n");
	}

	/// A repository's `.omp/*.cfg` is untrusted: an outside symlink, a FIFO,
	/// `/dev/zero`-like special files, and oversize files are refused with a
	/// typed error (and never hang or echo content), while a link that stays
	/// inside the project loads.
	#[cfg(unix)]
	#[test]
	fn project_cfg_reads_refuse_escapes_special_files_and_oversize() {
		use std::{os::unix::fs::symlink, process::Command, sync::mpsc, thread, time::Duration};

		use omp_core::project_file::{ProjectFileError, Refusal};

		let dir = tempfile::tempdir().unwrap();
		let project = dir.path().join("proj");
		let omp = project.join(".omp");
		fs::create_dir_all(&omp).unwrap();
		fs::write(dir.path().join("secret.cfg"), "ai_model TOP-SECRET\n").unwrap();
		symlink(dir.path().join("secret.cfg"), omp.join("escape.cfg")).unwrap();
		assert!(
			Command::new("mkfifo")
				.arg(omp.join("pipe.cfg"))
				.status()
				.unwrap()
				.success()
		);
		fs::write(omp.join("big.cfg"), vec![b'#'; usize::try_from(PROJECT_CFG_LIMIT).unwrap() + 1])
			.unwrap();
		fs::write(project.join("real.cfg"), "ai_thinking low\n").unwrap();
		symlink("../real.cfg", omp.join("inside.cfg")).unwrap();
		let files = CfgFiles::with_roots(dir.path().join("o2"), Some(omp));

		let (sender, receiver) = mpsc::channel();
		thread::spawn(move || {
			let refused = ["escape", "pipe", "big"].map(|name| match files.load_project(name) {
				Err(ConError::ProjectFile(ProjectFileError::Refused { reason, .. })) => reason,
				other => panic!("{name} must be refused, got {other:?}"),
			});
			let _ = sender.send((refused, files.load_project("inside").unwrap()));
		});
		let (refused, inside) = receiver
			.recv_timeout(Duration::from_secs(10))
			.expect("a special project cfg must be refused, not read");
		assert_eq!(refused, [Refusal::OutsideRoot, Refusal::NotRegular, Refusal::TooLarge]);
		assert_eq!(inside.unwrap().as_str(), "ai_thinking low\n");
	}

	/// The project write path (`omp config set --scope project`) refuses a
	/// symlink or non-regular target instead of writing through it, while a
	/// user cfg symlink (a dotfile manager) keeps working.
	#[cfg(unix)]
	#[test]
	fn project_cfg_writes_refuse_symlink_targets() {
		use std::os::unix::fs::symlink;

		use omp_core::project_file::Refusal;

		let dir = tempfile::tempdir().unwrap();
		let project = dir.path().join("proj");
		fs::create_dir_all(project.join(".omp")).unwrap();
		let victim = dir.path().join("victim");
		fs::write(&victim, "keep").unwrap();
		let target = project.join(".omp/config.cfg");
		symlink(&victim, &target).unwrap();

		let refused = ConfigFileLock::acquire_project(target.clone())
			.err()
			.expect("symlink refused");
		assert!(matches!(
			refused,
			ConError::ProjectFile(ref error) if error.refusal() == Some(Refusal::Symlink)
		));
		assert_eq!(
			fs::read_to_string(&victim).unwrap(),
			"keep",
			"nothing was written through the link"
		);

		fs::remove_file(&target).unwrap();
		fs::create_dir(&target).unwrap();
		let refused = ConfigFileLock::acquire_project(target.clone())
			.err()
			.expect("directory refused");
		assert!(matches!(
			refused,
			ConError::ProjectFile(ref error) if error.refusal() == Some(Refusal::NotRegular)
		));
		fs::remove_dir(&target).unwrap();

		let lock = ConfigFileLock::acquire_project(target.clone()).unwrap();
		lock.replace("ai_thinking low\n").unwrap();
		assert_eq!(fs::read_to_string(&target).unwrap(), "ai_thinking low\n");
		// Swapped for a link after the lock was taken, the write still refuses.
		fs::remove_file(&target).unwrap();
		symlink(&victim, &target).unwrap();
		assert!(matches!(lock.replace("ai_thinking high\n"), Err(ConError::ProjectFile(_))));
		assert_eq!(fs::read_to_string(&victim).unwrap(), "keep");
	}
}
