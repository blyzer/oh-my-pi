# Advisory and correction ledger — ADR 0039 R2 / ADR 0041 integration

Session 2026-11-03/04, base `omp2`. Every advisory raised against the coordinator,
what it changed, and where the fix lives. Written because these existed only in the
transcript and would not survive the session.

Legend: **A** = reviewer advisory, **I** = implementer finding (agent correcting the
coordinator), **C** = coordinator self-correction.

---

## Decisions and scope

| # | Src | Concern | Outcome | Lands in |
|---|-----|---------|---------|----------|
| 1 | A | Don't add an approval gate after research; owner already authorized | Gate dropped, proceeded | process |
| 2 | A | `AnswerUi` IS in A4's R2 checklist; deferring it rescopes an accepted ADR | **Deferral retracted**, record rewritten with explicit retraction | DECISION record |
| 3 | A | `AnswerUi -> control` is a NEW authorization mapping, not implementation | **Slice halted mid-flight**, mapping withdrawn, fail-closed seam instead | feat/r2-answer-ui |
| 4 | A | A4 scopes Submit/Approve PER COMMAND, not per verb | **All three briefs corrected**: prompt / interrupt / approve / approve_persist / control | all verb slices |
| 5 | C | `approve_persist` additive vs substitute | Additive, fail-closed; test: holder of approve_persist WITHOUT approve is refused | feat/r2-approve |

## Wire and protocol

| # | Src | Concern | Outcome | Lands in |
|---|-----|---------|---------|----------|
| 6 | A | Provisional `UiAnswer` encoding must still be typed serde, no `Value`/`json!` | Private typed struct, one isolated decoder; encoding marked owner-undecided | feat/r2-answer-ui |
| 7 | C | Coordinator invented `SubmitOutcome`; `CommandReceipt` already existed | `SubmitOutcome` withdrawn | feat/r2-submit |
| 8 | A | Pin `accepted` semantics before adding a proto field | **Pinned**: `session.proto:299-303` already distinguishes applied vs duplicate. **No field added**, no constructor churn | feat/r2-approve |
| 9 | A | Verify the approval journal key is PERSISTED before reporting an outcome | Open: journal-persisted keys with drop-and-reopen test | feat/r2-submit + approve |

## Concurrency and lifetime

| # | Src | Concern | Outcome | Lands in |
|---|-----|---------|---------|----------|
| 10 | A | `is_closed()` is TOCTOU; state the guarantee at the dispatch boundary | Guarantee restated there; residual race documented, not claimed away | feat/r2-approve |
| 11 | I | Residual is **not time-bounded**: `Journal::append` -> `file.sync_data()` (journal/src/lib.rs:201) | **Contract changed**: timed-out Approve = outcome UNKNOWN, never "not applied"; recover by idempotent retry | feat/r2-approve + ADR status |
| 12 | C | Claimed retry-with-same-key is safe because the receipt carries the key | **Retracted**: a field is not a dedupe mechanism; #473 keys only `MsgUser` | both slices |
| 13 | C | Approval-send helper spawned one Tokio task per queued local approval | Flagged as an unbounded task leak; park the receiver in session-owned state, or one shared reaper tied to session shutdown | feat/r2-approve |
| 14 | I | Reply oneshot is NOT dropped when a turn ends, so a late send hangs forever | Bounded wait mandatory; coordinator's "dropped reply maps to typed error" was wrong | feat/r2-submit |
| 15 | I | No path from `SessionWire` starts a turn (`TurnRequest` private to chat_control.rs) | Fail-closed idle refusal is the only honest option | feat/r2-submit |
| 16 | I | `turn_activity()` exists but `KernelHandle` lacks it (~30 literal sites) | Propagated; documented as a FAST PATH, not a guarantee; bounded wait still required | feat/r2-submit |

## Substrate gaps found after merge

| # | Src | Concern | Outcome |
|---|-----|---------|---------|
| 17 | I | `Session::user_authored` inserts a model-facing node immediately; wrong for steer/queue | Do not call it for steer/queue |
| 18 | I | **#473 (merged) keys only `MsgUser`; ADR A5 requires keys on steer/queue records** | Gap in already-merged work; one generalized journal-backed key across MsgUser + steer/queue + approval records |
| 19 | I | `ApprovalBook::decide` returns `Ok` without saying whether it changed state | Must report mutation as a typed outcome; the retry contract depends on it |
| 20 | I | `ControlContext` carries only a `Str` principal, no `RemotePrincipal`/`Origin` | `decided_by` from `principal()`; origin attribution deferred to R3b; do not fabricate |

## Process and infrastructure

