# Gap 11: context tools (`context-tools`)

Scope: `context_notes`, `new_context`, `wait`, and the `out://`, `proc://`, `cfg://` schemes; `xd://` is checked as well.
Upstream: can1357/oh-my-pi `c308970263`, paths under `packages/coding-agent/src` (`up:`). omp2: branch `docs/upstream-gap-research` at `f4186b783b`.
Classes: **a** already covered, **b** not needed, **c** port, **d** propose. Decision record: [ADR 0058](../adr/0058-context-url-schemes.md).

| Item | Class | One line |
|---|---|---|
| `context_notes` | **b** | Experimental, off by default; no consumer lifecycle in omp2 |
| `new_context` | **b** | Same gate; a bare signal tool |
| `wait` | **a** | `hub` op `wait` |
| `out://` | **d** | Write-only mid-turn reply channel; no omp2 equivalent |
| `proc://` | **a** | Functions covered by `hub`; no URL form; no per-stage stream view |
| `cfg://` | **d** | Agent-visible settings with approval; omp2 has convar flags that already encode the safety rules |
| `xd://` | **a** | `dyn` builtin and device catalog; the `xd://` spelling is not resolved |

**omp2 scheme registry.** `crates/envd/src/tool_url.rs` registers the unknown-scheme fallback `host` (582), `memory`, `ssh`, `security`, `vault`, `mcp`, `issue`/`pr`, `attachment`, `artifact`, `local` (689), content resolvers, `agent` (704), `history`, `conflict`, `docs` and `adw`. The closed built-in vocabulary is `crates/tools/url-vocab.json` (`file, http/https, artifact, history, agent, local, memory, mcp, skill, rule, omp, issue, pr, ssh, security, vault, job, attachment, conflict, adw`). There is no `out`, `proc`, `cfg` or `xd` entry; nothing under `crates` mentions `out://`, `proc://` or `cfg://`; `xd://` appears only in `crates/app/src/acp_events.rs:599,625`. **ADR 0050 D2 says the built-in vocabulary does not grow**, and D4 lets an extension declare its own scheme (writable or immutable) through the manifest, served by the existing unknown-scheme fallback (`HostUriResolver`, which already has a write path: `crates/envd/src/tool_url/host.rs:102` `resolve_write`). ADR 0050's S1-S5 are not started; `crates/py/python/omp/schemes.py:1` holds registrations inert.

## 1. `context_notes`

**Upstream.** `up:tools/context-notes.ts:81`. Optional `text`: omitted reads the notebook, a string replaces it, `""` clears it; limit 16 KiB (`up:session/context-notes.ts:6`). Persisted as a session custom entry `experimental_context_notes`; `getContextNotes` returns the latest valid revision after the last `reset_boundary` entry. Active only when `compaction.experimentalContextManagement` is true (`up:session/context-settings.ts:119`, **default false**) and the tool's session owns the journal.
**omp2.** No tool, kind or prompt block. Only the v1 importer lists `context_notes` and `new_context` as v1 tool names (`crates/driver/src/v1_import/agents.rs:144-145`) and classifies an agent requesting them `NotMigratable` (`agents_tests.rs:220,615`). omp2 has a native reset primitive: the importer maps `reset_boundary` to a `Compaction` with method `clear` (`crates/app/src/session_import/convert/omp1.rs:167,372-385`), so a notebook scoped to entries after the last reset could be built on compaction records.
**Class b.** An opt-in experiment with no agent-facing consumer in omp2; adding it would add a roster tool (ADR 0024). Revisit if upstream graduates it.

## 2. `new_context`

**Upstream.** `up:tools/context-notes.ts:155`: no parameters, approval `write`, returns "New context window requested." with `details.requested`; "the owning lifecycle consumes this turn-local signal". It does not itself compact.
**omp2.** Absent. `/handoff` is a user slash command (`crates/chat/src/commands/mod.rs:111-118`, a summary path next to `/compact`), not an agent-callable tool, so the agent cannot request a fresh context.
**Class b**, for the same reason as `context_notes`: experimental, off by default, no effect without a consumer lifecycle.

## 3. `wait`

