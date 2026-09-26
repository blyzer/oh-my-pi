//! Where a v1 install keeps each item, resolved exactly as v1's
//! `packages/utils/src/dirs.ts` does, and where each v2 profile receives it.
//!
//! v1 layout rules (`dirs.ts` on `main`):
//!
//! - The profile-independent root is `$HOME/<PI_CONFIG_DIR or .omp>`
//!   (`getBaseConfigRoot`); a named profile's root is
//!   `<root>/profiles/<profile>`, selected by `OMP_PROFILE`, else `PI_PROFILE`
//!   (an explicitly empty `OMP_PROFILE` selects the default).
//! - The agent directory is `<profile root>/agent`. `PI_CODING_AGENT_DIR`
//!   replaces it for the default profile only, and is ignored when it equals
//!   the agent directory `PI_PROFILE` derives (a value a parent's `setProfile`
//!   propagated).
//! - On Linux and macOS, when the agent directory is the default one, each XDG
//!   purpose relocates once `$XDG_<PURPOSE>_HOME/omp` exists
//!   (`<…>/omp/profiles/<profile>` for a named profile): config-root items move
//!   under it, and agent items move under it with the `agent/` prefix
//!   flattened. Configuration files (`config.yml`, `models.yml`, …) never
//!   relocate.
//! - `install-id` always lives in the profile-independent root.
//!
//! The `PI_*` variables are read here and nowhere else in v2: they only find v1
//! data.

use std::{
	env,
	ffi::OsString,
	fs, io,
	path::{Component, Path, PathBuf},
};

use omp_core::{Str, dirs::ProfileNameError};
use strum::{Display, EnumIter, IntoStaticStr};
use thiserror::Error;

/// v1's default configuration directory name under the owner's home.
pub const V1_CONFIG_DIR_NAME: &str = ".omp";

/// v1's application directory name under each XDG base.
const V1_APP_NAME: &str = "omp";

/// XDG purpose a v1 path relocates under.
#[derive(
	Clone, Copy, Debug, Display, EnumIter, Eq, Hash, IntoStaticStr, Ord, PartialEq, PartialOrd,
)]
#[strum(serialize_all = "kebab-case")]
pub enum XdgCategory {
	/// `$XDG_DATA_HOME`: durable data (sessions, databases, plugins).
	Data,
	/// `$XDG_STATE_HOME`: runtime state (memories, keys, logs).
	State,
	/// `$XDG_CACHE_HOME`: re-creatable caches.
	Cache,
}

impl XdgCategory {
	const fn index(self) -> usize {
		self as usize
	}
}

/// Whether a v1 item is one file or a directory tree.
#[derive(Clone, Copy, Debug, Display, Eq, IntoStaticStr, PartialEq)]
#[strum(serialize_all = "kebab-case")]
pub enum ItemShape {
	/// A regular file.
	File,
	/// A directory tree.
	Directory,
}

