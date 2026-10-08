# Project-sourced input trust audit (parked)

Status: **audit only; nothing here is approved or implemented.** Static read of omp2 at
`1246634b5c` by a read-only agent; no code was run. VERIFIED = read in code, INFERRED =
reasoned, UNCONFIRMED = not determined. Paths are under `crates/`. Not authoritative:
`PLAN.md` and code outrank it. Recorded 2026-10-06. F11 was re-read on 2026-10-07 at `a76c5d67e1`
(read-only again; nothing was run) and re-rated; see "F11 detail".

## Bottom line

- PR #167 holds: a project cfg overlay can only `set`/`reset` the five PROJECT convars;
  `bind`, `alias`, `exec` and all other commands are denied (`con/src/ctx.rs:1674-1721`).
- Workspace trust exists as data only (status at `705788b466`, 2026-10-08). `omp-ext` now has
  `WorkspaceTrust`, the `[[workspace_trust]]` grant rows, `evaluate` and the gated-input
  inventory with its digest (PR2, #191 and #196). Nothing outside `omp-ext` reads a trust
  decision or the digest: the loaders in envd, driver and app import only the path names
  from the inventory. No input is gated yet, so every finding below is still open. The other
  trust machinery (`ext/src/trust.rs`) covers installed or plugin-packaged code, not bare repo
  files.
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
| F11 | `.omp/workflows/*.toml` | `Command` phases run project-chosen argv unsandboxed, unapproved, with the full environment, only via explicit `omp adw run`; the posture the run reports is not what the phase gets. Detail F11a-F11e below. | Medium (re-rated from Low, 2026-10-07) |
| F12 | The five PROJECT convars | `ai_model`/`ai_model_roles` can steer to any catalog model on the user's credentials; `cl_disabled_extensions` can disable a user's guard extension. | Low-Medium |
| F13/14 | `.omp/config.toml [extensions]`, `omp.lock` (inert today); ancestor scanning of context files | Informational. | Info |

Safe or acceptable: `.omp/prompts`, `.omp/themes`, `.envrc` (direnv's allow list), `.omp/rules`
and skills as data (containment exists).

## F11 detail: `omp adw run` workflows (re-read 2026-10-07)

Static read of `driver/src/adw/{definition,production,mod}.rs`, `adw/src/profile.rs`,
`app/src/adw_cmd.rs` and `driver/src/cleanse/checkers.rs` at `a76c5d67e1`. No code was run.

- **F11a (VERIFIED) Code phases run unconfined.** `ProductionAdwHost::run_command`
  (`driver/src/adw/production.rs`) resolves the executable and spawns it with a bare
  `tokio::process::Command`. There is no `omp-sandbox` wrapper, no envd exec authority, no
  approval and no secret masking, and nothing clears the environment, so the child inherits the
  process environment, provider API keys included. The argv comes verbatim from the project's
  TOML (`PhaseSpec::Command`, `definition.rs`).
- **F11b (VERIFIED) Resolution shadows `$PATH`.** `FilesystemResolver::resolve`
  (`driver/src/cleanse/checkers.rs:32-60`) tries, for the phase cwd and then the project root,
  `<name>`, `node_modules/.bin/<name>`, `.venv/bin/<name>`, `venv/bin/<name>` and
  `vendor/bin/<name>` before it reads `$PATH`. `command = ["cargo", "test"]` therefore runs an
  executable file named `cargo` at the repo root. Same family as F3. The loader rejects a binary
  containing `/` or `\` and rejects a `cwd` that is absolute or contains `..`, but only
  lexically (`escapes_project`, `definition.rs`); a symlinked directory inside the repo is not
  checked (UNCONFIRMED by test).
- **F11c (VERIFIED) The reported posture is not applied to code phases.** `posture()` derives
  write, network and approval scope from the sandbox convars and the sandbox probe, and the
  domain compares each phase's `requires` against it (`Requirement::check`, `adw/src/profile.rs`).
  A code phase never goes through the sandbox, so whatever the check admits runs unconfined. On
  a default host where the sandbox is constructed the posture is `workspace-write` with
  `scoped` network, so a phase that declares `write = "workspace-write"` or
  `network = "scoped"` passes and then runs unconfined; with `sv_sandbox_network_mode disabled`
  set, `network = "disabled"` passes too. Under the defaults `network = "disabled"` is refused
  (`a_phase_requiring_no_network_is_refused_under_the_default_posture`,
  `driver/src/adw/production.rs`), and on a host where the sandbox cannot be constructed the
  posture is unconfined, so any write or network requirement tighter than that is refused. That
  is the outcome the module doc rules out ("a workflow that believes it ran confined when it did
  not"). `requires` is itself declared in the project file, and an unset
  field means no check. ADR 0039 B5 states the intended rule: admissibility uses the sandbox the
  worker actually enforced, "never self-described intentions".
- **F11d (VERIFIED) `omp adw run` ignores operator configuration.** `adw_cmd::execute` builds
  `Arc::new(omp_con::Ctx::new())` and never goes through `process_ctx`, so `config.cfg` and the
  project overlay are not loaded. Posture and child kernels use convar defaults. An operator who
  configured a stricter sandbox mode or `always-ask` approvals does not get it here.
- **F11e (VERIFIED, with one UNCONFIRMED) Agent and review phases.** They run through
  `compose_kernel` with `KernelOptions { ephemeral: true, ..Default::default() }`: every tool,
  and `LaunchExtensionPolicy::default()` (`native_mode = Merge`, `include_workspace = true`), so
  workspace `.omp/extensions` are admitted in each child (F2). The project's prompt text is the
  child's turn input. UNCONFIRMED: how a child kernel settles an approval-requiring call when no
  approver is attached (print mode denies immediately; the adw child was not traced).
- **Mitigations (VERIFIED).** Nothing starts a workflow automatically: the only caller is the
  `Adw` command dispatch in `app/src/cli.rs`, and `omp adw list` only parses and prints (an
  invalid file echoes its parse error). The operator names the workflow, which is closer to
  running `make` in a clone than to F1-F3. Runs are not journaled and cannot resume (ADR 0001,
  ADR 0039), so nothing durable records what ran.
- **Re-checked 2026-10-08 at `705788b466`.** F11a, F11b and F11d are unchanged: `run_command`
  still spawns with a bare `Command::new(resolved)` and `adw_cmd::execute` still builds
  `Ctx::new()`. F11c is narrower but not closed: `posture()` now takes the network scope from
  `network_confinement` (what shell sessions get; an explicit sandbox `off` reports
  `Unrestricted`), with tests, but a code phase is still not run under that confinement. The
  workflow directory moved into the trust inventory (`WORKFLOWS_DIR`, `WORKFLOW_EXTENSION`), so
  the digest covers `.omp/workflows`; `omp adw run` does not consult trust.
- **Proposed fixes (not decided).** (1) Route command phases through the envd exec authority or
  `omp-sandbox` with the resolved posture, so the check describes what runs. (2) Resolve code-phase
  binaries from `$PATH` only, or give repo-local roots the F3 treatment. (3) Build the context with
  `process_ctx_with` so operator settings apply. (4) Gate the command through the workspace-trust
  decision below (`adw` is already in the gated list). (5) Until trust lands, disable
  `include_workspace` extensions for adw children.

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
- PR1 (~300; implemented as `omp_core::project_file`, F5/F9 closed): contained project-file reader (regular file, no root escape, size cap) applied
  to cfg load/write, context files, SYSTEM/APPEND, whole-file rules, `secrets.yml`,
  `hosts.toml`, lsp configs. Fixes F5 and F9 without a trust decision.
- PR2 (~250; implemented in two parts): PR2a `omp-ext` `WorkspaceTrust`, the
  `[[workspace_trust]]` grants table keyed by canonical workspace path, `evaluate`, the grant-file
  lock; PR2b `omp_ext::workspace_trust::inventory`, the gated-input inventory and its
  `omp.workspace-trust.inputs.v1` digest (gated and excluded inputs listed in `crates/ext/README.md`).
  The inventory owns every gated path name and the envd/driver loaders import it. Found in code
  beyond the findings above: `.omp/TITLE_SYSTEM.md`, `.omp/vaults.toml` (project vaults shadow user
  vaults and may name any absolute root), `.agent/plugins`, `.agents/plugins`,
  `.claude/settings.local.json`, and the YAML `lsp`/`dap` names.
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
   (ADR 0028 amendment): the sandbox is now on by default (`workspace-write`) and the default `yolo` holds
   only inside an active sandbox (else `write`); an explicit `yolo` is respected.
2. Should Untrusted still load AGENTS.md, rules and skills (contained) or withhold them too?
3. Trust per workspace, or per inputs digest (re-ask after `git pull`)? Proposal: both.
4. Should `Subtree` trust exist (a trusted `~/src` trusts every clone below)?
5. Is native `.omp/extensions` auto-scan a dev path only? Keep behind trust, or replace with
   an explicit link/lock record?
6. Forbid `!cmd`/env-name substitution for project MCP files regardless of trust?
7. Should `cl_disabled_extensions` lose its PROJECT flag?
8. Gate all foreign-ecosystem MCP files, or only `.omp/mcp.json` and `.mcp.json`? Answered
   (PR2b): the digest covers every MCP kind envd classes as project-scoped.
9. Is a new typed trust method acceptable on the `proto`/`rpc` wire contract?
10. UNCONFIRMED follow-ups: whether chat startup installs a cfg reply sink (F9 echo); exact
    LSP lazy-start triggers; whether the extension-host sandbox env is filtered; whether any
    `ai_model` selector can name an arbitrary endpoint.
11. F11: should `omp adw run` require the same workspace-trust grant as the other gated inputs,
    and should code phases run inside the sandbox (F11a, F11c) before or after that gate exists?
    Should `omp adw run` load operator configuration (F11d)?
