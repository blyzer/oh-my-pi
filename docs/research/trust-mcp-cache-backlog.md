# Project-input trust, MCP, cache, hosts and API-key backlog: status on `omp2`

Status: research; nothing here is approved or implemented
Date: 2026-10-11

Baseline: `origin/omp2` at `76ce331505` (merge of #251). Line numbers are at that commit. Sources:

- `docs/parked/2026-10-06-project-input-trust-audit.md` (findings F1-F14, PR0-PR8, owner questions);
- `docs/parked/2026-10-06-handoff-pending-work.md` (§1-§5);
- `docs/parked/2026-10-09-handoff-pending-work.md` ("Open on 2026-10-11" and "Carried over").

Evidence labels:

- **OBSERVED**: read in the repo or run in this session. The citation is `path:line` under `crates/`, or the command.
- **[INFERENCE]**: reasoned from the code but not exercised.
- **UNVERIFIED**: not determined.

No Rust was built or tested. The only commands run were reads, `rg`, `git`, `gh`, and three throwaway shell or Python probes of filesystem semantics (each under 3 s). Host: `hw.ncpu` 10. `vm.loadavg` was `28.93 19.99 18.87` at the start and `8.21 11.57 13.49` near the end. Another agent was compiling on the same machine.

## Bottom line

- **The auth and envd blockers are fixed on `omp2`.** Each was confirmed by its merge commit and the code; see "Fixed blockers". #251, which the 2026-10-11 handoff still listed as in CI, merged during this session (`76ce331505`).
- **Workspace trust is still data only.** `rg` for `TrustDecision`, `workspace_trust::evaluate`, `InputsDigest` and `WorkspaceTrust` outside `crates/ext` finds nothing. The loaders in `envd`, `driver` and `app` import only path constants from `omp_ext::workspace_trust::inventory`. Nothing is gated by trust (PR3-PR8 not started).
- **The remaining exposure from cloning a repo, ranked:**
  1. **F3 (High).** Repo-local language-server binaries (`node_modules/.bin`, `.venv/bin`, `bin`, ...) are preferred over `$PATH`. They start unsandboxed and without a prompt when a matching document opens.
  2. **F11 (Medium).** `omp adw run` code phases run unconfined, prefer repo-root binaries, and ignore operator cfg.
  3. **F2 (Medium, was High).** Native `.omp/extensions` load automatically. Their DATA effects are now bounded by declared tool effects and the approval tier.
  4. **F6 and F7 (Medium).** Project `SYSTEM.md` replaces the role block, and project `hosts.toml` shadows user aliases.
  5. **New (Low-Medium).** `.omp/vaults.toml` is read with a bare `fs::read_to_string` (it blocks on a FIFO) and can redirect a user vault name.
- **Fixed since the audit:**
  - F1 stopgap (#178): project MCP is off by default, and its values are literal.
  - F4 posture (#181, #197): sandbox on by default; a defaulted `yolo` holds only for sandbox-confined tools.
  - F5 and F9 (#180): contained project-file reader.
- **Still waiting on owner decisions:** the cache defaults (`ai_cache_retention auto` places no markers, and the Volatile band sits in the system prefix).
- **`--api-key` precedence over stored accounts already exists** in the broker since `fb0a7c0b06` (2026-08-22), so the handoff's "not started" is obsolete. What remains is the account the lease is attributed to.

## Fixed blockers (auth/envd), confirmed

| Item | PR / merge | Code evidence (OBSERVED) |
|---|---|---|
| Logout purges durable account state | #235 / `0f52afc3ba` | `ai/src/auth/manager.rs:3145` calls `purge_account` on the logout path; `ai/src/account/pool.rs:482` |
| Chat login finishes when the loopback callback wins | #238 / `12d76caf69` | `app/src/chat_services/accounts.rs:248-333` selects on session events |
| CLI login callback, Ctrl-C, `/launch` URL | #251 / `76ce331505` (merged 2026-10-11, this session) | `app/src/auth_cli.rs:583-596` takes a pasted `://` value; diff `d133d2e429..76ce331505` touches `ai/src/auth/oauth.rs`, `oauth/callback.rs`, `app/src/auth_cli.rs` |
| `InstrumentedTool` forwards `projection`, `authorize_visibility`, `stream_match_text` | #239 / `fa0492915e` | `envd/src/tools.rs:346-362` |
| `DoneProjected` visibility, `Detached` verdicts | #248 / `de41dc5fac` | `driver/src/headless/kernel.rs:1492`, `:1547-1560` |
| Large extension result spill | #248 | `envd/src/worker.rs:3335-3350` (`CONTROL_RESULT_INLINE_BYTES`, blob storage) |
| External eval requires Python >= 3.10 | #234 / `f0a1cf5944` | `envd/src/eval/process.rs:591` |
| Attached posture notice | #229 / `dec59c1287` | `driver/src/headless/kernel.rs:624` (`install_tool_authority`), `:531-537` (`rewound`/`switched` revoke) |
| BuildBuddy job after #250 | — | `gh run list --workflow buildbuddy-remote.yml --branch omp2`: `506b7c143e` success, `da8bf743bd` success, `76ce331505` in progress |

## Every backlog item: status and evidence

Status words: **open**, **fixed**, **partly fixed** (a residual is named), **obsolete**, **decision** (waiting only on the owner).

### Trust audit findings (F1-F14)

| # | Item | Status | Evidence (OBSERVED unless marked) | Severity now | Owner |
|---|---|---|---|---|---|
| F1 | Project MCP files spawn stdio servers with user authority; `!cmd` and env-name values | **partly fixed** | Stopgap (#178): `envd/src/mcp/settings.rs:11-20`, default `false`, user-level only. A project overlay is refused (`:63-70`). `config.rs:118-142` loads project kinds only when enabled. Values are literal (`mod.rs:1353-1426` test). **Residual:** opting in is one global switch for every project, with no per-workspace approval (needs trust). An opted-in stdio server still spawns unsandboxed and inherits the daemon environment: `mcp/stdio.rs:448-470` adds `config.env` and never calls `env_clear` (`rg env_clear` finds nothing in `envd/src/mcp`). Precedence is unchanged: Project 200 outranks User 199 (`config.rs:83-84`) | Medium (needs an opt-in); High for opted-in users | envd (+driver, app) |
| F1-UX | Project-scope `mcp add` tells the operator it stays inactive until trust or the explicit opt-in; literal project MCP values reach the session as typed notices | **fixed** | `app/src/{config_cmd.rs,chat_services/mcp.rs}` use the shared inactive-until-trusted hint. `envd/src/mcp/manager.rs` buffers and posts `mcp-literal-value` typed notices through the active session mailbox; `driver/src/headless/kernel.rs` binds the session host. | — | app, envd, driver |
| F2 | Native `.omp/extensions` load automatically, with self-declared DATA grants | **open (mitigated)** | Automatic: `LaunchExtensionPolicy::default` has `include_workspace: true` (`driver/src/headless/kernel.rs:120-125`), `chat_cmd.rs:115` keeps it unless `--no-workspace`, and `driver/src/discovery/native.rs:289-296` scans the workspace root. Grants come from the manifest (`native.rs:603`, `Grants::supported` in `envd/src/policy.rs:66-81`). **Mitigation since the audit:** per-invocation DATA effects are the intersection of the host grants and the Core-narrowed envelope (`policy.rs:1643`). The envelope must be a subset of the declared tool maximum (`worker.rs:721-740`). A call with no scope from a host connection is refused (`server.rs:12064-12068`). Extension tools are `Confinement::Host` (`envd/src/tools.rs:5922-5924`), so a defaulted `yolo` is `Write` for them (`admission.rs:241-252`). Exec sessions run under the active sandbox (`exec.rs:850`). [INFERENCE] What remains is automatic code execution in the extension host, plus write-tier effects auto-allowed under the default mode | Medium | driver (+envd) |
| F3 | Project LSP/DAP config, and repo-local binaries resolved before `$PATH` | **partly fixed** | Config reads are contained (#180): `envd/src/docserver/lsp_config.rs:107`, `:398-405`. **Binary resolution is open.** `lsp_binary.rs:57-69` searches local roots first, and `:85-105` lists `node_modules/.bin`, `.venv/bin`, `venv/bin`, `.env/bin`, `vendor/bundle/bin` and `bin`, each enabled by a marker file. The spawn (`lsp_process.rs:368-399`) is a bare `Command::new` with no sandbox, no approval and the inherited environment. The roster probe (`lsp_supervisor.rs:457-472`) uses the same roots. `tools/src/read.rs:660-667` documents that language servers start when a document opens, with no declared effect. The existing test `lsp_binary.rs:144-145` proves `node_modules/.bin` precedence. DAP launches are also bare (`dap_session.rs:1383`, `dap_protocol.rs:164,464`) | **High** | envd |
| F4 | Yolo plus sandbox-off defaults | **fixed** (#181, #197) | `envd/src/exec_settings/sandbox.rs:184-187` (`WorkspaceWrite`), `:193-196` (`Scoped`). `admission.rs:223-231`: a defaulted `yolo` without a confining sandbox is `Write`. `:241-252`: `Host` tools never see the sandbox | Low (posture) | envd |
| F5 | Context files and SYSTEM/rules follow symlinks out of the repository | **fixed** (#180) | `driver/src/discovery/rules.rs:779`, `:959`; `driver/src/prompt_input.rs:69-82`; `core/src/project_file.rs:145-181` | — | driver |
| F6 | `.omp/SYSTEM.md` replaces the role block; the project wins over the user | **open** | `driver/src/prompt_input.rs:63-75` (project first), `:131`. `agent/src/prompt/mod.rs:77-79` omits the role block when `custom_prompt` is set. `TITLE_SYSTEM.md` also takes project precedence (`prompt_input.rs:139`) | Medium | driver |
| F7 | `.omp/hosts.toml` shadows user SSH aliases | **open** | `envd/src/ssh.rs:222-225`: the project layer `extend`s, and so overwrites, user aliases. The read is contained (`:224`, `:242`) | Medium | envd |
| F8 | Project plugin registry and `enabledPlugins` shadow user installs | **open** | `ext/src/claude_plugin.rs:46-56`: an enabled project install shadows the user one, and `enabledPlugins` opts entries in. Launches are gated by `plugin_command` approvals (`ext/README.md:59-69`); skills and commands load ungated | Low-Medium | ext, driver |
| F9 | Project cfg reads and writes follow symlinks; FIFO and `/dev/zero` | **partly fixed** (#180) | `driver/src/cfg.rs:252`, `:287`, `:316-320`, `:339-341`. **Residual:** the `.cfg.lock` sibling, see §1.4 below | Low | driver |
| F10 | `.omp/secrets.yml` overrides user rules | **open (by design)** | `driver/src/secrets/config.rs:100-102`: a refused project file is an error, not a skipped file | Low | driver |
| F11a | adw code phases run unconfined with the full environment | **open** | `driver/src/adw/production.rs:114-120`: bare `Command::new(resolved)`, no sandbox, no `env_clear` | Medium | driver (+envd `ExecHost`) |
| F11b | Resolver shadows `$PATH` | **open** | `driver/src/cleanse/checkers.rs:33-56` tries `<root>/<name>`, `node_modules/.bin`, `.venv/bin`, `venv/bin` and `vendor/bin` under the phase cwd and the project root before `PATH`. `cleanse` shares it (`cleanse/production.rs:69`, `:142`). `escapes_project` is lexical only (`adw/definition.rs:214-220`) | Medium | driver |
| F11c | Reported posture is not applied to code phases | **open** | `production.rs:153-180` computes the posture; `run_command` (`:102-142`) never applies it | Medium | driver |
| F11d | `omp adw run` ignores operator cfg | **open** | `app/src/adw_cmd.rs:55`: `Arc::new(omp_con::Ctx::new())` | Medium | app |
| F11e | Agent phases admit workspace extensions | **open** | `production.rs:84-89`: `KernelOptions { ephemeral: true, ..Default::default() }`, so `include_workspace: true` | Medium | driver |
| F11f | Ctrl-C is not wired | **open** | `adw_cmd.rs:58`: the `CancellationToken` is created and never cancelled; the file has no signal handler | Low | app |
| F12 | Five PROJECT convars (`ai_model`, `ai_model_roles`, `ai_thinking`, `sv_tools`, `cl_disabled_extensions`) | **decision** | `agent/src/vars.rs:146`, `:164`, `:274`; `catalog/src/settings.rs:1134`; `driver/src/discovery/mod.rs:42`. `cl_disabled_extensions` can disable a user's guard extension (`native.rs:304-308`). Whether an `ai_model` selector can name an arbitrary endpoint: UNVERIFIED | Low-Medium | owner (con/driver) |
| F13/14 | `.omp/config.toml [extensions]`, `omp.lock`; ancestor scanning of context files | **info** | `ext/README.md:113-116` (inert records excluded from the digest) | Info | — |
| New | `.omp/vaults.toml`: uncontained read; project vaults shadow user vaults and may name any absolute root | **open** | `envd/src/vault.rs:1101-1112` uses `fs::read_to_string` (no regular-file check, no size cap, no containment). `:154-156`: project roots `extend` user roots. `:1119-1123` accepts absolute roots. It loads at environment open (`envd/src/tools.rs:4544`). Probe (OBSERVED): `cat` on a FIFO at `.omp/vaults.toml` blocked for more than 2 s, so [INFERENCE] the environment open hangs the same way. Scoped writes refuse `vault://` (`envd/src/write_scope.rs:239`, `:280`) | Low-Medium | envd |

### Handoff §1 (security) and the 2026-10-09 carried-over items

| Item | Status | Evidence | Severity | Owner |
|---|---|---|---|---|
| §1.1 Trust model PR3-PR8 | **open, not started** | No consumer of `evaluate`/`TrustDecision` outside `crates/ext` (`rg`, OBSERVED). PR2 exists: `ext/src/workspace_trust.rs:564` (`evaluate`), `ext/src/trust.rs:592-660` (locked writers), `ext/README.md:83-143` (inventory) | High (aggregate) | driver, envd, app, ext |
| §1.2 Explicit per-workspace MCP approval | **open** (needs §1.1) | See F1 residual | Medium | envd, driver |
| §1.3 F11, F2, F3 | **open** | See the table above | — | — |
| §1.4a `HostStore::upsert`/`persist_hosts` "follow symlinks" | **partly wrong as stated; residual open** | `persist_hosts` → `envd/src/lib.rs:327-358`: `create_dir_all(parent)`, a `create_new` temporary in the parent, then `rename`. Probe (OBSERVED, Python): `rename` over a final-component symlink replaces the link and never writes through it. A symlinked `.omp` directory is followed: the probe's file landed in the outside directory. `omp ssh add` defaults to `--scope project` (`app/src/ssh_cmd.rs:60`, `:68`). It loads that file with `HostStore::load` → `Containment::Unconfined` (`ssh.rs:155-157`), so the read follows a link out and can block on a FIFO | Low | envd, app |
| §1.4b `.cfg.lock` opened with `create(true)` | **open** | `driver/src/cfg.rs:295-301`. `check_write_target` (`:287`) checks the cfg path, not the lock. Probe (OBSERVED): `O_CREAT` through a dangling `.omp/config.cfg.lock` symlink created the target file outside the repository | Low | driver |
| §1.4c User-level `SYSTEM.md` and user cfg not hardened | **open** | `driver/src/cfg.rs:213-219` (`fs::read_to_string`); `prompt_input.rs:93-97`. `project_file.rs:13-15` says user files keep the regular-file and size checks under `Unconfined`; these call sites do not use it | Low | driver |
| §1.4d Windows path untested (`O_NOFOLLOW` unix-only) | **open** | `core/src/project_file.rs:128-132`. [INFERENCE] The canonicalize-then-check order still refuses an escaping link; only the swap-after-check race is unguarded on Windows | Low | core |
| §1.4e Refused project `secrets.yml` is a hard error | **by design** | `driver/src/secrets/config.rs:100-102` | — | owner |
| §1.5 `cl_disabled_extensions` PROJECT flag | **decision** | `driver/src/discovery/mod.rs:36-46` | Low-Medium | owner |
| §2 In-process builtins ignore `read_deny` | **open** | `shell-builtins/src/host.rs:254-262`: `resolve` only joins the cwd; `ensure_writable` (`:266-272`) does consult the policy. Opens without a policy check: `cat.rs:344-354`, `head.rs:1439`, `grep.rs:1023`, `:1177`. The shell checks reads only for programs and globs (`shell/src/commands.rs:228`, `shell/src/patterns.rs:302`). 99 `File::open`/`fs::read*` sites in `shell-builtins/src` (`rg -c`). `read_deny` defaults to empty (`envd/src/exec_settings/sandbox.rs:298-299`), so this matters once configured | Medium | shell-builtins (+envd seam test) |
| §2 Seatbelt items verified live 2026-10-08 | **fixed** | 2026-10-09 handoff, "Carried over" §2 | — | — |
| §2 Memory `reflect`: `MnemopiSettings.llm_mode` not consulted | **UNVERIFIED** | `memory/src/session.rs:151`, `:227` read `llm_mode`; whether the daemon relay path (`envd/src/reflection_relay.rs`) honours it was not traced | Low | envd/memory |
| 10-09 #4 Harnesses without `install_tool_authority` | **open (tests only)** | Only `driver/src/headless/kernel.rs:2977` and `driver/tests/approval_authority.rs` install it. `Kernel::new` without it: `app/src/chat_control.rs:3835`, `:4192`, `:4351` (all in its `mod tests`, which starts at `:3546`), `app/tests/{rpc_spine,acp_spine}.rs`, `driver/tests/{memory_reflect,plugin_hooks,hub_sessions}.rs`. No production bypass was found | Low (test fidelity) | driver/app tests |
| 10-09 #5 Disabled-network diag gaps | **accepted, documented** | ADR 0028 (not re-read this session) | Low | — |
| 10-09 #3 Model sees `approval-posture`? / unknown-scheme reads non-fetch? | **decision** | `tools/src/read.rs:655-683` (classes) | — | owner |

### Accounts (handoff §3) and the 2026-10-11 login item

| Item | Status | Evidence | Severity | Owner |
|---|---|---|---|---|
| Slice 2: `--api-key` precedence over stored accounts | **obsolete as stated; residual decision** | `ai/src/auth/broker.rs:228-255` (`with_api_key_override`, provider-scoped, scalar kinds only). `:520-522`: the invocation lease is answered before any plan source, stored store included. The test at `:996-1078` asserts that neither the environment nor the store is read. Wired from `driver/src/registry.rs:1204-1214` and `:1481-1492`. Added in `fb0a7c0b06` (2026-08-22), before the handoff. **Residual:** the lease keeps the pool-selected stored account and principal (`broker.rs:332-340`; asserted at `:1052-1053`), so usage, quota, rotation and pins are attributed to a stored account the key does not belong to. [INFERENCE] With no stored account, the pool's behaviour was not traced (UNVERIFIED). Chat refuses `--api-key` without `--model`/`--models` (`app/src/chat_cmd.rs:496-498`) | Low | ai (+owner) |
| Slice 3: model-pool preference | **obsolete** | No model-pool concept; the account pool is per provider (`docs/py/13-inference.md:453-454`) | — | — |
| Slice 4: project-level pins | **blocked on §1.1** | Proposal only | — | — |
| Chat prompt cancels on a pasted callback URL | **open** | `app/src/chat_services/accounts.rs:398-403`: a `Confirmation` answer other than empty, `y` or `yes` becomes `Cancel`. The CLI accepts the URL since #251 (`app/src/auth_cli.rs:595-596`) | Low (UX) | app |

### Inference cost and caches (handoff §4) and `omp-cache`

| Item | Status | Evidence | Severity | Owner |
|---|---|---|---|---|
| `ai_cache_retention auto` places no cache markers | **decision** | `catalog/src/settings.rs:1297` (default `Auto`); `ai/src/settings.rs:560-566` (`Auto` → `Setting::Unset`); `ai/src/codec/anthropic.rs:1687-1698` (`automatic_cache_ttl` returns `None`), `:1767-1768` (`ttl == None` places no markers) | Cost, not security | ai |
| Volatile band (turn, cwd, date, mounts, todos) in the system prefix | **open** | `agent/src/prompt/mod.rs:108-114` puts `status()` in `SlotClass::Volatile` as a system item. `agent/prompts/system/status.md` renders turn, cwd, date, mounts and todos. `agent/src/loop.rs:2486-2509`: system items, then the mode prompt, then the thread, so every turn rewrites the prefix after the Stable band | Cost | agent |
| Measure cache hits under the default config | **not done** | Needs an Anthropic key | — | — |
| Dedicated TUI message for `account-pin` | **open** | `rg account-pin` finds no hit in `crates/chat` (OBSERVED) | Low | chat |
| MCP definition cache (`omp-cache`) | **no action** | `cache/src/mcp_cache.rs:15-30` keys by server name and canonical config SHA-256, 30-day TTL, in the per-project `state_dir/mcp-cache.sqlite3` (`envd/src/server.rs:2493`). Cached definitions publish only for an installed mount (`envd/src/mcp/manager.rs:1942-1976`). [INFERENCE] Gating at mount time therefore covers the cache: an untrusted project server has no mount, so its cached rows are never published. A revoke leaves rows on disk until TTL or `remove_server` | Info | cache, envd |

## Implementation slices (ordered)

Each slice is one PR of about 500 lines or fewer. "Seam" is the crate whose public boundary the acceptance test exercises, per `AGENTS.md`. Host changes go to `omp-envd`, client protocol to `omp-env`, composition to `omp-driver`, and presentation to `omp-app`.

| # | Slice | Scope and files | Acceptance test (seam) | Depends on | Size | Owner decision? |
|---|---|---|---|---|---|---|
| 1 | **Repo-local LSP/DAP binaries off by default** (F3 stopgap) | `envd/src/docserver/{lsp_binary,lsp_process,lsp_supervisor}.rs`: pass no local roots unless a user-level convar (proposal: `sv_lsp_project_binaries`, default false, not PROJECT) enables them; same for DAP `program` resolution. Slice 4 later replaces the convar with the trust decision | envd unit: a root with `package.json` and `node_modules/.bin/<cmd>`, plus a `PATH` dir holding `<cmd>`, resolves the `PATH` one; the roster drops a server only the local root provides; with the convar on, the local one wins (the existing `lsp_binary.rs:144` test moves under it) | — | S | **Yes** (Q1): breaks users who rely on a project-pinned `typescript-language-server` |
| 2 | **adw and cleanse containment** (F11a-f) | `driver/src/cleanse/checkers.rs`: resolve code-phase binaries from `$PATH` only, or behind slice 1's rule. `driver/src/adw/production.rs`: run command phases through envd's exec authority (sandbox and posture), and give child kernels `include_workspace: false`. `app/src/adw_cmd.rs`: `process_ctx_with` instead of `Ctx::new()`, and cancel the token on Ctrl-C | driver: a repo-root executable `cargo` is not chosen for `command = ["cargo", …]`; a phase that declares `write = "workspace-write"` fails to write outside the workspace; the child `KernelOptions` excludes workspace extensions. app: a user `config.cfg` with `sv_sandbox_mode off` changes the reported posture. Ctrl-C needs a PTY probe (`.omp/tools/tui.ts`) | — | M (split: 2a resolver plus `include_workspace` plus cfg plus Ctrl-C, S; 2b exec routing, M) | Overlaps ADR 0040 Slice 1 PR 1d and Slice 2 (Local backend over `ExecHost`); **owner picks one home** (Q2) |
| 3 | **Trust plumbing in the driver** (audit PR3) | New `KernelOptions.workspace_trust` (host-supplied, never a convar). At `compose_kernel`: compute `inventory` and `evaluate` against `grants.toml` (and session rows), journal the decision once, and post a one-line "withheld" notice. Gate native `.omp/extensions` (`native.rs:289`), `SYSTEM`/`APPEND`/`TITLE_SYSTEM.md` (`prompt_input.rs:63`), the project plugin registry and `enabledPlugins`, the `secrets.yml` override, adw code phases. Subagents inherit the decision | driver: `compose_kernel` with `Untrusted` admits no workspace extension, ignores `.omp/SYSTEM.md`, journals `TrustDecision::Untrusted`; a `Trusted` row whose digest matches admits them; editing `.omp/mcp.json` gives `DigestChanged`. Update `e2e/tests/p7_tui.rs`, `driver/tests/plugin_commands.rs`, `app/tests/config.rs` (they rely on workspace inputs) | — | L (3a plumbing plus journal, M; 3b gates, M) | Q3, Q4, Q5 |
| 4 | **Trust gating in envd** (audit PR4) | `envd`: replace the `McpSettings.enable_project_config` bool with a typed policy (trusted, or the user opt-in); gate project LSP/DAP sources and local roots (subsumes slice 1); gate the project `hosts.toml` and `vaults.toml` layers. `env`: fold the trust decision into `DaemonPolicy` (`env/src/project_state.rs:22-34`), so trusted and untrusted clients never share a daemon (the #213/#215 pattern) | envd: an untrusted environment open mounts no project MCP server, its LSP roster has no project source or local root, and a project alias in `hosts.toml` does not resolve; env: two clients with different trust get different sockets and `ServerHello.policy_digest` | 3 | L (4a MCP plus policy digest, M; 4b LSP/DAP/hosts/vaults, M) | Q6, Q9 |
| 5 | **Builtins honour the read policy** (§2) | `shell-builtins/src/host.rs`: add `Host::open_read(path)` → `PathPolicy::open(path, OpenRequest { access: Read, .. })` (`shell/src/interp.rs:98-107`). Migrate every builtin read site (99 counted: `cat`, `head`, `tail`, `grep`, `rg`, `sed`, `sort`, `jq`, `diff`, `comm`, `cksum`, `wc`, `uniq`, `tac`, `sponge`, `tee`, `find`, ...); no dual path | envd: `cat <read_deny root>/file` in an exec session under `workspace-write` ends `Denied`; shell-builtins: the `HiddenRoot` policy (`shell/src/commands.rs:1350-1380`) refuses `cat`/`head`/`grep` reads | — | L (mechanical after the helper; `sonic`-able) | — |
| 6 | **Contained-reader residue** (§1.4, the vaults item) | envd: `vault.rs` `parse_vaults` through `project_file::read_text` (project `Within`, user `Unconfined`); `ssh.rs` `HostStore::load` takes a `Containment`, `ssh_cmd.rs` passes `Within` for the project scope; `persist_hosts` refuses a symlinked parent or target (`check_write_target`). driver: `cfg.rs` opens the lock with `O_NOFOLLOW` (or `check_write_target` on the lock path); user cfg and user `SYSTEM.md` through `read_text(.., Unconfined, ..)` | envd: a FIFO or outside symlink at `.omp/vaults.toml` is skipped with a warning and the environment opens (mirror `ssh.rs:1268-1315`); `omp ssh add` into a repo whose `.omp` links outside is refused. driver: a dangling `.omp/config.cfg.lock` symlink is refused and nothing is created outside | — | M (6a envd, S; 6b driver, S) | Whether a refused project `vaults.toml` is skipped (proposal) or an error |
| 7 | **`omp trust` CLI, `--trust-workspace`, RPC/ACP trust request** (audit PR5) | `app/src/cli.rs` and a new `trust_cmd.rs` over `GrantsFile::persist_workspace_trust`/`revoke_workspace_trust`; `--trust-workspace` (session only); one structured notice in print, RPC and ACP; `proto`/`rpc`: a typed trust request | app: `omp trust` then `omp chat` in the same tree loads the project MCP; `omp trust --revoke` withholds it again; `omp print` in an untrusted tree prints the notice and denies | 3, 4 | M-L | Q9 (wire) |
| 8 | **First-run TUI trust overlay** (audit PR6) | `crates/chat` and app; default "Do not trust" | Real-PTY proof through `.omp/tools/tui.ts`; the journal shows the decision | 7 | M | — |
| 9 | **Approval floor for untrusted workspaces** (audit PR7) | `envd/src/admission.rs`: an `Untrusted` decision clamps an explicit `yolo` to `write` | envd: `resolve_approval` with explicit `yolo` plus `Untrusted` prompts for an exec-tier call | 3, 4 | S | Q7 |
| 10 | **MCP UX** (F1-UX) | **done** — app `mcp add --scope project` and `/mcp add` print the inactive-until-trusted hint; envd projects `LiteralValueNotice` as a typed session notice that the TUI renders | app: the add output contains the hint; envd: a notice reaches the session for a literal project value | 3 (wording), else none | S | — |
| 11 | **Chat callback paste** | `app/src/chat_services/accounts.rs:398-403`: a `://` answer at the confirmation prompt becomes `AuthInput::CallbackUrl`, as in `auth_cli.rs:595` | app unit test next to `accounts.rs:408` | — | S | — |
| 12 | **Cache: `auto` places markers** | `ai/src/settings.rs:560-566` maps `Auto` to `Prefer(Short)` for codecs with breakpoint support, or the codec decides | ai: Anthropic codec golden, `auto` emits at most 4 `cache_control` markers | owner | S | **Yes** (Q10) |
| 13 | **Cache: Volatile band out of the system prefix** | `agent/src/prompt/mod.rs:108-114`, `status.md`, `loop.rs:2486-2509`: deliver turn, todos and mounts in the message stream; keep `date` stable | agent: extend `p5_prefix_stability` to message-prefix stability across a todo update and a new turn; update goldens | — | M | **Yes** (Q10) |
| 14 | **`--api-key` attribution** | `ai/src/auth/broker.rs:326-352`: lease under a synthetic `invocation` account, or skip pool selection when an invocation key covers the spec | ai: usage and quota observations for an invocation-key call do not land on the stored account | owner | S | **Yes** (Q11) |
| 15 | **`cl_disabled_extensions` loses PROJECT** (F12) | `driver/src/discovery/mod.rs:42` | con/driver: `.omp/config.cfg` setting it is `ProjectVarDenied` | owner | S | **Yes** (Q8) |
| 16 | **Posture-notice harness fidelity** | Route test kernels in `app/tests/{rpc,acp}_spine.rs` and `driver/tests/*` through `install_tool_authority` or a shared test builder | Those tests observe one `approval-posture` notice under a defaulted `yolo` with no sandbox | — | S | — |

Order rationale:

- 1 and 2 are the cheapest cuts of exposure from an untrusted repo, and need no trust machinery.
- 3 and 4 are the structural fix and unlock 7-10.
- 5 closes a bypass of a configured confidentiality control, but only matters when `read_deny` is set.
- 6 is hygiene.
- 11-16 are UX, cost or decisions.

## Open owner questions

1. **Slice 1:** may repo-local LSP binaries stop being preferred before trust lands? If not, should they at least resolve only after `$PATH`?
2. **F11:** fix inside `omp adw run` now (slice 2), or fold it into ADR 0040 Slice 1 PR 1d and Slice 2? The research table in `docs/research/provider-neutral-build-execution.md:375` (G4) already requires the same end state.
3. **Untrusted workspaces** (audit Q2): load `AGENTS.md`, rules and skills contained, or withhold them?
4. **Re-ask on a digest change** after `git pull` (audit Q3)? PR2's `DigestChanged` assumes yes. Should `Subtree` trust exist (audit Q4)? `ext` already implements it with the `/` and `$HOME` refusals.
5. **Native `.omp/extensions`** (audit Q5): keep automatic scan behind trust, or require an explicit link or lock record?
6. **Project MCP values** (audit Q6): keep `!cmd` and env-name substitution forbidden even for trusted workspaces? Today they are always literal (`config_values.rs:23-30`).
7. **Approval floor:** should an untrusted workspace override an explicit user `yolo`?
8. **`cl_disabled_extensions`** (audit Q7): keep the PROJECT flag?
9. **Wire contract** (audit Q9): is a typed trust request on `proto`/`rpc` acceptable, and should the trust decision enter `DaemonPolicy` and `ServerHello`?
10. **Cache:** should `ai_cache_retention auto` place markers, and may the Volatile band move into messages (goldens and the ADR 0024 prefix contract change)?
11. **`--api-key`:** which account should an invocation key's usage be recorded under?
12. **Carried over (10-09, item 3):** should the model see `approval-posture`? Are unknown-scheme reads non-fetch?
13. **Vaults:** should a project `.omp/vaults.toml` be able to shadow a user vault name, or only add new names? The same question applies to `hosts.toml` (F7).

## Owner decisions (2026-10-11)

Answers to the questions above. Slices 1 and 2 have landed (#265, #267).

| # | Decision | Applies to slice |
|---|---|---|
| 1 | Repo-local LSP/DAP binaries off by default; user-only `sv_lsp_project_binaries` opts in. | 1 (done) |
| 2 | Fix F11 inside `omp adw run` now. | 2 (done) |
| 3 | Untrusted workspaces still load `AGENTS.md`, rules and skills, contained to the repo. | 3 |
| 4 | Re-ask on a digest change: trust is bound to the inventory digest, and a change after `git pull` gives `DigestChanged`. `Subtree` trust exists (2026-10-12): the domain already models it, and slice 7 exposes it (`omp trust --subtree`) after checking that no existing command creates subtree grants. | 3, 7 |
| 5 | Native `.omp/extensions` load automatically once the workspace is trusted. No per-extension link is required. | 3 |
| 6 | Project MCP values stay literal: `!cmd` and env-name substitution are forbidden even when trusted. | 4 |
| 7 | An untrusted workspace clamps an explicit user `yolo` to `write`. | 9 |
| 8 | `cl_disabled_extensions` loses the PROJECT flag; only the user config (`~/.o2/config.cfg`) may set it. | 15 |
| 9 | A typed trust request on `proto`/`rpc` is accepted, and the decision enters `DaemonPolicy` and `ServerHello`. | 4, 7 |
| 10 | `ai_cache_retention auto` places markers, and the Volatile band moves into messages. Goldens and the ADR 0024 prefix contract are updated with it. | 12, 13 |
| 11 | Invocation-key usage is recorded under a synthetic `invocation` account. | 14 |
| 12 | The model sees the `approval-posture` notice. Reads of an unknown scheme count as possible network access, not as non-fetch (2026-10-12), unless the code shows the scheme stays local. | posture notice |
| 13 | A project `vaults.toml`/`hosts.toml` may only add new names, never shadow a user name. | 4 |

## Commands that took more than one minute

None. The longest command was a 5.5 s `git fetch`; the FIFO probe ran for 2.3 s by design.
