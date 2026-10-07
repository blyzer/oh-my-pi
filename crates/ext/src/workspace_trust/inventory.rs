//! The gated project inputs of a workspace and the [`InputsDigest`] over them.
//!
//! This module is the single owner of every project path a gated loader reads.
//! envd's MCP, LSP/DAP, SSH host and vault loaders, the driver's native
//! extension, prompt, secret, skill and workflow loaders, and this crate's
//! plugin resolution all join these names, so a loader cannot start reading a
//! gated input the digest does not cover.
//!
//! [`inventory`] reads every gated input of one workspace:
//!
//! - fixed files ([`gated_files`]): every project MCP file envd classes as
//!   project-scoped ([`MCP_PROJECT_FILES`]), the LSP and DAP declaration files
//!   in `.omp` and at the root, `.omp/hosts.toml`, `.omp/vaults.toml`,
//!   `.omp/secrets.yml`, and the `.omp` system, append and title prompts;
//! - the `enabledPlugins` projection of `.claude/settings.json` and
//!   `.claude/settings.local.json` ([`CLAUDE_SETTINGS_FILES`]): only the keys
//!   plugin resolution reads, so a Claude Code permission edit does not change
//!   the digest. A file that is not JSON is hashed whole;
//! - the project plugin registry file (`.omp/plugins/installed_plugins.json`)
//!   and, for every enabled install it records, the install root: one inside
//!   the repository is walked as a tree, one in the user plugin cache adds
//!   nothing (the operator materialized it; its launches stay gated by
//!   [`crate::plugin_command`]), and one anywhere else is refused
//!   ([`WorkspaceTrustError::ExternalPluginRoot`]). The `.omp/plugins` tree
//!   itself is not walked: a project-scope install links
//!   `.omp/plugins/node_modules/<name>` into the user plugin cache;
//! - the trees [`AGENT_PLUGIN_DIRS`] (native extensions and Agent Plugins
//!   packages), walked in full, `__pycache__` and dotfiles included (an
//!   unchecked-hash `.pyc` loads without its source), skipping only inert
//!   `.DS_Store`, `Thumbs.db` and nested `.git` entries;
//! - the workflow files `.omp/workflows/*.toml`.
//!
//! Files are read through [`omp_core::project_file`]: contained in the
//! repository, regular, and under a size cap. A symbolic link inside a tree is
//! followed while its canonical target stays in the repository (a cycle is
//! broken) and refused when it leaves ([`WorkspaceTrustError::Escapes`]). Any
//! refusal or [`InventoryBudget`] overrun fails closed: no digest is produced.
//!
//! The digest is SHA-256 over [`INVENTORY_DOMAIN`], the entry count (`u64`
//! little-endian), then every entry in path-byte order: the length of its
//! `/`-joined relative path (`u64` little-endian), that path, its
//! [`InputKind`] tag, and its 32-byte content hash (zero for
//! [`InputKind::Absent`]). An absent file and an empty one differ; an empty
//! tree and an absent one do not.

use std::{
	collections::BTreeMap,
	ffi::OsString,
	fs, io,
	iter::FusedIterator,
	path::{Component, Path, PathBuf},
};

use omp_core::{
	Hash32, Str,
	project_file::{self, Containment, containment_root},
};
use strum::{Display, IntoStaticStr};

use super::{InputsDigest, WorkspaceTrustError, canonical};
use crate::{
	claude_plugin::{
		InstalledPluginsRegistry, REGISTRY_FILE, enabled_plugin_overrides, plugin_cache_dir,
		split_plugin_id, user_plugins_dir,
	},
	plugin_command::PluginId,
};

/// Domain separator of the inputs digest. Bumping the version asks about
/// every workspace once more.
pub const INVENTORY_DOMAIN: &[u8] = b"omp.workspace-trust.inputs.v1\0";

/// The project's own configuration directory.
pub const PROJECT_DIR: &str = ".omp";

