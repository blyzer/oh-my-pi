# OMP2 existing versus missing capability matrix

> Part of the [OMP2 gateway-capabilities research](README.md). Deliverable 4. Audited commit `85d219ae7f` (`origin/omp2`). Research proposal: nothing here is implemented.

**How to read it.** Rows use the dimension IDs of [`evidence/01-method-and-dimension-ids.md`](evidence/01-method-and-dimension-ids.md). Status is one of Implemented (code on `omp2` reachable from a production path), Partial, Prototype (code exists but is unwired, test-only or opt-in), Proposed (ADR text only), Unmerged, Absent (searched, not found). Evidence is `path:line` plus symbol at the audited commit. **Implemented means the code exists and is wired as cited; it does not mean it was exercised, tested or measured.** Nothing in this table is a measurement.

**Source.** Each row is copied from the area audit named in the last column (`evidence/omp2-*.tsv`), written by the agent that read that area. Rows that two areas both answered appear twice. Decision classes for the gaps are assigned in the area documents (5 to 11) and collected in the [backlog](15-prioritized-architectural-backlog.md); this table is status only.


## G: provider registry and discovery

11 rows audited: 6 Implemented, 4 Partial, 1 Prototype.

| ID | Dimension | Status | Evidence | Note | Audit |
|---|---|---|---|---|---|
| G01 | canonical provider identity | Implemented | crates/catalog/src/id.rs:6 ProviderId; crates/catalog/src/provider.rs:698 ProviderDef | ProviderId newtype; 125 providers and 171 routes in the embedded snapshot (measurement 11) | [provider](evidence/omp2-provider-runtime.md) |
| G02 | canonical model identity | Implemented | crates/catalog/src/id.rs:40 provider_scoped; crates/catalog/src/model.rs:285 PolicyModel | ModelKey is provider/model; opaque WireModelId; PolicyModel hides wire ids; 5289 models | [provider](evidence/omp2-provider-runtime.md) |
| G03 | model aliases | Implemented | crates/catalog/src/compile.rs:1048 CatalogAlias; crates/catalog/src/snapshot.rs:716 resolve_alias | 230 exact provider-scoped aliases; no cross-provider virtual model, fallback chains instead | [provider](evidence/omp2-provider-runtime.md) |
| G04 | provider-specific deployments | Partial | crates/catalog/src/model.rs:236 ModelSpec.wire_ids | Per-route wire ids and templated Azure deployment URL; no per-account deployment inventory | [provider](evidence/omp2-provider-runtime.md) |
| G05 | dynamic discovery | Implemented | crates/ai/src/discovery/probe.rs:79 DiscoveryProbe; crates/driver/src/registry.rs:847 DiscoveryRefresher::spawn | Probe, SQLite TTL store and session refresh owner with atomic registry swap | [provider](evidence/omp2-provider-runtime.md) |
| G06 | hosted model catalogs | Prototype | crates/catalog/src/snapshot.rs:236 additive_shared_catalog_overlay | Shared-catalog overlay compiler has no production caller, only a test | [provider](evidence/omp2-provider-runtime.md) |
| G07 | catalog synchronization | Partial | crates/driver/src/registry.rs:451 production_catalog; crates/catalog/src/snapshot.rs:529 with_overlay_stack | Pinned offline snapshot plus runtime overlay layers; no remote catalog pull | [provider](evidence/omp2-provider-runtime.md) |
| G08 | provider capability metadata | Implemented | crates/catalog/src/capability.rs:181 Availability; crates/ai/src/plan.rs:570 negotiate | Tri-state Availability per capability, negotiated per route with receipt evidence | [provider](evidence/omp2-provider-runtime.md) |
| G09 | authentication requirements declared per provider | Implemented | crates/catalog/src/provider.rs:95 AuthSpecKind; crates/ai/src/auth/spec.rs:13 AuthSpec; crates/ai/src/auth/broker.rs provider_auth_specs | AuthSpecKind (11 kinds) and per-route AuthSpec are catalog data; provider_auth_specs lists leased auths; login refuses unleased kinds | [auth](evidence/omp2-auth-credentials.md) |
| G10 | free-tier and anonymous discovery | Partial | crates/catalog/src/provider.rs:104 OptionalBearer; crates/driver/src/registry.rs:363 ensure_stored_logins_unlockable | AuthSpecKind None and OptionalBearer permit anonymous use; no free-tier flag found in catalog data (searched provider.rs) | [auth](evidence/omp2-auth-credentials.md) |
| G11 | connection identity and deduplication | Partial | crates/ai/src/account/pool.rs:440 ProviderRebind; crates/ai/src/auth/store.rs:55 CredentialMetadata principal_id; crates/ai/src/account/pin.rs:1 CredentialAffinityDigest | AccountId and PrincipalId bind one provider and principal; digests in journal; dedup of one upstream identity not verified | [auth](evidence/omp2-auth-credentials.md) |

## A: authentication and credentials

12 rows audited: 7 Partial, 5 Implemented.

