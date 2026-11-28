# Provider integration recommendations

> Part of the [OMP2 gateway-capabilities research](README.md). Deliverable 5. Audited commit `85d219ae7f` (`origin/omp2`). Research proposal: nothing here is implemented.

Basis: [`evidence/omp2-provider-runtime.md`](evidence/omp2-provider-runtime.md) (current state, with citations) and [`evidence/omp2-provider-runtime.tsv`](evidence/omp2-provider-runtime.tsv). Transport and protocol cards are in [`06-transport-and-protocol-recommendations.md`](06-transport-and-protocol-recommendations.md). Cards `PRV-01` to `PRV-09` are here; `PRV-10` onward are in 06.

## 1. Summary

OMP2 already has the hard part of a gateway inside one process: a typed catalog (125 providers, 171 routes, 5289 models, 230 aliases; measurement 11), a fixed Tower stack per route, one canonical turn (`ChatEvent`), typed failure classification, replay-gated retry and cross-route fallback, and an account pool with deterministic rotation. The gap is not missing machinery. It is that several routing inputs have **no production producer**: the router ranks on health, quota and latency, but every production `Router::new` supplies none of them (`crates/ai/src/router.rs:785-794`), and the account pool's rate and quota windows are only written by tests and by client-reported blocks (`crates/ai/src/auth/manager.rs:552`). The recommendations below therefore extend existing types and wire producers; none adds a second provider runtime, scheduler or config store.

Placement rule used (ADR 0049 D1, D6): provider logic that is vendor-neutral and on the hot path is T1 (`omp-ai`, `omp-catalog`); anything that names a vendor stays class B providers-as-data (catalog KDL and TOML) or becomes a T2 extension under `extensions/`; nothing here enters T0.

| Card | Title | Decision class |
|---|---|---|
| PRV-01 | Route runtime evidence from observed attempts | Partially implemented: extend |
| PRV-02 | Header-driven rate and quota observation into the account pool | Partially implemented: extend |
| PRV-03 | Circuit breaker as a health state | Requires benchmarking |
| PRV-04 | Batch inference | Suitable as an optional extension |
| PRV-05 | Hosted catalog overlay | Partially implemented: extend |
| PRV-06 | Route-level affinity for prompt-cache reuse | Partially implemented: extend |
| PRV-07 | Unit-aware cost ranking | Partially implemented: extend |
| PRV-08 | Cross-provider virtual model names | Unnecessary or redundant |
| PRV-09 | Credential pooling across other principals | Rejected for security, licensing or operational reasons |

## 2. Cards

### PRV-01 Route runtime evidence from observed attempts
**Decision class:** Partially implemented: extend
**Existing implementation evidence:** `RuntimeRouteEvidence { health, quota_millionths, latency, affinity, capabilities }` (`crates/ai/src/plan.rs:401`); ranking on those fields (`crates/ai/src/router.rs:1277 compare_evaluated`); `Unavailable` rejects a candidate (`router.rs:815`); `Router::with_runtime_evidence` (`router.rs:166`) is called only from `crates/ai/tests/conformance.rs:1202`; production sites `crates/driver/src/registry.rs:942,958`, `crates/serve/src/inference.rs:449,470`, `crates/app/src/daemon.rs:489` do not call it, so every route gets `health: Unknown, quota 0, latency Duration::MAX` (`router.rs:785-794`). Per-attempt facts already exist in `ExecutionReceipt` (`crates/ai/src/receipt.rs`) and `ErrorKind`/`RetryAction` (`crates/ai/src/error.rs:26,265`). Maturity: Prototype.
**Actual missing capability:** An in-process, deterministic observer that folds finished attempts (outcome class, handshake and first-chunk time, error kind) into a per-route `RuntimeRouteEvidence` for the current registry generation, and a `Router` that reads it. No LLM, no new store; state is memory-only and keyed by `RouteId` plus registry generation (`Registry::generation`, `registry.rs:189`), so a catalog refresh resets it, which the stale-generation check already enforces (`router.rs:795`).
**Alternatives considered:** (a) Leave ranking static and rely on per-call fallback: simple, but each new call re-tries a degraded first route. (b) Route through an external gateway (OmniRoute or CLIProxyAPI style) for health: adds a second runtime, rejected by owner constraint 8. (c) Persist health in SQLite: unnecessary for session-length state.
**Expected benefit:** Latency and reliability: avoid repeating a failed first attempt on every call; availability of `S03`, `S08`. Not a token or network saving.
**Implementation complexity:** medium. The data type, ranking and staleness check exist; the work is the observer, a decay rule and tests. `omp-ai` change only.
**Security implications:** Health evidence is credential-free by type (`plan.rs:399`). It must not expose account ids; keep it route-scoped.
**Maintenance cost:** Low to medium: one small state machine with fixed constants in settings; no catalog schema change.
**Performance measurement strategy:** Replay a recorded failure-heavy session against fake providers (`crates/ai/src/provider/fake.rs`, cassette transport `crates/ai/src/transport/cassette.rs`) and compare total first-attempt failures and end-to-end latency with and without the observer. Not measured.
**Acceptance criteria:** After N classified failures of a route inside a window, the next plan ranks it below a healthy equivalent route; after the decay interval it is eligible again; a registry generation change discards the evidence; ranking stays a total deterministic order (property test over `compare_evaluated`); no behaviour change when only one route exists.
**Follow-up:** PRV-03 is the same state machine with explicit open and half-open states; decide them together.