/// Native MCP configuration: `<project>/.omp/mcp.json`, and the user's
/// `<config root>/mcp.json`.
pub const MCP_FILE: &str = "mcp.json";
/// Project-root MCP fallback.
pub const ROOT_MCP_FILE: &str = ".mcp.json";
/// Claude Code project MCP configuration.
pub const CLAUDE_MCP_FILE: &str = ".claude/.mcp.json";
/// `OpenAI` Codex project configuration.
pub const CODEX_CONFIG_FILE: &str = ".codex/config.toml";
/// Gemini CLI project settings.
pub const GEMINI_SETTINGS_FILE: &str = ".gemini/settings.json";
/// `OpenCode` project configuration, highest precedence first.
pub const OPENCODE_CONFIG_FILES: [&str; 4] =
	[".opencode/opencode.jsonc", ".opencode/opencode.json", "opencode.jsonc", "opencode.json"];
/// Cursor project MCP configuration.
pub const CURSOR_MCP_FILE: &str = ".cursor/mcp.json";
/// Windsurf project MCP configuration.
pub const WINDSURF_MCP_FILE: &str = ".windsurf/mcp_config.json";
/// VS Code project MCP configuration.
pub const VSCODE_MCP_FILE: &str = ".vscode/mcp.json";
/// Lowest-precedence standalone project MCP files.
pub const STANDALONE_MCP_FILES: [&str; 2] = ["mcp.json", "mcp.config.json"];

/// Every project MCP file envd's discovery reads: the native pair, then every
/// foreign kind it classes as project-scoped, in discovery order.
pub const MCP_PROJECT_FILES: [GatedPath; 14] = [
	GatedPath::new(PROJECT_DIR, MCP_FILE),
	GatedPath::root(ROOT_MCP_FILE),
	GatedPath::root(CLAUDE_MCP_FILE),
	GatedPath::root(CODEX_CONFIG_FILE),
	GatedPath::root(GEMINI_SETTINGS_FILE),
	GatedPath::root(OPENCODE_CONFIG_FILES[0]),
	GatedPath::root(OPENCODE_CONFIG_FILES[1]),
	GatedPath::root(OPENCODE_CONFIG_FILES[2]),
	GatedPath::root(OPENCODE_CONFIG_FILES[3]),
	GatedPath::root(CURSOR_MCP_FILE),
	GatedPath::root(WINDSURF_MCP_FILE),
	GatedPath::root(VSCODE_MCP_FILE),
	GatedPath::root(STANDALONE_MCP_FILES[0]),
	GatedPath::root(STANDALONE_MCP_FILES[1]),
];

/// Language-server declaration files, lowest precedence first: read in
/// `<project>/.omp`, at the project root, in the user configuration root,
/// and at every plugin root.
pub const LSP_CONFIG_NAMES: [&str; 6] =
	["lsp.json", ".lsp.json", "lsp.yaml", ".lsp.yaml", "lsp.yml", ".lsp.yml"];
/// Debug-adapter declaration files, lowest precedence first: read where the
/// [`LSP_CONFIG_NAMES`] are.
pub const DAP_CONFIG_NAMES: [&str; 6] =
	["dap.json", ".dap.json", "dap.yaml", ".dap.yaml", "dap.yml", ".dap.yml"];

/// SSH host aliases: `<project>/.omp/hosts.toml` and the user's
/// `<config root>/hosts.toml`.
pub const HOSTS_FILE: &str = "hosts.toml";
/// Obsidian vault roots: `<project>/.omp/vaults.toml` and the user's
/// `<config root>/vaults.toml`.
pub const VAULTS_FILE: &str = "vaults.toml";
/// Secret redaction rules: `<project>/.omp/secrets.yml` and the user's.
pub const SECRETS_FILE: &str = "secrets.yml";
/// System prompt override: `<project>/.omp/SYSTEM.md` and the user's.
pub const SYSTEM_PROMPT_FILE: &str = "SYSTEM.md";
/// System prompt suffix: `<project>/.omp/APPEND_SYSTEM.md` and the user's.
pub const APPEND_SYSTEM_PROMPT_FILE: &str = "APPEND_SYSTEM.md";
/// Title-generation prompt: `<project>/.omp/TITLE_SYSTEM.md` and the user's.
pub const TITLE_SYSTEM_PROMPT_FILE: &str = "TITLE_SYSTEM.md";

