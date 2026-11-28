# Reference gateways, second group: LLMHub, ProxyCast, 9router, FreeLLMAPI, new-api

> Part of the [OMP2 gateway-capabilities research](../README.md). Evidence file feeding deliverable 3 (cross-gateway matrix) and the diagrams. Audited OMP2 commit `85d219ae7f` (`origin/omp2`). Research proposal: nothing here is implemented in OMP2.

Back to the [index](../README.md).

## 0. Scope, method and reading rules

Five repositories under `/private/tmp/omp-research-refs/inference/`, all read as untrusted input (nothing run, nothing installed). Per-dimension rows live in `evidence/matrix-<owner>_<repo>.tsv`; this file is the narrative and the adoption analysis. Sections are in reading order: LLMHub, ProxyCast, 9router, FreeLLMAPI, new-api.

| Repository | Pinned SHA | Head date | Licence (API / files) | Notes |
|---|---|---|---|---|
| `nicolaeser/llmhub` | `8bdc1de` | 2026-11-26 | MIT / `LICENSE` | `src/lib/gateway` is the gateway core |
| `hank9999/proxycast` | `3ad8ff6` | 2025-12-26 | none detected | see section 2 |
| `decolua/9router` | `5ae63d2` | 2026-11-23 | MIT / `LICENSE` | section 3 |
| `fallencolors/freellmapi` | `b5d8333` | 2026-09-23 | MIT / `LICENSE` | section 4 |
| `QuantumNous/new-api` | `04dfcbb` | 2026-11-28 | AGPL-3.0 / `LICENSE` | study only, copy nothing |

How size was counted: `git ls-files` in the clone, then newline counts over every tracked non-binary file, grouped by extension (Python one-off, not stored as a script). "Lines" are physical lines, comments and blank lines included; vendored lockfiles and i18n JSON inflate `.json` totals and are called out where they matter.

Adoption filter applied to every "idea worth adopting": the owner-locked decisions in `AGENTS.md` `### Locked Deviations from pi` (lines 526-582) and ADR 0049 D1/D6. Concretely: providers are data (no model-name conditionals or hardcoded model metadata in `.rs`); typed serde/prost wire structs, no untyped `Value` traversal; provider frames decode to canonical semantic events; no vendor server-side tools; tokio and rayon only; new capabilities default to extensions (T2) and vendor code never enters T0 or T1. Each idea below says which of these it touches. An idea that violates one is marked `Requires ADR reconsideration` or `Rejected`; the final class is the parent's decision.

Provider-terms rule (`01-method-and-dimension-ids.md` section 2, item 8): patterns that pool subscriptions, harvest free tiers, impersonate official clients or burn through reset credits are described neutrally here and flagged `Rejected` with the reason. This file does not recommend any of them.

## 1. nicolaeser/llmhub (`8bdc1de`)

### 1.1 What it is

A multi-tenant, self-hosted LLM gateway with an admin console: virtual API keys per company/project/person, budgets, PII redaction, guardrails, request logging, and routing across API-key providers and across "subscription" accounts (ChatGPT/Codex, Claude Code, SuperGrok, Antigravity, Kimi Code, Muse Code, Devin) (`README.md` Features, "Subscription sign-ins"; code in `src/lib/gateway/upstreams/provider-kinds.ts:PROVIDER_CATALOG`). Runs as one Docker image on PostgreSQL, Redis and S3 optional (`README.md` intro). Licence MIT (`LICENSE`).

Language and size: TypeScript on Next.js 16 and React 19 (`package.json` dependencies `next`, `react`, `prisma`, `bullmq`, `ioredis`). 1,670 tracked files; 1,083 `.ts` files (169,103 lines) and 321 `.tsx` files (47,834 lines); `src/lib/gateway` alone is 170 files, 31,947 lines. Tests: 254 `*.test.ts` under `src/tests/` plus 42 under `src/tests/integration/`, run with `node --test` (`package.json` scripts `test`, `test:integration`).

### 1.2 Entry points and request path

Entry points are Next.js route handlers: `src/app/api/v1/chat/completions/route.ts` delegates to `src/lib/gateway/endpoints/chat-completions-route.ts:chatCompletionsRoute("api")`; the subscription surface lives under `src/app/api/subscription/v1/*` and never mixes with API-key traffic (`README.md` "Subscription sign-ins"). Management REST is `src/app/api/mgmt/*`.

Request path (`chatCompletionsRoute`, `endpoints/pipeline.ts:gateModelRequest`, `dispatch/chat.ts`):

1. `gateRequest` authenticates the virtual key, reads the body, builds a `Principal` and a trace (`pipeline.ts:33-43`).
2. `screenModelRequest` applies PII masking, guardrails and model access policy; `resolveChatFiles` and `modelChain` expand file refs and the alias list.
3. `streamChat`/`dispatchChat` call `withDeployment(aliases, limits, fn, opts)` (`dispatch/chat.ts:295`), which loops alias entries, then `num_retries + 1` attempts per alias, calling `acquireGroup` (`routing/runtime.ts:470`) to pick a deployment and holding a concurrency permit.
4. The upstream adapter (`dispatch/upstream.ts`, `upstreams/*.ts`) builds provider headers and body and `fetch`es with `AbortSignal.any([headersTimer, req.signal])` and a stream idle timeout (`dispatch/chat.ts:openUpstreamStream`, 300 s each).
5. The response is relayed through `protocols/chat-stream.ts:pipeChatStream` with a per-protocol encoder, then `recordUsage` writes tokens, cost and the request log (`dispatch/chat.ts:recordSuccess`).

