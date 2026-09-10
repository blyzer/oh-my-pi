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
