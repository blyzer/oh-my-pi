use std::{
	path::{Path, PathBuf},
	str::FromStr as _,
};

use omp_core::{
	Str,
	project_file::{self, Containment, ProjectFileError, containment_root},
};
use omp_secrets::rule::{SecretKind, SecretMode, SecretRule, SecretRuleError};
use serde::Deserialize;
use thiserror::Error;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct RawRule {
	#[serde(rename = "type")]
	kind:          String,
	content:       String,
	#[serde(default)]
	mode:          Option<String>,
	#[serde(default)]
	replacement:   Option<String>,
	#[serde(default)]
	flags:         Option<String>,
	#[serde(default)]
	friendly_name: Option<String>,
}

/// Failure to load or validate a secret configuration file.
#[derive(Debug, Error)]
pub enum SecretConfigError {
	/// A present configuration file was refused or could not be read.
	#[error(transparent)]
	File(#[from] ProjectFileError),
	/// YAML syntax or shape is invalid.
	#[error("invalid secret configuration YAML in `{path}`")]
	Yaml {
		/// Configuration path.
		path:   PathBuf,
		/// YAML decoding failure.
		#[source]
		source: serde_yaml::Error,
	},
	/// An entry contains an unknown kind.
	#[error("secret configuration `{path}` entry {index} has unknown kind `{value}`")]
	Kind {
		/// Configuration file containing the invalid declaration.
		path:  PathBuf,
		/// Zero-based position of the declaration in the YAML sequence.
		index: usize,
		/// Unsupported rule-kind token, not the configured secret content.
		value: Str,
	},
	/// An entry contains an unknown mode.
	#[error("secret configuration `{path}` entry {index} has unknown mode `{value}`")]
	Mode {
		/// Configuration file containing the invalid declaration.
		path:  PathBuf,
		/// Zero-based position of the declaration in the YAML sequence.
		index: usize,
		/// Unsupported masking-mode token, not the configured secret content.
		value: Str,
	},
	/// Core rule validation failed.
	#[error("secret configuration `{path}` entry {index} is invalid")]
	Rule {
		/// Configuration file containing the rejected rule.
		path:   PathBuf,
		/// Zero-based position of the rule in the YAML sequence.
		index:  usize,
		/// Validation failure whose public display omits configured secret
		/// content.
		#[source]
		source: SecretRuleError,
	},
}

/// Largest `secrets.yml` the loader reads: a rule list, never a document.
pub const SECRETS_FILE_LIMIT: u64 = 256 * 1024;

/// Loads global rules first and project rules second, with project declarations
/// overriding global declarations that have identical content.
/// Loads the native global and project-local `secrets.yml` files.
pub fn load_for_project(
	project_root: &Path,
	agent_dir: &Path,
) -> Result<Vec<SecretRule>, SecretConfigError> {
	load_secret_rules(
		&agent_dir.join("secrets.yml"),
		&project_root.join(".omp").join("secrets.yml"),
		project_root,
	)
}

/// Loads explicit global and project files, with project declarations
/// overriding global declarations that have identical content.
///
/// The project file is read through the contained project-file reader against
/// `project_root`; a refused one is an error rather than a skipped file,
/// because dropping a project's masking rules would let its secrets through.
pub fn load_secret_rules(
	global: &Path,
	project: &Path,
	project_root: &Path,
) -> Result<Vec<SecretRule>, SecretConfigError> {
	let mut global_rules = load_secret_file(global)?;
	let project_rules = load_file(project, Containment::Within(containment_root(project_root)))?;
	if project_rules.is_empty() {
		return Ok(global_rules);
	}
	global_rules.retain(|rule| {
		!project_rules
			.iter()
			.any(|project| project.content() == rule.content())
	});
	global_rules.extend(project_rules);
	Ok(global_rules)
}

/// Loads one user-owned `secrets.yml` (a missing file is empty): no
/// containment, but still a bounded regular file.
pub fn load_secret_file(path: &Path) -> Result<Vec<SecretRule>, SecretConfigError> {
	load_file(path, Containment::Unconfined)
}

fn load_file(
	path: &Path,
	containment: Containment<'_>,
) -> Result<Vec<SecretRule>, SecretConfigError> {
	let Some(text) = project_file::read_text(path, containment, SECRETS_FILE_LIMIT)? else {
		return Ok(Vec::new());
	};
	let raw: Vec<RawRule> = serde_yaml::from_str(&text)
		.map_err(|source| SecretConfigError::Yaml { path: path.to_owned(), source })?;
	raw.into_iter()
		.enumerate()
		.map(|(index, raw)| {
			let kind = SecretKind::from_str(&raw.kind).map_err(|_| SecretConfigError::Kind {
				path: path.to_owned(),
				index,
				value: Str::new(raw.kind),
			})?;
			let mode_value = raw.mode.as_deref().unwrap_or("obfuscate");
			let mode = SecretMode::from_str(mode_value).map_err(|_| SecretConfigError::Mode {
				path: path.to_owned(),
				index,
				value: Str::new(mode_value),
			})?;
			SecretRule::new(
				kind,
				mode,
				raw.content,
				raw.replacement.map(Str::new),
				raw.flags.as_deref(),
				raw.friendly_name.map(Str::new),
			)
			.map_err(|source| SecretConfigError::Rule { path: path.to_owned(), index, source })
		})
		.collect()
}

#[cfg(test)]
mod tests {
	use std::fs;

