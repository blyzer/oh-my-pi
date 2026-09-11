# Factory discovery: what already exists

Reuse-first discovery against the current tree, before writing Factory code.
Evidence classes are strict: **OBSERVED** means the implementation was read
and cited; **CLAIMED** means a doc or README asserts it and the code was not
read. Nothing is promoted without reading the body.

The short version: the capability the Factory needs mostly exists, in Rust,
reachable from TypeScript. The package built to provide it — `omp-factory` —
is largely a second implementation of it.

## What `crates/pi-tasks` already provides

| Capability | Class | Evidence | Actual semantics |
| --- | --- | --- | --- |
| Acceptance-gated readiness | OBSERVED | `orchestrator.rs:625-695`, `phase.rs:47-59` | A phase dispatches only when every `depends_on` is `DispatchStatus::Passed`. A completed-but-failed dependency cannot unlock it. |
| Versioned inputs + provenance | OBSERVED | `orchestrator.rs:625-695,995-1042`, `trace.rs:110-126` | Declared `inputs` resolve to the producer's accepted envelope and ordinal, traced as `InputSelected{consumer,producer,attempt,version}`; envelopes persist at `envelopes/<phase>.<version>.json`. |
| Gate beats review | OBSERVED | `orchestrator.rs:744-790,830-835` | Gate violations are assembled before the review branch; a negative review is reachable only after gates are green. A red gate cannot be overruled. |
| Bounded attempts and revisions | OBSERVED | `orchestrator.rs:74,1069-1221`, `tasks.rs:259-282,705-778` | Budget defaults to 3, clamped ≥1; each permitted revision grants exactly one extra attempt; resume refuses a substituted `maxAttempts`. |
| Durable trace and resume | OBSERVED | `trace.rs:1-63,364-486`, `orchestrator.rs:299-590` | Append-only 36-byte records + interned strings, strict v6 header, torn tails ignored. Resume replays by phase name and requeues in-flight `Running` phases at the same attempt. |
| Write-scope enforcement with rollback | OBSERVED | `write_guard.rs:631-803`, `tasks.rs:66-152,635-684` | `begin()` snapshots the actual tree including ignored files; `settle()` rolls unauthorized paths back byte-for-byte and fails closed with a preserved patch when restore is unsafe. |
| Claims vs actual diff | OBSERVED | `tasks.rs:66-152` | `diff_matches_claims` compares real git status and diff against the envelope's declared artifacts. |
| Concurrent dispatch | OBSERVED | `orchestrator.rs:625-680` | Multiple independent phases may be `Running`; the core decides state and never spawns, limits, or isolates workers. |

Deliberately **not** in `pi-tasks`, by design: patch application, worktree
lifecycle, worker execution. The core decides state; the driver acts. The
existing driver already covers those — isolated workspace per concurrent
writer, per-workspace guard, serialized integration with
`prepared`/`applying`/`integrated`/`rejected`, moved-base detection
(`adw/runner.ts:1168-1431`, `adw/integration.ts:17-100`).

## Where that leaves `packages/omp-factory`

Duplication, all OBSERVED against the citations above:

| `omp-factory` | Already provided by |
| --- | --- |
| `dag.ts`, `graph.ts` — accepted-state readiness, version selection, invalidation, revision routing | `TaskRun` + `dependsOn`/`inputs` |
| `envelope.ts`, `loop.ts`, much of `workflow.ts` | `Envelope::from_agent_text`, `Run::reject`/`rewind`/`revise` |
| `ledger.ts` — JSONL truth + checkpoint projection | binary trace + resume, with a stricter ABI |
| `write-guard.ts` | a thin wrapper over `TaskWriteGuard`; adds no enforcement |
| `graph.ts` isolated writers | `adw/runner.ts:1168-1431` |
| `integrate.ts` | `adw/integration.ts`, by a different recovery model |

Genuine deltas, and they are small:

- **Human pause.** `decisions.ts` is a durable exactly-once decision store
  with no production caller — only its own test. `pi-tasks` exposes `Run`,
  `Done`, `Wait`; `Wait` means work is in flight, not awaiting a human.
  Neither engine has a pause gate. MISSING in both.
- **Integration reconciliation by content.** `omp-factory/src/integrate.ts`
  compares file content and writes via rename; the ADW journal uses a
  different model. If the content comparison is better, it is a patch to
  `adw/integration.ts`, not grounds for a second engine.

## Missing everywhere

| Capability | Class | Searched |
| --- | --- | --- |
| Host metrics to TypeScript (RAM, pressure, load, disk, thermal) | MISSING | `packages/natives/native/index.d.ts`, `crates/pi-natives/src` |
| Capacity-aware admission (`READY ≠ ADMITTED`) | MISSING | `task/index.ts`, `task/parallel.ts`, `async/job-manager.ts`, `adw/runner.ts` — all are counters and queues |
| Resource vs semantic failure taxonomy | MISSING | provider flags classify rate-limit/transient (`packages/ai/src/error/flags.ts`) but nothing separates OOM/disk-full/worker-lost from a failed test |
| Reusable scenario harness (FactoryBench) | MISSING | direct-call tests and fixed-purpose drills exist; no way to declare a scenario and run it |
| Human approval pause | MISSING | `crates/pi-tasks/src`, `crates/pi-natives/src`, `packages/coding-agent/src/adw` |

Native metrics exist in Rust but only for their own callers: `ps.rs:967-970`
reads total RAM to render a percentage, `sort.rs:68-79` sizes its own buffer.
Neither is exported.