/// The gated files directly inside [`PROJECT_DIR`].
const PROJECT_DIR_FILES: [&str; 6] = [
	HOSTS_FILE,
	VAULTS_FILE,
	SECRETS_FILE,
	SYSTEM_PROMPT_FILE,
	APPEND_SYSTEM_PROMPT_FILE,
	TITLE_SYSTEM_PROMPT_FILE,
];

/// Claude Code's project settings, lowest precedence first; only their
/// `enabledPlugins` are gated.
pub const CLAUDE_SETTINGS_FILES: [&str; 2] =
	[".claude/settings.json", ".claude/settings.local.json"];

/// The project-scope plugin directory holding the plugin registry.
pub const PROJECT_PLUGINS_DIR: &str = ".omp/plugins";

/// Native extension packages, also scanned for Agent Plugins packages.
pub const EXTENSIONS_DIR: &str = ".omp/extensions";
/// Every project directory whose packages load as native extensions or
/// Agent Plugins packages (MCP servers, skills).
pub const AGENT_PLUGIN_DIRS: [&str; 3] = [EXTENSIONS_DIR, ".agent/plugins", ".agents/plugins"];

/// Project workflow definitions, `<name>.toml` each.
pub const WORKFLOWS_DIR: &str = ".omp/workflows";
/// The extension of a workflow definition file.
pub const WORKFLOW_EXTENSION: &str = "toml";

/// Entry names a tree walk skips: inert OS metadata no loader reads, and
/// nested repositories.
const SKIPPED_NAMES: [&str; 3] = [".DS_Store", "Thumbs.db", ".git"];

/// A gated input's location under the workspace root: a directory (empty for
/// the root) and a name, either of which may hold `/`-separated components.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct GatedPath {
	dir:  &'static str,
	name: &'static str,
}

impl GatedPath {
	/// `name` inside the workspace directory `dir`.
	#[inline]
	pub const fn new(dir: &'static str, name: &'static str) -> Self {
		Self { dir, name }
	}

	/// `name` at the workspace root.
	#[inline]
	pub const fn root(name: &'static str) -> Self {
		Self { dir: "", name }
	}

	/// The directory under the workspace root; empty for the root.
	#[inline]
	pub const fn dir(&self) -> &'static str {
		self.dir
	}

	/// The name inside [`Self::dir`].
	#[inline]
	pub const fn name(&self) -> &'static str {
		self.name
	}

	/// The input's path under `root`.
	pub fn under(&self, root: &Path) -> PathBuf {
		if self.dir.is_empty() {
			root.join(self.name)
		} else {
			root.join(self.dir).join(self.name)
		}
	}

	/// The input's path relative to the workspace root.
	pub fn relative(&self) -> PathBuf {
		self.under(Path::new(""))
	}
}

/// The LSP declaration files a workspace supplies: [`LSP_CONFIG_NAMES`] in
/// [`PROJECT_DIR`], then at the root.
pub fn lsp_config_files() -> impl FusedIterator<Item = GatedPath> + Clone {
	config_files(&LSP_CONFIG_NAMES)
}

/// The DAP declaration files a workspace supplies: [`DAP_CONFIG_NAMES`] in
/// [`PROJECT_DIR`], then at the root.
pub fn dap_config_files() -> impl FusedIterator<Item = GatedPath> + Clone {
	config_files(&DAP_CONFIG_NAMES)
}

fn config_files(names: &'static [&'static str]) -> impl FusedIterator<Item = GatedPath> + Clone {
	[PROJECT_DIR, ""]
		.into_iter()
		.flat_map(move |dir| names.iter().map(move |name| GatedPath::new(dir, name)))
}

/// Every fixed gated file: [`MCP_PROJECT_FILES`], the LSP and DAP
/// declaration files, and the hosts, vaults, secrets and prompt files in
/// [`PROJECT_DIR`]. Each is hashed whole, or recorded absent.
pub fn gated_files() -> impl FusedIterator<Item = GatedPath> + Clone {
	MCP_PROJECT_FILES
		.into_iter()
		.chain(lsp_config_files())
		.chain(dap_config_files())
		.chain(
			PROJECT_DIR_FILES
				.into_iter()
				.map(|name| GatedPath::new(PROJECT_DIR, name)),
		)
}

