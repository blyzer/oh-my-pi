# omp-factory

Deterministic software-factory controller for Oh My Pi. An external extension (L1) that owns workflow truth — readiness, attempts, gates, acceptance, durable replay, serialized integration — while OMP owns agent execution.

Status: early scaffold. The `/factory` command runs one managed builder through the public `runSubprocess` path and accepts the candidate only through the deterministic driver (exit check → write-scope check against the **captured** change-set → file assertions → gate command → review → ledger).

## Layout

```text
packages/omp-factory/
  src/extension.ts   /factory command (thin: spawn + report)
  src/workflow.ts    acceptance owner for one phase (attempt loop + ledger)
  src/graph.ts       multi-phase runner over accepted versions
  src/isolation.ts   per-writer workspace sandboxes
  src/envelope.ts    builder envelope contract
  src/loop.ts        bounded correction loop
  src/scope.ts       declared-scope verification
  src/capture.ts     baseline/delta capture via @oh-my-pi/pi-coding-agent
  src/gates.ts       file assertions + gate commands
  src/ledger.ts      events.jsonl truth + checkpoint projection
  src/integrate.ts   serialized reconcile journal
  src/dag.ts         accepted-version readiness + waves
  src/review.ts      gate-dominant review combination
  src/decisions.ts   nonce-bound human decisions
  src/telemetry.ts   verified-outcome metrics
  src/route.ts       capability routing
```

Change-set capture reuses core `captureBaseline`/`captureDeltaPatch`/`patchTouchedFiles`
(public barrel of `@oh-my-pi/pi-coding-agent`); no forked worktree logic lives here.

## Equivalence with the frozen ADW oracle

The prototype stays the behavioral reference. It is frozen at the local tag
`adw-prototype-reference`; its suite (`cargo test -p pi-tasks`, 74 tests) is the
oracle. `src/equivalence.test.ts` names each oracle behavior this driver
reproduces — internal traces differ by design, acceptance semantics do not.

Covered families: gate failure returning the named violation to the next
attempt, non-vacuous artifact claims, existence vs emptiness vs structure,
self-reported failure rejected over green gates, unparseable output as a
correction rather than a crash, finite attempt budgets, graph-ordered
scheduling, refused cycles, superseded-but-stable accepted versions, blocked
descendants of a failed producer, and in-flight replay that never invents
acceptance. Crash safety is proved separately by `scripts/crash-drill.ts`,
which SIGKILLs a real applier mid-apply over 8000 files and reconciles to a
byte-exact committed tree.

### Live evidence

Runs against real models in scratch git repos:

| Run | Outcome |
| --- | --- |
| Builder writes and declares `src/greeting.txt` | accepted, version 1, file present |
| Builder answers in prose, no JSON | rejected on `envelope violation` |
| `scripts/live-wi0024.ts` — contradictory `file_contains` + `file_not_contains` | rejected on the contradiction, root untouched, no journal |
| `scripts/live-workflow.ts` with the frozen `ship.yml` (only its `command` retargeted) | `plan → build → verify` all accepted, `PLAN.md` and the edit landed through the journal |
| `scripts/live-workflow.ts` with the frozen `review.yml`, unmodified | two read-only seats in parallel, one fuser wrote `REVIEW.md` naming both panellists |

The WI-0024 drill runs the builder in a sandbox on purpose: without one, a
rejected builder's writes stay in the shared tree, so "nothing landed" would
only be true of the integration journal.

### Run-for-run against the frozen engine

The frozen ADW engine (tag `adw-prototype-reference`, driven through `runAdw`
directly — `/adw` is a TUI slash command that print mode never dispatches)
and this driver ran the same `fix.yml` on byte-identical scratch repos:

| Workflow | Frozen engine | This driver |
| --- | --- | --- |
| `fix.yml`, green gate | `build` → `verify`, one attempt each, accepted, `subtract` landed | same order, same attempt counts, accepted, same file |
| `fix.yml`, `command: exit 1` | terminal refusal, nothing accepted | terminal refusal, `verify` blocked without an accepted producer |

The red run is where the comparison earned its cost: it found two defects no
deterministic test caught. A code phase with `onFail: correct` was re-running
its own unchanged command before correcting, spending the budget on an input
that could not change; and every spawn on a revision reused the first spawn's
registry id, so a corrector died on a duplicate id and the failure was
misreported as a builder exit. Both are fixed here.

A third run made the red gate adversarial: the same `exit 1` gate, and a
builder that reached for the workflow file to make the gate pass. Both
engines refuse it, and the refusals now line up on everything that decides
acceptance:

