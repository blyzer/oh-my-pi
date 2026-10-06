//! Bounded, contained reads of files a project supplies.
//!
//! A repository's `AGENTS.md`, `.omp/*.cfg`, `.omp/hosts.toml` and similar are
//! content the user did not write. A plain `fs::read_to_string` follows a
//! symlink anywhere on disk, blocks on a FIFO, never ends on `/dev/zero`, and
//! has no size bound. [`read_bytes`] and [`read_text`] are the single reader
//! every such call site goes through: the target must be a regular file, its
//! canonical path must stay under the project root (a symlink that stays
//! inside is fine), and the read is one bounded `read` of at most `limit`
//! bytes. A refusal is a typed [`ProjectFileError`] naming the path and a
//! [`Refusal`]; it never carries file content.
//!
//! User-owned files (under the user's configuration root) are read with
//! [`Containment::Unconfined`]: the user may symlink their own dotfiles
//! anywhere, but the regular-file and size checks still apply.

use std::{
	fs::{self, File, OpenOptions},
	io::{self, Read as _},
	path::{Path, PathBuf},
};

use strum::{Display, IntoStaticStr};

/// Where a file's canonical path may resolve to.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Containment<'a> {
	/// The canonical target must lie under this directory, which is itself
	/// canonicalized before the comparison.
	Within(&'a Path),
	/// A user-owned file: any location is acceptable.
	Unconfined,
}

/// Why a project file was refused.
#[derive(Clone, Copy, Debug, Display, Eq, IntoStaticStr, PartialEq)]
pub enum Refusal {
	/// A directory, FIFO, socket, or device.
	#[strum(to_string = "refused: not a regular file")]
	NotRegular,
	/// The canonical path leaves the project root.
	#[strum(to_string = "refused: resolves outside the project root")]
	OutsideRoot,
	/// The file is larger than the call site's limit.
	#[strum(to_string = "refused: larger than the size limit")]
	TooLarge,
	/// The text is not valid UTF-8.
	#[strum(to_string = "refused: not valid UTF-8")]
	NotUtf8,
	/// A write target that is a symbolic link.
	#[strum(to_string = "refused: symbolic link")]
	Symlink,
}

/// A project file could not be read or written under the reader's rules.
#[derive(Debug, thiserror::Error)]
pub enum ProjectFileError {
	/// The file exists but the reader's rules refuse it.
	#[error("project file `{}` {reason}", path.display())]
	Refused {
		/// The path as the caller named it.
		path:   PathBuf,
		/// The rule that refused it.
		reason: Refusal,
	},
	/// A filesystem operation failed.
	#[error("failed to read project file `{}`", path.display())]
	Io {
		/// The path the operation was attempted on.
		path:   PathBuf,
		/// The filesystem failure.
		#[source]
		source: io::Error,
	},
}

impl ProjectFileError {
	/// The path the error is about.
	#[must_use]
	pub fn path(&self) -> &Path {
		match self {
			Self::Refused { path, .. } | Self::Io { path, .. } => path,
		}
	}

	/// The refusal rule, when the file was refused rather than unreadable.
	#[must_use]
	pub const fn refusal(&self) -> Option<Refusal> {
		match self {
			Self::Refused { reason, .. } => Some(*reason),
			Self::Io { .. } => None,
		}
	}
}

/// The directory project files must resolve inside: the repository root (the
/// nearest `.git` at or above `project_root`), else `project_root` itself.
#[must_use]
pub fn containment_root(project_root: &Path) -> &Path {
	project_root
		.ancestors()
		.find(|dir| dir.join(".git").exists())
		.unwrap_or(project_root)
}

fn absent(error: &io::Error) -> bool {
	matches!(error.kind(), io::ErrorKind::NotFound | io::ErrorKind::NotADirectory)
}

fn refuse(path: &Path, reason: Refusal) -> ProjectFileError {
	ProjectFileError::Refused { path: path.to_path_buf(), reason }
}

fn io_error(path: &Path, source: io::Error) -> ProjectFileError {
	ProjectFileError::Io { path: path.to_path_buf(), source }
}

fn canonical_root(root: &Path) -> Result<PathBuf, ProjectFileError> {
	fs::canonicalize(root).map_err(|source| io_error(root, source))
}

/// Opens without following a final symlink and without blocking on a FIFO, so
/// a file swapped for a special file after the type check still cannot hang
/// the open.
fn open_regular(path: &Path) -> io::Result<File> {
	let mut options = OpenOptions::new();
	options.read(true);
	#[cfg(unix)]
	{
		use std::os::unix::fs::OpenOptionsExt as _;
		options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
	}
	options.open(path)
}

