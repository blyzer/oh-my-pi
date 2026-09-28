//! Operator approval of plugin-launched commands through the composition's
//! seams: an installed plugin resolved from omp's registry, its launches
//! enumerated and gated by the environment host's MCP, LSP, and DAP
//! parsers and by its hook declarations, and approvals persisted in the
//! `omp-ext` grant file and read back by the next resolution.

use std::{fs, path::Path};

use omp_core::Str;
use omp_driver::plugin_commands::{
	agent_plugin_roots, approve_launch, blocked_launches, command_sets, plugin_launches,
};
use omp_envd::{
	docserver::{
		dap_adapter::builtin_adapters,
		dap_config::{discover_dap_sources, load_dap_config},
		lsp_config::{discover_lsp_sources, load_lsp_config},
	},
	mcp::McpConfigPaths,
};
use omp_ext::{
	claude_plugin::{
		ClaudePlugins, InstallScope, InstalledPluginEntry, InstalledPluginsRegistry,
		PluginDiagnostic, REGISTRY_FILE,
	},
	plugin_command::{PluginId, PluginLaunchKind},
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
	home:     std::path::PathBuf,
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
		write(
			&plugin.join("hooks/hooks.json"),
			r#"{"hooks":{"Stop":[{"hooks":[{"type":"command","command":"${CLAUDE_PLUGIN_ROOT}/bin/on-stop"}]}]}}"#,
		);
		install(&data, &plugin, "1.0.0");
		Self { home: root.join("home/.o2"), _scratch: scratch, data, project, plugin }
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
			approve_launch(&self.data, &plugin.id, &plugin.version, &launch, Str::new_static("cli"))
				.expect("persist approval");
		}
	}

	/// Discovery of Agent Plugins packages in the project and an empty user
	/// configuration root, gated by `plugins`' approvals.
	fn agent_plugins(&self, plugins: &ClaudePlugins) -> McpConfigPaths {
		McpConfigPaths::new(&self.home, &self.project)
			.with_command_approvals(plugins.command_approvals.clone())
	}

	fn blocked_servers(&self) -> Vec<(PluginLaunchKind, Str)> {
		let plugins = self.resolve();
		blocked_launches(
			&plugins,
			&self.agent_plugins(&plugins),
			&omp_envd::mcp::McpSettings::default(),
		)
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
	let blocked = blocked_launches(
		&plugins,
		&fixture.agent_plugins(&plugins),
		&omp_envd::mcp::McpSettings::default(),
	);
	assert_eq!(
		blocked
			.iter()
			.map(|blocked| (blocked.kind, blocked.server.as_str()))
			.collect::<Vec<_>>(),
		[
			(PluginLaunchKind::McpServer, "tools@market:db"),
			(PluginLaunchKind::LanguageServer, "acme"),
			(PluginLaunchKind::DebugAdapter, "acme-dbg"),
			(PluginLaunchKind::Hook, "Stop"),
		],
		"every process-launching server and hook awaits approval; the remote MCP server launches \
		 nothing"
	);
	let hook = blocked[3].to_string();
	assert!(
		hook.contains(&format!(
			"plugin `tools@market` hook `Stop` would run `{}`",
			fixture
				.plugin
				.canonicalize()
				.expect("canonical plugin")
				.join("bin/on-stop")
				.display()
		)),
		"{hook}"
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
	assert_eq!(grants.plugin_commands.len(), 4);
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
	assert_eq!(grants.plugin_commands.len(), 5);

	// A plugin update is a new version: every launch asks again.
	install(&fixture.data, &fixture.plugin, "1.1.0");
	assert_eq!(fixture.blocked_servers().len(), 4);
}

#[test]
fn an_installed_agent_plugins_package_is_approved_under_its_marketplace_identity() {
	let fixture = Fixture::new();
	let package = fixture
		.data
		.join("plugins/cache/plugins/market___portable___1.0.0");
	write(
		&package.join("plugin.json"),
		r#"{"$schema":"https://agent-plugins.org/schemas/1.0.0/plugin.schema.json","name":"portable","version":"1.0.0"}"#,
	);
	write(
		&package.join("mcp.json"),
		r#"{"$schema":"https://agent-plugins.org/schemas/1.0.0/mcp.schema.json","mcpServers":{
			"local":{"type":"stdio","command":"${PLUGIN_ROOT}/server"},
			"remote":{"type":"http","url":"https://example.test/mcp"}}}"#,
	);
	let mut registry = InstalledPluginsRegistry::default();
	registry
		.plugins
		.insert(Str::new_static("portable@market"), vec![InstalledPluginEntry {
			scope:          InstallScope::User,
			install_path:   package,
			version:        Str::new_static("1.0.0"),
			installed_at:   Str::new_static("2026-01-01T00:00:00Z"),
			last_updated:   Str::new_static("2026-01-01T00:00:00Z"),
			git_commit_sha: None,
			enabled:        true,
		}]);
	write(
		&fixture.data.join("plugins").join(REGISTRY_FILE),
		&serde_json::to_string(&registry).expect("registry"),
	);
	let agent_plugins = |plugins: &ClaudePlugins| {
		fixture
			.agent_plugins(plugins)
			.with_agent_plugin_roots(agent_plugin_roots(&[], plugins))
	};

	let plugins = fixture.resolve();
	let blocked =
		blocked_launches(&plugins, &agent_plugins(&plugins), &omp_envd::mcp::McpSettings::default());
	let [local] = &blocked[..] else {
		panic!("the package's stdio server alone awaits approval: {blocked:?}");
	};
	assert_eq!(
		(local.plugin.as_str(), local.server.as_str()),
		("portable@market", "portable@market:local"),
		"a marketplace install is identified, and its servers named, as `name@marketplace`"
	);

	// `omp ext trust portable@market` finds the package through its install;
	// its bare manifest name names no plugin.
	let sets =
		command_sets(PluginId::from_ref("portable@market"), &plugins, &agent_plugins(&plugins));
	let [set] = &sets[..] else {
		panic!("one package: {sets:?}");
	};
	assert_eq!((set.plugin.as_str(), set.version.as_str()), ("portable@market", "1.0.0"));
	assert!(
		command_sets(PluginId::from_ref("portable"), &plugins, &agent_plugins(&plugins)).is_empty()
	);
	for launch in &set.launches {
		approve_launch(&fixture.data, &set.plugin, &set.version, launch, Str::new_static("cli"))
			.expect("persist approval");
	}
	let plugins = fixture.resolve();
	assert!(
		blocked_launches(&plugins, &agent_plugins(&plugins), &omp_envd::mcp::McpSettings::default())
			.is_empty()
	);
}