/// One v1 item the migrator knows how to find.
///
/// The kebab-case name is the item's stable identity in reports.
#[derive(
	Clone, Copy, Debug, Display, EnumIter, Eq, Hash, IntoStaticStr, Ord, PartialEq, PartialOrd,
)]
#[strum(serialize_all = "kebab-case")]
pub enum V1Item {
	/// Main settings: `agent/config.yml`, else `agent/config.yaml`.
	Settings,
	/// Model configuration: `agent/models.yml`, `models.yaml`, or `models.json`.
	Models,
	/// Settings and auth database `agent.db` (XDG data).
	AgentDb,
	/// Session transcripts `sessions/` (XDG data).
	Sessions,
	/// Content-addressed blob store `blobs/` (XDG data).
	Blobs,
	/// Prompt history database `history.db` (XDG data).
	HistoryDb,
	/// Key bindings: `agent/keybindings.yml`, `.yaml`, or `.json`.
	Keybindings,
	/// User skills `agent/skills/`.
	Skills,
	/// Autolearn-managed skills `agent/managed-skills/`.
	ManagedSkills,
	/// User rules `agent/rules/`.
	Rules,
	/// Sticky user rule `agent/RULES.md`.
	RulesMd,
	/// User context file `agent/AGENTS.md`.
	AgentsMd,
	/// Prompt templates `agent/prompts/`.
	Prompts,
	/// Custom themes `agent/themes/`.
	Themes,
	/// MCP servers: `agent/mcp.json`, else `agent/.mcp.json`.
	Mcp,
	/// SSH hosts `agent/ssh.json`.
	Ssh,
	/// LSP servers: `agent/lsp.json`, `.lsp.json`, `lsp.yaml`, `.lsp.yaml`,
	/// `lsp.yml`, or `.lsp.yml`.
	Lsp,
	/// DAP adapters, with the same six spellings as [`V1Item::Lsp`].
	Dap,
	/// Secret redaction rules `agent/secrets.yml`.
	Secrets,
	/// Transcript placeholder key `secret-placeholder.key` (XDG state; v1 adopts
	/// the unrelocated `agent/` copy).
	SecretPlaceholderKey,
	/// Per-install identity `install-id` in the profile-independent root.
	InstallId,
	/// Mnemopi memory store `memories/mnemopi/` (XDG state).
	MnemopiMemory,
	/// Claude-format marketplace registry `marketplaces.json` (XDG data; v1
	/// adopts the unrelocated root copy).
	Marketplaces,
	/// Installed plugins and their cache `plugins/` (XDG data).
	Plugins,
	/// User system prompt `agent/SYSTEM.md`.
	SystemMd,
	/// User system-prompt suffix `agent/APPEND_SYSTEM.md`.
	AppendSystemMd,
	/// User title-generation prompt `agent/TITLE_SYSTEM.md`.
	TitleSystemMd,
	/// Slash commands `agent/commands/`.
	Commands,
	/// Custom agents `agent/agents/`.
	Agents,
	/// Memory root `memories/` (XDG state): the `local` backend's per-project
	/// `--<encoded cwd>--/learned.md` lessons, beside `mnemopi/`.
	Memories,
	/// Usage statistics database `stats.db` (XDG data).
	StatsDb,
}

/// Base a v1 item hangs off.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Anchor {
	/// The agent directory; never XDG-relocated.
	Agent,
	/// The agent directory, flattened under the XDG root of that purpose when
	/// relocated.
	AgentXdg(XdgCategory),
	/// The profile root, under the XDG root of that purpose when relocated.
	RootXdg(XdgCategory),
	/// The profile-independent root.
	BaseRoot,
}

/// Static facts locating one [`V1Item`].
struct ItemSpec {
	anchor:        Anchor,
	/// Candidate names relative to the anchor, in v1's lookup order.
	names:         &'static [&'static str],
	shape:         ItemShape,
	/// Whether v1 adopts the unrelocated copy when the XDG location lacks it
	/// (`adoptLegacyFile` in `dirs.ts`).
	adopts_legacy: bool,
}

impl V1Item {
	/// Whether this item is one file or a directory tree.
	#[must_use]
	pub fn shape(self) -> ItemShape {
		self.spec().shape
	}

