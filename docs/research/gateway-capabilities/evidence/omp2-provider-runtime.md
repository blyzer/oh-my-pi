# OMP2 provider runtime audit

> Part of the [OMP2 gateway-capabilities research](../README.md). Evidence file for deliverables 2, 4, 5 and 6. Audited commit `85d219ae7f` (`origin/omp2`). Research proposal: nothing here is implemented.

Scope: `crates/ai`, `crates/catalog`, `crates/http`, `crates/serve`, `crates/observability`, `crates/roster-probe`, `crates/proto`, `crates/driver/src/registry.rs` (production composition). Dimension groups G, S, T, I, E, O as assigned. Authentication internals are out of scope. The TSV twin is [`omp2-provider-runtime.tsv`](omp2-provider-runtime.tsv). Counts come from the code and catalog data, not from docs: see [`measurements/11-catalog-census.md`](measurements/11-catalog-census.md).

## 0. Request path, verified

Kernel to wire: `compose_kernel` (`crates/driver/src/headless/kernel.rs:2457`) builds `Kernel<C>` (`crates/agent/src/loop.rs:463`), which calls the `Inference` capability (`crates/agent/src/loop.rs:115`), adapted by `GatewayInference` (`crates/driver/src/headless/gateway.rs:30`) onto `Client<ProviderService, Router>` (`crates/driver/src/registry.rs:942`). `Router::plan_call` (`crates/ai/src/router.rs:173`) builds an `ExecutionPlan` with ranked `fallbacks`; `RegistryHandle::service` (`crates/ai/src/registry.rs:346`) builds the outer stack `Observe > OverallBudget > Staging > dispatch` (`crates/ai/src/layer/stack.rs:228`, `build_execution_stack`). `dispatch_preplanned` (`registry.rs:651`) loops over the planned candidates; each candidate has one preconstructed route stack (`build_route_stack`, `stack.rs:236-274`): `ProviderError` hook, `OperationPolicy`, `Intent`, `Session`, `Answer`, `Semantic`, `Recovery`, then `Attempt` containing `Admission`, `AccountPool`, `AuthLease`, `TransportRetry`, `Rate`, `Encode`, `CredentialApply`, `WireTransport`. The wire codec implements the sans-I/O `Codec` trait (`crates/ai/src/codec/mod.rs:918`); its `Decoder` (`:844`) emits `RawEvent` (`:797`), whose chat arm is the canonical `ChatEvent` (`crates/ai/src/event.rs:178`). Maturity: Implemented. Routes are rebuilt off the request path and swapped atomically (`RegistryHandle`, `registry.rs:324`, `ArcSwap`).

## 1. G, registry and discovery

