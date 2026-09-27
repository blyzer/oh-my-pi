//! Operator approval of plugin-launched processes through the composition's
//! seams: an installed plugin resolved from omp's registry, its launches
//! enumerated and gated by the environment host's MCP, LSP, and DAP
//! parsers, and approvals persisted in the `omp-ext` grant file and read back
//! by the next resolution.

use std::{fs, path::Path};

use omp_core::Str;
use omp_driver::plugin_commands::{approve_launch, blocked_launches, plugin_launches};
use omp_envd::docserver::{
	dap_adapter::builtin_adapters,
	dap_config::{discover_dap_sources, load_dap_config},
	lsp_config::{discover_lsp_sources, load_lsp_config},
};
use omp_ext::{
	claude_plugin::{
		ClaudePlugins, InstallScope, InstalledPluginEntry, InstalledPluginsRegistry,
		PluginDiagnostic, REGISTRY_FILE,
	},
	plugin_command::PluginLaunchKind,
	trust::{GrantsFile, grants_path},
};

fn write(path: &Path, body: &str) {
	fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
	fs::write(path, body).expect("write");
}

/// Records `tools@market` at `version`, rooted at `root`, in the user
/// registry under `data`.
fn install(data: &Path, root: &Path, version: &'static str) {
	let mut registry = InstalledPluginsRegistry::default();
	registry
		.plugins
		.insert(Str::new_static("tools@market"), vec![InstalledPluginEntry {
			scope:          InstallScope::User,
			install_path:   root.to_path_buf(),
			version:        Str::new_static(version),
			installed_at:   Str::new_static("2026-01-01T00:00:00Z"),
			last_updated:   Str::new_static("2026-01-01T00:00:00Z"),
			git_commit_sha: None,
			enabled:        true,
		}]);
	write(
		&data.join("plugins").join(REGISTRY_FILE),
		&serde_json::to_string(&registry).expect("registry"),
	);
}

fn lsp_declaration(args: &str) -> String {
	format!(
		r#"{{"acme":{{"command":"${{CLAUDE_PLUGIN_ROOT}}/bin/acme-lsp","args":{args},"extensionToLanguage":{{".acme":"acme"}}}}}}"#
	)
}

struct Fixture {
	_scratch: tempfile::TempDir,
	data:     std::path::PathBuf,
	project:  std::path::PathBuf,
	plugin:   std::path::PathBuf,
}

