//! Operator approval of the commands plugins launch.
//!
//! The MCP, language-server, and debug-adapter seams of the environment
//! host, and the plugin hook host ([`crate::plugin_hooks`]), each refuse a
//! plugin launch the operator has not approved
//! ([`omp_ext::claude_plugin::ClaudePlugin::admit_launch`],
//! [`CommandApprovals::admit`]). This module is the composition's view of that
//! gate: [`blocked_launches`] reports every refusal of a session's plugins up
//! front, so a launching host can name each plugin and command instead of the
//! servers and hooks silently missing; [`command_sets`] lists what one plugin
//! identity launches for `omp ext trust`; and [`approve_launch`] records an
//! approval in the `omp-ext` trust domain's local grant file, which the next
//! session reads.
//!
//! Two kinds of plugin own launches, each under its own approval identity:
//! an installed Claude-layout plugin (its registry id, `name@marketplace`,
//! and recorded version) launches MCP servers, language servers, debug
//! adapters, and hooks; an Agent Plugins 1.0 package (its manifest `name` and
//! `version`) launches the stdio MCP servers its `mcp.json` declares, whether
//! it was found in a plugin directory, passed with `--plugin-dir`, or
//! installed from a marketplace.

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
	claude_plugin::{ClaudePlugin, ClaudePlugins, PluginLayout},
	plugin_command::{CommandApprovals, PluginCommandBlocked, PluginLaunch, plugin_command_digest},
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
					origin: AgentPluginOrigin::Installed(plugin.scope),
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
	/// Approval identity: an installed plugin's `name@marketplace`, or an
	/// Agent Plugins package's manifest name.
	pub plugin:   Str,
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
/// `id`, and every Agent Plugins package `agent_plugins` discovers whose
/// manifest name is `id` or that is the installed plugin `id`.
#[must_use]
pub fn command_sets(
	id: &str,
	plugins: &ClaudePlugins,
	agent_plugins: &McpConfigPaths,
) -> Vec<PluginCommandSet> {
	let installed = plugins.plugins.iter().find(|plugin| plugin.id == id);
	let mut sets = installed
		.filter(|plugin| plugin.claude_components().is_some())
		.map(|plugin| PluginCommandSet {
			plugin:   plugin.id.clone(),
			version:  plugin.version.clone(),
			root:     plugin.root.clone(),
			launches: plugin_launches(plugin),
		})
		.into_iter()
		.collect::<Vec<_>>();
	let installed_root = installed
		.filter(|plugin| plugin.layout == PluginLayout::AgentPlugins)
		.and_then(|plugin| std::fs::canonicalize(&plugin.root).ok());
	sets.extend(
		omp_envd::plugin_commands::agent_plugin_launches(agent_plugins)
			.into_iter()
			.filter(|package| package.plugin == id || installed_root.as_ref() == Some(&package.root))
			.map(PluginCommandSet::from),
	);
	sets
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
	plugin: &Str,
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
