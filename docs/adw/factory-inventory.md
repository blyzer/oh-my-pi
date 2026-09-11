# `omp-factory` inventory: what survives deletion

Module-by-module audit before dissolving the package. Every verdict was read
on both sides. The column that matters is the last one: not "what feature
goes away" but "what failure becomes possible again".

Totals: 5,614 lines across 20 source modules and 19 test files.

## Engine

| Module | Lines | Verdict | Equivalent | Lost on deletion |
| --- | --- | --- | --- | --- |
| `graph.ts` | 532 | **STRONGER** | `orchestrator.rs:648-665` + `adw/runner.ts:1321-1395` | Two writers whose sandboxes predate each other's landing both apply, and the second silently erases the first (landing registry, `:271-272,:356,:366-375`). Plus the human gate — see below. |
| `workflow.ts` | 299 | **STRONGER** | `orchestrator.rs:733-838` | A change-set that passes in its sandbox and breaks the shared root is committed unverified (`verifyDelivered`, `:136-152`). |
| `loop.ts` | 73 | **STRONGER** | `orchestrator.rs:1069-1221` | An out-of-memory kill spends the full correction budget re-asking an agent to fix code that never ran. Wiring gap, not a missing capability: `classifyFailure` exists at `task/admission.ts:177` and the ADW runner never calls it. |
| `dag.ts` | 67 | DUPLICATE | `orchestrator.rs` topological order + readiness | Nothing. |
| `envelope.ts` | 99 | DUPLICATE | `envelope.rs:92-121` | Nothing. Same depth-0, string-and-escape-aware scan; the Rust side additionally keeps payload passthrough. |
| `review.ts` | 34 | DUPLICATE | `orchestrator.rs:784,831` | Nothing. Its gate-failure branch is already dead: the only production caller passes `gatePassed: true` unconditionally. |
| `route.ts` | 24 | **ORPHAN** | — | Nothing. Despite the name it is model selection, not rejection routing, and its only caller is its own test. |

## Durability

| Module | Lines | Verdict | Equivalent | Lost on deletion |
| --- | --- | --- | --- | --- |
| `integrate.ts` | 172 | **UNIQUE** | `adw/integration.ts:48-83` refuses, cannot undo | A landing that applied but failed verification stays on disk with no way back. No revert path exists anywhere else in the tree. |
| `decisions.ts` | 112 | **UNIQUE** | — | A phase that must not run without human authorization runs anyway. `adw/decisions.test.ts` is retry/fusion decisions — the filename trap, checked and rejected. |
| `ledger.ts` | 254 | **STRONGER** | `trace.rs` binary trace | Integration lifecycle and `awaiting-human` become unrecordable: `trace.rs` has no `EventKind` for either. |
| `gates.ts` | 98 | SPLIT | `gate.rs` artifact gates | `file_contains` / `file_not_contains` have no native equivalent — and they are what makes WI-0024 expressible. The gate-command runner is a weaker duplicate of `adw/runner.ts`. |
| `write-guard.ts` | 67 | GLUE | `write_guard.rs` | Nothing; it is an adapter that discards `TaskGuardChange.kind`. |
| `isolation.ts` | 56 | DUPLICATE (weaker) | `ensureIsolation` COW backends | Nothing. `fs.cp` versus copy-on-write plus git detachment. |
| `scope.ts` | 47 | DUPLICATE (weaker) | `write_guard.rs` globset | Nothing. Hand-rolled globbing versus a real globset, same protection-beats-authorization rule. |
| `capture.ts` | 34 | GLUE | already-exported `coding-agent` functions | Nothing beyond nested-repo path prefixing. |

## Surface

| Module | Lines | Verdict | Equivalent | Lost on deletion |
| --- | --- | --- | --- | --- |
| `workflow-config.ts` | 494 | **DUPLICATE (weaker)** | `adw/config.ts:365` + `types.ts` | Nothing. Both reject unknown keys; the ADW loader is stricter on eight constructs and can express `schema`, `concurrency` and per-seat `prompt`, which mine cannot. Mine wins on two: `timeoutMs` refused on non-code phases, and a load-time refusal when `verdict_consistent` has no reviewer. Port those two checks. |
| `extension.ts` | 101 | DUPLICATE (weaker) | `builtin-adw.ts` | Nothing. `/adw` has list, cancel, resume and real workflows; `/factory` has one hardcoded phase. |
| `bench.ts` + `scenarios.ts` | 422 | **UNIQUE** | — | The repo's only named scenario registry, and with it the ability to ask which safety family is unproven. |
| `telemetry.ts` | 64 | **ORPHAN** | — | Cost-per-verified-acceptance has no equivalent, but nothing calls it either. |
| `scripts/crash-drill.ts` + `crash-applier.ts` | — | **UNIQUE** | — | The 8000-file SIGKILL-mid-apply drill. No equivalent in any suite. |

## Tests

Five of nineteen carry assertions that exist nowhere else — the honest
migration cost:

| File | Pins |
| --- | --- |
| `human.test.ts` | Durable pause, stale-approval refusal, exactly-once under a race |
| `delivered.test.ts` | Delivered-tree verification and its revert |
| `integrate.test.ts` | Content-based reconciliation, atomic replace, revert |
| `boundary.test.ts` | WI-0024 as a deterministic case |
| `telemetry.test.ts` | Cost accounting (of orphaned code) |

The other fourteen re-assert behaviour `packages/coding-agent/test/adw/`
already covers against the shipped engine.

## What this means

Roughly **3,900 of 5,614 lines are duplicate or weaker** — the loader, the
DAG, the envelope parser, the review combiner, isolation, scope, capture,
the extension. They target `GraphPhase`, not `TaskRun`, so nothing they
validate ever reaches the shipped engine.

Five things must be ported before deletion, or the capability leaves with
the package:

1. **Human gate** (`decisions.ts` + `graph.ts` gate + `awaiting-human`) — no equivalent anywhere.
2. **Delivered-tree verification** (`workflow.ts:136-152`) — nothing else re-checks after landing.
3. **Integration revert** (`integrate.ts`) — the ADW journal refuses but cannot undo.
4. **`file_contains` / `file_not_contains`** — without them WI-0024 is not expressible natively.
5. **FactoryBench** (`bench.ts` + `scenarios.ts` + `crash-drill.ts`).

And two wiring gaps that are cheaper to fix in place than to port:
`classifyFailure` is never called by the ADW runner, and the landing
registry has only a weaker substitute (git patch-conflict refusal at
`adw/integration.ts:69-71`).

Two orphans die unmourned: `route.ts` and `telemetry.ts` — real code, no
production caller, discovered by this audit rather than by anyone missing
them.