| # | Src | Concern | Outcome |
|---|-----|---------|---------|
| 21 | A | Unpinned retries keep selecting exhausted Grok | Root cause: running process read `config.yml` at startup, so file edits are inert. Workaround: dispatch on `sdd-implementer` (pinned `openai-codex/gpt-6.1-terra`) |
| 22 | A | A clean worktree proves only the instant of inspection; superseded agents may still be alive | Stop orders sent; `VerbSubmitB` complied and returned findings 14-18 |
| 23 | C | Two hours of subagent work sat uncommitted when an agent died | Committed for it (`dce5bd7b44`); **commit-as-you-go now mandatory in every brief** |
| 24 | C | `local://` artifacts are unreadable by subagents | Share via absolute paths under `/tmp/omp-research/` |

---
| 25 | A | A schema comment states intent, not behaviour; prove `accepted`/`seq` by test | Behavioural proof required on the approval path: assert by value, verify key-in-journal ordering before the first receipt returns, drop/reopen/retry | feat/r2-approve |
| 26 | A | Generic `Session::commit` ordering does not prove the APPROVAL path journals its key | **Ledger item 2 REOPENED**; closes only when the approval-specific test passes including reopen | feat/r2-approve |
| 27 | A | The new scope must be an explicit ADR amendment, not only a code variant | A4 table row, owner-grant policy, reasoning, dated Amendment section, plus VARIANTS / welcome-frame / granted-vs-refused tests | feat/r2-approve |
| 28 | A | "A4 rows" vs "R2 status line" is not independent at merge time: one physical file, two worktrees | **VerbApproveD is sole editor** of ADR 0039 this wave; VerbAnswerUiE sends finished prose plus anchor instead of editing | both slices |
| 29 | A | `/tmp` ledger is not durable; land it in the repo | Landed as `docs/audits/adr-0039-r2-advisory-ledger.md`, **PR #474, merged** | this file |
| 30 | R | Independent review: the disposition refusal rested on an unverified premise, untested on the REPLAY path | Premise then verified in code (`CallControl` does not degrade; cross-arm key reuse conflicts), so the refusal stands **on evidence, not on the original argument** | feat/r2-submit |
| 31 | I | `entry_id`: a client that loses its first receipt cannot re-derive its entry (no key in the DOM, no lookup-by-key RPC, `seq = 0` on replay) | **Field ADDED** on a new number, after being refused twice on two different arguments | feat/r2-submit |
| 32 | A | Pin the no-degradation invariant with executable tests, not a comment | Cross-arm key reuse in BOTH directions plus same-arm reopen replay, asserted by variant and by value | feat/r2-submit |

## Still open

1. **Ledger item 2 / #9 / #26** — closed by `19c807d686` (#477). The approval-specific test `crates/agent/src/dispatch.rs:a_remote_approval_journals_its_key_before_its_outcome_and_replays_after_reopen` covers durable key replay.
2. **`UiAnswer` encoding** — still owner-undecided. Shipping provisionally behind ONE
   private typed decoder (no `Value` traversal, no `json!`), recorded as provisional,
   alternatives named: a proto `oneof`, a typed sub-message, or the provisional JSON.
3. **claude-bridge OAuth** — expired. The installed binary has no `auth` subcommand
   (both `omp auth login --help` and `omp auth --help` fall through to `launch` help),
   and `claude-bridge` appears nowhere in `crates/`, so the binary and `config.yml`
   disagree about which providers exist. `omp auth login <provider>` exists only in
   `omp2` source (`crates/app/src/auth_cli.rs`).

## Resolved since the first revision

- **`AnswerUi` scope**: its own scope `answer_ui`, owner socket holds it, devices do
  not (R3b). A UI request's content is chosen by the agent at runtime, so the blast
  radius of answering is not bounded at grant time; folding it into `prompt` or
  `approve` would make that grant open-ended by construction. Amended into A4.
- **Submit receipt**: `disposition` out (no degradation path, verified), `entry_id` in
  (lost-receipt recovery, verified).

## The recurring coordinator error
Inferring behaviour from the existence of a name: a field called `idempotency_key`
implied dedupe; a reply channel implied cancellation on drop; a `turn_activity`
accessor implied reachable liveness; `CommandReceipt` carrying a key implied safe
retry. Each was wrong, and each was caught by an agent that read the code.
---

## Status of rows 25-34 (2026-11-04)

A row closes only when its code and targeted tests have landed and PASSED together.
Instructing an agent does not close an advisory; a green test does.

> **CORRECTION (2026-11-04).** The revision that introduced this table was merged to
> `omp2` while every test it cites still lived on an unmerged branch, so for a period
> this file asserted proof `omp2` did not contain. That is precisely the failure the
> ledger exists to catch, committed by the ledger. Rows now read **PROVEN ON PR #n**
> until that PR merges; only then do they become CLOSED ON `omp2`.