### 1.3 Registry, credentials and secrets

- Registry: `PROVIDER_CATALOG` is a hardcoded TypeScript array of provider kinds with default base URL and an `auth: "sign_in"` flag (`upstreams/provider-kinds.ts:4-65`). Connections, model groups (alias to weighted deployments), per-deployment cost and concurrency are rows in PostgreSQL (`routing/runtime.ts:mapDeployment`, `loadGroup`). Model discovery is `GET /models` with kind-specific URL rules (`upstreams/discovery.ts:modelsUrl`). A catalog sync worker adds routes, and an LLM judge (`catalog/jev.ts:jevRequest`) is asked "which public alias serves exactly the same model as this provider model" with a confidence threshold (`catalog/model-catalog.ts:JEV_SUGGEST_CONFIDENCE`).
- Secrets: all stored in PostgreSQL, AES-256-GCM sealed with the `enc1:` prefix, key derived by HKDF from `APP_SECRET` (`src/lib/crypto.ts:seal`, `open`, `subkey`). Subscription tokens are sealed JSON (`upstreams/credentials.ts:sealTokens`). There is no reference-to-secret indirection and no OS keychain.
- Refresh: single-flight per account plus a 30 s DB lease (`credentials.ts:LEASE_MS`, `refreshing`), per-kind refresh lead times and 1 to 30 min exponential retry (`REFRESH_LEAD_MS`, `retryDelay`), a login journal that keeps the newest refresh token (`journalLogin`, `upstreams/login-journal.ts`), and a separate `keeper` process that refreshes due sign-ins every cycle and needs only PostgreSQL (`src/keeper/main.ts`, `src/keeper/cycle.ts:runKeeperCycle`; `README.md` "Keeping sign-ins alive").

### 1.4 Scheduling and routing

All in `src/lib/gateway/routing/`:

- Selection: `runtime.ts:pick` dispatches on strategy: `round_robin`, `quota_balanced`, `quota_expiring`, `smart`, `weighted_random`, `cost_lowest`, `priority`, `fast` (latency EWMA, alpha 0.3, `router.ts:nextLatencyEwma`), default least in-flight (`router.ts:pickLeastInflight`).
- Eligibility chain in `acquireGroup`: enabled, account allowed, signed in, not cooling, not already tried this request, within quota, concurrency room, then affinity pin, then account policy.
- Circuit breaking and cooldown: `provider-health.ts:recordFailure` trips a cooldown after `ALLOWED_FAILS = 2` consecutive failures for `COOLDOWN_MS = 15 s`, or until the provider's `retryAt` for limit errors (capped at 7 days); cooldown state is in memory and shared through Redis when configured (`shareCooldown`).
- Failure classification: `dispatch/chat.ts:failureClass` returns one of `request`, `context`, `depleted`, `limit`, `rejected`, `signed_out`, `model`, `overloaded`, `connection`, `transient`, `caller`; `recordAttemptFailure` maps each to a cooldown kind and `rotatesAccount` decides whether to try another account. One connection-level retry and one reasoning-drop retry are special-cased in the loop (`chat.ts:454-463`, `438-447`).
- Backpressure: per-deployment gates with bounded wait queues (`concurrency.ts:queueCapacity`, 16 to 1,024) that answer 429 `concurrency_limit_exceeded` with a retry-at hint.
- Rate-limit awareness: `upstreams/observed-limits.ts` parses `anthropic-ratelimit-unified-*` and `x-codex-*` response headers into 5 h and 7 d usage windows; `dispatch/upstream.ts:observeUpstreamLimits` is called on every upstream response. A poller refreshes plan limits about every five minutes (`README.md` "Limits").
- Affinity: `deployment-affinity.ts` pins a conversation to one deployment for 30 min (`DEPLOYMENT_PIN_TTL_MS`) with a spill margin when that replica is busy; the session key is, in order, a client key (`prompt_cache_key`, Claude Code metadata/header), the `x-session-affinity` header, or a SHA-256 fingerprint of the system prompt and first user message (`session-fingerprint.ts:affinitySession`). Stated purpose is reuse of each replica's KV prefix cache for self-hosted vLLM/SGLang/llama.cpp (`README.md` "Session affinity"). A separate account pin (6 h, `account-affinity.ts:PIN_TTL_MS`) keeps a session on one subscription account and hands the whole conversation over when that account is exhausted.
- Context fit: `ContextFit` (`dispatch/chat.ts:332`) plans which deployments can hold the prompt and defers "tight" ones.

### 1.5 Protocol translation

Hub-and-spoke on OpenAI Chat Completions as an untyped `JsonMap`. `protocols/messages.ts:messagesToChat`/`chatToMessage` and `protocols/responses.ts:responsesToChat`/`chatToResponse` convert at the edge; same-protocol deployments get native pass-through (`dispatch/messages-dispatch.ts:NATIVE_PATH`). `messages.ts:chatIncompatibility` rejects, before dispatch, a Messages request whose tools or content blocks a non-Anthropic deployment cannot represent, returning a typed path-qualified reason instead of silently dropping them. Reasoning is carried across turns by encoding thinking blocks into Responses items (`responses.ts:encodeThinking`) and dropped on provider rejection (`chat.ts:438`). Streaming tool-call deltas are merged by one accumulator (`chat-stream.ts:ToolCallDeltas`) shared by encoders. Gemini and Vertex exist only as upstream kinds; no inbound Gemini-native route appears under `src/app/api/v1`.

