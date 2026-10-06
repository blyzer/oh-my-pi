//! Hermetic `git` for tests.
//!
//! Production git runs deliberately see the user's configuration (identity,
//! credential helpers, signing). Test fixtures must not: a developer's
//! `commit.gpgsign`, `core.hooksPath`, `init.defaultBranch`, `diff.external`
//! or aliases would otherwise decide whether a fixture commit succeeds, which
//! branch it lands on, or what a diff looks like. Every test that spawns the
//! `git` binary goes through [`command`] / [`isolate`] / [`run`] so the
//! isolation lives in one place.
//!
//! [`isolate`] cuts every config source outside the fixture repository:
//! - the global and system files (`GIT_CONFIG_GLOBAL`, `GIT_CONFIG_SYSTEM`,
//!   `GIT_CONFIG_NOSYSTEM`), including the `$XDG_CONFIG_HOME/git` fallbacks
//!   (`HOME` and `XDG_CONFIG_HOME` no longer resolve to the real user);
//! - config smuggled in through the environment (`GIT_CONFIG_COUNT`,
//!   `GIT_CONFIG_PARAMETERS`), the identity overrides
//!   (`GIT_AUTHOR_*`/`GIT_COMMITTER_*`), repository-location overrides, and the
//!   template directory (a source of default hooks and excludes);
//! - then re-pins the few settings fixtures need to be deterministic: no
//!   signing, `main` as the initial branch, and no prompts.
//!
//! Identity is deliberately not pinned here: that would outrank a fixture's own
//! repository-local `user.name`/`user.email`. Fixtures set those locally.
//! Repository-local config (`.git/config`) stays authoritative so tests can
//! still exercise per-repository settings.
//!
//! Only commands built through this module are isolated. In-process
//! gitoxide opens in the code under test read the real process environment;
//! they only read, and never sign or run hooks on behalf of a fixture.

use std::{path::Path, process::Command};

/// The null device: an empty config file and a path that is never a home
/// directory.
#[cfg(unix)]
const NULL_DEVICE: &str = "/dev/null";
/// The null device: an empty config file and a path that is never a home
/// directory.
#[cfg(windows)]
const NULL_DEVICE: &str = "NUL";

/// Config pinned at command scope (`GIT_CONFIG_COUNT`), so neither the host nor
/// an inherited `GIT_CONFIG_COUNT` can change a fixture's outcome.
const PINNED_CONFIG: [(&str, &str); 3] =
	[("commit.gpgsign", "false"), ("tag.gpgsign", "false"), ("init.defaultBranch", "main")];

/// Environment variables removed so the host cannot steer a fixture.
const STRIPPED_ENV: [&str; 17] = [
	"GIT_CONFIG",
	"GIT_CONFIG_PARAMETERS",
	"GIT_AUTHOR_NAME",
	"GIT_AUTHOR_EMAIL",
	"GIT_AUTHOR_DATE",
	"GIT_COMMITTER_NAME",
	"GIT_COMMITTER_EMAIL",
	"GIT_COMMITTER_DATE",
	"GIT_TEMPLATE_DIR",
	"GIT_DIR",
	"GIT_COMMON_DIR",
	"GIT_WORK_TREE",
	"GIT_INDEX_FILE",
	"GIT_OBJECT_DIRECTORY",
	"GIT_ALTERNATE_OBJECT_DIRECTORIES",
	"GIT_EXTERNAL_DIFF",
	"XDG_CONFIG_HOME",
];

/// Make `command` ignore the host's git configuration. See the module docs for
/// exactly what is cut and what is pinned. Safe to call on any command that
/// will run `git`; later `.env(..)` calls by the caller still win.
pub fn isolate(command: &mut Command) -> &mut Command {
	for name in STRIPPED_ENV {
		command.env_remove(name);
	}
	command
		.env("HOME", NULL_DEVICE)
		.env("GIT_CONFIG_GLOBAL", NULL_DEVICE)
		.env("GIT_CONFIG_SYSTEM", NULL_DEVICE)
		.env("GIT_CONFIG_NOSYSTEM", "1")
		.env("GIT_TERMINAL_PROMPT", "0")
		.env("GIT_CONFIG_COUNT", PINNED_CONFIG.len().to_string());
	for (index, (key, value)) in PINNED_CONFIG.iter().enumerate() {
		command
			.env(format!("GIT_CONFIG_KEY_{index}"), key)
			.env(format!("GIT_CONFIG_VALUE_{index}"), value);
	}
	command
}

