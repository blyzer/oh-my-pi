//! Operator approval of the processes installed plugins launch.
//!
//! The MCP, language-server, and debug-adapter seams of the environment
//! host each refuse a plugin launch the operator has not approved
//! ([`omp_ext::claude_plugin::ClaudePlugin::admit_launch`]). This module is
//! the composition's view of that gate: [`blocked_launches`] reports every
//! refusal of a resolved plugin set up front, so a launching host can name
//! each plugin and command instead of the servers silently missing, and
//! [`approve_launch`] records an approval in the `omp-ext` trust domain's
//! local grant file, which the next plugin resolution reads.

use std::path::Path;

use omp_core::Str;
use omp_ext::{
	claude_plugin::{ClaudePlugin, ClaudePlugins},
	plugin_command::{PluginCommandBlocked, PluginLaunch},
	trust::{GrantPersistenceError, GrantsFile, PluginCommandGrant, grants_path},
};

/// Every process `plugin` declares, as the launching seams resolve and gate
/// it: stdio MCP servers, then language servers, then debug adapters.
#[must_use]
pub fn plugin_launches(plugin: &ClaudePlugin) -> Vec<PluginLaunch> {
	omp_envd::plugin_commands::plugin_launches(plugin)
}

/// Every declared launch of `plugins` the operator has not approved; none
/// of them starts.
#[must_use]
pub fn blocked_launches(plugins: &ClaudePlugins) -> Vec<PluginCommandBlocked> {
	omp_envd::plugin_commands::blocked_launches(&plugins.plugins)
}

/// Persists the operator's approval of `launch` for `plugin` in the grant
/// file under `data_dir`.
///
/// `granted_by` records the approving channel. A later resolution of the
/// plugin at the same version admits the launch until its command,
/// arguments, or environment change.
pub fn approve_launch(
	data_dir: &Path,
	plugin: &ClaudePlugin,
	launch: &PluginLaunch,
	granted_by: Str,
) -> Result<(), GrantPersistenceError> {
	GrantsFile::persist_plugin_command(
		&grants_path(data_dir),
		PluginCommandGrant::approve(plugin, launch, granted_by),
	)
	.map(drop)
}