### 1.6 Streaming and errors

SSE only; no WebSocket path (grep for `WebSocket`/`wss://` found one Codex instruction string). `GateError` carries `status`, `code`, `retryAt`, `requestScoped` (`errors.ts`) and is rendered in Anthropic error shape for Messages clients (`errors.ts:anthropicErrorBody`). Failover happens only until the upstream response headers arrive; a stream that stalls after that surfaces a 504 `upstream_timeout` through `withIdleTimeout` (`chat.ts:663-667`) rather than switching deployment mid-stream (reading of the code; no test run).

### 1.7 Usage, cost, observability, management

Per-request usage normalised and priced by `billing/cost.ts` with cache read/write multipliers and service-tier factors, using regex tables keyed on model names (`cost.ts:CACHE_READ_RATES`, `CLAUDE_MODEL`); budgets at key, user, member, team, organisation and project with a reserve-then-settle hold taken inside one transaction in a fixed lock order (`billing/budget-hold.ts:reserveBudget`, `LOCK_ORDER`). Provider health, cooldown and plan-limit state are pushed to the console (`routing/health-report.ts`, `upstreams/subscription-limits.ts`). Logging is a structured `logger.<level>(event, fields)`; no Prometheus or OpenTelemetry (grep found none) and no TTFT metric. Management is the Next.js console plus `/api/mgmt/*` REST, SCIM, OIDC and passkeys under `src/lib/auth`. Compliance archive exports request content to S3 (`src/lib/compliance-archive`).

### 1.8 Plugin or extension mechanism

None for providers: new provider kinds require a code change to `PROVIDER_CATALOG` and an adapter. Extension points are MCP (an MCP hub and tool loop in chat, `src/lib/mcp/hub.ts`, `endpoints/chat-completions-route.ts:runMcpChat`), guardrail rules and an `integrations` runtime (`src/lib/integrations`).

### 1.9 Dependencies on unofficial endpoints, pooling and similar (neutral description)

The subscription kinds talk to consumer-product endpoints rather than published model APIs: ChatGPT backend `https://chatgpt.com/backend-api/codex`, Antigravity `https://daily-cloudcode-pa.googleapis.com`, Devin `https://server.codeium.com` with its own wire format (`upstreams/devin-wire.ts`), plus Kimi Code, Muse Code and SuperGrok kinds (`provider-kinds.ts:14-64`). The Claude adapter presents requests as the Claude Code CLI: fixed CLI version, `claude-cli/<version>` user agent, `x-app: cli`, OAuth beta flags and a Claude Code identity system prompt (`upstreams/claude.ts:CLAUDE_CODE_VERSION`, `CLAUDE_CODE_IDENTITY`, `claudeIdentityHeaders`); the Codex path sets a fixed `originator` and embeds Codex CLI base instructions (`credentials.ts:CODEX_ORIGINATOR`, `catalog/codex-instructions.ts`). Several accounts can be pooled behind one endpoint with automatic hand-over when one reaches its limit (`dispatch/chat.ts:withDeployment`, `rotatesAccount`; `README.md` "Limits"); an option sends a one-line message the moment a plan window resets to start the next window (`src/lib/providers/account-routing.ts` `kickoffModel`; `README.md`); and `upstreams/reset-redeem.ts` plus `reset-credits.ts` claim provider-issued reset credits automatically under an auto-reset policy (`routing/auto-reset-policy.ts`; inferred from the imports and names of `reset-redeem.ts:1-45`, function bodies not read). Accounts can be imported from CLIProxyAPI auth files or its management API (`upstreams/auth-file-import.ts` by name; `README.md` "Subscription sign-ins"). Whether providers authorise any of this is not established by the repository; for OMP2 it is classified as rejected (section 1.14).

### 1.10 What it does well

- A typed failure taxonomy that separates request errors, context overflow, quota depletion, rate limits, account rejection, overload and connection faults, and ties each to a distinct recovery action (`chat.ts:failureClass`, `recordAttemptFailure`).
- Deterministic eligibility pipeline with explicit reasons for emptiness (`ERR_ALL_PAUSED`, `ERR_NOT_ASSIGNED`, `ERR_SIGNED_OUT`, `ERR_NO_HEALTHY`) so callers learn why nothing was routable (`runtime.ts:acquireGroup:506-509`).
- Credential-refresh hardening for one-shot refresh tokens: lease, journal and a keeper that survives app restarts (`credentials.ts`, `src/keeper`).
- Affinity done as a hash of the conversation opening, with ephemeral hashed keys, tied to an explicit cache-locality rationale (`session-fingerprint.ts`, `README.md`).
- Bounded backpressure rather than unbounded queues (`concurrency.ts`).
- Broad test surface relative to code size (296 test files).

### 1.11 Risks

