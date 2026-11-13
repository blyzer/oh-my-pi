# 0058. Context tools: agent replies and settings without growing the URL vocabulary

Status: proposed
Date: 2026-11-13
Area: tools

Research: [context-tools](../research/context-tools.md). Ledger gap 11. The class d items (`out://`, `cfg://`) get decisions here. The class a and b items are listed under "Covered" and "Not adopted" with the reason.

## Context

- Upstream OMPv1 (can1357/oh-my-pi `c308970263`, paths under `packages/coding-agent/src`, written `up:`) gives the agent seven context tools:
  - `out://` (`up:internal-urls/out-protocol.ts:68`): a write-only reply channel. Markdown written there is shown to the user like a response without ending the turn, through `write out://`, a shell redirect, or an eval cell. 64 KiB cap (`MAX_REPLY_BYTES`, line 35). On in the interactive TUI, never for subagents (`outRepliesActive`, 45).
  - `cfg://` (`up:internal-urls/cfg-protocol.ts:321`): the agent's view of settings. Reads show value, type, default, source and description. `cfg://<setting>` is a session-only override, `/save` persists to the global config. Only the top-level TUI session (`settingsApproval`, line 312) may write, each write needs approval, credentials are redacted.
  - `context_notes`, `new_context`, `wait`, `proc://`, `xd://`: see "Covered" and "Not adopted".
- **The URL vocabulary is closed.** [ADR 0050](0050-extension-declared-url-schemes.md) D2: "The built-in vocabulary does not grow." `crates/tools/url-vocab.json` lists the built-ins (`file, http/https, artifact, history, agent, local, memory, mcp, skill, rule, omp, issue, pr, ssh, security, vault, job, attachment, conflict, adw`). `out` and `cfg` are not in it, so they are not built-in collisions, and D4 lets an extension declare a scheme, with writable and immutable flags, served by the unknown-scheme fallback `HostUriResolver` (`crates/envd/src/tool_url.rs:582`), which already has a write path (`crates/envd/src/tool_url/host.rs:102`). ADR 0050 S1-S5 are not started; `crates/py/python/omp/schemes.py:1` holds registrations inert.
- **Convar flags already encode the cfg safety rules** (`crates/con/src/spec.rs:18-45`): `READONLY` (scripts read, never write), `UNSAFE` (script writes need `sv_cheats`, `UNSAFE_NAME`, `crates/con/src/ctx.rs:26`), `PROJECT`, `SESSION`, `ARCHIVE`. A write with source `SetSource::Script` applies all gates and commits to the session layer (`Ctx::set_value`, `ctx.rs:836`; the gate at `ctx.rs:1422-1426`); `SetSource::Code` bypasses them (`ctx.rs:55`). That is exactly "session-only, gated" for an agent.
- **A device is the existing home for long-tail operations.** ADR 0024/0025 keep the roster small and put the long tail behind `dyn` (`crates/shell-builtins/src/dyn.rs:1`; devices `crates/tool/src/registry.rs:320`). `omp config` has `Dump, List, Get, Set` (`crates/app/src/cli.rs:2319`) but an agent reaches it only through the shell tool.
- Credential scrubbing exists: `redact_sensitive_credentials` (`crates/app/src/diagnostics.rs:241,268`).
- Extensions are Python only (ADR 0036) and carry vendor integrations (ADR 0049); nothing here is vendor-specific.

## Decision

**D1. `out://` is an extension-declared scheme, not a built-in.** A first-party extension in `extensions/` declares `out` as a writable, effect-free scheme through the ADR 0050 manifest. On a write it shows the body to the user as a reply (rendered through `omp.ui`, `crates/py/python/omp/ui/__init__.py:782,1137`). Core, not the extension, bounds the body (ADR 0009, ADR 0050 D5); the 64 KiB figure is the central ceiling's job. It is not available to subagents. The extension is disabled until the user enables it. It depends on ADR 0050 S1-S4 and does not start before S2.