	fn spec(self) -> ItemSpec {
		const LSP: &[&str] =
			&["lsp.json", ".lsp.json", "lsp.yaml", ".lsp.yaml", "lsp.yml", ".lsp.yml"];
		const DAP: &[&str] =
			&["dap.json", ".dap.json", "dap.yaml", ".dap.yaml", "dap.yml", ".dap.yml"];
		let (anchor, names, shape) = match self {
			Self::Settings => (Anchor::Agent, &["config.yml", "config.yaml"][..], ItemShape::File),
			Self::Models => {
				(Anchor::Agent, &["models.yml", "models.yaml", "models.json"][..], ItemShape::File)
			},
			Self::AgentDb => (Anchor::AgentXdg(XdgCategory::Data), &["agent.db"][..], ItemShape::File),
			Self::Sessions => {
				(Anchor::AgentXdg(XdgCategory::Data), &["sessions"][..], ItemShape::Directory)
			},
			Self::Blobs => (Anchor::AgentXdg(XdgCategory::Data), &["blobs"][..], ItemShape::Directory),
			Self::HistoryDb => {
				(Anchor::AgentXdg(XdgCategory::Data), &["history.db"][..], ItemShape::File)
			},
			Self::Keybindings => (
				Anchor::Agent,
				&["keybindings.yml", "keybindings.yaml", "keybindings.json"][..],
				ItemShape::File,
			),
			Self::Skills => (Anchor::Agent, &["skills"][..], ItemShape::Directory),
			Self::ManagedSkills => (Anchor::Agent, &["managed-skills"][..], ItemShape::Directory),
			Self::Rules => (Anchor::Agent, &["rules"][..], ItemShape::Directory),
			Self::RulesMd => (Anchor::Agent, &["RULES.md"][..], ItemShape::File),
			Self::AgentsMd => (Anchor::Agent, &["AGENTS.md"][..], ItemShape::File),
			Self::Prompts => (Anchor::Agent, &["prompts"][..], ItemShape::Directory),
			Self::Themes => (Anchor::Agent, &["themes"][..], ItemShape::Directory),
			Self::Mcp => (Anchor::Agent, &["mcp.json", ".mcp.json"][..], ItemShape::File),
			Self::Ssh => (Anchor::Agent, &["ssh.json"][..], ItemShape::File),
			Self::Lsp => (Anchor::Agent, LSP, ItemShape::File),
			Self::Dap => (Anchor::Agent, DAP, ItemShape::File),
			Self::Secrets => (Anchor::Agent, &["secrets.yml"][..], ItemShape::File),
			Self::SecretPlaceholderKey => {
				(Anchor::AgentXdg(XdgCategory::State), &["secret-placeholder.key"][..], ItemShape::File)
			},
			Self::InstallId => (Anchor::BaseRoot, &["install-id"][..], ItemShape::File),
			Self::MnemopiMemory => {
				(Anchor::AgentXdg(XdgCategory::State), &["memories/mnemopi"][..], ItemShape::Directory)
			},
			Self::Marketplaces => {
				(Anchor::RootXdg(XdgCategory::Data), &["marketplaces.json"][..], ItemShape::File)
			},
			Self::Plugins => {
				(Anchor::RootXdg(XdgCategory::Data), &["plugins"][..], ItemShape::Directory)
			},
			Self::SystemMd => (Anchor::Agent, &["SYSTEM.md"][..], ItemShape::File),
			Self::AppendSystemMd => (Anchor::Agent, &["APPEND_SYSTEM.md"][..], ItemShape::File),
			Self::TitleSystemMd => (Anchor::Agent, &["TITLE_SYSTEM.md"][..], ItemShape::File),
			Self::Commands => (Anchor::Agent, &["commands"][..], ItemShape::Directory),
			Self::Agents => (Anchor::Agent, &["agents"][..], ItemShape::Directory),
			Self::Memories => {
				(Anchor::AgentXdg(XdgCategory::State), &["memories"][..], ItemShape::Directory)
			},
			Self::StatsDb => (Anchor::RootXdg(XdgCategory::Data), &["stats.db"][..], ItemShape::File),
		};
		ItemSpec {
			anchor,
			names,
			shape,
			adopts_legacy: matches!(self, Self::SecretPlaceholderKey | Self::Marketplaces),
		}
	}
}

/// Inputs the v1 locator reads: the process environment captured once, or a
/// test's explicit values. Nothing else in v2 reads the `PI_*` variables.
#[derive(Clone, Debug, Default)]
pub struct V1Inputs {
	/// The owner's home directory.
	pub home:           PathBuf,
	/// `PI_CONFIG_DIR`: the configuration directory name under `home`.
	pub config_dir:     Option<OsString>,
	/// `PI_CODING_AGENT_DIR`: the default profile's agent directory.
	pub agent_dir:      Option<PathBuf>,
	/// `OMP_PROFILE`, which v1 read first.
	pub omp_profile:    Option<OsString>,
	/// `PI_PROFILE`, v1's fallback profile selector.
	pub pi_profile:     Option<OsString>,
	/// `XDG_DATA_HOME`.
	pub xdg_data_home:  Option<PathBuf>,
	/// `XDG_STATE_HOME`.
	pub xdg_state_home: Option<PathBuf>,
	/// `XDG_CACHE_HOME`.
	pub xdg_cache_home: Option<PathBuf>,
	/// An explicit v1 configuration root (`--from`), read as-is: it replaces
	/// `$HOME/.omp` and disables `PI_*` and XDG relocation.
	pub explicit_root:  Option<PathBuf>,
}

