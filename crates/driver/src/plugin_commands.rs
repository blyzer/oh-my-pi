//! Operator approval of the commands plugins launch.
//!
//! The MCP, language-server, and debug-adapter seams of the environment
//! host, and the plugin hook host ([`crate::plugin_hooks`]), each refuse a
//! plugin launch the operator has not approved
//! ([`omp_ext::claude_plugin::ClaudePlugin::admit_launch`],
//! [`CommandApprovals::admit`]). This module is the composition's view of that
//! gate: [`blocked_launches`] reports every refusal of a session's plugins up
//! front, so a launching host can name each plugin and command instead of the
//! servers and hooks silently missing; [`resolve_command_sets`] lists what
//! one plugin identity launches; and [`approve_commands`] (over
//! [`approve_launch`]) records approvals in the `omp-ext` trust domain's
//! local grant file, which the next session reads. `omp ext trust
//! --approve-command(s)` and the in-chat `/plugins approve` both approve
//! through [`approve_commands`]: one admission, one writer.
//!
//! Two kinds of plugin own launches, each under its approval identity
//! ([`PluginId`]): an installed Claude-layout plugin (its registry id,
//! `name@marketplace`, and recorded version) launches MCP servers, language
//! servers, debug adapters, and hooks; an Agent Plugins 1.0 package launches
//! the stdio MCP servers its `mcp.json` declares, under its registry id when
//! it was installed from a marketplace, else (found in a plugin directory or
//! passed with `--plugin-dir`) under its manifest `name`, at its manifest
//! `version`.

use std::path::{Path, PathBuf};

use omp_core::{
	Hash32, Str,
	dirs::{DataDirError, user_config_root},
};
use omp_envd::{
	mcp::{AgentPluginOrigin, AgentPluginRoot, McpConfigPaths, McpSettings},
	plugin_commands::AgentPluginLaunches,
};
use omp_ext::{
	claude_plugin::{ClaudeCodeHome, ClaudePlugin, ClaudePlugins, PluginLayout},
	plugin_command::{
		CommandApprovals, PluginCommandBlocked, PluginId, PluginLaunch, plugin_command_digest,
	},
	trust::{GrantPersistenceError, GrantsFile, PluginCommandGrant, grants_path},
};

/// Every command `plugin` declares, as the launching seams resolve and gate
/// it: stdio MCP servers, language servers, debug adapters, then hooks.
#[must_use]
pub fn plugin_launches(plugin: &ClaudePlugin) -> Vec<PluginLaunch> {
	let mut launches = omp_envd::plugin_commands::plugin_launches(plugin);
	launches.extend(plugin.hook_launches().map(|(_, launch)| launch));
	launches
}

/// The Agent Plugins package roots a session passes to MCP discovery.
///
/// Beside the plugin directories discovery scans: every
/// `--extension`/`--plugin-dir` root that is an Agent Plugins package
/// ([`AgentPluginOrigin::Explicit`]), then every installed plugin in that
/// layout at its registry scope ([`AgentPluginOrigin::Installed`]). Only a
/// project install is project-scoped; an explicit root or a user install
/// loads whatever `sv_mcp_enable_project_config`.
#[must_use]
pub fn agent_plugin_roots(
	native_roots: &[PathBuf],
	plugins: &ClaudePlugins,
) -> Vec<AgentPluginRoot> {
	native_roots
		.iter()
		.filter(|root| crate::discovery::skills::is_agent_plugin_root(root))
		.map(|root| AgentPluginRoot::explicit(root.clone()))
		.chain(
			plugins
				.plugins
				.iter()
				.filter(|plugin| plugin.layout == PluginLayout::AgentPlugins)
				.map(|plugin| AgentPluginRoot {
					root:   plugin.root.clone(),
					origin: AgentPluginOrigin::Installed {
						id:    plugin.id.clone(),
						scope: plugin.scope,
					},
				}),
		)
		.collect()
}

/// The MCP discovery inputs deciding which Agent Plugins packages a session
/// in `project_root` loads, and which of their launches are approved.
///
/// Those are the user configuration root's plugin directories, the
/// project's, the explicit [`agent_plugin_roots`], and the approvals
/// `plugins` read.
///
/// # Errors
///
/// No home directory locates the user configuration root.
pub fn agent_plugin_paths(
	project_root: &Path,
	native_roots: &[PathBuf],
	plugins: &ClaudePlugins,
) -> Result<McpConfigPaths, DataDirError> {
	Ok(McpConfigPaths::new(&user_config_root()?, project_root)
		.with_agent_plugin_roots(agent_plugin_roots(native_roots, plugins))
		.with_command_approvals(plugins.command_approvals.clone()))
}

