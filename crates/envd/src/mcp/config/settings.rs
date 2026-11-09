//! Typed settings owned by the Environment MCP runtime.

use omp_con::Ctx;
use omp_ext::workspace_trust::WorkspaceTrust;

omp_con::var! {
	/// Load project-scoped MCP server definitions (.omp/mcp.json, .mcp.json, and the
	/// foreign editor files) from every project, trusted or not. Off by default: a
	/// project file can start processes with your authority, so a trusted workspace
	/// loads them without it. User-level only; a project cfg overlay can never set it.
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

/// Whether project-scoped MCP sources (`.omp/mcp.json`, `.mcp.json`, the
/// foreign editor files and project plugin installs) participate, and why.
///
/// Their values stay literal under every policy
/// ([`crate::mcp::config_values::ValueResolution::for_source`]).
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, strum::IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
pub enum ProjectMcpPolicy {
	/// The workspace is trusted, so its project sources load.
	Trusted,
	/// The workspace is not trusted, but the user opted every project in
	/// (`sv_mcp_enable_project_config`).
	UserOptIn,
	/// The workspace is not trusted and the user did not opt in: project
	/// sources are withheld.
	#[default]
	Withheld,
}

impl ProjectMcpPolicy {
	/// The policy for a workspace of `trust` when the user's opt-in is
	/// `user_opt_in`.
	#[must_use]
	pub const fn of(trust: WorkspaceTrust, user_opt_in: bool) -> Self {
		match (trust, user_opt_in) {
			(WorkspaceTrust::Trusted, _) => Self::Trusted,
			(WorkspaceTrust::Untrusted, true) => Self::UserOptIn,
			(WorkspaceTrust::Untrusted, false) => Self::Withheld,
		}
	}

	/// Whether project-scoped sources load under this policy.
	#[must_use]
	pub const fn loads_project(self) -> bool {
		!matches!(self, Self::Withheld)
	}
}

/// Native MCP discovery policy.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct McpSettings {
	/// Whether project-scoped sources participate. Withheld by default.
	pub project_config: ProjectMcpPolicy,
}

impl McpSettings {
	/// Resolves MCP discovery policy from the process control context and the
	/// composition's workspace trust decision.
	#[must_use]
	pub fn from_con(ctx: &Ctx, trust: WorkspaceTrust) -> Self {
		Self { project_config: ProjectMcpPolicy::of(trust, SV_MCP_ENABLE_PROJECT_CONFIG.get(ctx)) }
	}
}

#[cfg(test)]
mod tests {
	use omp_con::{ConError, Source};
	use omp_core::Str;

	use super::*;

	#[test]
	fn con_defaults_withhold_an_untrusted_project() {
		assert_eq!(McpSettings::default().project_config, ProjectMcpPolicy::Withheld);
		assert_eq!(
			McpSettings::from_con(&Ctx::new(), WorkspaceTrust::Untrusted).project_config,
			ProjectMcpPolicy::Withheld
		);
	}

	#[test]
	fn a_trusted_workspace_loads_its_project_config_without_the_opt_in() {
		assert_eq!(
			McpSettings::from_con(&Ctx::new(), WorkspaceTrust::Trusted).project_config,
			ProjectMcpPolicy::Trusted
		);
	}

	#[test]
	fn user_level_opt_in_enables_project_config() {
		let ctx = Ctx::new();
		SV_MCP_ENABLE_PROJECT_CONFIG
			.set(&ctx, true)
			.expect("user-level set");
		assert_eq!(
			McpSettings::from_con(&ctx, WorkspaceTrust::Untrusted).project_config,
			ProjectMcpPolicy::UserOptIn
		);
		assert_eq!(
			McpSettings::from_con(&ctx, WorkspaceTrust::Trusted).project_config,
			ProjectMcpPolicy::Trusted
		);
	}

	#[test]
	fn a_project_overlay_can_never_enable_it() {
		let ctx = Ctx::new();
		let denied = ctx
			.exec("sv_mcp_enable_project_config true", Source::Project(Str::new_static("config.cfg")))
			.expect_err("project overlay is denied");
		assert!(matches!(denied, ConError::ProjectVarDenied { .. }), "{denied:?}");
		assert_eq!(
			McpSettings::from_con(&ctx, WorkspaceTrust::Untrusted).project_config,
			ProjectMcpPolicy::Withheld
		);
	}
}