- Provider kinds and pricing/cache-rate rules are code, not data, and key on model-name regexes (`provider-kinds.ts`, `billing/cost.ts:23-30`), the opposite of OMP2's providers-as-data rule.
- Protocol translation operates on untyped `JsonMap` (`protocols/messages.ts:238` `messagesToChat(request: MessagesRequest): JsonMap`), so wire correctness rests on runtime checks.
- Failure classification partly sniffs error text (`chat.ts:109` `MODEL_UNAVAILABLE = /\bmodel\b/i`), which is brittle across providers.
- An LLM judge decides model equivalence at catalog sync time (`catalog/jev.ts`), which is non-deterministic and ships provider model metadata to a third-party model.
- Operational weight: Node 24, Next.js, PostgreSQL required, Redis and S3 optional, 7 GB default memory limits (`README.md` "Memory limits").
- Subscription-account pooling and client impersonation carry provider-terms and account-suspension risk (section 1.9).

### 1.12 Licence implications

MIT. Ideas and small excerpts may be reused with the MIT notice, but this research copies nothing; any future code reuse would need an entry in `THIRD-PARTY-NOTICES.txt`. No licence barrier to studying the design.

### 1.13 Ideas worth adopting in OMP2's existing provider runtime

Checked against the locked decisions; `fit` says which lock each idea touches.

| # | Idea | Evidence | Fit with locks | Suggested handling |
|---|---|---|---|---|
| L1 | Typed failure classes that select the recovery action (rotate credential, wait until `retryAt`, shrink context, surface to caller), with the class derived from status/code and provider-declared data, not message regexes | `chat.ts:failureClass`, `recordAttemptFailure`, `rotatesAccount` | Fine if classes come from typed fields and catalog data; message sniffing would violate providers-as-data in spirit | Compare with OMP2's existing classifier (PRV area), extend only the missing classes |
| L2 | Pre-flight capability check that rejects with a path-qualified reason instead of silently degrading a request | `protocols/messages.ts:chatIncompatibility` | Fine; operates on typed request structs in OMP2 | Candidate for capability-aware routing (S06) using catalog capability data |
| L3 | Per-credential concurrency permits with a bounded queue and a retry-at hint | `routing/concurrency.ts:queueCapacity`, `concurrencyLimitedError` | Fine (tokio semaphore); no new runtime | Relevant to single-session swarm sharing one credential (W09); measure before adding |
| L4 | Session key hashed into an affinity pin so the same replica serves the whole conversation (prefix-cache reuse) | `routing/session-fingerprint.ts`, `routing/deployment-affinity.ts` | OMP2 already owns a session id, so the fingerprint half is unnecessary; the pin only matters if OMP2 routes across several deployments of one model | Send a stable session/cache key; add pinning only when multi-replica routing exists |
| L5 | Parse provider-reported rate-limit headers into typed windows and feed them to the selector | `upstreams/observed-limits.ts`, `dispatch/upstream.ts:observeUpstreamLimits` | Header names are vendor-specific: allowed only as catalog/KDL data (class B), never as `.rs` conditionals | Data-declared header schema, extend only if the PRV audit shows the gap |
| L6 | Lease plus journal for refresh-token rotation so concurrent processes never double-spend a one-shot refresh token | `credentials.ts:LEASE_MS`, `journalLogin`; `src/keeper` | Fine if kept in the existing credential store; a separate keeper process is an extension-level choice (T2) | Relevant to hybrid swarm with child OMP processes; depends on AUTH audit |
| L7 | Reason-specific "nothing routable" errors | `runtime.ts:506-509` | Fine | Cheap, deterministic diagnostic improvement |

### 1.14 Ideas to avoid

| # | Idea | Why |
|---|---|---|
| A1 | Provider kinds, cache-rate and tier pricing as code keyed on model-name regexes | Violates providers-as-data; `cost.ts:CACHE_READ_RATES` is exactly what lintx rejects. Rejected |
| A2 | Untyped `JsonMap` translation | Violates typed wire structs. Rejected |
| A3 | LLM judge for model equivalence in the registry | Non-deterministic where a curated catalog or explicit alias works; at most an offline curation aid. Rejected as runtime behaviour |
| A4 | Subscription pooling with automatic hand-over, plan-window kickoff messages, automatic reset-credit redemption, CLI identity headers and embedded CLI prompts | Designed around provider usage limits and official-client identity; provider-terms risk. Rejected for security, licensing or operational reasons |
| A5 | Required Postgres and Redis, Next.js console, mandatory web UI | Contradicts a lightweight native harness; owner brief section 13 forbids a mandatory dashboard or database server. Unnecessary or redundant |
| A6 | Importing another gateway's auth files or management API to move accounts | Credential-handling surface with no OMP2 use case. Rejected |

## 2. hank9999/proxycast (`3ad8ff6`)

### 2.1 What it is

A Tauri 2 desktop application, version 0.20.1 (`src-tauri/Cargo.toml`), that turns credentials of other AI clients (Kiro, Gemini CLI, Qwen, Antigravity, Codex, Claude OAuth, iFlow) and plain API keys into a local OpenAI-compatible and Anthropic-compatible endpoint (`README.md` lines 16-45, Chinese). The README says it is the desktop version of AIClient-2-API (`README.md:43-45`). Last commit 2025-12-26 (`fix: ...上游的编译错误`).

Language and size: 591 tracked files; 203 `.rs` files (117,444 lines, `src-tauri/src`), 136 `.tsx` (42,939) and 65 `.ts` (7,595) for the React UI. Rust stack: tokio, axum 0.7 with WebSocket and rustls, reqwest, rusqlite, serde, tiktoken-rs (`src-tauri/Cargo.toml`). Tests: `tests.rs` files per module (for example `router/tests.rs` 760 lines, `credential/tests.rs` 1,929 lines); no test run.

### 2.2 Entry points and request path