impl V1Inputs {
	/// Captures the process environment; `None` without a home directory.
	#[must_use]
	pub fn from_process() -> Option<Self> {
		let path = |name: &str| env::var_os(name).map(PathBuf::from);
		Some(Self {
			home:           omp_core::dirs::home_dir()?,
			config_dir:     env::var_os("PI_CONFIG_DIR"),
			agent_dir:      path("PI_CODING_AGENT_DIR"),
			omp_profile:    env::var_os("OMP_PROFILE"),
			pi_profile:     env::var_os("PI_PROFILE"),
			xdg_data_home:  path("XDG_DATA_HOME"),
			xdg_state_home: path("XDG_STATE_HOME"),
			xdg_cache_home: path("XDG_CACHE_HOME"),
			explicit_root:  None,
		})
	}

	/// Reads the v1 tree rooted at `root` (`--from`) instead of the owner's.
	#[must_use]
	pub fn with_explicit_root(mut self, root: PathBuf) -> Self {
		self.explicit_root = Some(root);
		self
	}

	fn xdg_home(&self, category: XdgCategory) -> Option<&Path> {
		match category {
			XdgCategory::Data => self.xdg_data_home.as_deref(),
			XdgCategory::State => self.xdg_state_home.as_deref(),
			XdgCategory::Cache => self.xdg_cache_home.as_deref(),
		}
		.filter(|path| !path.as_os_str().is_empty())
	}
}

/// Normalizes a v1 profile selector the way `normalizeProfileName` does.
fn profile_name(value: Option<&OsString>) -> Option<Str> {
	value
		.and_then(|value| value.to_str())
		.and_then(|value| omp_core::dirs::normalize_profile_name(value).ok())
		.flatten()
}

/// Appends `relative` under `base` the way Node's `path.join` does: an
/// absolute `relative` still lands under `base`.
fn node_join(base: &Path, relative: &Path) -> PathBuf {
	let mut joined = base.to_owned();
	for component in relative.components() {
		match component {
			Component::Normal(part) => joined.push(part),
			Component::ParentDir => {
				joined.pop();
			},
			Component::RootDir | Component::Prefix(_) | Component::CurDir => {},
		}
	}
	joined
}

/// A located v1 install: its profile-independent root plus v1's own profile
/// selection.
#[derive(Clone, Debug)]
pub struct V1Source {
	inputs:         V1Inputs,
	base_root:      PathBuf,
	active_profile: Option<Str>,
}

impl V1Source {
	/// Locates the v1 install `inputs` describe. Nothing is read yet.
	#[must_use]
	pub fn new(inputs: V1Inputs) -> Self {
		let base_root = inputs.explicit_root.clone().unwrap_or_else(|| {
			let name = inputs
				.config_dir
				.as_ref()
				.filter(|name| !name.is_empty())
				.map_or_else(|| PathBuf::from(V1_CONFIG_DIR_NAME), PathBuf::from);
			node_join(&inputs.home, &name)
		});
		// `resolveProfileEnv`: a defined `OMP_PROFILE`, even empty, wins; an
		// invalid value selects the default, as v1's module load did.
		let active_profile = if inputs.explicit_root.is_some() {
			None
		} else if inputs.omp_profile.is_some() {
			profile_name(inputs.omp_profile.as_ref())
		} else {
			profile_name(inputs.pi_profile.as_ref())
		};
		Self { inputs, base_root, active_profile }
	}

	/// The profile-independent root (`~/.omp`).
	#[must_use]
	pub fn base_root(&self) -> &Path {
		&self.base_root
	}

	/// The profile v1 itself would have run with (`OMP_PROFILE`, else
	/// `PI_PROFILE`). Imports cover every profile; this only decides which
	/// profile-derived agent directory `PI_CODING_AGENT_DIR` must not be
	/// mistaken for.
	#[must_use]
	pub fn active_profile(&self) -> Option<&str> {
		self.active_profile.as_deref()
	}