/// With `sv_mcp_enable_project_config` off, MCP discovery loads no
/// project-scoped source: neither the project's Agent Plugins packages nor
/// the MCP servers of a plugin installed for the project. The report leaves
/// them out as well; user-scope packages, and the project plugin's language
/// servers, debug adapters, and hooks, are still named.
#[test]
fn disabled_project_mcp_config_reports_no_project_mcp_servers() {
	let fixture = Fixture::new();
	// Move the installed plugin from the user registry to the project's.
	let mut registry = InstalledPluginsRegistry::default();
	registry
		.plugins
		.insert(Str::new_static("tools@market"), vec![InstalledPluginEntry {
			scope:          InstallScope::Project,
			install_path:   fixture.plugin.clone(),
			version:        Str::new_static("1.0.0"),
			installed_at:   Str::new_static("2026-01-01T00:00:00Z"),
			last_updated:   Str::new_static("2026-01-01T00:00:00Z"),
			git_commit_sha: None,
			enabled:        true,
		}]);
	fs::remove_file(fixture.data.join("plugins").join(REGISTRY_FILE)).expect("user registry");
	write(
		&omp_ext::claude_plugin::project_plugins_dir(&fixture.project).join(REGISTRY_FILE),
		&serde_json::to_string(&registry).expect("registry"),
	);
	let package = |root: &Path, name: &str| {
		write(
			&root.join("plugin.json"),
			&format!(
				r#"{{"$schema":"https://agent-plugins.org/schemas/1.0.0/plugin.schema.json","name":"{name}","version":"1.0.0"}}"#
			),
		);
		write(
			&root.join("mcp.json"),
			r#"{"$schema":"https://agent-plugins.org/schemas/1.0.0/mcp.schema.json","mcpServers":{
				"local":{"type":"stdio","command":"${PLUGIN_ROOT}/server"}}}"#,
		);
	};
	package(&fixture.project.join(".agents/plugins/portable"), "portable");
	package(&fixture.home.join("agent/plugins/personal"), "personal");

	let plugins = fixture.resolve();
	assert_eq!(plugins.plugins[0].scope, omp_ext::claude_plugin::PluginScope::Project);
	let reported = |enable_project_config: bool| {
		blocked_launches(&plugins, &fixture.agent_plugins(&plugins), &omp_envd::mcp::McpSettings {
			enable_project_config,
		})
		.into_iter()
		.map(|blocked| (blocked.plugin, blocked.server))
		.collect::<Vec<_>>()
	};
	let named = |pairs: &[(&'static str, &'static str)]| {
		pairs
			.iter()
			.map(|(plugin, server)| (PluginId::new_static(plugin), Str::new_static(server)))
			.collect::<Vec<_>>()
	};
	assert_eq!(
		reported(true),
		named(&[
			("tools@market", "tools@market:db"),
			("tools@market", "acme"),
			("tools@market", "acme-dbg"),
			("tools@market", "Stop"),
			("portable", "local"),
			("personal", "local"),
		])
	);
	assert_eq!(
		reported(false),
		named(&[
			("tools@market", "acme"),
			("tools@market", "acme-dbg"),
			("tools@market", "Stop"),
			("personal", "local"),
		]),
		"project-scoped MCP servers are neither loaded nor reported"
	);
}

