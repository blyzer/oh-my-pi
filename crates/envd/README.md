# omp-envd

`omp-envd` is OMP's live project-environment host. It assembles and serves the
environment daemon and owns project-scoped filesystem and document access,
process execution, workspace search, blob storage, tool dispatch, policy, and
extension runtime resources exposed through the environment protocol.

This is the crate to change for host behavior. `omp-env` is only the typed
client and framing boundary; it does not contain an alternate host.

## Structure

- `server` owns environment-service dispatch, project state, client
  connections, and the `EnvServer`/`EnvdError` server boundary.
- `workspace`, `docs`, `document_cache`, `search_backend`, and `tool_search`
  provide workspace, search, and document operations.
- `exec`, `process_store`, `process_log`, and `direnv` manage
  commands, named processes, logs, and shell environment setup.
- `tools` and the `tool_*` modules implement daemon-backed tool operations.
- `exthost` owns extension manifests, lifecycle, CONTROL routing, quotas,
  service routing, cancellation, and the sole Python extension child role,
  `__omp-ext-host`.
- `eval` owns the lazy, killable `__omp-eval-child` machinery. The built-in
  `py_eval` is an Environment-routed tool backed by a fresh disposable eval
  namespace for each call.
- `worker_pool` owns named-worker placement and generation-fenced DATA
  transport. Named workers are distinct from extension hosts and eval
  children; they are not a legacy Python tool-child route.
- `policy`, `admission`, `http_egress`, `vault`, and `recovery` enforce access
  decisions and manage durable runtime state.
- `exec_sandbox` compiles the command sandbox (`sv_sandbox_mode`, on by
  default as `workspace-write`). `admission::effective_approval_mode` is the
  one rule that joins it to approval: the default `yolo` holds only while a
  sandbox was actually constructed (`SandboxState::Active`); otherwise `write`
  is in force. An explicit `yolo` (flag or user config) is respected. With no
  active sandbox the `bash` tool resolves to the `exec` tier, and one typed
  `approval-posture` notice reports the downgrade or the unconfined `yolo`.
  The sandbox's in-shell path check walks each path as the kernel does
  (links followed in place, `..` applied after them) and a redirection opens
  exactly the path it judged. Under the default `host` read mode reads follow
  symlinks and are refused only by `read_deny` (a walk that enters or ends in
  a denied root, or a `..`-collapsed spelling inside one), so toolchains
  reached through links (Homebrew, rustup, nix profiles) run without an
  amendment. Restricted read modes refuse a walk through a symlink outside
  the runtime roots, and redirections that write refuse any; builtin writes
  gated by `check_write` resolve links and judge the target.
  The network defaults to `scoped` (`sv_sandbox_network_mode`): each shell
  session owns an egress broker (`sandbox_proxy`) that proxy-aware clients
  reach. `NetworkConfinement` is the one answer for what applies: a defaulted
  network mode never sandboxes an explicit `sv_sandbox_mode off`, eval cells
  and detached processes (which hold no broker token) get `disabled`, and a
  broker that cannot start under the shipped default disables the network
  instead of failing every command. Every broker refusal carries a typed
  cause; only a policy refusal is an amendable network fact, so a loopback
  name or a non-routable IP literal offers no approval the rerun would
  refuse again, and a name that does not resolve stays an ordinary failure.
  Network trouble reaches the model as one `sandbox` diag
  (`exec_network_diag`): the refused `host:port` with the mode and remedy on
  every command that records a refusal, or, for a failed command whose
  stderr shows a resolver or connection failure, the mode's generic text
  once per session.
- `run` starts the platform transport. `ProjectEnvironment::attach` joins the
  build-keyed detached daemon and composes session-only tools locally.

## Document authority (docserver module)

The `docserver` module is the project-scoped authority over document state,
portable filesystem values, revision-aware edits, transactions, file watching,
and language-server sessions. Connection-specific behavior remains isolated in
sessions, while bounded protocol framing and adapters keep LSP and edit formats
from becoming independent sources of state.

The `omp` executable recognizes the hidden `__omp-eval-child` and
`__omp-ext-host` arguments because those children re-enter the same binary.
Their entry functions and runtime implementations remain owned by `omp-envd`;
`omp-app` only performs process-level dispatch. Parent processes do not
preflight-boot CPython: each Python child initializes its own interpreter.

## Project MCP configuration (mcp module)

MCP server definitions that a repository supplies (`.omp/mcp.json`, `.mcp.json`,
and the foreign editor files such as `.cursor/mcp.json`, `.vscode/mcp.json`,
`.codex/config.toml`, `opencode.json`) can start processes with the user's
authority, so they are **off by default**. The user-level archive convar
`sv_mcp_enable_project_config` opts in; it is deliberately not a project convar,
so a repository's own cfg overlay can never enable it. User-level files and
explicitly named or user-installed plugins always load.

Even after opting in, a project-scoped declaration
(`ConfigSourceKind::project_scoped`) resolves its `env` and `headers` values as
literal data: a `!command` value never runs and a value naming a process
environment variable is never substituted. Each such value is reported once as
a typed `LiteralValueNotice` (section and key, never the value) through the
`tracing` warning channel the other discovery diagnostics use. Move a
declaration that needs dynamic values into the user-level `~/.o2/mcp.json`.

## Philosophy

Each project and executable generation has one detached environment daemon.
Environment-locus tools — including opt-in `py_eval` — and filesystem,
process, document, browser, debugger, and memory effects execute there.
Session-locus tools, client-layer extension hosts, MCP, presenters, and agent
controls stay in the attaching process behind the same partitioned
`EnvClient`. Named-worker placement remains a separate execution facility. An
embedded full host is used only as a loud spawn fallback or by explicitly
isolated compositions.

The document socket is build-stable while environment sockets are build-keyed.
`DocumentHost` reconnects after a server restart, and a surviving current-build
environment may rehost the document authority without invalidating its clones.
A stale-build daemon drains without rehosting and releases authority as soon as
its last client disconnects.

The crate is deliberately below the headless driver and application layers.
Capabilities that require Director/goal state, inference composition,
application-authored content, host RPC resources, or telemetry delivery enter
through `RegistryBridges`. `omp-driver` constructs those bridges and the
session composition; `omp-envd` does not import app presentation policy.

## Development

Run `just setup-python` once before commands that link embedded Python. Then
use the workspace recipes:

- `just check-pkg omp-envd`
- `just test-pkg omp-envd`

Run joined behavior separately with `just e2e` or the exact narrower E2E
recipe shown by `just --list`.