**Upstream.** `up:tools/wait.ts:176`: blocks on background jobs and project services the caller started, and on IRC replies; ends early when no asked agent can still reply; interruptible; timeout.
**omp2.** `hub` (`crates/tools/src/hub.rs`): `Op` (28) includes `Wait, Jobs, Cancel, Ps, Logs, Stop, Restart, Describe, Send, Inbox, List, Start`. `Params` (111) has `ids`, `timeoutMs` (zero means infinite), `from`, `await`. The description says `wait` "races every selected source and returns the first peer message or settled job without consuming unrelated events". Implementation `crates/driver/src/subagent/hub.rs:160`.
**Class a.** The stand-alone name is gone by design; `hub` unifies peers, jobs and named processes.

## 4. `out://`

**Upstream.** `up:internal-urls/out-protocol.ts:68`: write-only, immutable, path ignored, nothing stored. Markdown written there is shown like a response without ending the turn. Transports: `write out://` (streamed as assistant prose), a shell redirect `cmd > out://`, and an eval cell's bridged `tool.write`/`tool.bash`. Reply capped at 64 KiB (`MAX_REPLY_BYTES`, line 35). Enabled by `outRepliesActive` (45): the interactive TUI by default, never subagents.
**omp2.** No scheme. `crates/tools/src/shell_uri.rs:1` ("Quote-aware discovery and replacement of path-backed internal resource URIs") rewrites registered URIs inside shell commands, so the redirect transport needs the scheme registered there. A declared (writable) scheme is the ADR 0050 route.
**Class d.** **Options:** (A) add `out` to the built-in vocabulary: rejected (ADR 0050 D2); (B) a first-party extension declares `out` as a writable, no-fetch scheme (ADR 0050 D4) and renders the body with `omp.ui` (`crates/py/python/omp/ui/__init__.py:782` `notify`; `message_renderer` 1137) (chosen, after ADR 0050 S2-S4); (C) do nothing: the model's own text between tool calls is already streamed (whether it reaches the transcript before the turn ends was not verified). **Plan:** blocked on 0050 S2; then one extension PR; the shell redirect needs a registration seam in `shell_uri.rs` (open question). Default-off convar `cl_out_replies_enabled`.

## 5. `proc://`

**Upstream.** `up:internal-urls/proc-protocol.ts:141`: caller-visible jobs and project services; `proc://<id>/kill` (write, no content) cancels; reading a write-only action returns an error naming the write that acts; `proc://<id>/streams[/<n>]` shows bytes crossing a bash job's pipes, including what a later `| tail` held back.
**omp2.** The same functions are `hub` ops split by target. **Jobs:** `Jobs` returns a row per job with `id, kind, status, owner, started, output, error` (`crates/driver/src/subagent/hub.rs:166,459-472`); `Cancel` kills by `ids` (167). **Named processes:** `Ps`, `Describe`, `Logs` (`lines, head, grep, cursor, follow`; `crates/tools/src/hub.rs:167-176`), `Stop`, `Restart`; these require a `name` (hub.rs:491), so they do not read a job's output.
**Job output cap, settled.** A job's `output` is "bounded serialized output projected from the DOM" (`crates/agent/src/jobs.rs:444`); `commit_settlement` (2375) bounds it with `output_bound`, whose default is `DispatchPolicy::DEFAULT_MAX_OUTPUT_BYTES = 64 KiB` (`jobs.rs:720`, `crates/agent/src/dispatch.rs:107`). So the `hub jobs` row carries a value capped at 64 KiB.
**Class a** for function. Not covered: upstream's per-pipeline-stage `streams` view; omp2's job `output` is the job's own output only. A `proc://` spelling is not added. `job` is already a member of the closed vocabulary (`url-vocab.json`, `JOB`; `Scheme::Job` "Detached-job output", `crates/tools/src/read/resolver.rs:216`), but no resolver is registered for it in `tool_url.rs` (none found by grep). What does read a job by id is `agent://<id>`: `agent_projection` matches `Job` and `Subagent` DOM nodes and returns `id, kind, status, output, data, result` (`crates/envd/src/tool_url.rs`, kind `job`); a miss says "Agent or job `{id}` is not live in the session tree". So a job's full result is readable by URL today.

