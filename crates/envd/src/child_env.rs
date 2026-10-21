//! The environment a spawned child process is allowed to see.
//!
//! A child launched by OMP (an eval interpreter, a delegated agent CLI) must
//! not inherit OMP's own process environment: it routinely holds provider API
//! keys and service tokens. [`ChildEnv`] starts from a fixed allowlist of
//! ambient names, always drops any name that looks like a secret, and adds only
//! the explicit entries an operator profile names.
//!
//! The allowlist and the secret-name markers live here once; eval and the
//! delegated executors share them.

use std::{
	collections::BTreeMap,
	env,
	ffi::{OsStr, OsString},
	process::Command,
};

/// Name fragments that mark a variable as a secret. A name containing any of
/// these is never inherited, even when it would otherwise be allowed.
const SECRET_MARKERS: &[&str] =
	&["TOKEN", "SECRET", "PASSWORD", "PASSWD", "API_KEY", "PRIVATE_KEY", "CREDENTIAL"];

/// Ambient names every child may inherit when they are not secrets.
const AMBIENT_NAMES: &[&str] = &[
	"PATH",
	"HOME",
	"USER",
	"LOGNAME",
	"SHELL",
	"TMPDIR",
	"TEMP",
	"TMP",
	"LANG",
	"TERM",
	"COLORTERM",
	"NO_COLOR",
	"SYSTEMROOT",
	"WINDIR",
	"COMSPEC",
	"PATHEXT",
	"USERPROFILE",
	"APPDATA",
	"LOCALAPPDATA",
];

/// Whether `name` looks like a secret by its name alone.
#[must_use]
pub(crate) fn is_secret_name(name: &OsStr) -> bool {
	let upper = name.to_string_lossy().to_ascii_uppercase();
	SECRET_MARKERS.iter().any(|marker| upper.contains(marker))
}

/// Whether `name` is an ambient variable a child may inherit: allowlisted, a
/// locale (`LC_*`), and not secret-looking.
#[must_use]
pub(crate) fn is_ambient_name(name: &OsStr) -> bool {
	if is_secret_name(name) {
		return false;
	}
	let upper = name.to_string_lossy().to_ascii_uppercase();
	AMBIENT_NAMES.contains(&upper.as_str()) || upper.starts_with("LC_")
}

/// The environment of one child process.
///
/// Built from the parent's environment through
/// [`ambient_from`](Self::ambient_from) or [`ambient`](Self::ambient), then
/// extended with [`with_inherited`](Self::with_inherited)
/// and [`with_set`](Self::with_set). Applying it with [`apply`](Self::apply)
/// clears the command's environment first, so nothing outside it reaches the
/// child.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ChildEnv {
	vars: BTreeMap<OsString, OsString>,
}

impl ChildEnv {
	/// An empty environment: the child sees nothing.
	#[must_use]
	pub fn empty() -> Self {
		Self::default()
	}

	/// The ambient subset of this process's environment.
	#[must_use]
	pub fn ambient() -> Self {
		Self::ambient_from(env::vars_os())
	}

	/// The ambient subset of `source`.
	#[must_use]
	pub fn ambient_from(source: impl IntoIterator<Item = (OsString, OsString)>) -> Self {
		Self {
			vars: source
				.into_iter()
				.filter(|(name, _)| is_ambient_name(name))
				.collect(),
		}
	}

