# Advisory and correction ledger — ADR 0039 R2 / ADR 0041 integration

Session 2026-11-03/04, base `omp2`. Every advisory raised against the coordinator,
what it changed, and where the fix lives. Written because these existed only in the
transcript and would not survive the session.

Legend: **A** = reviewer advisory, **I** = implementer finding (agent correcting the
coordinator), **C** = coordinator self-correction. **R** = independent review.

**Status discipline.** A row closes only when its code and targeted tests have landed
and PASSED together. Rows carrying **PENDING** hold an instruction that has been given
and recorded, which is NOT executable evidence: dispatching a brief does not close an
advisory, a green test on the verb branch does. This note exists because the first
revision of this file labelled instructed work as "outcome", which is the same
infer-from-intent error the ledger's closing section describes.

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
| 25 | A | **CLOSED** | A schema comment states intent, not behaviour; prove `accepted`/`seq` by test | Behavioural proof required on the approval path: assert by value, verify key-in-journal ordering before the first receipt returns, drop/reopen/retry | feat/r2-approve |
| 26 | A | **CLOSED** | Generic `Session::commit` ordering does not prove the APPROVAL path journals its key | **Ledger item 2 REOPENED**; closes only when the approval-specific test passes including reopen | feat/r2-approve |
| 27 | A | **PENDING** | The new scope must be an explicit ADR amendment, not only a code variant | A4 table row, owner-grant policy, reasoning, dated Amendment section, plus VARIANTS / welcome-frame / granted-vs-refused tests | feat/r2-approve |
| 28 | A | "A4 rows" vs "R2 status line" is not independent at merge time: one physical file, two worktrees | **VerbApproveD is sole editor** of ADR 0039 this wave; VerbAnswerUiE sends finished prose plus anchor instead of editing | both slices |
| 29 | A | `/tmp` ledger is not durable; land it in the repo | Landed as `docs/audits/adr-0039-r2-advisory-ledger.md`, **PR #474, merged** | this file |
| 30 | R | Independent review: the disposition refusal rested on an unverified premise, untested on the REPLAY path | Premise then verified in code (`CallControl` does not degrade; cross-arm key reuse conflicts), so the refusal stands **on evidence, not on the original argument** | feat/r2-submit |
| 31 | I | **PENDING** | `entry_id`: a client that loses its first receipt cannot re-derive its entry (no key in the DOM, no lookup-by-key RPC, `seq = 0` on replay) | **Field ADDED** on a new number, after being refused twice on two different arguments | feat/r2-submit |
| 32 | A | **PENDING** | Pin the no-degradation invariant with executable tests, not a comment | Cross-arm key reuse in BOTH directions plus same-arm reopen replay, asserted by variant and by value | feat/r2-submit |

## Closed by passing tests (PR #477)

Rows 25 and 26 are closed by `a_remote_approval_journals_its_key_before_its_outcome_and_replays_after_reopen`
on `feat/r2-approve`. It runs the real `CallControl::handle` with Remote delivery, asserts
`Applied{entry: head, seq: 3}` **by value**, and a waker **reads the journal from disk inside
`reply.send`**, so the decision entry and its key are proven on disk BEFORE the outcome is
sent. It then drops the session, reopens, retries the same key and asserts `Duplicate{entry}`
with no new entry. An in-memory index answers `NotPending` there.

`just check` exit 0; `0 failed` across omp-agent 351, omp-app 462, omp-driver 601, omp-e2e 68
(1 skipped), omp-journal 58 (3 skipped), omp-proto 33, omp-serve 44, omp-session 71 (1 skipped).

## Open: owner decisions and unproven work

1. **Ledger item 2 / #9 / #26** — the approval path must journal its idempotency key
   **durably**, with a test that applies, drops, reopens from the journal, retries the
   same key, and asserts `accepted` and `seq` by value. Until that test passes, the
   indeterminate-retry contract is unproven. An in-memory index passes everything else.
2. **`UiAnswer` encoding** — still owner-undecided. Shipping provisionally behind ONE
   private typed decoder (no `Value` traversal, no `json!`), recorded as provisional,
   alternatives named: a proto `oneof`, a typed sub-message, or the provisional JSON.
3. **claude-bridge OAuth** — expired. The installed binary has no `auth` subcommand
   (both `omp auth login --help` and `omp auth --help` fall through to `launch` help),
   and `claude-bridge` appears nowhere in `crates/`, so the binary and `config.yml`
   disagree about which providers exist. `omp auth login <provider>` exists only in
   `omp2` source (`crates/app/src/auth_cli.rs`).

## Decided since the first revision (DECIDED, not shipped)

None of the following has landed. Each is a decision with its reasoning recorded; the
code, ADR amendment and tests are pending on the verb branches, and the five PENDING
rows above are exactly that proof gate. "Merged" in this file means a PR on `omp2`;
everything else is decided or in flight.

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