## 6. `cfg://`

**Upstream.** `up:internal-urls/cfg-protocol.ts` (scheme at 321). Reads: `cfg://` (all), `cfg://<namespace>`, `cfg://<setting>` (value, type, default, source, description). Writes: `cfg://<setting>` is a session-only override, `cfg://<setting>/save` persists to global `config.yml`; only a session created with `settingsApproval` (top-level TUI, check at 312) may write, each write needs user approval (`once`/`session`/`deny`/`timeout`); subagents, print, RPC and ACP never see it. Credentials always redacted.
**omp2.** No agent-facing settings resource. `omp config` has `Dump, List, Get, Set` (`crates/app/src/cli.rs:2319`), reachable by an agent only through the shell tool. The settings catalogue is `crates/catalog/src/settings.rs`. **Convar flags already encode upstream's safety rules** (`crates/con/src/spec.rs:18-45`): `READONLY` "scripts can read but never write", `UNSAFE` "script writes require the `sv_cheats` gate", `PROJECT` (a repo overlay may set it only if opted in), `SESSION`, `ARCHIVE`; `sv_cheats` is `UNSAFE_NAME` (`crates/con/src/ctx.rs:26`). `crates/app/src/diagnostics.rs:265` `sanitize_json` and `redact_sensitive_credentials` scrub dumps.
**Class d.** **Options:** (A) add `cfg` to the built-in vocabulary: rejected (ADR 0050 D2); (B) first-party extension scheme: rejected, an extension cannot write core convars (`omp.convars` declares extension-owned ones only, `crates/py/python/omp/convars.py:40`); (C) a `config` device behind `dyn` with read and session-write over the convar flags (chosen): no scheme, no roster change (ADR 0024/0025); (D) do nothing: the shell `omp config get` already reads. **Plan (default-off `sv_config_device_enabled`):** S1 read form with credential redaction; S2 session-only write honouring `READONLY`, `UNSAFE`/`sv_cheats`, `PROJECT`, effect tier exec by default; S3 persisted write only with a threat review. Persisting is an agent-writes-global-config vector, which is why upstream gates it to the top-level TUI session.

## 7. `xd://`

**Upstream.** `up:internal-urls/xd-protocol.ts:59`: `write xd://<device>` takes a JSON payload; approval is the mounted tool's own argument-dependent approval, resolution devices finalizing an already-previewed action stay at read tier, and anything unresolved, malformed or throwing fails closed to `exec`.
**omp2.** `DevicePath` (`crates/tool/src/device_path.rs:18`, `name[/sub][@publisher/extension]`); catalog rendering (`crates/tools/src/device.rs:1`; `DOCS_TOTAL_BUDGET` 48000 at 22, `PER_DEVICE_DOCS_CAP` 10000 at 24); schema-derived command lines (`device_ctl.rs`); the `dyn` builtin (`crates/shell-builtins/src/dyn.rs:1`: "the only discovery path for long-tail devices ... binds command-line arguments to that schema"). Mounted by `crates/envd/src/tools/registry.rs` (`dyn_installed`). `xd://` is only a v1-compat classifier in ACP events (`acp_events.rs:599`, `625`).
**Class a.** The approval difference: upstream computes it per payload from the mounted tool; omp2 uses the device's declared effects (`MountedDevice.effects`, `crates/tool/src/registry.rs:320-342`). Not compared in detail.

## Open questions

- `out://` shell-redirect transport: is a registration seam in `shell_uri.rs` acceptable, given ADR 0050 keeps the vocabulary closed? Recommendation: write-only `write out://` first; the redirect only if the extension scheme can be registered there.
- `cfg://` persisted write: recommendation, do not ship; session-only.

## Upstream origin and license

Files drawn on: `up:tools/context-notes.ts`, `session/context-notes.ts`, `tools/wait.ts`, `internal-urls/out-protocol.ts`, `proc-protocol.ts`, `cfg-protocol.ts`, `xd-protocol.ts`. License MIT, "Copyright (c) 2025 Mario Zechner; (c) 2025-2026 Can Bölük; (c) 2026 Stencil Labs, Inc." Re-implemented in Rust; keep the notice if text is copied.
