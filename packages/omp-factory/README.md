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

Open gaps, unproven rather than absent:

- `copyIsolation` copies the tree. OMP core owns copy-on-write backends
  (`ensureIsolation`), which the public barrel does not expose; in this
  checkout the published `@oh-my-pi/pi-natives` resolves ahead of the built
  workspace addon, so that path is unverified here.
- No rewind targets beyond the single `onReject` route, and no per-seat
  `model`/`thinking` pinning, so a panel may land two seats on one model.
- The old engine has not been run side by side with this one. The frozen
  workflows run here and the oracle suite pins the semantics, but no
  run-for-run comparison exists, so cutover remains unjustified.

`/factory` runs through `runGraph` with one writer phase, so the command and
the engine share a single acceptance path; a multi-phase workflow is a longer
`phases` list, never a second implementation.
