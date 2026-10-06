# Project-sourced input trust audit (parked)

Status: **audit only; nothing here is approved or implemented.** Static read of omp2 at
`1246634b5c` by a read-only agent; no code was run. VERIFIED = read in code, INFERRED =
reasoned, UNCONFIRMED = not determined. Paths are under `crates/`. Not authoritative:
`PLAN.md` and code outrank it. Recorded 2026-10-06.

## Bottom line

- PR #167 holds: a project cfg overlay can only `set`/`reset` the five PROJECT convars;
  `bind`, `alias`, `exec` and all other commands are denied (`con/src/ctx.rs:1674-1721`).
- No workspace trust exists. The only trust machinery (`ext/src/trust.rs`) covers installed
  or plugin-packaged code, not bare repo files.
- A cloned repo can run code with the user's authority when `omp` starts in it, with no
  prompt: project MCP config (F1), repo-local LSP binaries and `lsp.json` (F3), native
  `.omp/extensions` with self-declared grants (F2), and model tool calls driven by repo
  text (F4). Defaults make it worse: `sv_tools_approval_mode` = Yolo
  (`envd/src/tool_settings.rs:93-94`), `sv_sandbox_mode` = Off
  (`envd/src/exec_settings/sandbox.rs:149-150`); approvals never gate startup spawns.

## Findings (ranked)

| # | Input | Effect | Severity |
|---|---|---|---|
| F1 | `.omp/mcp.json`, `.mcp.json` and ~10 foreign MCP files (`.cursor`, `.vscode`, `.codex`, `opencode.json`, `mcp.json`...) | Stdio server spawned at env attach, user authority, full env, no sandbox, no approval (`envd/src/mcp/stdio.rs:448-470`). `!cmd` values run (`mcp/config_values.rs:105-111`); a value equal to an env var name resolves to its secret (`mcp/manager.rs:896`), so `headers.Authorization: "ANTHROPIC_API_KEY"` exfiltrates the key. Project (200) outranks User (199) on name collision. Gate `sv_mcp_enable_project_config` defaults true and is user-level. | Critical |
| F2 | `.omp/extensions/*` native Python | Python runs sandboxed (good), but the manifest's own `capabilities` become the DATA grants (`driver/src/discovery/native.rs:600`): `env.exec`, `env.net`, `env.fs.write` execute in envd outside the sandbox. No grants/lock/signature check. | High |
| F3 | `.omp/lsp.json`, `lsp.json`, `.lsp.json`, `.dap.json`; repo-local `node_modules/.bin`, `.venv/bin`, `bin` resolved before `$PATH` (`envd/src/docserver/lsp_binary.rs:57-104`) | Spawned unsandboxed; no config file needed, a `package.json` plus a `node_modules/.bin` binary suffices. Lazy start on first matching document (trigger INFERRED). | High |
| F4 | Yolo + Off sandbox defaults | Every prompt-injection input can drive `bash`/`fetch`/`write` with no prompt. | High (posture) |
| F5 | Context files (`AGENTS.md`, `CLAUDE.md`, `.cursorrules`, ...), `SYSTEM.md`, whole-file rules | Symlinks followed without containment (`driver/src/discovery/rules.rs:723-738`, `prompt_input.rs:56-61`): `AGENTS.md -> ~/.aws/credentials` sends it to the provider. `.omp/rules`, skills do check containment. | Medium-High |
| F6 | `.omp/SYSTEM.md` | Replaces the role block; project wins over user (`agent/src/prompt/mod.rs:79-84`). | Medium |
| F7 | `.omp/hosts.toml` | Shadows user SSH aliases with attacker host/key (`envd/src/ssh.rs:205-208`). | Medium |
| F8 | `.omp/plugins/installed_plugins.json`, `.claude/settings.json enabledPlugins` | Shadows user installs; skills/commands load ungated (`ext/src/claude_plugin.rs:751-921`). | Low-Medium |
| F9 | Project cfg file reads | Follows symlinks, no size or regular-file check (`driver/src/cfg.rs:109-199`): FIFO or `/dev/zero` hangs startup; clean-parsing files echo first words in diagnostics; `omp config set --scope project` writes through symlinks. | Low-Medium |
| F10 | `.omp/secrets.yml` | Can override user rules with identical content, cannot unmask. | Low |
| F11 | `.omp/workflows/*.toml` | `Command` phases run argv, only via explicit `omp adw run`. | Low |
| F12 | The five PROJECT convars | `ai_model`/`ai_model_roles` can steer to any catalog model on the user's credentials; `cl_disabled_extensions` can disable a user's guard extension. | Low-Medium |
| F13/14 | `.omp/config.toml [extensions]`, `omp.lock` (inert today); ancestor scanning of context files | Informational. | Info |

