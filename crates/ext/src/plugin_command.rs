//! Operator approval for the processes installed plugins launch.
//!
//! An installed Claude-format plugin can declare MCP servers, language
//! servers, debug adapters, and hooks, and an Agent Plugins 1.0 package can
//! declare MCP servers; each of those runs a command the plugin names. None
//! of them runs until the operator approved that exact launch: the approval
//! is keyed on the plugin identity plus a [`plugin_command_digest`] (a
//! [`Hash32`] over the plugin version, command, arguments, environment
//! overrides, working directory, for a hook the event and matcher that
//! trigger it, and the contents of every plugin file the launch names), so a
//! plugin update, an edited command line, a hook moved to another event, or
//! a script the plugin ships edited in place asks again.
//!
//! # Plugin files
//!
//! The launching seam binds the files a launch names inside the plugin root
//! ([`PluginLaunch::with_plugin_files`], [`PluginFiles`]): the command when
//! it resolves to a file there, and every argument that does (after
//! `${CLAUDE_PLUGIN_ROOT}` expansion; a relative one against the launch's
//! working directory, else the plugin root). A hook's script is split into
//! shell words (quotes removed, no expansion) and every word that names a
//! file inside the plugin root after `${CLAUDE_PLUGIN_ROOT}` or
//! `$CLAUDE_PLUGIN_ROOT` expansion is bound; a path only a nested shell
//! (`bash -c '…'`), a variable, or a word fragment (`--config=<path>`) forms
//! is not. Files outside the plugin root (`node`, `npx`, `python`, a symlink
//! out of the root) are not hashed. A named file that exists but cannot be
//! read makes the launch unapprovable ([`PluginFileUnreadable`]): it never
//! runs. The files are hashed when the seam builds the launch, which is when
//! it admits it.
//!
//! The digest domain is `omp.plugin-command.v2`; approvals recorded under
//! `v1` (before plugin files were bound) no longer match anything, so every
//! plugin command must be approved again once.
//!
//! Plugin identities ([`PluginId`]): a plugin installed from a marketplace,
//! in either layout, is approved under its registry id `name@marketplace`
//! (Claude Code's convention), so two marketplaces shipping the same plugin
//! name never share approvals; an Agent Plugins 1.0 package discovered in a
//! plugin directory or named with `--plugin-dir`/`--extension` is approved
//! under its manifest `name`.
//!
//! Approvals persist beside the extension grants in the local grant file
//! ([`crate::trust::GrantsFile::plugin_commands`]), read once per session
//! into [`CommandApprovals`].
//! [`crate::claude_plugin::ClaudePlugins::resolve`] attaches each plugin's
//! approved digests to it, and every runtime seam that would launch a plugin
//! process gates it through
//! [`crate::claude_plugin::ClaudePlugin::admit_launch`] or
//! [`CommandApprovals::admit`]; a refused launch surfaces as a
//! [`PluginCommandBlocked`] naming the plugin, the command, and the
//! `omp ext trust` invocation that approves it.

use std::{
	fmt::{self, Display},
	fs, io,
	path::{Path, PathBuf},
	sync::Arc,
};

use omp_core::{Hash32, Str};
use serde::{Deserialize, Serialize};
use strum::{Display as StrumDisplay, IntoStaticStr};

use crate::claude_plugin::{expand_plugin_vars, resolve_plugin_command};

/// Domain separator of [`plugin_command_digest`]'s canonical encoding. `v2`
/// binds the plugin files a launch names; `v1` approvals match nothing.
const DIGEST_DOMAIN: &[u8] = b"omp.plugin-command.v2\0";

omp_core::string_id!(
	/// A plugin's identity: what its command approvals, its blocked-launch
	/// reports, `omp ext trust`, and its hook and MCP server names are keyed
	/// on.
	///
	/// A marketplace install is `name@marketplace` ([`Self::installed`]); an
	/// Agent Plugins package loaded from a local directory is its manifest
	/// `name`.
	PluginId
);

impl PluginId {
	/// The identity of the plugin `name` installed from `marketplace`.
	#[must_use]
	pub fn installed(name: &str, marketplace: &str) -> Self {
		let mut id = omp_core::StrMut::with_capacity(name.len() + 1 + marketplace.len());
		id.push_str(name);
		id.push('@');
		id.push_str(marketplace);
		Self::new(id.freeze())
	}
}

