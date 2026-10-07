# omp-ext

Extension configuration, dependency resolution, lockfiles, index metadata, and
local trust state for OMP.

## Structural philosophy

The crate is a pure domain: deterministic data transformations over declared
configuration and durable on-disk state. It owns no process, no connection, and
no CLI. Everything that needs a running Environment — Git materialization,
site publication — lives in the host or the CLI driver above it, so this crate
sits below both and can be reasoned about as data in, data out.

- `config`: declared extension configuration, overlays, scopes, and CLI
  contributions.
- `lock`: reproducible lockfiles and local installed/enabled records.
- `resolver`: the `uv` resolution driver and R1-R12 policy checks.
- `trust`: signature verification, trust tiers, and the local grant file
  (`<data>/ext/grants.toml`), including operator approvals of plugin-launched
  commands and workspace trust rows. Every write is a locked read-modify-write
  (`GrantsFile::update`/`try_update`, and the `persist*`/`revoke*` writers
  built on it) under the sibling `grants.toml.lock`, waited on for a bounded
  time, so a concurrent writer's stale read never brings back a revoked row.
  Only persistent rows are read or written.
- `workspace_trust`: operator trust in a workspace's own project-sourced
  inputs. `WorkspaceTrust` is `untrusted` by default and only the host sets
  it, never a convar. A `[[workspace_trust]]` row is keyed by the canonical
  workspace root and bound, by its `scope`, to an `InputsDigest`
  (`sha256:<hex>` of the gated inputs): `exact` trusts the workspace while
  its inputs keep that digest; `subtree` is an explicit grant over a root
  that never trusts a workspace on first use but asks once per workspace
  (`PinUnderSubtree`; a non-interactive host treats it as untrusted); `pin`
  is that answer, live only while the subtree row at exactly `under` is;
  `deny` is a revocation that outranks any covering subtree. A subtree at
  `/`, at the canonical home directory, or at an ancestor of it is refused,
  and when home cannot be canonicalized every subtree is. Containment is per
  path component (`/w` never covers `/w2`). `evaluate` is the pure decision
  (`Trusted`, `PinUnderSubtree`, `DigestChanged`, `Denied`, `Untrusted`;
  serializable for the journal). It takes the trust key separately from where
  the inputs are hashed, so a host can key an isolated worktree by its
  primary checkout. An operator's answer (`exact` or `pin`) is recorded by
  `GrantsFile::persist_workspace_trust` only while `evaluate`, run again
  under the grant file lock, still returns the decision the operator was
  asked about; a stale answer is refused, so it never undoes a revoke made
  after the ask. A pin also needs the live subtree row at exactly `under`
  and never replaces a deny. Subtrees are granted through
  `persist_workspace_subtree`; one granted anew drops the dormant pins naming
  its root, so it asks once per workspace again. `revoke_workspace_trust`
  matches the workspace's canonical path, then its spelling as given (a
  workspace deleted since), and records a deny only at a spelling rows are
  keyed by: never for a missing workspace no row names.
- `workspace_trust::inventory`: the gated project inputs and the
  `InputsDigest` over them. It is the single owner of every gated path name:
  envd's MCP, LSP/DAP, SSH host and vault loaders, the driver's native
  extension, skill, prompt, secret and workflow loaders, and
  `claude_plugin` import these constants, so a loader cannot read a gated
  input the digest misses (envd tests assert its loaders read exactly these
  names). See "Workspace trust inventory" below.
- `plugin_command`: the approval key (`Hash32` digest of plugin version,
  command, arguments, environment, working directory, a hook's event and
  matcher, and the contents of every plugin-root file the launch names, so a
  script edited in place asks again; a named file that cannot be read is
  never approvable) and typed refusal for every command a plugin would run: an
  installed plugin's MCP, LSP, and DAP servers and its hooks (approved under
  `name@marketplace`), and an Agent Plugins 1.0 package's stdio MCP servers
  (approved under its manifest name). Plugin resolution reads the approvals
  (`CommandApprovals`) and each launching seam gates through them. The
  digest domain is `omp.plugin-command.v2`: approvals recorded before files
  were bound match nothing, so plugin commands are approved once more.
- `index`, `upgrade`, `doctor`: index metadata, generation commits, and
  integrity diagnostics. A doctor finding carries its typed cause; the
  report's presenter renders it.
- `marketplace`, `claude_plugin`: Claude-compatible marketplace catalogs, the
  `installed_plugins.json` registry `omp ext install` writes, and the
  resolution of enabled installs into contained plugin roots whose skills,
  commands, rules, MCP, LSP/DAP servers, and hooks the driver and Environment
  discovery load. Claude Code's own registry merges in read-only
  (`ClaudeCodeHome`); omp's registries win for the same id.
