//! Behavioral coverage for workspace LSP management: discovery filtering,
//! lazy startup on a matching document open, and roster status transitions.

#![cfg(unix)]

use std::{fs, os::unix::fs::PermissionsExt, path::Path, time::Duration};

use omp_envd::docserver::{
	Environment, LspServerState, NativeLspSupervisor, ServerConfig,
	lsp_registry::{LspRegistryEvent, LspStartupStage},
};
use omp_ext::claude_plugin::ClaudePlugins;
use tempfile::TempDir;
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;

/// Minimal LSP server: answers `initialize` and `shutdown`, ignores the rest.
const FAKE_SERVER: &str = r#"#!/usr/bin/env python3
import json, sys

def send(identifier, result):
    payload = json.dumps({"jsonrpc": "2.0", "id": identifier, "result": result}).encode()
    sys.stdout.buffer.write(b"Content-Length: %d\r\n\r\n" % len(payload) + payload)
    sys.stdout.buffer.flush()

def read():
    length = None
    while True:
        line = sys.stdin.buffer.readline()
        if not line:
            return None
        if line in (b"\r\n", b"\n"):
            break
        name, value = line.decode("ascii").split(":", 1)
        if name.lower() == "content-length":
            length = int(value.strip())
    if length is None:
        return None
    return json.loads(sys.stdin.buffer.read(length))

while True:
    message = read()
    if message is None:
        break
    method = message.get("method")
    if method == "initialize":
        send(message["id"], {"capabilities": {}})
    elif method == "shutdown":
        send(message["id"], None)
"#;

fn fixture(project: &Path) {
	let server = project.join("fake-lsp.py");
	fs::write(&server, FAKE_SERVER).expect("write fake server");
	fs::set_permissions(&server, fs::Permissions::from_mode(0o700)).expect("chmod fake server");
	fs::write(project.join("foo.marker"), b"").expect("write marker");
	fs::write(
		project.join(".lsp.json"),
		serde_json::to_vec(&serde_json::json!({
			"servers": {
				"fake": {
					"command": server,
					"args": [],
					"fileTypes": [".foo"],
					"rootMarkers": ["foo.marker"],
				}
			}
		}))
		.expect("encode config"),
	)
	.expect("write config");
}

fn stage_of(supervisor: &NativeLspSupervisor, name: &str) -> LspServerState {
	supervisor
		.status()
		.into_iter()
		.find(|server| server.name.as_str() == name)
		.unwrap_or_else(|| panic!("server {name} missing from roster"))
		.state
}

#[tokio::test]
async fn lazy_open_starts_matching_server_and_publishes_startup_stages() {
	let scratch = TempDir::new().expect("scratch");
	let project = scratch
		.path()
		.canonicalize()
		.expect("canonical project root");
	fixture(&project);

	let environment =
		Environment::new(ServerConfig::new(project.clone()).expect("config")).expect("environment");
	let supervisor = NativeLspSupervisor::discover(&environment, None, Default::default())
		.expect("discover supervisor");
	environment.install_lsp_supervisor(supervisor.clone());
	let mut events = environment.lsp().subscribe_events();

	// Discovery admits the declaration lazily: present, not started.
	assert_eq!(stage_of(&supervisor, "fake"), LspServerState::Available);

	// A non-matching open starts nothing.
	supervisor.notify_open(&project.join("readme.md"));
	supervisor.wait_idle(&CancellationToken::new()).await;
	assert_eq!(stage_of(&supervisor, "fake"), LspServerState::Available);

	// A matching open starts the server and quiesces to Ready.
	supervisor.notify_open(&project.join("main.foo"));
	supervisor.wait_idle(&CancellationToken::new()).await;
	assert_eq!(stage_of(&supervisor, "fake"), LspServerState::Ready);

	// The startup lifecycle was published on the registry event bus.
	let mut stages = Vec::new();
	while let Ok(Ok(event)) = timeout(Duration::from_millis(200), events.recv()).await {
		if let LspRegistryEvent::Startup(event) = event {
			stages.push(event.stage);
		}
	}
	assert!(stages.contains(&LspStartupStage::Starting), "stages: {stages:?}");
	assert!(stages.contains(&LspStartupStage::Ready), "stages: {stages:?}");

	// A second matching open is a no-op: the server is pooled, not respawned.
	supervisor.notify_open(&project.join("other.foo"));
	supervisor.wait_idle(&CancellationToken::new()).await;
	assert_eq!(stage_of(&supervisor, "fake"), LspServerState::Ready);

	supervisor.shutdown().await;
	environment.shutdown().await;
}