impl PluginId<str> {
	/// The identity as one path segment: every character but ASCII letters,
	/// digits, `_`, and `-` replaced by `-` (`docs@official` is
	/// `docs-official`), as Claude Code names a plugin's data directory.
	#[must_use]
	pub fn dir_name(&self) -> String {
		self
			.chars()
			.map(|c| {
				if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
					c
				} else {
					'-'
				}
			})
			.collect()
	}
}

/// The plugin-root variables a hook script may name unbraced; the hook host
/// exports each.
const BARE_ROOT_VARS: [&str; 3] = ["$CLAUDE_PLUGIN_ROOT", "$PLUGIN_ROOT", "$OMP_PLUGIN_ROOT"];

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
	/// A `command` hook handler (`hooks/hooks.json` / manifest `hooks`); its
	/// command is the script omp's in-process shell runs.
	#[strum(to_string = "hook")]
	Hook,
}

/// One process a plugin declaration would start, as the launching seam
/// resolved it (plugin variables expanded, relative commands rooted).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PluginLaunch {
	/// Declaring component.
	pub kind:    PluginLaunchKind,
	/// Server or adapter name as the runtime registers it; for a hook, the
	/// trigger: its event, then its matcher unless it matches everything
	/// (`PreToolUse Bash|Edit`).
	pub server:  Str,
	/// Executable; empty when the declaration only overrides the arguments
	/// or environment of a server another layer declares. For a hook, the
	/// whole script the in-process shell runs.
	pub command: Str,
	/// Arguments in order.
	pub args:    Box<[Str]>,
	/// Environment overrides, sorted by name.
	pub env:     Box<[(Str, Str)]>,
	/// Declared working directory, when the launch sets one.
	pub cwd:     Option<Str>,
	/// The plugin files it names, by content ([`Self::with_plugin_files`]);
	/// none until the launching seam binds them.
	pub files:   PluginFiles,
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
		Self {
			kind,
			server,
			command,
			args: args.into_iter().collect(),
			env: env.into(),
			cwd: None,
			files: PluginFiles::default(),
		}
	}

	/// The same launch run in the working directory `cwd`.
	#[must_use]
	pub fn with_cwd(mut self, cwd: Option<Str>) -> Self {
		self.cwd = cwd;
		self
	}

	/// The same launch bound to the contents of every file it names inside
	/// the plugin rooted at `root` ([`PluginFiles::named_by`]). Every seam
	/// that launches a plugin process builds its launches through this, set
	/// after the working directory, so the approval key covers what runs.
	#[must_use]
	pub fn with_plugin_files(mut self, root: &Path) -> Self {
		self.files = PluginFiles::named_by(&self, root);
		self
	}

	/// The command line for display: the executable then each argument,
	/// single-quoted when it is empty or contains whitespace or quotes; a
	/// hook's script verbatim.
	#[must_use]
	pub fn command_line(&self) -> CommandLine<'_> {
		CommandLine { command: &self.command, args: &self.args, script: self.kind.is_script() }
	}
}

impl PluginLaunchKind {
	/// Whether the launch's command is a whole shell script rather than an
	/// executable.
	#[must_use]
	pub const fn is_script(self) -> bool {
		matches!(self, Self::Hook)
	}
}

/// One plugin file a launch names, bound into its approval by content.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct PluginFile {
	/// Path relative to the canonical plugin root.
	pub path: Str,
	/// SHA-256 of its contents.
	pub hash: Hash32,
}

/// A file inside the plugin root that a launch names but that cannot be
/// read, so its contents cannot be bound: the launch cannot be approved and
/// never runs.
#[derive(Debug, thiserror::Error)]
#[error("plugin file `{}` cannot be read", path.display())]
pub struct PluginFileUnreadable {
	/// The file.
	pub path:   PathBuf,
	/// Why reading it failed.
	#[source]
	pub source: io::Error,
}

// Two refusals of one file compare equal when they name the same file and
// fail the same way; `io::Error` itself has no equality.
impl PartialEq for PluginFileUnreadable {
	fn eq(&self, other: &Self) -> bool {
		self.path == other.path && self.source.kind() == other.source.kind()
	}
}

impl Eq for PluginFileUnreadable {}

/// The plugin files a launch names, as its approval binds them.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PluginFiles {
	/// Every file inside the plugin root it names, sorted by path, each once.
	Hashed(Arc<[PluginFile]>),
	/// A file it names cannot be read: the launch cannot be approved.
	Unreadable(Arc<PluginFileUnreadable>),
}

impl Default for PluginFiles {
	fn default() -> Self {
		Self::Hashed(Arc::default())
	}
}