`src-tauri/src/lib.rs` wires Tauri state; `server/mod.rs:run_server` builds the axum router: `/v1/chat/completions`, `/v1/messages`, `/v1/messages/count_tokens`, `/v1/gemini/*path`, `/v1/ws`, `/v1/models`, `/api/provider/:provider/v1/messages` (Amp), `/v0/management/*` behind `ManagementAuthLayer` (`server/mod.rs:801-882`).

`handlers/api.rs:chat_completions` (line 621): `verify_api_key`; `RequestContext`; `processor.resolve_and_route` (alias map then routing rules, `processor/mod.rs:218`); optional parameter injection (`api.rs:662-680`); `select_provider_for_client` by detected client type (`api.rs:349`); `pool_service.select_credential` (`api.rs:705`); flow-monitor capture with optional user intercept (`api.rs:726-760`); `call_provider_openai` (`api.rs:768`); telemetry and token-usage recording (`api.rs:777-794`).

A seven-step `RequestProcessor` pipeline (Auth, Injection, Routing, PluginPre, Provider with retry and failover, PluginPost, Telemetry) is documented in `processor/mod.rs:5-14` and implemented in `processor/steps/*`, but the HTTP handlers only call `resolve_and_route`; a grep for `execute_with_resilience`, `execute_with_retry`, `.failover.` and `.retrier.` finds callers only inside `processor/steps/provider.rs`. So retry and failover exist as library code that this reading did not find on the live request path (`[INFERENCE]` that it is dead code; only the absence of callers is observed).

### 2.3 Registry, credentials and secrets

- Registry: `ProviderType` is a fixed enum (`lib.rs:63`); `CredentialData` has one variant per provider, either `creds_file_path` (OAuth) or `api_key` (`models/provider_pool_model.rs:33-87`). Model support is per credential: `not_supported_models`, wildcard `excluded_models` for Gemini keys, and a hardcoded Antigravity model list (`provider_pool_model.rs:290-310`). A hardcoded Kiro model map lives in `converter/openai_to_cw.rs:get_model_map`. These are model-name conditionals in Rust.
- Secrets: OAuth tokens stay in the other client's token files, for example Kiro reads `~/.aws/sso/cache/kiro-auth-token.json` (`providers/kiro.rs:default_creds_path`); API keys are plain `String` fields persisted in SQLite. A grep for `encrypt|aes|keyring|chacha` over `src-tauri/src` finds no hits, so no encryption at rest or keychain use was found.
- Refresh: `CredentialProvider`/`TokenManager` traits with expiring-soon check (5 min) and retried refresh (`providers/traits.rs`); each provider implements its own OAuth (`providers/codex.rs`, `claude_oauth.rs`, `qwen.rs` device code, `antigravity.rs`).

### 2.4 Scheduling and routing

Two overlapping mechanisms: `credential/balancer.rs:LoadBalancer` (RoundRobin, LeastUsed, Random, cooldown marking, failover-on-proxy-error) and the DB-backed `services/provider_pool_service.rs:select_credential`, which is what the handlers call: filter healthy-and-enabled credentials that support the model, return the only one, otherwise pick the best by a 100-point score (health 40, usage frequency 30, idle time up to 30; `calculate_credential_score`, lines 240-290). Health flips after `max_error_count` errors (`provider_pool_service.rs:341`). Every request reads the credential rows from SQLite under a mutex (`select_credential:187-191`). No session affinity, no rate-limit-header parsing and no cost or latency routing were seen (`quota.rs` 1,141 lines was not read, so quota awareness is `U`). `resilience/failover.rs:FailureType::detect` classifies QuotaExceeded, AuthenticationFailed, ServiceUnavailable, Other from status code and message text.

### 2.5 Protocol translation

Pairwise converters around OpenAI types: Anthropic to OpenAI (`converter/anthropic_to_openai.rs`), OpenAI to CodeWhisperer and back (`openai_to_cw.rs`, `cw_to_openai.rs`), OpenAI to Antigravity/Gemini (`openai_to_antigravity.rs`), with a `ProtocolSelector` that scores conversion paths (`protocol_selector.rs`). Request and response types are typed Rust structs in `models/openai.rs`, `anthropic.rs`, `codewhisperer.rs`; the Antigravity converter returns `serde_json::Value` (`openai_to_antigravity.rs:365`). Kiro thinking is emulated by injecting tags (`kiro_thinking.rs:ensure_kiro_thinking_tags`).

### 2.6 Streaming and errors

`streaming/traits.rs:StreamingProvider` returns `Pin<Box<dyn Stream<Item = Result<Bytes, StreamError>>>>` plus a `stream_format`; `streaming/aws_parser.rs:AwsEventStreamParser` decodes AWS EventStream binary frames from Kiro (`AwsEvent` enum) and `cw_to_openai.rs` maps events to OpenAI chunks. Errors are `ProviderError`, `ProcessError`, `StreamError` enums (`providers/error.rs`, `processor/error.rs`, `streaming/error.rs`; bodies not read).

### 2.7 Usage, observability, management, extension

`telemetry/tokens.rs` (tiktoken-rs) and `stats.rs` aggregate usage; there is no money cost accounting found. The flow monitor (24,418 lines under `flow_monitor/`) captures each request and response, supports interception, replay, diff, filter DSL and export including code export (`flow_monitor/monitor.rs`, `replayer.rs`, `interceptor.rs`, `code_exporter.rs`). Management is the Tauri UI plus `/v0/management/*` (`server/mod.rs:802`). Extension: a `Plugin` trait with `Script` (JSON-driven) and reserved `Native` types and pre/post hooks (`plugin/types.rs:138-145`, `plugin/loader.rs:ScriptPlugin`), loaded from `~/.config/proxycast/plugins`.