**D2. `cfg://` is a `config` device behind `dyn`, not a scheme.** The device has two operations. `get`: no argument lists namespaces, a namespace lists its settings, a setting returns value, type, default, source and description, with credential values redacted. `set`: a session-only write using `SetSource::Script`, so `READONLY`, `UNSAFE` (`sv_cheats`) and the layer rules apply unchanged. Approval is the exec tier, as ADR 0050 D5 uses for undeclared effects. The device is mounted only for the top-level session (`SV_TASK_RECURSION_DEPTH` 0, `crates/driver/src/subagent/settings.rs:101`), at composition, so the roster never changes mid-session (ADR 0024).

**D3. No persisted agent write.** The `/save` form is not built. An agent writing the global config is a persistence path; upstream gates it to the top-level TUI session for that reason. A user saves with `omp config` or the existing commands.

**D4. Placement.** The device needs authority over the convar context, which has one owner, so it is T0/T1 core (ADR 0049 D1), vendor-neutral. `out://` has no such need, so it is T2 (an extension). Neither changes an existing tool, schema, command or URL scheme.

**D5. Items already covered or not adopted stay out.** See the two lists below.

### Covered (class a)

- **`wait`.** The `hub` tool's `wait` op (`crates/tools/src/hub.rs:28,111`; `crates/driver/src/subagent/hub.rs:160`): `ids`, `timeoutMs` (zero is infinite), `from`, `await`; it races every selected source and returns the first peer message or settled job.
- **`proc://`.** Functions are `hub` ops: `Jobs` returns `id, kind, status, owner, started, output, error` (`hub.rs:166,459-472`), `Cancel` kills by id (167), `Ps`, `Describe`, `Logs`, `Stop`, `Restart` handle named processes and require a name (`crates/tools/src/hub.rs:491`). A job's full result is also readable as `agent://<id>` (`agent_projection`, `tool_url.rs`). **Job output cap:** a job's output is bounded when it settles (`commit_settlement`, `crates/agent/src/jobs.rs:2375`) by `output_bound`, default `DispatchPolicy::DEFAULT_MAX_OUTPUT_BYTES` = 64 KiB (`jobs.rs:720`, `crates/agent/src/dispatch.rs:107`), so the `hub jobs` row is capped at 64 KiB. Not covered: upstream's `proc://<id>/streams` view of bytes a later pipeline stage withheld; omp2 shows the job's own output only.
- **`xd://`.** The `dyn` builtin and device catalog (`crates/shell-builtins/src/dyn.rs:1`, `crates/tools/src/device.rs:1`, `crates/tool/src/device_path.rs:18`). `xd://` survives only as a v1-compat classifier in ACP events (`crates/app/src/acp_events.rs:599,625`). Upstream computes approval per payload from the mounted tool; omp2 uses the device's declared effects (`MountedDevice.effects`).

### Not adopted (class b)

- **`context_notes` and `new_context`.** Upstream gates both behind `compaction.experimentalContextManagement`, default false (`up:session/context-settings.ts:119`); `new_context` is a bare signal consumed by a lifecycle omp2 does not have (`up:tools/context-notes.ts:81,155`). Adding either would add roster tools (ADR 0024). omp2's `/handoff` is a user command (`crates/chat/src/commands/mod.rs:111-118`), not an agent tool. The v1 importer already reports an agent that asks for them as `NotMigratable` (`crates/driver/src/v1_import/agents.rs:144-145`, `agents_tests.rs:220,615`). omp2 does have a reset primitive (the importer maps `reset_boundary` to a `Compaction` with method `clear`, `crates/app/src/session_import/convert/omp1.rs:167,372`), so a notebook could be built on compaction records if upstream graduates the feature.

## Options rejected

- **Add `out` and `cfg` to `url-vocab.json`.** Contradicts ADR 0050 D2.
- **`cfg://` as an extension scheme.** An extension cannot write core convars: `omp.convars` declares extension-owned ones only (`crates/py/python/omp/convars.py:40`).
- **`cfg://` writes with `SetSource::Code`.** It bypasses `READONLY` and `UNSAFE` (`ctx.rs:55`).
- **Persisted `cfg://.../save`.** See D3.
- **`out://` through a new `say`-style roster tool.** Adds a permanent tool (ADR 0024) for a rare need.
- **Do nothing for `out://`.** Acceptable fallback: assistant text between tool calls is already streamed. Whether it reaches the transcript before the turn ends was not verified; it matters only for the shell-redirect and eval transports.

## Consequences

