//! The processes installed plugins and Agent Plugins packages would launch,
//! enumerated through the same parsers and gates the MCP, language-server,
//! and debug-adapter seams admit them with.
//!
//! Each seam refuses an unapproved launch on its own
//! ([`omp_ext::claude_plugin::ClaudePlugin::admit_launch`],
//! [`omp_ext::plugin_command::CommandApprovals::admit`]); this module lets a
//! composition report every refusal up front and lets `omp ext trust` list
//! and approve a plugin's launches, with digests that match what the seams
//! check.
//!
//! A report of what a session refuses covers only what the session would
//! load: under [`McpSettings::enable_project_config`] off, MCP discovery
//! drops every project-scoped source ([`ConfigSourceKind::loads`]), so the
//! project's Agent Plugins packages and the MCP servers of plugins installed
//! for the project are neither loaded nor reported.

use std::path::PathBuf;

use omp_core::Str;
use omp_ext::{
	claude_plugin::ClaudePlugin,
	plugin_command::{PluginCommandBlocked, PluginLaunch, plugin_command_digest},
};

use crate::{
	docserver::{dap_config::plugin_dap_launches, lsp_config::plugin_lsp_launches},
	mcp::{
		McpConfigPaths, McpSettings,
		config::ConfigSourceKind,
		discovery::{claude_plugin_kind, plugin_mcp_launches},
	},
};

/// One Agent Plugins 1.0 package's stdio MCP launches, under the identity
/// its approvals are keyed on.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentPluginLaunches {
	/// The package's manifest `name`: its approval identity.
	pub plugin:   Str,
	/// The manifest `version`; empty when the manifest records none.
	pub version:  Str,
	/// Canonical package root.
	pub root:     PathBuf,
	/// Every stdio server launch its `mcp.json` declares, as discovery
	/// resolves it.
	pub launches: Vec<PluginLaunch>,
	/// The discovery source it joins as; a project-scoped package loads only
	/// while project MCP configuration is enabled ([`ConfigSourceKind::loads`]).
	pub kind:     ConfigSourceKind,
}

impl AgentPluginLaunches {
	/// The approval key of `launch` for this package at its version.
	#[must_use]
	pub fn command_digest(&self, launch: &PluginLaunch) -> omp_core::Hash32 {
		plugin_command_digest(&self.plugin, &self.version, launch)
	}
}

/// Every Agent Plugins package MCP discovery would load for `paths`: the
/// project and user plugin directories, then the explicit roots.
#[must_use]
pub fn agent_plugin_launches(paths: &McpConfigPaths) -> Vec<AgentPluginLaunches> {
	crate::mcp::discovery::agent_plugin_launches(paths)
}

/// Every unapproved Agent Plugins stdio launch a session would load.
///
/// Those are the launches a session under `settings` loads for `paths` that
/// the approvals `paths` carries do not admit, so they do not start. A
/// package MCP discovery skips under `settings` (a project
/// package while project configuration is disabled) launches nothing and is
/// not reported.
#[must_use]
pub fn blocked_agent_plugin_launches(
	paths: &McpConfigPaths,
	settings: &McpSettings,
) -> Vec<PluginCommandBlocked> {
	agent_plugin_launches(paths)
		.into_iter()
		.filter(|package| package.kind.loads(settings.enable_project_config))
		.flat_map(|package| {
			let AgentPluginLaunches { plugin, version, launches, .. } = package;
			launches.into_iter().filter_map(move |launch| {
				paths
					.command_approvals
					.admit(&plugin, &version, launch)
					.err()
			})
		})
		.collect()
}

/// Every process `plugin` declares: its stdio MCP servers, then language
/// servers, then debug adapters, each as the launching seam resolves it.
#[must_use]
pub fn plugin_launches(plugin: &ClaudePlugin) -> Vec<PluginLaunch> {
	let mut launches = plugin_mcp_launches(plugin).collect::<Vec<_>>();
	launches.extend(plugin_lsp_launches(plugin));
	launches.extend(plugin_dap_launches(plugin));
	launches
}

/// Every process `plugin` declares that a session under `settings` loads.
///
/// That is [`plugin_launches`] without the MCP servers of a plugin whose MCP
/// declarations discovery skips (one installed for the project while project
/// configuration is disabled).
#[must_use]
pub fn loaded_plugin_launches(plugin: &ClaudePlugin, settings: &McpSettings) -> Vec<PluginLaunch> {
	let mut launches = if claude_plugin_kind(plugin).loads(settings.enable_project_config) {
		plugin_mcp_launches(plugin).collect::<Vec<_>>()
	} else {
		Vec::new()
	};
	launches.extend(plugin_lsp_launches(plugin));
	launches.extend(plugin_dap_launches(plugin));
	launches
}

/// Every launch across `plugins` a session under `settings` loads that the
/// operator has not approved, so it does not start.
#[must_use]
pub fn blocked_launches(
	plugins: &[ClaudePlugin],
	settings: &McpSettings,
) -> Vec<PluginCommandBlocked> {
	plugins
		.iter()
		.flat_map(|plugin| {
			loaded_plugin_launches(plugin, settings)
				.into_iter()
				.filter_map(|launch| plugin.admit_launch(launch).err())
		})
		.collect()
}

/// Marks every launch each plugin declares as approved, as if the operator
/// approved them all; for tests exercising what an approved plugin loads.
#[cfg(test)]
pub(crate) fn approve_all(plugins: &mut [ClaudePlugin]) {
	for plugin in plugins {
		plugin.approved_commands = plugin_launches(plugin)
			.iter()
			.map(|launch| plugin.command_digest(launch))
			.collect();
	}
}