- `claude_hooks`: Claude Code hook declarations (`hooks/hooks.json`, manifest
  `hooks`) parsed into typed hooks, plus the data tables mapping Claude events
  onto omp hook seams and Claude tool names onto omp tool families.

## Workspace trust inventory

`inventory(workspace, data_dir, budget)` reads a workspace's gated inputs and
digests them. The digest is SHA-256 over the domain tag
`omp.workspace-trust.inputs.v1\0`, the entry count (`u64` little-endian), then
each entry in path-byte order: the length of its `/`-joined relative path
(`u64` little-endian), the path, a `#[repr(u8)]` kind tag (`0` absent, `1`
bytes, `2` `enabledPlugins` projection, `3` tree file, `4` executable tree
file), and a 32-byte content hash (zero when absent). A golden test pins the
encoding; bumping the domain version asks about every workspace once more.

Gated (any change asks again):

| Input | Recorded as |
| --- | --- |
| Every MCP file envd classes as project-scoped: `.omp/mcp.json`, `.mcp.json`, `.claude/.mcp.json`, `.codex/config.toml`, `.gemini/settings.json`, `.opencode/opencode.json{c,}`, `opencode.json{c,}`, `.cursor/mcp.json`, `.windsurf/mcp_config.json`, `.vscode/mcp.json`, `mcp.json`, `mcp.config.json` | whole file, or absent |
| `lsp`/`dap` `.json`/`.yaml`/`.yml`, with and without a leading dot, in `.omp/` and at the root | whole file, or absent |
| `.omp/hosts.toml`, `.omp/vaults.toml`, `.omp/secrets.yml`, `.omp/SYSTEM.md`, `.omp/APPEND_SYSTEM.md`, `.omp/TITLE_SYSTEM.md` | whole file, or absent |
| `.claude/settings.json`, `.claude/settings.local.json` | only the `enabledPlugins` boolean entries plugin resolution reads (a permission edit does not ask again); the whole file when it is not JSON. The Claude Code installs they opt in live under the operator's Claude Code home and are not walked |
| `.omp/plugins/installed_plugins.json` | whole file, or absent |
| The root of every enabled install that registry records | inside the repository: walked as a tree; in the user plugin cache (`<data>/plugins/cache/plugins`): nothing, since the operator materialized it and its launches stay gated by `plugin_command`; anywhere else: refused (`ExternalPluginRoot`) |
| `.omp/extensions`, `.agent/plugins`, `.agents/plugins` | every file, walked whole (`__pycache__` and dotfiles included: an unchecked-hash `.pyc` loads without its source), with its execute bit |
| `.omp/workflows/*.toml` | whole file |

Excluded, with the reason:

- Context files (`AGENTS.md`, `CLAUDE.md`, ...), rules and skills outside the
  walked trees: data the model reads, never authority; they load contained
  (`omp_core::project_file`), and digesting them would ask again on every
  documentation edit.
- `.omp/config.cfg`: a project overlay may only `set`/`reset` the five
  PROJECT-flag convars; every other command is denied.
- `.omp/omp.lock` and `.omp/installed.toml`: inert records (audit F13) that
  load nothing by themselves.
- The `.omp/plugins` tree: a project-scope install links
  `.omp/plugins/node_modules/<name>` to the user plugin cache, so walking it
  would refuse every such workspace; the registry and in-repository install
  roots are what bind project plugins.
- Repo-local binary roots (`node_modules/.bin`, `.venv/bin`, `bin`, ...) that
  the LSP resolver searches: they are gated by the trust decision, not by the
  digest, so a trusted digest never vouches for them.
- `.DS_Store`, `Thumbs.db` and nested `.git` entries inside trees: inert, no
  loader reads them.

Every read goes through `omp_core::project_file` (contained in the repository,
regular files only, size-capped). A symbolic link inside a tree is followed
while its canonical target stays in the repository (a link back to a
directory being walked is not followed again); one that leaves is `Escapes`.
Any refusal, unreadable directory, or `InventoryBudget` overrun (files, bytes,
depth) is a typed `WorkspaceTrustError`: no digest, so nothing trusts the
workspace.

## Claude Code hook events

Every event of the Claude Code hooks reference either runs at the omp
lifecycle point with its semantics (`ClaudeHookEvent::seam`; the driver's
plugin hook host runs it) or is unsupported. A hook on an unsupported event
never runs, and the launching host names it once per plugin and event (chat
notice, print-mode `warning:` on stderr, and the RPC/ACP log): "plugin `id`
declares a `Event` hook: `Event` is not supported by omp; this hook will not
run". Every mapped hook still needs the operator's approval of its command and
trigger (`omp ext trust`).