| ID | Dimension | Status | Evidence | Note | Audit |
|---|---|---|---|---|---|
| A01 | API keys | Implemented | crates/ai/src/auth/spec.rs:440 CredentialSourceSpec; crates/ai/src/auth/store.rs:591 put_inner EphemeralCredential; crates/ai/src/auth/command.rs:54 CommandCredentialResolver | Catalog-ordered env, stored and command sources; env keys never persisted; stored keys sealed with kind authenticated | [auth](evidence/omp2-auth-credentials.md) |
| A02 | OAuth | Implemented | crates/ai/src/auth/oauth.rs:236 OAuthEngine; crates/ai/src/auth/oauth.rs:104 OAuthCustomHandler; crates/oauth/src/lib.rs:1 module doc | PKCE, paste and typed custom exchanges in ai/auth/oauth; wire primitives in omp-oauth with no credential selection | [auth](evidence/omp2-auth-credentials.md) |
| A03 | device authorization | Implemented | crates/ai/src/auth/oauth.rs:387 begin_device; crates/ai/src/auth/oauth.rs:452 poll_device; crates/catalog/src/provider.rs:364 OAuthFlowSpec | RFC 8628 device grant engine and catalog DeviceCode spec; which providers use it was not enumerated | [auth](evidence/omp2-auth-credentials.md) |
| A04 | official CLI delegation | Partial | crates/driver/src/delegated_acp.rs:47 SV_DELEGATED_DSH_PROGRAM; crates/envd/src/acp_client/delegated_session.rs:790 AuthRequired; docs/adr/0043-native-cli-integration.md:130 | ACP delegation (dsh, Devin, Grok) only when operator names a program; no token extraction; login Unknown; Codex adapter not read | [auth](evidence/omp2-auth-credentials.md) |
| A05 | credential references (secret never inline) | Implemented | crates/ai/src/auth/lease.rs:155 CredentialLease; crates/driver/src/headless/kernel.rs:2849 bind_placement; crates/driver/src/secrets/mod.rs:70 credential_secret_control_factory | Catalog holds names and placements only; leases sealed; extensions name a provider id; omp.creds CONTROL domain unbound in prod | [auth](evidence/omp2-auth-credentials.md) |
| A06 | OS keychain | Partial | crates/ai/src/auth/key.rs:416 OsCredentialKeySource; crates/driver/src/registry.rs:212 CredentialKeyMode; crates/driver/src/registry.rs:125 KEYCHAIN_SERVICE | macOS keychain holds the master key only; other OS Absent; default owner-only key file; auto fails closed when unattended | [auth](evidence/omp2-auth-credentials.md) |
| A07 | refresh coordination | Partial | crates/ai/src/account/refresh.rs:381 FLIGHTS; crates/driver/src/registry.rs:1381 RefreshCoordinator::new; crates/ai/src/auth/store.rs:1196 try_acquire_lease held_owner != owner | In-process single flight plus SQLite lease and CAS fence; prod owner token is constant omp-auth-refresh so cross-process fence is inert | [auth](evidence/omp2-auth-credentials.md) |
| A08 | credential expiration handling | Implemented | crates/ai/src/auth/manager.rs:1314 RefreshingCredentialSource; crates/ai/src/auth/manager.rs:1550 set_enabled false; crates/ai/src/auth/store.rs:1323 lease_now | 60 s skew and typed Expired, NotRenewable, RefreshFailed; extension refresh failure disables account, built-in path does not | [auth](evidence/omp2-auth-credentials.md) |
| A09 | credential recovery | Partial | crates/app/src/cli.rs:2168 AuthCommand; crates/ai/src/auth/manager.rs:416 repair_static_secret_kinds; crates/app/src/auth_broker_cmd.rs:81 import | auth login, refresh, logout, key rotation, kind repair, v1 and CLIProxyAPI import exist; no auto re-login and no secret backup | [auth](evidence/omp2-auth-credentials.md) |
| A10 | independent credential keepers | Partial | crates/app/src/auth_broker_cmd.rs:63 Serve; crates/app/src/daemon.rs:417 UnauthenticatedTcp; crates/driver/src/auth_backend.rs:633 GatewayCredentialSecretControlFactory | auth-broker serve and --gateway exist (UDS or TCP with bearer); refresh is lazy; remote creds CONTROL client is test-only | [auth](evidence/omp2-auth-credentials.md) |
| A11 | authorized subscription entitlements | Partial | crates/proto/proto/omp/auth/v1/auth.proto:86 GetUsage; crates/ai/src/account/pool.rs:76 CooldownReason::ModelPolicy; crates/ai/src/auth/mod.rs:55 discover_copilot_api_endpoint | Usage RPCs, plan discovery and ModelPolicy cooldown learned after a denial; no declared per-account entitlement set before first request | [auth](evidence/omp2-auth-credentials.md) |
| A12 | account and regional restrictions | Partial | crates/ai/src/auth/oauth.rs:56 CODEX_DATA_RESIDENCY_CLAIM; crates/ai/src/auth/mod.rs:28 ALIBABA_TOKEN_PLAN_CN_BASE_URL; crates/ai/src/account/pool.rs:86 Eligibility | Region lives in endpoints and claims (Codex residency, Alibaba CN, Copilot enterprise); Eligibility has no region variant | [auth](evidence/omp2-auth-credentials.md) |

## S: scheduling and routing

14 rows audited: 7 Implemented, 5 Partial, 1 Prototype, 1 Absent.

| ID | Dimension | Status | Evidence | Note | Audit |
|---|---|---|---|---|---|
| S01 | deterministic provider selection | Implemented | crates/ai/src/router.rs:1277 compare_evaluated | Total deterministic order: rank, affinity, health, quota, latency, price, ids | [provider](evidence/omp2-provider-runtime.md) |
| S02 | credential eligibility | Implemented | crates/ai/src/account/pool.rs:86 Eligibility; crates/ai/src/account/pool.rs:232 AccountPin; crates/ai/src/account/pool.rs:212 RotationPolicy | Eligibility has 12 variants, exclusive pins, rotation policy and quota reserve; selection itself belongs to the routing audit | [auth](evidence/omp2-auth-credentials.md) |
| S03 | availability tracking | Partial | crates/ai/src/router.rs:785 default RuntimeRouteEvidence; crates/ai/src/plan.rs:388 RouteHealth | RouteHealth exists but production Router never supplies evidence, so health is Unknown | [provider](evidence/omp2-provider-runtime.md) |
| S04 | rate-limit awareness | Partial | crates/ai/src/provider/builtin.rs:2721 PoolRateLimiter; crates/ai/src/auth/manager.rs:552 report_block | Rate windows and limiter exist; only report_block and 429 helpers write them, no header observer | [provider](evidence/omp2-provider-runtime.md) |
| S05 | session affinity | Partial | crates/ai/src/account/pool.rs:932 select; crates/ai/src/router.rs:285 | Account and principal affinity and pin work; route-level affinity_route always None | [provider](evidence/omp2-provider-runtime.md) |
| S06 | capability-aware routing | Implemented | crates/ai/src/plan.rs:570 negotiate; crates/catalog/src/provider.rs:721 RouteRestrictions | Required versus preferred features negotiated; route restrictions; forced-call ladder | [provider](evidence/omp2-provider-runtime.md) |
| S07 | cost-aware routing | Partial | crates/ai/src/router.rs:1346 price_score; crates/ai/src/layer/budget.rs | Price is a late tiebreaker summing mixed units; per-call budget ledger exists | [provider](evidence/omp2-provider-runtime.md) |
| S08 | latency-aware routing | Prototype | crates/ai/src/router.rs:1302; crates/ai/src/plan.rs:410 latency | Latency is ranked but never populated (Duration::MAX) in production | [provider](evidence/omp2-provider-runtime.md) |
| S09 | circuit breakers | Absent | crates/ai/src/account/pool.rs:632 cooldown (closest, no production writer) | No breaker or half-open logic in ai, serve, catalog, http; grep circuit, breaker, half_open | [provider](evidence/omp2-provider-runtime.md) |
| S10 | backpressure | Implemented | crates/ai/src/layer/admission.rs:24 AdmissionController; crates/ai/src/settings.rs:412 | Per-provider in-flight cap and bounded queue (default 64) failing fast | [provider](evidence/omp2-provider-runtime.md) |
| S11 | cancellation | Implemented | crates/ai/src/layer/mod.rs:516 checkpoint; crates/ai/src/codec/mod.rs:957 Cancellation | Cancel flag plus Notify, checkpoint between phases, transport Cancellation handle | [provider](evidence/omp2-provider-runtime.md) |
| S12 | retry policies | Implemented | crates/ai/src/layer/retry.rs:97 TransportRetryService; crates/ai/src/registry.rs:874 fallback_is_safe | Same-route jittered retry pre-commit, credential re-entry, replay-gated cross-route fallback | [provider](evidence/omp2-provider-runtime.md) |
| S13 | failure classification | Implemented | crates/ai/src/error.rs:26 ErrorKind; crates/ai/src/transport/http.rs:1273 classify_http_error_with_hint | 43 ErrorKind variants, RetryAction, deterministic HTTP status and body table | [provider](evidence/omp2-provider-runtime.md) |
| S14 | quota-aware scheduling | Partial | crates/ai/src/account/pool.rs:134 QuotaReservePolicy; crates/ai/src/operation/usage.rs | Reserve policies and 15 usage fetchers exist; no writer of fetched usage into the pool found | [provider](evidence/omp2-provider-runtime.md) |