| Dim | Finding | Maturity |
|---|---|---|
| G01 provider identity | `ProviderId` newtype (`crates/catalog/src/id.rs:6`), `ProviderDef` with its `RouteId`s (`crates/catalog/src/provider.rs:698`). Census of the embedded snapshot: 125 providers, 171 routes (measurement 11). `providers.toml` has 125 `[providers.*]` tables (24 `transport = "embedded"` tool or search entries, counted with a TOML parse). Extensions add a provider at runtime through `Catalog::with_runtime_provider` (`crates/catalog/src/snapshot.rs:462`). | Implemented |
| G02 model identity | `ModelKey` is `<provider>/<model>` (`id.rs:40 provider_scoped`), so two providers serving one model id stay two models. `ClassId` carries vendor lineage, `WireModelId` is the opaque wire spelling (`id.rs:30`). Router-facing `PolicyModel` (`model.rs:285`) cannot reveal wire ids; only `WireTarget` (`model.rs:342`) carries them. Census: 5289 models (source fixture `models.json.zst` has 5369 rows across 75 providers, 3321 distinct ids, parsed with `zstd -dc` and `json`; the compiler collapses 80 rows into aliases). | Implemented |
| G03 aliases | `CatalogAlias` (`compile.rs:1048`, 230 in the snapshot) resolved by `Catalog::resolve_alias` (`snapshot.rs:716`); provider-scoped `AliasSelector` and `ModelSelector::{Exact, Alias}` (`resolve.rs:51,75`); user roles in `selection.rs`. Aliases are exact and provider-scoped. There is no gateway-style virtual model name that fans out across providers; ordered `FallbackChain` (`resolve.rs:84`) covers the use. | Implemented |
| G04 deployments | One model maps to per-route wire ids (`ModelSpec.wire_ids: (RouteId, WireModelId)`, `model.rs:236`). Azure uses `{region}` and `{deployment}` in `base_url` (`fixtures/llm-oracle/catalog/providers.toml`, `[providers."azure"]`). Per-account deployment inventories are not modelled in the catalog. | Partial |
| G05 dynamic discovery | `DiscoveryProbe` (`crates/ai/src/discovery/probe.rs:79`), discovery kinds `OpenAiModels` 80, `Specialized` 10, `OllamaTags` 1, `GoogleModels` 1 (measurement 11), SQLite `DiscoveryStore` with TTL (`discovery/store.rs:110`), and a session refresh owner: `DiscoveryRefresher::spawn` (`crates/driver/src/registry.rs:847`) re-probes at cache expiry or after a login and publishes through `RegistryHandle`. Failure keeps the old catalog (`registry.rs:875`). | Implemented |
| G06 hosted catalogs | `Catalog::additive_shared_catalog_overlay` (`snapshot.rs:236`) compiles a models.dev-style catalog into a safe additive overlay. A workspace grep finds only its definition and one test (`snapshot.rs:1120`); no production caller. | Prototype |
| G07 catalog sync | Offline: `catalog.postcard` compiled from pinned sources (`data/sources.lock.json`, `just catalog-snapshot`). Runtime: `production_catalog` layers bundled, `models.toml` user overlay and fresh discovery rows in explicit precedence (`registry.rs:451`, `with_overlay_stack` `snapshot.rs:529`); `DiscoveryPollGate` limits polling (`catalog/src/discover.rs:230`). No remote catalog pull (see G06). | Partial |
| G08 capability metadata | `ModelCapabilities` (`catalog/src/capability.rs:940`) uses tri-state `Availability<C>` (`capability.rs:181`), operations bitset, reasoning, image input, `PromptCacheCapabilities` (`:446`). The router negotiates required versus preferred features against it (`plan.rs:570 negotiate`, `router.rs:970 catalog_capability_evidence`) and can drop or emulate with receipt evidence. | Implemented |

Providers-as-data check (AGENTS.md lines 526-582): provider facts live in `providers.toml` and `compat/*.kdl`; `crates/catalog/src/compile.rs:3699,3958,4862` branch on the literal provider id `"cursor"`, a provider-name conditional in the compiler (not a model-name one; the lintx rule is about model names).

## 2. S, scheduling and routing

Two selection layers exist and must not be confused. Route selection (which model and route) is the `Router`; account selection (which credential of one provider) is the `AccountPool`.