/// Bounds on what [`inventory`] reads. Any overrun fails closed with
/// [`WorkspaceTrustError::Budget`] or a [`project_file::Refusal::TooLarge`]
/// refusal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InventoryBudget {
	/// Largest fixed gated file, Claude settings file, plugin registry, or
	/// workflow file. At least every gated loader's own cap.
	pub file_bytes:      u64,
	/// Largest single file inside a gated tree.
	pub tree_file_bytes: u64,
	/// Most files across every gated tree and the workflow directory.
	pub tree_files:      usize,
	/// Most bytes across every gated tree and the workflow directory.
	pub tree_bytes:      u64,
	/// Deepest directory nesting below a tree root (the root is depth 0).
	pub depth:           usize,
}

impl Default for InventoryBudget {
	fn default() -> Self {
		Self {
			file_bytes:      4 * 1024 * 1024,
			tree_file_bytes: 32 * 1024 * 1024,
			tree_files:      8192,
			tree_bytes:      256 * 1024 * 1024,
			depth:           32,
		}
	}
}

/// Which [`InventoryBudget`] bound a workspace overran.
#[derive(Clone, Copy, Debug, Display, Eq, IntoStaticStr, PartialEq)]
pub enum BudgetLimit {
	/// [`InventoryBudget::tree_files`].
	#[strum(to_string = "file count")]
	Files,
	/// [`InventoryBudget::tree_bytes`].
	#[strum(to_string = "byte")]
	Bytes,
	/// [`InventoryBudget::depth`].
	#[strum(to_string = "directory depth")]
	Depth,
}

/// What an [`InputEntry`] records. The discriminant is the tag byte of the
/// digest encoding, so it never changes within one [`INVENTORY_DOMAIN`].
#[derive(Clone, Copy, Debug, Eq, Hash, IntoStaticStr, Ord, PartialEq, PartialOrd)]
#[strum(serialize_all = "snake_case")]
#[repr(u8)]
pub enum InputKind {
	/// A fixed gated file that does not exist; its hash is 32 zero bytes.
	Absent         = 0,
	/// A gated file's whole content.
	Bytes          = 1,
	/// A Claude Code settings file's `enabledPlugins` projection: the
	/// `u64`-counted, sorted `(u64-length-prefixed id, 0|1)` pairs.
	EnabledPlugins = 2,
	/// A non-executable file inside a gated tree.
	TreeFile       = 3,
	/// A file inside a gated tree with any execute bit set.
	TreeExecutable = 4,
}

impl InputKind {
	/// The tag byte the digest encodes.
	#[inline]
	pub const fn tag(self) -> u8 {
		self as u8
	}
}

/// One gated input of an [`InputsInventory`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InputEntry {
	path: Box<[u8]>,
	kind: InputKind,
	hash: Option<Hash32>,
}

impl InputEntry {
	/// The path relative to the workspace root, its components joined with
	/// `/` (`..` components lead to a plugin root elsewhere in the
	/// repository), in the platform's encoded bytes.
	#[inline]
	pub fn path(&self) -> &[u8] {
		&self.path
	}

	/// What the entry records.
	#[inline]
	pub const fn kind(&self) -> InputKind {
		self.kind
	}

	/// The content hash; `None` for [`InputKind::Absent`].
	#[inline]
	pub const fn hash(&self) -> Option<&Hash32> {
		self.hash.as_ref()
	}
}

/// Every gated input of one workspace and their digest.
#[derive(Clone, Debug)]
pub struct InputsInventory {
	entries: Box<[InputEntry]>,
	digest:  InputsDigest,
}

impl InputsInventory {
	/// The digest a [`super::TrustBinding`] binds the workspace to.
	#[inline]
	pub const fn digest(&self) -> &InputsDigest {
		&self.digest
	}

	/// Every entry in digest order (path bytes, then kind).
	pub fn entries(
		&self,
	) -> impl ExactSizeIterator<Item = &InputEntry> + DoubleEndedIterator + FusedIterator + Clone + '_
	{
		self.entries.iter()
	}