| Observation | Frozen engine | This driver |
| --- | --- | --- |
| Gate re-run before correcting | no | no |
| Correction carries the gate's failure | yes | yes — `Phase "verify" rejected …: gate exit 1` |
| Write to `.omp/adw/**` | refused | refused — `scope violation: .omp/adw/fix.yml (protected)` |
| Second attempt after the protected write | refused | refused — the entry-state baseline still sees it |
| Terminal state | nothing accepted | nothing accepted, `verify` blocked |
| Rejected write left in the tree | reverted | reverted — `TaskWriteGuard`, wired since |

That last row was a real divergence when the run was made, and closing it is
what `src/write-guard.ts` exists for: core's `TaskWriteGuard` snapshots the
tree at each attempt boundary and restores what the attempt wrote outside
its scope. `src/rollback.test.ts` pins both halves — guarded, the protected
write is undone and the clean retry stands; unguarded, the entry-state
baseline still refuses it and the file stays on disk.

`src/workflow-config.ts` loads the prototype's own `.omp/adw/*.yml` format —
`dependsOn` (including the implicit declaration edge), `inputs` as transitive
dependencies, `writes` as scope, `protected`, `gates`, `onFail: correct`
routed to the nearest agent ancestor, `onReject`, and `kind: fusion` (a
read-only panel in parallel, one fuser writing; a retry re-runs the fuser
only, since the panel was not what got rejected). All four frozen examples
load; unknown gates, artifact gates on a code phase, a panel under two seats
and a missing fuser are refused at load time rather than dropped.

Concurrency: readers in a wave overlap, and writers overlap too **when an
isolation provider gives each one its own tree** — accepted diffs then land on
the shared root one at a time through the integration lock. Without a
provider, writers take a single token on the shared workspace, which is the
only other safe option. `src/isolation.ts` ships `copyIsolation()`, a portable
recursive-copy sandbox. A coherent rejection with
`onReject: { to, maxRevisions }` re-opens the target's dependent closure and
re-runs it against the new accepted version, bounded by the declared budget.

Delivered-tree verification (I14) runs between APPLIED and COMMITTED: the
phase's own gate is re-run against the landing root whenever that differs
from the workspace the candidate was built in, and a failure reverts the
landing from the journal's `before` content rather than leaving an
unverifiable tree behind. `src/delivered.test.ts` pins it with two writers
whose change-sets each pass alone in their own sandbox and break only once
both are on the shared root -- neutralising the check accepts that run.

Human gates (I11) are a durable pause, not a prompt. A phase marked
`requiresHuman` does not dispatch until an injected gate answers; a pending
answer halts the run with `awaiting-human` -- distinct from `failed`, since
an operator reading "failed" would think their work died rather than that it
is waiting for them -- and carries the nonce a later process resolves by.
Decisions bind workflow+phase+attempt and are consumed exactly once through
an exclusive claim, so a stale approval cannot authorize a later attempt and
two racing resolvers cannot both spend one. `src/human.test.ts` pins it;
neutralising the gate accepts a run nobody authorized.

FactoryBench (`src/bench.ts`) is a named scenario registry, not a second
runner: `bench.test.ts` drives it through Bun like everything else. Six
scenarios are registered today -- FB-FAILCLOSED-001 (canonical, held out of
any tuning loop), FB-SCOPE-001, FB-DAG-001, FB-ISOLATION-001,
FB-PROVENANCE-001, FB-RESOURCE-001 -- and each drives the real engine rather
than a mock.
The registry's point is `uncoveredFamilies()`: it names what is NOT proven.
Today that is FB-GATE, FB-CORRECTION, FB-RECOVERY, FB-INTEGRATION,
FB-HANDOFF and FB-HUMAN. Those behaviours are covered by
ordinary tests elsewhere in the suite; what they lack is a named, auditable
scenario, and a benchmark that reported only its passes would describe its
author's attention rather than the system's safety.

Open gaps, unproven rather than absent:

- `copyIsolation` copies the tree. OMP core owns copy-on-write backends
  (`ensureIsolation`), which the public barrel does not expose; in this
  checkout the published `@oh-my-pi/pi-natives` resolves ahead of the built
  workspace addon, so that path is unverified here.
- No rewind targets beyond the single `onReject` route.
- Rolling back a rejected attempt costs a whole-tree scan, so the guard is
  injected rather than assumed. The scan deliberately covers ignored files
  (an ignore rule must not hide a protected path): a clean 7k-file worktree
  costs ~8s to snapshot and ~1s to settle, the same checkout carrying
  `target/` and `node_modules` costs ~220s and 32GB of stored objects. A
  caller that cannot pay that isolates instead; one that does neither must
  pass `allowUnguardedWrites` and say so.
- The run-for-run comparison covers `fix.yml` green and red only. The
  fusion, concurrent and resume families are pinned by the oracle suite and
  by live runs here, but not yet by a side-by-side against the old engine.

`/factory` runs through `runGraph` with one writer phase, so the command and
the engine share a single acceptance path; a multi-phase workflow is a longer
`phases` list, never a second implementation.
