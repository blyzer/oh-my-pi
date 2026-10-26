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

## ADR 0045 — envd responsibility boundaries

ADR status: **proposed; owner review required**. Its implementation gate says
that no implementation is authorized before acceptance.

- **BLOCKED** owner acceptance and decision-item-7 answers. Required answers
  include reconnect policy, admission placement, approval/reflection relay
  treatment, MCP process locus, daemon sharing, and the future of schedules.
- **DONE** W8 daemon-kill evidence with an in-flight invocation:
  `w8_probe_remote_close_leaves_stream_pending_then_drops_local_stream`
  (`crates/env/src/partition.rs`) and
  `w8_sigkill_daemon_leaves_remote_pending_then_drops_local`
  (`crates/e2e/tests/p12_daemon_approvals.rs`, macOS, real daemon). Both pin
  the gap; the containment fix inverts them into acceptance tests.
- **OPEN** P8 baselines for workloads W1–W9 in attached and embedded modes. The current workflow run [40246613996](https://github.com/blyzer/oh-my-pi/actions/runs/40246613996) records only the synthetic `omp-e2e` recorder on self-hosted macOS arm64; it does not satisfy the W1–W9 attached/embedded baseline requirement.
- **OPEN after acceptance** containment fixes in `crates/env/src/partition.rs`:
  typed terminal errors for pending remote routes and local-backend survival
  after remote closure.
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

- **STILL OPEN** ADR 0039 phase R1 (read-only attach over the owner UDS). PR
  #342, titled "ADR 0039 A5", reports merged (`3c2695eb8e`), but it targeted
  `feat/session-host`, not `omp2`. Only the base commits under that merge
  (`de39d50718` and earlier) reached `omp2`, and `origin/feat/session-host`
  still points at the merge itself. Two of its nine paths never arrived:
  `crates/app/src/attach_cmd.rs` (the CLI command) and
  `crates/driver/src/session_wire.rs` (the driver-side gRPC projection). The
  attach wiring the merge added to `crates/app/src/cli.rs` and
  `crates/app/src/lib.rs` is absent with them. The top-level `Command` enum
  has no `Attach` variant, and the `JobsCommand::Attach` in `cli.rs` is the
  unrelated `omp jobs attach`. The merge's other content is on `omp2` through
  separate commits (`session.proto` and `omp_proto::session`,
  `crates/rpc/src/device_auth.rs` and its `ring` dependency, and the
  `device_load_or_generate` block in `crates/app/src/daemon.rs`). Both lost
  files live in `561583c686`, the merge's second parent, reachable from
  `origin/feat/session-host` or after `git fetch origin refs/pull/342/head`.
  `attach_cmd::run` parks on `std::future::pending()`, so it is not restored
  on its own. It lands with the real R1 gRPC projection. ADR 0039's `Status
  in omp` and its `STATUS.md` row are corrected in the same change.
- **NO ACTION** Codex `auth_required` object-form classification. PR #366
  classified a signed-out `-32000` inside `codex_app_server`; `286ff0945b`
  reverted it and moved the rule to `acp_client::wire::is_missing_login`,
  because that shape arrives through the ACP client. The Codex adapter is
  correct as it stands. Do not re-apply #366.

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
7. **DONE** `omp attach --observe`: connects to the owner UDS, folds frames into a local `Dom`,
   and renders it through the production transcript projection as it converges. `omp chat` hosts
   the socket, first process wins. The historical `std::future::pending()` stub is not restored.
   Deviation from the ADR sketch: the retained chat-actor viewer is not the R1 presentation. The
   chat host takes a controller, services, and an editor that an observer has nothing to put
   behind; the transcript projection is the read-only path that exists today. R2 brings the
   observer input, and the retained viewer lands with it.
8. **PARTLY DONE** Proofs: unit tests per crate and the driver integration test over a real UDS
   (`crates/driver/tests/session_attach_uds.rs`, `crates/app/tests/attach_client.rs`) are in.
   **OPEN**: the P-proof covering convergence, detach and cursor resume, eviction and `Resync`,
   host restart and a new epoch, redaction, and the refusal of every mutation path, over a real
   `omp chat` host on a PTY.
9. **OPEN, blocks R1 from being usable** An idle interactive kernel never answers `Up::Subscribe`.
   `CallControl::handle` (`crates/agent/src/dispatch.rs`) serves that message, but the kernel only
   drains its mailbox inside a turn (`Kernel::drain_mailbox`), and between turns the chat
   controller owns the session. Smoked against a real `omp chat` on 2026-10-26: `omp attach` lists
   the live session from the socket, and attaching to it answers `UNAVAILABLE` until a turn runs.
   Child sessions are unaffected — the revive loop drains continuously
   (`crates/driver/src/subagent/revive.rs`). The fix is a host-side seam that serves a subscription
   while idle, not a client retry: an observer must be able to attach to a session that is doing
   nothing, which is the case it exists for.

Deferred by the same decision: WebSocket, JSON, and gRPC-Web surfaces, which are adapters over
this projection and need a named consumer first.

Found while building units 2 and 4, and not part of R1:

- **OPEN** `Snapshot::from_bytes` is not a fixed point. `Value`'s `Deserialize`
  (`crates/dom/src/node.rs`) routes a map through `serde_json::Value` and `to_raw_value`, and
  `Snapshot::build` re-serializes it, so a non-compact `Value::Json` property loses its whitespace
  and number form on the first hop and is stable after. `Snapshot` equality is a byte compare, so
  two structurally identical trees can compare unequal. An attached observer folding events into
  its own replica therefore cannot prove convergence by comparing canonical bytes with the host
  until this is fixed. Fixing it means capturing the raw value directly in `Value::deserialize`
  rather than round-tripping through `serde_json::Value`.
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