/// Every launch a session loads that the operator has not approved.
///
/// None of them runs: each installed plugin's servers and hooks, then the
/// stdio MCP servers of each Agent Plugins package `agent_plugins`
/// discovers, as a session under the MCP discovery policy `mcp` loads them.
///
/// What MCP discovery skips under `mcp` is not reported: with project
/// configuration disabled, neither the project's Agent Plugins packages nor
/// the MCP servers of a plugin installed for the project load
/// ([`omp_envd::plugin_commands::loaded_plugin_launches`]), whatever their
/// approval.
#[must_use]
pub fn blocked_launches(
	plugins: &ClaudePlugins,
	agent_plugins: &McpConfigPaths,
	mcp: &McpSettings,
) -> Vec<PluginCommandBlocked> {
	let mut blocked = plugins
		.plugins
		.iter()
		.flat_map(|plugin| {
			omp_envd::plugin_commands::loaded_plugin_launches(plugin, mcp)
				.into_iter()
				.chain(plugin.hook_launches().map(|(_, launch)| launch))
				.filter_map(|launch| plugin.admit_launch(launch).err())
		})
		.collect::<Vec<_>>();
	blocked.extend(omp_envd::plugin_commands::blocked_agent_plugin_launches(agent_plugins, mcp));
	blocked
}

/// Every command one plugin identity launches, with the version its
/// approvals bind.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PluginCommandSet {
	/// Approval identity: a marketplace install's `name@marketplace`, or a
	/// local Agent Plugins package's manifest name.
	pub plugin:   PluginId,
	/// Version the approval digests bind.
	pub version:  Str,
	/// Where the plugin lives.
	pub root:     PathBuf,
	/// Declared launches, as the seams gate them.
	pub launches: Vec<PluginLaunch>,
}

impl PluginCommandSet {
	/// The approval key of `launch`.
	#[must_use]
	pub fn command_digest(&self, launch: &PluginLaunch) -> Hash32 {
		plugin_command_digest(&self.plugin, &self.version, launch)
	}
}

impl From<AgentPluginLaunches> for PluginCommandSet {
	fn from(package: AgentPluginLaunches) -> Self {
		let AgentPluginLaunches { plugin, version, root, launches, kind: _ } = package;
		Self { plugin, version, root, launches }
	}
}

/// What `omp ext trust <id>` approves: the installed Claude-layout plugin
/// `id`, and every Agent Plugins package `agent_plugins` discovers under the
/// identity `id` (a marketplace install `name@marketplace`, a local package
/// its manifest name).
#[must_use]
pub fn command_sets(
	id: &PluginId<str>,
	plugins: &ClaudePlugins,
	agent_plugins: &McpConfigPaths,
) -> Vec<PluginCommandSet> {
	let mut sets = plugins
		.plugins
		.iter()
		.find(|plugin| plugin.id == *id)
		.filter(|plugin| plugin.claude_components().is_some())
		.map(|plugin| PluginCommandSet {
			plugin:   plugin.id.clone(),
			version:  plugin.version.clone(),
			root:     plugin.root.clone(),
			launches: plugin_launches(plugin),
		})
		.into_iter()
		.collect::<Vec<_>>();
	sets.extend(
		omp_envd::plugin_commands::agent_plugin_launches(agent_plugins)
			.into_iter()
			.filter(|package| package.plugin == *id)
			.map(PluginCommandSet::from),
	);
	sets
}

/// Every command the plugin identity `id` launches as a session in
/// `project`, with the Agent Plugins roots `plugin_dirs` the invocation
/// names, resolves it ([`command_sets`]), with the operator's current
/// approvals.
///
/// This is the one resolution `omp ext trust` and the in-chat
/// `/plugins approve` approve from.
///
/// # Errors
///
/// No home directory locates the user configuration root.
pub fn resolve_command_sets(
	data_dir: &Path,
	project: &Path,
	plugin_dirs: &[PathBuf],
	id: &PluginId<str>,
) -> Result<(Vec<PluginCommandSet>, CommandApprovals), DataDirError> {
	let plugins = ClaudePlugins::resolve(data_dir, project, ClaudeCodeHome::detect().as_ref());
	let agent_plugins = agent_plugin_paths(project, plugin_dirs, &plugins)?;
	let sets = command_sets(id, &plugins, &agent_plugins);
	Ok((sets, plugins.command_approvals))
}

/// Every launch a session in `project`, with the Agent Plugins roots
/// `plugin_dirs`, under the MCP policy `mcp`, would not start
/// ([`blocked_launches`]), resolved fresh from the registries and the grant
/// file.
///
/// # Errors
///
/// No home directory locates the user configuration root.
pub fn session_blocked_launches(
	data_dir: &Path,
	project: &Path,
	plugin_dirs: &[PathBuf],
	mcp: &McpSettings,
) -> Result<Vec<PluginCommandBlocked>, DataDirError> {
	let plugins = ClaudePlugins::resolve(data_dir, project, ClaudeCodeHome::detect().as_ref());
	let agent_plugins = agent_plugin_paths(project, plugin_dirs, &plugins)?;
	Ok(blocked_launches(&plugins, &agent_plugins, mcp))
}