/// Reads at most `limit` bytes of the regular file at `path`.
///
/// `Ok(None)` means the file is absent (including a dangling symlink).
///
/// # Errors
///
/// [`ProjectFileError::Refused`] when the canonical target escapes
/// `containment`, is not a regular file, or is larger than `limit`;
/// [`ProjectFileError::Io`] for any other filesystem failure.
pub fn read_bytes(
	path: &Path,
	containment: Containment<'_>,
	limit: u64,
) -> Result<Option<Vec<u8>>, ProjectFileError> {
	let canonical = match fs::canonicalize(path) {
		Ok(canonical) => canonical,
		Err(error) if absent(&error) => return Ok(None),
		Err(source) => return Err(io_error(path, source)),
	};
	if let Containment::Within(root) = containment
		&& !canonical.starts_with(canonical_root(root)?)
	{
		return Err(refuse(path, Refusal::OutsideRoot));
	}
	let kind = fs::symlink_metadata(&canonical).map_err(|source| io_error(path, source))?;
	if !kind.is_file() {
		return Err(refuse(path, Refusal::NotRegular));
	}
	let mut file = open_regular(&canonical).map_err(|source| io_error(path, source))?;
	let metadata = file.metadata().map_err(|source| io_error(path, source))?;
	if !metadata.is_file() {
		return Err(refuse(path, Refusal::NotRegular));
	}
	if metadata.len() > limit {
		return Err(refuse(path, Refusal::TooLarge));
	}
	let mut bytes = Vec::with_capacity(usize::try_from(metadata.len()).unwrap_or(0));
	(&mut file)
		.take(limit.saturating_add(1))
		.read_to_end(&mut bytes)
		.map_err(|source| io_error(path, source))?;
	if bytes.len() as u64 > limit {
		return Err(refuse(path, Refusal::TooLarge));
	}
	Ok(Some(bytes))
}

/// [`read_bytes`] for UTF-8 text.
///
/// # Errors
///
/// Those of [`read_bytes`], plus [`Refusal::NotUtf8`].
pub fn read_text(
	path: &Path,
	containment: Containment<'_>,
	limit: u64,
) -> Result<Option<String>, ProjectFileError> {
	read_bytes(path, containment, limit)?
		.map(|bytes| String::from_utf8(bytes).map_err(|_| refuse(path, Refusal::NotUtf8)))
		.transpose()
}

/// Refuses a write to `path` unless it is a plain, new-or-regular file whose
/// directory stays under `root`.
///
/// A project file the user did not author must never be written through a
/// symlink (the link target may be anywhere) or onto a special file.
///
/// # Errors
///
/// [`Refusal::Symlink`], [`Refusal::NotRegular`], or [`Refusal::OutsideRoot`]
/// (when the file's directory resolves outside `root`).
pub fn check_write_target(path: &Path, root: &Path) -> Result<(), ProjectFileError> {
	match fs::symlink_metadata(path) {
		Ok(metadata) if metadata.file_type().is_symlink() => {
			return Err(refuse(path, Refusal::Symlink));
		},
		Ok(metadata) if !metadata.is_file() => return Err(refuse(path, Refusal::NotRegular)),
		Ok(_) => {},
		Err(error) if absent(&error) => {},
		Err(source) => return Err(io_error(path, source)),
	}
	if let Some(parent) = path.parent()
		&& let Ok(parent) = fs::canonicalize(parent)
		&& !parent.starts_with(canonical_root(root)?)
	{
		return Err(refuse(path, Refusal::OutsideRoot));
	}
	Ok(())
}

#[cfg(all(test, unix))]
mod tests {
	use std::{os::unix::fs::symlink, process::Command, sync::mpsc, thread, time::Duration};

	use super::*;

	const LIMIT: u64 = 64;

	fn refusal(result: Result<Option<String>, ProjectFileError>) -> Refusal {
		result
			.expect_err("file must be refused")
			.refusal()
			.expect("refusal, not an I/O failure")
	}

	#[test]
	fn regular_file_and_absent_file() {
		let dir = tempfile::tempdir().unwrap();
		fs::write(dir.path().join("a.md"), "hello").unwrap();
		let within = Containment::Within(dir.path());
		let text = read_text(&dir.path().join("a.md"), within, LIMIT).unwrap();
		assert_eq!(text.as_deref(), Some("hello"));
		assert_eq!(read_text(&dir.path().join("missing"), within, LIMIT).unwrap(), None);
		symlink(dir.path().join("gone"), dir.path().join("dangling")).unwrap();
		assert_eq!(read_text(&dir.path().join("dangling"), within, LIMIT).unwrap(), None);
	}

