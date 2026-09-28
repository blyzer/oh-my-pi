//! Operator approval for the processes installed plugins launch.
//!
//! An installed Claude-format plugin can declare MCP servers, language
//! servers, and debug adapters, and each of those starts an executable the
//! plugin names. None of them starts until the operator approved that exact
//! launch: the approval is keyed on the plugin id plus a
//! [`plugin_command_digest`] (a [`Hash32`] over the plugin version, command,
//! arguments, and environment overrides), so a plugin update or an edited
//! command line asks again.
//!
//! Approvals persist beside the extension grants in the local grant file
//! ([`crate::trust::GrantsFile::plugin_commands`]).
//! [`crate::claude_plugin::ClaudePlugins::resolve`] attaches each plugin's
//! approved digests to it, and every runtime seam that would launch a plugin
//! process gates it through
//! [`crate::claude_plugin::ClaudePlugin::admit_launch`]; a refused launch
//! surfaces as a [`PluginCommandBlocked`] naming the plugin, the command, and
//! the `omp ext trust` invocation that approves it.

use std::fmt::{self, Display};

use omp_core::{Hash32, Str};
use serde::{Deserialize, Serialize};
use strum::{Display as StrumDisplay, IntoStaticStr};

/// Domain separator of [`plugin_command_digest`]'s canonical encoding.
const DIGEST_DOMAIN: &[u8] = b"omp.plugin-command.v1\0";

/// The plugin component that would launch a process.
#[derive(
	Clone,
	Copy,
	Debug,
	Deserialize,
	Eq,
	Hash,
	IntoStaticStr,
	Ord,
	PartialEq,
	PartialOrd,
	Serialize,
	StrumDisplay,
)]
#[serde(rename_all = "kebab-case")]
pub enum PluginLaunchKind {
	/// A stdio MCP server (`.mcp.json` / manifest `mcpServers`).
	#[strum(to_string = "MCP server")]
	McpServer,
	/// A language server (`.lsp.json` family / manifest `lspServers`).
	#[strum(to_string = "language server")]
	LanguageServer,
	/// A debug adapter (`.dap.json` family).
	#[strum(to_string = "debug adapter")]
	DebugAdapter,
}

/// One process a plugin declaration would start, as the launching seam
/// resolved it (plugin variables expanded, relative commands rooted).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PluginLaunch {
	/// Declaring component.
	pub kind:    PluginLaunchKind,
	/// Server or adapter name as the runtime registers it.
	pub server:  Str,
	/// Executable; empty when the declaration only overrides the arguments
	/// or environment of a server another layer declares.
	pub command: Str,
	/// Arguments in order.
	pub args:    Box<[Str]>,
	/// Environment overrides, sorted by name.
	pub env:     Box<[(Str, Str)]>,
}

impl PluginLaunch {
	/// A launch whose environment overrides are canonicalized (sorted by
	/// name) so the digest does not depend on declaration order.
	#[must_use]
	pub fn new(
		kind: PluginLaunchKind,
		server: Str,
		command: Str,
		args: impl IntoIterator<Item = Str>,
		env: impl IntoIterator<Item = (Str, Str)>,
	) -> Self {
		let mut env = env.into_iter().collect::<Vec<_>>();
		env.sort_unstable();
		Self { kind, server, command, args: args.into_iter().collect(), env: env.into() }
	}

	/// The command line for display: the executable then each argument,
	/// single-quoted when it is empty or contains whitespace or quotes.
	#[must_use]
	pub fn command_line(&self) -> CommandLine<'_> {
		CommandLine { command: &self.command, args: &self.args }
	}
}

/// Display form of a [`PluginLaunch`] command line.
#[derive(Clone, Copy, Debug)]
pub struct CommandLine<'a> {
	command: &'a str,
	args:    &'a [Str],
}

impl Display for CommandLine<'_> {
	fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
		if self.command.is_empty() {
			formatter.write_str("<inherited command>")?;
		} else {
			write_word(formatter, self.command)?;
		}
		for arg in self.args {
			formatter.write_str(" ")?;
			write_word(formatter, arg)?;
		}
		Ok(())
	}
}

fn write_word(formatter: &mut fmt::Formatter<'_>, word: &str) -> fmt::Result {
	if !word.is_empty()
		&& !word
			.chars()
			.any(|c| c.is_whitespace() || matches!(c, '\'' | '"' | '\\'))
	{
		return formatter.write_str(word);
	}
	formatter.write_str("'")?;
	for (index, part) in word.split('\'').enumerate() {
		if index > 0 {
			formatter.write_str("'\\''")?;
		}
		formatter.write_str(part)?;
	}
	formatter.write_str("'")
}

/// The approval key of `launch` for the plugin `plugin_id` at `version`.
///
/// SHA-256 over a length-prefixed canonical encoding of the plugin id,
/// version, command, arguments, and sorted environment overrides. The
/// component kind and server name are not part of it: the same process
/// approved once is the same process wherever the plugin declares it.
#[must_use]
pub fn plugin_command_digest(plugin_id: &str, version: &str, launch: &PluginLaunch) -> Hash32 {
	let mut hasher = Hash32::hasher();
	hasher.update(DIGEST_DOMAIN);
	let mut field = |bytes: &[u8]| {
		hasher
			.update((bytes.len() as u64).to_le_bytes())
			.update(bytes);
	};
	field(plugin_id.as_bytes());
	field(version.as_bytes());
	field(launch.command.as_bytes());
	field(&(launch.args.len() as u64).to_le_bytes());
	for arg in &launch.args {
		field(arg.as_bytes());
	}
	field(&(launch.env.len() as u64).to_le_bytes());
	for (name, value) in &launch.env {
		field(name.as_bytes());
		field(value.as_bytes());
	}
	hasher.finalize()
}

