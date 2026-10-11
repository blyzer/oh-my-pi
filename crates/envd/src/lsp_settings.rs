//! Typed settings owned by the project LSP runtime.

use omp_con::Ctx;
use serde::{Deserialize, Serialize};

use crate::docserver::NativeLspOptions;

omp_con::var! {
	/// Enable the lsp tool for code intelligence (definitions, references, diagnostics, rename).
	pub static SV_LSP_ENABLED = sv_lsp_enabled: bool {
		default: true,
		flags: archive,
		meta: {
			"ui.tab": "files",
			"ui.group": "LSP",
			"ui.label": "LSP",
			"legacy.path": "lsp.enabled",
		},
	};
	/// Start language servers on first use (lsp tool or editing a matching file type) instead of at session startup.
	pub static SV_LSP_LAZY = sv_lsp_lazy: bool {
		default: true,
		flags: archive,
		meta: {
			"ui.tab": "files",
			"ui.group": "LSP",
			"ui.label": "Lazy LSP Startup",
			"legacy.path": "lsp.lazy",
		},
	};
	/// Let language servers start from project-local bins (node_modules/.bin, .venv/bin, bin, ...)
	/// ahead of $PATH. Off by default: a repository can ship its own executable under a name a
	/// language server uses, so enable it only if you trust every project you open.
	/// User-level only; a project cfg overlay can never set it.
	pub static SV_LSP_PROJECT_BINARIES = sv_lsp_project_binaries: bool {
		default: false,
		flags: archive,
		meta: {
			"ui.tab": "files",
			"ui.group": "LSP",
			"ui.label": "Project-Local Language Servers",
		},
	};
}

/// Layered language-server enablement and mutation feedback policy.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
pub struct LspSettings {
	/// Enables language-server bindings and the model-facing LSP surface.
	pub enabled:                 bool,
	/// Defers language-server startup until the first matching operation.
	pub lazy:                    bool,
	/// Searches project-local bin directories before `$PATH` for server
	/// executables. Off by default; user-level only.
	pub project_binaries:        bool,
	/// Formats supported files after a write transaction.
	pub format_on_write:         bool,
	/// Returns diagnostics after write transactions.
	pub diagnostics_on_write:    bool,
	/// Returns diagnostics after edit transactions.
	pub diagnostics_on_edit:     bool,
	/// Suppresses unchanged diagnostics already surfaced in this session.
	pub diagnostics_deduplicate: bool,
}

impl Default for LspSettings {
	fn default() -> Self {
		Self {
			enabled:                 true,
			lazy:                    true,
			project_binaries:        false,
			format_on_write:         false,
			diagnostics_on_write:    true,
			diagnostics_on_edit:     false,
			diagnostics_deduplicate: true,
		}
	}
}

impl LspSettings {
	/// Resolves language-server policy from the process control context.
	#[must_use]
	pub fn from_con(ctx: &Ctx) -> Self {
		Self {
			enabled:                 SV_LSP_ENABLED.get(ctx),
			lazy:                    SV_LSP_LAZY.get(ctx),
			project_binaries:        SV_LSP_PROJECT_BINARIES.get(ctx),
			format_on_write:         omp_tools::settings::SV_LSP_FORMAT_ON_WRITE.get(ctx),
			diagnostics_on_write:    omp_tools::settings::SV_LSP_DIAGNOSTICS_ON_WRITE.get(ctx),
			diagnostics_on_edit:     omp_tools::settings::SV_LSP_DIAGNOSTICS_ON_EDIT.get(ctx),
			diagnostics_deduplicate: omp_tools::settings::SV_LSP_DIAGNOSTICS_DEDUPLICATE.get(ctx),
		}
	}

	/// The document authority's native language-server options.
	#[must_use]
	pub const fn native_options(&self) -> NativeLspOptions {
		NativeLspOptions {
			enabled:          self.enabled,
			lazy:             self.lazy,
			project_binaries: self.project_binaries,
		}
	}
}

#[cfg(test)]
mod tests {
	use omp_con::{ConError, Source};
	use omp_core::Str;

	use super::*;

	#[test]
	fn defaults_match_pi_policy_without_shared_toggle() {
		assert_eq!(LspSettings::from_con(&Ctx::new()), LspSettings::default());
	}

	#[test]
	fn con_projection_is_typed() {
		let ctx = Ctx::new();
		SV_LSP_ENABLED.set(&ctx, false).expect("set enabled");
		omp_tools::settings::SV_LSP_DIAGNOSTICS_ON_EDIT
			.set(&ctx, true)
			.expect("set diagnostics");
		assert_eq!(LspSettings::from_con(&ctx), LspSettings {
			enabled: false,
			diagnostics_on_edit: true,
			..LspSettings::default()
		});
	}

	#[test]
	fn project_binaries_default_off_and_user_level_opt_in() {
		assert!(
			!LspSettings::from_con(&Ctx::new())
				.native_options()
				.project_binaries
		);
		let ctx = Ctx::new();
		SV_LSP_PROJECT_BINARIES
			.set(&ctx, true)
			.expect("user-level set");
		assert!(
			LspSettings::from_con(&ctx)
				.native_options()
				.project_binaries
		);
	}

	#[test]
	fn a_project_overlay_can_never_enable_project_binaries() {
		let ctx = Ctx::new();
		let denied = ctx
			.exec("sv_lsp_project_binaries true", Source::Project(Str::new_static("config.cfg")))
			.expect_err("project overlay is denied");
		assert!(matches!(denied, ConError::ProjectVarDenied { .. }), "{denied:?}");
		assert!(!LspSettings::from_con(&ctx).project_binaries);
	}
}