- An agent can read its settings and change a session-only value, with approval, without a shell call; every existing convar gate still applies.
- `out://` waits on ADR 0050. Until then nothing changes.
- No built-in scheme, roster tool, tool schema or CLI command changes.

## Slices

One PR each. `sv_config_device_enabled` (bool, default false) gates C1-C2. `out://` ships as an extension disabled by default with an extension-owned convar `out.enabled` (default false, declared through `omp.convars.declare`). Prefixes follow ADR 0012. No `cap-*` feature: no new heavy dependency.

| Slice | Content | Acceptance | Tests |
|---|---|---|---|
| C1 | `config` device, read form (`omp-tools`) | List, namespace and one-setting reads; credential values redacted | A seeded credential never appears; unknown name fails; device absent when the convar is off |
| C2 | `config` device, session write | `SetSource::Script`; `READONLY` and `UNSAFE` refuse; exec-tier approval; only at depth 0 | Write to a `READONLY` var refused; `UNSAFE` var refused until `sv_cheats`; value is session-layer only and gone after `reset`; not mounted at depth 1 |
| X1 | `out` extension (after ADR 0050 S2) | Manifest declares `out` writable and effect-free; a write shows the body as a reply; subagents get nothing | Write shows one reply; over-ceiling body bounded by core; not offered to a subagent; disabled by default |
| X2 | Shell redirect and eval transports | `cmd > out://` and an eval `tool.write` deliver the same reply | Redirect test through `crates/tools/src/shell_uri.rs` rewriting; eval bridge test |

## Open questions

- **O1.** What approval tier does a write to a declared, effect-free scheme get? ADR 0050 D5 covers fetch and credentials, not writes, and a write today goes through `host::write` (`crates/envd/src/tool_document.rs:1694`). Recommendation: a manifest declaration of no effects gives read tier, so replies need no prompt as upstream; anything undeclared stays exec.
- **O2.** How does the extension know it runs in the top-level interactive session? Recommendation: the host mounts it only there at composition, not the extension checking at run time.
- **O3.** Is X2 (shell redirect) acceptable? It needs a registration seam in `shell_uri.rs` for a declared scheme. Recommendation: defer; ship `write out://` only.
- **O4.** Should the persisted `cfg://.../save` ever ship? Recommendation: no.
- **O5.** Should omp2 add upstream's per-pipeline-stage stream view to `hub`? Recommendation: no; the user can run a job without `| tail`.

Settled by the code, not open: the `hub jobs` output cap (64 KiB); `xd://` is covered by `dyn`.

## Acceptance tests

1. `url-vocab.json` is byte-identical after this record lands.
2. The `config` device refuses a write to a `READONLY` variable, refuses an `UNSAFE` variable until `sv_cheats` is on, and commits only to the session layer.
3. No credential value is returned by `get`.
4. The `config` device is not mounted for a subagent, and the roster is identical before and after a session starts.
5. With the convars off, no command, tool or schema differs from today.
6. `out://` text reaches the user without ending the turn, and a subagent cannot use it.

## Status in omp

**Not started.** Reusable: convar flags and `SetSource`, `dyn` and device mounting, `hub`. Blocked: X1-X2 on ADR 0050 S2. Unverified: whether a declared scheme write can already reach an extension handler end to end (`HostUriResolver` has a write path, but ADR 0050's extension path is inert).

## Upstream origin and license

Draws on `up:internal-urls/out-protocol.ts`, `cfg-protocol.ts`, `proc-protocol.ts`, `xd-protocol.ts`; `up:tools/context-notes.ts`, `wait.ts`; `up:session/context-notes.ts`, `context-settings.ts`. License: MIT, "Copyright (c) 2025 Mario Zechner; (c) 2025-2026 Can Bölük; (c) 2026 Stencil Labs, Inc." (upstream `LICENSE`). omp2 re-implements the behaviour in Rust and Python; if reply or settings prompt text is copied, keep the notice and list it in `THIRD-PARTY-NOTICES.txt`.

## References

ADR 0009 (bound output once); ADR 0012 (convars); ADR 0024/0025 (roster, long tail behind `dyn`); ADR 0036; ADR 0049; ADR 0050; `crates/con/src/spec.rs`; `crates/con/src/ctx.rs`; `crates/tools/src/hub.rs`; research note [context-tools](../research/context-tools.md).