	/// Whether any v1 data exists: the root or a relocated XDG root.
	#[must_use]
	pub fn exists(&self) -> bool {
		self.base_root.is_dir() || self.layout(None).xdg.iter().any(Option::is_some)
	}

	/// Whether the named profile has a v1 root (`~/.omp/profiles/<profile>`).
	#[must_use]
	pub fn has_profile(&self, profile: &str) -> bool {
		self.base_root.join("profiles").join(profile).is_dir()
	}

	/// Named v1 profiles, sorted; directories whose names v1 would reject are
	/// left out.
	///
	/// # Errors
	///
	/// Returns [`LocateError::Profiles`] when `profiles/` exists but cannot be
	/// listed.
	pub fn profiles(&self) -> Result<Vec<Str>, LocateError> {
		let directory = self.base_root.join("profiles");
		let entries = match fs::read_dir(&directory) {
			Ok(entries) => entries,
			Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
			Err(source) => return Err(LocateError::Profiles { path: directory, source }),
		};
		let mut profiles = Vec::new();
		for entry in entries {
			let entry =
				entry.map_err(|source| LocateError::Profiles { path: directory.clone(), source })?;
			if !entry.path().is_dir() {
				continue;
			}
			let name = entry.file_name();
			let Some(name) = name.to_str() else {
				continue;
			};
			if let Ok(Some(profile)) = omp_core::dirs::normalize_profile_name(name)
				&& profile == name
			{
				profiles.push(profile);
			}
		}
		profiles.sort();
		Ok(profiles)
	}

	/// The layout of one v1 profile (`None` is the default profile).
	#[must_use]
	pub fn layout(&self, profile: Option<&str>) -> V1Layout {
		let config_root = profile.map_or_else(
			|| self.base_root.clone(),
			|profile| self.base_root.join("profiles").join(profile),
		);
		let default_agent = config_root.join("agent");
		let agent_dir = self
			.agent_dir_override(profile)
			.unwrap_or_else(|| default_agent.clone());
		let mut xdg = [None, None, None];
		let relocatable = cfg!(any(target_os = "linux", target_os = "macos"))
			&& self.inputs.explicit_root.is_none()
			&& agent_dir == default_agent;
		if relocatable {
			for category in [XdgCategory::Data, XdgCategory::State, XdgCategory::Cache] {
				let Some(home) = self.inputs.xdg_home(category) else {
					continue;
				};
				let app_root = home.join(V1_APP_NAME);
				let root = match profile {
					Some(profile) => app_root.join("profiles").join(profile),
					None => app_root,
				};
				if root.exists() {
					xdg[category.index()] = Some(root);
				}
			}
		}
		V1Layout {
			profile: profile.map(Str::new),
			base_root: self.base_root.clone(),
			config_root,
			agent_dir,
			xdg,
		}
	}

	/// `PI_CODING_AGENT_DIR` as v1's `resolvePreProfileAgentDir` snapshots it
	/// for the default profile: default profile only, and never a value a
	/// parent's `setProfile` derived for v1's selected profile or `PI_PROFILE`.
	fn agent_dir_override(&self, profile: Option<&str>) -> Option<PathBuf> {
		if profile.is_some() || self.inputs.explicit_root.is_some() {
			return None;
		}
		let value = self
			.inputs
			.agent_dir
			.as_ref()
			.filter(|value| !value.as_os_str().is_empty())?;
		let derived = |profile: &str| self.base_root.join("profiles").join(profile).join("agent");
		if self
			.active_profile
			.iter()
			.chain(&profile_name(self.inputs.pi_profile.as_ref()))
			.any(|profile| *value == derived(profile))
		{
			return None;
		}
		Some(std::path::absolute(value).unwrap_or_else(|_| value.clone()))
	}
}

/// Where one v1 profile keeps its items.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct V1Layout {
	profile:     Option<Str>,
	base_root:   PathBuf,
	config_root: PathBuf,
	agent_dir:   PathBuf,
	xdg:         [Option<PathBuf>; 3],
}

impl V1Layout {
	/// The v1 profile (`None` is the default profile).
	#[must_use]
	pub fn profile(&self) -> Option<&str> {
		self.profile.as_deref()
	}

