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
- **OPEN** P8 baselines for workloads W1–W9 in attached and embedded modes.
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

- **OPEN** B4. `validate_worker_frame` is used for the Ping probe, but the
  normal worker decode path does not call it, and `validate_host_frame` has no
  production caller. Call the validators before prost decode or delete the
  dead validators and their tests.
- **PARTIAL** B5. The golden journal harness exists with two fixture pairs.
  Decide whether the ADR's “each kind or field addition” wording requires
  fixtures for `compaction`, `tool.update`, and `patch`, then add the missing
  fixtures if required.
- **BLOCKED** owner acceptance. Confirm the Buf ignore list, choose the B1
  lenient-decode versus revision-bump variant, and run the required local Buf
  breaking proof.

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