/// A plugin launch the operator has not approved; the process does not
/// start.
///
/// Carries what the operator needs to recognize and approve it; the
/// environment overrides are part of the digest but not of the notice.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[error(
	"plugin `{plugin}` {kind} `{server}` would run `{}`, which is not approved, so it was not \
	 started; approve it with `omp ext trust {plugin} --approve-command {digest}` and start a \
	 new session",
	self.command_line()
)]
pub struct PluginCommandBlocked {
	/// Plugin id, `name@marketplace`.
	pub plugin: Str,
	/// Declaring component.
	pub kind:   PluginLaunchKind,
	/// Server or adapter name.
	pub server: Str,
	/// The refused executable (empty for an argument or environment override)
	/// followed by its arguments.
	argv:       Box<[Str]>,
	/// [`plugin_command_digest`] an approval must carry.
	pub digest: Hash32,
}

impl PluginCommandBlocked {
	/// The refusal of `launch` for `plugin`, whose approval key is `digest`.
	#[must_use]
	pub fn new(plugin: Str, launch: PluginLaunch, digest: Hash32) -> Self {
		let PluginLaunch { kind, server, command, args, env: _ } = launch;
		let argv = std::iter::once(command).chain(args).collect();
		Self { plugin, kind, server, argv, digest }
	}

	/// The refused executable; empty for an argument or environment override.
	#[must_use]
	pub fn command(&self) -> &Str {
		&self.argv[0]
	}

	/// The refused arguments.
	#[must_use]
	pub fn args(&self) -> &[Str] {
		&self.argv[1..]
	}

	/// The refused command line for display.
	#[must_use]
	pub fn command_line(&self) -> CommandLine<'_> {
		CommandLine { command: self.command(), args: self.args() }
	}
}

// Returned by value from every launch gate (`Result<(), PluginCommandBlocked>`)
// and carried in plugin diagnostic lists; keep it below the large-error bound.
const _: () =
	assert!(size_of::<PluginCommandBlocked>() <= 104, "PluginCommandBlocked must stay compact");

#[cfg(test)]
mod tests {
	use super::*;

	fn launch(command: &str, args: &[&str], env: &[(&str, &str)]) -> PluginLaunch {
		PluginLaunch::new(
			PluginLaunchKind::LanguageServer,
			Str::new_static("srv"),
			Str::new(command),
			args.iter().map(|arg| Str::new(*arg)),
			env.iter()
				.map(|(name, value)| (Str::new(*name), Str::new(*value))),
		)
	}

	#[test]
	fn digest_binds_plugin_version_command_args_and_env() {
		let base = launch("/p/bin/srv", &["--stdio"], &[("A", "1")]);
		let digest = plugin_command_digest("p@m", "1.0.0", &base);
		assert_eq!(digest, plugin_command_digest("p@m", "1.0.0", &base));
		for (plugin, version, other) in [
			("q@m", "1.0.0", base.clone()),
			("p@m", "1.0.1", base),
			("p@m", "1.0.0", launch("/p/bin/other", &["--stdio"], &[("A", "1")])),
			("p@m", "1.0.0", launch("/p/bin/srv", &["--stdio", "--x"], &[("A", "1")])),
			("p@m", "1.0.0", launch("/p/bin/srv", &["--stdio"], &[("A", "2")])),
			("p@m", "1.0.0", launch("/p/bin/srv", &[], &[("A", "1")])),
		] {
			assert_ne!(digest, plugin_command_digest(plugin, version, &other), "{plugin} {version}");
		}
		// Field boundaries are length-prefixed: moving a byte between the
		// command and an argument changes the digest.
		assert_ne!(
			plugin_command_digest("p@m", "1", &launch("ab", &["c"], &[])),
			plugin_command_digest("p@m", "1", &launch("a", &["bc"], &[])),
		);
	}

	#[test]
	fn digest_ignores_kind_server_and_env_order() {
		let first = launch("srv", &[], &[("A", "1"), ("B", "2")]);
		let mut second = launch("srv", &[], &[("B", "2"), ("A", "1")]);
		second.kind = PluginLaunchKind::McpServer;
		second.server = Str::new_static("other");
		assert_eq!(
			plugin_command_digest("p@m", "1", &first),
			plugin_command_digest("p@m", "1", &second)
		);
	}

	#[test]
	fn blocked_diagnostic_names_plugin_command_and_approval() {
		let launch = launch("/p/bin/srv", &["--stdio", "a b"], &[]);
		let digest = plugin_command_digest("p@m", "1", &launch);
		let blocked = PluginCommandBlocked::new(Str::new_static("p@m"), launch, digest);
		let text = blocked.to_string();
		assert!(text.contains("plugin `p@m` language server `srv`"), "{text}");
		assert!(text.contains("`/p/bin/srv --stdio 'a b'`"), "{text}");
		assert!(text.contains(&format!("omp ext trust p@m --approve-command {digest}")), "{text}");
	}
}