/// A `git` command that is already [`isolate`]d.
#[must_use]
pub fn command() -> Command {
	let mut command = Command::new("git");
	isolate(&mut command);
	command
}

/// Run `git args…` in `cwd` hermetically and return its stdout verbatim.
///
/// # Panics
/// When git cannot be launched, exits non-zero, or prints non-UTF-8: this is
/// fixture plumbing, so a failure is a broken test, not a result to handle.
pub fn run(cwd: &Path, args: &[&str]) -> String {
	let output = command()
		.current_dir(cwd)
		.args(args)
		.output()
		.unwrap_or_else(|err| panic!("run git {args:?}: {err}"));
	assert!(
		output.status.success(),
		"git {args:?} failed in {}: {}",
		cwd.display(),
		String::from_utf8_lossy(&output.stderr)
	);
	String::from_utf8(output.stdout).unwrap_or_else(|err| panic!("git {args:?} output: {err}"))
}

/// [`run`] with the trailing whitespace of the output removed.
///
/// # Panics
/// As [`run`].
pub fn run_trimmed(cwd: &Path, args: &[&str]) -> String {
	let mut output = run(cwd, args);
	output.truncate(output.trim_end().len());
	output
}

#[cfg(all(test, unix))]
mod tests {
	use std::{fs, os::unix::fs::PermissionsExt, path::PathBuf};

	use super::*;

	/// A home directory whose configuration would break every fixture: signing
	/// through a signer that does not exist, a failing global hook, another
	/// initial branch, a foreign identity and a rewriting alias.
	struct Hostile {
		dir:    tempfile::TempDir,
		global: PathBuf,
	}

	impl Hostile {
		fn new() -> Self {
			let dir = tempfile::tempdir().expect("tempdir");
			let hooks = dir.path().join("hooks");
			fs::create_dir(&hooks).expect("hooks dir");
			let hook = hooks.join("pre-commit");
			fs::write(&hook, "#!/bin/sh\necho hostile hook ran >&2\nexit 1\n").expect("hook");
			fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).expect("chmod");
			let global = dir.path().join("hostile.gitconfig");
			let config = format!(
				"[commit]\n\tgpgsign = true\n[tag]\n\tgpgsign = true\n[gpg]\n\tprogram = \
				 {root}/no-such-signer\n[core]\n\thooksPath = {hooks}\n[init]\n\tdefaultBranch = \
				 hostile\n[user]\n\tname = Hostile\n\temail = \
				 hostile@example.invalid\n[alias]\n\tstatus = !echo aliased\n",
				root = dir.path().display(),
				hooks = hooks.display()
			);
			fs::write(&global, config).expect("global config");
			Self { dir, global }
		}