	/// Adds the named variables from this process's environment, secret-looking
	/// or not. The operator names each one; nothing is inherited wholesale.
	#[must_use]
	pub fn with_inherited<'a>(self, names: impl IntoIterator<Item = &'a str>) -> Self {
		self.with_inherited_from(names, env::vars_os())
	}

	/// As [`with_inherited`](Self::with_inherited), reading from `source`.
	#[must_use]
	pub fn with_inherited_from<'a>(
		mut self,
		names: impl IntoIterator<Item = &'a str>,
		source: impl IntoIterator<Item = (OsString, OsString)>,
	) -> Self {
		let wanted: Vec<&str> = names.into_iter().collect();
		for (name, value) in source {
			if wanted.iter().any(|want| OsStr::new(want) == name) {
				self.vars.insert(name, value);
			}
		}
		self
	}

	/// Sets one variable to an explicit value, replacing any inherited one.
	#[must_use]
	pub fn with_set(mut self, name: impl Into<OsString>, value: impl Into<OsString>) -> Self {
		self.vars.insert(name.into(), value.into());
		self
	}

	/// The variables, in name order.
	pub fn iter(&self) -> impl Iterator<Item = (&OsStr, &OsStr)> + '_ {
		self
			.vars
			.iter()
			.map(|(k, v)| (k.as_os_str(), v.as_os_str()))
	}

	/// Whether `name` is present.
	#[must_use]
	pub fn contains(&self, name: &OsStr) -> bool {
		self.vars.contains_key(name)
	}

	/// Clears `command`'s environment and sets exactly this one.
	pub fn apply(&self, command: &mut Command) {
		command.env_clear().envs(self.iter());
	}

	/// As [`apply`](Self::apply), for a tokio command.
	pub fn apply_tokio(&self, command: &mut tokio::process::Command) {
		command.env_clear().envs(self.iter());
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn source(pairs: &[(&str, &str)]) -> Vec<(OsString, OsString)> {
		pairs
			.iter()
			.map(|(k, v)| (OsString::from(k), OsString::from(v)))
			.collect()
	}

	#[test]
	fn ambient_keeps_allowlisted_names_and_drops_everything_else() {
		let env = ChildEnv::ambient_from(source(&[
			("PATH", "/usr/bin"),
			("HOME", "/home/u"),
			("LC_ALL", "C"),
			("LANG", "C"),
			("RANDOM_AMBIENT_VALUE", "x"),
		]));
		assert!(env.contains(OsStr::new("PATH")));
		assert!(env.contains(OsStr::new("HOME")));
		assert!(env.contains(OsStr::new("LC_ALL")));
		assert!(!env.contains(OsStr::new("RANDOM_AMBIENT_VALUE")));
	}

	#[test]
	fn ambient_never_carries_secret_looking_names_even_when_allowlisted_prefixes_match() {
		let env = ChildEnv::ambient_from(source(&[
			("ANTHROPIC_API_KEY", "sk-1"),
			("BUILDBUDDY_API_KEY", "bb-2"),
			("AWS_SECRET_ACCESS_KEY", "s3"),
			("GITHUB_TOKEN", "gh"),
			("LC_TOKEN", "locale-prefix-but-secret"),
			("PATH", "/usr/bin"),
		]));
		assert!(!env.contains(OsStr::new("ANTHROPIC_API_KEY")));
		assert!(!env.contains(OsStr::new("BUILDBUDDY_API_KEY")));
		assert!(!env.contains(OsStr::new("AWS_SECRET_ACCESS_KEY")));
		assert!(!env.contains(OsStr::new("GITHUB_TOKEN")));
		assert!(!env.contains(OsStr::new("LC_TOKEN")));
		assert!(env.contains(OsStr::new("PATH")));
	}

	#[test]
	fn an_operator_named_variable_is_inherited_but_a_sibling_secret_is_not() {
		let parent = source(&[("ANTHROPIC_API_KEY", "sk-1"), ("OPENAI_API_KEY", "sk-2")]);
		let env =
			ChildEnv::ambient_from(parent.clone()).with_inherited_from(["ANTHROPIC_API_KEY"], parent);
		assert!(env.contains(OsStr::new("ANTHROPIC_API_KEY")));
		assert!(!env.contains(OsStr::new("OPENAI_API_KEY")));
	}

	#[test]
	fn an_explicit_set_replaces_an_inherited_value() {
		let parent = source(&[("HOME", "/home/real")]);
		let env = ChildEnv::ambient_from(parent).with_set("HOME", "/tmp/isolated");
		let value: Vec<_> = env.iter().collect();
		assert_eq!(value, vec![(OsStr::new("HOME"), OsStr::new("/tmp/isolated"))]);
	}

	#[tokio::test]
	async fn applying_to_a_command_hides_a_parent_secret_from_the_child() {
		use tokio::io::AsyncReadExt as _;
		let env = ChildEnv::ambient_from(source(&[("PATH", "/usr/bin:/bin")]));
		let mut command = tokio::process::Command::new("env");
		command
			.env("OMP_TEST_CHILD_ENV_SENTINEL_TOKEN", "must-not-leak")
			.stdout(std::process::Stdio::piped());
		env.apply_tokio(&mut command);
		let mut child = command.spawn().expect("spawn env");
		let mut out = String::new();
		child
			.stdout
			.take()
			.expect("piped")
			.read_to_string(&mut out)
			.await
			.expect("read");
		child.wait().await.expect("wait");
		assert!(!out.contains("must-not-leak"), "child saw: {out}");
		assert!(out.contains("PATH="), "child lost PATH: {out}");
	}
}