impl PluginFiles {
	/// The files `launch` names inside the plugin rooted at `root`.
	///
	/// A server's command and each argument are candidates, after
	/// `${CLAUDE_PLUGIN_ROOT}` expansion ([`expand_plugin_vars`]) and
	/// relative resolution ([`resolve_plugin_command`]) against the launch's
	/// working directory, else `root`. A hook's script contributes every
	/// shell word that is an absolute path once `$CLAUDE_PLUGIN_ROOT` is
	/// expanded (relative words resolve against the project the hook runs
	/// in). A candidate counts when it resolves to an existing regular file
	/// inside the canonical `root`; one that exists but cannot be read (or
	/// whose directory cannot be searched) makes the whole launch
	/// [`Self::Unreadable`]. A root that does not exist names nothing.
	#[must_use]
	pub fn named_by(launch: &PluginLaunch, root: &Path) -> Self {
		let Ok(root) = fs::canonicalize(root) else {
			return Self::default();
		};
		let mut files = Vec::new();
		let mut bind = |candidate: PathBuf| -> Result<(), PluginFileUnreadable> {
			if let Some(file) = plugin_file(&root, &candidate)? {
				files.push(file);
			}
			Ok(())
		};
		let bound = if launch.kind.is_script() {
			script_words(&launch.command)
				.into_iter()
				.filter_map(|word| script_path(&word, &root))
				.try_for_each(&mut bind)
		} else {
			let base = launch
				.cwd
				.as_deref()
				.map(Path::new)
				.filter(|cwd| cwd.is_absolute())
				.unwrap_or(&root);
			std::iter::once(&launch.command)
				.chain(launch.args.iter())
				.filter(|word| !word.is_empty())
				.map(|word| {
					let resolved =
						resolve_plugin_command(expand_plugin_vars(word.clone(), &root, None), base);
					base.join(resolved.as_str())
				})
				.try_for_each(&mut bind)
		};
		if let Err(unreadable) = bound {
			return Self::Unreadable(Arc::new(unreadable));
		}
		files.sort_unstable();
		files.dedup();
		Self::Hashed(files.into())
	}

	/// The bound files; empty for [`Self::Unreadable`].
	#[must_use]
	pub fn hashed(&self) -> &[PluginFile] {
		match self {
			Self::Hashed(files) => files,
			Self::Unreadable(_) => &[],
		}
	}

	/// The file that cannot be read, when one makes the launch unapprovable.
	#[must_use]
	pub const fn unreadable(&self) -> Option<&Arc<PluginFileUnreadable>> {
		match self {
			Self::Hashed(_) => None,
			Self::Unreadable(unreadable) => Some(unreadable),
		}
	}
}

/// `candidate` as a bound file when it resolves to a regular file inside the
/// canonical `root`. Missing paths and paths outside `root` name nothing;
/// a file that exists but cannot be read, or a path under `root` whose
/// directories cannot be searched, is unreadable.
fn plugin_file(root: &Path, candidate: &Path) -> Result<Option<PluginFile>, PluginFileUnreadable> {
	let unreadable = |path: &Path, source| PluginFileUnreadable { path: path.to_path_buf(), source };
	let real = match fs::canonicalize(candidate) {
		Ok(real) => real,
		Err(source)
			if source.kind() == io::ErrorKind::PermissionDenied && candidate.starts_with(root) =>
		{
			return Err(unreadable(candidate, source));
		},
		Err(_) => return Ok(None),
	};
	let Ok(relative) = real.strip_prefix(root) else {
		return Ok(None);
	};
	let metadata = fs::metadata(&real).map_err(|source| unreadable(&real, source))?;
	if !metadata.is_file() {
		return Ok(None);
	}
	let mut hasher = Hash32::hasher();
	fs::File::open(&real)
		.and_then(|mut file| io::copy(&mut file, &mut hasher))
		.map_err(|source| unreadable(&real, source))?;
	Ok(Some(PluginFile { path: Str::new(relative.to_string_lossy()), hash: hasher.finalize() }))
}

/// A hook script word as the path it names inside `root`, when it is one:
/// `$CLAUDE_PLUGIN_ROOT`-prefixed words are rooted at `root`, and other
/// absolute words (`${CLAUDE_PLUGIN_ROOT}` was expanded when the launch was
/// built) are taken as they are.
fn script_path(word: &str, root: &Path) -> Option<PathBuf> {
	for var in BARE_ROOT_VARS {
		if let Some(rest) = word.strip_prefix(var) {
			return match rest.strip_prefix('/') {
				Some(rest) => Some(root.join(rest)),
				None if rest.is_empty() => Some(root.to_path_buf()),
				None => None,
			};
		}
	}
	let path = Path::new(word);
	path.is_absolute().then(|| path.to_path_buf())
}