| Row | Status | Evidence |
|-----|--------|----------|
| 25 | **CLOSED ON omp2** | `a_remote_approval_journals_its_key_before_its_outcome_and_replays_after_reopen` (`crates/agent/src/dispatch.rs`); merge `19c807d686` (#477) |
| 26 | **CLOSED ON omp2** | The same test reads the journal from disk inside `reply.send`, proving the key durable before the outcome is sent; merge `19c807d686` (#477) |
| 27 | **CLOSED ON omp2** | The ADR's dated amendment records the A4 row, owner-grant policy, and reason for the separate `AnswerUi` scope; merge `19c807d686` (#477) |
| 28 | **CLOSED ON omp2** | single-ADR-owner rule held; #477 pasted both siblings' prose verbatim, no conflict |
| 29 | **CLOSED ON omp2** | this file, merged as #474 and #475 |
| 30 | **CLOSED ON omp2** | `CallControl` does not degrade steer to queue or vice versa; cross-arm key reuse conflicts. Merge `3410f1e74f` (#478) |
| 31 | **CLOSED ON omp2** | `entry_id` is proto field 4; `session_submit` recovers a lost receipt through a held kernel turn and replays after restart. Merge `3410f1e74f` (#478) |
| 32 | **CLOSED ON omp2** | Mutation-verified: routing a steer through `prompt_txn` turns 3 tests red; routing a queue through `steering_txn` turns `queue_then_steer` red. Merge `3410f1e74f` (#478) |
| 33 | **CLOSED ON omp2** | Dismissal had no wire form, so a remote operator could answer a dialog but never decline it. `ProvisionalUiAnswer` is one private enum with `deny_unknown_fields`; `"cancelled"` becomes `AskReply::Cancelled`. `the_owner_socket_dismisses_a_waiting_ui_request` runs the production owner socket and asserts the presenter sees `Fault::cancelled()`, not the host-went-away fault. Merge `76c3097052` (#476), included on omp2 by `19c807d686` (#477) |
| 34 | **CLOSED ON omp2** | A stale bridge epoch could answer a request it never saw because the route keys by call id and lets a new call reuse a settled one. The guard now returns `FAILED_PRECONDITION` with the request still waiting, pinned by `answer_ui_from_another_bridge_epoch_never_reaches_the_route`. Merge `76c3097052` (#476), included on omp2 by `19c807d686` (#477) |

### The A5 gap #473 shipped with is closed on omp2
Steer and queue records now use the same journal-backed key index as `msg.user` and
approval records (`patch_keyed` over `mutation_keys`). One mechanism, three record kinds,
as ADR A5 requires. #473 keyed only `MsgUser`; merge `3410f1e74f` (#478) closed the gap.

### Checks
- **#477**: `just check` exit 0; `0 failed` across omp-agent 351, omp-app 462, omp-driver 601,
  omp-e2e 68 (1 skipped), omp-journal 58 (3 skipped), omp-proto 33, omp-serve 44, omp-session 71 (1 skipped).
- **#478**: `just check` exit 0; `0 failed` across journal 58, session 71, proto 33, serve 37,
  agent 352, driver 606, app 462, plus `just e2e-p14`.
- **#476**: `just check` exit 0; omp-driver 609, omp-serve 46, omp-app 462, omp-e2e 68 (1 skipped);
  clippy on omp-driver exits 0 with no warning on a changed line.

### Still open
1. **`UiAnswer` encoding** — owner-undecided, shipped provisionally behind one private typed
   decoder; alternatives named in the ADR. The `Cancelled` arm is part of that provisional
   encoding, so settling the encoding settles dismissal's wire form too (`crates/driver/src/session_wire.rs:ProvisionalUiAnswer`).
2. **AnswerUi has no idempotency index**, so a retry after success answers `NOT_FOUND` rather
   than a duplicate receipt. A deliberate asymmetry with Submit and Approve: a UI answer
   resolves a one-shot waiter rather than mutating durable state (`crates/driver/src/session_wire.rs:SessionWire::answer_ui`).
3. **Approve sent between turns times out** because the mailbox drains only inside a turn.
   Submit refuses an idle session; it can time out if the turn ends after its activity check
   (`crates/driver/src/session_wire.rs:SessionWire::approve`, `SessionWire::submit`, `submit_command`).
4. **claude-bridge OAuth** — the installed binary has no `auth` subcommand.

### Merge order
Completed: #476 merged into the approval branch as `76c3097052`. On `omp2`, #478
landed as `3410f1e74f` first; #477 then merged the approval branch, including #476,
as `19c807d686`.