## T: protocol translation

12 rows audited: 10 Implemented, 2 Partial.

| ID | Dimension | Status | Evidence | Note | Audit |
|---|---|---|---|---|---|
| T01 | OpenAI Chat Completions | Implemented | crates/ai/src/codec/openai_chat.rs:2595 classify_error | openai-chat codec, 81 routes | [provider](evidence/omp2-provider-runtime.md) |
| T02 | OpenAI Responses | Implemented | crates/ai/src/codec/openai_responses.rs:3764; crates/ai/src/codec/openai_codex.rs:590 | openai-responses 14 routes plus openai-codex 10 routes | [provider](evidence/omp2-provider-runtime.md) |
| T03 | Anthropic Messages | Implemented | crates/ai/src/codec/anthropic.rs:3326 classify_http_error | anthropic codec, 28 routes | [provider](evidence/omp2-provider-runtime.md) |
| T04 | Gemini | Implemented | crates/ai/src/codec/gemini.rs:3562; crates/catalog/src/provider.rs:67 CodecProfile | google-genai, google-vertex, google-cca with CodecProfile variants | [provider](evidence/omp2-provider-runtime.md) |
| T05 | provider-specific protocols | Implemented | crates/ai/src/codec/mod.rs:43-71 codec modules | Bedrock, Cursor Connect, Devin, GitLab Duo, Ollama, local, omp_native codecs | [provider](evidence/omp2-provider-runtime.md) |
| T06 | reasoning normalization | Implemented | crates/ai/src/recovery/reasoning.rs; crates/ai/src/recovery/thinking.rs | Catalog ThinkingPolicy plus sans-I/O recovery reasoning and thinking modules | [provider](evidence/omp2-provider-runtime.md) |
| T07 | tool-call correlation | Implemented | crates/ai/src/event.rs:203-224 ChatEvent tool variants | ToolCallReady is the sole execution authorization; deltas are display-only | [provider](evidence/omp2-provider-runtime.md) |
| T08 | streaming event normalization | Implemented | crates/ai/src/codec/mod.rs:844 Decoder; crates/ai/src/event.rs:178 ChatEvent | Decoder emits RawEvent which maps to canonical ChatEvent; no rollback event | [provider](evidence/omp2-provider-runtime.md) |
| T09 | multimodal content | Partial | crates/ai/src/codec/openai_media.rs; crates/ai/src/operation/image.rs | Image input capability and media operations; audio and video input per provider not verified | [provider](evidence/omp2-provider-runtime.md) |
| T10 | structured output | Partial | crates/ai/src/recovery/json.rs; docs/adr/STATUS.md:39 | JSON recovery and StructuredOutputFailure; ADR 0021 status is Partial | [provider](evidence/omp2-provider-runtime.md) |
| T11 | error normalization | Implemented | crates/ai/src/error.rs:696 Error | One Error type with kind, phase, retry action and receipt | [provider](evidence/omp2-provider-runtime.md) |
| T12 | translation architecture (pairwise translators versus can... | Implemented | crates/ai/src/codec/mod.rs:918 Codec trait | Canonical IR with one sans-I/O codec per wire protocol; no pairwise wire translators | [provider](evidence/omp2-provider-runtime.md) |

## I: inference interfaces

15 rows audited: 11 Implemented, 2 Absent, 2 Partial.

| ID | Dimension | Status | Evidence | Note | Audit |
|---|---|---|---|---|---|
| I01 | HTTP | Implemented | crates/ai/src/transport/http.rs:336 pooled_client | Pooled hyper client over rustls, HTTP/1.1 and HTTP/2; 165 of 171 routes | [provider](evidence/omp2-provider-runtime.md) |
| I02 | SSE | Implemented | crates/ai/src/transport/sse.rs; crates/ai/src/transport/ndjson.rs | SSE and NDJSON framing; stream_protocol values in providers.toml | [provider](evidence/omp2-provider-runtime.md) |
| I03 | WebSocket | Implemented | crates/ai/src/transport/websocket_transport.rs; fixtures/llm-oracle/catalog/providers.toml openai-codex | WebSocket transport; 1 route Websocket, 2 providers websocket-preferred | [provider](evidence/omp2-provider-runtime.md) |
| I04 | persistent inference sessions | Implemented | crates/ai/src/session/mod.rs; crates/catalog/src/model.rs:46 ServerStatePolicy | Server-state binding and revisions; ReseedSession recovery | [provider](evidence/omp2-provider-runtime.md) |
| I05 | gRPC | Implemented | crates/app/src/daemon.rs:63 RPC_MESSAGE_MAX_BYTES | gRPC with explicit 64 MiB message limits | [ipc](evidence/omp2-ipc-serialization.md) |
| I05 | gRPC | Partial | crates/proto/proto/omp/inference/v1/inference.proto:39; crates/serve/Cargo.toml:33 | gRPC Inference service is served by omp-serve via tonic; no provider codec uses gRPC | [provider](evidence/omp2-provider-runtime.md) |
| I06 | Protobuf | Implemented | crates/proto/build.rs:64 compile_fds | Protobuf is the process and network contract | [ipc](evidence/omp2-ipc-serialization.md) |
| I07 | AWS EventStream | Implemented | crates/ai/src/transport/eventstream.rs | Bounded EventStream framing with CRC; 2 bedrock-converse routes | [provider](evidence/omp2-provider-runtime.md) |
| I08 | bidirectional streaming | Implemented | crates/proto/proto/omp/inference/v1/inference.proto:48 Turn | Turn and Realtime are bidirectional streaming RPCs; Blob.Put is client streaming | [ipc](evidence/omp2-ipc-serialization.md) |
| I08 | bidirectional streaming | Implemented | crates/proto/proto/omp/inference/v1/inference.proto:48 | gRPC Turn and Realtime bidi streams; WebSocket realtime | [provider](evidence/omp2-provider-runtime.md) |
| I09 | WebRTC | Implemented | crates/ai/Cargo.toml:40; crates/app/Cargo.toml:94; crates/app/src/chat_voice.rs:25 | webrtc behind omp-ai realtime feature; omp-app enables it and imports it; 0 catalog routes use Webrtc | [provider](evidence/omp2-provider-runtime.md) |
| I10 | HTTP/3 and QUIC | Absent | Cargo.lock reqwest dependencies; cargo tree -i quinn | quinn and h3 not in resolved graph; quinn only in Cargo.lock under reqwest optional | [provider](evidence/omp2-provider-runtime.md) |
| I11 | asynchronous inference jobs | Partial | crates/ai/src/operation/job.rs; crates/ai/src/operation/video.rs | Job operation and video generation exist; semantics not read | [provider](evidence/omp2-provider-runtime.md) |
| I12 | batch APIs | Absent | crates/ai/src/operation (searched) | No batch API code in ai or serve; grep batches, batch_api, BatchJob, message_batches | [provider](evidence/omp2-provider-runtime.md) |
| I13 | native execution | Implemented | crates/ai/src/local; crates/ai/Cargo.toml:16 | Local transport kind; Apple FM and candle routes behind local features | [provider](evidence/omp2-provider-runtime.md) |
| I14 | local IPC | Implemented | crates/envd/src/exthost/spawn.rs:545 UnixStream | UDS or named pipe for envd, socketpair for CONTROL, piped stdio for workers | [ipc](evidence/omp2-ipc-serialization.md) |
| I15 | external agent-harness protocols | Partial | crates/driver/src/delegated_acp.rs; docs/adr/STATUS.md:61 | ACP and Codex delegated executors live outside omp-ai; ADR 0043 Partially implemented | [provider](evidence/omp2-provider-runtime.md) |

## E: token and resource efficiency

12 rows audited: 8 Implemented, 3 Partial, 1 Absent.

| ID | Dimension | Status | Evidence | Note | Audit |
|---|---|---|---|---|---|
| E01 | prompt-cache awareness | Implemented | crates/catalog/src/capability.rs:446; crates/catalog/src/model.rs:35 | Prompt cache capabilities, prefix cache policy, route override to disable caching | [provider](evidence/omp2-provider-runtime.md) |
| E02 | session affinity for cache reuse | Partial | crates/ai/src/account/pool.rs:974 affinity_rank | Account stickiness keeps caches reachable; no cache-key or route affinity | [provider](evidence/omp2-provider-runtime.md) |
| E03 | incremental response chaining | Implemented | crates/ai/src/codec/openai_responses.rs; crates/catalog/src/model.rs:46 | previous_response_id chaining in Responses and Codex codecs | [provider](evidence/omp2-provider-runtime.md) |
| E04 | provider-side state | Implemented | crates/ai/src/session/mod.rs; crates/ai/src/error.rs:26 | ServerStateBinding with SessionExpired and SessionConflict errors | [provider](evidence/omp2-provider-runtime.md) |
| E05 | context retransmission handling | Implemented | crates/catalog/src/model.rs:58; crates/ai/src/registry.rs:816 | Canonical history reseed, ContextStrategy, remote compaction, context promotion fallback | [provider](evidence/omp2-provider-runtime.md) |
| E06 | semantic compression | Partial | crates/catalog/src/model.rs:236 ModelSpec.compaction_model | Compaction model is catalog data; snapcompact and minimizer crates not read | [provider](evidence/omp2-provider-runtime.md) |
| E07 | batch inference | Absent | crates/ai/src/operation (searched) | No batch inference path; same search as I12 | [provider](evidence/omp2-provider-runtime.md) |
| E08 | connection pooling | Implemented | crates/ai/src/transport/http.rs:318 | One pooled client with HTTP/2; sizing and idle policy not read | [provider](evidence/omp2-provider-runtime.md) |
| E09 | streaming backpressure | Partial | crates/ai/src/operation/realtime.rs:169; crates/ai/src/layer/mod.rs:93 | Bounded realtime queue with Backpressure error and buffer ceiling; HTTP path not verified | [provider](evidence/omp2-provider-runtime.md) |
| E10 | token accounting | Implemented | crates/ai/src/layer/mod.rs:91; crates/observability/src/metrics.rs:73 | Usage in receipts, atomics in ExecutionContext, OTel token histogram | [provider](evidence/omp2-provider-runtime.md) |
| E11 | cache accounting | Implemented | crates/ai/src/receipt.rs:147 | cache_read, cache_write and one-hour cache write token counters | [provider](evidence/omp2-provider-runtime.md) |
| E12 | time-to-first-token and latency measurement | Implemented | crates/observability/src/span.rs:123; crates/serve/src/inference.rs:2661 | ttft_ms on span exported as time_to_first_chunk; not used for routing | [provider](evidence/omp2-provider-runtime.md) |

## X: extension management

11 rows audited: 6 Partial, 5 Implemented.

| ID | Dimension | Status | Evidence | Note | Audit |
|---|---|---|---|---|---|
| X01 | plugin mechanism (state which: native shared library, chi... | Implemented | crates/envd/src/exthost/spawn.rs:46 EXT_HOST_ARG; crates/envd/src/exthost/spawn.rs:746 run_ext_host_entry; crates/app/src/main.rs:119 EXT_HOST_ARG dispatch; crates/py/python/omp/_host.py:1 bootstrap; crates/ext/src/config.rs:1963 content rows; docs/adr/0036-python-for-extensions.md:40 no second l... | Child process: same-binary re-entry with embedded CPython; also declarative rows and MCP. No native lib, Wasm or static-crate plugin | [ext](evidence/omp2-extensions.md) |
| X01 | plugin mechanism (state which: native shared library, chi... | Implemented | crates/envd/src/exthost/spawn.rs:746 run_ext_host_entry;crates/py/src/lib.rs:179 init | Plugin mechanism: killable child processes embedding static CPython 3.14t; no JS/Wasm/native-lib runtime | [python](evidence/omp2-python-runtime.md) |
| X02 | capability manifest | Partial | crates/ext/src/config.rs:1360 DeploymentManifest; crates/ext/src/config.rs:1482 StaticDeclaration; crates/ext/src/config.rs:1886 manifest_capability_digest; crates/ext/src/config.rs:1918 validate_capability_name; crates/envd/src/policy.rs:37 CAPABILITIES | omp.toml manifest with typed declaration table and digests; capability names only charset-checked in omp-ext; Rust ignores omp_api | [ext](evidence/omp2-extensions.md) |
| X03 | registry and distribution | Partial | crates/ext/src/index.rs:180 SignedIndex; crates/ext/src/config.rs:381 SourceSpec; crates/app/src/ext_cli/mod.rs:279 ExtCommand; crates/env/src/lib.rs:19 AirgapBundle; docs/adr/STATUS.md:54 repository not started | Signed Ed25519 index, 5 source kinds (index, PyPI, git, path, url), full omp ext CLI, air-gap bundle; no hosted official index evidenced | [ext](evidence/omp2-extensions.md) |
| X04 | artifact integrity and signatures | Implemented | crates/ext/src/trust.rs:1251 verify_artifact_signature; crates/ext/src/trust.rs:996 verify_or_pin; crates/ext/src/trust.rs:1076 RevocationsFile; crates/ext/src/index.rs:246 verify_at; crates/ext/src/doctor.rs:338 verify_artifact | BLAKE3+SHA-256 per wheel, Ed25519 signatures, TOFU key pins, signed revocations, index expiry; path/link sources unsigned by design | [ext](evidence/omp2-extensions.md) |
| X05 | permissions and isolation | Partial | crates/ext/src/lib.rs:47 TrustTier; crates/envd/src/exthost/spawn.rs:553 sandbox profile; crates/envd/src/exthost/spawn.rs:578 trusted no sandbox; crates/ext/src/trust.rs:84 Grant; crates/ext/src/credential_grant.rs:223 CredentialGrant; docs/adr/STATUS.md:94 ADR 0076 | Two tiers (sandboxed default, trusted), kernel sandbox, operator and credential grants; capability names coarse; ADR 0076 only Proposed | [ext](evidence/omp2-extensions.md) |
| X05 | permissions and isolation | Partial | crates/envd/src/exthost/spawn.rs:553 sandbox | Sandboxed tier has no network and a scoped read/write list; trusted tier is unsandboxed; eval sandbox optional | [python](evidence/omp2-python-runtime.md) |
| X06 | lazy startup and independent termination | Implemented | crates/envd/src/exthost/lifecycle.rs:558 ActivationTrigger; crates/envd/src/exthost/cancel.rs:15 MAX_KILL_ESCALATIONS_PER_SESSION; crates/envd/src/exthost/cancel.rs:94 kill_after_grace; crates/envd/src/worker.rs:3723 ControlRestartBreaker; crates/envd/src/exthost/spawn.rs:631 setpgid | Lazy triggers; cancel ladder ends in process-group SIGKILL, disable after 2 kills; restart breaker 1s to 30s | [ext](evidence/omp2-extensions.md) |
| X06 | lazy startup and independent termination | Implemented | crates/envd/src/eval/process.rs:569 spawn | Child per host or eval session, kill_on_drop, TERM-grace-KILL; startup latency of embedded child not measured | [python](evidence/omp2-python-runtime.md) |
| X07 | health reporting | Partial | crates/ext/src/doctor.rs:88 RuntimeHealth; crates/ext/src/doctor.rs:110 diagnose; crates/envd/src/worker.rs:1619 publish_host_down; crates/envd/src/exthost/lifecycle.rs:38 AvailabilityBatch | omp ext doctor (integrity, revocation, Environment, credentials) and host-down availability; no periodic liveness probe found | [ext](evidence/omp2-extensions.md) |
| X07 | health reporting | Implemented | crates/envd/src/worker_child.rs:95 health_loop | Named worker answers Ping with Pong from a separate health thread | [python](evidence/omp2-python-runtime.md) |
| X08 | upgrade and rollback | Implemented | crates/ext/src/upgrade.rs:728 commit_generation; crates/ext/src/upgrade.rs:768 load_generation; crates/app/src/ext_cli/mod.rs:1865 rollback; crates/ext/src/upgrade.rs:139 PinsFile; crates/driver/src/ext_updates.rs:1 startup updates | Generation commit keeps prior generation; omp ext upgrade --rollback; pins; gc; capability widening needs consent; startup update policy | [ext](evidence/omp2-extensions.md) |
| X09 | platform-specific executable distribution | Partial | crates/ext/src/index.rs:76 IndexArtifact; crates/ext/src/resolver.rs:623 validate_target; crates/ext/src/config.rs:1179 BinaryManifest; crates/ext/src/config.rs:1381 binaries; crates/ext/src/lib.rs:132 EBinPlatform | Per-target wheels only; [[binaries]] parsed with no materializer; EBinPlatform declared, never produced; no standalone executable artifact | [ext](evidence/omp2-extensions.md) |
| X09 | platform-specific executable distribution | Partial | crates/py/src/lib.rs:100 compile_error | Embedded interpreter builds only for aarch64-apple-darwin and x86_64-unknown-linux-gnu; compile_error elsewhere | [python](evidence/omp2-python-runtime.md) |
| X10 | dependency declarations | Implemented | crates/ext/src/config.rs:1370 requires; crates/ext/src/config.rs:1168 FeatureManifest.requires; crates/ext/src/resolver.rs:1 uv driver; crates/ext/src/lock.rs:103 LockFile; crates/ext/src/lib.rs:92 EXlayerDep EExtCycle | requires and per-feature requires, uv resolution with hash-pinned lock closure, extension cycle and cross-layer dependency codes | [ext](evidence/omp2-extensions.md) |
| X11 | compatibility declarations | Partial | crates/py/python/omp/limits.py:15 API_LEVELS; crates/py/python/omp/__init__.py:463 omp_api check; crates/ext/src/config.rs:1501 StaticDeclaration.api; crates/ext/src/resolver.rs:19 ACCEPTED_ABIS; crates/proto/proto/omp/toolhost/v1/toolhost.proto:24 WorkerHello; crates/ext/src/lib.rs:166 WApiSkew | Exact-integer omp_api (levels {1}), per-row api, wheel ABI tags, schema_rev; no ranges; W-API-SKEW documented, never produced | [ext](evidence/omp2-extensions.md) |
| X11 | compatibility declarations | Partial | crates/envd/src/worker_child.rs:56 python_rev | Worker hello carries python_rev and api_level; eval external runner needs Python 3.10+; no wheel-ABI manifest field checked | [python](evidence/omp2-python-runtime.md) |

## Z: serialization and IPC formats

10 rows audited: 5 Absent, 4 Implemented, 1 Partial.

| ID | Dimension | Status | Evidence | Note | Audit |
|---|---|---|---|---|---|
| Z01 | JSON/JSONL | Implemented | crates/memory/src/embedding/protocol.rs:12 MAX_FRAME_BYTES | JSON and JSONL on four channels: journal SSE+JSON, embedding worker NDJSON, `omp rpc` NDJSON, extension CONTROL (length-prefixed JSON) | [ipc](evidence/omp2-ipc-serialization.md) |
| Z01 | JSON/JSONL | Implemented | crates/envd/src/eval/process.rs:1804 write_frame | Eval child protocol is newline-delimited JSON, 64 MiB cap | [python](evidence/omp2-python-runtime.md) |
| Z02 | Protobuf | Implemented | crates/proto/build.rs:64 compile_fds | 19 proto3 files, 15 packages, prost 0.14.4 and tonic 0.14.6 in Cargo.lock; codegen in build.rs with protox | [ipc](evidence/omp2-ipc-serialization.md) |
| Z02 | Protobuf | Implemented | crates/envd/src/worker_child.rs:142 write_frame | Named worker protocol is varint-length protobuf WorkerFrame | [python](evidence/omp2-python-runtime.md) |
| Z03 | Cap'n Proto | Absent | crates/proto/src/lib.rs:51 SCHEMA_REV | No capnp package in Cargo.lock; searched Cargo.lock and workspace Cargo.toml | [ipc](evidence/omp2-ipc-serialization.md) |
| Z04 | FlatBuffers | Absent | crates/proto/src/lib.rs:51 SCHEMA_REV | No flatbuffers package in Cargo.lock; ADR 0048 rejected without measurement | [ipc](evidence/omp2-ipc-serialization.md) |
| Z05 | rkyv | Absent | crates/proto/src/lib.rs:51 SCHEMA_REV | rkyv 0.8.18 appears in Cargo.lock only as a dependency of dtls; no omp crate declares it | [ipc](evidence/omp2-ipc-serialization.md) |
| Z06 | MessagePack | Absent | crates/proto/src/lib.rs:51 SCHEMA_REV | No rmp or rmp-serde in Cargo.lock | [ipc](evidence/omp2-ipc-serialization.md) |
| Z07 | shared memory or memory-mapped files | Absent | crates/shell-builtins/src/tac.rs:15 Mmap | No shared memory, memfd or IPC mmap; memmap2 only in shell-builtins for file reads | [ipc](evidence/omp2-ipc-serialization.md) |
| Z08 | binary tensor buffers | Partial | crates/proto/proto/omp/inference/v1/inference.proto:895 values | Packed repeated float in gRPC Embed and bytes chunks exist; no raw tensor buffer framing and the worker pipe uses JSON floats | [ipc](evidence/omp2-ipc-serialization.md) |
| Z09 | JSON-RPC | Implemented | crates/envd/src/mcp/transport/stdio.rs:1 | JSON-RPC for ACP, MCP, LSP and DAP | [ipc](evidence/omp2-ipc-serialization.md) |
| Z09 | JSON-RPC | Absent | crates/envd/src/exthost/control.rs:3780 from_be_bytes | No JSON-RPC 2.0 framing in the Python child protocols (custom JSON frames); searched read of the three entry points | [python](evidence/omp2-python-runtime.md) |
| Z10 | gRPC | Implemented | crates/serve/src/lib.rs:1 | tonic servers for Inference, Auth, Blob, Gateway, ForwardProxy and SessionControl | [ipc](evidence/omp2-ipc-serialization.md) |

## W: swarm, orchestration, supervision

16 rows audited: 10 Partial, 4 Implemented, 1 Absent, 1 Proposed.

| ID | Dimension | Status | Evidence | Note | Audit |
|---|---|---|---|---|---|
| W01 | multiple logical agents in one process | Implemented | crates/driver/src/subagent/spawn.rs:89 DriverSubagentSpawner;crates/driver/src/headless/kernel.rs:2457 compose_kernel | Child kernels composed in the same process, journal-first | [swarm](evidence/omp2-swarm-adw.md) |
| W02 | asynchronous agent execution | Implemented | crates/driver/src/subagent/spawn.rs:903 tokio::spawn;crates/agent/src/jobs.rs:699 JobBoard | Child runs as a detached tokio task settling a job; parent polls the job board | [swarm](evidence/omp2-swarm-adw.md) |
| W03 | native worker threads | Absent | crates/driver/src/subagent/spawn.rs:903 tokio::spawn | No dedicated worker threads for agents; agents are tokio tasks (searched subagent/*.rs for thread spawn) | [swarm](evidence/omp2-swarm-adw.md) |
| W04 | lightweight OMP child agents | Partial | crates/driver/src/subagent/spawn.rs:923 run_child | In-process child kernel is the lightweight form; revive.rs not read | [swarm](evidence/omp2-swarm-adw.md) |
| W05 | headless OMP workers | Proposed | docs/adr/0039-remote-control-and-factory-modes.md:346 | Headless composition exists in-process; out-of-process omp serve --worker only in ADR 0039 B1; no code found | [swarm](evidence/omp2-swarm-adw.md) |
| W06 | recursive self-orchestration | Implemented | crates/driver/src/subagent/settings.rs:106 SV_TASK_MAX_RECURSION_DEPTH;crates/driver/src/subagent/spawn.rs:459 admit_child | Recursion depth journaled and capped (default max 2) | [swarm](evidence/omp2-swarm-adw.md) |
| W07 | external agent harnesses | Partial | crates/driver/src/delegated_host.rs:19 ExecutorRegistry;crates/contract/src/delegated/executor.rs:217 Executor | ExecutorRegistry and Executor contract; ADR 0052 row Partially implemented | [swarm](evidence/omp2-swarm-adw.md) |
| W08 | sandboxed subprocesses | Implemented | crates/envd/src/eval/process.rs:586 command_for | Eval and extension children can run inside the native sandbox wrapper | [python](evidence/omp2-python-runtime.md) |
| W08 | sandboxed subprocesses | Partial | crates/envd/src/child_tree.rs:1 | Sandbox crates and envd exec sandbox exist (not re-read); child agents themselves are not sandboxed processes | [swarm](evidence/omp2-swarm-adw.md) |
| W09 | shared runtime services (provider registry, credentials,... | Partial | crates/driver/src/subagent/spawn.rs:89 DriverSubagentSpawner | envd, blobs, session registry shared; registry, credentials, memory not examined | [swarm](evidence/omp2-swarm-adw.md) |
| W10 | lazy capability initialization | Partial | crates/driver/src/headless/kernel.rs:2497 sandbox_probe;crates/driver/src/subagent/settings.rs:237 SV_TASK_ENABLE_LSP | Sandbox probe lazy; LSP in children off by default; extension admission per child | [swarm](evidence/omp2-swarm-adw.md) |
| W11 | resource-aware worker pools | Partial | crates/driver/src/subagent/workpool_scheduler.rs:101 WorkpoolPolicy;crates/driver/src/subagent/settings.rs:123 SV_TASK_MAX_CONCURRENCY | Pool limit is a count from a convar; no CPU, memory or provider input | [swarm](evidence/omp2-swarm-adw.md) |
| W12 | orchestrator responsibilities (decomposition, planning, a... | Partial | crates/adw/src/run.rs:173 Run;crates/tools/src/task.rs:1 | Decomposition by the model via task; sequencing by ADW; concurrent phase driver not verified | [swarm](evidence/omp2-swarm-adw.md) |
| W13 | foreman responsibilities (workstream ownership, conflict... | Partial | docs/adr/0039-remote-control-and-factory-modes.md:618;crates/driver/src/subagent/spawn.rs:1042 finish_isolation | Role defined in ADR 0039 D3; integration by finish_isolation and merge_worktree; no foreman code | [swarm](evidence/omp2-swarm-adw.md) |
| W14 | supervisor responsibilities (lifecycle, cancellation, tim... | Partial | crates/agent/src/cancel.rs:1;crates/envd/src/child_tree.rs:1;crates/driver/src/subagent/settings.rs:144 SV_TASK_MAX_RUNTIME | Cancel tree, TERM/grace/KILL, deadline, orphan settle exist; default runtime Never; auto-restart not found | [swarm](evidence/omp2-swarm-adw.md) |
| W15 | Git worktree and copy-on-write isolation | Partial | crates/envd/src/workspace/operations.rs:673 create_worktree;docs/adr/0007-subagent-filesystem-isolation.md:53 | Per-file CoW clone implemented; git worktree backend absent by design (ADR 0007) | [swarm](evidence/omp2-swarm-adw.md) |
| W16 | workspace revision identity | Partial | crates/envd/src/workspace/operations.rs:253 WorktreeRecord;crates/envd/src/workspace/operations.rs:275 DurableSnapshotRecord | Generation counter plus snapshot manifest and tree hashes; not on the job record | [swarm](evidence/omp2-swarm-adw.md) |

## L: local inference

16 rows audited: 6 Absent, 5 Prototype, 5 Partial.

| ID | Dimension | Status | Evidence | Note | Audit |
|---|---|---|---|---|---|
| L01 | shared embedding worker | Prototype | crates/memory/src/embedding/supervisor.rs:50 EmbeddingSupervisor; crates/tools/src/memory.rs:261 search | Supervisor and stdio worker exist; nothing builds EmbeddingSupervisor; recall passes no query vector | [local](evidence/omp2-local-inference.md) |
| L02 | tiny generative model | Partial | docs/adr/0023-tiny-local-model.md:20; crates/ai/src/local/tiny_catalog.rs:24 TinyWorkload | Tiny role resolves to an online model; Apple FM route is opt-in on-device text; no in-process GGUF generator | [local](evidence/omp2-local-inference.md) |
| L03 | local classifier | Absent | crates/ai/src/local/tiny_catalog.rs:263 CLASSIFIER_MODELS; crates/app/src/tiny_models_cmd.rs:40 unique_models | CLASSIFIER_MODELS aliases MEMORY_MODELS (catalog only); difficulty classifier deleted; only the installer reads the catalog | [local](evidence/omp2-local-inference.md) |
| L04 | reranker | Prototype | crates/memory/src/aux_lane.rs:17 rank | Apple FM 0-10 relevance reranker behind apple-aux-memory; only tests call rank | [local](evidence/omp2-local-inference.md) |
| L05 | semantic compression | Partial | crates/driver/src/compress/mod.rs:1 Standalone | Standalone semantic-compression command uses the configured session model; no local-model path read | [local](evidence/omp2-local-inference.md) |
| L06 | ONNX Runtime CPU | Prototype | Cargo.toml:60 fastembed; crates/ai/src/local/embedding.rs:195 EmbeddingAdapter | ONNX Runtime via fastembed (ort-download-binaries); opt-in features in omp-ai and omp-memory; not default | [local](evidence/omp2-local-inference.md) |
| L07 | ONNX Runtime Core ML | Absent | Cargo.toml:60 fastembed | No Core ML execution provider feature on fastembed; only ort-download-binaries-rustls-tls is enabled | [local](evidence/omp2-local-inference.md) |
| L08 | MLX | Absent | crates/catalog/compat/providers/llama.cpp.kdl:3 provider | No mlx crate in Cargo.lock (parent grep of package names); no MLX server route in catalog | [local](evidence/omp2-local-inference.md) |
| L09 | direct Core ML | Absent | crates/ai/src/local/device.rs:12 TinyDevice | Only a name: TinyDevice::Coreml; TinyDevice has no reference outside its own file | [local](evidence/omp2-local-inference.md) |
| L10 | Candle | Prototype | crates/ai/src/local/stt.rs:433 whisper_device; Cargo.toml:42 candle-core | Candle Whisper, Parakeet and Kokoro behind local-stt and local-tts; no candle embedding or text generator | [local](evidence/omp2-local-inference.md) |
| L11 | llama | Partial | crates/ai/src/discovery/endpoints.rs:113 known_loopback_endpoints; crates/ai/src/discovery/probe.rs:896 apply_llama_props | Server route Implemented (loopback discovery of llama-server, /props); in-process llama.cpp barred by AGENTS.md | [local](evidence/omp2-local-inference.md) |
| L12 | Apple Neural Engine use | Absent | crates/ai/src/local/applefm.rs:97 FRAMEWORK_TIMEOUT | No explicit ANE control; Apple FM placement is decided by the OS inside the framework | [local](evidence/omp2-local-inference.md) |
| L13 | GPU and Metal | Prototype | crates/ai/src/local/parakeet.rs:240 parakeet_device; crates/ai/src/local/tts.rs:397 kokoro_device | Candle Metal device for Whisper, Parakeet, Kokoro (opt-in); CPU default for tiny devices | [local](evidence/omp2-local-inference.md) |
| L14 | shared model residency across agents | Partial | crates/ai/src/local/runtime.rs:246 LocalRuntime; crates/envd/src/memory.rs:101 session | LocalRuntime clones share weights per process; MemoryRuntime shared with subagents; pools per consumer, no process-wide pool | [local](evidence/omp2-local-inference.md) |
| L15 | Linux without Docker or Podman | Partial | crates/ai/Cargo.toml:20 local-applefm; Cargo.toml:60 fastembed | Default graph has no local feature; Apple FM macOS only; ORT binaries downloaded at build; Linux build not run here | [local](evidence/omp2-local-inference.md) |
| L16 | hardware-aware workload allocation | Absent | crates/ai/src/local/device.rs:83 gpu_layers | TinyDevice names devices (and a llama.cpp gpu_layers map) but nothing reads it; no placement policy | [local](evidence/omp2-local-inference.md) |

## C: code intelligence

20 rows audited: 7 Implemented, 5 Absent, 5 Partial, 3 Proposed.

| ID | Dimension | Status | Evidence | Note | Audit |
|---|---|---|---|---|---|
| C01 | LSP | Implemented | crates/tools/src/lsp/mod.rs:354 LspControl;crates/tools/src/lsp/mod.rs:365 LspTool | Model-facing LSP tool; LspControl bridges to the Environment document authority | [code](evidence/omp2-code-intelligence.md) |
| C02 | SCIP import | Implemented | crates/kb/src/graph.rs:408 Index::read_scip;crates/kb/src/producer.rs:55 PRODUCERS;crates/kb/src/store.rs:168 Store::build;crates/app/src/cli.rs:3362 | SCIP import in omp-kb: 3 producers, merged, decoded to JSON index. Reachable only via the omp kb CLI, no agent tool | [code](evidence/omp2-code-intelligence.md) |
| C03 | SCIP export | Absent | crates/kb/src/store.rs:190 (reads producer bytes only); git grep write_to crates/kb: no hit | No SCIP writer in repo code: no write_to_bytes or write_message_to_file call under crates/; kb only reads | [code](evidence/omp2-code-intelligence.md) |
| C04 | Glean | Absent | docs/architecture/p1-capability-inventory.md:68; Cargo.lock: no glean entry | No Glean code or dependency; the P1 inventory marks Glean/Angle absent in both trees, REMOVE | [code](evidence/omp2-code-intelligence.md) |
| C05 | tree-sitter | Implemented | crates/ast/src/parse_cache.rs:1;crates/kb/src/graph.rs:549 Parser::new | Bounded content-addressed tree-sitter parse cache; kb and ast crate use tree-sitter directly | [code](evidence/omp2-code-intelligence.md) |
| C06 | ast-grep | Implemented | crates/tools/src/ast_grep.rs:417 AstGrep;crates/tools/src/ast_edit.rs:1 | ast-grep search and edit tools over omp-ast | [code](evidence/omp2-code-intelligence.md) |
| C07 | tgrep or native trigram search | Partial | extensions/fuzzy-index/README.md:3;extensions/fuzzy-index/src/omp_fuzzy_index/score.py:26 trigrams | Trigram fuzzy index only as a Python T2 extension (warm Environment worker); no native trigram engine or tgrep | [code](evidence/omp2-code-intelligence.md) |
| C08 | Tantivy | Absent | Cargo.lock: no tantivy entry; git grep -w -i tantivy -- crates: no hit | No tantivy in Cargo.lock or sources; git grep -w tantivy outside research docs finds nothing | [code](evidence/omp2-code-intelligence.md) |
| C09 | native text search | Implemented | crates/grep/src/lib.rs:321 GrepSink;crates/grep/src/lib.rs:779 grep_stream (lines as cited by docs/adr/0060-semantic-find.md:34) | Native grep crate (streaming sink, compiled grep) plus the glob and walker crates, per ADR 0060 context | [code](evidence/omp2-code-intelligence.md) |
| C10 | structural summaries | Implemented | crates/ast/src/summary.rs:1;crates/ast/src/symbol.rs:1;docs/knowledge-base/README.md:11 | omp-ast summary and symbol modules; docs tier 1 AST outline generated by a script, not by omp-kb | [code](evidence/omp2-code-intelligence.md) |
| C11 | Mnemopi | Implemented | crates/memory/src/recall.rs:34 RecallVoice;crates/memory/src/store.rs:1718 fts_working;docs/adr/STATUS.md row 0062 | Mnemopi: four-voice recall (vector, graph, episodic FTS5 BM25, working) for conversation memory, not code. ADR 0062 Proposed | [code](evidence/omp2-code-intelligence.md) |
| C12 | embeddings | Partial | crates/memory/src/embedding/supervisor.rs:50 EmbeddingSupervisor;crates/memory/src/recall.rs:261 | Embeddings exist for memory via an isolated supervised worker; no code-symbol or code-chunk embedding index | [code](evidence/omp2-code-intelligence.md) |
| C13 | reranking | Absent | crates/memory/src/recall.rs:16 RRF_K; git grep -i rerank -- crates/memory: no hit | Reciprocal-rank fusion only (RRF_K 60); no reranker model or call path in crates/memory | [code](evidence/omp2-code-intelligence.md) |
| C14 | CodeQueryIR | Absent | crates/kb/src/graph.rs:115 Query; git grep -i CodeQueryIR -- crates docs/adr: no hit | Exact name CodeQueryIR has zero hits in code or docs outside research notes; analogue is kb Query (read-only JSON) | [code](evidence/omp2-code-intelligence.md) |
| C15 | CodeIR | Proposed | docs/parked/2026-10-06-p14-p16-architecture-notes.md:1178 | CodeIR appears only in parked design notes (status parked, not authoritative); no type in crates/ | [code](evidence/omp2-code-intelligence.md) |
| C16 | ChangeIR | Proposed | docs/parked/2026-10-06-p14-p16-architecture-notes.md:1184;crates/envd/src/workspace/operations.rs:789 merge_worktree | ChangeIR only in parked notes; implemented neighbours are envd manifest diff artifacts and content-addressed child patches | [code](evidence/omp2-code-intelligence.md) |
| C17 | VerificationIR | Proposed | docs/parked/2026-10-06-p14-p16-architecture-notes.md:1186;docs/adr/STATUS.md row 0053 | VerificationIR only in parked notes; neighbour is ADR 0053 Verdict with an independent field (Partially implemented) | [code](evidence/omp2-code-intelligence.md) |
| C18 | sharing across agents | Partial | crates/kb/src/store.rs:105 git-path omp-kb;docs/adr/0007-subagent-filesystem-isolation.md:26;crates/memory/src/bank.rs:75 BankScope::resolve | LSP and memory banks are shared via the Environment; kb cache is per Git worktree dir and child views have no .git, so kb cannot open there | [code](evidence/omp2-code-intelligence.md) |
| C19 | revision and document-version consistency | Partial | crates/kb/src/store.rs:20 Snapshot;crates/kb/src/store.rs:137 Store::stale;crates/envd/src/workspace/operations.rs:275 DurableSnapshotRecord | kb Snapshot holds HEAD plus a file-hash fingerprint that stale() recomputes; envd has generation and manifest hash; no shared key | [code](evidence/omp2-code-intelligence.md) |
| C20 | incremental invalidation | Partial | crates/kb/src/store.rs:268 update;crates/kb/src/store.rs:306 refresh;crates/ast/src/parse_cache.rs:1 | kb rebuilds the whole index; refresh is a detached full update behind refresh.pending and a lock; parse cache is per file content | [code](evidence/omp2-code-intelligence.md) |

## O: observability

5 rows audited: 3 Partial, 2 Implemented.

| ID | Dimension | Status | Evidence | Note | Audit |
|---|---|---|---|---|---|
| O01 | provider health | Partial | crates/roster-probe/src/lib.rs:1; crates/ai/src/discovery/store.rs:81 | No inference provider health probe; roster-probe is tool-roster only; RouteHealth unpopulated | [provider](evidence/omp2-provider-runtime.md) |
| O02 | authentication health | Partial | crates/proto/proto/omp/auth/v1/auth.proto:43 WatchCredentials; crates/serve/src/auth.rs:986 probe_credentials; crates/app/src/cli.rs:2197 Status | Per-credential state, WatchCredentials and ProbeCredentials exist; no metrics export of refresh or lease events was verified | [auth](evidence/omp2-auth-credentials.md) |
| O03 | metrics (sessions, agents, tokens, cache, queue latency,... | Partial | crates/observability/src/metrics.rs:73 | Token, call, duration, error, cost metrics; no queue-latency metric found | [provider](evidence/omp2-provider-runtime.md) |
| O04 | cost accounting | Implemented | crates/ai/src/layer/mod.rs:95; crates/observability/src/metrics.rs:78 | Receipt Cost, cost_micro_usd and estimated_usd counter | [provider](evidence/omp2-provider-runtime.md) |
| O05 | structured runtime events | Implemented | crates/ai/src/registry.rs:785; crates/ai/src/registry.rs:834 | ExecutionReceipt attempts and provider_request spans with fallback warnings | [provider](evidence/omp2-provider-runtime.md) |

## Dimensions not yet audited

O06, O07, O08.

These have no row in any `evidence/omp2-*.tsv` at the time of this commit. They are **not** assumed absent. The largest block is code intelligence (C01 to C20) when its audit has not landed, and observability (O01 to O08), which no area owned.


## Reading the table: what it shows

1. **The provider runtime is gateway-class but partly unfed.** Identity, capability negotiation, account pooling, rate reservation, admission, retry and recovery are layers of one fixed stack (`crates/ai/src/layer/stack.rs:261-274`). The provider audit found that router health, quota, latency and route-affinity inputs have no production producer, and there is no circuit breaker. The gap is evidence wiring, not another runtime.
2. **Protocol translation is canonical (`ChatEvent`, ADR 0022), not pairwise.**
3. **Extensions are child-process, message-based and signed; no C ABI plugin interface exists.** Several manifest codes are declared but have no producer (extension audit).
4. **SCIP import exists** (`crates/kb`); the owner's four IR names do not exist as types, schemas or ADR text (IPC audit).
5. **The swarm is child kernels, a workpool and a job board;** the pool limit is a count, with no CPU or memory input.
6. **Local inference is mostly opt-in or prototype:** the embedding worker exists but nothing builds it in production.

## Not verified

Every `Not verified` or `not read` note in the Note column, every dimension listed under "Dimensions not yet audited", and anything about performance, size or memory.

Back to the [index](README.md).