### 2.8 Dependencies on unofficial endpoints, pooling and similar (neutral description)

Provider code targets consumer and internal endpoints: Kiro/CodeWhisperer through AWS OIDC tokens (`providers/kiro.rs:544` `oidc.{region}.amazonaws.com/token`), ChatGPT backend `chatgpt.com/backend-api/codex` (`codex.rs:19`), Claude consumer OAuth at `claude.ai` (`claude_oauth.rs:26`), Google internal Cloud Code endpoints and sandbox hosts for Gemini CLI and Antigravity (`gemini.rs:17`, `antigravity.rs:16-17`), Qwen and iFlow consumer OAuth. It reuses tokens written by the official clients. The README frames the purpose as using "client quota" in other tools and balancing several accounts "whichever has quota" (`README.md:18-27`). A `MachineIdService` can read and overwrite the operating-system machine identifier, generate a random one, and keep a history, requiring admin rights (`services/machine_id_service.rs:set_machine_id`, `generate_random_machine_id`, `check_admin_privileges`). `browser_interceptor/` and `scripts/playwright-login` automate capture of browser login flows. For OMP2 these patterns are classified as rejected (section 2.12).

### 2.9 Relation to other repositories

No reference to `proxycast` appears in `limecloud/lime`'s README, `CLAUDE.md`, `AGENTS.md`, `FEATURE-MAP.md`, `docs`, `lime-rs/Cargo.toml`, `package.json` or `governance` (bounded grep, 2026-11-28). Lime is described as a desktop and CLI/TUI agent with a Rust workspace `lime-rs`, GPL-3.0, last commit 2026-11-18. Supersession is therefore not established from the clones; ProxyCast's own README names AIClient-2-API as its upstream. GitHub fork-parent metadata was not queried.

### 2.10 What it does well

- Idiomatic Rust gateway on tokio and axum with typed request structs, a direct comparison point for OMP2's own stack (`server/mod.rs`, `models/openai.rs`).
- Binary AWS EventStream decoding into typed events (`streaming/aws_parser.rs`).
- A flow monitor that records, replays and diffs real requests, a useful debugging model for provider-runtime regressions (`flow_monitor/replayer.rs`, `diff.rs`).
- Clear provider-credential trait with proactive refresh (`providers/traits.rs`).

### 2.11 Risks

- Secrets at rest are plaintext in SQLite (grep found no encryption), and OAuth tokens are borrowed from other applications' files.
- Routing, retry and failover are partly dead or duplicated code (two balancers, processor steps not on the handler path), which is the kind of drift OMP2's "no second scheduler" rule exists to prevent.
- Model-name conditionals and hardcoded model maps in `.rs` (`provider_pool_model.rs:309`, `openai_to_cw.rs:16`).
- Failure classification relies on message text (`failover.rs:FailureType::detect`).
- One SQLite read under a mutex on every request (`provider_pool_service.rs:187-191`).
- Licence ambiguity (below).

### 2.12 Licence implications

No `LICENSE` file, no `license` field in `Cargo.toml` or `package.json`; the README badge and text declare GPL-3.0 (`README.md:7`, `README.md:221`). The GitHub API reports none. Treat it as GPL-3.0 by declaration with no verifiable grant. Study ideas only and copy nothing; GPL-3.0 code cannot be merged into OMP2 without a licence decision by the owner.

### 2.13 Ideas worth adopting in OMP2's existing provider runtime

| # | Idea | Evidence | Fit with locks | Suggested handling |
|---|---|---|---|---|
| P1 | Request/response flow recorder with replay and diff for provider regressions | `flow_monitor/monitor.rs`, `replayer.rs`, `diff.rs` | Must reuse OMP2's journal and artifact addressing rather than a new store (no second store) | Compare with existing journal replay; at most an extension (T2) |
| P2 | Binary frame parser producing a typed event enum for a non-SSE provider protocol | `streaming/aws_parser.rs:AwsEvent` | Consistent with "stream frames decode to canonical semantic events" if the codec is genuinely a different wire protocol (ADR 0049 D6 class B) | Only if a Bedrock-style provider is in scope |
| P3 | Proactive token-expiry check with a fixed lead and retried refresh behind one trait | `providers/traits.rs:is_token_expiring_soon`, `ensure_valid_token` | Fine | Already a common pattern; low value beyond LLMHub's lease idea |

### 2.14 Ideas to avoid

| # | Idea | Why |
|---|---|---|
| A1 | Borrowing tokens from other clients' credential files and presenting them through a local proxy | Uses another product's entitlement outside its client; provider-terms risk. Rejected for security, licensing or operational reasons |
| A2 | OS machine-identifier rewriting and browser-login interception | Identity manipulation and credential capture; Rejected for security, licensing or operational reasons |
| A3 | Plaintext API keys in a local DB | Contradicts credential-reference practice; Rejected |
| A4 | Two parallel credential balancers plus an unused resilience pipeline | Second scheduler; Unnecessary or redundant |
| A5 | Model maps and supported-model lists in `.rs` | Violates providers-as-data (lintx reviewer-reject) |
| A6 | GPL-3.0 licensed code as a copy source | Licence conflict; study only |

