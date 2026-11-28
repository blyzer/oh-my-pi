# OmniRoute architecture

> Part of the [OMP2 gateway-capabilities research](../README.md). Deliverable 12. Audited commit `85d219ae7f` (`origin/omp2`). Research proposal: nothing here is implemented.

Status: CURRENT, verified at `cheaper-inference/OmniRoute@3e23a8a` (reference repository, not OMP2). Code read for the cited symbols only; many modules located by file name.

```mermaid
flowchart TD
  CL["Client request"] --> H["handlers (chatCore, embeddings, audio, batches)"]
  H --> SC["Semantic cache: SHA-256 exact, then vector similarity"]
  SC -->|"miss"| CB["Combo: strategy priority, weighted, round-robin, cost-optimized, reset-aware"]
  CB --> AC["Auto combo scorer: task and tier fitness, diversity, lockout filter"]
  AC --> RE["Deterministic routing engine: cost, latency, cooldown"]
  RE --> GU["Guards: circuit breaker, semaphore, admission queue, quota preflight"]
  GU --> TR["Translator registry: request and response"]
  TR --> EX["Executor per provider"]
  EX --> UP["Upstream API"]
  UP --> TR
  UP -. "latency, failures, quota" .-> RE
  UP -. "failures" .-> GU
```

```
request -> handler -> semantic cache (exact | vector) -> miss
  -> combo (named strategy) -> auto scorer -> routing engine (cost | latency | cooldown)
  -> guards: circuitBreaker(CLOSED/DEGRADED/OPEN/HALF_OPEN) + semaphore + admission + quota
  -> translator registry -> executor(provider) -> upstream
  feedback: latency samples, failure kinds, quota -> engine and breaker
```

## Evidence

- Cache: `cheaper-inference/OmniRoute@3e23a8a:open-sse/services/cache/semanticCacheManager.ts`.
- Combos: `open-sse/services/combo.ts`, `comboConfig.ts`; auto scorer: `open-sse/services/autoCombo/*`.
- Engine: `open-sse/services/routingStrategies.ts:DeterministicRoutingEngine`.
- Breaker: `src/shared/utils/circuitBreaker.ts`; per connection: `open-sse/services/connectionCircuitBreaker.ts`.
- Guards: `open-sse/services/accountSemaphore.ts`, `admission/queue.ts`, `quotaPreflight.ts`.
- Translators: `open-sse/translator/registry.ts`.

## Legend

Rectangles are components found in code or by file name. Dashed edges are feedback. OMP2 equivalents: `Router` plus `AccountPool` plus the route stack (`06-current-provider-execution-path.md`); the breaker, latency and cost producers are the missing parts (PRV-01, PRV-03).

Back to the [index](../README.md).