### PRV-02 Header-driven rate and quota observation into the account pool
**Decision class:** Partially implemented: extend
**Existing implementation evidence:** Rate and quota windows (`crates/ai/src/account/rate.rs`, `quota.rs`), `AccountPool::observe_rate`/`observe_quota` (`crates/ai/src/account/pool.rs:750,769`) with persisted state (`account/store.rs`), `PoolRateLimiter` consuming the state (`crates/ai/src/provider/builtin.rs:2721`), `QuotaReservePolicy` (`pool.rs:134`), 15 usage fetchers (`crates/ai/src/operation/usage/*.rs`). A grep across `crates/` finds no non-test caller of `observe_rate` or `observe_quota`; the only writers are `report_block` (`auth/manager.rs:552`) and `record_rate_429` (`pool.rs:788`). Maturity: Partial.
**Actual missing capability:** A producer: a response observer that reads each provider's rate-limit and `Retry-After` headers (codec-decoded `ProviderTelemetryEvent`, `crates/ai/src/codec/mod.rs:666`) and the usage manager's fetched windows, and writes them through the existing `observe_*` methods. After that, a 429 on one call is remembered for the next call instead of only rotating within the current call.
**Alternatives considered:** (a) Keep client-reported blocks only (current). (b) Poll usage endpoints on a timer: costs requests and some providers meter them; use fetched usage only when the user already asked for it. (c) New quota service: rejected (second scheduler).
**Expected benefit:** Reliability and latency: fewer doomed attempts; fewer 429 round trips (network transmission). It reduces waste against limits the user already has; it does not raise any limit.
**Implementation complexity:** medium: per-codec header mapping, which belongs in catalog data where the header names are provider facts (providers-as-data).
**Security implications:** Headers can carry account identifiers; store only window ids, reset times and remaining counts (the existing types already do). Must not be used to rotate across principals the user is not entitled to use (see PRV-09).
**Maintenance cost:** Medium: provider header formats drift; keep the mapping in KDL with fixture tests.
**Performance measurement strategy:** Cassette replay of recorded 429 sequences; count wasted attempts before and after. Not measured.
**Acceptance criteria:** With two fake accounts and a recorded 429 plus `Retry-After`, the second logical call selects the other account without a failed first attempt; state survives restart through the existing store; `QuotaReservePolicy::FallbackPercent` triggers from observed remaining quota in an integration test.
**Follow-up:** Auth-side ownership of the usage manager belongs to the auth area; coordinate before changing `operation/usage`.

