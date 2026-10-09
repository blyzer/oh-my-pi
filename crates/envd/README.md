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
  is in force. An explicit `yolo` (flag or user config) is respected.
  `admission::resolve_approval` applies it per call with the tool's typed
  `omp_tool::Confinement` (`admission::call_approval_mode`, which `/security`
  also reports per confinement): the sandbox counts only for an `ExecSandbox`
  tool (`bash@2`, `hub@2`), so the default `yolo` covers just those, and a
  `Host` tool is admitted as if no sandbox existed (`write` for that call, so
  its exec tier prompts). With no active sandbox an `ExecSandbox` tool resolves
  to the `exec` tier, and one typed `approval-posture` notice reports the
  downgrade or the unconfined `yolo`. The host that registers a tool asserts
  its confinement; worker, extension and MCP declarations are always `Host`.
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
  instead of failing every command. Every refusal from the broker's
  CONNECT/SOCKS authorization and upstream connect carries a typed cause
  (the TLS ClientHello gate and a request on an inactive attempt token
  record none). Only a policy refusal is an amendable network fact, so a
  loopback name or a non-routable IP literal offers no approval the rerun
  would refuse again, and a name that does not resolve stays an ordinary
  failure (answered `502` with `X-Omp-Broker-Refused`, also when the client
  prints the headers). An `sv_sandbox_deny_domains` entry beats every
  approval: the broker records it as its own `deny-listed` refusal, which is
  never amendable and never prompts. Both are answered with the
  `X-Omp-Policy-Blocked` marker. A network amendment offers `once` or
  `session`; a path amendment only `once`. A `session` answer admits the
  endpoint for the rest of the session through the broker's attempt tokens:
  each attempt carries the session grants (`EgressGrants`) of the approval
  binding that issued its command, the in-process route or one daemon
  connection's relay, and the broker consults them live, with no restart and
  no recompiled profile. The grants are a cache of journaled decisions: a
  rebound route, a closed connection, a rewind and a switch to another
  session (`SessionGrants::revoke`, over the wire `RevokeApprovalGrants`)
  clear them, and the session's approval desk refills them from the journal
  it serves one refused attempt at a time.
  A command whose session-approved endpoint was refused may rerun again only
  for a new endpoint, at most `sv_sandbox_network_session_reruns` times
  (default 4); a `once` approval still ends the chain after one rerun.
  Network trouble reaches the model as one `sandbox` diag
  (`exec_network_diag`): a refusal is explained in full (endpoint, mode,
  remedy) the first time the session meets its `host:port` and cause, with
  the host left out when it cannot be quoted; a later failure on it gets one
  short line and a later success none. Without a refusal, a failed command
  whose stderr shows a resolver or connection failure gets the mode's
  generic text once per session. A disabled network has no broker to record
  anything, so there a failed command that launched a program able to run
  with a network URL among its arguments (seen through
  `SpawnWrapper::observe_launch`) gets the mode's locator text instead, which
  names the URL handed to a program rather than an output marker, from the
  same once-per-session slot. That covers a quiet client such as `curl -s`,
  but not one failing before a pipeline stage that succeeds
  (`curl -s URL | head`), nor a quiet client that bypasses the proxy under
  `scoped`.
- `approval_relay` carries a daemon command's sandbox amendment prompt to the
  session that issued the command, and answers it there. The
  daemon's host binds no approval route; an attached session advertises
  `approval-relay` in its hello, and only such an application connection to
  an environment host gets a relay. Extension connections never do, so
  extension code cannot approve its own commands. Each Exec, and each native
  invocation (through a task-local that `ShellExecHost::run` reads), captures
  the relay for its request, so the query travels only on that request and
  only that connection's answer decides it. The daemon builds the ticket from
  its own requirements. A command's relay outranks the host route an
  in-process composition binds; named processes carry none. Every unanswered
  path fails closed: the backstop is the prompt's timeout plus a 10 s grace, a
  cancelled command withdraws its query, and a closed connection denies its
  pending and later prompts, including those of commands that outlive it.
  Each relay is also an approval binding: the network endpoints its
  connection approves for the session stay with it, for that connection's
  commands only, and are cleared when it closes or sends
  `RevokeApprovalGrants`. On
  the session side, `pump_approval_queries` files each query on the route
  `ProjectEnvironment::bind_approval_authority` binds (the driver's kernel
  route, so the prompt is journaled and answered like an in-process one) with
  `request_cancellable`, and answers with the decision. A withdrawal, a closed
  transport or shutdown cancels the filed prompt without answering, and a
  query that arrives while no route is bound is decided by its unreachable
  rules, which deny a sandbox amendment.
  Embedded and isolated compositions advertise nothing and keep prompting
  through their in-process host. Only sandbox amendments are relayed: a
  daemon command's `dyn` admissions and privileged mutations still fail
  closed.
- `run` starts the platform transport. `ProjectEnvironment::attach` joins the
  detached daemon keyed by its build and by the session's sandbox and approval
  policy (`daemon_policy`), checks the policy that daemon reports in its hello,
  and composes session-only tools locally. It spawns no daemon whose
  configured policy (`AttachOptions::spawn_policy`) differs from the session's,
  and a spawn forwards the session's profile.

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

Each project, executable generation and daemon policy has one detached
environment daemon. A daemon compiles its command sandbox, egress broker and
approval posture from the control context it starts under and keeps them, so
the policy digest (`daemon_policy`: every `sv_sandbox_*` setting,
`sv_tools_approval_mode` with its provenance, `sv_tools_approval`,
`sv_shell_command_prefix` and `sv_fetch_enabled`) keys its socket, under a key
private to the project so the name never reveals the policy, and rides every
`ServerHello`; a client never joins a daemon that reports another policy and
runs an embedded host under its own instead.
Environment-locus tools — including opt-in `py_eval` — and filesystem,
process, document, browser, debugger, and memory effects execute there.
Session-locus tools, client-layer extension hosts, MCP, presenters, and agent
controls stay in the attaching process behind the same partitioned
`EnvClient`. Named-worker placement remains a separate execution facility. An
embedded full host is used only as a loud spawn fallback or by explicitly
isolated compositions.

The document socket is build- and policy-stable while environment sockets are
build- and policy-keyed.
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
