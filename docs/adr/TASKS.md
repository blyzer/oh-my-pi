[docs/adr/TASKS.md (verbatim, 267 lines total)]
[docs/adr/TASKS.md (verbatim, 264 lines total)]
# Active ADR and deferred-work ledger

This file is the active work queue for ADR follow-ups. It is distinct from
`STATUS.md`, which records implementation status for every ADR.

## Authority

Use these sources in order:

1. Current code and tests.
2. Merged PR state on `omp2`.
3. The affected ADR's `Status in omp` and implementation gate.
4. This ledger for sequencing and owner decisions.

A historical audit is evidence. It is not an active task state unless this
ledger promotes the item after revalidation.

Status meanings:

- **DONE**: implemented and verified in the current tree.
- **OPEN**: actionable work remains.
- **BLOCKED**: an owner decision or prerequisite prevents implementation.
- **PARTIAL**: some acceptance evidence exists, but the ADR requirement is not
  complete.
- **HISTORICAL**: retained for traceability; not an active queue.

## Coordinator monitoring record

- 2026-10-28: Native GitHub `run_watch` monitored PR #449 through all terminal
  checks. The 14 required CI jobs, including BuildBuddy and macOS/Linux acceptance
  jobs, passed; PR #449 then merged. No separate BuildBuddy monitor was available.
- The coordinator now records monitor and merge evidence per PR instead of treating
  a targeted local or BuildBuddy invocation as a substitute for PR CI.
- 2026-10-28: Owner accepted ADR 0045 Alternative F and item-7 defaults. Admission remains host-decided with envd enforcement; approval relay is issuing-connection-only; reflection relay is bounded; MCP remains envd-owned; daemon sharing requires matching workspace/trust/policy digests; reconnect and schedules remain deferred.
- 2026-10-28: Owner accepted ADR 0048 with B4 full frame preflight validation, behavior-family golden fixtures including compaction/tool.update/patch, `toolhost` retained in Buf breaking scope, and the lenient B1 decode variant. B9 Variant 1 is the selected implementation direction.

## Decision matrix for remaining ADR work

The owner authorized the council to decide software-development questions. That authority does not
override an ADR's explicit owner-acceptance gate. The council dispatch on 2026-10-28 failed before
producing a verdict because child startup rejected the requested tool set and hit an extension-load
error. No council verdict is recorded.

- **ADR 0045 owner gate:** accept or reject Alternative F before production implementation.
- **ADR 0045 decision item 7:** decide reconnect policy; admission placement on untrusted workers;
  whether approval and reflection relays may violate ADR 0006's duplex prohibition; MCP process
  locus; per-subagent daemon sharing; and the future of schedules.
- **ADR 0045 containment:** after acceptance, authorize typed remote-route termination and local
  backend survival. Reconnect remains a separate policy decision.
- **ADR 0045 P8 evidence:** define or confirm W1-W9 workloads, attached versus embedded modes,
  macOS/Linux execution, metrics, artifact format, and comparison thresholds.
- **ADR 0048 B4:** choose validation of every HostFrame and WorkerFrame before prost decode, or
  deletion of the unused validators; identify the owning decode paths.
- **ADR 0048 B5:** choose the golden corpus scope. Current fixtures cover `chat-stream` and
  `tool-calls-and-rewind`; decide whether `compaction`, `tool.update`, `patch`, and each field
  addition require separate fixtures.
- **ADR 0048 B9:** prove the real worker launcher and WorkerHello path plus gateway Hello behavior,
  or authorize correcting stale documentation and missing production wiring.
- **ADR 0048 acceptance:** confirm the Buf ignore list, especially `toolhost`; choose the B1
  lenient-decode versus revision-bump variant; and run the required local Buf proof.

## Snapshot JSON byte preservation

- **DONE** integration merged as PR #449. The snapshot fix is `e2d0723` plus correction
  commit `c4ceb04` on `fix/snapshot-json-fixed-point`. The correction retains validated
  source bytes while preserving typed scalar `Value` compatibility. Local `omp-dom`
  verification passed 28 tests and doctests. PR CI passed all 14 jobs, including BuildBuddy,
  macOS, Linux, acceptance proofs, and format checks. The PR merged at 2026-10-28T09:30:13Z.
- The configured reviewer dispatch failed before producing a verdict. The session-model fallback
  reviewed the correction and returned READY after the scalar-compatibility issue was fixed.

