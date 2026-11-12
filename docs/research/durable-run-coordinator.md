# Durable run coordinator: options, recommendation and plan

Date: 2026-11-12. Status: research, not a decision record. The decisions it feeds are ADR 0051 (durable ADW), 0052 (executor contract), 0053 (independent verdicts) and the Part D amendment of ADR 0039 (workstreams, multi-repository work, the Issue-to-PR lifecycle).

Evidence labels: **[code]** read in the repository at `origin/omp2` 7772f1c94d, **[doc]** read in a repository document, **[ext]** read in an external repository README, **[general]** general knowledge not checked in this session.

## 1. The question

Make long software-factory work survive a crash, a disconnect and a restart without duplicating external effects, and without a second workflow engine. Specifically: what component owns authoritative run state, how does it recover, and what is the smallest first step.

## 2. What already exists

- ADW domain: `omp-adw` is a pure state machine with attempts, accepted input versions, retries, corrections and posture requirements; its `Run` is not serializable and nothing persists it [code: `crates/adw/src/run.rs:173`, `lib.rs:3-11`].
- ADW driver: dispatches one phase at a time; `Step::Wait` ends the run as halted [code: `crates/driver/src/adw/mod.rs`, line 270].
- Journal: a flat `.oms` file, one entry per committed blank-line-terminated record, single-line JSON data, ULID ids, branch parents, an `IdempotencyKey` committed in the same entry as its message, closed kind set in revision 1 [code: `crates/journal/src/`].
- Fleet coordinator design: ADR 0039 Part B specifies a coordinator that owns a long-lived fleet session, workers, leases, generation fencing and a journal-derived ledger, and states that one host is a valid one-worker factory [doc: ADR 0039 B2, B3, B10]. It is partly implemented (R2 and S0 done; the fleet itself is not) [doc: ADR 0039 "Status in omp"].
- Workpool: the agent runtime already persists pool items, workers and batches into the session journal [doc: `docs/research/adw-on-eval-vs-omp-adw.md`, citing `workpool_scheduler.rs`].
- Workflow extension: `extensions/workflows` runs declarative subagent DAGs; its README says its resume state is a rebuildable cache, not authoritative journal state [per the owner-supplied review; the README was not re-read here].
- Memory: `omp-memory` (Mnemopi) has SQLite rows as authority and rebuildable indexes [code: `crates/memory/src/lib.rs`].

## 3. Requirements

R1 crash recovery with an explicit "uncertain" state; R2 no duplicated external effect (PR creation, push, merge, deploy); R3 audit and provenance (who, which model, which evidence); R4 works on one host with no service; R5 extension-friendly stores and effect reconcilers; R6 deterministic replay; R7 low write amplification; R8 stale-owner fencing; R9 hierarchical budgets and permission narrowing.

## 4. Options

| Option | Recovery | Effects | Audit | One host, no service | Extensible | Verdict |
|---|---|---|---|---|---|---|
| **A. Event-sourced run journal over `omp-journal` with deterministic replay into `omp-adw::Run`** | Replay; uncertain phases classified | Idempotency keys plus reconcilers | Complete, append-only | Yes | Store trait, reconcilers | **Recommended** |
| B. SQLite or Mnemopi state tables | Read last row | Same keys possible | Weaker: current state, not history, unless an event table is added (then it is A) | Yes | Moderate | Offer as an alternative store of A, not a different design |
| C. External durable-workflow engine (Temporal- or DBOS-style) [general] | Strong, mature | Strong | Strong | **No** (a service), conflicts with "one host is a valid factory" | Via the engine's SDK | Reject for v1; revisit only for a hosted fleet |
| D. Heap snapshot of a running script (dquickts: pause mid-loop, serialize the whole heap, resume in another process) [ext] | Resume exactly where paused | Does not solve external effects by itself | Opaque: state is a heap, not named facts | Yes | High for script workflows | Useful as a *workflow runtime* for extension scripts; not the authority for run state |
| E. In memory (today) | None | None | None | Yes | n/a | Keep only as an explicit `--ephemeral` store |

