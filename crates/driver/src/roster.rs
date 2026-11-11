//! Composer-locus built-ins.
//!
//! `task` and `hub` close over driver spawners. The construction table decides
//! that those two rows run; this module only supplies the values they capture.

use omp_tool::{Tool, ToolsPolicy};
use omp_tools::roster::{BuiltinRosterEntry, ComposerInstall, SelectedRoster, ToolGate};

/// Dynamic tools for the composer rows of `roster`.
pub(crate) fn composer_dynamic_tools(roster: &SelectedRoster) -> Vec<omp_envd::DynamicTool> {
	let mut host = ComposerHost { tools: Vec::new() };
	omp_tools::roster::install_composer(&mut host, roster);
	host.tools
}

struct ComposerHost {
	tools: Vec<omp_envd::DynamicTool>,
}

impl ToolGate for ComposerHost {
	fn tool_enabled(&self, _key: &str) -> bool {
		true
	}

	fn policy(&self) -> ToolsPolicy {
		ToolsPolicy::Auto
	}
}

impl ComposerInstall for ComposerHost {
	type HubBackend = crate::subagent::hub::HubDeclarationBackend;
	type TaskSpawner = crate::subagent::spawn::TaskDeclarationSpawner;

	fn task_spawner(&self) -> Self::TaskSpawner {
		crate::subagent::spawn::TaskDeclarationSpawner
	}

	fn hub_backend(&self) -> Self::HubBackend {
		crate::subagent::hub::HubDeclarationBackend
	}

	fn register_composer<T: Tool>(&mut self, tool: T, row: &BuiltinRosterEntry) {
		let (presentation, claims) = omp_tools::roster::registration(row, self.policy());
		self
			.tools
			.push(omp_envd::DynamicTool::new(tool, presentation, claims));
	}
}