| Dim | Finding | Maturity |
|---|---|---|
| S01 deterministic selection | `compare_evaluated` (`router.rs:1277`) orders candidates by configured model rank, affinity, health, remaining quota, latency, price, provider id, route id, codec id: a total order with no randomness. | Implemented |
| S03 availability | `RouteHealth` and `RuntimeRouteEvidence` exist (`plan.rs:388,401`), and `Unavailable` rejects a candidate (`router.rs:815`). Every production `Router::new` site (`driver/src/registry.rs:942,958`, `serve/src/inference.rs:449,470`, `app/src/daemon.rs:489`) omits `with_runtime_evidence` (`router.rs:166`; only tests call it, `ai/tests/conformance.rs:1202`), so each route gets the default `health: Unknown, quota 0, latency Duration::MAX` (`router.rs:785-794`). All `RouteHealth::Healthy` constructors are inside `#[cfg(test)]` modules. Account-level eligibility is real (`Eligibility`, `account/pool.rs:86`). | Partial |
| S04 rate-limit awareness | Rate windows and `Retry-After` handling (`account/rate.rs`), `PoolRateLimiter` rejects or delays an attempt for a blocked account (`provider/builtin.rs:2721`). Producers: only `AuthManager::report_block` (`auth/manager.rs:552`, exposed to Python) and `record_rate_429` (`pool.rs:788`). `observe_rate` and `observe_quota` have no caller outside tests (grep across `crates/`). A 429 changes the same call (rotate or retry, below) but is not remembered for the next call. | Partial |
| S05 session affinity | Account level: previous-account then principal affinity rank first in `AccountPool::select` (`pool.rs:932-990`); `AccountPin` (`pool.rs:232`) pins a session; `SessionAffinity` (`layer/mod.rs:74`). Route level: `PlanRequest.affinity_route` is always `None` (`router.rs:285`). | Partial |
| S06 capability-aware routing | `extract_requirements` and `negotiate` pick only routes that satisfy required features; `RouteRestrictions` (`catalog/src/provider.rs:721`) cap operations, context and strict tools; wire settings can disable a route (`router.rs:744`). Forced tool calls use `forced_call_ladder` (`plan.rs:267`, ADR 0019). | Implemented |
| S07 cost-aware | `price_score` (`router.rs:1346`) sums `nanos_usd` over all price components with weight 1 for every unit (tokens, images, seconds, requests are added raw); it is a tiebreaker after rank, affinity, health, quota and latency. Per-call budgets and cost ledger exist (`layer/budget.rs`, `ExecutionContext.cost_micro_usd`, `layer/mod.rs:95`). | Partial |
| S08 latency-aware | The `latency` field is ranked (`router.rs:1302`) but never populated in production (S03). | Prototype |
| S09 circuit breakers | No breaker, half-open state or consecutive-failure ejection in `crates/ai`, `crates/serve`, `crates/catalog`, `crates/http`, `driver/src/registry.rs` (case-insensitive grep for `circuit`, `breaker`, `half_open`). Closest mechanisms: per-account cooldown (`pool.rs:632`, persisted, but no production writer found) and per-call route reselection. | Absent |
| S10 backpressure | `AdmissionController` bounds in-flight and queued callers per provider (`layer/admission.rs:24`; `max_in_flight` keyed by provider, `max_queued` default 64, `settings.rs:412-433`) and fails fast when the queue is full (`admission.rs:139`). Unlimited by default. | Implemented |
| S11 cancellation | `ExecutionContext` holds `cancelled` plus a `Notify` (`layer/mod.rs:86-87`); `checkpoint` is called between phases (`mod.rs:516`) and a `Cancellation` handle reaches the transport (`codec/mod.rs:957`). `crates/e2e/tests/p2_cancel_matrix.rs` exists (not run). | Implemented |
| S12 retry policies | `RetrySettings` (`settings.rs:80`: 10 retries, 500 ms base, 300 s cap, model fallback on); `TransportRetryService` retries the same route only before commit, only when body replay evidence allows it, with full jitter and `Retry-After` as a floor (`layer/retry.rs:97,124-197,234`); `AttemptService` re-enters for credential refresh or rotation (`layer/attempt.rs:90-123`); cross-route fallback in `dispatch_preplanned`, guarded by `fallback_is_safe` (`registry.rs:874`: uncommitted, `ReselectRoute`, replay allowed). Chain state `ACTIVE_FALLBACKS` is a process-global `Mutex<BTreeMap>` (`settings.rs:123`). | Implemented |
| S13 failure classification | `ErrorKind` (43 variants, `error.rs:26`), `ErrorPhase`, `RetryAction` (`error.rs:265`: `Never`, `SameRoute`, `SameRouteLimited`, `RefreshCredential`, `RefreshCredentialOnce`, `RotateAccount`, `ReselectRoute`, `ReseedSession`, `SemanticRetry`). HTTP mapping is a deterministic table: `classify_http_error_with_hint` (`transport/http.rs:1273`: 401 refresh, 403 and account caps rotate, 429 and 5xx same-route with hint, 402 payment, 413 and context-overflow text never retried), plus per-codec classifiers (`anthropic.rs:3326`, `openai_chat.rs:2595`, `openai_responses.rs:3764`). Message-text tables are in Rust, not catalog data. | Implemented |
| S14 quota-aware scheduling | `QuotaReservePolicy` (`pool.rs:134`: disabled, fall back below N percent, fail closed), `UsageReservePolicy` (`settings.rs:67`), quota windows (`account/quota.rs`), 15 provider usage fetchers (`operation/usage/*.rs`). The usage manager reads pool state but a grep finds no code that writes fetched usage into the pool (S04 note), so reserve fallback depends on state that production does not populate. | Partial |