	use omp_core::project_file::Refusal;

	use super::*;

	#[test]
	fn project_content_overrides_global() {
		let root = tempfile::tempdir().expect("tempdir");
		let global = root.path().join("global.yml");
		let project = root.path().join("project.yml");
		fs::write(
			&global,
			"- type: plain\n  content: global-secret\n- type: plain\n  content: shared-secret\n",
		)
		.expect("global");
		fs::write(&project, "- type: plain\n  content: shared-secret\n  friendlyName: project\n")
			.expect("project");
		let rules = load_secret_rules(&global, &project, root.path()).expect("rules");
		assert_eq!(rules.iter().map(SecretRule::content).collect::<Vec<_>>(), [
			"global-secret",
			"shared-secret"
		]);
		assert_eq!(rules[1].friendly_name(), Some("project"));
	}

	#[cfg(unix)]
	#[test]
	fn project_secrets_must_be_a_contained_bounded_regular_file() {
		use std::os::unix::fs::symlink;

		let scratch = tempfile::tempdir().expect("tempdir");
		let repo = scratch.path().join("repo");
		fs::create_dir_all(repo.join(".omp")).expect("project config directory");
		let global = scratch.path().join("global.yml");
		let project = repo.join(".omp/secrets.yml");
		let body = "- type: plain\n  content: from-project\n";
		fs::write(&global, "").expect("global");
		let load = || load_secret_rules(&global, &project, &repo);
		fs::write(scratch.path().join("elsewhere.yml"), body).expect("outside file");
		symlink(scratch.path().join("elsewhere.yml"), &project).expect("outside link");
		let refused = load().expect_err("an outside symlink must be refused");
		assert!(
			matches!(&refused, SecretConfigError::File(error) if error.refusal() == Some(Refusal::OutsideRoot)),
			"{refused:?}"
		);
		assert!(!refused.to_string().contains("from-project"), "the diagnostic never echoes content");
		fs::remove_file(&project).expect("remove link");

		fs::write(repo.join("rules.yml"), body).expect("inside file");
		symlink("../rules.yml", &project).expect("inside link");
		assert_eq!(load().expect("an inside symlink loads").len(), 1);
		fs::remove_file(&project).expect("remove link");

		fs::write(&project, vec![b'#'; usize::try_from(SECRETS_FILE_LIMIT).unwrap() + 1])
			.expect("oversize file");
		let oversize = load().expect_err("an oversize file must be refused");
		assert!(
			matches!(oversize, SecretConfigError::File(error) if error.refusal() == Some(Refusal::TooLarge))
		);
	}
}