	/// The profile-independent root (`~/.omp`).
	#[must_use]
	pub fn base_root(&self) -> &Path {
		&self.base_root
	}

	/// The profile root (`~/.omp` or `~/.omp/profiles/<profile>`).
	#[must_use]
	pub fn config_root(&self) -> &Path {
		&self.config_root
	}

	/// The agent directory (`<profile root>/agent` or `PI_CODING_AGENT_DIR`).
	#[must_use]
	pub fn agent_dir(&self) -> &Path {
		&self.agent_dir
	}

	/// The XDG root this profile's items of `category` relocated to, if any.
	#[must_use]
	pub fn xdg_root(&self, category: XdgCategory) -> Option<&Path> {
		self.xdg[category.index()].as_deref()
	}

	/// Every path v1 could keep `item` at, in v1's lookup order: the relocated
	/// location, then the unrelocated one v1 adopts from, each in candidate
	/// name order.
	pub fn candidates(&self, item: V1Item) -> impl Iterator<Item = PathBuf> + '_ {
		let spec = item.spec();
		let (primary, legacy) = match spec.anchor {
			Anchor::Agent => (self.agent_dir.as_path(), None),
			Anchor::AgentXdg(category) => match self.xdg_root(category) {
				Some(root) => (root, spec.adopts_legacy.then_some(self.agent_dir.as_path())),
				None => (self.agent_dir.as_path(), None),
			},
			Anchor::RootXdg(category) => match self.xdg_root(category) {
				Some(root) => (root, spec.adopts_legacy.then_some(self.config_root.as_path())),
				None => (self.config_root.as_path(), None),
			},
			Anchor::BaseRoot => (self.base_root.as_path(), None),
		};
		std::iter::once(primary)
			.chain(legacy)
			.flat_map(move |base| spec.names.iter().map(move |name| base.join(name)))
	}

	/// Where v1 reads `item` from, when it exists.
	#[must_use]
	pub fn locate(&self, item: V1Item) -> Option<PathBuf> {
		let shape = item.shape();
		self.candidates(item).find(|path| match shape {
			ItemShape::File => path.is_file(),
			ItemShape::Directory => path.is_dir(),
		})
	}

	/// Every item this profile has, with where it lives, in [`V1Item`]
	/// order.
	pub fn inventory(&self) -> impl Iterator<Item = (V1Item, PathBuf)> + '_ {
		use strum::IntoEnumIterator as _;
		V1Item::iter().filter_map(|item| self.locate(item).map(|path| (item, path)))
	}
}

/// Where one v2 profile receives imported items.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct V2Target {
	/// The v2 profile (`None` is the default profile).
	pub profile:    Option<Str>,
	/// Profile configuration root (`~/.o2` or `~/.o2/profiles/<profile>`),
	/// which also holds the per-step import markers.
	pub config_dir: PathBuf,
	/// Profile data directory (`<data>` or `<data>/profiles/<profile>`).
	pub data_dir:   PathBuf,
	/// v2 state root.
	pub state_dir:  PathBuf,
	/// v2 cache root.
	pub cache_dir:  PathBuf,
}

/// The v2 roots every profile target derives from.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct V2Roots {
	/// Base configuration directory (`~/.o2`, or `OMP_CONFIG_DIR`).
	pub config_dir:     PathBuf,
	/// Base data directory (`OMP_DATA_DIR`, else the XDG data root).
	pub data_dir:       PathBuf,
	/// Base state directory.
	pub state_dir:      PathBuf,
	/// Base cache directory.
	pub cache_dir:      PathBuf,
	/// The active v2 profile.
	pub active_profile: Option<Str>,
}

impl V2Roots {
	/// Resolves the process's v2 roots and active profile.
	///
	/// # Errors
	///
	/// Returns [`LocateError`] without a home directory or with an invalid
	/// `OMP_PROFILE`.
	pub fn from_process() -> Result<Self, LocateError> {
		let home = omp_core::dirs::home_dir().ok_or(LocateError::HomeUnset)?;
		let native = omp_core::dirs::native_directories(&home);
		Ok(Self {
			config_dir:     omp_core::dirs::config_dir(&home),
			data_dir:       native.data,
			state_dir:      native.state,
			cache_dir:      native.cache,
			active_profile: omp_core::dirs::active_profile()?,
		})
	}