/// The words of `script` as a POSIX shell splits them, before expansion:
/// single quotes literal, double quotes with `\` escaping `"`, `\`, `$`, and
/// `` ` ``, a backslash outside quotes escaping the next character, and
/// unquoted whitespace or control operators (`;`, `&`, `|`, `<`, `>`, `(`,
/// `)`) ending a word.
fn script_words(script: &str) -> Vec<String> {
	let mut words = Vec::new();
	let mut word = String::new();
	let mut started = false;
	let mut chars = script.chars();
	while let Some(ch) = chars.next() {
		match ch {
			'\'' => {
				started = true;
				word.extend(chars.by_ref().take_while(|&ch| ch != '\''));
			},
			'"' => {
				started = true;
				while let Some(ch) = chars.next() {
					match ch {
						'"' => break,
						'\\' => match chars.next() {
							Some(next @ ('"' | '\\' | '$' | '`')) => word.push(next),
							Some(next) => {
								word.push('\\');
								word.push(next);
							},
							None => word.push('\\'),
						},
						ch => word.push(ch),
					}
				}
			},
			'\\' => {
				started = true;
				if let Some(next) = chars.next() {
					word.push(next);
				}
			},
			ch if ch.is_whitespace() || matches!(ch, ';' | '&' | '|' | '<' | '>' | '(' | ')') => {
				if started {
					words.push(std::mem::take(&mut word));
					started = false;
				}
			},
			ch => {
				started = true;
				word.push(ch);
			},
		}
	}
	if started {
		words.push(word);
	}
	words
}

/// Display form of a [`PluginLaunch`] command line.
#[derive(Clone, Copy, Debug)]
pub struct CommandLine<'a> {
	command: &'a str,
	args:    &'a [Str],
	script:  bool,
}

impl Display for CommandLine<'_> {
	fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
		if self.script {
			formatter.write_str(self.command)?;
		} else if self.command.is_empty() {
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
/// version, command, arguments, sorted environment overrides, when set the
/// working directory, and the plugin files it names ([`PluginFiles`]: each
/// path relative to the plugin root with its content hash, sorted by path),
/// so editing a script the plugin ships changes the key even when its
/// version does not. The component kind and server name are not
/// part of it for a server: the same process approved once is the same
/// process wherever the plugin declares it. A hook's trigger (its event and
/// matcher, [`PluginLaunch::server`]) is: the event decides what the command
/// reads on stdin and what its output can decide (deny a tool call, add
/// model context, continue a stopped turn), and the matcher decides when it
/// runs, so moving an approved command to another event or widening its
/// matcher asks again. A hook's timeout and `async` flag are not: they bound
/// or discard what the approved command does, never what runs or when.
#[must_use]
pub fn plugin_command_digest(
	plugin_id: &PluginId<str>,
	version: &str,
	launch: &PluginLaunch,
) -> Hash32 {
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
	// Optional sections, each tagged, in a fixed order; an absent section
	// adds nothing, so server digests without them are unchanged.
	if let Some(cwd) = &launch.cwd {
		field(b"cwd");
		field(cwd.as_bytes());
	}
	if launch.kind == PluginLaunchKind::Hook {
		field(b"hook");
		field(launch.server.as_bytes());
	}
	match &launch.files {
		PluginFiles::Hashed(files) => {
			field(b"files");
			field(&(files.len() as u64).to_le_bytes());
			for file in files.iter() {
				field(file.path.as_bytes());
				field(file.hash.as_bytes());
			}
		},
		// Never admitted (see `CommandApprovals::admit`); tagged apart so it
		// can never share a readable launch's key either.
		PluginFiles::Unreadable(unreadable) => {
			field(b"unreadable");
			field(unreadable.path.as_os_str().as_encoded_bytes());
		},
	}
	hasher.finalize()
}

/// The operator's plugin command approvals, read once from the local grant
/// file.
///
/// Every approved [`plugin_command_digest`] with the plugin identity it was
/// approved for. The default approves nothing, so a seam that was never
/// handed approvals fails closed.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CommandApprovals {
	approvals: Arc<[(PluginId, Hash32)]>,
}