| Event | omp lifecycle point | Notes |
| --- | --- | --- |
| `SessionStart` | `session_start` | Every session start of chat, print, RPC, and ACP: launch (`startup`, or `resume` on a journal that already holds a conversation), `/new` and ACP `session/new` (`clear`), `/resume`, ACP `session/load`/`session/resume`, and a hand-off (`resume`), a fork (`fork`). Context reaches the next prompt. |
| `SessionEnd` | `session_shutdown` | Quit or the end of an RPC/ACP run (`prompt_input_exit`), `/new` (`clear`), `/resume` (`resume`), a finished print run, fork, hand-off, signal, or failure (`other`). Runs within the 1.5 s shutdown budget, whatever its `timeout`; output discarded. |
| `UserPromptSubmit` | `before_agent_start` | Main session only. |
| `PreToolUse` | `tool_call` | |
| `PostToolUse` | `tool_result` (`ok`) | |
| `PostToolUseFailure` | `tool_result` (`faulted`) | |
| `Stop` | `agent_settled` | Main session only. |
| `SubagentStop` | `agent_settled` | Subagent kernels only. |
| `SubagentStart` | `before_agent_start`, a subagent's first prompt | Matcher on the agent class; `additionalContext` opens the subagent's context. |
| `StopFailure` | `agent_end` of a turn that failed on a provider error | `error` from the provider failure category; output discarded. |
| `PreCompact` | `compaction` | Exit 2 / `block` cancels the compaction. |
| `PostCompact` | `compaction_done` | `trigger` and `compact_summary`; output discarded. |
| `Notification` | `tool_approval_requested`, `agent_end` | The types in the table below; output discarded. |
| `Setup` | unsupported | omp has no `--init`/`--maintenance` run. |
| `UserPromptExpansion` | unsupported | Prompt templates expand without a lifecycle point (`command_invoke` is not emitted). |
| `PostToolBatch` | unsupported | No awaited point between a resolved tool batch and the next request: `turn_end` is a non-blocking observation and `turn_start` sees an already built request, so neither context nor a block could land before the next model call. |
| `PermissionRequest` | unsupported | `tool_approval_requested` is observe-only: a hook cannot answer omp's approval prompt. |
| `PermissionDenied` | unsupported | omp has no auto-mode classifier whose denial a hook could let the model retry. |
| `TaskCreated`, `TaskCompleted` | unsupported | omp's `todo` list has no create/complete lifecycle point a hook could roll back. |
| `TeammateIdle` | unsupported | omp has no agent teams. |
| `FileChanged` | unsupported | omp has no watched-file hook point. |
| `ConfigChange` | unsupported | Configuration changes have no blockable lifecycle point. |
| `CwdChanged` | unsupported | The session shell's working directory changes without a lifecycle point. |
| `DirectoryAdded` | unsupported | Directories are added only at launch (`--add-dir`). |
| `InstructionsLoaded` | unsupported | Context files and rules are discovered once at composition, without a lifecycle point. |
| `WorktreeCreate`, `WorktreeRemove` | unsupported | Subagent worktree isolation has no hook point that could replace creation or removal. |
| `PreModelSwitch` | unsupported | Model changes have no blockable point. |
| `PostModelSwitch` | `model_changed` | Every change of the selected model (a thinking-only change is no switch): `from_model`/`to_model` are the model ids the matcher filters on, `source` is `user_request` for a user or client selection and `automatic` for role routing and fallback, `effort.level` the effective thinking effort. Only `systemMessage` is honored. |
| `Elicitation`, `ElicitationResult` | unsupported | omp's MCP client does not offer elicitation, so no server ever asks. |
| `MessageDisplay` | unsupported | omp has no display-transform point: the TUI renders the journaled text, and `message_update` is an observation that cannot replace it. |

`Notification` hooks filter on `notification_type`
(`omp_ext::claude_hooks::NotificationType`); omp raises three types:

| Type | omp lifecycle point | Notes |
| --- | --- | --- |
| `permission_prompt` | `tool_approval_requested` | When the approval prompt is filed. |
| `idle_prompt` | `agent_end` of a main session | Once no prompt, switch, or session end followed the run within `sv_plugin_hook_idle_prompt` (60 s; `never` disables). |
| `agent_completed` | `agent_end` of a subagent | `agent_type` (its class), `agent_id`, and `summary` (the run's final assistant text). |
| `auth_success` | not raised | Provider and MCP logins run outside the session's hook surface. |
| `elicitation_dialog`, `elicitation_url_dialog`, `elicitation_complete`, `elicitation_response` | not raised | omp's MCP client does not offer elicitation. |
| `agent_needs_input` | not raised | A subagent waits on the user only at an approval prompt, which raises `permission_prompt`. |
| `quota_auto_resume_fired`, `quota_auto_resume_stale`, `quota_auto_resume_disabled` | not raised | omp has no quota auto-resume. |