Provider-restriction note: account rotation here is bounded to credentials the user owns and to the failure classes above (`RotateAccount` on account-local caps). Nothing in the audited code spreads one subscription across other principals. `RotationPolicy.preserve_principal` (`pool.rs:212`) can pin rotation to one principal.

## 3. T, protocol translation

Architecture (T12): canonical intermediate representation, not pairwise translators. Every route has one `Codec` that lowers a canonical `OperationCall` to the wire and decodes frames to `RawEvent`/`ChatEvent` (`codec/mod.rs:918,797`; `event.rs:178`); a provider adds a codec only for a genuinely new wire protocol. OMP2 translates canonical to provider wire; it does not translate one provider wire into another provider wire.

Codecs counted from the snapshot (measurement 11): 33 distinct codec ids on 171 routes. Fourteen are model-serving: `openai-chat` (81 routes, 4082 model-route pairs), `anthropic` (28), `openai-responses` (14), `openai-codex` (10), `google-cca` (3), `google-genai` (2), `google-vertex` (2), `bedrock-converse` (2), `local` (2), `bedrock-mantle`, `cursor`, `devin`, `gitlab-duo`, `ollama` (1 each). One is `parallel-extract` and 18 are web-search codecs (`search-*`). Source modules: `crates/ai/src/codec/*.rs` (the same set plus `openai_embedding`, `openai_media`, `openai_realtime`, `omp_native`, `native`).

| Dim | Finding | Maturity |
|---|---|---|
| T01 OpenAI Chat | `codec/openai_chat.rs` (4981 lines), 81 routes. | Implemented |
| T02 OpenAI Responses | `codec/openai_responses.rs`, 14 routes; ChatGPT backend as `openai-codex` (10 routes, HTTP or WebSocket per `codex_transport`). | Implemented |
| T03 Anthropic | `codec/anthropic.rs`, 28 routes, includes a Vertex error path (`anthropic.rs:3339`). | Implemented |
| T04 Gemini | `google-genai`, `google-vertex`, `google-cca` (Gemini CLI and Antigravity profiles, `CodecProfile`, `provider.rs:67`). | Implemented |
| T05 provider-specific | Bedrock Converse and Mantle, Cursor (Connect framing), Devin, GitLab Duo workflow, Ollama chat, Apple FM and candle local routes, `omp_native`. | Implemented |
| T06 reasoning | Catalog `ThinkingPolicy` per model plus sans-I/O recovery modules `recovery/reasoning.rs`, `thinking.rs`, `harmony.rs`; `ChatEvent::ThinkingDelta`. | Implemented |
| T07 tool-call correlation | `ToolCallId`, `ToolCallStarted`, `ToolArgumentsDelta` (display only) and `ToolCallReady`, the sole execution authorization (`event.rs:203-224`); validation in `recovery/tools.rs` (ADR 0020). | Implemented |
| T08 stream normalization | Frames to `RawEvent` to `ChatEvent`; no restart or rollback event by design (`event.rs:173-176`). | Implemented |
| T09 multimodal | Image input capabilities and media operations (`openai_media.rs`, `operation/{image,video,speech,transcription}.rs`). Audio and video input per provider not verified. | Partial |
| T10 structured output | `recovery/json.rs`, `ErrorKind::StructuredOutputFailure`, `structured_output_valid` flag (`layer/mod.rs:108`); ADR 0021 status is Partial in `docs/adr/STATUS.md:39`. | Partial |
| T11 error normalization | One `Error` with kind, phase, action, receipt (section 2, S13). | Implemented |
| T12 architecture | Canonical IR, as above. | Implemented |

