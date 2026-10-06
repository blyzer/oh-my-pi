//! Typed settings owned by the Environment MCP runtime.

use omp_con::Ctx;
use serde::{Deserialize, Serialize};

omp_con::var! {
	/// Load project-scoped MCP server definitions (.omp/mcp.json, .mcp.json, and the
	/// foreign editor files) from the project. Off by default: a project file can
	/// start processes with your authority, so enable it only for projects you trust.
	/// User-level only; a project cfg overlay can never set it.
	pub static SV_MCP_ENABLE_PROJECT_CONFIG = sv_mcp_enable_project_config: bool {
		default: false,
		flags: archive,
		meta: {
			"ui.tab": "tools",
			"ui.group": "Discovery & MCP",
			"ui.label": "MCP Project Config",
			"legacy.path": "mcp.enableProjectConfig",
		},
	};
}

/// Native MCP discovery policy.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default, rename_all = "camelCase")]
pub struct McpSettings {
	/// Whether project-scoped sources (`.omp/mcp.json`, `.mcp.json`, and the
	/// foreign editor files) participate. Off by default.
	pub enable_project_config: bool,
}

impl McpSettings {
	/// Resolves MCP discovery policy from the process control context.
	#[must_use]
	pub fn from_con(ctx: &Ctx) -> Self {
		Self { enable_project_config: SV_MCP_ENABLE_PROJECT_CONFIG.get(ctx) }
	}
}

#[cfg(test)]
mod tests {
	use omp_con::{ConError, Source};
	use omp_core::Str;

	use super::*;

	#[test]
	fn con_defaults_disabled() {
		assert!(!McpSettings::default().enable_project_config);
		assert!(!McpSettings::from_con(&Ctx::new()).enable_project_config);
	}

	#[test]
	fn user_level_opt_in_enables_project_config() {
		let ctx = Ctx::new();
		SV_MCP_ENABLE_PROJECT_CONFIG
			.set(&ctx, true)
			.expect("user-level set");
		assert!(McpSettings::from_con(&ctx).enable_project_config);
	}

	#[test]
	fn a_project_overlay_can_never_enable_it() {
		let ctx = Ctx::new();
		let denied = ctx
			.exec("sv_mcp_enable_project_config true", Source::Project(Str::new_static("config.cfg")))
			.expect_err("project overlay is denied");
		assert!(matches!(denied, ConError::ProjectVarDenied { .. }), "{denied:?}");
		assert!(!McpSettings::from_con(&ctx).enable_project_config);
	}
}