		/// Point a command at the hostile configuration the way a developer's
		/// shell would: through `HOME`, `XDG_CONFIG_HOME`, the global file and
		/// config smuggled in through the environment.
		fn expose<'c>(&self, command: &'c mut Command) -> &'c mut Command {
			command
				.env("HOME", self.dir.path())
				.env("XDG_CONFIG_HOME", self.dir.path())
				.env("GIT_CONFIG_GLOBAL", &self.global)
				.env_remove("GIT_CONFIG_NOSYSTEM")
				.env("GIT_CONFIG_COUNT", "1")
				.env("GIT_CONFIG_KEY_0", "commit.gpgsign")
				.env("GIT_CONFIG_VALUE_0", "true")
				.env("GIT_AUTHOR_NAME", "Env Author")
		}
	}

	fn commit_seed(command: &mut Command, repo: &Path) -> std::process::Output {
		command
			.current_dir(repo)
			.args(["commit", "-q", "--allow-empty", "-m", "seed"])
			.output()
			.expect("launch git")
	}

	fn repo_with_identity() -> tempfile::TempDir {
		let repo = tempfile::tempdir().expect("tempdir");
		run(repo.path(), &["init", "-q"]);
		run(repo.path(), &["config", "user.name", "Fixture"]);
		run(repo.path(), &["config", "user.email", "fixture@example.invalid"]);
		repo
	}

	#[test]
	fn hostile_config_breaks_an_unisolated_commit() {
		// Control: without `isolate` the same setup fails, so the isolation
		// assertions below are not vacuous.
		let hostile = Hostile::new();
		let repo = repo_with_identity();
		let mut raw = Command::new("git");
		hostile.expose(&mut raw);
		let output = commit_seed(&mut raw, repo.path());
		assert!(!output.status.success(), "hostile config should break a bare commit");
	}

	#[test]
	fn isolate_overrides_hostile_global_system_and_env_config() {
		let hostile = Hostile::new();
		let repo = repo_with_identity();
		let mut hardened = Command::new("git");
		isolate(hostile.expose(&mut hardened));
		let output = commit_seed(&mut hardened, repo.path());
		assert!(
			output.status.success(),
			"signing/hooks leaked into the fixture: {}",
			String::from_utf8_lossy(&output.stderr)
		);
		// Repository-local identity wins; neither the hostile global identity
		// nor `GIT_AUTHOR_NAME` reached the commit.
		assert_eq!(
			run_trimmed(repo.path(), &["log", "-1", "--format=%an <%ae>"]),
			"Fixture <fixture@example.invalid>"
		);
		assert_eq!(run_trimmed(repo.path(), &["config", "--get", "commit.gpgsign"]), "false");
	}

	#[test]
	fn isolated_config_lists_nothing_outside_the_repository_and_the_pins() {
		let hostile = Hostile::new();
		let repo = repo_with_identity();
		let mut list = Command::new("git");
		isolate(hostile.expose(&mut list));
		let output = list
			.current_dir(repo.path())
			.args(["config", "--list", "--show-origin"])
			.output()
			.expect("launch git");
		assert!(output.status.success());
		let listing = String::from_utf8(output.stdout).expect("utf-8");
		assert!(!listing.contains("hostile"), "host config leaked:\n{listing}");
		for line in listing.lines() {
			let origin = line.split('\t').next().unwrap_or_default();
			assert!(
				origin == "file:.git/config" || origin == "command line:",
				"unexpected config origin {origin:?} in:\n{listing}"
			);
		}
	}

	#[test]
	fn initial_branch_is_pinned() {
		let hostile = Hostile::new();
		let repo = tempfile::tempdir().expect("tempdir");
		let mut init = Command::new("git");
		isolate(hostile.expose(&mut init));
		let output = init
			.current_dir(repo.path())
			.args(["init", "-q"])
			.output()
			.expect("launch git");
		assert!(output.status.success());
		assert_eq!(run_trimmed(repo.path(), &["symbolic-ref", "--short", "HEAD"]), "main");
	}

	#[test]
	fn caller_env_set_after_isolation_still_wins() {
		let repo = repo_with_identity();
		let output = command()
			.current_dir(repo.path())
			.env("GIT_AUTHOR_NAME", "Explicit")
			.args(["commit", "-q", "--allow-empty", "-m", "seed"])
			.output()
			.expect("launch git");
		assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
		assert_eq!(run_trimmed(repo.path(), &["log", "-1", "--format=%an"]), "Explicit");
	}

	#[test]
	fn repository_local_config_stays_authoritative() {
		let repo = tempfile::tempdir().expect("tempdir");
		run(repo.path(), &["init", "-q"]);
		run(repo.path(), &["config", "core.hooksPath", "custom-hooks"]);
		assert_eq!(run_trimmed(repo.path(), &["config", "--get", "core.hooksPath"]), "custom-hooks");
	}
}