Locked-deviation tension: `serde_json::Value` appears 42 times in `codec/openai_chat.rs` and 48 in `codec/cursor.rs` (`grep -c JsonValue`), against "typed serde wire structs, no untyped Value traversal". Whether these are tool-schema pass-through (allowed) or traversal is not verified.

## 4. I, inference interfaces

Transport kinds in the catalog: `Http`, `Websocket`, `Webrtc`, `AwsEventStream`, `Connect`, `Local` (`catalog/src/provider.rs:35`). Routes per kind in the snapshot: Http 165, Websocket 1, Connect 2, AwsEventStream 2, Local 1, Webrtc 0 (measurement 11).

| Dim | Finding | Maturity |
|---|---|---|
| I01 HTTP | One pooled hyper client over rustls, HTTP/1.1 and HTTP/2 (`transport/http.rs:318-336`, module doc `:1`). | Implemented |
| I02 SSE | `transport/sse.rs`, `ndjson.rs`; `stream_protocol` values `sse_events`, `sse_data`, `connect`, `ndjson` in `providers.toml`. | Implemented |
| I03 WebSocket | `transport/websocket_transport.rs`, `websocket.rs`; two providers set `codex_transport = "websocket-preferred"`; `openai_realtime` codec. | Implemented |
| I04 persistent sessions | `ai/src/session` (server-state binding, revisions), `ServerStatePolicy` (`catalog/src/model.rs:46`), `RetryAction::ReseedSession`. | Implemented |
| I05 gRPC | Served, not consumed: `service Inference` (`crates/proto/proto/omp/inference/v1/inference.proto:39`) is projected by tonic in `crates/serve/src/inference.rs`. No provider codec speaks gRPC (`grep -il grpc crates/ai/src` hits only `codec/cursor.rs`, `codec/omp_native.rs`). `tonic` users (Cargo.lock reverse deps): omp-app, omp-driver, omp-e2e, omp-envd, omp-exec-reapi, omp-proto, omp-rpc, omp-serve; not `omp-ai`. | Partial |
| I07 AWS EventStream | `transport/eventstream.rs` (CRC and header validation) with 2 `bedrock-converse` routes. | Implemented |
| I08 bidirectional streaming | gRPC `Turn(stream TurnFrame) returns (stream TurnEvent)` and `Realtime(stream)` (`inference.proto:48,58`); WebSocket realtime. | Implemented |
| I09 WebRTC | `webrtc` is `optional` in `crates/ai/Cargo.toml` (dependency list and `target.cfg` block), enabled only by feature `realtime` (`Cargo.toml:40-46`, with `opus`, `audiopus_sys`, `device-attestation`). `omp-app` enables it unconditionally (`crates/app/Cargo.toml:94`) and uses it: `omp_ai::realtime::transport` in `app/src/chat_control.rs:25`, `chat_voice.rs:25`, `live_reachability.rs:9`. `omp-ai` is the only direct dependent of `webrtc` in `Cargo.lock`. No catalog route has transport `Webrtc` (0 of 171). So the AGENTS.md example of an unused `app -> omp-ai/realtime -> WebRTC` edge is no longer accurate on this commit: the code is imported. The cost remains: every `omp-app` build compiles WebRTC, DTLS and Opus. Provider use case (which hosted realtime API) not verified. | Implemented |
| I10 HTTP/3, QUIC | `cargo tree --offline --locked --workspace -i quinn` and `-i h3` both answer "did not match any packages". `Cargo.lock` lists `quinn` only as a dependency of `reqwest` (optional feature), not compiled. No QUIC code in `crates/ai/src/transport`. | Absent |
| I11 async jobs | `operation/job.rs` and video generation (`operation/video.rs`); semantics not read. | Partial |
| I12 batch APIs | grep for `/batches`, `batch_api`, `BatchJob`, `message_batches` in `crates/ai/src` and `crates/serve/src`: no hits. | Absent |
| I13 native execution | `transport` kind `Local`; `crates/ai/src/local` (Apple FM, candle STT and TTS, fastembed behind features `local-*`). | Implemented |
| I15 external harness protocols | ACP client and Codex app-server are delegated executors, outside `omp-ai`: `crates/driver/src/delegated_acp.rs`, `crates/envd/src/acp_client/`; ADR 0043 is Partially implemented (`STATUS.md:61`). Not read in depth. | Partial |

