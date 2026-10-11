//! Deterministic language-server command planning within Environment-owned
//! path and process authority.

use std::{
	env,
	ffi::OsStr,
	path::{Path, PathBuf},
};

use omp_core::Str;
use thiserror::Error;

const PYTHON_MARKERS: [&str; 9] = [
	"pyproject.toml",
	"ty.toml",
	"requirements.txt",
	"setup.py",
	"setup.cfg",
	"Pipfile",
	"pyrightconfig.json",
	"ruff.toml",
	".ruff.toml",
];
const WINDOWS_SUFFIXES: [&str; 3] = [".exe", ".cmd", ".bat"];

/// Platform used for suffix-aware executable lookup.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BinaryPlatform {
	/// POSIX-style executable names.
	Posix,
	/// Windows launchers and executable suffixes.
	Windows,
}

/// A resolved command ready for an Environment process launcher.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedLspBinary {
	/// Absolute executable path.
	pub executable: PathBuf,
	/// Arguments after exact `$PID` substitution.
	pub args:       Vec<Str>,
}

/// Resolves `command` to an executable. A bare name is looked up in the
/// project-local bins under `project_root` (`node_modules/.bin`, `.venv/bin`,
/// `bin`, ..., each behind its marker file) and then in the supplied `PATH`;
/// `None` searches `PATH` only. Callers pass a project root only when the user
/// enabled `sv_lsp_project_binaries`. This function probes paths only; the
/// caller remains the owner of process creation.
pub fn resolve_lsp_binary(
	command: &str,
	args: &[Str],
	project_root: Option<&Path>,
	path: Option<&OsStr>,
	pid: u32,
	platform: BinaryPlatform,
) -> Result<ResolvedLspBinary, LspBinaryError> {
	if command.is_empty() {
		return Err(LspBinaryError::EmptyCommand);
	}
	let executable = resolve_executable(command, project_root, path, platform)
		.ok_or_else(|| LspBinaryError::Unavailable { command: Str::new(command) })?;
	let pid_text = pid.to_string();
	let args = args
		.iter()
		.map(|arg| {
			if arg == "$PID" {
				Str::new(&pid_text)
			} else {
				arg.clone()
			}
		})
		.collect();
	Ok(ResolvedLspBinary { executable, args })
}

/// Resolves a language- or debug-server `command` to an executable path: a
/// path-like command as given, a bare name from the project-local bins under
/// `project_root` (when supplied), then from `path`.
pub(crate) fn resolve_executable(
	command: &str,
	project_root: Option<&Path>,
	path: Option<&OsStr>,
	platform: BinaryPlatform,
) -> Option<PathBuf> {
	if Path::new(command).components().count() > 1 {
		return resolve_candidate(Path::new(command), platform);
	}
	project_root
		.and_then(|root| resolve_local(root, command, platform))
		.or_else(|| {
			path.and_then(|path| {
				env::split_paths(path)
					.find_map(|directory| resolve_candidate(&directory.join(command), platform))
			})
		})
}

fn resolve_local(root: &Path, command: &str, platform: BinaryPlatform) -> Option<PathBuf> {
	let declarations: [(&[&str], &str); 10] = [
		(&["package.json", "package-lock.json", "yarn.lock", "pnpm-lock.yaml"], "node_modules/.bin"),
		(&PYTHON_MARKERS, ".venv/bin"),
		(&PYTHON_MARKERS, ".venv/Scripts"),
		(&PYTHON_MARKERS, "venv/bin"),
		(&PYTHON_MARKERS, "venv/Scripts"),
		(&PYTHON_MARKERS, ".env/bin"),
		(&PYTHON_MARKERS, ".env/Scripts"),
		(&["Gemfile", "Gemfile.lock"], "vendor/bundle/bin"),
		(&["Gemfile", "Gemfile.lock"], "bin"),
		(&["go.mod", "go.sum", "go.work"], "bin"),
	];
	declarations.iter().find_map(|(markers, bin)| {
		markers
			.iter()
			.any(|marker| root.join(marker).exists())
			.then(|| resolve_candidate(&root.join(bin).join(command), platform))
			.flatten()
	})
}