impl CommandApprovals {
	/// Approvals from `(plugin identity, digest)` pairs.
	#[must_use]
	pub fn new(approvals: impl IntoIterator<Item = (PluginId, Hash32)>) -> Self {
		Self { approvals: approvals.into_iter().collect() }
	}

	/// Digests approved for `plugin`.
	pub fn of<'a>(&'a self, plugin: &'a PluginId<str>) -> impl Iterator<Item = Hash32> + Clone + 'a {
		self
			.approvals
			.iter()
			.filter(move |(id, _)| **id == *plugin)
			.map(|(_, digest)| *digest)
	}

	/// Whether the operator approved the launch with `digest` for `plugin`.
	#[must_use]
	pub fn approves(&self, plugin: &PluginId<str>, digest: &Hash32) -> bool {
		self.of(plugin).any(|approved| approved == *digest)
	}

	/// Admits `launch` of `plugin` at `version` when the operator approved
	/// it and every plugin file it names could be read; otherwise returns the
	/// diagnostic naming the plugin, the command, and how to approve it (or
	/// the file that cannot be read). A launching seam never starts a refused
	/// launch.
	pub fn admit(
		&self,
		plugin: &PluginId,
		version: &str,
		launch: PluginLaunch,
	) -> Result<(), PluginCommandBlocked> {
		let digest = plugin_command_digest(plugin, version, &launch);
		if launch.files.unreadable().is_none() && self.approves(plugin, &digest) {
			return Ok(());
		}
		let approved_others = self.of(plugin).next().is_some();
		Err(
			PluginCommandBlocked::new(plugin.clone(), launch, digest)
				.with_other_approvals(approved_others),
		)
	}
}

/// A plugin launch the operator has not approved, or one naming a plugin
/// file that cannot be read; the process does not start.
///
/// Carries what the operator needs to recognize and approve it; the
/// environment overrides, working directory, and bound plugin files are part
/// of the digest but not of the notice.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[error("plugin `{plugin}` {kind} `{server}` would run `{}`, {}", self.command_line(), self.refusal())]
pub struct PluginCommandBlocked {
	/// Plugin identity ([`PluginId`]).
	pub plugin:          PluginId,
	/// Declaring component.
	pub kind:            PluginLaunchKind,
	/// Whether the operator approved other launches of this plugin identity:
	/// an earlier approval of this one may no longer match.
	pub approved_others: bool,
	/// Server or adapter name; for a hook, its event and matcher.
	pub server:          Str,
	/// The refused executable (empty for an argument or environment override)
	/// followed by its arguments.
	argv:                Box<[Str]>,
	/// [`plugin_command_digest`] an approval must carry.
	pub digest:          Hash32,
	/// The plugin file it names that cannot be read; such a launch cannot be
	/// approved at all.
	#[source]
	pub unreadable:      Option<Arc<PluginFileUnreadable>>,
}

impl PluginCommandBlocked {
	/// The refusal of `launch` for `plugin`, whose approval key is `digest`.
	#[must_use]
	pub fn new(plugin: PluginId, launch: PluginLaunch, digest: Hash32) -> Self {
		let PluginLaunch { kind, server, command, args, env: _, cwd: _, files } = launch;
		let argv = std::iter::once(command).chain(args).collect();
		let unreadable = files.unreadable().cloned();
		Self { plugin, kind, approved_others: false, server, argv, digest, unreadable }
	}

	/// The same refusal, noting whether the operator approved other launches
	/// of the plugin identity.
	#[must_use]
	pub const fn with_other_approvals(mut self, approved_others: bool) -> Self {
		self.approved_others = approved_others;
		self
	}

	/// Why it does not run, and what would let it, for display.
	#[must_use]
	pub const fn refusal(&self) -> Refusal<'_> {
		Refusal { blocked: self }
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
		CommandLine { command: self.command(), args: self.args(), script: self.kind.is_script() }
	}
}

/// Display form of why a [`PluginCommandBlocked`] launch does not run.
#[derive(Clone, Copy, Debug)]
pub struct Refusal<'a> {
	blocked: &'a PluginCommandBlocked,
}

impl Display for Refusal<'_> {
	fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
		let PluginCommandBlocked { plugin, digest, approved_others, unreadable, .. } = self.blocked;
		if let Some(unreadable) = unreadable {
			return write!(
				formatter,
				"but the plugin file `{}` it names cannot be read, so it cannot be approved and does \
				 not run",
				unreadable.path.display()
			);
		}
		formatter.write_str("which is not approved")?;
		if *approved_others {
			formatter.write_str(
				" (an earlier approval stops matching when the plugin, the command line, or a plugin \
				 file it runs changes)",
			)?;
		}
		write!(
			formatter,
			", so it does not run; approve it with `omp ext trust {plugin} --approve-command \
			 {digest}` and start a new session"
		)
	}
}