#[tokio::test]
async fn failed_start_records_detail_and_keeps_roster_entry() {
	let scratch = TempDir::new().expect("scratch");
	let project = scratch
		.path()
		.canonicalize()
		.expect("canonical project root");
	// Executable exists (passes discovery) but exits immediately without
	// answering initialize, so startup fails.
	let server = project.join("broken-lsp.py");
	fs::write(&server, "#!/usr/bin/env python3\nraise SystemExit(1)\n").expect("write server");
	fs::set_permissions(&server, fs::Permissions::from_mode(0o700)).expect("chmod server");
	fs::write(project.join("foo.marker"), b"").expect("write marker");
	fs::write(
		project.join(".lsp.json"),
		serde_json::to_vec(&serde_json::json!({
			"servers": {
				"broken": {
					"command": server,
					"args": [],
					"fileTypes": [".foo"],
					"rootMarkers": ["foo.marker"],
					"warmupTimeoutMs": 2000,
				}
			}
		}))
		.expect("encode config"),
	)
	.expect("write config");

	let environment =
		Environment::new(ServerConfig::new(project.clone()).expect("config")).expect("environment");
	let supervisor = NativeLspSupervisor::discover(&environment, None, Default::default())
		.expect("discover supervisor");
	environment.install_lsp_supervisor(supervisor.clone());

	supervisor.notify_open(&project.join("main.foo"));
	supervisor.wait_idle(&CancellationToken::new()).await;
	let status = supervisor
		.status()
		.into_iter()
		.find(|server| server.name.as_str() == "broken")
		.expect("broken stays in roster");
	assert_eq!(status.state, LspServerState::Failed);
	assert!(status.detail.is_some(), "failure detail must be recorded");

	supervisor.shutdown().await;
	environment.shutdown().await;
}

/// Installs `(id, root, enabled)` plugins in a user registry under `data`.
fn install_plugins(data: &Path, installs: &[(&str, &Path, bool)]) {
	let mut plugins = serde_json::Map::new();
	for (id, root, enabled) in installs {
		plugins.insert(
			(*id).to_owned(),
			serde_json::json!([{
				"scope": "user",
				"installPath": root,
				"version": "1.0.0",
				"installedAt": "2026-01-01T00:00:00Z",
				"lastUpdated": "2026-01-01T00:00:00Z",
				"enabled": enabled,
			}]),
		);
	}
	fs::create_dir_all(data.join("plugins")).expect("plugin registry directory");
	fs::write(
		data.join("plugins/installed_plugins.json"),
		serde_json::to_vec(&serde_json::json!({"version": 2, "plugins": plugins})).expect("registry"),
	)
	.expect("write registry");
}

/// A plugin root shipping the fake server under `bin/` and a Claude-format
/// `.lsp.json` whose command and env name `${CLAUDE_PLUGIN_ROOT}`. The server
/// refuses to start unless the env value is its own plugin root.
fn plugin_fixture(root: &Path, server_name: &str) {
	let server = root.join("bin/fake-lsp.py");
	fs::create_dir_all(server.parent().expect("bin")).expect("plugin bin");
	let body = FAKE_SERVER.replacen(
		"import json, sys\n",
		"import json, os, sys\nif os.environ.get(\"PLUGIN_HOME\") != \
		 os.path.dirname(os.path.dirname(os.path.realpath(__file__))):\n    sys.exit(1)\n",
		1,
	);
	fs::write(&server, body).expect("write plugin server");
	fs::set_permissions(&server, fs::Permissions::from_mode(0o700)).expect("chmod plugin server");
	fs::write(
		root.join(".lsp.json"),
		serde_json::to_vec(&serde_json::json!({
			server_name: {
				"command": "${CLAUDE_PLUGIN_ROOT}/bin/fake-lsp.py",
				"env": {"PLUGIN_HOME": "${CLAUDE_PLUGIN_ROOT}"},
				"extensionToLanguage": {".foo": "foo"},
			}
		}))
		.expect("encode plugin config"),
	)
	.expect("write plugin config");
}

#[tokio::test]
async fn installed_plugin_server_joins_the_roster_and_starts_with_its_root_expanded() {
	let scratch = TempDir::new().expect("scratch");
	let scratch = scratch.path().canonicalize().expect("canonical scratch");
	let project = scratch.join("project");
	fs::create_dir_all(&project).expect("project");
	let enabled = scratch.join("enabled");
	let disabled = scratch.join("disabled");
	plugin_fixture(&enabled, "plugin-fake");
	plugin_fixture(&disabled, "ghost");
	let data = scratch.join("data");
	install_plugins(&data, &[("enabled@m", &enabled, true), ("disabled@m", &disabled, false)]);
	let plugins = ClaudePlugins::resolve(&data, &project, None);
	assert!(plugins.diagnostics.is_empty(), "{:?}", plugins.diagnostics);

	let environment =
		Environment::new(ServerConfig::new(project.clone()).expect("config")).expect("environment");
	let supervisor = NativeLspSupervisor::discover(&environment, None, plugins.plugins.into())
		.expect("discover supervisor");
	environment.install_lsp_supervisor(supervisor.clone());

	let status = supervisor.status();
	let fake = status
		.iter()
		.find(|server| server.name.as_str() == "plugin-fake")
		.expect("plugin server in roster");
	assert_eq!(fake.source, "plugin");
	assert_eq!(fake.state, LspServerState::Available);
	assert!(
		status.iter().all(|server| server.name.as_str() != "ghost"),
		"a disabled plugin contributed a server"
	);

	supervisor.notify_open(&project.join("main.foo"));
	supervisor.wait_idle(&CancellationToken::new()).await;
	assert_eq!(stage_of(&supervisor, "plugin-fake"), LspServerState::Ready);

	supervisor.shutdown().await;
	environment.shutdown().await;
}