/// Which of a plugin's commands one approval request covers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CommandSelection<'a> {
	/// Every command the plugin currently launches (`--approve-commands`,
	/// `/plugins approve <plugin> all`).
	All,
	/// The commands with these digests, as a blocked notice names them
	/// (`--approve-command`, `/plugins approve <plugin> <digest>`).
	Digests(&'a [Hash32]),
}

/// What one approval request recorded.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ApprovedCommands {
	/// Launches now approved, with the identity each was approved under.
	pub approved:   Vec<(PluginId, PluginLaunch)>,
	/// Selected launches naming a plugin file that cannot be read: never
	/// approved ([`PluginLaunch::files`]).
	pub unreadable: Vec<(PluginId, PluginLaunch)>,
}

/// Why an approval request recorded nothing.
#[derive(Debug, thiserror::Error)]
pub enum ApproveCommandsError {
	/// Nothing this project loads carries the identity.
	#[error(
		"{plugin} is neither an installed, enabled plugin nor an Agent Plugins package this project \
		 loads"
	)]
	UnknownPlugin {
		/// The requested identity.
		plugin: PluginId,
	},
	/// A requested digest names none of the plugin's current commands.
	#[error("{plugin} declares no command with digest {digest}")]
	UnknownDigest {
		/// The requested identity.
		plugin: PluginId,
		/// The digest no command carries.
		digest: Hash32,
	},
	/// The grant file could not be updated.
	#[error("the plugin command approval cannot be recorded")]
	Persist(#[from] GrantPersistenceError),
}

/// Records the operator's approval of `plugin`'s `selection` among `sets`
/// ([`resolve_command_sets`]) in the grant file under `data_dir`.
///
/// The single approval writer behind `omp ext trust --approve-command(s)`
/// and the in-chat `/plugins approve`: every requested digest must name a
/// current command (else nothing is recorded), each selected launch is
/// persisted through [`approve_launch`] under the digest the launching seams
/// check, and a launch naming a plugin file that cannot be read is reported
/// and never approved. `granted_by` records the approving channel.
///
/// # Errors
///
/// [`ApproveCommandsError`]: `sets` is empty, a digest names no command, or
/// the grant file cannot be written.
pub fn approve_commands(
	data_dir: &Path,
	sets: &[PluginCommandSet],
	plugin: &PluginId<str>,
	selection: CommandSelection<'_>,
	granted_by: Str,
) -> Result<ApprovedCommands, ApproveCommandsError> {
	if sets.is_empty() {
		return Err(ApproveCommandsError::UnknownPlugin { plugin: plugin.to_owned() });
	}
	if let CommandSelection::Digests(digests) = selection
		&& let Some(digest) = digests.iter().find(|digest| {
			!sets.iter().any(|set| {
				set.launches
					.iter()
					.any(|launch| set.command_digest(launch) == **digest)
			})
		}) {
		return Err(ApproveCommandsError::UnknownDigest {
			plugin: plugin.to_owned(),
			digest: *digest,
		});
	}
	let mut outcome = ApprovedCommands::default();
	for set in sets {
		let selected = set.launches.iter().filter(|launch| match selection {
			CommandSelection::All => true,
			CommandSelection::Digests(digests) => digests.contains(&set.command_digest(launch)),
		});
		for launch in selected {
			// A launch naming a plugin file that cannot be read cannot be
			// approved: its contents are part of the approval.
			if launch.files.unreadable().is_some() {
				outcome
					.unreadable
					.push((set.plugin.clone(), launch.clone()));
				continue;
			}
			approve_launch(data_dir, &set.plugin, &set.version, launch, granted_by.clone())?;
			outcome.approved.push((set.plugin.clone(), launch.clone()));
		}
	}
	Ok(outcome)
}

/// The operator's plugin command approvals in the grant file under
/// `data_dir`; none when it cannot be read, so every plugin launch stays
/// blocked (fail closed).
#[must_use]
pub fn command_approvals(data_dir: &Path) -> CommandApprovals {
	let path = grants_path(data_dir);
	GrantsFile::read(&path).map_or_else(
		|error| {
			tracing::warn!(
				error = &error as &(dyn std::error::Error + 'static),
				path = %path.display(),
				"plugin command approvals cannot be read; plugin commands do not run"
			);
			CommandApprovals::default()
		},
		|grants| grants.command_approvals(),
	)
}

/// Persists the operator's approval of `launch` for the plugin identity
/// `plugin` at `version` in the grant file under `data_dir`.
///
/// `granted_by` records the approving channel. A later session admits the
/// launch until the plugin's version or the launch's command, arguments,
/// environment, working directory, (for a hook) trigger, or the contents of
/// a plugin file it names change. A launch naming a plugin file that cannot
/// be read is never admitted, approved or not.
pub fn approve_launch(
	data_dir: &Path,
	plugin: &PluginId,
	version: &Str,
	launch: &PluginLaunch,
	granted_by: Str,
) -> Result<(), GrantPersistenceError> {
	GrantsFile::persist_plugin_command(
		&grants_path(data_dir),
		PluginCommandGrant::approve(plugin, version, launch, granted_by),
	)
	.map(drop)
}