### PRV-03 Circuit breaker as a health state
**Decision class:** Requires benchmarking
**Existing implementation evidence:** None. Case-insensitive grep for `circuit`, `breaker`, `half_open` in `crates/ai`, `crates/serve`, `crates/catalog`, `crates/http`, `crates/driver/src/registry.rs` finds nothing. Nearest: account cooldown (`pool.rs:632`, no production writer) and per-call `ReselectRoute` (`crates/ai/src/registry.rs:874`). Maturity: Absent.
**Actual missing capability:** An open, half-open and closed state per route and account, derived from classified failures (`ErrorKind` network and 5xx classes), so repeated failures stop being re-tried for a cool-off.
**Alternatives considered:** Fold into PRV-01 as a `RouteHealth::Unavailable` with a timer (preferred: no new type); a separate breaker crate (rejected, redundant).
**Expected benefit:** Latency and reliability, only if first-attempt failures repeat in real sessions. Unmeasured, which is why the class is benchmarking.
**Implementation complexity:** low once PRV-01 exists.
**Security implications:** A breaker keyed by a shared host could be made to open by one noisy account; key by route and account.
**Maintenance cost:** Low.
**Performance measurement strategy:** Count, from existing session journals or a replay, how often consecutive calls hit the same failing route; plan: instrument via `provider_request` spans (`registry.rs:785`).
**Acceptance criteria:** If the measurement shows repeated failures above an agreed rate, implement as PRV-01 states with tests for open, half-open probe and close.
**Follow-up:** Extend PRV-01 if the benchmark passes.

### PRV-04 Batch inference
**Decision class:** Suitable as an optional extension
**Existing implementation evidence:** None. Grep for `/batches`, `batch_api`, `BatchJob`, `message_batches` in `crates/ai/src` and `crates/serve/src` is empty. Asynchronous job plumbing exists for video (`crates/ai/src/operation/job.rs`, not read in depth). Maturity: Absent.
**Actual missing capability:** Submitting many non-interactive requests (evals, bulk summarisation, `omp bench`) to a provider batch endpoint and polling results.
**Alternatives considered:** In-core `OperationKind::Batch` (adds a core operation for a niche flow; against ADR 0049 default of T2); the existing `operation/job.rs` pattern (extend later).
**Expected benefit:** Reduced billable cost where providers discount batch work; no benefit for interactive turns. Discount sizes are provider-published and were not verified here.
**Implementation complexity:** medium: vendor-specific request shapes and a polling lifecycle.
**Security implications:** Batch results arrive later and may be stored provider-side; document retention; credentials follow the normal lease path.
**Maintenance cost:** Medium per provider.
**Performance measurement strategy:** Not applicable until a workload exists; plan is cost per 1000 eval requests with and without batching.
**Acceptance criteria:** An extension submits a batch through the frozen `omp.*` API and existing auth leases, with no change to `crates/ai` beyond a documented generic async-job operation if needed.
**Follow-up:** Placement T2 per ADR 0049 D1; promote only on evidence.