## 5. E, token and resource efficiency

| Dim | Finding | Maturity |
|---|---|---|
| E01 prompt-cache awareness | `PromptCacheCapabilities` (`capability.rs:446`), `PrefixCachePolicy` (`model.rs:35`), `RouteRestrictions.disable_prompt_caching`; scribe slots keep volatile facts out of the stable prefix (AGENTS.md). Reduces billable tokens and latency. | Implemented |
| E02 affinity for cache reuse | Account stickiness (S05) keeps one credential, which keeps provider-side prefix caches reachable; route level affinity is unset. No cache-key routing. | Partial |
| E03 incremental chaining | `previous_response_id` handling in `codec/openai_responses.rs`, `openai_codex.rs`, `provider/builtin.rs`; `ServerStatePolicy` (`model.rs:46`). Reduces network transmission. | Implemented |
| E04 provider-side state | `ServerStateBinding` (`session/mod.rs`), `ErrorKind::SessionExpired`/`SessionConflict`, `RetryAction::ReseedSession`. | Implemented |
| E05 retransmission handling | Reseed by replaying canonical history; `ContextStrategy` (`model.rs:58`), `ModelRemoteCompaction` (`model.rs:69`), `context_promotion_target` and `ContextOverflow` fallback (`registry.rs:816-829`). | Implemented |
| E06 semantic compression | Crates `omp-snapcompact` and `omp-minimizer` exist and `ModelSpec.compaction_model` is catalog data; not read. | Partial |
| E07 batch inference | Absent (I12 search). | Absent |
| E08 connection pooling | `pooled_client()` (`transport/http.rs:336`), HTTP/2 enabled (`:318`). Pool sizing and idle policy not read. | Implemented |
| E09 stream backpressure | Bounded `flume` queue with `RealtimeSessionError::Backpressure` (`operation/realtime.rs:169`), `ErrorKind::PolicyBufferExceeded`, `provisional_bytes` counter (`layer/mod.rs:93`). HTTP body backpressure not verified. | Partial |
| E10 token accounting | `Usage` in receipts and `ExecutionContext` atomics (`layer/mod.rs:91-92`), `ChatEvent::Usage`, OTel histogram `gen_ai.client.token.usage` and attribute `omp.gen_ai.usage.accuracy` (`observability/src/metrics.rs:73,136`). | Implemented |
| E11 cache accounting | `cache_read_tokens`, `cache_write_tokens`, `cache_write_1h_tokens` (`ai/src/receipt.rs:147-154`). | Implemented |
| E12 TTFT | `ttft_ms` on the model-request span, exported as `gen_ai.response.time_to_first_chunk` (`observability/src/span.rs:123,430`), also set by `serve/src/inference.rs:2661`. Not used for routing (S08). | Implemented |

## 6. O, observability

