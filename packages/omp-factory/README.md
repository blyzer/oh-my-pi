# omp-factory

Deterministic software-factory controller for Oh My Pi. An external extension (L1) that owns workflow truth — readiness, attempts, gates, acceptance, durable replay, serialized integration — while OMP owns agent execution.

Status: early scaffold. The `/factory` command runs one managed builder through the public `runSubprocess` path and accepts the candidate only through the deterministic driver (exit check → write-scope check against the **captured** change-set → file assertions → gate command → review → ledger).

## Layout

```text
packages/omp-factory/
  src/extension.ts   /factory command (thin: spawn + report)
  src/workflow.ts    acceptance owner (attempt loop + ledger)
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

Open gaps, unproven rather than absent:

- The driver runs one phase. `src/dag.ts` and `VersionStore` are tested in
  isolation; no multi-phase run drives them yet.
- No `json_parses` assertion; malformed JSON currently fails closed as an
  unknown assertion type instead of reporting a parse position.
- No fusion panels, `onReject` revision routing, rewind targets, or budgets
  spanning revisions.
- No end-to-end run of the old workflows against the new driver — that needs
  live models, so cutover remains unjustified.
