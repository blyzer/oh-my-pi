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

Back to the [index](../README.md).