// Returned by value from every launch gate (`Result<(), PluginCommandBlocked>`)
// and carried in plugin diagnostic lists; keep it below clippy's 128-byte
// large-error threshold.
const _: () =
	assert!(size_of::<PluginCommandBlocked>() <= 112, "PluginCommandBlocked must stay compact");

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
		let digest = plugin_command_digest(PluginId::from_ref("p@m"), "1.0.0", &base);
		assert_eq!(digest, plugin_command_digest(PluginId::from_ref("p@m"), "1.0.0", &base));
		for (plugin, version, other) in [
			("q@m", "1.0.0", base.clone()),
			("p@m", "1.0.1", base),
			("p@m", "1.0.0", launch("/p/bin/other", &["--stdio"], &[("A", "1")])),
			("p@m", "1.0.0", launch("/p/bin/srv", &["--stdio", "--x"], &[("A", "1")])),
			("p@m", "1.0.0", launch("/p/bin/srv", &["--stdio"], &[("A", "2")])),
			("p@m", "1.0.0", launch("/p/bin/srv", &[], &[("A", "1")])),
		] {
			assert_ne!(
				digest,
				plugin_command_digest(PluginId::from_ref(plugin), version, &other),
				"{plugin} {version}"
			);
		}
		// Field boundaries are length-prefixed: moving a byte between the
		// command and an argument changes the digest.
		assert_ne!(
			plugin_command_digest(PluginId::from_ref("p@m"), "1", &launch("ab", &["c"], &[])),
			plugin_command_digest(PluginId::from_ref("p@m"), "1", &launch("a", &["bc"], &[])),
		);
	}

	#[test]
	fn digest_ignores_kind_server_and_env_order() {
		let first = launch("srv", &[], &[("A", "1"), ("B", "2")]);
		let mut second = launch("srv", &[], &[("B", "2"), ("A", "1")]);
		second.kind = PluginLaunchKind::McpServer;
		second.server = Str::new_static("other");
		assert_eq!(
			plugin_command_digest(PluginId::from_ref("p@m"), "1", &first),
			plugin_command_digest(PluginId::from_ref("p@m"), "1", &second)
		);
	}

	#[test]
	fn digest_binds_the_working_directory_and_a_hooks_trigger() {
		let base = launch("srv", &[], &[]);
		let digest = plugin_command_digest(PluginId::from_ref("p@m"), "1", &base);
		let in_dir = base.clone().with_cwd(Some(Str::new_static("/p/sub")));
		assert_ne!(digest, plugin_command_digest(PluginId::from_ref("p@m"), "1", &in_dir));
		assert_ne!(
			plugin_command_digest(PluginId::from_ref("p@m"), "1", &in_dir),
			plugin_command_digest(
				PluginId::from_ref("p@m"),
				"1",
				&base.with_cwd(Some(Str::new_static("/p")))
			)
		);
		let hook = |trigger: &'static str| {
			PluginLaunch::new(
				PluginLaunchKind::Hook,
				Str::new_static(trigger),
				Str::new_static("srv"),
				[],
				[],
			)
		};
		let pre = plugin_command_digest(PluginId::from_ref("p@m"), "1", &hook("PreToolUse Bash"));
		assert_ne!(pre, digest, "a hook never shares a server's approval");
		assert_ne!(
			pre,
			plugin_command_digest(PluginId::from_ref("p@m"), "1", &hook("PostToolUse Bash"))
		);
		assert_eq!(
			pre,
			plugin_command_digest(PluginId::from_ref("p@m"), "1", &hook("PreToolUse Bash"))
		);
	}

	#[test]
	fn approvals_admit_only_their_plugin_and_digest() {
		let approved = launch("srv", &["--stdio"], &[]);
		let digest = plugin_command_digest(PluginId::from_ref("portable"), "", &approved);
		let approvals = CommandApprovals::new([(PluginId::new_static("portable"), digest)]);
		let portable = PluginId::new_static("portable");
		assert!(approvals.admit(&portable, "", approved.clone()).is_ok());
		let other = approvals
			.admit(&PluginId::new_static("other"), "", approved.clone())
			.expect_err("an approval never crosses plugins");
		assert_eq!(other.plugin, "other");
		let bumped = approvals
			.admit(&portable, "2.0.0", approved.clone())
			.expect_err("a new version asks again");
		assert_eq!(
			bumped.digest,
			plugin_command_digest(PluginId::from_ref("portable"), "2.0.0", &approved)
		);
		assert!(
			CommandApprovals::default()
				.admit(&portable, "", approved)
				.is_err(),
			"no approvals fail closed"
		);
	}

	#[test]
	fn blocked_diagnostic_names_plugin_command_and_approval() {
		let launch = launch("/p/bin/srv", &["--stdio", "a b"], &[]);
		let digest = plugin_command_digest(PluginId::from_ref("p@m"), "1", &launch);
		let blocked = PluginCommandBlocked::new(PluginId::new_static("p@m"), launch, digest);
		let text = blocked.to_string();
		assert!(text.contains("plugin `p@m` language server `srv`"), "{text}");
		assert!(text.contains("`/p/bin/srv --stdio 'a b'`"), "{text}");
		assert!(text.contains(&format!("omp ext trust p@m --approve-command {digest}")), "{text}");

		let hook = PluginLaunch::new(
			PluginLaunchKind::Hook,
			Str::new_static("PreToolUse Bash"),
			Str::new_static("./guard.sh \"$1\""),
			[],
			[],
		);
		let digest = plugin_command_digest(PluginId::from_ref("p@m"), "1", &hook);
		let text = PluginCommandBlocked::new(PluginId::new_static("p@m"), hook, digest).to_string();
		assert!(
			text.contains("plugin `p@m` hook `PreToolUse Bash` would run `./guard.sh \"$1\"`"),
			"{text}"
		);
	}

	fn write(path: &Path, body: &str) {
		fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
		fs::write(path, body).expect("write");
	}

	/// A plugin root holding a server, a hook script, and an unrelated file.
	fn plugin_tree() -> (tempfile::TempDir, PathBuf) {
		let scratch = tempfile::tempdir().expect("scratch");
		let root = scratch.path().join("plugin");
		write(&root.join("bin/server.js"), "serve()\n");
		write(&root.join("scripts/guard.sh"), "exit 0\n");
		write(&root.join("README.md"), "docs\n");
		(scratch, root)
	}

	fn server(root: &Path, command: &str, args: &[&str]) -> PluginLaunch {
		PluginLaunch::new(
			PluginLaunchKind::McpServer,
			Str::new_static("srv"),
			Str::new(command),
			args.iter().map(|arg| Str::new(*arg)),
			[],
		)
		.with_plugin_files(root)
	}

	fn bound(launch: &PluginLaunch) -> Vec<&str> {
		launch
			.files
			.hashed()
			.iter()
			.map(|file| file.path.as_str())
			.collect()
	}

	#[test]
	fn an_installed_identity_names_the_marketplace_and_a_path_safe_directory() {
		let id = PluginId::installed("docs", "official");
		assert_eq!(id, "docs@official");
		assert_eq!(id.dir_name(), "docs-official");
		assert_eq!(PluginId::from_ref("../x@y/z").dir_name(), "---x-y-z");
		// Two marketplaces shipping the same plugin never share an approval.
		let launch = launch("srv", &[], &[]);
		assert_ne!(
			plugin_command_digest(&id, "1", &launch),
			plugin_command_digest(&PluginId::installed("docs", "mirror"), "1", &launch)
		);
	}

	#[test]
	fn script_words_split_like_a_shell_before_expansion() {
		assert_eq!(script_words(r#"node "/p/a b.js" --x='y z'&&/p/c.sh>out;echo \"q\" "e\$\x""#), [
			"node",
			"/p/a b.js",
			"--x=y z",
			"/p/c.sh",
			"out",
			"echo",
			"\"q\"",
			"e$\\x"
		]);
		assert_eq!(script_words("'/p/exec form' '/p/arg'"), ["/p/exec form", "/p/arg"]);
		assert_eq!(script_words(" '' "), [""]);
		assert!(script_words("  ").is_empty());
	}

	#[test]
	fn a_server_binds_the_plugin_files_it_names_and_no_system_binary() {
		let (_scratch, root) = plugin_tree();
		let real = fs::canonicalize(&root).expect("canonical root");
		let js = real.join("bin/server.js");
		// `/bin/sh` exists but lies outside the plugin root; `--stdio` names
		// nothing; `${CLAUDE_PLUGIN_ROOT}` expands.
		let launch = server(&root, "/bin/sh", &["${CLAUDE_PLUGIN_ROOT}/bin/server.js", "--stdio"]);
		assert_eq!(bound(&launch), ["bin/server.js"]);
		assert_eq!(
			launch.files.hashed()[0].hash,
			Hash32::sum(fs::read(&js).expect("server")),
			"bound by content"
		);
		// The command itself when it is a plugin file, a relative argument
		// against the working directory, each file once, sorted.
		let launch = server(&root, &js.to_string_lossy(), &["guard.sh", "../bin/server.js"])
			.with_cwd(Some(Str::new(real.join("scripts").to_string_lossy())))
			.with_plugin_files(&root);
		assert_eq!(bound(&launch), ["bin/server.js", "scripts/guard.sh"]);
		// A missing root names nothing.
		assert!(bound(&server(&root.join("missing"), "/bin/sh", &[])).is_empty());
	}

	#[test]
	fn editing_a_named_plugin_file_changes_the_digest_and_an_unrelated_one_does_not() {
		let (_scratch, root) = plugin_tree();
		let digest = || {
			let launch = server(&root, "node", &["${CLAUDE_PLUGIN_ROOT}/bin/server.js"]);
			plugin_command_digest(PluginId::from_ref("p@m"), "1.0.0", &launch)
		};
		let approved = digest();
		assert_eq!(approved, digest(), "deterministic");
		write(&root.join("README.md"), "other docs\n");
		assert_eq!(approved, digest(), "an unrelated plugin file is not bound");
		write(&root.join("bin/server.js"), "steal()\n");
		assert_ne!(approved, digest(), "an edited server script asks again");
	}

	#[test]
	fn a_hook_script_binds_the_plugin_paths_it_names_as_words() {
		let (_scratch, root) = plugin_tree();
		let real = fs::canonicalize(&root).expect("canonical root");
		let hook = |script: String| {
			PluginLaunch::new(
				PluginLaunchKind::Hook,
				Str::new_static("PreToolUse Bash"),
				Str::new(script),
				[],
				[],
			)
			.with_plugin_files(&root)
		};
		let braced = hook(format!(
			"node \"{}\" && \"$CLAUDE_PLUGIN_ROOT/scripts/guard.sh\" >/dev/null",
			real.join("bin/server.js").display()
		));
		assert_eq!(bound(&braced), ["bin/server.js", "scripts/guard.sh"]);
		// Exec form reaches the shell as single-quoted words.
		let exec = hook(format!("'/bin/sh' '{}'", real.join("scripts/guard.sh").display()));
		assert_eq!(bound(&exec), ["scripts/guard.sh"]);
		// Relative words resolve in the project the hook runs in, and a word
		// fragment is not a path: neither is bound.
		assert!(bound(&hook("./scripts/guard.sh --config=$CLAUDE_PLUGIN_ROOT/x".into())).is_empty());
	}

	#[test]
	fn an_unreadable_named_file_is_never_admitted() {
		use std::os::unix::fs::PermissionsExt as _;

		let (_scratch, root) = plugin_tree();
		let script = root.join("scripts/guard.sh");
		fs::set_permissions(&script, fs::Permissions::from_mode(0o000)).expect("chmod");
		if fs::read(&script).is_ok() {
			// Running with privileges that ignore file modes: nothing to prove.
			return;
		}
		let launch = server(&root, "/bin/sh", &["${CLAUDE_PLUGIN_ROOT}/scripts/guard.sh"]);
		let unreadable = launch.files.unreadable().expect("unreadable file").clone();
		assert!(unreadable.path.ends_with("scripts/guard.sh"), "{unreadable:?}");
		assert_eq!(unreadable.source.kind(), io::ErrorKind::PermissionDenied);
		// Even an approval carrying its exact digest does not admit it.
		let digest = plugin_command_digest(PluginId::from_ref("p"), "", &launch);
		let approvals = CommandApprovals::new([(PluginId::new_static("p"), digest)]);
		let blocked = approvals
			.admit(&PluginId::new_static("p"), "", launch)
			.expect_err("an unreadable plugin file fails closed");
		assert!(blocked.unreadable.is_some());
		let text = blocked.to_string();
		assert!(text.contains("cannot be read, so it cannot be approved"), "{text}");
		assert!(std::error::Error::source(&blocked).is_some(), "the io error is its source");
		fs::set_permissions(&script, fs::Permissions::from_mode(0o644)).expect("chmod back");
	}
}