## ADR 0045 — envd responsibility boundaries

ADR status: **accepted 2026-10-28**. Alternative F selected.

- **DONE** owner acceptance and decision-item-7 answers. Reconnect remains deferred.
- **DONE** W8 daemon-kill containment evidence and implementation. The partition proof and real-daemon macOS proof pass with typed remote terminal errors and local-backend survival.
- **DONE** P8 W1-W9 baseline evidence: PR #450 merged the recorder and envd descriptor fix; BuildBuddy Linux and native macOS workflow run 41070076113 completed 30-repetition attached/embedded coverage with zero failures and no EMFILE, and the local single-process W7/W9 proof also passed under the 16384 descriptor ceiling.
- **DONE** containment fixes in `crates/env/src/partition.rs`: remote pending routes receive typed terminal errors, the Local backend survives Remote closure, and the focused partition suite passes 16/16. The production daemon W8 SIGKILL proof also passes.
- **DEFERRED** reconnect implementation. It is a policy decision, not a safe
  implementation detail.

`adr0045-slice-b` remains a valid investigation report. It confirms the
containment defect and identifies the smallest fix, but it does not authorize
that fix.

## ADR 0048 — schema and serialization boundaries

ADR status: **proposed; owner review required**.

Completed implementation items:

- **DONE** `buf breaking` configuration, CI check, and `just proto-breaking`.
- **DONE** B1, B2, B3, B6, B7, and B8.
- **PARTIAL** B9. `omp.limits.SCHEMA_REV` is synchronized and guarded by
  `frozen_surface.rs`, but `docs/architecture/processes.md` still carries the
  stale `WorkerProcess::spawn` description. Filing/verification of the
  `WorkerHello` and `chat --gateway` issues is also not established by code.
  **OPEN implementation evidence gate:** `docs/architecture/processes.md` names
  `__omp-py-worker`, but the application entrypoint has no matching selector.
  `WorkerSupervisor::publish_process` has a test caller but no production caller
  found under `crates`. Prove the actual worker launcher and protocol before
  correcting the process documentation or closing B9.

Active items:

- **BLOCKED** B4 pending owner acceptance. `validate_worker_frame` is used only for the Ping probe, and `validate_host_frame` has no production caller. The current tree has no normal `HostFrame` decode path, while the worker path already preflights its only `WorkerFrame` decode in `crates/envd/src/exec.rs::take_worker_frame`. Do not add a second or speculative decode path before the owner accepts ADR 0048.
- **BLOCKED** B5 pending owner acceptance and a corpus decision. The committed harness in `crates/session/tests/golden_journals.rs` has two fixture pairs, `chat-stream` and `tool-calls-and-rewind`, and passes its replay proof. It does not cover `compaction`, `tool.update`, or `patch` as separate corpus builders. Add those fixtures only after the owner decides that the ADR's “each kind or field addition” wording requires them.
- **BLOCKED** owner acceptance. Confirm the Buf ignore list, choose the B1 lenient-decode versus revision-bump variant, and run the required local Buf breaking proof.