/// An approval binds the contents of the plugin files a launch names: a
/// server binary or hook script the plugin edits in place, without a new
/// version, asks again; a plugin file no launch names does not.
#[test]
fn editing_a_plugin_file_a_launch_names_requires_approval_again() {
	let fixture = Fixture::new();
	write(&fixture.plugin.join("bin/db"), "#!/bin/sh\nexec db --stdio\n");
	write(&fixture.plugin.join("bin/on-stop"), "#!/bin/sh\nexit 0\n");
	write(&fixture.plugin.join("README.md"), "tools\n");
	fixture.approve_all();
	assert!(fixture.blocked_servers().is_empty());

	write(&fixture.plugin.join("README.md"), "tools, edited\n");
	assert!(fixture.blocked_servers().is_empty(), "an unrelated plugin file is not bound");

	write(&fixture.plugin.join("bin/db"), "#!/bin/sh\nexec evil\n");
	write(&fixture.plugin.join("bin/on-stop"), "#!/bin/sh\nexec evil\n");
	assert_eq!(fixture.blocked_servers(), [
		(PluginLaunchKind::McpServer, Str::new_static("tools@market:db")),
		(PluginLaunchKind::Hook, Str::new_static("Stop")),
	]);
	fixture.approve_all();
	assert!(fixture.blocked_servers().is_empty());
}

/// Only an Agent Plugins package that comes from the project is
/// project-scoped. With `sv_mcp_enable_project_config` off, a package the
/// user installed and one the invocation names (`--plugin-dir`) still load
/// and are still reported; a project install is neither.
#[test]
fn user_installed_and_explicit_agent_plugins_ignore_the_project_mcp_policy() {
	let fixture = Fixture::new();
	let package = |root: &Path, name: &str| {
		write(
			&root.join("plugin.json"),
			&format!(
				r#"{{"$schema":"https://agent-plugins.org/schemas/1.0.0/plugin.schema.json","name":"{name}","version":"1.0.0"}}"#
			),
		);
		write(
			&root.join("mcp.json"),
			r#"{"$schema":"https://agent-plugins.org/schemas/1.0.0/mcp.schema.json","mcpServers":{
				"local":{"type":"stdio","command":"${PLUGIN_ROOT}/server"}}}"#,
		);
	};
	let cache = fixture.data.join("plugins/cache/plugins");
	let user_package = cache.join("market___personal___1.0.0");
	let project_package = cache.join("market___shared___1.0.0");
	package(&user_package, "personal");
	package(&project_package, "shared");
	let explicit = fixture.data.join("../explicit");
	package(&explicit, "explicit");
	let entry = |scope, install_path: &Path| InstalledPluginEntry {
		scope,
		install_path: install_path.to_path_buf(),
		version: Str::new_static("1.0.0"),
		installed_at: Str::new_static("2026-01-01T00:00:00Z"),
		last_updated: Str::new_static("2026-01-01T00:00:00Z"),
		git_commit_sha: None,
		enabled: true,
	};
	let mut user = InstalledPluginsRegistry::default();
	user
		.plugins
		.insert(Str::new_static("personal@market"), vec![entry(InstallScope::User, &user_package)]);
	write(
		&fixture.data.join("plugins").join(REGISTRY_FILE),
		&serde_json::to_string(&user).expect("registry"),
	);
	let mut project = InstalledPluginsRegistry::default();
	project
		.plugins
		.insert(Str::new_static("shared@market"), vec![entry(
			InstallScope::Project,
			&project_package,
		)]);
	write(
		&omp_ext::claude_plugin::project_plugins_dir(&fixture.project).join(REGISTRY_FILE),
		&serde_json::to_string(&project).expect("registry"),
	);

	let plugins = fixture.resolve();
	let explicit = explicit.canonicalize().expect("canonical package");
	let paths = fixture
		.agent_plugins(&plugins)
		.with_agent_plugin_roots(agent_plugin_roots(&[explicit], &plugins));
	let reported = |enable_project_config: bool| {
		blocked_launches(&plugins, &paths, &omp_envd::mcp::McpSettings { enable_project_config })
			.into_iter()
			.map(|blocked| blocked.plugin)
			.collect::<Vec<_>>()
	};
	assert_eq!(reported(true), ["explicit", "shared@market", "personal@market"]);
	assert_eq!(
		reported(false),
		["explicit", "personal@market"],
		"the project install alone follows the project MCP policy"
	);
}