	#[test]
	fn symlink_outside_root_is_refused_and_inside_is_accepted() {
		let scratch = tempfile::tempdir().unwrap();
		let root = scratch.path().join("repo");
		fs::create_dir_all(root.join("docs")).unwrap();
		fs::write(scratch.path().join("secret"), "token").unwrap();
		fs::write(root.join("docs/real.md"), "inside").unwrap();
		symlink(scratch.path().join("secret"), root.join("out.md")).unwrap();
		symlink("docs/real.md", root.join("in.md")).unwrap();
		let within = Containment::Within(&root);
		assert_eq!(refusal(read_text(&root.join("out.md"), within, LIMIT)), Refusal::OutsideRoot);
		let inside = read_text(&root.join("in.md"), within, LIMIT).unwrap();
		assert_eq!(inside.as_deref(), Some("inside"));
		// A user-owned file may live anywhere.
		let outside = read_text(&root.join("out.md"), Containment::Unconfined, LIMIT).unwrap();
		assert_eq!(outside.as_deref(), Some("token"));
	}

	#[test]
	fn special_files_and_oversize_files_are_refused_without_blocking() {
		let dir = tempfile::tempdir().unwrap();
		let fifo = dir.path().join("pipe");
		assert!(
			Command::new("mkfifo")
				.arg(&fifo)
				.status()
				.unwrap()
				.success()
		);
		fs::create_dir(dir.path().join("dir")).unwrap();
		fs::write(dir.path().join("big"), vec![b'x'; 65]).unwrap();
		let (sender, receiver) = mpsc::channel();
		let root = dir.path().to_path_buf();
		thread::spawn(move || {
			let within = Containment::Within(&root);
			let reasons = [
				refusal(read_text(&root.join("pipe"), within, LIMIT)),
				refusal(read_text(&root.join("dir"), within, LIMIT)),
				refusal(read_text(Path::new("/dev/zero"), Containment::Unconfined, LIMIT)),
				refusal(read_text(&root.join("big"), within, LIMIT)),
			];
			let _ = sender.send(reasons);
		});
		let reasons = receiver
			.recv_timeout(Duration::from_secs(10))
			.expect("a special file must be refused, not read");
		assert_eq!(reasons, [
			Refusal::NotRegular,
			Refusal::NotRegular,
			Refusal::NotRegular,
			Refusal::TooLarge
		]);
	}

	#[test]
	fn invalid_utf8_is_refused_and_write_targets_are_checked() {
		let scratch = tempfile::tempdir().unwrap();
		let root = scratch.path().join("repo");
		fs::create_dir_all(root.join(".omp")).unwrap();
		fs::write(root.join("bin"), [0xff, 0xfe]).unwrap();
		let binary = read_text(&root.join("bin"), Containment::Within(&root), LIMIT);
		assert_eq!(refusal(binary), Refusal::NotUtf8);

		let target = root.join(".omp/config.cfg");
		assert!(check_write_target(&target, &root).is_ok(), "an absent file is writable");
		fs::write(&target, "x").unwrap();
		assert!(check_write_target(&target, &root).is_ok(), "a regular file is writable");
		fs::remove_file(&target).unwrap();
		fs::write(scratch.path().join("victim"), "x").unwrap();
		symlink(scratch.path().join("victim"), &target).unwrap();
		let linked = check_write_target(&target, &root).unwrap_err();
		assert_eq!(linked.refusal(), Some(Refusal::Symlink));
		fs::remove_file(&target).unwrap();
		fs::remove_dir_all(root.join(".omp")).unwrap();
		symlink(scratch.path(), root.join(".omp")).unwrap();
		let escaped = check_write_target(&target, &root).unwrap_err();
		assert_eq!(escaped.refusal(), Some(Refusal::OutsideRoot));
	}

	#[test]
	fn refusal_text_is_static_and_names_no_content() {
		let text: &'static str = Refusal::OutsideRoot.into();
		assert_eq!(text, "refused: resolves outside the project root");
		let error = refuse(Path::new("/r/AGENTS.md"), Refusal::TooLarge);
		assert_eq!(
			error.to_string(),
			"project file `/r/AGENTS.md` refused: larger than the size limit"
		);
	}
}