Safe or acceptable: `.omp/prompts`, `.omp/themes`, `.envrc` (direnv's allow list), `.omp/rules`
and skills as data (containment exists).

## Recommended design: one workspace-trust decision

- `WorkspaceTrust::{Untrusted, Trusted}` in `omp-ext`, default Untrusted, host-supplied via
  `KernelOptions`, not a convar, not settable from any cfg.
- Gated when Untrusted: all project MCP kinds, project/dotfile LSP/DAP sources and repo-local
  binary roots, native `.omp/extensions`, `.omp/hosts.toml`, project plugin registry and
  `enabledPlugins`, `SYSTEM.md`/`APPEND_SYSTEM.md`, project `secrets.yml` override, `adw`
  command phases. Still loaded as data with containment: context files, rules, skills,
  prompt templates, PROJECT convars, with a one-line notice of what was withheld.
- Approval floor: Untrusted raises the effective approval mode to at least Write.
- Stored in `<data>/ext/grants.toml` as a `[[workspace_trust]]` table (workspace URI, scope
  Exact|Subtree, duration Once|Session|Persistent, `inputs_digest` over the gated files so a
  changed `.mcp.json` re-asks, direnv style). Default scope Exact. Never inside the repo.
- Granting: first-run TUI overlay (default "Do not trust"), `omp trust [path] [--subtree]`,
  `omp trust --revoke`, `--trust-workspace` (Session only). Non-interactive modes default-deny
  with one structured notice; RPC/ACP get a typed trust request; subagents inherit.
- Journal-first: the decision is recorded once per session.

## PR sequence (each under ~500 lines)

- PR0 (stopgaps, ~130 lines): default `sv_mcp_enable_project_config` to false; treat
  project-scoped MCP env/headers as literals (no `!cmd`, no env-name lookup).
- PR1 (~300): contained project-file reader (regular file, no root escape, size cap) applied
  to cfg load/write, context files, SYSTEM/APPEND, whole-file rules, `secrets.yml`,
  `hosts.toml`, lsp configs. Fixes F5 and F9 without a trust decision.
- PR2 (~250): `omp-ext` `WorkspaceTrust`, grants table, inventory digest, round-trip tests.
- PR3 (~400): driver plumbing, default Untrusted; gate native extensions, SYSTEM/APPEND,
  plugin registry, secrets override; update e2e/driver tests that rely on workspace extensions.
- PR4 (~450): envd gating of MCP project kinds, LSP/DAP sources and local roots, hosts;
  replace the MCP bool with a typed policy.
- PR5 (~450): `omp trust`, `--trust-workspace`, non-interactive notice, RPC/ACP trust method.
- PR6 (~400): TUI first-run overlay (needs real-PTY proof per AGENTS.md).
- PR7 (~150): approval floor for untrusted workspaces.
- PR8 (optional): per-launch digest approval for trusted workspaces (reuse `CommandApprovals`).

## Open questions for the owner

1. Are the global Yolo approval default and Off sandbox default intentional? Answered 2026-10-06
   (ADR 0028 amendment): the sandbox is now on by default (`workspace-write`) and `yolo` is honoured
   only inside an active sandbox; without one, `write` is in force.
2. Should Untrusted still load AGENTS.md, rules and skills (contained) or withhold them too?
3. Trust per workspace, or per inputs digest (re-ask after `git pull`)? Proposal: both.
4. Should `Subtree` trust exist (a trusted `~/src` trusts every clone below)?
5. Is native `.omp/extensions` auto-scan a dev path only? Keep behind trust, or replace with
   an explicit link/lock record?
6. Forbid `!cmd`/env-name substitution for project MCP files regardless of trust?
7. Should `cl_disabled_extensions` lose its PROJECT flag?
8. Gate all foreign-ecosystem MCP files, or only `.omp/mcp.json` and `.mcp.json`?
9. Is a new typed trust method acceptable on the `proto`/`rpc` wire contract?
10. UNCONFIRMED follow-ups: whether chat startup installs a cfg reply sink (F9 echo); exact
    LSP lazy-start triggers; whether the extension-host sandbox env is filtered; whether any
    `ai_model` selector can name an arbitrary endpoint.