	/// The entries of inputs that exist, in digest order: what an untrusted
	/// workspace withholds.
	pub fn present(
		&self,
	) -> impl DoubleEndedIterator<Item = &InputEntry> + FusedIterator + Clone + '_ {
		self
			.entries
			.iter()
			.filter(|entry| entry.kind != InputKind::Absent)
	}
}

/// Reads every gated input of the workspace at `workspace` and digests them.
///
/// `data_dir` locates the user plugin cache
/// ([`crate::claude_plugin::plugin_cache_dir`]): an enabled project plugin
/// installed there adds nothing to the digest. A relative install path
/// resolves against the process working directory, exactly as plugin
/// resolution opens it.
///
/// # Errors
///
/// [`WorkspaceTrustError::Canonicalize`] when `workspace` cannot be
/// canonicalized; [`WorkspaceTrustError::Input`] when a gated file is refused
/// (outside the repository, not a regular file, over its cap) or unreadable;
/// [`WorkspaceTrustError::Read`] when a gated directory cannot be listed;
/// [`WorkspaceTrustError::Escapes`] when a tree entry or a gated directory
/// resolves outside the repository; [`WorkspaceTrustError::ExternalPluginRoot`]
/// for an enabled project plugin installed outside the repository and the
/// user plugin cache; [`WorkspaceTrustError::Budget`] when `budget` is
/// overrun.
pub fn inventory(
	workspace: &Path,
	data_dir: &Path,
	budget: &InventoryBudget,
) -> Result<InputsInventory, WorkspaceTrustError> {
	let workspace = canonical(workspace)?;
	let containment = containment_root(&workspace);
	let cache = fs::canonicalize(plugin_cache_dir(&user_plugins_dir(data_dir))).ok();
	let mut walk =
		Walk { workspace: &workspace, containment, budget, files: 0, bytes: 0, entries: Vec::new() };
	for gated in gated_files() {
		walk.file(&gated.relative())?;
	}
	for settings in CLAUDE_SETTINGS_FILES {
		walk.claude_settings(Path::new(settings))?;
	}
	walk.plugin_registry(cache.as_deref())?;
	for tree in AGENT_PLUGIN_DIRS {
		walk.tree_at(Path::new(tree))?;
	}
	walk.workflows()?;
	Ok(walk.finish())
}

/// One inventory in progress.
struct Walk<'a> {
	/// Canonical workspace root; entry paths are relative to it.
	workspace:   &'a Path,
	/// Canonical repository root every read must stay inside.
	containment: &'a Path,
	budget:      &'a InventoryBudget,
	/// Tree and workflow files read so far.
	files:       usize,
	/// Tree and workflow bytes read so far.
	bytes:       u64,
	entries:     Vec<InputEntry>,
}

/// What a directory entry resolves to.
enum Resolved {
	/// Absent (a dangling link) or inert: contributes nothing.
	Skip,
	/// A directory, at its canonical path.
	Directory(PathBuf),
	/// Anything else: read through the contained reader, which refuses a
	/// special file.
	File,
}