## 3. decolua/9router (`5ae63d2`)

### 3.1 What it is

A local or self-hosted AI router (Next.js dashboard, npm package `9router`, Docker image) that exposes one OpenAI-compatible endpoint (`http://localhost:20128/v1`) over 40+ providers and combines "fallback to cheap and free models", multi-account rotation, quota tracking and an "RTK token saver" that compresses tool output (`README.md:4-42`). Licence MIT (`LICENSE`, "Copyright (c) 2024-2026 decolua and contributors").

Language and size: JavaScript (no TypeScript). 2,024 tracked files; 1,610 `.js` files (255,200 lines), 135 `.md` (52,512), 173 PNG. Core is `open-sse/` (executors 63 files 21,924 lines; translator 54 files 12,057; services 62 files 13,327; handlers 66 files 8,483; provider registry 164 files 10,334), the dashboard and API is `src/` (`src/app` 302 files 55,024 lines), `src/mitm` (3,441 lines) and `cli/` (6,702 lines). Tests: 441 files under `tests/unit` plus 43 under `tests/translator`; not run.

### 3.2 Entry points and request path

Entry points: `custom-server.js`, Next.js route handlers under `src/app/api`, `src/sse/handlers/chat.js` (inbound key extraction at line 68) and `open-sse/handlers/chatCore.js`. Path: inbound request, API-key check, RTK compression of the source-format body (`open-sse/rtk/index.js:compressMessages`, applied "in chatCore ... before translateRequest" per its header comment), model/combo resolution (`open-sse/services/combo.js`), account choice (`src/sse/services/auth.js`), translation source to OpenAI to target (`open-sse/translator/index.js:68 translateRequest`), provider executor (`open-sse/executors/*`), response translators and SSE back to the client.

### 3.3 Registry, credentials and secrets

Provider registry is data-like JavaScript: 152 files in `open-sse/providers/registry/`, each from `REGISTRY_TEMPLATE.js`, plus `capabilities.js`, `pricing.js`, `catalogOverride.js`. `capabilities.js` resolves model capabilities in four layers (provider override, exact id, glob patterns, default floor) and overlays modalities and limits from a models.dev catalog refreshed in the background (file header comment, lines 1-30; refresher body not read). Provider connections are stored in a local database (`src/lib/localDb.js`, `src/lib/db`); a grep for `createCipheriv|aes-256|keytar|safeStorage` hit only `src/mitm/manager.js`, a login helper and a Qoder helper, so no encryption of stored connections was found. OAuth helpers live in `src/lib/oauth/providers/`.

### 3.4 Scheduling and routing

- Account selection: provider-level `fallbackStrategy` (default `fill-first`, also round-robin) with per-provider overrides (`src/sse/services/auth.js:144-146`), plus proxy-pool rotation per connection (`auth.js:50-55`, `pickProxyPoolId`).
- Error handling is **configuration-driven**: `accountFallback.js:checkFallbackError` walks `ERROR_RULES` top to bottom (text substring or status, optionally provider-specific) and returns `shouldFallback`, `cooldownMs`, exponential `newBackoffLevel` (1 s doubling to a cap) and flags `modelScoped` and `requestScoped`; the second flag separates "credential at fault" from "this request was rejected" so combo rotation still tries the next member (`accountFallback.js:29-56`, `open-sse/config/errorConfig.js`). Per-model locks (`MODEL_LOCK_PREFIX`, `buildModelLockUpdate`) cool one model on one connection without blocking the account.
- Combos: ordered model lists with `round-robin` and a sticky limit (`combo.js:getRotatedModels`), capability reordering from the request body (`detectRequiredCapabilities`, `reorderByCapabilities`), and zero-content failover that peeks the start of an SSE stream and replays it if content arrives (`combo.js:peekComboResponse`, `emptyFailover.js:scanSSEBuffer`). A "fusion" mode calls several models and a judge model (`combo.js:820 handleFusionChat`), which is LLM-in-the-loop routing.
- No session affinity for cache locality, no rate-limit-header parsing and no concurrency gates were found in the files read.

### 3.5 Protocol translation

Hub-and-spoke with OpenAI as the intermediate: `translateRequest` goes source to `openai` to target unless a direct pair is registered (`translator/index.js:106-125`). Translators exist for Claude, Gemini, Vertex, Responses, Kiro, Cursor, CommandCode, Antigravity and Ollama (`translator/request/*`, `translator/response/*`). All bodies are untyped JavaScript objects. Provider quirks that affect translation are declared in the provider registry (`PROVIDERS[provider]?.quirks?.preserveCacheControl`, `translator/index.js:102`).

### 3.6 Streaming, usage, management

Streams are SSE with AWS EventStream decoding for Kiro and Bedrock (`utils/awsEventStream.js`, `awsSigv4.js`) and Protobuf for Cursor (`utils/cursorProtobuf.js`). Usage is recorded in `src/lib/usageDb.js` and `requestDetailsDb.js` with pricing in `providers/pricing.js` (cost roll-up not read). Management is the Next.js dashboard and a CLI (`cli/src`), a tunnel module and an updater (`src/lib/tunnel`, `appUpdater.js`).

### 3.7 RTK token saver