`just proto-breaking` passes on the current tree. `just e2e-p8` passes. The latest non-gating P8 artifact is workflow run [40246613996](https://github.com/blyzer/oh-my-pi/actions/runs/40246613996), but it measures the synthetic recorder workload, not ADR 0045 workloads W1–W9.

`adr0048-slice-c` is a **HISTORICAL** audit, not the live state. Its B8 and B9
labels are stale relative to the current tree. Use this section, the ADR, and
`STATUS.md` instead.

## Deferred PR audit

The active inventory is the committed
[`docs/audits/pr-deferred-adr-audit-2026-10-21.md`](../audits/pr-deferred-adr-audit-2026-10-21.md)
(95 classified rows: already-done, actionable-now, blocked, stale). The
session-local `pr-deferred-audit` (PRs 1–346, 2026-10-19) is **HISTORICAL**
and superseded by that file.

Deltas verified 2026-10-25 against `omp2` after PR #419:

- **RESOLVED** `SCOPED_WRITERS` typed-confinement follow-up (PR #197 item):
  PR #349 added `Confinement::DocumentAuthority`; the name list is gone.
- **RESOLVED** account-pin notice (PR #172 item): `crates/app/tests/rpc_spine.rs`
  asserts the `account-pin` notice frame.
- **RESOLVED** ADR 0038 open decisions (FOLLOW-0038, FOLLOW-0038B): PR #413.
- **RESOLVED** ADR 0045 W8 evidence (0045-CONTAIN test deliverable): PR #419.
- **Progressed** FOLLOW-0035 error-format migration: ratchet 1042 → 639
  formatted sites after PRs #414–#417.
- **STILL OPEN** `reattach_execution` backend call
  (`crates/agent/src/jobs.rs:979` TODO; blocked on ADR 0039 Part B).
- **STILL OPEN** kernel dispatch `channel_for(None, ..)`
  (`crates/agent/src/dispatch.rs:1549`; owner decision).

Deltas verified 2026-10-25 against `omp2` at `1776c5cd25` (after PR #429):

- **RESOLVED** ADR 0039 phase R1 (read-only attach over the owner UDS). The orphaned PR #342
  is superseded, not recovered: its two lost files were rebuilt against the real projection
  rather than restored, and its `attach_cmd::run` stub that parked on
  `std::future::pending()` was never reinstated. R1 landed as PRs #433 through #442 and the
  P14 proof: `omp_dom::wire`/`redact`, `ControlVisibility`, `SessionBridge`, `SessionWire`
  on the owner socket, `SessionControlRpc` with its four `UNIMPLEMENTED` verbs, the
  `omp attach --observe` client, and the idle-session subscription seam. The unit queue and
  what each unit carries are below; `origin/feat/session-host` holds nothing `omp2` needs.
- **NO ACTION** Codex `auth_required` object-form classification. PR #366
  classified a signed-out `-32000` inside `codex_app_server`; `286ff0945b`
  reverted it and moved the rule to `acp_client::wire::is_missing_login`,
  because that shape arrives through the ACP client. The Codex adapter is
  correct as it stands. Do not re-apply #366.

  Re-confirmed 2026-10-27, because a stash-triage plan written before the
  revert listed the Codex-side typing as the one change worth retaking from
  the stashes. It is not. `crates/envd/src/acp_client/wire.rs:58-61` holds
  `MISSING_LOGIN_CODE` and `MISSING_LOGIN_DATA`, consumed at
  `delegated_session.rs:611` and `:789`, and `acp_client/tests.rs:956-1015`
  pins the exact shape plus four near-misses that must stay internal (`-32603`
  with the same `data`, `-32000` with no `data`, `-32000` with other `data`,
  and a structured `data`). `codex_app_server/session.rs:935-937` states the
  decision in the code: the object form stays `Internal` on purpose. The
  stashes are gone, so retaking it would mean re-implementing the reverted
  approach by hand.

## ADR 0039 R1 — decided and queued

The owner answered R1's blocking questions on 2026-10-26, recorded as the ADR 0039 amendment of
the same date. R1 is gRPC, owner-local, read-only, and carries canonical `omp_dom` bytes. The
`omp.control.v1` session messages are kept. Advisor-subtree redaction is out of R1 because the DOM
has no advisor marker to key on.

Units, in dependency order. Each lands as its own change with its own tests.

1. **DONE** `KernelHandle` carries the session's project root, so `SessionRef.project_root` can be
   filled (PR #433).
2. **DONE** `omp_dom::wire` encodes and decodes `Event`, replacing the private codec in
   `crates/driver/src/collab/session.rs`, and `omp_dom::redact` blanks host-local properties in
   place without pruning nodes or moving handles (PR #434).
3. **DONE** `ControlVisibility`: the configurable redaction table, backed by a convar so the owner
   can change the rules without a recompile (PR #436).
4. **DONE** `SessionBridge`: one bounded replay ring per attached session, an `(epoch, seq)`
   cursor, `Resync` for stale or evicted cursors, ordered `Reset` entries, lossless stream-append
   merging, and a drain task that never blocks the kernel (PR #437).
5. **DONE** `SessionWire` in `crates/driver`: reads `SessionRegistry`, owns the bridge table, binds
   the owner-only UDS at `<data dir>/run/session.sock`, and absorbs the permission and stale-socket
   handling from the unused `SessionHost` (PRs #439, #440).
6. **DONE** `SessionControlRpc` in `crates/serve`: `ListSessions` and `Attach` over a
   `SessionControlAuthority` trait that driver implements, with identity taken from transport
   extensions. The four mutating verbs answer `UNIMPLEMENTED` until R2 (PR #435).
7. **DONE** `omp attach --observe` remains the pipeable owner-socket observer. The retained
   R2 viewer is also implemented in `crates/chat/src/overlays/hub.rs`: `/hub` Enter runs
   `transcript <id>`, `TranscriptViewer` consumes `AgentView` snapshots and ordered patch
   streams, renders the detached transcript, supports steering input and scrolling, and closes
   cleanly. The `omp-chat` package verification passed 738 tests and doctests.
8. **DONE** Proofs: unit tests per crate, the driver integration test over a real UDS
   (`crates/driver/tests/session_attach_uds.rs`), the command's own socket path
   (`crates/app/tests/attach_client.rs`), and P14 (`crates/e2e/tests/p14_session_attach.rs`):
   a production kernel, the owner socket with its interceptor, and the generated client prove
   convergence from a snapshot plus ordered events, detach and cursor resume replayed from the
   ring, a foreign epoch answered with a resync and a fresh snapshot, a restarted host minting a
   new epoch, redaction of every host-local property under the shipped table, and the refusal of
   all four mutating verbs. Gated in `just e2e` and in CI's Linux proofs job.
9. **DONE** An idle interactive kernel never answered `Up::Subscribe`, because a kernel drains its
   mailbox only inside a turn. `KernelHandle` now carries `idle_subscribe`, the channel whoever
   holds the session between turns serves, and the wire asks the mailbox and that seam together,
   taking whichever answers. The chat controller serves it from its idle loop; `SessionHome`
   installs the seam and patches the handle `compose_kernel` registered before the controller
   existed. Smoked against a real `omp chat` sitting idle: welcome, a two-chunk snapshot, and a
   heartbeat.

   Found while fixing it, and left as it stands. The arm must be **first** in the controller's
   `biased` idle `select!`: placed third, behind `command_rx` and the pending-input gate, the host
   did not answer an attach within the 5 s subscribe budget, reproducibly, and moving it to the
   front fixed it just as reproducibly (three failures, three successes, same build otherwise).
   The mechanism is unexplained: `command_rx` carries only discrete user commands, so an idle chat
   should not feed it. Open question, with a real defect found next to it — the `REVIVED_POLL` and
   `GOAL_CONTINUATION_DELAY` arms construct their `tokio::time::sleep` inside the loop, so every
   iteration discards the timer and starts a new one, and neither fires while any other arm fires
   more often than 500 ms / 800 ms. The subscription arm is one such arm, so an observer
   re-attaching in a tight loop postpones goal continuation indefinitely. Both belong to the
   controller's loop, not to R1.

Deferred by the same decision: WebSocket, JSON, and gRPC-Web surfaces, which are adapters over
this projection and need a named consumer first.

Found while building units 2 and 4, and not part of R1:

- **RESOLVED by PR #449** `Snapshot::from_bytes` now validates through borrowed
  `RawValue` tokens and retains the validated source bytes, so structured and scalar lexical JSON
  survives the fixed-point round trip while typed scalar `Value` compatibility remains unchanged.
- **OPEN** `PropPolicy::redacts_content` receives only a `Tag`, so a value-conditioned rule cannot
  be expressed. The advisor transcript card is identifiable in the DOM, as a `notice` carrying
  `kind = advisor`, but classifying it needs a wider signature; `notice/data` alone would swallow
  unrelated notices. ADR 0039 section A4 names advisor subtrees as host-local, so R1 ships with
  that card unredacted.

## Superseded task snapshots

- `adr-task-list` is **HISTORICAL**. It is a session snapshot, predates ADR
  0045/0048 reconciliation, and contains both completed work and stale
  blockers. It is replaced by this file for active sequencing.
- `adr0045-slice-b` is retained as evidence and remains valid as an
  investigation.
- `adr0048-slice-c` is retained as evidence but is superseded for status by
  this file.
- `pr-deferred-audit` is retained as a dated audit, not an active queue.

## Validation rule for future sessions

Before starting an ADR task, verify the current branch, merged PR state, the
ADR implementation gate, and the cited code/test evidence. When a task lands,
update the affected ADR's `Status in omp`, `STATUS.md`, and this ledger in the
same change. Do not copy completed items into another task list.