//! File-or-inline prompt customization resolution.

use std::{
	fs, io,
	path::{Path, PathBuf},
};

use omp_core::{Str, dirs::ProfileNameError};
use thiserror::Error;

/// A prompt customization file could not be read.
#[derive(Debug, Error)]
pub enum PromptInputError {
	/// Reading an existing candidate failed for a reason other than absence or
	/// an overlong path.
	#[error("failed to read prompt input {path}")]
	Read {
		/// Candidate path.
		path:   PathBuf,
		/// Filesystem failure.
		#[source]
		source: io::Error,
	},
	/// The selected profile could not be resolved to a configuration root.
	#[error("failed to resolve the user configuration root")]
	Profile(#[from] ProfileNameError),
}

/// Resolves a value as inline text or a readable file.
///
/// Values containing a newline are always inline. Missing and overlong paths
/// are treated as literal text at the command boundary.
pub fn resolve_prompt_input(input: Option<&str>) -> Result<Option<Str>, PromptInputError> {
	let Some(input) = input else {
		return Ok(None);
	};
	if input.contains('\n') {
		return Ok(Some(Str::new(input)));
	}
	match fs::read_to_string(input) {
		Ok(content) => Ok(Some(content.into())),
		Err(source) if tolerant_literal_error(&source) => Ok(Some(Str::new(input))),
		Err(source) => Err(PromptInputError::Read { path: input.into(), source }),
	}
}

/// Discovers one native Markdown prompt with project-over-user precedence.
///
/// The project candidate is `<cwd>/.omp/<name>`; the user candidate lives in
/// the selected profile's agent asset tree ([`user_prompt_path`]).
pub fn discover_prompt_file(
	cwd: &Path,
	home: &Path,
	name: &str,
) -> Result<Option<Str>, PromptInputError> {
	for path in [cwd.join(".omp").join(name), user_prompt_path(home, name)?] {
		match fs::read_to_string(&path) {
			Ok(content) => return Ok(Some(content.into())),
			Err(source) if tolerant_literal_error(&source) => {},
			Err(source) => return Err(PromptInputError::Read { path, source }),
		}
	}
	Ok(None)
}

/// Discovers one prompt only in the native user configuration root.
pub fn discover_user_prompt_file(
	cwd: &Path,
	home: &Path,
	name: &str,
) -> Result<Option<Str>, PromptInputError> {
	let _ = cwd;
	let path = user_prompt_path(home, name)?;
	match fs::read_to_string(&path) {
		Ok(content) if !content.trim().is_empty() => Ok(Some(content.into())),
		Ok(_) => Ok(None),
		Err(source) if tolerant_literal_error(&source) => Ok(None),
		Err(source) => Err(PromptInputError::Read { path, source }),
	}
}

/// Path of a user prompt file: `<profile config root>/agent/<name>`, i.e.
/// `~/.o2/agent/<name>` (or `~/.o2/profiles/<p>/agent/<name>`), beside the
/// user `AGENTS.md`, `RULES.md` and `prompts/`.
///
/// # Errors
///
/// Returns [`PromptInputError::Profile`] when the selected profile is invalid.
pub fn user_prompt_path(home: &Path, name: &str) -> Result<PathBuf, PromptInputError> {
	Ok(omp_core::dirs::profile_config_dir(home)?
		.join("agent")
		.join(name))
}

/// Resolves CLI customization ahead of project/user `SYSTEM.md` discovery and
/// resolves append guidance independently.
pub fn resolve_system_inputs(
	cwd: &Path,
	home: &Path,
	custom: Option<&str>,
	append: Option<&str>,
) -> Result<(Option<Str>, Option<Str>), PromptInputError> {
	let custom = match resolve_prompt_input(custom)? {
		Some(custom) => Some(custom),
		None => discover_prompt_file(cwd, home, "SYSTEM.md")?,
	};
	let append = resolve_prompt_input(append)?;
	Ok((custom, append))
}
/// Resolves the title-generation system prompt with project-over-user
/// `TITLE_SYSTEM.md` precedence and the embedded native prompt as fallback.
pub fn resolve_title_system_prompt(cwd: &Path, home: &Path) -> Result<Str, PromptInputError> {
	Ok(discover_prompt_file(cwd, home, "TITLE_SYSTEM.md")?
		.unwrap_or_else(|| Str::new_static("Generate a concise title for this coding session.")))
}

fn tolerant_literal_error(error: &io::Error) -> bool {
	error.kind() == io::ErrorKind::NotFound || matches!(error.raw_os_error(), Some(36 | 63))
}

#[cfg(test)]
mod tests {
	use std::fs;

