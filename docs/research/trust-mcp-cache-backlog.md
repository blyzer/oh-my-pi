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
- **Still waiting on owner decisions:** the Volatile band sits in the system prefix; cache-marker placement for `auto` is done in slice 12.
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
| F1 | Project MCP files spawn stdio servers with user authority; `!cmd` and env-name values | **partly fixed** (#273) | Project configs are withheld unless trusted or explicitly user-opted-in: `envd/src/mcp/settings.rs:46-73`; project values remain literal. **Residual:** the deliberate user-wide opt-in and trusted stdio authority. | Medium (needs an opt-in); High when trusted | envd |
| F1-UX | Project-scope `mcp add` tells the operator it stays inactive until trust or the explicit opt-in; literal project MCP values reach the session as typed notices | **fixed** | `app/src/{config_cmd.rs,chat_services/mcp.rs}` use the shared inactive-until-trusted hint. `envd/src/mcp/manager.rs` buffers and posts `mcp-literal-value` typed notices through the active session mailbox; `driver/src/headless/kernel.rs` binds the session host. | — | app, envd, driver |
| F2 | Native `.omp/extensions` load automatically, with self-declared DATA grants | **fixed** (#272) | `driver/tests/workspace_trust_gates.rs:369-377` proves an untrusted workspace extension is withheld; `driver/src/workspace_trust.rs:70-112` evaluates the inventory-bound grant. | Low | driver |
| F3 | Project LSP/DAP config, and repo-local binaries resolved before `$PATH` | **fixed** (#265, #273) | `envd/src/docserver/lsp_supervisor.rs:619-685` proves local bins require both trust and user opt-in; `lsp_config.rs:375-412` withholds project config when untrusted. | Low | envd |
| F4 | Yolo plus sandbox-off defaults | **fixed** (#181, #197) | `envd/src/exec_settings/sandbox.rs:184-187` (`WorkspaceWrite`), `:193-196` (`Scoped`). `admission.rs:223-231`: a defaulted `yolo` without a confining sandbox is `Write`. `:241-252`: `Host` tools never see the sandbox | Low (posture) | envd |
| F5 | Context files and SYSTEM/rules follow symlinks out of the repository | **fixed** (#180) | `driver/src/discovery/rules.rs:779`, `:959`; `driver/src/prompt_input.rs:69-82`; `core/src/project_file.rs:145-181` | — | driver |
| F6 | `.omp/SYSTEM.md` replaces the role block; the project wins over the user | **fixed** (#272) | `driver/src/prompt_input.rs:68-69`, with `workspace_trust_gates.rs:369-377`, withholds project prompt inputs when untrusted. | Low | driver |
| F7 | `.omp/hosts.toml` shadows user SSH aliases | **fixed** (#273) | `envd/src/ssh.rs:153-163`, `:253-266`, and `:1298-1308` withhold an untrusted project file and preserve user aliases. | Low | envd |
| F8 | Project plugin registry and `enabledPlugins` shadow user installs | **fixed** (#272) | `ext/src/claude_plugin.rs:754-777` withholds the project registry for untrusted workspaces; `:1692-1695` tests both states. | Low | ext, driver |
| F9 | Project cfg reads and writes follow symlinks; FIFO and `/dev/zero` | **fixed** (#263) | `driver/src/cfg.rs:300-320` checks both config and lock targets; user reads use bounded `read_text` at `:221-225`. | Low | driver |
| F10 | `.omp/secrets.yml` overrides user rules | **fixed** (#272) | `driver/src/secrets/config.rs:105-121` accepts project rules only when trusted; `:205-230` tests the withheld project override. | Low | driver |
| F11a | adw code phases run unconfined with the full environment | **fixed** (#267) | `driver/src/adw/production.rs:128-145` opens the environment authority; untrusted code phases are refused at `:380-383`. | Low | driver (+envd `ExecHost`) |
| F11b | Resolver shadows `$PATH` | **fixed** (#267) | `driver/src/adw/production.rs:114-116` uses `FilesystemResolver`; the merge also covers cleanse containment. | Low | driver |
| F11c | Reported posture is not applied to code phases | **fixed** (#267) | `driver/src/adw/production.rs:356-363` derives configured approval and sandbox before dispatching the environment command. | Low | driver |
| F11d | `omp adw run` ignores operator cfg | **fixed** (#267) | `app/src/adw_cmd.rs:138-141` composes with `process_ctx_with`. | Low | app |
| F11e | Agent phases admit workspace extensions | **fixed** (#267) | `driver/src/adw/production.rs:235-243` gives child kernels `include_workspace: false`. | Low | driver |
| F11f | Ctrl-C is not wired | **fixed** (#267) | `app/src/adw_cmd.rs:63-72` selects `ctrl_c` and cancels the active run. | Low | app |
| F12 | Five PROJECT convars (`ai_model`, `ai_model_roles`, `ai_thinking`, `sv_tools`, `cl_disabled_extensions`) | **decision** | `agent/src/vars.rs:146`, `:164`, `:274`; `catalog/src/settings.rs:1134`; `driver/src/discovery/mod.rs:42`. `cl_disabled_extensions` can disable a user's guard extension (`native.rs:304-308`). Whether an `ai_model` selector can name an arbitrary endpoint: UNVERIFIED | Low-Medium | owner (con/driver) |
| F13/14 | `.omp/config.toml [extensions]`, `omp.lock`; ancestor scanning of context files | **info** | `ext/README.md:113-116` (inert records excluded from the digest) | Info | — |
| New | `.omp/vaults.toml`: uncontained read; project vaults shadow user vaults and may name any absolute root | **fixed** (#263, #273) | `envd/src/vault.rs:164-184`, `:1150-1156`, and `:1532-1545` use contained reads, withhold untrusted project vaults, and preserve user names. | Low | envd |

### Handoff §1 (security) and the 2026-10-09 carried-over items

| Item | Status | Evidence | Severity | Owner |
|---|---|---|---|---|
| §1.1 Trust model PR3-PR8 | **fixed** (#272-#277) | Driver trust evaluation: `driver/src/workspace_trust.rs:70-112`; envd policy includes trust: `envd/src/daemon_policy.rs:69-104`; CLI/RPC and overlay are covered by #274-#276. | High (closed) | driver, envd, app, ext |
| §1.2 Explicit per-workspace MCP approval | **fixed** (#273, #274) | `envd/src/mcp/settings.rs:46-73` gates project MCP by trust; `app/src/trust_cmd.rs:20` records and revokes workspace trust. | Low | envd, driver |
| §1.3 F11, F2, F3 | **fixed** (#265, #267, #272, #273) | See the current F2, F3, and F11 rows above. | — | — |
| §1.4a `HostStore::upsert`/`persist_hosts` "follow symlinks" | **fixed** (#263, #273) | `envd/src/ssh.rs:171-183`, `:253-266`, and `:309-311` contain project reads/writes and prevent shadowing. | Low | envd, app |
| §1.4b `.cfg.lock` opened with `create(true)` | **fixed** (#263) | `driver/src/cfg.rs:300-320` validates the lock target before opening it. | Low | driver |
| §1.4c User-level `SYSTEM.md` and user cfg not hardened | **fixed** (#263) | `driver/src/cfg.rs:221-225` and `prompt_input.rs:99-103` use bounded regular-file reads under `Unconfined`. | Low | driver |
| §1.4d Windows path untested (`O_NOFOLLOW` unix-only) | **open — UNVERIFIED** | `core/src/project_file.rs:122-133` still applies `O_NOFOLLOW` only on Unix; no Windows proof was confirmed on current `omp2`. | Low | core |
| §1.4e Refused project `secrets.yml` is a hard error | **by design** | `driver/src/secrets/config.rs:100-102` | — | owner |
| §1.5 `cl_disabled_extensions` PROJECT flag | **fixed** (#281) | `driver/src/discovery/mod.rs` rejects the project value as `ProjectVarDenied`; see slice 15. | Low-Medium | owner |
| §2 In-process builtins ignore `read_deny` | **fixed** (#268) | `shell-builtins/src/host.rs:265-282` routes reads through `open_read`; `read_policy_tests.rs:1-4` covers hidden roots. | Low | shell-builtins (+envd seam test) |
| §2 Seatbelt items verified live 2026-10-08 | **fixed** | 2026-10-09 handoff, "Carried over" §2 | — | — |
| §2 Memory `reflect`: `MnemopiSettings.llm_mode` not consulted | **UNVERIFIED** | `memory/src/session.rs:151`, `:227` read `llm_mode`; whether the daemon relay path (`envd/src/reflection_relay.rs`) honours it was not traced | Low | envd/memory |
| 10-09 #4 Harnesses without `install_tool_authority` | **fixed** (#264 / `186928bfaa`) | `app/tests/{rpc_spine,acp_spine}.rs:794-800,1886-1892` and `driver/tests/support/mod.rs:41-45` install tool authority and assert typed posture notices. | Low (test fidelity) | driver/app tests |
| 10-09 #5 Disabled-network diag gaps | **accepted, documented** | ADR 0028 (not re-read this session) | Low | — |
| 10-09 #3 Model sees `approval-posture` / unknown-scheme reads possible network access | **implemented (Q12)** | `session/src/projection.rs` projects the typed posture once per journaled notice; `envd/src/tool_url.rs` classifies the host fallback as anonymous fetch | — | — |

### Accounts (handoff §3) and the 2026-10-11 login item

| Item | Status | Evidence | Severity | Owner |
|---|---|---|---|---|
| Slice 2: `--api-key` precedence over stored accounts | **fixed** | `ai/src/auth/broker.rs`: invocation leases always use the synthetic `invocation` account/principal, bypass stored refresh and rejection accounting. `ai/src/provider/builtin.rs`: the lease identity replaces the pool-selected account in request, usage, quota, and error attribution while preserving routing metadata. Broker and provider tests cover synthetic identity and a 429 rejection without stored-source mutation. | Medium | ai |
| Slice 3: model-pool preference | **obsolete** | No model-pool concept; the account pool is per provider (`docs/py/13-inference.md:453-454`) | — | — |
| Slice 4: project-level pins | **blocked on §1.1** | Proposal only | — | — |
| Chat prompt cancels on a pasted callback URL | **fixed** (#262 / `2213dd1519`) | `app/src/chat_services/accounts.rs:394-399` maps `://` to `AuthInput::CallbackUrl`; `:414-439` tests a pasted browser callback. | Low (UX) | app |

### Inference cost and caches (handoff §4) and `omp-cache`

| Item | Status | Evidence | Severity | Owner |
|---|---|---|---|---|
| `ai_cache_retention auto` places cache markers | **fixed (slice 12)** | `catalog/src/settings.rs:1297` (default `Auto`); `ai/src/settings.rs:560-572` (`Auto` → `Setting::Prefer(CacheRetention::Short)` for Anthropic and Bedrock); `ai/src/codec/anthropic.rs:1679-1697` (short TTL and explicit none); `:1748-1838` (automatic placement capped at four); Anthropic behavior test `auto_retention_places_short_breakpoints_only_on_supported_codecs` | Cost, not security | ai |
| Volatile band (turn, cwd, date, mounts, todos) in the system prefix | **fixed** (slice 13) | `agent/src/prompt/mod.rs`: `CanonicalPromptSource::system_items` projects Frozen, Stable and Dynamic only; `volatile_items` projects the Volatile band (recall, `status.md`) as synthetic user messages. `agent/src/loop.rs` `project_request`: system items, the mode prompt, the thread, then the volatile items. `e2e/tests/p5_prefix_stability.rs` holds the system-prefix bytes constant across a todo update and a new turn | Cost | agent |
| Measure cache hits under the default config | **not done** | Needs an Anthropic key | — | — |
| Dedicated TUI message for `account-pin` | **open — UNVERIFIED** | No implementation was confirmed on current `omp2`; this is outside the merged trust slices. | Low | chat |
| MCP definition cache (`omp-cache`) | **no action** | `cache/src/mcp_cache.rs:15-30` keys by server name and canonical config SHA-256, 30-day TTL, in the per-project `state_dir/mcp-cache.sqlite3` (`envd/src/server.rs:2493`). Cached definitions publish only for an installed mount (`envd/src/mcp/manager.rs:1942-1976`). [INFERENCE] Gating at mount time therefore covers the cache: an untrusted project server has no mount, so its cached rows are never published. A revoke leaves rows on disk until TTL or `remove_server` | Info | cache, envd |

## Implementation slices (ordered)

Each slice is one PR of about 500 lines or fewer. "Seam" is the crate whose public boundary the acceptance test exercises, per `AGENTS.md`. Host changes go to `omp-envd`, client protocol to `omp-env`, composition to `omp-driver`, and presentation to `omp-app`.

| # | Slice | Scope and files | Acceptance test (seam) | Depends on | Size | Owner decision? |
|---|---|---|---|---|---|---|
| 1 | **Repo-local LSP/DAP binaries off by default** **(done #265; `envd/src/docserver/lsp_supervisor.rs:619-685`)** | `envd/src/docserver/{lsp_binary,lsp_process,lsp_supervisor}.rs`: pass no local roots unless a user-level convar (proposal: `sv_lsp_project_binaries`, default false, not PROJECT) enables them; same for DAP `program` resolution. Slice 4 later replaces the convar with the trust decision | envd unit: a root with `package.json` and `node_modules/.bin/<cmd>`, plus a `PATH` dir holding `<cmd>`, resolves the `PATH` one; the roster drops a server only the local root provides; with the convar on, the local one wins (the existing `lsp_binary.rs:144` test moves under it) | — | S | **Yes** (Q1): breaks users who rely on a project-pinned `typescript-language-server` |
| 2 | **adw and cleanse containment** **(done #267; `app/src/adw_cmd.rs:63-72`)** | `driver/src/cleanse/checkers.rs`: resolve code-phase binaries from `$PATH` only, or behind slice 1's rule. `driver/src/adw/production.rs`: run command phases through envd's exec authority (sandbox and posture), and give child kernels `include_workspace: false`. `app/src/adw_cmd.rs`: `process_ctx_with` instead of `Ctx::new()`, and cancel the token on Ctrl-C | driver: a repo-root executable `cargo` is not chosen for `command = ["cargo", …]`; a phase that declares `write = "workspace-write"` fails to write outside the workspace; the child `KernelOptions` excludes workspace extensions. app: a user `config.cfg` with `sv_sandbox_mode off` changes the reported posture. Ctrl-C needs a PTY probe (`.omp/tools/tui.ts`) | — | L | **Yes** (Q2) |
| 3 | **Trust plumbing in the driver** **(done #272; `driver/tests/workspace_trust_gates.rs:369-377`)** | New `KernelOptions.workspace_trust` (host-supplied, never a convar). At `compose_kernel`: compute `inventory` and `evaluate` against `grants.toml` (and session rows), journal the decision once, and post a one-line "withheld" notice. Gate native `.omp/extensions` (`native.rs:289`), `SYSTEM`/`APPEND`/`TITLE_SYSTEM.md` (`prompt_input.rs:63`), the project plugin registry and `enabledPlugins`, the `secrets.yml` override, adw code phases. Subagents inherit the decision | driver: `compose_kernel` with `Untrusted` admits no workspace extension, ignores `.omp/SYSTEM.md`, journals `TrustDecision::Untrusted`; a `Trusted` row whose digest matches admits them; editing `.omp/mcp.json` gives `DigestChanged`. Update `e2e/…` | — | L | **Yes** (Q2-Q6) |
| 4 | **Trust gating in envd** **(done #273; `envd/src/mcp/settings.rs:46-73`)** | `envd`: replace the `McpSettings.enable_project_config` bool with a typed policy (trusted, or the user opt-in); gate project LSP/DAP sources and local roots (subsumes slice 1); gate the project `hosts.toml` and `vaults.toml` layers. `env`: fold the trust decision into `DaemonPolicy` (`env/src/project_state.rs:22-34`), so trusted and untrusted clients never share a daemon (the #213/#215 pattern) | envd: an untrusted environment open mounts no project MCP server, its LSP roster has no project source or local root, and a project alias in `hosts.toml` does not resolve; env: two clients with different trust get different sockets and `ServerHello.policy_digest` | 3 | L | **Yes** (Q6, Q9, Q13) |
| 5 | **Builtins honour the read policy** **(done #268; `shell-builtins/src/host.rs:265-282`)** | `shell-builtins/src/host.rs`: add `Host::open_read(path)` → `PathPolicy::open(path, OpenRequest { access: Read, .. })` (`shell/src/interp.rs:98-107`). Migrate every builtin read site (99 counted: `cat`, `head`, `tail`, `grep`, `rg`, `sed`, `sort`, `jq`, `diff`, `comm`, `cksum`, `wc`, `uniq`, `tac`, `sponge`, `tee`, `find`, ...); no dual path | envd: `cat <read_deny root>/file` in an exec session under `workspace-write` ends `Denied`; shell-builtins: the `HiddenRoot` policy (`shell/src/commands.rs:1350-1380`) refuses `cat`/`head`/`grep` reads | — | L | — |
| 6 | **Contained-reader residue** **(done #263; `driver/src/cfg.rs:300-320`)** | envd: `vault.rs` `parse_vaults` through `project_file::read_text` (project `Within`, user `Unconfined`); `ssh.rs` `HostStore::load` takes a `Containment`, `ssh_cmd.rs` passes `Within` for the project scope; `persist_hosts` refuses a symlinked parent or target (`check_write_target`). driver: `cfg.rs` opens the lock with `O_NOFOLLOW` (or `check_write_target` on the lock path); user cfg and user `SYSTEM.md` through `read_text(.., Unconfined, ..)` | envd: a FIFO or outside symlink at `.omp/vaults.toml` is skipped with a warning and the environment opens (mirror `ssh.rs:1268-1315`); `omp ssh add` into a repo whose `.omp` links outside is refused. driver: a dangling `.omp/config.cfg.lock` symlink is refused | — | M | — |
| 7 | **`omp trust` CLI, `--trust-workspace`, RPC/ACP trust request** **(done #274, #275; `app/src/rpc_mode.rs:84-88`)** | `app/src/cli.rs` and a new `trust_cmd.rs` over `GrantsFile::persist_workspace_trust`/`revoke_workspace_trust`; `--trust-workspace` (session only); one structured notice in print, RPC and ACP; `proto`/`rpc`: a typed trust request | app: `omp trust` then `omp chat` in the same tree loads the project MCP; `omp trust --revoke` withholds it again; `omp print` in an untrusted tree prints the notice and denies | 3, 4 | M-L | **Yes** (Q9) |
| 8 | **First-run TUI trust overlay** **(done #276; `crates/chat/src/overlays/workspace_trust.rs:61-150`)** | `crates/chat` and app; default "Do not trust" | Real-PTY proof through `.omp/tools/tui.ts`; the journal shows the decision | 7 | M | — |
| 9 | **Approval floor for untrusted workspaces** **(done #277; `envd/src/admission.rs:230-233`)** | `envd/src/admission.rs`: an `Untrusted` decision clamps an explicit `yolo` to `write` | envd: `resolve_approval` with explicit `yolo` plus `Untrusted` prompts for an exec-tier call | 3, 4 | S | **Yes** (Q7) |
| 10 | **MCP UX** | **done (#282; `envd/src/mcp/manager.rs`)** — app `mcp add --scope project` and `/mcp add` print the inactive-until-trusted hint; envd projects `LiteralValueNotice` as a typed session notice that the TUI renders | app: the add output contains the hint; envd: a notice reaches the session for a literal project value | 3 (wording), else none | S | — |
| 11 | **Chat callback paste (done #262 / `2213dd1519`; `app/src/chat_services/accounts.rs:394-439`)** | A `://` answer at the confirmation prompt becomes `AuthInput::CallbackUrl`, as in `auth_cli.rs:610-613`. | app unit test at `accounts.rs:414-439` | — | S | — |
| 12 | **Cache: `auto` places markers (done #279; `ai/src/codec/anthropic.rs:3884-3892`)** | `ai/src/settings.rs:560-572` maps `Auto` to `Prefer(Short)` for codecs with automatic breakpoint support; unsupported codecs remain marker-free. Anthropic placement is capped at four markers | ai: Anthropic behavior test proves `auto` emits 1–4 `cache_control` markers on a long multi-turn request, explicit `none` emits 0, and unsupported codec `auto` emits 0 | owner | S | **Yes** (Q10) |
| 13 | **Cache: Volatile band out of the system prefix (done #284; `e2e/tests/p5_prefix_stability.rs`)** | `agent/src/prompt/mod.rs`, `status.md`, `loop.rs`: deliver turn, cwd, date, mounts, todos, directors, and recall as trailing message-stream notices; `date` stays journaled once in `prompt-facts` | agent/e2e: `p5_prefix_stability` holds system-prefix bytes constant across a todo update and a new turn while the latest message carries the volatile facts; prompt goldens updated | — | M | **Yes** (Q10) |
| 14 | **`--api-key` attribution (done #280; `ai/src/auth/broker.rs:333-350`)** | `ai/src/auth/broker.rs:326-352`: invocation-key leases use the synthetic `invocation` account and do not refresh or reject a stored source; `ai/src/provider/builtin.rs` propagates that identity into request, usage, quota, and error attribution | ai: usage and quota observations for an invocation-key call do not land on the stored account (done; broker/provider tests cover a 429 rejection) | owner | S | **Yes** (Q11) |
| 15 | **`cl_disabled_extensions` loses PROJECT (done #281)** (F12) | `driver/src/discovery/mod.rs:42`; user-only extension configuration UI | driver: `.omp/config.cfg` setting it is `ProjectVarDenied`, changes nothing; user config remains effective | owner | S | **Yes** (Q8) |
| 16 | **Posture-notice harness fidelity (done #264 / `186928bfaa`; `driver/tests/support/mod.rs:41-45`)** | Route test kernels in `app/tests/{rpc,acp}_spine.rs` and `driver/tests/*` through `install_tool_authority` or a shared test builder | Those tests observe one `approval-posture` notice under a defaulted `yolo` with no sandbox | — | S | — |

Slice 8's terminal startup selector runs before launch discovery and kernel
composition. Untrusted workspaces, changed input digests, and unpinned workspaces
under a subtree ask only when the inventory contains gated inputs. Enter defaults
to **Do not trust**; Esc and Ctrl-C also continue without trust. Session approval
does not write grants; workspace approval persists an exact inventory-bound row;
the subtree choice pins only this workspace under the existing subtree. Changed
inputs or grants while the prompt is open refuse approval. The selected decision
flows through `KernelOptions.workspace_trust` into the existing session journal.
Already trusted workspaces and explicit revocations do not prompt.

Order rationale:

- 1 and 2 are the cheapest cuts of exposure from an untrusted repo, and need no trust machinery.
- 3 and 4 are the structural fix and unlock 7-10.
- 5 closes a bypass of a configured confidentiality control, but only matters when `read_deny` is set.
- 6 is hygiene.
- 11-16 are UX, cost or decisions.

## Resolved owner questions
| Answers below are implemented on current `omp2`; the cited slice rows record the merge and code evidence.
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

| 1 | Repo-local LSP/DAP binaries off by default; user-only `sv_lsp_project_binaries` opts in. | 1 (done #265) |
| 2 | Fix F11 inside `omp adw run` now. | 2 (done #267) |
| 3 | Untrusted workspaces still load `AGENTS.md`, rules and skills, contained to the repo. | 3 (done #272) |
| 4 | Re-ask on a digest change: trust is bound to the inventory digest, and a change after `git pull` gives `DigestChanged`. `Subtree` trust exists (2026-10-12): the domain already models it, and slice 7 exposes it (`omp trust --subtree`) after checking that no existing command creates subtree grants. | 3, 7 (done #272, #274) |
| 5 | Native `.omp/extensions` load automatically once the workspace is trusted. No per-extension link is required. | 3 (done #272) |
| 6 | Project MCP values stay literal: `!cmd` and env-name substitution are forbidden even when trusted. | 4 (done #273) |
| 7 | An untrusted workspace clamps an explicit user `yolo` to `write`. | 9 (done #277) |
| 8 | `cl_disabled_extensions` loses the PROJECT flag; only the user config (`~/.o2/config.cfg`) may set it. | 15 (done #281) |
| 9 | A typed trust request on `proto`/`rpc` is accepted, and the decision enters `DaemonPolicy` and `ServerHello`. | 4, 7 (done #273-#275) |
| 10 | `ai_cache_retention auto` places markers, and the Volatile band moves into messages. Goldens and the ADR 0024 prefix contract are updated with it. | 12, 13 (done #279, #284) |
| 11 | Invocation-key usage is recorded under a synthetic `invocation` account. | 14 (done #280) |
| 12 | The model sees the `approval-posture` notice. Reads of an unknown scheme count as possible network access, not as non-fetch (2026-10-12), unless the code shows the scheme stays local. | posture notice (done #278) |
| 13 | A project `vaults.toml`/`hosts.toml` may only add new names, never shadow a user name. | 4 (done #273) |

## Commands that took more than one minute

None. The longest command was a 5.5 s `git fetch`; the FIFO probe ran for 2.3 s by design.