	/// The target for one v2 profile.
	#[must_use]
	pub fn target(&self, profile: Option<&str>) -> V2Target {
		let scoped = |base: &Path| match profile {
			Some(profile) => base.join("profiles").join(profile),
			None => base.to_owned(),
		};
		V2Target {
			profile:    profile.map(Str::new),
			config_dir: scoped(&self.config_dir),
			data_dir:   scoped(&self.data_dir),
			state_dir:  self.state_dir.clone(),
			cache_dir:  self.cache_dir.clone(),
		}
	}
}

/// Which v1 profiles to import (owner decision #8). Each v1 profile always
/// imports into the same-named v2 profile, and the v1 default profile into the
/// v2 default roots.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum ProfileSelection {
	/// The v1 default profile and every named v1 profile (the default, for
	/// both the first run and `omp config import-v1`).
	#[default]
	All,
	/// Only this profile (`None` is the default profile): `--profile <p>`, and
	/// the lazy `models.toml` import of the active profile.
	Named(Option<Str>),
}

/// One v1 profile paired with the v2 profile it imports into.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ImportPair {
	/// Where the v1 profile keeps its items.
	pub source: V1Layout,
	/// Where the v2 profile receives them.
	pub target: V2Target,
}

/// A v1 XDG root that is, or lies inside, the matching v2 root.
///
/// v1 relocated to `$XDG_<PURPOSE>_HOME/omp`, which is also v2's default
/// root, so both versions share one tree. The importer reports this and never
/// relocates anything.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct XdgCollision {
	/// Colliding XDG purpose.
	pub category: XdgCategory,
	/// v1's relocated root.
	pub v1:       PathBuf,
	/// The v2 root it collides with.
	pub v2:       PathBuf,
}

impl ImportPair {
	/// The XDG collisions between this pair's v1 and v2 roots.
	#[must_use]
	pub fn xdg_collisions(&self) -> Vec<XdgCollision> {
		[
			(XdgCategory::Data, &self.target.data_dir),
			(XdgCategory::State, &self.target.state_dir),
			(XdgCategory::Cache, &self.target.cache_dir),
		]
		.into_iter()
		.filter_map(|(category, v2)| {
			let v1 = self.source.xdg_root(category)?;
			(v1 == v2.as_path() || v1.starts_with(v2)).then(|| XdgCollision {
				category,
				v1: v1.to_owned(),
				v2: v2.clone(),
			})
		})
		.collect()
	}
}

/// Pairs v1 profiles with v2 targets per owner decision #8.
///
/// # Errors
///
/// Returns [`LocateError::Profiles`] when [`ProfileSelection::All`] cannot
/// list the v1 profiles.
pub fn plan(
	source: &V1Source,
	roots: &V2Roots,
	selection: &ProfileSelection,
) -> Result<Vec<ImportPair>, LocateError> {
	let pair = |profile: Option<&str>| ImportPair {
		source: source.layout(profile),
		target: roots.target(profile),
	};
	Ok(match selection {
		ProfileSelection::Named(profile) => vec![pair(profile.as_deref())],
		ProfileSelection::All => std::iter::once(None)
			.chain(
				source
					.profiles()?
					.iter()
					.map(|profile| Some(profile.as_str())),
			)
			.map(pair)
			.collect(),
	})
}

/// Failure to locate v1 or v2 roots.
#[derive(Debug, Error)]
pub enum LocateError {
	/// No home directory to resolve `~/.omp` and `~/.o2` against.
	#[error("HOME or USERPROFILE must be set to locate v1 and v2 configuration")]
	HomeUnset,
	/// The active v2 profile is invalid.
	#[error(transparent)]
	Profile(#[from] ProfileNameError),
	/// The v1 `profiles/` directory could not be listed.
	#[error("could not list v1 profiles in {}", path.display())]
	Profiles {
		/// The `profiles/` directory.
		path:   PathBuf,
		/// Typed filesystem failure.
		#[source]
		source: io::Error,
	},
}
