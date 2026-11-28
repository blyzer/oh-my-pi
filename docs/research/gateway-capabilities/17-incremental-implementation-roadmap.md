# Incremental implementation roadmap

> Part of the [OMP2 gateway-capabilities research](README.md). Deliverable 17. Audited commit `85d219ae7f`. Research proposal: nothing here is implemented; no date or effort estimate is given, because none was measured.

The roadmap is a sequence of **vertical slices**: each ends in something observable and reversible, extends a seam that exists today, and leaves the default build unchanged unless its card says otherwise. Slices are ordered by dependency, not by size. A slice does not start until its preconditions (owner answers in [deliverable 20](20-open-questions.md) and measurements in [deliverable 13](13-performance-and-benchmark-plan.md)) exist. Nothing here is approved: the owner's principle is to review the research, evidence, diagrams, alternatives and proposed ADRs first.

## Slice 0: baseline and decisions (no product code)

- **Cards:** measurement only ([SWM-08](09-single-session-swarm-assessment.md) per-child duplication; INF B2 embedding-worker duplication in [deliverable 10](10-local-inference-assessment.md); the idle-machine baseline of binary size, start time and idle RSS from [deliverable 13](13-performance-and-benchmark-plan.md) section 6).
- **Preconditions:** owner answers to OQ-11 (thresholds) and OQ-19 (draft-ADR order).
- **Acceptance:** each measurement file under `evidence/measurements/` holds command, environment, at least 30 runs and raw output on an idle machine; thresholds are written down before the comparison.
- **Must not:** change any code or ADR.

## Slice 1: credential and routing-evidence correctness (P0)

- **Cards:** [AUTH-01](05b-authentication-and-credential-lifecycle.md) per-process refresh-owner token; [AUTH-07](05b-authentication-and-credential-lifecycle.md) one disable-on-refresh-failure rule; [PRV-01](05-provider-integration-recommendations.md) route runtime evidence from observed attempts; [PRV-02](05-provider-integration-recommendations.md) header-driven rate and quota observation. [PRV-03](05-provider-integration-recommendations.md) (circuit breaker) follows only after PRV-01 and its benchmark.
- **Preconditions:** OQ-02 and OQ-03; a draft ADR amendment for the provider-evidence change ([deliverable 16](16-draft-adrs.md)).
- **Acceptance (from the cards):** two processes do not both refresh one rotating token (test with a two-process harness against a fake provider); the router receives non-default health, quota and latency evidence from production attempts, shown with the cassette transport and fake providers so no provider quota is consumed.
- **Must not:** add a second scheduler or evidence store; use header observation to defeat a provider's limits.

## Slice 2: harden existing contracts (P0 and P1)

- **Cards:** [IPC-03](06b-ipc-and-serialization.md) hash-while-streaming blob upload; [IPC-09](06b-ipc-and-serialization.md) bounded frame allocation; [IPC-06](06b-ipc-and-serialization.md) document the extension CONTROL channel; [EXT-03, EXT-04, EXT-05](07-abi-independent-extension-assessment.md) enforce or remove the declared-but-unproduced manifest codes and admit `omp_api` in Rust; [SWM-05](09-single-session-swarm-assessment.md) default wall-clock limit and recovery for children; [SWM-09](09-single-session-swarm-assessment.md) workspace base digest on the job record; [SWM-13](09-single-session-swarm-assessment.md) reclaim orphaned worktrees by owner liveness.
- **Preconditions:** OQ-06 (what to do with the unproduced codes), OQ-15 (default deadline).
- **Acceptance:** each card's own criteria; a regression test for every behaviour change; no wire change without the existing `buf breaking` and `SCHEMA_REV` discipline (ADR 0048).
- **Must not:** change a wire format without a compatibility check.

## Slice 3: share, do not duplicate (P1)

- **Cards:** [AUTH-03](05b-authentication-and-credential-lifecycle.md) one auth stack across in-process children; [INF-01](10-local-inference-assessment.md) wire or retire the embedding worker; [INF-02, INF-10](10-local-inference-assessment.md) shared model residency, one memory budget and a typed placement policy; [AUTH-09](05b-authentication-and-credential-lifecycle.md) authentication health; [PRV-16](06-transport-and-protocol-recommendations.md) queue-latency and per-route metrics.
- **Preconditions:** Slice 0 measurements show duplication is real (otherwise the cards close); OQ-03, OQ-08, OQ-09.
- **Acceptance:** a child reuses the parent's account pool and credential connection without widening its pin or key-source mode; one embedding worker serves all agents in a process; the sum of model memory is bounded by one budget.
- **Must not:** share generation state or KV caches (only weights); add a daemon outside the existing envd supervision.

## Slice 4: provider efficiency, measured (P1 and M)

- **Cards:** [PRV-06](05-provider-integration-recommendations.md) route-level cache affinity; [PRV-07](05-provider-integration-recommendations.md) unit-aware cost ranking; [PRV-05](05-provider-integration-recommendations.md) hosted catalog overlay (Prototype today); [PRV-15](06-transport-and-protocol-recommendations.md) failure classification tables as catalog data.
- **Preconditions:** Slice 1 evidence feed exists; each card's measurement (cache effectiveness, TTFT overhead) with owner-set thresholds.
- **Acceptance:** the benefit is stated as one of reduced network transmission, reduced inference computation or reduced billable tokens, and measured as such.
- **Must not:** add model-name conditionals in `.rs`; classify by LLM at runtime.

## Slice 5: optional and external integrations (P3)

- **Cards:** [PRV-13](06-transport-and-protocol-recommendations.md) OpenAI-compatible facade over `omp-serve` (only if OQ-10 says third-party clients are in scope); [PRV-04](05-provider-integration-recommendations.md) batch inference; [INF-03, INF-05](10-local-inference-assessment.md) llama.cpp server and MLX as managed servers; [EXT-06, EXT-08](07-abi-independent-extension-assessment.md) host status in management and non-Python providers as external processes; [SWM-11](09-single-session-swarm-assessment.md) external harnesses as delegated executors. Code-intelligence cards: [CIX-14](11-code-intelligence-and-scip-assessment.md) report absent producers and file coverage in every query (Slice 5); [CIX-02, CIX-03, CIX-05, CIX-11](11-code-intelligence-and-scip-assessment.md) belong to Slice 3 (read-only query surface, one shared index keyed by workspace identity and revision, provenance on every relation, one typed result contract); [CIX-06, CIX-10](11-code-intelligence-and-scip-assessment.md) wait for their measurements.
- **Preconditions:** a concrete user or provider need for each; tier T2 or out-of-process placement (ADR 0049 D1).
- **Acceptance:** off by default, no new default listener, removable without touching core.

## Not scheduled

Everything in the backlog's tier D ([deliverable 15](15-prioritized-architectural-backlog.md)): changes that reopen ADR 0036, the locked "candle only" rule, or the extension runtime. They enter the roadmap only after an owner decision.

Back to the [index](README.md).