### PRV-05 Hosted catalog overlay
**Decision class:** Partially implemented: extend
**Existing implementation evidence:** `Catalog::additive_shared_catalog_overlay` and `sanitize_shared_catalog_overlay` (`crates/catalog/src/snapshot.rs:236,304`) compile a remote models.dev-style catalog into an additive overlay restricted to known providers and routes; a unit test exists (`snapshot.rs:1120`); no production caller. The overlay stack and discovery refresh are production (`crates/driver/src/registry.rs:451,847`). Maturity: Prototype.
**Actual missing capability:** An opt-in refresh that fetches a hosted catalog, verifies it, and feeds the existing overlay stack so new models and prices appear between releases.
**Alternatives considered:** Rely on per-provider discovery (already live for 82 of 92 discovery specs of kind `OpenAiModels`/`OllamaTags`/`GoogleModels`; discovery rows carry little capability metadata, to be verified); rebuild the bundled snapshot more often (release cost).
**Expected benefit:** Freshness of capability and price metadata (accuracy of routing and cost). No token saving.
**Implementation complexity:** medium: fetch, size and time bounds, signature or pin policy.
**Security implications:** A remote catalog is untrusted input that can change prices, context limits and capabilities and so influence routing. Keep additive-only, bounded, and never let it add routes or endpoints (the sanitizer's contract); require an explicit opt-in setting.
**Maintenance cost:** Low: code exists.
**Performance measurement strategy:** Not applicable; measure catalog staleness (days between a provider release and availability) before and after.
**Acceptance criteria:** Overlay applies only to known providers; a corrupted or oversized document is rejected with no change; precedence versus user `models.toml` and discovery is tested.
**Follow-up:** Decide trust anchor with the security owner.

### PRV-06 Route-level affinity for prompt-cache reuse
**Decision class:** Partially implemented: extend
**Existing implementation evidence:** Account-level affinity ranks previous account then principal first (`crates/ai/src/account/pool.rs:974-980`); `AccountPin` (`pool.rs:232`); `PlanRequest.affinity_route` exists but the planner always sets `None` (`crates/ai/src/router.rs:285`); ranking already prefers affinity (`router.rs:1294`). Cache capability and accounting exist (`crates/catalog/src/capability.rs:446`, `crates/ai/src/receipt.rs:147-154`). Maturity: Partial.
**Actual missing capability:** Set `affinity_route` from the session's last successful route so fallback does not hop routes without need, which loses provider-side prefix caches and chained state.
**Alternatives considered:** Cache-key routing across routes (providers do not share caches); leave as is.
**Expected benefit:** Reduced billable tokens and latency where the provider prices cached reads lower; reduced network transmission when chaining by id. Unmeasured.
**Implementation complexity:** low: session already tracks the binding (`crates/ai/src/session`).
**Security implications:** None beyond existing session binding.
**Maintenance cost:** Low.
**Performance measurement strategy:** Cache read share (`cache_read_tokens` over input tokens) across multi-turn replays with and without route affinity, using the receipt counters. Not measured.
**Acceptance criteria:** In a multi-turn fake-provider test with two eligible routes, turn N+1 uses the route of turn N unless it failed; cache read tokens in receipts do not drop at a route switch that did not need to happen.
**Follow-up:** Part of the PRV-01 work item.

### PRV-07 Unit-aware cost ranking
**Decision class:** Partially implemented: extend
**Existing implementation evidence:** `price_score` sums `nanos_usd` over every price component with weight 1 regardless of unit (`crates/ai/src/router.rs:1346-1361`), so per-million-token, per-image and per-request prices are added raw; it is only a tiebreaker (`router.rs:1303`). Pricing types are typed (`crates/catalog/src/pricing.rs`); cost ledger exists (`crates/ai/src/layer/mod.rs:95`). Maturity: Partial.
**Actual missing capability:** Rank on estimated cost of this request from known input size, expected output and cache-read share, using the typed units.
**Alternatives considered:** Keep tiebreaker; add user-set per-provider preference (already `configured_model_rank`, `router.rs:1327`).
**Expected benefit:** Reduced billable tokens spend when two routes serve one model; small unless duplicates are common (census: `openai-chat` 4082 model-route pairs for far fewer distinct models).
**Implementation complexity:** low.
**Security implications:** None.
**Maintenance cost:** Low.
**Performance measurement strategy:** Count model keys served by more than one route in the snapshot to size the benefit; not measured here.
**Acceptance criteria:** For two routes with different price shapes, the cheaper estimated request wins; ties fall through to the existing id order; mixed-unit overflow cannot occur (`u128` checked).
**Follow-up:** None.

### PRV-08 Cross-provider virtual model names
**Decision class:** Unnecessary or redundant
**Existing implementation evidence:** Exact provider-scoped aliases (`crates/catalog/src/compile.rs:1048`, `snapshot.rs:716`), ordered `FallbackChain` with revert policy (`crates/catalog/src/resolve.rs:84,107`), roles (`crates/catalog/src/selection.rs:24 ModelRole`). Maturity: Implemented.
**Actual missing capability:** None: a name that fans out to several providers is a role plus fallback chain.
**Alternatives considered:** A gateway-style combo registry; rejected as a second config store.
**Expected benefit:** None beyond current.
**Implementation complexity:** not applicable.
**Security implications:** Fan-out under one name hides which provider received the prompt; the current explicit chain keeps that visible.
**Maintenance cost:** not applicable.
**Performance measurement strategy:** not applicable.
**Acceptance criteria:** not applicable.
**Follow-up:** Revisit after the reference comparison in section 4.

### PRV-09 Credential pooling across other principals
**Decision class:** Rejected for security, licensing or operational reasons
**Existing implementation evidence:** OMP2 rotates among accounts the user holds and can restrict rotation to one principal (`RotationPolicy.preserve_principal`, `crates/ai/src/account/pool.rs:212`); `AccountPin` pins a session (`pool.rs:232`). Maturity: Implemented for the user's own accounts.
**Actual missing capability:** Gateways in the reference set offer multi-account pools and shared-subscription serving; see section 4 for what each does.
**Alternatives considered:** Keep own-account rotation only.
**Expected benefit:** Pooling one subscription across other people, or rotating to evade a provider's usage limits, would reduce nothing legitimate and breaches provider terms.
**Implementation complexity:** not applicable.
**Security implications:** Credential sharing widens blast radius and breaks per-principal audit.
**Maintenance cost:** not applicable.
**Performance measurement strategy:** not applicable.
**Acceptance criteria:** Rotation stays bounded to credentials the user is entitled to use; PRV-02 never feeds a policy that selects a principal the session was not pinned to.
**Follow-up:** None.

## 3. Placement and sequence

1. PRV-01, PRV-06 and PRV-07 touch `crates/ai` only and can ship as one slice with a deterministic replay test.
2. PRV-02 needs catalog data for header names plus the auth owner's agreement on the usage manager.
3. PRV-03 waits on PRV-01 and a measurement.
4. PRV-04 and PRV-05 are optional and independent.

## 4. Reference comparison

Pinned references and the full read notes are in [`evidence/ref-gateways-a.md`](evidence/ref-gateways-a.md); matrices are `evidence/matrix-router-for-me_CLIProxyAPI.tsv`, `matrix-Ve-ria_CLIProxyAPIPlus.tsv` and `matrix-cheaper-inference_OmniRoute.tsv`.

| Card | Reference evidence | What it changes |
|---|---|---|
| PRV-01, PRV-06 | `cheaper-inference/OmniRoute@3e23a8a:open-sse/services/routingStrategies.ts:DeterministicRoutingEngine` records per-provider latency samples (bounded at 200) and orders by cost and latency; `Ve-ria/CLIProxyAPIPlus@60bb379:sdk/cliproxy/auth/selector.go:437 SessionAffinitySelector` pins by session | OMP2 already has the ranking fields and a stale-generation guard; the references confirm the producer is small, in-memory and deterministic. Session affinity exists in two gateways, which supports PRV-06. |
| PRV-02 | `router-for-me/CLIProxyAPI@dbe641f:sdk/cliproxy/auth/quota_signals.go:ObserveResponseHeadersForProvider` writes response-header quota signals into `QuotaState`; `cheaper-inference/OmniRoute@3e23a8a:open-sse/services/rateLimitManager.ts` | Same producer OMP2 lacks. Header names are provider facts, so keep them in catalog data (providers-as-data), not Rust. |
| PRV-03 | `cheaper-inference/OmniRoute@3e23a8a:src/shared/utils/circuitBreaker.ts` (CLOSED, DEGRADED, OPEN, HALF_OPEN, adaptive backoff, failure-kind thresholds) and `open-sse/services/connectionCircuitBreaker.ts` (per connection scope); CLIProxyAPI and Plus have none | One reference has a breaker and two do not. The OmniRoute lesson worth taking is scoping by connection and failure kind, so one account's failures do not open the route. Benefit still unmeasured, so the class stays benchmarking. |
| PRV-04 | `cheaper-inference/OmniRoute@3e23a8a:open-sse/services/batchProcessor.ts:processPendingBatches` is a gateway-side queue; provider batch endpoints were not read | Does not show a provider-batch integration to copy; supports keeping batch as an optional extension. |
| PRV-07 | `cheaper-inference/OmniRoute@3e23a8a:open-sse/services/routingStrategies.ts` costPriority sorts by USD per 1M input tokens only | A single-unit price sort; OMP2's typed pricing can do better. |
| PRV-08 | `cheaper-inference/OmniRoute@3e23a8a:open-sse/services/combo.ts` named combos with strategies priority, weighted, round-robin, least-used, cost-optimized, reset-aware, strict-random | Combos are a second config store for what roles plus `FallbackChain` already express; kept `Unnecessary or redundant`. |
| PRV-09 | `router-for-me/CLIProxyAPI@dbe641f:internal/runtime/executor/claude_executor_cloaking.go` and `claude_fingerprint_policy.go` (names and headers read only); OmniRoute web-session modules such as `open-sse/services/notionThreadSessions.ts` | Described neutrally: client-identity emulation, shared subscriptions and cookie-session providers are outside the authorised-use line and stay rejected. Weighted or fill-first rotation across the user's own keys is the legitimate part and OMP2 already has rotation. |

Not adopted from any of the three: a mandatory dashboard, a database server, a semantic response cache (OmniRoute `open-sse/services/cache/semanticCacheManager.ts`; single-user benefit is small and replaying answers needs per-user namespacing), pairwise translators (see 06).

Back to the [index](README.md).
