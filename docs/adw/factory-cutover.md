# Cutover: `omp-factory` dissolved

The package is gone. What it proved is now proven against the engine that
ships, which is the only place a safety property is worth having.

## What was ported, and what porting turned out to mean

| Capability | Expected | Actual |
| --- | --- | --- |
| `file_contains` / `file_not_contains` | port ~100 lines of gate | new `FileContains` in `pi-tasks/src/gate.rs`; the native gates genuinely had no content assertion, so WI-0024 was not expressible against the shipped engine at all |
| Integration revert | port a 172-line content journal | three lines. `TaskWriteGuard`'s `begin()` boundary survives a settle, so a second settle denying every path undoes an apply exactly — measured by probe, not assumed |
| Delivered-tree verification | port `verifyDelivered` | same edit as the revert: `integrateAccepted` re-runs the downstream code phase against the landing root between APPLIED and COMMITTED |
| Human pause | port `decisions.ts` + a `pending-human` state | exposing `PhaseKind::Engineer`, which the engine already had and the ADW schema never surfaced. No durable decision store needed |
| FactoryBench | move the registry | registry moved as-is; scenarios rewritten against `TaskRun`, which found a real semantic difference — see below |

Four of five were wiring, not code. The primitive existed and nothing called
it. That is the finding worth keeping from this exercise: in this tree,
*writing a mechanism* and *connecting it* are separate commits, and the
second one gets forgotten.

Five capabilities were found written-but-unreachable during this work:
`hostCapacity`, `decisions.ts`, `classifyFailure`, `route.ts`,
`telemetry.ts`. The first three are now wired; the last two were deleted
with the package, discovered by audit rather than by anyone missing them.

## What the rewrite corrected

`runGraph` blocked a failed producer's descendants and let unrelated
branches finish. `TaskRun` aborts the whole run when a phase exhausts its
attempts. FB-DAG-001 asserted the former — my claim, not the engine's — and
now asserts what the shipped engine does. A benchmark aimed at a parallel
implementation measures the wrong system.

## FactoryBench today

Eight scenarios, all driving `TaskRun` or the native guard:

`FB-FAILCLOSED-001` (canonical, held out) · `FB-GATE-001` ·
`FB-CORRECTION-001` · `FB-DAG-001` · `FB-PROVENANCE-001` · `FB-HUMAN-001` ·
`FB-RESOURCE-001` · `FB-RECOVERY-001`

Uncovered, and reported by `uncoveredFamilies()` rather than omitted:
`FB-SCOPE`, `FB-ISOLATION`, `FB-INTEGRATION`, `FB-HANDOFF`. All four are
covered by ordinary tests; what they lack is a named scenario.

## What was deleted rather than ported

The loader (494 lines — `adw/config.ts` is stricter on eight constructs and
expresses three it could not), the DAG, the envelope parser, the review
combiner, `isolation.ts`, `scope.ts`, `capture.ts`, the `/factory` command,
and fourteen of nineteen test files whose assertions `test/adw/` already
makes against the real engine.

The crash drill did not port as a script: it exercised `omp-factory`'s own
journal, which no longer exists. Its property — an interrupted apply
restores byte-exact — is now `FB-RECOVERY-001` against `TaskWriteGuard`.

## Invariant coverage after cutover

All fifteen hold against the shipped engine. The two that were missing when
this started, I14 (delivered tree) and I11 (human gate), are the two that
took real porting; the rest were already native and simply unproven by a
named scenario.
