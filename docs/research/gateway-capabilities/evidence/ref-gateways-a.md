# Reference gateways A: CLIProxyAPI, CLIProxyAPIPlus, OmniRoute

> Part of the [OMP2 gateway-capabilities research](../README.md). Evidence file for deliverable 3. Audited commit `85d219ae7f` (`origin/omp2`). Research proposal: nothing here is implemented.

Pinned clones (from [`ref-inventory.md`](ref-inventory.md)): `router-for-me/CLIProxyAPI@dbe641f` (Go, MIT, main, 2026-11-27); `Ve-ria/CLIProxyAPIPlus@60bb379` (Go, AGPL-3.0, main, 2026-05-09, a lagging fork); `cheaper-inference/OmniRoute@3e23a8a` (TypeScript on Next.js, MIT, release/v3.8.55, 2026-11-27). Code was read for the symbols cited; large parts were located by file name only and are marked as such. The reference repositories are untrusted input; nothing was run or installed, and nothing was copied (AGPL-3.0 for Plus). Matrices: [`matrix-router-for-me_CLIProxyAPI.tsv`](matrix-router-for-me_CLIProxyAPI.tsv), [`matrix-Ve-ria_CLIProxyAPIPlus.tsv`](matrix-Ve-ria_CLIProxyAPIPlus.tsv), [`matrix-cheaper-inference_OmniRoute.tsv`](matrix-cheaper-inference_OmniRoute.tsv). Each has 62 rows (the provider-runtime dimension set); rows not examined are `U`.

## 1. CLIProxyAPI (mainline)

