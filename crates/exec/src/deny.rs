//! Default upload deny rules for working-tree capture (ADR 0040, Part D.2).
//!
//! These are data only; capture (in `omp-envd`) compiles and applies them
//! before hashing. An untracked file that matches is excluded and counted per
//! rule; a tracked file that matches refuses the run with
//! [`crate::ExecutionError::InputRefused`]. The operator's
//! `sv_exec_upload_exclude` adds globs and `sv_exec_upload_allow` exempts
//! paths. They are needed because the repository's `.gitignore` has no `.env`
//! entry.
//!
//! Globs use gitignore name semantics: a pattern without `/` matches a file
//! name at any depth; a pattern with `/` matches that path below any
//! directory.

/// Name patterns never uploaded by default.
pub const DEFAULT_UPLOAD_DENY: &[&str] = &[
	".env",
	".env.*",
	".envrc",
	"*.pem",
	"*.key",
	"*.p12",
	"*.pfx",
	"id_rsa*",
	"id_ed25519*",
	".ssh/**",
	".aws/**",
	".gnupg/**",
	".kube/**",
	".netrc",
	".npmrc",
	".pypirc",
	".git-credentials",
	".docker/config.json",
	"*.kdbx",
	"*.tfvars",
	"*.tfstate",
];

#[cfg(test)]
mod tests {
	use super::DEFAULT_UPLOAD_DENY;

	#[test]
	fn defaults_are_unique() {
		for (index, rule) in DEFAULT_UPLOAD_DENY.iter().enumerate() {
			assert!(!DEFAULT_UPLOAD_DENY[index + 1..].contains(rule), "duplicate rule {rule}");
		}
	}
}
