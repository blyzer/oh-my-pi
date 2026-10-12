//! File-or-inline prompt customization resolution.

use std::{
	fs, io,
	path::{Path, PathBuf},
};

use omp_core::{
	Str,
	dirs::ProfileNameError,
	project_file::{self, Containment, ProjectFileError, containment_root},
};
use omp_ext::workspace_trust::{
	WorkspaceTrust,
	inventory::{PROJECT_DIR, SYSTEM_PROMPT_FILE, TITLE_SYSTEM_PROMPT_FILE},
};
use thiserror::Error;

use crate::discovery::rules::CONTEXT_FILE_LIMIT;

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
	/// A user prompt file is not a regular file or exceeds the size limit.
	#[error("refused prompt input")]
	Refused(#[source] ProjectFileError),
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
/// The project candidate is `<cwd>/.omp/<name>`, read only while the
/// workspace is [`WorkspaceTrust::Trusted`] and then through the contained
/// project-file reader ([`omp_core::project_file`]): a symlink out of the
/// repository, a special file, or one over [`CONTEXT_FILE_LIMIT`] is skipped
/// with a warning and discovery falls through to the user candidate, which
/// lives in the selected profile's agent asset tree ([`user_prompt_path`]).
/// An untrusted workspace's candidate is withheld; the user candidate is never
/// gated.
pub fn discover_prompt_file(
	cwd: &Path,
	home: &Path,
	name: &str,
	trust: WorkspaceTrust,
) -> Result<Option<Str>, PromptInputError> {
	if trust == WorkspaceTrust::Trusted {
		let project = cwd.join(PROJECT_DIR).join(name);
		match project_file::read_text(
			&project,
			Containment::Within(containment_root(cwd)),
			CONTEXT_FILE_LIMIT,
		) {
			Ok(Some(content)) => return Ok(Some(content.into())),
			Ok(None) => {},
			Err(ProjectFileError::Refused { path, reason }) => {
				tracing::warn!(path = %path.display(), reason = <&str>::from(reason), "project prompt file skipped");
			},
			Err(ProjectFileError::Io { source, .. }) if tolerant_literal_error(&source) => {},
			Err(ProjectFileError::Io { path, source }) => {
				return Err(PromptInputError::Read { path, source });
			},
		}
	}
	read_user_prompt(&user_prompt_path(home, name)?)
}

/// Reads a user prompt file. Its owner may symlink it anywhere, but a FIFO,
/// device, or file over [`CONTEXT_FILE_LIMIT`] is refused rather than read.
fn read_user_prompt(path: &Path) -> Result<Option<Str>, PromptInputError> {
	match project_file::read_text(path, Containment::Unconfined, CONTEXT_FILE_LIMIT) {
		Ok(content) => Ok(content.map(Str::from)),
		Err(ProjectFileError::Io { source, .. }) if tolerant_literal_error(&source) => Ok(None),
		Err(ProjectFileError::Io { path, source }) => Err(PromptInputError::Read { path, source }),
		Err(error @ ProjectFileError::Refused { .. }) => Err(PromptInputError::Refused(error)),
	}
}

/// Discovers one prompt only in the native user configuration root.
pub fn discover_user_prompt_file(
	cwd: &Path,
	home: &Path,
	name: &str,
) -> Result<Option<Str>, PromptInputError> {
	let _ = cwd;
	Ok(read_user_prompt(&user_prompt_path(home, name)?)?
		.filter(|content| !content.trim().is_empty()))
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
/// resolves append guidance independently. The project `SYSTEM.md` counts
/// only under `trust` [`WorkspaceTrust::Trusted`].
pub fn resolve_system_inputs(
	cwd: &Path,
	home: &Path,
	custom: Option<&str>,
	append: Option<&str>,
	trust: WorkspaceTrust,
) -> Result<(Option<Str>, Option<Str>), PromptInputError> {
	let custom = match resolve_prompt_input(custom)? {
		Some(custom) => Some(custom),
		None => discover_prompt_file(cwd, home, SYSTEM_PROMPT_FILE, trust)?,
	};
	let append = resolve_prompt_input(append)?;
	Ok((custom, append))
}
/// Resolves the title-generation system prompt with project-over-user
/// `TITLE_SYSTEM.md` precedence (the project's only under `trust`
/// [`WorkspaceTrust::Trusted`]) and the embedded native prompt as fallback.
pub fn resolve_title_system_prompt(
	cwd: &Path,
	home: &Path,
	trust: WorkspaceTrust,
) -> Result<Str, PromptInputError> {
	Ok(discover_prompt_file(cwd, home, TITLE_SYSTEM_PROMPT_FILE, trust)?
		.unwrap_or_else(|| Str::new_static("Generate a concise title for this coding session.")))
}

fn tolerant_literal_error(error: &io::Error) -> bool {
	error.kind() == io::ErrorKind::NotFound || matches!(error.raw_os_error(), Some(36 | 63))
}

#[cfg(test)]
mod tests {
	use std::fs;

	use super::*;

	const TRUSTED: WorkspaceTrust = WorkspaceTrust::Trusted;

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
			discover_prompt_file(&project, &home, "SYSTEM.md", TRUSTED)
				.expect("system discovery")
				.as_deref(),
			Some("project")
		);
		assert_eq!(
			resolve_title_system_prompt(&project, &home, TRUSTED)
				.expect("embedded title prompt fallback")
				.as_str(),
			"Generate a concise title for this coding session."
		);

		fs::write(project.join(".omp/TITLE_SYSTEM.md"), "project title")
			.expect("project title system prompt");
		assert_eq!(
			resolve_title_system_prompt(&project, &home, TRUSTED)
				.expect("project title prompt")
				.as_str(),
			"project title"
		);
	}

	/// An untrusted workspace's `.omp/SYSTEM.md`, `APPEND_SYSTEM.md` and
	/// `TITLE_SYSTEM.md` are withheld; the user's own prompts still apply.
	#[test]
	fn an_untrusted_workspace_prompt_is_withheld_and_the_user_prompt_applies() {
		let scratch = tempfile::tempdir().expect("scratch directory");
		let home = scratch.path().join("home");
		let project = scratch.path().join("repo");
		fs::create_dir_all(project.join(".omp")).expect("project config directory");
		for name in [SYSTEM_PROMPT_FILE, "APPEND_SYSTEM.md", TITLE_SYSTEM_PROMPT_FILE] {
			fs::write(project.join(".omp").join(name), "project").expect("project prompt");
		}
		let untrusted = WorkspaceTrust::Untrusted;
		for name in [SYSTEM_PROMPT_FILE, "APPEND_SYSTEM.md", TITLE_SYSTEM_PROMPT_FILE] {
			assert_eq!(
				discover_prompt_file(&project, &home, name, untrusted).expect("discovery"),
				None,
				"{name}"
			);
		}
		assert_eq!(
			resolve_system_inputs(&project, &home, None, None, untrusted).expect("inputs"),
			(None, None)
		);
		assert_eq!(
			resolve_title_system_prompt(&project, &home, untrusted)
				.expect("title")
				.as_str(),
			"Generate a concise title for this coding session."
		);

		let user_system = user_prompt_path(&home, SYSTEM_PROMPT_FILE).expect("user prompt path");
		fs::create_dir_all(user_system.parent().expect("agent dir")).expect("user agent directory");
		fs::write(&user_system, "user").expect("user system prompt");
		assert_eq!(
			discover_prompt_file(&project, &home, SYSTEM_PROMPT_FILE, untrusted)
				.expect("discovery")
				.as_deref(),
			Some("user")
		);
		assert_eq!(
			discover_prompt_file(&project, &home, SYSTEM_PROMPT_FILE, TRUSTED)
				.expect("discovery")
				.as_deref(),
			Some("project")
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
		assert_eq!(
			discover_prompt_file(&project, &home, "SYSTEM.md", TRUSTED).expect("discovery"),
			None
		);
		assert_eq!(
			discover_prompt_file(&project, &home, "APPEND_SYSTEM.md", TRUSTED).expect("discovery"),
			None
		);

		let user_append = user_prompt_path(&home, "APPEND_SYSTEM.md").expect("user prompt path");
		assert!(user_append.ends_with("agent/APPEND_SYSTEM.md"));
		fs::create_dir_all(user_append.parent().expect("agent dir")).expect("user agent dir");
		fs::write(&user_append, "user append").expect("user append prompt");
		assert_eq!(
			discover_prompt_file(&project, &home, "APPEND_SYSTEM.md", TRUSTED)
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

	/// A repository's `.omp/SYSTEM.md` that is a symlink out of the repository
	/// or an oversize file is skipped (user guidance still applies); a link
	/// that stays inside the repository is read.
	#[cfg(unix)]
	#[test]
	fn project_system_prompt_must_stay_inside_the_repository_and_under_the_limit() {
		use std::os::unix::fs::symlink;

		let scratch = tempfile::tempdir().expect("scratch directory");
		let home = scratch.path().join("home");
		let project = scratch.path().join("repo");
		let user_system = user_prompt_path(&home, "SYSTEM.md").expect("user prompt path");
		fs::create_dir_all(user_system.parent().expect("agent dir")).expect("user agent directory");
		fs::create_dir_all(project.join(".omp")).expect("project config directory");
		fs::create_dir_all(project.join(".git")).expect("repository marker");
		fs::write(&user_system, "user").expect("user system prompt");
		fs::write(scratch.path().join("secret"), "TOP-SECRET").expect("outside file");
		let system = project.join(".omp/SYSTEM.md");

		symlink(scratch.path().join("secret"), &system).expect("outside link");
		let found = discover_prompt_file(&project, &home, "SYSTEM.md", TRUSTED).expect("discovery");
		assert_eq!(
			found.as_deref(),
			Some("user"),
			"an outside symlink falls through to the user prompt"
		);
		fs::remove_file(&system).expect("remove link");

		fs::write(&system, vec![b'x'; usize::try_from(CONTEXT_FILE_LIMIT).unwrap() + 1])
			.expect("oversize prompt");
		let found = discover_prompt_file(&project, &home, "SYSTEM.md", TRUSTED).expect("discovery");
		assert_eq!(found.as_deref(), Some("user"), "an oversize prompt is skipped");
		fs::remove_file(&system).expect("remove prompt");

		fs::write(project.join("prompt.md"), "inside").expect("inside file");
		symlink("../prompt.md", &system).expect("inside link");
		let found = discover_prompt_file(&project, &home, "SYSTEM.md", TRUSTED).expect("discovery");
		assert_eq!(found.as_deref(), Some("inside"));
	}

	/// The user `SYSTEM.md` may be the owner's symlink, but a FIFO there is
	/// refused rather than blocking session startup.
	#[cfg(unix)]
	#[test]
	fn user_system_prompt_follows_links_but_refuses_a_fifo() {
		use std::{os::unix::fs::symlink, process::Command, sync::mpsc, thread, time::Duration};

		let scratch = tempfile::tempdir().expect("scratch directory");
		let home = scratch.path().join("home");
		let project = scratch.path().join("repo");
		fs::create_dir_all(&project).expect("project");
		let user_system = user_prompt_path(&home, "SYSTEM.md").expect("user prompt path");
		fs::create_dir_all(user_system.parent().expect("agent dir")).expect("user agent directory");
		fs::write(scratch.path().join("dotfiles.md"), "linked").expect("dotfile");
		symlink(scratch.path().join("dotfiles.md"), &user_system).expect("user link");
		assert_eq!(
			discover_prompt_file(&project, &home, "SYSTEM.md", TRUSTED)
				.expect("discovery")
				.as_deref(),
			Some("linked")
		);
		fs::remove_file(&user_system).expect("remove link");

		assert!(
			Command::new("mkfifo")
				.arg(&user_system)
				.status()
				.expect("mkfifo")
				.success()
		);
		let (sender, receiver) = mpsc::channel();
		thread::spawn(move || {
			let _ = sender.send(discover_prompt_file(&project, &home, "SYSTEM.md", TRUSTED).is_err());
		});
		assert!(
			receiver
				.recv_timeout(Duration::from_secs(10))
				.expect("a FIFO user prompt must be refused, not read"),
			"a FIFO user prompt is an error"
		);
	}
}
