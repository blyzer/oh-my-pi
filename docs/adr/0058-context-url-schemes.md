# 0058. Context tools: agent replies and settings without growing the URL vocabulary

Status: proposed
Date: 2026-11-13
Area: tools

Research: [context-tools](../research/context-tools.md). Ledger gap 11. The class d items (`out://`, `cfg://`) get decisions here. The class a items are listed under "Covered"; the experimental context tools are "Deferred".

## Context

- Upstream OMPv1 (can1357/oh-my-pi `c308970263`, paths under `packages/coding-agent/src`, written `up:`) gives the agent seven context tools:
  - `out://` (`up:internal-urls/out-protocol.ts:68`): a write-only reply channel. Markdown written there is shown to the user like a response without ending the turn, through `write out://`, a shell redirect, or an eval cell. 64 KiB cap (`MAX_REPLY_BYTES`, line 35). On in the interactive TUI, never for subagents (`outRepliesActive`, 45).
  - `cfg://` (`up:internal-urls/cfg-protocol.ts:321`): the agent's view of settings. Reads show value, type, default, source and description. `cfg://<setting>` is a session-only override, `/save` persists to the global config. Only the top-level TUI session (`settingsApproval`, line 312) may write, each write needs approval, credentials are redacted.
  - `context_notes`, `new_context`, `wait`, `proc://`, `xd://`: see "Covered" and "Deferred".
- **The URL vocabulary is closed.** [ADR 0050](0050-extension-declared-url-schemes.md) D2: "The built-in vocabulary does not grow." `crates/tools/url-vocab.json` lists the built-ins (`file, http/https, artifact, history, agent, local, memory, mcp, skill, rule, omp, issue, pr, ssh, security, vault, job, attachment, conflict, adw`). `out` and `cfg` are not in it, so they do not collide, and ADR 0050 D4 (line 60) lets an extension declare a scheme with immutable and writable flags, served by the unknown-scheme fallback `HostUriResolver` (`crates/envd/src/tool_url.rs:582`).
- **ADR 0050 does not yet specify writes.** D4 names a writable flag but defines no write dispatch; the S2 exit covers `read` only (0050 line 114). Today the write path is `host::write` (`crates/envd/src/tool_document.rs:1694`, `crates/envd/src/tool_url/host.rs:102`), which sends the body to the RPC host's declared generation and fails with "RPC host resource authority is unavailable." when there is no broker. The 64 KiB figure in 0050 is the central read ceiling (ADR 0009); no write bound exists. Reads of any declared scheme are currently judged `FetchEffects { credentials: false }` whatever the extension declared (`crates/envd/src/tool_url.rs:453`, 0050 D5 "Current behavior" and the condition that fixing it is a prerequisite of S3). S1-S5 are not started; `crates/py/python/omp/schemes.py:1` holds registrations inert.
- **The convar gates protect less than they look.** A write with source `SetSource::Script` runs `check_gates` (`crates/con/src/ctx.rs:1413-1428`): `READONLY` refuses, `UNSAFE` needs `sv_cheats`. `SetSource::Code` bypasses both (`ctx.rs:55`). But: `sv_cheats` is flagged `REPLICATED | NOTIFY` only (`crates/con/src/builtins.rs`), so a Script write can turn it on and then write `UNSAFE` variables; no production convar carries `UNSAFE` (the only `unsafe` flags are in `crates/con/tests/console.rs:46`); and the only production `READONLY` variable is `SV_TOOLS_READ_ONLY` (`crates/agent/src/vars.rs:262-268`). Security-relevant variables are `archive`-only and so writable by Script: `sv_approval_mode`, `sv_sandbox_allow_domains` and the other `sv_sandbox_*`, `sv_exec_remote_endpoint`, `sv_exec_remote_credential`, `sv_exec_remote_credential_header`, `sv_browser_cdp_url`. Changing those flags would change existing behaviour, so this record does not.
- **An agent with a shell can already persist settings.** `omp config set <key> <value>` (`crates/app/src/cli.rs:2319`, `config_cmd.rs:39`) calls `set_persisted` (`config_cmd.rs:658-671`), which executes the assignment as a config script into the user's `config.cfg` (or the project overlay). It is reachable only through the shell tool, so it is subject to that tool's approval and sandbox; whether the config directory is inside the default sandbox's writable roots, and whether a persisted value takes effect in the running session, were not verified.
- **A device is the existing home for long-tail operations.** ADR 0024/0025 keep the roster small and put the long tail behind `dyn` (`crates/shell-builtins/src/dyn.rs:1`; mounted devices `crates/tool/src/registry.rs:320`).
- Credential scrubbing exists: `redact_sensitive_credentials` (`crates/app/src/diagnostics.rs:241,268`).
- Extensions are Python only (ADR 0036) and carry vendor integrations (ADR 0049); nothing here is vendor-specific.

