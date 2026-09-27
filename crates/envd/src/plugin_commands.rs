//! The processes installed plugins would launch, enumerated through the same
//! parsers and gates the MCP, language-server, and debug-adapter seams admit
//! them with.
//!
//! Each seam refuses an unapproved launch on its own
//! ([`omp_ext::claude_plugin::ClaudePlugin::admit_launch`]); this module lets
//! a composition report every refusal up front and lets `omp ext trust`
//! list and approve a plugin's launches, with digests that match what the
//! seams check.

use omp_ext::{
	claude_plugin::ClaudePlugin,
	plugin_command::{PluginCommandBlocked, PluginLaunch},
};

use crate::{
	docserver::{dap_config::plugin_dap_launches, lsp_config::plugin_lsp_launches},
	mcp::discovery::plugin_mcp_launches,
};

/// Every process `plugin` declares: its stdio MCP servers, then language
/// servers, then debug adapters, each as the launching seam resolves it.
#[must_use]
pub fn plugin_launches(plugin: &ClaudePlugin) -> Vec<PluginLaunch> {
	let mut launches = plugin_mcp_launches(plugin).collect::<Vec<_>>();
	launches.extend(plugin_lsp_launches(plugin));
	launches.extend(plugin_dap_launches(plugin));
	launches
}

/// Every declared launch across `plugins` that the operator has not
/// approved, so it does not start.
#[must_use]
pub fn blocked_launches(plugins: &[ClaudePlugin]) -> Vec<PluginCommandBlocked> {
	plugins
		.iter()
		.flat_map(|plugin| {
			plugin_launches(plugin)
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