	use super::*;

	#[test]
	fn missing_path_is_literal_and_project_system_wins() {
		let scratch = tempfile::tempdir().expect("scratch directory");
		let home = scratch.path().join("home");
		let project = scratch.path().join("repo");
		let user_system = user_prompt_path(&home, "SYSTEM.md").expect("user prompt path");
		fs::create_dir_all(user_system.parent().expect("agent dir")).expect("user agent directory");
		fs::create_dir_all(project.join(".omp")).expect("project config directory");
		fs::write(&user_system, "user").expect("user system prompt");
		fs::write(project.join(".omp/SYSTEM.md"), "project").expect("project system prompt");

		assert_eq!(
			resolve_prompt_input(Some("not-a-real-prompt-file"))
				.expect("literal fallback")
				.as_deref(),
			Some("not-a-real-prompt-file")
		);
		assert_eq!(
			discover_prompt_file(&project, &home, "SYSTEM.md")
				.expect("system discovery")
				.as_deref(),
			Some("project")
		);
		assert_eq!(
			resolve_title_system_prompt(&project, &home)
				.expect("embedded title prompt fallback")
				.as_str(),
			"Generate a concise title for this coding session."
		);

		fs::write(project.join(".omp/TITLE_SYSTEM.md"), "project title")
			.expect("project title system prompt");
		assert_eq!(
			resolve_title_system_prompt(&project, &home)
				.expect("project title prompt")
				.as_str(),
			"project title"
		);
	}

	#[test]
	fn user_prompts_live_in_the_agent_asset_tree_not_home_dot_omp() {
		let scratch = tempfile::tempdir().expect("scratch directory");
		let home = scratch.path().join("home");
		let project = scratch.path().join("repo");
		fs::create_dir_all(&project).expect("project directory");

		// v1 kept user prompts under `~/.omp/agent/`; `~/.omp/` itself was
		// never a prompt root. Neither is read by v2.
		fs::create_dir_all(home.join(".omp/agent")).expect("v1 agent directory");
		fs::write(home.join(".omp/SYSTEM.md"), "stale").expect("stray prompt");
		fs::write(home.join(".omp/agent/APPEND_SYSTEM.md"), "v1").expect("v1 prompt");
		assert_eq!(discover_prompt_file(&project, &home, "SYSTEM.md").expect("discovery"), None);
		assert_eq!(
			discover_prompt_file(&project, &home, "APPEND_SYSTEM.md").expect("discovery"),
			None
		);

		let user_append = user_prompt_path(&home, "APPEND_SYSTEM.md").expect("user prompt path");
		assert!(user_append.ends_with("agent/APPEND_SYSTEM.md"));
		fs::create_dir_all(user_append.parent().expect("agent dir")).expect("user agent dir");
		fs::write(&user_append, "user append").expect("user append prompt");
		assert_eq!(
			discover_prompt_file(&project, &home, "APPEND_SYSTEM.md")
				.expect("discovery")
				.as_deref(),
			Some("user append")
		);
		assert_eq!(
			discover_user_prompt_file(&project, &home, "APPEND_SYSTEM.md")
				.expect("user discovery")
				.as_deref(),
			Some("user append")
		);
	}
}