## Action policy already exists, and is preventive

| Capability | Class | Evidence | Preventive? |
| --- | --- | --- | --- |
| Pre-dispatch tool hook | OBSERVED | `extensibility/extensions/wrapper.ts:205-247` | Yes — `block` throws before invocation; a hook error fails closed |
| Approval policy `allow`/`deny`/`prompt` | OBSERVED | `tools/approval.ts:120-221` | Yes — a resolved `deny` throws before dispatch; re-resolved after hook rewriting |
| Bash deny patterns | OBSERVED | `tools/bash.ts:595-632` | Yes — matching `deny` returns before the shell runs |
| Post-tool hooks | OBSERVED | `hooks/tool-wrapper.ts:75-121` | No — can rewrite the reported result, cannot undo the action |

So §12's action boundary needs no new mechanism. Default approval mode is
`yolo` (`settings-schema.ts:4117-4166`) and default `bash.patterns` is empty,
so the Factory's contribution is *configuration*, not machinery.

## OMP's `workflow` is not a workflow runtime

Issue #1544 is closed COMPLETED and PR #1559 describes a first-party
`workflow` tool with sandboxed JS and structured output. The tree disagrees:
`workflow` is a **magic keyword** that injects a prompt notice steering the
model toward `task` + `eval` (`modes/workflow.ts:50`,
`agent-session.ts:6073`). No workflow runtime, no acceptance semantics. A
closed issue is not evidence.

## External candidates

All CLAIMED — READMEs and package pages, no code read. None was adopted, so
none was worth the read budget yet.

| Component | Capability claimed | Decision | Why |
| --- | --- | --- | --- |
| `pi-flows`, `pi-workflows`, `pi-agents-flow`, `@pi-stef/flow` | YAML/TS DAGs, parallel scheduling, quality gates | REJECT as runtime | Target Pi, not OMP; completion semantics with gates bolted on. `pi-tasks` already has acceptance semantics, versioned inputs and durable replay — adopting one would be a downgrade plus a second control plane. |
| `omp-dynamic-workflows` (zerx-lab) | OMP plugin: `agent()`/`parallel()`/`pipeline()`, retries, quality gates | REJECT as runtime, EXTRACT IDEA | Genuinely an OMP plugin, but script-authored fanout with no accepted-version semantics or durable truth. Its authoring ergonomics are worth studying. |
| ECC / AgentShield | Cross-harness control plane, worktree lifecycle, 102 security rules over agent config | REJECT as control plane; DEFER AgentShield | A second control plane beside OMP is exactly what §40 forbids. AgentShield addresses §13 supply-chain scanning, which nothing here covers — revisit when that becomes the active gap. |
| Historical OMP Swarm | Multi-agent orchestration, DAG | SUPERSEDED | Completion semantics, shared workspace, no executable resume. `pi-tasks` exists because of its lessons. |

## Architectural delta

**Previous assumption.** OMP lacked an acceptance-gated workflow engine, so
`omp-factory` should provide one in TypeScript.

**Evidence discovered.** `crates/pi-tasks` provides acceptance-gated
readiness, versioned provenance, gate-beats-review, bounded budgets, durable
binary trace with in-flight reset, and — through `pi-natives` — write-scope
rollback and claims-vs-diff, all reachable from TypeScript. The ADW driver
already owns execution, isolation and integration.

**New decision.** Stop treating `omp-factory` as an engine. The Factory's
remaining work is three thin layers on the existing one: resource admission,
a scenario harness, and a human pause gate. Everything else is deletion.

**Alternatives considered.** (a) Finish `omp-factory` and cut over — rejected:
it is a second state authority for capabilities that already exist, and §40
forbids that. (b) Adopt an external engine — rejected: all inspected
candidates use completion semantics. (c) Keep both behind a flag — rejected:
two acceptance authorities is the failure mode, not the mitigation.

**Invariants preserved.** I3 and I8 are stronger under `pi-tasks` than under
the TypeScript reimplementation: dependency edges require `Passed`, and
declared `inputs` pin an exact producer version. I9 is stronger too — a
binary trace with explicit interrupted-flight reset versus a JSONL projection.

**Correction worth carrying.** A dependency edge proves accepted phase state,
not a pinned version; only declared `inputs` pin one. And a phase with no
gates accepts any parseable `status: success` envelope. Deterministic gates
and inputs must be configured deliberately — the engine does not supply
suspicion by default.

## Revised work packages

Deleted as already-provided: WP-02 (execution control plane), WP-03
(DAG/workflow engine), WP-04/05/06/09 (authority contracts, builder→gate
loop, WI-0024 parity, accepted-version DAG), WP-07 (durability), WP-10
(parallel writers), WP-13 (action policy — configuration, not machinery).

Remaining, each justified by a MISSING above:

- **WP-11 Resource admission.** Export host metrics from `pi-natives`, add an
  admission gate in front of the existing semaphores, classify resource
  failures apart from semantic ones. The largest genuine gap: a guard
  snapshot on this repo costs 32 GB, and nothing today would stop four of
  them starting at once.
- **WP-15 FactoryBench.** Turn the existing drills and the equivalence suite
  into a declarative scenario harness, with WI-0024 as the canonical
  fail-closed case.
- **WP-12 Human pause.** Wire `decisions.ts`-style durable decisions into a
  real `pending-human` phase state with resume binding.
- **WP-18 Cutover.** Dissolve `omp-factory`: port the content-comparison
  integration reconciliation into `adw/integration.ts` if it proves stronger,
  keep the tests worth keeping, delete the rest.