Why A: the engine is already deterministic and replayable by design ("every transition is a total function of recorded facts, so replaying the same transitions reconstructs the same state" [code: `crates/adw/src/lib.rs`]); the journal already gives atomic commit, ULID order and idempotency keys; and it needs no service. The cost is naming the facts and writing reconcilers.

## 5. The hard part: external effects

Exactly-once is not achievable in general. The recommended contract is: write intent before the effect, derive an idempotency key from (run, phase, attempt, effect), and on recovery ask the owning system whether the effect happened (a reconciler per effect kind: "does a PR for branch X exist", "is commit Y on the remote"). If no reconciler can decide, the phase becomes `NeedsHuman`; it is never marked accepted and never blindly retried. Forge and CI extensions ship the reconcilers for the effects they perform.

## 6. Roles, so they are not collapsed [doc: owner-supplied architecture review, 2026-11-12]

Foreman (LLM: decides approach and specialist), orchestrator (ADW: dependencies, sequencing, retries), coordinator (deterministic Rust: authoritative state, leases, recovery, enforcement), scheduler (when and where work runs), worker (native OMP2 or delegated harness), judge (deterministic gates plus an independent model verdict, ADR 0053). Rule: the Foreman proposes, the coordinator enforces. Nothing an LLM says can change a lease, a permission or a recorded verdict.

## 7. Recommended plan

| Step | Deliverable | Acceptance (observable) | Roadmap |
|---|---|---|---|
| S0 | Wire one production delegated path; prove it on real Codex (ADR 0052 D6) | stream, cancel, journal kept, permission denied stays denied | P0 |
| S1 | `RunStore` trait, run journal (default), in-memory store; `Run` replay constructor; `omp adw resume` | kill at every boundary, recovered state equals uninterrupted state; digest mismatch refuses | P1 |
| S2 | Effect keys and the first reconciler (PR creation through `omp-forge`) | killed after PR exists, before settle: no second PR | P1 |
| S3 | Router and assignment layer; native OMP2 as an executor (ADR 0052) | same workload on two executors, no workflow edit | P2 |
| S4 | Concurrent driver over `Step::Wait`; isolated workspaces; integration conflict detection (ADR 0007 amendment) | parallel phases produce isolated changes integrated without silent overwrite | P3 |
| S5 | Judge role and structured verdict (ADR 0053); workstream and work-item ledger (ADR 0039 Part D) | failed gate overrides an Accept; a workstream survives restart | P4 |
| S6 | Factory API over the existing RPC and SDK; dashboards | a second client reconnects, observes, approves, cancels and resumes | P5 |

First move: S1 with the journal store, because it is the smallest change that makes the rest possible. It needs the ADR 0051 O1 decision first, and both options change `omp-journal` (its kind set is closed on encode and open), so S1 starts with the journal owner, not with ADW.

## 8. Open items

- Read OMP1's metaharness and Vibemon adapter (`packages/metaharness/src/tb/vmon.ts`) before designing a harness-as-host adapter. Vibemon is a KVM/HVF microVM platform that speaks the E2B API, forks sandboxes copy-on-write from a template snapshot and can pause to disk and resume on another node [ext: stencil-hq/vibemon README]; that matches the isolated-workspace need of S4 and the leased-worker need of ADR 0039, but fit is unverified.
- Decide whether `extensions/workflows` migrates onto the run store or stays a cache (it must not become a third engine).
- OVNFS is stencil-hq/ovnfs, copy-on-write agent checkouts with diff and three-way apply (README read, source not read); evaluate it as the isolated-workspace backend for S4 (see the ADR 0007 amendment).
- Measure write amplification of a dispatch and settle append on a real workflow. ADR 0051 D3 and D4 fix per-dispatch and per-settle appends; a measurement that shows a problem would reopen that granularity.