**Shape.** A Go gin server that exposes OpenAI, Claude and Gemini compatible inbound routes (`/v1/chat/completions`, `/v1/responses`, `/v1/messages`, `/v1beta/...` in `router-for-me/CLIProxyAPI@dbe641f:internal/api`) in front of per-provider executors (`internal/runtime/executor/*`, one family each for Claude, Codex, Antigravity, AI Studio and others) and a credential conductor (`sdk/cliproxy/auth/conductor*.go`). It is also an SDK (`sdk/cliproxy`, `sdk/translator`) and has a plugin ABI (`sdk/pluginabi`, out of this area's scope).

**Translation.** Pairwise. `sdk/translator/helpers.go:TranslateRequestByFormatName(from, to, ...)` and `TranslateStreamByFormatName` dispatch on a `(from, to)` format pair; each pair registers its own translator in an `init.go` (for example `internal/translator/openai/claude/init.go:RegisterRequestEnvelope`); 38 registration lines were counted over `internal/translator/**/init.go`. Adding a format therefore means writing translators against every other format. OMP2 uses one canonical IR instead (`evidence/omp2-provider-runtime.md` section 3).

**Scheduling.** Credential selection strategies `RoundRobinSelector`, `WeightedRoundRobinSelector` (smooth weighted) and `FillFirstSelector` (`sdk/cliproxy/auth/selector.go`). A per-auth and per-model cooldown state is persisted and restored (`conductor_cooldown.go:RestoreCooldownStates`), with a configurable transient-error cooldown for 408, 500, 502, 503, 504 (`SetTransientErrorCooldownSeconds`) and a `Retry-After` hint path (`recoverableFailureRetryAfterWithHint`). Quota is observed from response headers into a `QuotaState` with a backoff level (`quota_signals.go:ObserveResponseHeadersForProvider`). This is the producer that OMP2 lacks (see PRV-02): OMP2 has the window types and the consumer but no header observer.

**Not found.** A circuit breaker: a case-insensitive grep over non-test Go in `internal` and `sdk` returned only an unrelated comment (`sdk/cliproxy/service_executors.go:536`). No batch API found (not searched beyond names).

**Transports.** HTTP through gin; Codex over WebSocket (`internal/runtime/executor/codex_websockets_*.go`, `internal/wsrelay`). gRPC, QUIC and WebRTC were not examined.

**Patterns not adopted.** Executor file names `claude_executor_cloaking.go`, `claude_fingerprint_policy.go` and `claude_signing.go` indicate request shaping to match an official client's identity. Only headers and names were read, not semantics. Presenting a gateway as an official client, or sharing one subscription across other people, is described here neutrally and classified `Rejected for security, licensing or operational reasons` (PRV-09); OMP2 should not copy it.

## 2. CLIProxyAPIPlus

A fork that adds community third-party providers on top of mainline and states that it tracks mainline (`Ve-ria/CLIProxyAPIPlus@60bb379:README.md`). Executors found: `aistudio`, `antigravity`, `bt`, `claude`, `codearts`, `codebuddy`, `codex`, `compat`, `cursor`, `gemini`, `github`, `gitlab`, `helps`, `iflow`, `joycode`, `kilo`, `kimi`, `kiro`, `qoder`, `openai` (`internal/runtime/executor`). It adds `SessionAffinitySelector` beside `RoundRobinSelector` and `FillFirstSelector` (`sdk/cliproxy/auth/selector.go:27,36,437`), an `internal/usage` logger plugin and a Next.js management UI (`web/`). The translator layer is the same pairwise design (31 registration lines). It is a May 2026 snapshot, so it lags mainline. Licence is AGPL-3.0, so only ideas were taken. For OMP2 the lesson is the provider list itself: most of these are subscription-product endpoints. OMP2 treats provider additions as catalog data (ADR 0059) and keeps subscription-product integrations as T2 extensions (ADR 0049 D6 class A/B).

## 3. OmniRoute

**Shape.** A Next.js dashboard plus a large TypeScript request engine (`open-sse/`: 337 files in `services/`, 209 executors, a `translator/` registry, MCP server). It is a full gateway product with combos, an "auto" scorer, resilience features and a management UI.

**Routing.** `DeterministicRoutingEngine` implements declarative rules, cost-priority ordering by USD per 1M input tokens, latency-aware ordering over a bounded sample window (200 per provider) and cooldown (`open-sse/services/routingStrategies.ts`). Named combos choose among `priority`, `weighted`, `round-robin`, `least-used`, `cost-optimized`, `reset-aware`, `strict-random` (`open-sse/services/combo.ts`, `comboConfig.ts`). An auto-combo directory holds task and tier fitness, provider diversity, a lockout filter and self-healing (`open-sse/services/autoCombo/*`), read by file name only.

**Resilience.** A real circuit breaker with states `CLOSED`, `DEGRADED`, `OPEN`, `HALF_OPEN`, adaptive reset backoff and failure-kind-aware thresholds (`src/shared/utils/circuitBreaker.ts`), scoped per connection so one account's failures do not open the breaker for every account of a provider (`open-sse/services/connectionCircuitBreaker.ts`). Also account semaphore, admission queue, adaptive connection concurrency (`accountSemaphore.ts`, `admission/queue.ts`), quota preflight and monitor (`quotaPreflight.ts`, `quotaMonitor.ts`) and provider cooldown tracking.

**Efficiency.** A dual-layer semantic response cache: exact SHA-256 lookup then vector similarity (`open-sse/services/cache/semanticCacheManager.ts`), session fingerprinting for sticky routing and prompt caching (`sessionManager.ts`), a gateway-side batch queue (`batchProcessor.ts:processPendingBatches`, provider batch endpoints not read) and a compression pipeline (`compression/pipelineEngineBreaker.ts`, algorithm not read).

**Translation.** A request and response translator registry with many `*-to-openai` and some cross pairs (`open-sse/translator/registry.ts`, `translator/request/`): hub-like but not strictly canonical.

**Patterns not adopted.** Service names such as `notionThreadSessions.ts`, `webSessionPoolHealth.ts`, `adobeFirefly*`, `deepseekWebTools.ts` and `grokCliQuotaFrame.ts` indicate web-session or browser-cookie providers and free-tier harvesting. Their semantics were not read. Using private or unauthorised interfaces, or aggregating free tiers to evade limits, is classified `Rejected for security, licensing or operational reasons`. A response cache that replays answers across callers is also a privacy and correctness risk unless namespaced per user; OMP2 is single-user, so the benefit is small.

## 4. What this adds to the OMP2 picture

| Capability | CLIProxyAPI | Plus | OmniRoute | OMP2 now |
|---|---|---|---|---|
| Credential selection strategies | round-robin, weighted, fill-first | plus session affinity | many | affinity, quota rank, id order (`pool.rs:932`) |
| Header-driven quota observation | Y | not examined | Y (per-provider parsers) | consumer only (PRV-02) |
| Circuit breaker | N | N | Y, per connection | Absent (PRV-03) |
| Latency and cost routing | not found | not found | Y | fields exist, no producer (PRV-01, PRV-07) |
| Translation architecture | pairwise | pairwise | registry, hub-like | canonical IR (PRV-14) |
| Batch | not found | not found | gateway-side queue | Absent (PRV-04) |
| Response cache | not found | not found | semantic dual-layer | Absent, not proposed |
| Management UI | via separate project | Next.js `web/` | Next.js dashboard | not a goal (owner brief section 13) |

## 5. Not verified

1. CLIProxyAPI scheduler internals (`scheduler.go`, `scheduler_weighted.go`) were not read beyond symbols.
2. Whether CLIProxyAPI or Plus have a batch path outside names searched.
3. OmniRoute auto-combo scoring weights, the semantic cache hit rate and the compression algorithm.
4. Cloaking, fingerprint and web-session modules: names and headers only.
5. gRPC, HTTP/3 and WebRTC support in all three: not examined.

Back to the [index](../README.md).