fn resolve_candidate(base: &Path, platform: BinaryPlatform) -> Option<PathBuf> {
	if base.is_file() {
		return Some(base.to_owned());
	}
	if platform == BinaryPlatform::Windows && base.extension().is_none() {
		return WINDOWS_SUFFIXES
			.iter()
			.map(|suffix| PathBuf::from(format!("{}{suffix}", base.display())))
			.find(|candidate| candidate.is_file());
	}
	None
}

/// Language-server executable lookup failure.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum LspBinaryError {
	/// The command field was empty.
	#[error("LSP command must not be empty")]
	EmptyCommand,
	/// No permitted local or PATH candidate exists.
	#[error("language-server executable {command} is unavailable")]
	Unavailable {
		/// Unresolved command.
		command: Str,
	},
}

#[cfg(test)]
mod tests {
	use std::fs;

	use super::*;

	/// A `PATH` directory holding `command`, and a project root whose
	/// `node_modules/.bin` (behind `package.json`) holds it too.
	fn path_and_local(command: &str) -> (tempfile::TempDir, PathBuf, tempfile::TempDir, PathBuf) {
		let root = tempfile::tempdir().unwrap();
		fs::write(root.path().join("package.json"), b"{}").unwrap();
		fs::create_dir_all(root.path().join("node_modules/.bin")).unwrap();
		let local = root.path().join("node_modules/.bin").join(command);
		fs::write(&local, b"").unwrap();
		let bin = tempfile::tempdir().unwrap();
		let on_path = bin.path().join(command);
		fs::write(&on_path, b"").unwrap();
		(root, local, bin, on_path)
	}

	#[test]
	fn without_a_project_root_the_path_binary_wins() {
		let (root, local, bin, on_path) = path_and_local("omnisharp");
		let resolved = resolve_lsp_binary(
			"omnisharp",
			&[],
			None,
			Some(bin.path().as_os_str()),
			42,
			BinaryPlatform::Posix,
		)
		.unwrap();
		assert_eq!(resolved.executable, on_path);
		assert_ne!(resolved.executable, local);
		// The project bin alone is never found without the opt-in.
		assert_eq!(
			resolve_lsp_binary("omnisharp", &[], None, None, 42, BinaryPlatform::Posix),
			Err(LspBinaryError::Unavailable { command: Str::new_static("omnisharp") })
		);
		drop(root);
	}

	#[test]
	fn opted_in_local_node_binary_precedes_path_and_pid_is_substituted() {
		let (root, local, bin, _) = path_and_local("omnisharp");
		let resolved = resolve_lsp_binary(
			"omnisharp",
			&[Str::new_static("--hostPID"), Str::new_static("$PID")],
			Some(root.path()),
			Some(bin.path().as_os_str()),
			42,
			BinaryPlatform::Posix,
		)
		.unwrap();
		assert_eq!(resolved.executable, local);
		assert_eq!(resolved.args[1], "42");
	}

	#[test]
	fn windows_suffixes_are_considered_for_local_launchers() {
		let root = tempfile::tempdir().unwrap();
		fs::write(root.path().join("package.json"), b"{}").unwrap();
		fs::create_dir_all(root.path().join("node_modules/.bin")).unwrap();
		let local = root
			.path()
			.join("node_modules/.bin/typescript-language-server.cmd");
		fs::write(&local, b"").unwrap();
		let resolved = resolve_lsp_binary(
			"typescript-language-server",
			&[],
			Some(root.path()),
			None,
			1,
			BinaryPlatform::Windows,
		)
		.unwrap();
		assert_eq!(resolved.executable, local);
	}
}