| Dim | Finding | Maturity |
|---|---|---|
| O01 provider health | No health endpoint or probe of inference providers. `crates/roster-probe` is a tool-roster capability probe (`roster-probe/src/lib.rs:1-6`), unrelated to provider health. Discovery keeps `ProviderDiscoveryState` and `ProviderLifecycle` (`discovery/store.rs:66,81`); `RouteHealth` is unpopulated (S03). | Partial |
| O03 metrics | Counters and histograms: tokens, chat calls, chat duration, errors, cost (`observability/src/metrics.rs:73-113`). No queue-latency metric found (grep `queue`/`waiting` in `metrics.rs`). | Partial |
| O04 cost accounting | `Cost` in receipts, `cost_micro_usd` (`layer/mod.rs:95`), `omp.agent.chat.cost.estimated_usd` (`metrics.rs:78`), `catalog/src/pricing.rs`. | Implemented |
| O05 structured events | `ExecutionReceipt` with hidden and visible attempts, `provider_request` tracing spans with provider, model, route, attempt (`registry.rs:785-792`), fallback warnings (`registry.rs:834`). | Implemented |

## 7. ADR divergences

ADR rows in `docs/adr/STATUS.md` were last verified 2026-10-04 at `083b38fe7d`, not at the audited commit.

| ADR | STATUS row | Code at `85d219ae7f` |
|---|---|---|
| 0017 compiled compat knowledge | Implemented (`STATUS.md:35`) | Consistent: `compat/*.kdl` cascade (`catalog/src/cascade.rs`). Minor: provider-id literals in `compile.rs`. |
| 0018 provider is shared infrastructure | Implemented (`:36`) | Retry, auth, admission, usage are in the stack. The routing part is thinner than the title implies: health, quota and latency ranking inputs have no production producer (S03, S08). |
| 0019 forced-call escalation | Implemented (`:37`) | `forced_call_ladder` present (`plan.rs:267`). |
| 0020 charitable repair | Implemented (`:38`) | `recovery/tools.rs`, `dialect.rs` present. |
| 0021 constrained sampling | Partially implemented (`:39`) | Consistent with T10 Partial. |
| 0022 one canonical turn | Implemented (`:40`) | Consistent (`RawEvent`, `ChatEvent`). |
| 0043 native CLI | Partially implemented (`:61`) | ACP and Codex app-server live outside `omp-ai`. |
| 0059 catalog additions | Proposed, S1 and S2 landed (`:77`) | Not re-verified beyond catalog size. |
| 0074 Anthropic advisor tool | Proposed, not started (`:92`) | Not implemented. Tension with the locked "no vendor server-side tools" line (AGENTS.md ~lines 526-582): the ADR itself is opt-in and codec-scoped; needs an owner ruling, so any card touching it is `Requires ADR reconsideration`. |

## 8. Not verified

1. Whether `RouteHealth`, quota and latency evidence are produced anywhere outside `crates/` (Python extensions, `crates/py`); only a Rust workspace grep was done.
2. Whether usage fetched by `operation/usage/*` reaches `AccountPool` through a path the grep missed (dynamic dispatch, events via `AccountPoolEvent`).
3. Behaviour under load of admission, retry and fallback; no load test was run, no timing measured.
4. Which hosted realtime API the WebRTC path serves (`realtime/live.rs`) and whether it is reachable from a default `omp` build.
5. Whether the 42 and 48 `JsonValue` uses in `openai_chat.rs` and `cursor.rs` violate the typed-wire rule.
6. Pool sizing, idle timeout and HTTP/2 stream limits of `pooled_client`.
7. Async job semantics (`operation/job.rs`), multimodal audio and video input per provider, and `omp-snapcompact` behaviour.
8. `crates/serve` consumer surface: which clients call `Inference` over gRPC, and whether anything serves OpenAI-compatible HTTP (grep for `chat/completions` in `serve`, `driver`, `app` found none).
9. The `.plan/parity-regression-audit.md` ledger named in AGENTS.md is absent from a clean clone.
10. Cross-check of the 125 `providers.toml` entries against compiled providers: both are 125, but per-provider field equality was not compared.

Back to the [index](../README.md).