impl Walk<'_> {
	fn push(&mut self, relative: &Path, kind: InputKind, hash: Option<Hash32>) {
		self
			.entries
			.push(InputEntry { path: path_key(relative), kind, hash });
	}

	fn read(&self, path: &Path, limit: u64) -> Result<Option<Vec<u8>>, WorkspaceTrustError> {
		Ok(project_file::read_bytes(path, Containment::Within(self.containment), limit)?)
	}

	/// A fixed gated file, whole.
	fn file(&mut self, relative: &Path) -> Result<(), WorkspaceTrustError> {
		match self.read(&self.workspace.join(relative), self.budget.file_bytes)? {
			Some(bytes) => self.push(relative, InputKind::Bytes, Some(Hash32::sum(bytes))),
			None => self.push(relative, InputKind::Absent, None),
		}
		Ok(())
	}

	/// A Claude Code settings file's `enabledPlugins` projection, or its
	/// bytes when it is not JSON.
	fn claude_settings(&mut self, relative: &Path) -> Result<(), WorkspaceTrustError> {
		match self.read(&self.workspace.join(relative), self.budget.file_bytes)? {
			Some(bytes) => match enabled_plugin_overrides(&bytes) {
				Ok(overrides) => {
					self.push(relative, InputKind::EnabledPlugins, Some(projection_hash(&overrides)));
				},
				Err(_) => self.push(relative, InputKind::Bytes, Some(Hash32::sum(bytes))),
			},
			None => self.push(relative, InputKind::Absent, None),
		}
		Ok(())
	}

	/// The project plugin registry file, then the root of every enabled
	/// install it records that plugin resolution would open.
	fn plugin_registry(&mut self, cache: Option<&Path>) -> Result<(), WorkspaceTrustError> {
		let relative = Path::new(PROJECT_PLUGINS_DIR).join(REGISTRY_FILE);
		let path = self.workspace.join(&relative);
		let Some(bytes) = self.read(&path, self.budget.file_bytes)? else {
			self.push(&relative, InputKind::Absent, None);
			return Ok(());
		};
		self.push(&relative, InputKind::Bytes, Some(Hash32::sum(&bytes)));
		// A registry plugin resolution rejects loads nothing.
		let Ok(registry) = InstalledPluginsRegistry::from_slice(&path, &bytes) else {
			return Ok(());
		};
		for (id, entries) in &registry.plugins {
			if split_plugin_id(id).is_none() {
				continue;
			}
			for entry in entries.iter().filter(|entry| entry.enabled) {
				// What plugin resolution opens: a missing root or a file
				// loads nothing.
				let Ok(root) = fs::canonicalize(&entry.install_path) else {
					continue;
				};
				if !root.is_dir() {
					continue;
				}
				if root.starts_with(self.containment) {
					let mut relative = relative_to(self.workspace, &root);
					self.directory(&root, &mut relative, &mut vec![root.clone()], 0)?;
				} else if !cache.is_some_and(|cache| root.starts_with(cache)) {
					return Err(WorkspaceTrustError::ExternalPluginRoot {
						plugin: PluginId::from(id.clone()),
						path:   root,
					});
				}
			}
		}
		Ok(())
	}

	/// The gated tree at `relative`; absent or not a directory, nothing.
	fn tree_at(&mut self, relative: &Path) -> Result<(), WorkspaceTrustError> {
		let Some(root) = self.gated_directory(relative)? else {
			return Ok(());
		};
		self.directory(&root, &mut relative.to_path_buf(), &mut vec![root.clone()], 0)
	}

	/// The canonical directory at `relative`: `None` when absent or not a
	/// directory.
	fn gated_directory(&self, relative: &Path) -> Result<Option<PathBuf>, WorkspaceTrustError> {
		let path = self.workspace.join(relative);
		match self.resolve(&path)? {
			Resolved::Directory(root) => Ok(Some(root)),
			Resolved::Skip | Resolved::File => Ok(None),
		}
	}

	/// Resolves `path`, following symbolic links (in any component) only while
	/// the canonical target stays in the repository; a dangling link is
	/// absent.
	fn resolve(&self, path: &Path) -> Result<Resolved, WorkspaceTrustError> {
		let read = |source| WorkspaceTrustError::Read { path: path.to_path_buf(), source };
		let target = match fs::canonicalize(path) {
			Ok(target) => target,
			Err(error) if absent(&error) => return Ok(Resolved::Skip),
			Err(source) => return Err(read(source)),
		};
		if !target.starts_with(self.containment) {
			return Err(WorkspaceTrustError::Escapes { path: path.to_path_buf() });
		}
		Ok(if fs::metadata(&target).map_err(read)?.is_dir() {
			Resolved::Directory(target)
		} else {
			Resolved::File
		})
	}

	/// Every entry below the canonical directory `dir`, named under
	/// `relative`. `stack` holds the canonical directories being walked, so a
	/// link back to one of them is a cycle and is not followed.
	fn directory(
		&mut self,
		dir: &Path,
		relative: &mut PathBuf,
		stack: &mut Vec<PathBuf>,
		depth: usize,
	) -> Result<(), WorkspaceTrustError> {
		if depth > self.budget.depth {
			return Err(WorkspaceTrustError::Budget {
				path:  dir.to_path_buf(),
				limit: BudgetLimit::Depth,
			});
		}
		for name in sorted_names(dir)? {
			if SKIPPED_NAMES.iter().any(|skipped| name == *skipped) {
				continue;
			}
			let path = dir.join(&name);
			relative.push(&name);
			let walked = match self.resolve(&path)? {
				Resolved::Skip => Ok(()),
				Resolved::Directory(target) if stack.contains(&target) => Ok(()),
				Resolved::Directory(target) => {
					stack.push(target.clone());
					let walked = self.directory(&target, relative, stack, depth + 1);
					stack.pop();
					walked
				},
				Resolved::File => self.tree_file(&path, relative),
			};
			relative.pop();
			walked?;
		}
		Ok(())
	}

	/// Counts one tree or workflow file against the budget and reads it.
	fn budgeted(&mut self, path: &Path, limit: u64) -> Result<Option<Vec<u8>>, WorkspaceTrustError> {
		self.files += 1;
		if self.files > self.budget.tree_files {
			return Err(WorkspaceTrustError::Budget {
				path:  path.to_path_buf(),
				limit: BudgetLimit::Files,
			});
		}
		let Some(bytes) = self.read(path, limit)? else {
			return Ok(None);
		};
		self.bytes = self.bytes.saturating_add(bytes.len() as u64);
		if self.bytes > self.budget.tree_bytes {
			return Err(WorkspaceTrustError::Budget {
				path:  path.to_path_buf(),
				limit: BudgetLimit::Bytes,
			});
		}
		Ok(Some(bytes))
	}

	/// One file inside a gated tree, with its execute bit.
	fn tree_file(&mut self, path: &Path, relative: &Path) -> Result<(), WorkspaceTrustError> {
		let Some(bytes) = self.budgeted(path, self.budget.tree_file_bytes)? else {
			return Ok(());
		};
		let kind = if executable(path)? {
			InputKind::TreeExecutable
		} else {
			InputKind::TreeFile
		};
		self.push(relative, kind, Some(Hash32::sum(bytes)));
		Ok(())
	}

	/// Every `.omp/workflows/*.toml` file, whole.
	fn workflows(&mut self) -> Result<(), WorkspaceTrustError> {
		let relative = Path::new(WORKFLOWS_DIR);
		let Some(dir) = self.gated_directory(relative)? else {
			return Ok(());
		};
		for name in sorted_names(&dir)? {
			if Path::new(&name)
				.extension()
				.is_none_or(|extension| extension != WORKFLOW_EXTENSION)
			{
				continue;
			}
			let path = dir.join(&name);
			if !matches!(self.resolve(&path)?, Resolved::File) {
				continue;
			}
			if let Some(bytes) = self.budgeted(&path, self.budget.file_bytes)? {
				self.push(&relative.join(&name), InputKind::Bytes, Some(Hash32::sum(bytes)));
			}
		}
		Ok(())
	}

	fn finish(mut self) -> InputsInventory {
		self
			.entries
			.sort_unstable_by(|left, right| (&left.path, left.kind).cmp(&(&right.path, right.kind)));
		// A plugin root inside a gated tree is walked twice.
		self
			.entries
			.dedup_by(|right, left| left.path == right.path && left.kind == right.kind);
		let digest = digest(&self.entries);
		InputsInventory { entries: self.entries.into_boxed_slice(), digest }
	}
}