## Decision

**D1. `out://` is an extension-declared scheme, not a built-in, and it needs a write contract first.** A first-party extension in `extensions/` declares `out` as a writable, effect-free scheme and shows the body to the user as a reply through `omp.ui` (`crates/py/python/omp/ui/__init__.py:782,1137`). It does not start until all of these hold:
  1. ADR 0050 S1 and S2 (declaration and registration; folding into the host generation);
  2. ADR 0050 S3 including its D5 prerequisite, so a declared scheme's effects come from its declaration and not from the blanket `FetchEffects { credentials: false }` (`tool_url.rs:453`);
  3. ADR 0050 S4 (rebind on reload, drop on quarantine), which is also the extension's enable and disable lifecycle;
  4. an amendment to ADR 0050 that adds the **write contract** below (owner decision, O1).

**D1a. The write contract (to be added to ADR 0050).**
  - *Dispatch:* a write to a declared scheme goes `write` tool, then `host::write` (`tool_document.rs:1694`), then the owning extension's handler through the host generation. Whole-body writes only, as `resolve_write` is today.
  - *Bound:* the manifest entry carries `max_write_bytes`; core enforces it, and a hard core ceiling of 64 KiB applies when the field is absent or larger. A body over the bound is refused **before** dispatch (ADR 0009: bound once, in core, not in the extension). `out` declares 64 KiB.
  - *Caller gate:* core refuses a write from a session whose `SV_TASK_RECURSION_DEPTH` is above 0 before dispatch. This is the subagent exclusion: it is deterministic, in core, and does not depend on the extension or on which extensions a child loads (the child's `KernelOptions` leave extension policy at its default, `crates/driver/src/subagent/spawn.rs:726-739`, so extension loading in children is not a control). The depth comes from the calling session's convar context.
  - *Approval:* from the declaration (ADR 0050 D5): an effect-free declaration gets the read tier; an omitted declaration stays `Effects::unknown()` (exec).
  - *Enable and disable:* S4's lifecycle, plus an extension-owned convar `out.enabled` (default false) declared through `omp.convars.declare` (`crates/py/python/omp/convars.py:40`).
  - *Before S2 lands:* nothing ships. A `write out://...` fails closed through the path above (no declared generation). The exact error text was not verified.

**D2. `cfg://` is a `config` device behind `dyn`, not a scheme; read first, writes through an allowlist.** The device does not change any existing convar's flags.
  - **C1, read only.** `get` with no argument lists namespaces, with a namespace lists its settings, with a setting returns value, type, default, source and description. Redaction on read: a variable whose name contains `credential`, `secret`, `token`, `key`, `password` or `header` returns `<redacted>` for string, list and kv values, and every returned string also passes `redact_sensitive_credentials`. An unknown type is redacted.
  - **C2, session-only write through an allowlist.** `set` works only for names on an explicit allowlist kept **in the device** (default deny). Initial members: `ai_default_thinking`, `cl_status_line_preset`, `cl_display_show_token_usage`. Growth is by reviewed PR, one name at a time. Never allowed, and tested as refused: `sv_cheats`, every `sv_sandbox_*`, `sv_approval_mode`, every `sv_exec_*` (including the remote endpoint and credential variables), `sv_browser_cdp_url`, and any variable whose name matches the redaction pattern above. A write uses `SetSource::Script` (so `READONLY` and `UNSAFE` still apply as defence in depth) and commits to the session layer only (`Ctx::set_value`, `ctx.rs:836`).
  - **Reset is scoped.** `reset` is offered only for names this device wrote in this session; the device keeps that set. It calls `Ctx::unset` with `Origin::Script` (`ctx.rs:872`), which removes only the script layer and falls back to whatever the archive, project or host layers hold; it cannot clear or change a value the user or host set.
  - **Approval** stays exec tier (ADR 0050 D5's rule for undeclared effects). The device is mounted only for the top-level session.
  - **Mount mechanism.** `SV_TASK_RECURSION_DEPTH` is declared at `crates/driver/src/subagent/settings.rs:101`; that is the variable, not a mount site. `task_withheld` (`settings.rs:500`) shows the pattern (read once at composition; `advertised()` at `spawn.rs:279`; withheld names recorded when the roster latches, `crates/agent/src/roster.rs:189`). The device has no existing mount site to reuse: C1 adds one in the tool registry composition next to `dyn_installed` (`crates/envd/src/tools/registry.rs:1315,1766`), reading depth once, so the roster never changes mid-session (ADR 0024).

**D2a. The device adds no new authority.** The shell route above can already persist any convar assignment, including the security-relevant ones listed, under the shell tool's approval and sandbox. The device is narrower: allowlisted names only, session layer only, no persistence, redacted reads. What it adds is a live session-layer effect without a shell call, and the allowlist is what bounds it. If the persisted shell route is a concern, it is an existing exposure outside this record (O6).

**D3. No persisted agent write through the device.** The `/save` form is not built.

**D4. Placement.** The device needs authority over the convar context, which has one owner, so it is T0/T1 core (ADR 0049 D1), vendor-neutral. `out://` has no such need, so it is T2 (an extension). Neither changes an existing tool, schema, command or URL scheme.

### Covered (class a)

- **`wait`.** The `hub` tool's `wait` op (`crates/tools/src/hub.rs:31-32` the `Wait` variant; `Params` at 111; implementation `crates/driver/src/subagent/hub.rs:160`): `ids`, `timeoutMs` (zero is infinite), `from`, `await`; it races every selected source and returns the first peer message or settled job.
- **`proc://` for jobs, cancel and logs.** `hub` ops: `Jobs` (`tools/src/hub.rs:38`) dispatches at `crates/driver/src/subagent/hub.rs:166` to `roster` (459-472), which returns `id, kind, status, owner, started, output, error` per job; `Cancel` kills by id (hub.rs:167); `Ps`, `Describe`, `Logs`, `Stop`, `Restart` handle named processes and require a name (`crates/tools/src/hub.rs:491`). A job's full result is also readable as `agent://<id>` (`agent_projection`, `tool_url.rs`). **Job output cap:** a job's output is bounded when it settles (`commit_settlement`, `crates/agent/src/jobs.rs:2375`) by `output_bound`, default `DispatchPolicy::DEFAULT_MAX_OUTPUT_BYTES` = 64 KiB (`jobs.rs:720`, `crates/agent/src/dispatch.rs:107`).
- **`xd://`.** The `dyn` builtin and device catalog (`crates/shell-builtins/src/dyn.rs:1`, `crates/tools/src/device.rs:1`, `crates/tool/src/device_path.rs:18`). The `xd://` spelling is **live code**, not dead: `crates/app/src/acp_events.rs:599` treats `xd://hub` as internal hub traffic and `625` renders a `write` to any `xd://` path as "execute". Upstream computes approval per payload from the mounted tool; omp2 uses the device's declared effects (`MountedDevice.effects`, `crates/tool/src/registry.rs:320-342`).

### Not adopted

- **`proc://<id>/streams[/<n>]`.** Upstream shows the bytes crossing a bash job's pipes, including what a later `| tail` withheld (`up:internal-urls/proc-protocol.ts`, the streams form). omp2's job `output` is the job's own output only. This is a **named gap**, class d if wanted, not adopted here: it needs the shell to retain per-stage bytes, which the current job output does not.

### Deferred

- **`context_notes` and `new_context`.** Upstream gates both behind `compaction.experimentalContextManagement`, default false (`up:session/context-settings.ts:119`); `new_context` is a bare signal consumed by a lifecycle omp2 does not have (`up:tools/context-notes.ts:81,155`). Adding either would add roster tools (ADR 0024). omp2's `/handoff` is a user command (`crates/chat/src/commands/mod.rs:111-118`), not an agent tool. The v1 importer marks an agent that asks for `context_notes` as `NotMigratable` (`crates/driver/src/v1_import/agents_tests.rs:220`; `new_context` at 615; names listed at `agents.rs:144-145`). **Revisit when** upstream turns the setting on by default, or omp2 adds an agent-callable reset or handoff. omp2 already has a reset primitive to build on (the importer maps `reset_boundary` to a `Compaction` with method `clear`, `crates/app/src/session_import/convert/omp1.rs:167,372`).

## Options rejected

- **Add `out` and `cfg` to `url-vocab.json`.** Contradicts ADR 0050 D2.
- **`cfg://` as an extension scheme.** An extension cannot write core convars: `omp.convars` declares extension-owned ones only (`crates/py/python/omp/convars.py:40`).
- **Rely on the convar gates alone for `cfg` writes.** `sv_cheats` is writable by Script and nothing production carries `UNSAFE` (Context), so the gates protect almost nothing.
- **Change convar flags to make the gates real** (add `READONLY`/`UNSAFE` to sandbox, approval, exec). Right in spirit but an existing-behaviour change; recorded as O6 for the owner.
- **A denylist in the device.** A new security-relevant convar would be writable by default. The allowlist is default deny.
- **`SetSource::Code` for device writes.** Bypasses `READONLY`/`UNSAFE` (`ctx.rs:55`).
- **Persisted `cfg://.../save`.** See D3.
- **`out://` through a new `say`-style roster tool.** Adds a permanent tool (ADR 0024) for a rare need.
- **Let the extension enforce the write bound and the subagent exclusion.** Both must hold even if the extension is wrong or absent (ADR 0009; ADR 0006).
- **Do nothing for `out://`.** Acceptable fallback: assistant text between tool calls is already streamed. Whether it reaches the transcript before the turn ends was not verified; it matters only for the shell-redirect and eval transports.

## Consequences

- An agent can read its settings, and change a few harmless ones for the session, without a shell call. No existing flag, tool, schema, command or scheme changes.
- `out://` waits on ADR 0050 S1-S4 and its write-contract amendment. Until then nothing changes.
- The finding that `sv_cheats` is Script-writable and no production variable is `UNSAFE` is independent of this record and needs an owner decision (O6).

## Slices

One PR each. `sv_config_device_enabled` (bool, default false) gates C1-C2. `out://` ships as an extension disabled by default (`out.enabled`, default false). Prefixes follow ADR 0012. No `cap-*` feature: no new heavy dependency.

| Slice | Content | Acceptance | Tests |
|---|---|---|---|
| C1 | `config` device, read only (`omp-tools`), mounted at depth 0 | List, namespace and one-setting reads; credential-like values redacted | A seeded credential never appears; each of `sv_exec_remote_credential`, `sv_exec_remote_credential_header` reads `<redacted>`; unknown name fails; device absent when the convar is off and at depth 1; roster identical before and after |
| C2 | Session write through the allowlist | Only allowlisted names; `SetSource::Script`; session layer only; exec-tier approval; scoped reset | `sv_cheats`, `sv_sandbox_allow_domains`, `sv_approval_mode`, `sv_exec_remote_endpoint`, `sv_browser_cdp_url` each refused; allowlisted write lands in the session layer and a `reset` restores the prior archive value; `reset` of a name the device did not write is refused; a user-set archive value is untouched |
| X0 | ADR 0050 amendment: write contract (owner) | Dispatch, `max_write_bytes`, caller gate, approval, lifecycle written down | Docs review; no code |
| X1 | `out` extension (after 0050 S1-S4 and X0) | Manifest declares `out` writable, effect-free, 64 KiB; a write shows the body as a reply | Write shows one reply; body over bound refused before dispatch; write from depth 1 refused by core; disabled by default; before S2 the write fails closed |
| X2 | Shell redirect and eval transports | `cmd > out://` and an eval `tool.write` deliver the same reply | Redirect test through `crates/tools/src/shell_uri.rs`; eval bridge test |

## Open questions

- **O1.** Does the owner accept the write-contract amendment to ADR 0050 (X0)? Recommendation: yes; without it `out://` cannot ship.
- **O2.** Approval tier of a write to an effect-free declared scheme. Recommendation: read tier, from the declaration (D1a).
- **O3.** Is X2 (shell redirect) acceptable? It needs a registration seam in `shell_uri.rs` for a declared scheme. Recommendation: defer; ship `write out://` only.
- **O4.** Should the persisted `cfg://.../save` ever ship? Recommendation: no.
- **O5.** Add upstream's per-pipeline-stage stream view to `hub`? Recommendation: no.
- **O6.** Should `sv_cheats`, `sv_approval_mode`, `sv_sandbox_*`, `sv_exec_remote_*` and `sv_browser_cdp_url` get real gates (`READONLY` or `UNSAFE` plus a gated `sv_cheats`)? The shell route (`omp config set`) can already persist them. Recommendation: yes, in a separate record, because it changes existing behaviour.
- **O7.** Does a persisted `omp config set` take effect in the running session, and is the config directory inside the default sandbox's writable roots? Not verified. Recommendation: answer before C2, because it decides how much new live effect the device adds.

Settled by the code, not open: the `hub jobs` output cap (64 KiB); `xd://` is covered by `dyn`.

## Acceptance tests

1. `url-vocab.json` is byte-identical after this record lands.
2. The `config` device refuses `sv_cheats`, `sv_sandbox_*`, `sv_approval_mode`, `sv_exec_*`, `sv_browser_cdp_url` and any name outside the allowlist, and commits only to the session layer.
3. `reset` removes only a value the device set and never a user, project or host value.
4. No credential-like value is returned by `get`.
5. The `config` device is not mounted for a subagent, and the roster is identical before and after a session starts.
6. No existing convar's flags differ after this record's slices land.
7. `out://` text reaches the user without ending the turn; a body over the bound is refused before dispatch; a write from a subagent is refused by core.

## Status in omp

**Not started.** Reusable: convar flags and `SetSource`, `dyn` and device mounting, `hub`. Blocked: X1-X2 on ADR 0050 S1-S4 and X0. Not existing: a device mount site depending on session depth (C1 adds it). Unverified: the exact failure text of a write to an undeclared scheme; the persisted-config questions in O7.

## Review (EGM, 2026-11-13)

Design critic and zoom-out findings, and what changed:
- The Script gate protects far less than the first draft claimed (`sv_cheats` is Script-writable; no production `UNSAFE`; one production `READONLY`; security variables are archive-only). Changed: C1 ships read-only first; C2 writes only through an in-device allowlist, with never-allowed names tested; no existing flag is changed; the flag gap is recorded as O6.
- Reset scope and credential redaction on read are now specified (D2).
- Checked the shell route: `omp config set` already persists settings under the shell tool's approval, so the device adds no authority and is narrower (D2a); what is unverified is O7.
- `out://` was blocked on S2 only. Changed: it also needs S3's D5 prerequisite (`tool_url.rs:453`), a write contract that 0050 lacks (dispatch, bound, caller gate), and S4 for enable and disable. The D1 versus X1 inconsistency is fixed; the write bound and the subagent exclusion are core-side; behaviour before S2 is stated.
- `proc://` was overstated: now class a for jobs, cancel and logs, with the streams view recorded as not adopted. `xd://` notes the live ACP code. `context_notes` and `new_context` are Deferred with a revisit trigger.
- Citation fixes: `Wait` is at `tools/src/hub.rs:31-32`; the `Jobs` dispatch is `driver/src/subagent/hub.rs:166` (confirmed) and its op at `tools/src/hub.rs:38`; `settings.rs:101` is the depth variable, not a mount site.

## Upstream origin and license

Draws on `up:internal-urls/out-protocol.ts`, `cfg-protocol.ts`, `proc-protocol.ts`, `xd-protocol.ts`; `up:tools/context-notes.ts`, `wait.ts`; `up:session/context-notes.ts`, `context-settings.ts`. License: MIT, "Copyright (c) 2025 Mario Zechner; (c) 2025-2026 Can Bölük; (c) 2026 Stencil Labs, Inc." (upstream `LICENSE`). omp2 re-implements the behaviour in Rust and Python; if reply or settings prompt text is copied, keep the notice and list it in `THIRD-PARTY-NOTICES.txt`.

## References

ADR 0006; ADR 0009 (bound output once); ADR 0012 (convars); ADR 0024/0025 (roster, long tail behind `dyn`); ADR 0036; ADR 0049; ADR 0050; `crates/con/src/spec.rs`; `crates/con/src/ctx.rs`; `crates/con/src/builtins.rs`; `crates/agent/src/vars.rs`; `crates/app/src/config_cmd.rs`; `crates/tools/src/hub.rs`; research note [context-tools](../research/context-tools.md).
