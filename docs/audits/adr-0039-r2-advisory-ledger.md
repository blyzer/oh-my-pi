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

## Still open

1. **`AnswerUi` scope** — owner decision. A4 names none. Slice ships refused behind a
   fail-closed seam. Options: fold into `approve`, fold into `prompt`, or its own scope
   (coordinator recommends the last: a UI request can carry a confirmation that
   authorizes something a steerer never intended).
2. **#9 / #18** — persisted journal keys across all three record kinds, in flight.
3. **claude-bridge OAuth** — expired; the installed binary has no `auth` subcommand, and
   `claude-bridge` appears nowhere in `crates/`, so binary and config disagree about
   which providers exist.

## The recurring coordinator error
Inferring behaviour from the existence of a name: a field called `idempotency_key`
implied dedupe; a reply channel implied cancellation on drop; a `turn_activity`
accessor implied reachable liveness; `CommandReceipt` carrying a key implied safe
retry. Each was wrong, and each was caught by an agent that read the code.