/// The names in `dir`, sorted, so a budget overrun is deterministic.
fn sorted_names(dir: &Path) -> Result<Vec<OsString>, WorkspaceTrustError> {
	let read = |source| WorkspaceTrustError::Read { path: dir.to_path_buf(), source };
	let mut names = fs::read_dir(dir)
		.map_err(read)?
		.map(|entry| entry.map(|entry| entry.file_name()))
		.collect::<Result<Vec<_>, _>>()
		.map_err(read)?;
	names.sort_unstable();
	Ok(names)
}

fn absent(error: &io::Error) -> bool {
	matches!(error.kind(), io::ErrorKind::NotFound | io::ErrorKind::NotADirectory)
}

#[cfg(unix)]
fn executable(path: &Path) -> Result<bool, WorkspaceTrustError> {
	use std::os::unix::fs::PermissionsExt as _;

	let metadata = fs::metadata(path)
		.map_err(|source| WorkspaceTrustError::Read { path: path.to_path_buf(), source })?;
	Ok(metadata.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
#[allow(clippy::unnecessary_wraps, reason = "same signature as the unix execute-bit probe")]
fn executable(_: &Path) -> Result<bool, WorkspaceTrustError> {
	Ok(false)
}

/// `relative`'s components joined with `/`, whatever the platform separator.
fn path_key(relative: &Path) -> Box<[u8]> {
	let mut key = Vec::with_capacity(relative.as_os_str().len());
	for (index, component) in relative.components().enumerate() {
		if index > 0 {
			key.push(b'/');
		}
		key.extend_from_slice(component.as_os_str().as_encoded_bytes());
	}
	key.into_boxed_slice()
}

/// `target` relative to `base`, both canonical: `..` for every component of
/// `base` below their common ancestor.
fn relative_to(base: &Path, target: &Path) -> PathBuf {
	let common = base
		.components()
		.zip(target.components())
		.take_while(|(left, right)| left == right)
		.count();
	base
		.components()
		.skip(common)
		.map(|_| Component::ParentDir)
		.chain(target.components().skip(common))
		.collect()
}

/// The hash of one settings file's `enabledPlugins` projection.
fn projection_hash(overrides: &BTreeMap<Str, bool>) -> Hash32 {
	let mut hasher = Hash32::hasher();
	hasher.update((overrides.len() as u64).to_le_bytes());
	for (id, enabled) in overrides {
		hasher
			.update((id.len() as u64).to_le_bytes())
			.update(id.as_bytes())
			.update([u8::from(*enabled)]);
	}
	hasher.finalize()
}

/// The digest of `entries`, already in digest order.
fn digest(entries: &[InputEntry]) -> InputsDigest {
	let mut hasher = Hash32::hasher();
	hasher
		.update(INVENTORY_DOMAIN)
		.update((entries.len() as u64).to_le_bytes());
	for entry in entries {
		hasher
			.update((entry.path.len() as u64).to_le_bytes())
			.update(&entry.path)
			.update([entry.kind.tag()])
			.update(entry.hash.map_or([0; 32], Hash32::into_bytes));
	}
	InputsDigest::new(hasher.finalize())
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn keys_join_components_with_a_slash() {
		assert_eq!(&*path_key(Path::new(".omp/extensions/a/b.py")), b".omp/extensions/a/b.py");
		assert_eq!(&*path_key(&GatedPath::root(CLAUDE_MCP_FILE).relative()), b".claude/.mcp.json");
		assert_eq!(&*path_key(&GatedPath::new(PROJECT_DIR, MCP_FILE).relative()), b".omp/mcp.json");
	}

	#[test]
	fn relative_paths_climb_out_of_a_nested_workspace() {
		assert_eq!(relative_to(Path::new("/r/w"), Path::new("/r/w/a/b")), Path::new("a/b"));
		assert_eq!(relative_to(Path::new("/r/w/x"), Path::new("/r/v/p")), Path::new("../../v/p"));
		assert_eq!(relative_to(Path::new("/r/w"), Path::new("/r/w")), Path::new(""));
		assert_eq!(&*path_key(Path::new("../../v/p/f")), b"../../v/p/f");
	}

	#[test]
	fn projection_hashes_only_the_sorted_pairs() {
		let mut overrides = BTreeMap::new();
		overrides.insert(Str::new_static("b@m"), false);
		overrides.insert(Str::new_static("a@m"), true);
		let mut expected = Hash32::hasher();
		expected
			.update(2_u64.to_le_bytes())
			.update(3_u64.to_le_bytes())
			.update(b"a@m")
			.update([1])
			.update(3_u64.to_le_bytes())
			.update(b"b@m")
			.update([0]);
		assert_eq!(projection_hash(&overrides), expected.finalize());
	}
}