`open-sse/rtk/` compresses `tool_result` content in the request body (Chat, Responses `function_call_output`, Kiro history) using filters selected by `autodetect.js` and applied with `safeApply`, guarded by `MIN_COMPRESS_SIZE` and `RAW_CAP` (`rtk/index.js:1-45`). The directory also holds prompt-style modes named `caveman`, `ponytail`, `headroom`, `pxpipe` that inject instructions (`rtk/systemInject.js`), which are prompt changes, not deterministic filters. README claims 20 to 40 percent token saving (`README.md:38`, not measured).

### 3.8 Dependencies on unofficial endpoints, pooling and similar (neutral description)

This repository leans hardest on non-API access of the five. It includes web-cookie providers (`executors/chatgpt-web.js`, `deepseek-web.js`) served through a shared stealth Chromium pool whose header says it exists to satisfy Cloudflare and Turnstile checks that plain HTTP cannot pass (`services/browserPool.js:1-12`); a TLS-impersonating client that sends a Chrome-like handshake to get past Cloudflare for grok.com (`services/grokTlsClient.js:1-10`); Claude request "cloaking" that generates the billing header expected from current Claude Code clients with a fixed CLI entrypoint (`utils/claudeCloaking.js:9-17`, `claudeCliVersion.js`); a local MITM server on port 443 with generated certificates that intercepts IDE traffic for chosen hosts and rewrites model names (`src/mitm/server.js:TARGET_HOSTS`, `MODEL_SYNONYMS`, `cert/generate`); Cursor checksum and Protobuf emulation (`utils/cursorChecksum.js`, `cursorProtobuf.js`); and multi-account rotation with proxy pools. The README's stated goals are to "use every bit before reset" and to fall back to free models (`README.md:38-41`). For OMP2 all of these are classified as rejected (section 3.12).

### 3.9 What it does well

- Error classification as data: one rules table with status, text, provider, cooldown, backoff, model scope and request scope (`errorConfig.js:ERROR_RULES`), the closest match to OMP2's providers-as-data rule among the five.
- Layered capability resolution with an explicit precedence order and a documented update procedure (`providers/capabilities.js:1-30`).
- Zero-content failover that peeks a stream without losing bytes (`emptyFailover.js`).
- Deterministic tool-output compression with a size floor (`rtk/index.js`).
- Separation of `requestScoped` from credential faults.

### 3.10 Risks

- Anti-bot circumvention, TLS fingerprint mimicry, MITM and client impersonation create provider-terms, account and legal exposure and a large attack surface (section 3.8).
- Connection secrets stored without found encryption.
- Untyped translation and ad-hoc quirks across 255 k lines of JavaScript.
- Fusion mode puts an LLM judge on the routing path.
- Error rules match message substrings, which are provider-text dependent.

### 3.11 Licence implications

MIT. Ideas may be reused freely; nothing is copied here. The MITM and cloaking components would be a licence-compatible but policy-incompatible import.

### 3.12 Ideas worth adopting in OMP2's existing provider runtime

| # | Idea | Evidence | Fit with locks | Suggested handling |
|---|---|---|---|---|
| N1 | Error classification as a declarative rules table (status, text, provider, cooldown, backoff, model scope, request scope) | `config/errorConfig.js`, `accountFallback.js:checkFallbackError` | Matches providers-as-data if the table lives in catalog/KDL, keyed on status and typed codes first; text rules only as data | Compare with OMP2 classifier; extend with a data-driven table if gaps exist |
| N2 | `requestScoped` versus credential-scoped failure so fallback still tries the next member | `accountFallback.js:21-27` | Fine | Cheap semantic split, deterministic |
| N3 | Per-model cooldown lock on one credential | `accountFallback.js:131-180` | Fine | Avoids disabling a whole credential for one model's quota |
| N4 | Capability resolution with explicit precedence and a synced external catalog as data | `providers/capabilities.js:1-30` | Fine if catalog stays KDL and sync is an extension (T2) | Candidate for G06/G07; benchmark nothing, deterministic |
| N5 | Deterministic tool-output filters selected by content type with a size floor | `rtk/index.js`, `rtk/autodetect.js` | OMP2 already has a minimizer (ADR 0049 D6 class C names it); compare, do not duplicate | Likely `Unnecessary or redundant` unless the PRV/TOK audit finds missing filters |
| N6 | Zero-content stream detection before committing a response | `emptyFailover.js:scanSSEBuffer` | Fits canonical semantic events (inspect the first events, not text) | Reliability idea for fallback; measure first |

### 3.13 Ideas to avoid

| # | Idea | Why |
|---|---|---|
| A1 | Stealth browser pools, TLS fingerprint mimicry, Cloudflare/Turnstile solving | Circumvents provider access controls. Rejected for security, licensing or operational reasons |
| A2 | Local MITM with generated certificates for IDE traffic and model rewriting | Traffic interception and trust-store changes; Rejected for security, licensing or operational reasons |
| A3 | Client cloaking (billing header, CLI version, tool-name suffixes) | Official-client impersonation; Rejected |
| A4 | Multi-account rotation with proxy pools to maximise free or subscription quota | Pooling to stretch limits; Rejected |
| A5 | LLM judge "fusion" on the routing path | Non-deterministic and costly; Requires benchmarking at best, outside the owner's deterministic preference |
| A6 | Prompt-style compression modes (caveman, ponytail variants) injected into system prompts | Prompt changes break stable-prefix cache rules (volatile text in stable prefix) and are not deterministic filters; Unnecessary or redundant |

Back to the [index](../README.md).
