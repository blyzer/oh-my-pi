# Deferred ADR work audit — 2026-10-21

Verified against `omp2` at `9e0c073d04` (PR #366 merged). Sources: `docs/adr/*.md`, `docs/adr/STATUS.md`, `docs/parked/`, `docs/audits/`, and merged PR history (`gh pr list --state merged --limit 120`).

---

## Key: gate types

| Code | Meaning |
|------|---------|
| OD | Owner decision required |
| PP | Prerequisite PR or branch |
| ME | Measurement required |
| EX | External dependency (outside the repo) |

## Key: classifications

| Code | Meaning |
|------|---------|
| AN | Actionable now — no gate blocks it |
| BD | Blocked on named dependency |
| ST | Stale or obsolete — superseded or no longer relevant |
| AD | Already done — work landed; ADR status text not yet updated |

---

## 1. ADR 0039 — Remote control and factory modes

Source: `docs/adr/0039-remote-control-and-factory-modes.md` §Status in omp, §Implementation plan.

ADR status as written: **Partially implemented (S0 done).**

Phases A1–A7 landed in PRs #331, #336, #341, #342, #345, #346 (all merged 2026-10-19). Phase S0 done in PR #129. Phase R1–R4 (TLS listener, daemon sessions, full attach) and F1–F4 (fleet) are open.

| ID | Deferred item | Gate | Prerequisite satisfied on 9e0c073d04? | Classification | Parallel-safe? |
|----|---------------|------|----------------------------------------|----------------|----------------|
| 0039-A | `omp.session.v1` proto and device key enrollment (R1) | PP | Yes — `omp.session.v1` merged in #331 (`9e0c073d04` ancestry); Ed25519 in #341 | AD | — |
| 0039-B | `SessionHost` UDS listener (R1) | PP | Yes — merged #336 | AD | — |
| 0039-C | `omp attach` command (R1/R2) | PP | Yes — merged #342 | AD | — |
| 0039-D | DOM subscription / AttachFrame pump (R2) | PP | Yes — merged #346 | AD | — |
| 0039-E | External approval `decided_by` wiring (R2/A7) | PP | Yes — merged #345 | AD | — |
| 0039-F | TLS non-loopback listener; full network session attach (R3) | OD + PP | No — `omp-rpc` TLS listener has no caller outside `crates/rpc`; open question 12 (make TLS mandatory, breaking for plain-text `auth-gateway`) unresolved | BD | Yes — isolated to `crates/rpc`, `crates/serve` |
| 0039-G | Fleet: `omp.fleet.v1`, `omp-fleet` crate, leased pull model (F1–F4) | OD | No — no fleet proto or crate exists; open questions 6–10 unresolved | BD | Yes — isolated to new crates |
| 0039-H | WebSocket/JSON session wire (open question 1) | OD | No | BD | Yes |
| 0039-I | Minimized remote stub for container/VM targets (from ADR 0006) | PP | No — deferred explicitly to 0039; R3+ not started | BD | Yes |
| 0039-J | `omp.control.v1` session snapshot/delta messages deleted (open question 11) | OD | No | BD | No — touches live protocol |

---

## 2. ADR 0040 — Remote build and test execution

Source: `docs/adr/0040-remote-build-and-test-execution.md` §Status in omp, §Implementation plan.

ADR status as written: **Not started.** This is stale. `crates/exec`, `crates/exec-reapi`, `crates/exec-buildbuddy`, `crates/exec-local`, `crates/exec-ssh` all exist on `omp2` at `9e0c073d04`. PRs #261, #293, #304, #309, #312, #318–321, #329–330, #332, #351, #354, #360 all merged.

| ID | Deferred item | Gate | Prerequisite satisfied on 9e0c073d04? | Classification | Parallel-safe? |
|----|---------------|------|----------------------------------------|----------------|----------------|
| 0040-A | Slice 0 `bb remote --script` operator trial | ME | Yes — `docs/audits/remote-verification-trial.md` records measurements; #226 and #353 merged | AD | — |
| 0040-B | Slice 0b raw-REAPI probe (`bb execute`) | ME | Yes — recorded in `remote-verification-trial.md`; recycling refuted | AD | — |
| 0040-C | Slice 1 PR 1a: `omp-exec` domain crate | PP | Yes — merged #261 | AD | — |
| 0040-D | Slice 1 PR 1b: `omp-exec-reapi` crate with vendored protos | PP | Yes — merged #304 | AD | — |
| 0040-E | Slice 1 PR 1c: `capture_inputs` in envd | PP | Yes — merged #293 | AD | — |
| 0040-F | Slice 1 PR 1d: wiring `sv_exec_*` convars, BuildBuddy backend, `--exec-policy` | PP | Yes — #309, #312, #354 merged | AD | — |
| 0040-G | Slice 2: four-way router, placement proptest, fallback, output modes | PP | Yes — #319, #320, #330 merged | AD | — |
| 0040-H | Slice 3: SSH backend in `crates/exec-ssh` | PP (partial) | Yes — `crates/exec-ssh` exists; exact completion unverified against spec | AD (likely) | Yes |
| 0040-I | P13 e2e proof (remote execution over fake REAPI) | PP | Yes — merged #321 | AD | — |
| 0040-J | `omp.exec.*` telemetry vocabulary | PP | Yes — merged #318, #329, #351 | AD | — |
| 0040-K | BuildBuddy Run API backend (warm snapshots) | PP | Partial — `crates/exec-buildbuddy` exists; warm-run via Run API vs raw REAPI is UNVERIFIED against the spec; audit records #353 measuring client CPU | AN (verify) | Yes — isolated crate |
| 0040-L | ADR status text update to reflect all merged slices | OD | No — status still says "Not started" | AN | Yes — docs only |
| 0040-M | Owner decisions: R0 (laptop as CI runner), disclosure, image hosting, Linux CI gate, budget tier | OD | No — six open questions unresolved | BD | — |
| 0040-N | `buf breaking` CI check for ADR 0048 REAPI protos | PP | Partial — `buf breaking` added to `crates/proto` (ADR 0048 #257), but REAPI protos under `crates/exec-reapi/proto/` are excluded per design; no gap | ST | — |

---

## 3. ADR 0041 — Native surfaces over TSP

Source: `docs/adr/0041-native-surfaces-over-tsp.md` §Status in omp.

Phases 0–4 are partially landed. Phase 3 (native editor events, autocomplete, picker, effort, screen surfaces) and phase 4 (`flow` surfaces, `TERN_BLOB_DIR`, ConPTY input) are open. ADR 0034 amendment not written.

| ID | Deferred item | Gate | Prerequisite satisfied on 9e0c073d04? | Classification | Parallel-safe? |
|----|---------------|------|----------------------------------------|----------------|----------------|
| 0041-A | Phase 3: native `editor` editing events (`edit`, `undo`, `send`), autocomplete overlay, `picker`, `prefs`, `effort` nodes | PP | Partial — #344 merged (editor edit/undo/send, autocomplete, picker for TSP phase 3); re-verify exact coverage | AD (likely) | Yes — isolated to `crates/chat/src/tsp.rs` |
| 0041-B | Phase 3: `screen` surfaces | PP | No — not found | AN | Yes — chat crate only |
| 0041-C | Phase 4: `flow` surfaces for print mode | PP | No | AN | Yes — chat and app crates |
| 0041-D | Phase 4: `TERN_BLOB_DIR` integration | PP | No | AN | Yes |
| 0041-E | Phase 4: Windows ConPTY input decoding | EX | No — Windows CI does not run | BD | Yes |
| 0041-F | ADR 0034 amendment for TSP viewport cutover | OD | No — ADR 0034 still has `cl_resize_policy` convar and no geometry (status row says "Yes" owner decision needed) | BD | Yes — docs only |
| 0041-G | GUI host decision (deferred in ADR) | OD | No | BD | — |
| 0041-H | Verified in real Tern terminal (not scripted fake) | EX | No — status says "Not verified" | BD | — |

---

## 4. ADR 0042 — Apple PCC integration

Source: `docs/adr/0042-apple-pcc-integration.md`, `docs/parked/2026-10-10-apple-pcc-next-steps.md`.

Status: **proposed; requires owner review. No implementation authorized.** Gates 1–4 are owner/external-gated.

| ID | Deferred item | Gate | Prerequisite satisfied on 9e0c073d04? | Classification | Parallel-safe? |
|----|---------------|------|----------------------------------------|----------------|----------------|
| 0042-PCC1 | Obtain managed PCC entitlement from Apple | EX | No — external Apple account action | BD | — |
| 0042-PCC2 | Signed macOS PCC proof host | EX | No — blocked on PCC-1 | BD | — |
| 0042-PCC3 | Verify CLI eligibility for PCC | EX | No — blocked on PCC-2 | BD | — |
| 0042-PCC4–7 | Extend `omp-ai::local::applefm`, catalog/runtime, tests, docs | OD + EX | No — blocked on PCC-3 and owner review | BD | — |

---

## 5. ADR 0043 — Native CLI integration (ACP client)

Source: `docs/adr/0043-native-cli-integration.md` §Status in omp, §Amendment 2026-10-20.

Codex adapter (`omp_envd::codex_app_server`) and ACP client (`omp_envd::acp_client`) landed (#299, #361). CLI-1 and CLI-3 common contract landed (#287, #305). `RoutePermissionResolver` landed #356. Driver conversion to `DelegatedPolicy` merged #359. Delegated run host merged #362. ACP probe harness merged #363.

| ID | Deferred item | Gate | Prerequisite satisfied on 9e0c073d04? | Classification | Parallel-safe? |
|----|---------------|------|----------------------------------------|----------------|----------------|
| 0043-A | ACP launch profile: DeepSeek Harness (`dsh --profile acp`) | PP | No: `ExecutorRegistry` builds only the Codex executor (`crates/driver/src/delegated_host.rs`). A no-credential handshake is recorded (`docs/research/acp-handshakes/dsh-no-credential.md`, #363), but ADR 0043 also requires a completed prompt, `session/cancel` and a denied permission, which the README records for no binary | BD | Yes: `crates/driver/src/delegated_host.rs` |
| 0043-B | ACP launch profile: Devin (`devin acp`) | PP | No: same registry and recording gaps. Also `AcpExecutor` returns `AuthRequired` from `initialize` whenever `authMethods` is non-empty (`crates/envd/src/acp_client/delegated_session.rs:150`), which Devin always advertises, so a signed-in Devin is refused until that heuristic is replaced | BD | Yes |
| 0043-C | ACP launch profile: Grok (`grok agent stdio`) | PP | No: same registry and recording gaps. Grok fails `session/new` with `-32000` and expects an authentication step the client never sends (README finding 3); the operator reports the account quota is exhausted | BD | Yes |
| 0043-D | ACP launch profile: Antigravity (`agy_acp_server.par`) | PP + EX | No — `agy_acp_server.par` authentication method and Python dependency unverified | BD | Yes |
| 0043-E | ACP launch profile: Claude Code (via `@agentclientprotocol/claude-agent-acp`, Node 22) | PP + EX | No — bridge licence conflict (`proprietary` vs `Apache-2.0`); node bridge not installed by OMP | BD | Yes |
| 0043-F | `omp-sandbox` wrapping an arbitrary agent child (`DelegatedPolicy` enforcement) | PP | No — unverified per ADR status | AN | Yes — isolated test |
| 0043-G | Claude Agent SDK alternative on embedded free-threaded Python 3.14t | OD | No — deferred explicitly | BD | — |
| 0043-H | CLI-7 opt-in real-CLI tests; CLI-8 user-facing docs/configuration | PP | No | AN | Yes |
| 0043-I | Serving document authority tools through ACP client | OD | No — deferred explicitly in amendment | BD | — |

---

## 6. ADR 0044 — Presentation surface and agent boundaries

Source: `docs/adr/0044-presentation-surface-and-agent-boundaries.md`.

Status: **proposed; requires owner review. Implementation gate blocks all work.**

| ID | Deferred item | Gate | Prerequisite satisfied on 9e0c073d04? | Classification | Parallel-safe? |
|----|---------------|------|----------------------------------------|----------------|----------------|
| 0044-ALL | All implementation items | OD | No — owner review gate; canonical Surface IR, adaptive transport, WebGPU-first all explicitly deferred | BD | — |

---

## 7. ADR 0045 — envd responsibility boundaries

Source: `docs/adr/0045-envd-responsibility-boundaries.md`.

Status: **proposed; requires owner review. No implementation authorized.**

Required evidence before acceptance: P8 baseline for W1–W9, daemon-kill test with in-flight invocations, cold build-time numbers, owner answers to Decision item 7 open questions.

| ID | Deferred item | Gate | Prerequisite satisfied on 9e0c073d04? | Classification | Parallel-safe? |
|----|---------------|------|----------------------------------------|----------------|----------------|
| 0045-CONTAIN-A | Fail pending remote routes with a typed error when the remote backend closes. Trace reports `withdraw_relayed` (`crates/env/src/partition.rs:259`) withdraws only approvals and reflections, leaving `routes.invocations` and `routes.requests` entries; not independently re-read | OD + ME | No: ADR 0045 is proposed with no implementation authorized, and the W8 daemon-kill evidence test is not written | BD | Yes: `crates/env/src/partition.rs` only |
| 0045-CONTAIN-B | A remote-bound frame sent after the remote backend closed ends the whole router, including the Local backend. Verified by reading: `crates/env/src/partition.rs:210` propagates `send_async` failure with `?` out of the router loop. Not reproduced by a test | OD + ME | No: same owner gate; the reproducing test (W8) is the first deliverable | BD | Yes: `crates/env/src/partition.rs` only |
| 0045-RECONNECT | `EnvClient` reconnect; ADR 0045 Decision 5 says "decide on reconnect" and item 7 defers it | OD | No: owner decision pending | BD | Yes: `crates/env` |
| 0045-REST | Remaining ENVD-01…15 steps (vocabulary extraction, peripheral migration, hot-path changes) | OD + ME | No: P8 baseline not recorded; owner open questions unresolved | BD | No: touches many crates |

---

## 8. ADR 0046 — Recover v1 extensibility without centralization

Source: `docs/adr/0046-recover-v1-extensibility-without-centralization.md`.

Status: **proposed; requires owner review. No implementation authorized.**

| ID | Deferred item | Gate | Prerequisite satisfied on 9e0c073d04? | Classification | Parallel-safe? |
|----|---------------|------|----------------------------------------|----------------|----------------|
| 0046-ALL | Table-driven built-in roster, vocabulary crate, additive events, extension UI, data-only discovery, peripheral extraction | OD + ME | No — baseline measurements (build timings, envd transitive crate count, co-change share) not recorded; three owner questions unresolved | BD | — |

---

## 9. ADR 0047 — Agent build execution model

Source: `docs/adr/0047-agent-build-execution-model.md` §Status in omp, §Gates.

ADR status as written: **Not started. Blocked on Gate 1.** This is stale. G1 requires owner acceptance; G2 (remote invocation proven) closed by PR #339 (invocation `03d524aa-71d2-40ed-b92f-f1da843c63af`, 54/54 tests, 2026-10-19). `BuildReport`+`BuildStatus` merged #350 (S2–S3).

| ID | Deferred item | Gate | Prerequisite satisfied on 9e0c073d04? | Classification | Parallel-safe? |
|----|---------------|------|----------------------------------------|----------------|----------------|
| 0047-G1 | Owner acceptance of ADR | OD | Not verified in writing — ADR status still says "Not started"; G2 closed by #339 | BD | — |
| 0047-G2 | Remote invocation proven | PP | Yes — #339 merged; invocation ID recorded | AD | — |
| 0047-G3 | Credential containment proof | OD + PP | No — first remote backend credential model undecided | BD | Yes |
| 0047-G4 | Local parity and F11 closed | PP | Verified for the criteria named in the TRUST-F11 row: F11e is open for `cleanse`. `BuildReport` landed in #350 | AN | Yes: `crates/driver/src/cleanse/production.rs` |
| 0047-G5 | Capture correctness (git-semantics `CaptureInputs`) | PP | Yes — `capture_inputs` merged #293 | AD | — |
| 0047-G6 | Structured results (`BuildReport`/`BuildStatus`) | PP | Yes — merged #350 | AD | — |
| 0047-G7 | Observability correlation chain (`omp.exec.*` spans) | PP | Yes — #318, #329, #351 merged | AD | — |
| 0047-G8 | Measured relief before remote-by-default | ME | No — measurements recorded in #353 for client CPU/mem; remote-by-default has not been enabled | AN (verify) | Yes — audit doc |
| 0047-ADR | ADR status text update to reflect G2, G5–G7, S2–S3 landed | OD | No — status still says "Not started" | AN | Yes — docs only |

---

## 10. ADR 0048 — Schema and serialization boundaries

Source: `docs/adr/0048-schema-and-serialization-boundaries.md` §Deferred items, §Implementation gate.

`buf breaking` landed in #257 (`ci: check Protobuf wire compatibility with buf breaking`). #285 (`fix: durable decode hardening (ADR 0048 backlog B1-B8)`) landed part of the backlog. A PR title was not accepted as evidence: each item below was checked against the code, and B4 was re-read by hand because two independent passes disagreed on it.

| ID | Deferred item | Gate | Prerequisite satisfied on 9e0c073d04? | Classification | Parallel-safe? |
|----|---------------|------|----------------------------------------|----------------|----------------|
| 0048-bufbreak | `buf breaking` CI step and `buf.yaml` `breaking:` section | PP | Yes: merged #257; `crates/proto/buf.yaml` `breaking:` (WIRE), `.github/workflows/ci.yml` protobuf job, `justfile` `proto-breaking`. Owner still has to confirm the ignore list and run `just proto-breaking` locally (ADR "Required evidence") | AD | n/a |
| 0048-B1-B3 | Lenient `ToolResult::Wire` (B1), typed newer-revision journal error (B2), frame cap and streaming decode (B3) | PP | Yes: reported at `crates/journal/src/data.rs:580`, `lib.rs`, `sse.rs`; B1 variant choice (lenient implemented) still needs owner recording | AD | n/a |
| 0048-B6-B8 | postcard DB versioning (B6), tonic message-size limits (B7), delete `cache::backend` (B8) | PP | Yes: reported at `crates/ai/src/session/store.rs`, `crates/app/src/daemon.rs`; `cache::backend` file removed | AD | n/a |
| 0048-B4 | Validate frames before prost decode on ext-host/worker paths, or delete the validators | PP | PARTIAL, verified by reading: `validate_worker_frame` is called only in the Ping probe (`crates/envd/src/exec.rs:3979`); `crates/envd/src/worker.rs` decodes (`UiHostEnvelope::decode`, line 2368) without it; `validate_host_frame` has no production caller (tests only) | AN | Yes: `crates/envd/src/worker.rs`, `crates/proto/src/bounds.rs` |
| 0048-B5 | Golden `.oms` fixture corpus with a replay test | PP | PARTIAL: `fixtures/journal/` holds two fixtures (`chat-stream`, `tool-calls-and-rewind`). A pass reports `compaction`, `tool.update` and `patch` kinds unpinned; not independently re-read | AN | Yes: `fixtures/journal`, `crates/session/tests` |
| 0048-B9 | Drift fixes: `limits.py` `SCHEMA_REV`, `processes.md` `WorkerProcess::spawn`, `build.rs` comment | PP | OPEN: `crates/py/python/omp/limits.py:39` still reads `SCHEMA_REV = 7`. The other two were reported open, not re-read | AN | Yes: docs and comment fixes |
| 0048-D1 | GC dropping unknown SSE lines | OD | No — deferred explicitly | BD | Yes — `crates/journal` |
| 0048-D2 | Process-store byte encoding and version policy | OD | No — deferred | BD | Yes |
| 0048-D3 | Workspace-record fsync | OD | No — deferred | BD | Yes |
| 0048-D4 | `transcript_v4_json` rename | OD | No — deferred | BD | Yes |
| 0048-CI | CI rule tying `.proto` diffs to `SCHEMA_REV` bumps | OD | No — deferred; false-positive concern; owner policy needed | BD | Yes — `ci.yml` only |
| 0048-HT | old-build ↔ new-build handshake tests | PP | No — deferred until gateway is deployed separately | BD | — |
| 0048-split | Split `omp-proto` per package | ME | No — deferred until generation benchmark decision rule fires | BD | — |

---

## 11. Parked work: security trust model

Source: `docs/parked/2026-10-06-handoff-pending-work.md` §1, `docs/parked/2026-10-09-handoff-pending-work.md` §Carried over.

PR2 (trust rows, locked grant writer) landed #191, #196. PR3–PR8 (driver plumbing, envd gating, `omp trust`, overlay, approval floor) were not started as of 2026-10-11.

| ID | Deferred item | Gate | Prerequisite satisfied on 9e0c073d04? | Classification | Parallel-safe? |
|----|---------------|------|----------------------------------------|----------------|----------------|
| TRUST-PR3 | Driver trust plumbing (nothing outside `omp-ext` reads a trust decision) | PP | Partial — #272, #273, #274, #275, #276, #277 merged (trust slices 3–8); re-verify exactly | AD (likely) | — |
| TRUST-PR4 | `omp trust`, workspace trust CLI | PP | Yes — #274 merged | AD | — |
| TRUST-MCP | Explicit per-workspace MCP approval | PP | Yes — #273 gates project MCP on workspace trust | AD | — |
| TRUST-F11 | F11: `omp adw run` code phases sandboxed, operator cfg applied, Ctrl-C wired | PP | F11a-d and F11f were reported closed by a code read (not re-read here). F11e is open for cleanse, verified by reading: `crates/driver/src/cleanse/production.rs:136` builds default `KernelOptions` (`include_workspace: true`, `crates/driver/src/headless/kernel.rs:129`) while ADW sets it false (`adw/production.rs:365`, test at `:901`). Bounded by workspace trust (`kernel.rs:2498`): only a trusted workspace's `.omp/extensions` load | AN | Yes: `crates/driver/src/cleanse/production.rs` |
| TRUST-F2 | F2: native `.omp/extensions` with self-declared grants | OD | Not started as of parked note; unverified on HEAD | AN | Yes |
| TRUST-F3 | F3: LSP/DAP project binaries not gated | PP | Partial — #265 (sv_lsp_project_binaries off by default) merged; full gating unverified | AN (verify) | Yes |
| TRUST-HOSTS | `hosts.toml` writes and `.cfg.lock` sibling follow symlinks | PP | Partial — #263 merged (harden cfg-lock readers); exact symlink fix unverified | AN (verify) | Yes |
| TRUST-BUILTIN | In-process builtins (`cat`, `head`, `grep`) ignore `read_deny` | PP | Yes — #268 merged (builtins honour read policy, slice 5) | AD | — |

---

## 12. Parked work: ADR follow-ups

Source: `docs/parked/2026-10-06-handoff-pending-work.md` §5, `docs/adr/STATUS.md` §Code departing.

| ID | Deferred item | Gate | Prerequisite satisfied on 9e0c073d04? | Classification | Parallel-safe? |
|----|---------------|------|----------------------------------------|----------------|----------------|
| FOLLOW-0006 | Per-call admission and nested-call hooks in eval (ADR 0006 amendment) | OD | No — "per-call admission is deferred until route- or session-scoped approvals exist" | BD | Yes |
| FOLLOW-0007 | Gitignored files not copied into CoW subagent view | PP | No — open gap retained in ADR; status row confirmed | AN | Yes — `crates/envd` workspace snapshot |
| FOLLOW-0024 | Goal/prewalk/mode prompts still move system prefix | OD | No — STATUS.md still lists this under "owner decision needed" | BD | Yes |
| FOLLOW-0034 | Full/compact/pulse geometry; `cl_resize_policy` removal; TLC in CI | OD | No — owner decision row still says "Yes" in STATUS.md | BD | Yes |
| FOLLOW-0035 | 245 bare-string and 1042 formatted error variants migrated to zero | PP | No — ratchet holds baseline, migration to zero ongoing | AN | Yes — one crate at a time |
| FOLLOW-0038 | Matcher pooled per-key caches (§5); redirect notices + hook events carry matched excerpt (§4) | OD | No — "open implementation decisions (owner)" in ADR text | BD | Yes |
| FOLLOW-0038B | Stream-rules P2/P5/P6 test rows; matcher proptests; rewind/interrupt/budget rows | PP | No — not found | AN | Yes |
| FOLLOW-PY-DOCS | `docs/py/05-hooks.md` and `docs/py/07-ui.md` rewritten against `KernelEvent` | PP | Not verified on HEAD | AN | Yes — docs only |
| FOLLOW-AGENTS | `AGENTS.md` tokio::watch reference (flume/CancellationToken actual) | PP | Not verified on HEAD | AN | Yes — docs only |

---

## 13. Parked work: accounts and cache

Source: `docs/parked/2026-10-06-handoff-pending-work.md` §3–4.

| ID | Deferred item | Gate | Prerequisite satisfied on 9e0c073d04? | Classification | Parallel-safe? |
|----|---------------|------|----------------------------------------|----------------|----------------|
| ACCT-2 | `--api-key` precedence over stored accounts (Slice 2) | PP | Not verified on HEAD | AN | Yes — `crates/ai` |
| CACHE-AUTO | `ai_cache_retention auto` marker placement decision | OD | No — decision pending | BD | — |
| CACHE-VOLATILE | Extract volatile band from system prefix into separate band | PP | Yes — #284 merged (volatile band moved out of system prefix) | AD | — |

---

## 14. Parked work: ADR 0043 open questions

Source: `docs/adr/0043-native-cli-integration.md` §Amendment 2026-10-20.

| ID | Deferred item | Gate | Prerequisite satisfied on 9e0c073d04? | Classification | Parallel-safe? |
|----|---------------|------|----------------------------------------|----------------|----------------|
| 0043-Q-U2-U5 | Codex live server feasibility items (system-prompt injection, policy merge, effective policy in ThreadStartResponse) | EX | No — mock peer only; no live server observed | BD | — |
| 0043-Q-TERMS | Claude bridge licence conflict (`proprietary`/`Apache-2.0`); `agy_acp_server.par` Python dependency and auth | OD + EX | No — unresolved per ADR | BD | — |

---

## Classification summary

| Classification | Count |
|----------------|-------|
| AD (already done, ADR text stale) | 28 (25 confirmed, 3 marked "likely") |
| AN (actionable now) | 23 (19 confirmed, 4 marked "verify") |
| BD (blocked on named dependency) | 43 |
| ST (stale/obsolete) | 1 |
| **Total rows** | **95**, counted from the tables by the classification column. 7 rows (`AD (likely)`, `AN (verify)`) are not verified. Refreshed 2026-10-21 after #368: 0043-A/B/C moved from actionable to blocked; the F11 rows are now confirmed |

---

## Top 5 actionable-now items

Ranked by value (impact × readiness):

| Rank | ID | Work | Why now | Touches |
|------|----|------|---------|---------|
| 1 | 0040-L + 0047-ADR | Update the ADR 0040 and 0047 status text | DONE: #368 merged; the STATUS rows for ADRs 0042-0048 are #369 | `docs/adr/` |
| 2 | 0043-A–C | ACP launch profiles for DeepSeek Harness, Devin, Grok | Not data-only. Prerequisites, in order: register an ACP executor in `ExecutorRegistry` (`crates/driver/src/delegated_host.rs`; only Codex is registered); replace the `authMethods` heuristic (`crates/envd/src/acp_client/delegated_session.rs:150`); record a completed prompt, `session/cancel` and a denied permission per binary, which ADR 0043 requires before a profile is enabled | `crates/driver/src/delegated_host.rs`, `crates/envd/src/acp_client/` |
| 3 | 0047-G4 / TRUST-F11 | Close F11e for cleanse: set `include_workspace: false` for cleanse child kernels as ADW does, with a test | F11e is verified open (TRUST-F11 row); the change mirrors `crates/driver/src/adw/production.rs:360-371` | `crates/driver/src/cleanse/production.rs` |
| 4 | FOLLOW-0007 | Copy gitignored files into the CoW subagent view | Acknowledged open gap in ADR 0007 status. The implementation approach was not verified: this audit did not read the snapshot or reflink code, so a design step comes before any change | `crates/envd/src/workspace/` (unverified) |
| 5 | FOLLOW-0035 | Continue error-variant migration toward zero (one crate at a time) | Ratchet holds per-crate baselines in `tools/lintx/baselines/error-formatting.toml`; each crate is independent; can run in parallel worktrees without coordination | One crate per worktree; no shared files |

---

## Unknowns

The following could not be verified with available evidence:

1. **ADR 0040-H (SSH backend completeness).** `crates/exec-ssh` exists and has tests, but the ADR implementation plan described it as Slice 3 with specific interfaces. Whether it fully satisfies the spec was not confirmed by reading its source in this pass.
2. **ADR 0041-A (`#344` coverage).** PR #344 is titled "ADR 0041 phase 3 — editor edit/undo/send, autocomplete and picker TSP". Whether `screen` surfaces are also in scope was not confirmed; the ADR status says "not started" for screen surfaces.
3. **TRUST-PR3 residual.** Trust slices 3–8 (#272–#277) landed after the parked handoff that called PR3–PR8 "not started". Whether they fully cover driver plumbing and envd gating was not verified by reading the code; the parked note's language predates those PRs.
4. **TRUST-F11 residual.** Resolved in this refresh: F11e is open for cleanse; see the TRUST-F11 row. F11a-d and F11f rest on a code read that was not repeated here.
5. **0048-B9.** Resolved in this pass: `limits.py:39` still reads `SCHEMA_REV = 7`, so B9 is open. The `processes.md` and `build.rs` halves were not re-read.
6. **ADR 0047 G1 written acceptance.** The ADR status text has not been updated; whether the owner has given a written acceptance in a PR description or commit was not searched exhaustively.
7. **`ACCT-2` `--api-key` precedence.** The parked note said "not started" in the 2026-10-06 session; whether any subsequent PR (#251 or later) addressed it was not confirmed.
8. **FOLLOW-PY-DOCS and FOLLOW-AGENTS.** The parked note listed these; whether they were addressed in any of the 120 merged PRs was not verified by reading the files on HEAD.