impl Fixture {
	fn new() -> Self {
		let scratch = tempfile::tempdir().expect("scratch");
		let root = scratch.path().canonicalize().expect("canonical scratch");
		let data = root.join("data");
		let project = root.join("project");
		fs::create_dir_all(&project).expect("project");
		let plugin = data.join("plugins/cache/plugins/market___tools___1.0.0");
		write(
			&plugin.join(".mcp.json"),
			r#"{"mcpServers":{"db":{"command":"${CLAUDE_PLUGIN_ROOT}/bin/db","args":["--stdio"]},"remote":{"type":"http","url":"https://example.test/mcp"}}}"#,
		);
		write(&plugin.join(".lsp.json"), &lsp_declaration(r#"["--stdio"]"#));
		write(
			&plugin.join(".dap.json"),
			r#"{"adapters":{"acme-dbg":{"command":"./bin/dbg","fileTypes":[".acme"]}}}"#,
		);
		install(&data, &plugin, "1.0.0");
		Self { _scratch: scratch, data, project, plugin }
	}

	fn resolve(&self) -> ClaudePlugins {
		let plugins = ClaudePlugins::resolve(&self.data, &self.project, None);
		assert!(plugins.diagnostics.is_empty(), "{:?}", plugins.diagnostics);
		plugins
	}

	/// Approves every launch the plugin currently declares, as
	/// `omp ext trust tools@market --approve-commands` does.
	fn approve_all(&self) {
		let plugins = self.resolve();
		let plugin = &plugins.plugins[0];
		for launch in plugin_launches(plugin) {
			approve_launch(&self.data, plugin, &launch, Str::new_static("cli"))
				.expect("persist approval");
		}
	}

	fn blocked_servers(&self) -> Vec<(PluginLaunchKind, Str)> {
		blocked_launches(&self.resolve())
			.into_iter()
			.map(|blocked| {
				assert_eq!(blocked.plugin, "tools@market");
				(blocked.kind, blocked.server)
			})
			.collect()
	}
}

#[test]
fn unapproved_plugin_launches_are_blocked_and_named() {
	let fixture = Fixture::new();
	let plugins = fixture.resolve();
	let blocked = blocked_launches(&plugins);
	assert_eq!(
		blocked
			.iter()
			.map(|blocked| (blocked.kind, blocked.server.as_str()))
			.collect::<Vec<_>>(),
		[
			(PluginLaunchKind::McpServer, "tools:db"),
			(PluginLaunchKind::LanguageServer, "acme"),
			(PluginLaunchKind::DebugAdapter, "acme-dbg"),
		],
		"every process-launching server awaits approval; the remote MCP server launches nothing"
	);
	let root = fixture.plugin.canonicalize().expect("canonical plugin");
	let lsp = &blocked[1];
	assert_eq!(*lsp.command(), root.join("bin/acme-lsp").to_string_lossy().as_ref());
	let notice = lsp.to_string();
	assert!(notice.contains("plugin `tools@market` language server `acme`"), "{notice}");
	assert!(notice.contains(&root.join("bin/acme-lsp").display().to_string()), "{notice}");
	assert!(
		notice.contains(&format!("omp ext trust tools@market --approve-command {}", lsp.digest)),
		"{notice}"
	);

	// The seams the environment host loads from leave them out.
	let lsp = discover_lsp_sources(None, &fixture.project, Vec::new(), &plugins.plugins)
		.expect("lsp discovery");
	assert!(matches!(&lsp.diagnostics[..], [PluginDiagnostic::CommandNotApproved(_)]));
	let config = load_lsp_config(&lsp.sources).expect("lsp config");
	assert!(!config.servers.contains_key("acme"), "an unapproved language server loaded");
	let dap = discover_dap_sources(None, &fixture.project, Vec::new(), &plugins.plugins)
		.expect("dap discovery");
	assert!(matches!(&dap.diagnostics[..], [PluginDiagnostic::CommandNotApproved(_)]));
	let adapters = load_dap_config(builtin_adapters(), &dap.sources).expect("dap config");
	assert!(!adapters.contains_key("acme-dbg"), "an unapproved debug adapter loaded");
}

#[test]
fn approved_launches_persist_and_load() {
	let fixture = Fixture::new();
	fixture.approve_all();

	let grants = GrantsFile::read(&grants_path(&fixture.data)).expect("grant file");
	assert_eq!(grants.plugin_commands.len(), 3);
	assert!(grants.plugin_commands.iter().all(|grant| {
		grant.plugin == "tools@market" && grant.version == "1.0.0" && grant.granted_by == "cli"
	}));

	assert!(fixture.blocked_servers().is_empty());
	let plugins = fixture.resolve();
	let lsp = discover_lsp_sources(None, &fixture.project, Vec::new(), &plugins.plugins)
		.expect("lsp discovery");
	assert!(lsp.diagnostics.is_empty(), "{:?}", lsp.diagnostics);
	assert!(
		load_lsp_config(&lsp.sources)
			.expect("lsp config")
			.servers
			.contains_key("acme")
	);
	let dap = discover_dap_sources(None, &fixture.project, Vec::new(), &plugins.plugins)
		.expect("dap discovery");
	assert!(dap.diagnostics.is_empty(), "{:?}", dap.diagnostics);
	assert!(
		load_dap_config(builtin_adapters(), &dap.sources)
			.expect("dap config")
			.contains_key("acme-dbg")
	);
}

#[test]
fn changed_arguments_or_version_require_approval_again() {
	let fixture = Fixture::new();
	fixture.approve_all();
	assert!(fixture.blocked_servers().is_empty());

	write(&fixture.plugin.join(".lsp.json"), &lsp_declaration(r#"["--stdio","--trace"]"#));
	assert_eq!(fixture.blocked_servers(), [(
		PluginLaunchKind::LanguageServer,
		Str::new_static("acme")
	)]);

	// Approving the changed launch admits it; the earlier approval stays
	// recorded but no longer matches anything.
	fixture.approve_all();
	assert!(fixture.blocked_servers().is_empty());
	let grants = GrantsFile::read(&grants_path(&fixture.data)).expect("grant file");
	assert_eq!(grants.plugin_commands.len(), 4);

	// A plugin update is a new version: every launch asks again.
	install(&fixture.data, &fixture.plugin, "1.1.0");
	assert_eq!(fixture.blocked_servers().len(), 3);
}
