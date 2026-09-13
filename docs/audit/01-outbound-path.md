# 01 — Outbound Data Path Audit

**Scope:** message construction → HTTP request leaving the machine, in `oh-my-pi`.
**Method:** read-only source tracing. Every claim below cites `file:line` and the symbol. Findings are marked OBSERVED (code was read) or MISSING (searched, does not exist).
**Date:** 2026-09-12

---

## 1. The real call/data flow

### 1.1 Agent loop → provider dispatch

**Finding:** The main turn assembles its outbound `Context` in one function and hands it to an injectable `streamFn`.

**Evidence / Runtime path:**

1. `packages/agent/src/agent-loop.ts:1598` — `prepareProviderCall(context, config, signal)`. This is where `AgentMessage[]` becomes the provider `Context`:
   - `agent-loop.ts:1604-1606` — `config.transformContext(messages, signal)` (host pre-LLM message transform).
   - `agent-loop.ts:1608` — `config.convertToLlm(messages)` → `Message[]`.
   - `agent-loop.ts:1609` — `normalizeMessagesForProvider(llmMessages, model)`.
   - `agent-loop.ts:1616-1630` — builds `llmContext: Context` (systemPrompt + messages + tools).
   - `agent-loop.ts:1625-1627` — **`config.transformProviderContext(llmContext, model)`** — the last host-owned hook that sees the whole `Context` as a typed object.
2. `packages/agent/src/agent-loop.ts:1663` — `const streamFunction = streamFn || streamSimple;` inside `streamAssistantResponse` (`agent-loop.ts:1646`). Default is `streamSimple` from `@oh-my-pi/pi-ai`.
3. `packages/agent/src/agent.ts:470` — `this.streamFn = opts.streamFn || streamSimple;` and `agent.ts:1528-1530` — `agentLoop(...)/agentLoopContinue(...)` are called with `this.streamFn`.

**Observed behavior:** `transformProviderContext` is the single host hook applied to the final `Context`. In the coding agent it is wired at `packages/coding-agent/src/sdk.ts:3447-3452` and does, in order: secret obfuscation (`obfuscateProviderContext`, `sdk.ts:3448`), inline snapcompact (`sdk.ts:3449`), image clamping (`sdk.ts:3450`), image normalization (`sdk.ts:3451`).

**Confidence:** HIGH

---

### 1.2 Host stream wrapper chain (coding agent)

**Finding:** The coding agent wraps `streamSimple` in three layers, none of which inspect or filter content.

**Evidence:**
- `packages/coding-agent/src/sdk.ts:3509-3512` —
  ```
  const settingsAwareStreamFn = wrapStreamFnWithBlobUrlFallback(
      wrapStreamFnWithProviderConcurrency(settings, createSettingsAwareStreamFn(settings)),
      blobBroker,
  );
  ```
- `packages/coding-agent/src/session/settings-stream-fn.ts:30` — `createSettingsAwareStreamFn(settings, base: StreamFn = streamSimple)` — injects routing/provider settings only.
- `packages/coding-agent/src/task/provider-concurrency.ts:76` — `wrapStreamFnWithProviderConcurrency` — semaphore only.
- `packages/coding-agent/src/blob-broker/stream-fallback.ts:22` — `wrapStreamFnWithBlobUrlFallback` — image/provider-file recovery only.
- `packages/coding-agent/src/sdk.ts:3564` / `sdk.ts:3580` — the agent's `streamFn` closure calls `settingsAwareStreamFn(streamModel, context, {...})`.
- Same function is reused as `sideStreamFn` and `advisorStreamFn` at `packages/coding-agent/src/sdk.ts:3784-3785`.

**Observed behavior:** OBSERVED — these wrappers mutate `options`, never `context.messages` or `context.systemPrompt`. No filtering.

**Confidence:** HIGH

---

### 1.3 pi-ai entry: `streamSimple` → `stream` → `streamDispatch`

**Finding:** All in-process provider traffic that uses the pi-ai abstraction funnels through three functions in `packages/ai/src/stream.ts`.

**Evidence / Runtime path:**
- `packages/ai/src/stream.ts:1440` — `export function streamSimple<TApi>(model, context, options)`. Applies `applyGlyphCodec(context)` (`stream.ts:1447`) for glyph-tokenizing models.
- `packages/ai/src/stream.ts:1483` — `streamSimpleRequest(...)`; `stream.ts:1488` — `const requestOptions = withTransportFetch(model, (options || {}) as SimpleStreamOptions);` — **first injection of the central transport**.
- `packages/ai/src/stream.ts:1733` — `export async function completeSimple(...)` — the non-streaming façade; it just calls `streamSimple` (`stream.ts:1747`). Every oneshot in the repo goes through it.
- `packages/ai/src/stream.ts:874` — `export function stream<TApi>(model, context, options)` → `stream.ts:895` `streamDispatch`.
- `packages/ai/src/stream.ts:900` — `const requestOptions = withTransportFetch(model, (options || {}) as StreamOptions)` — idempotent second application (stamped symbol, `packages/ai/src/utils/transport-fetch.ts:9`).
- `packages/ai/src/stream.ts:903-906` — **custom-API escape hatch**: `getCustomApi(model.api)` → `customApiProvider.stream(model, context, requestOptions)`. Returns before any built-in encoder runs.
- `packages/ai/src/stream.ts:953-1025` — the `switch (api)` that fans out to `streamAnthropic`, `streamOpenAICompletions`, `streamOpenAIResponses`, `streamOpenAICodexResponses`, `streamAzureOpenAIResponses`, `streamGoogle`, `streamGoogleGeminiCli`, `streamOllama`, `streamCursor`, `streamDevin`; plus early exits for GitLab Duo (`stream.ts:908-928`), Vertex (`stream.ts:931`) and Bedrock (`stream.ts:934`).

**Confidence:** HIGH

---

### 1.4 Per-provider transform: `transformMessages`

**Finding:** Each provider encoder independently calls `transformMessages`, which is the only place in `packages/ai` that performs any content rewriting of message text. It is NOT a chokepoint — it is a convention, and several providers skip it.

**Evidence:** `packages/ai/src/providers/transform-messages.ts:588` — `export function transformMessages<TApi>(messages, model, normalizeToolCallId?, ...)`.

Callers (OBSERVED):

| Encoder | Call site |
|---|---|
| Anthropic Messages | `packages/ai/src/providers/anthropic.ts:4757` |
| OpenAI Responses (shared) | `packages/ai/src/providers/openai-shared.ts:1957` |
| OpenAI Chat Completions | `packages/ai/src/providers/openai-completions.ts:1986` |
| OpenAI Codex Responses | `packages/ai/src/providers/openai-codex-responses.ts:4545` |
| Google GenAI (also Vertex, Gemini CLI) | `packages/ai/src/providers/google-shared.ts:167` |
| Amazon Bedrock Converse | `packages/ai/src/providers/amazon-bedrock.ts:961` |
| Ollama chat | `packages/ai/src/providers/ollama.ts:255` |
| Devin/Cascade | `packages/ai/src/providers/devin.ts:173` |
| OpenAI remote compaction | `packages/agent/src/compaction/openai.ts:513` |

Providers that do NOT call it (MISSING — grepped `transform-messages` across `packages/ai/src/providers`):

- `packages/ai/src/providers/cursor.ts` — builds its own protobuf blob store (`cursor.ts:4903` `buildRootPromptMessagesJson`, `cursor.ts:5346` `buildConversationTurns`) directly from `context.messages` (`cursor.ts:5304-5358`).
- `packages/ai/src/providers/gitlab-duo-workflow.ts` — builds a bare ChatML goal transcript (`gitlab-duo-workflow.ts:2582`); its own comment at `gitlab-duo-workflow.ts:2583` states *"The goal transcript bypasses transformMessages"*, and it compensates with a direct `redactSensitiveCredentials` call at `gitlab-duo-workflow.ts:2587` and `:2589`.
- `packages/ai/src/providers/pi-native-client.ts` — serializes the raw `context` object verbatim (`pi-native-client.ts:186`).

**Confidence:** HIGH

---

### 1.5 What `transformMessages` actually does to content

**Finding:** A single opt-in, regex-based credential redaction pass — **disabled by default** — plus structural normalization (tool-call IDs, thinking blocks, malformed-call sanitization).

**Evidence:**
- `packages/ai/src/providers/transform-messages.ts:599` — `messages = redactSensitiveCredentialsInMessages(messages);` is the FIRST statement in `transformMessages`.
- `packages/ai/src/providers/transform-messages.ts:505` — `redactSensitiveCredentialsInMessages(messages)`; guard at `:506` `if (!credentialRedactionEnabled) return messages;`.
- `packages/ai/src/providers/transform-messages.ts:444` — `let credentialRedactionEnabled = false;` — **off by default**.
- `packages/ai/src/providers/transform-messages.ts:452` — `configureCredentialRedaction(enabled)`; the only production caller is `packages/coding-agent/src/config/settings.ts:3112-3113` (`"secrets.enabled": value => configureCredentialRedaction(value === true)`), reset to `false` at `settings.ts:3287`.
- `packages/coding-agent/src/config/settings-schema.ts:5478-5480` — `"secrets.enabled"` has `default: false`.
- `packages/ai/src/providers/transform-messages.ts:418-419` — `SENSITIVE_TOKEN_RE` matches only `gh[opusr]_`, `github_pat_`, `glpat-`, `sk-proj-`, `sk-ant-`, `sk-` shapes; `transform-messages.ts:421` `hasPlausibleCredentialEntropy` further gates matches.
- `packages/ai/src/providers/transform-messages.ts:456` `redactSensitiveCredentials`, `:477` `redactSensitiveInObject`.
- Coverage inside the pass (`transform-messages.ts:507-585`): user/developer text, toolResult text, assistant text/thinking/toolCall arguments. **Images, provider payloads, and tool definitions are not walked.**
- System prompts take the same scrub via a different route: `packages/ai/src/utils.ts:32` — `normalizeSystemPrompts` maps `redactSensitiveCredentials(prompt.toWellFormed())`.

**Conclusion:** Out of the box (`secrets.enabled=false`), `transformMessages` performs **zero** content redaction. It is a structural normalizer.

**Confidence:** HIGH

---

### 1.6 Final wire body: `onPayload`

**Finding:** After encoding, each provider offers exactly one mutation hook on the fully-built wire body, immediately before serialization.

**Evidence (OBSERVED, all are `options?.onPayload?.(body, model)` with the return value replacing the body):**

| Provider | Hook site | Serialize/send site |
|---|---|---|
| Anthropic Messages | `packages/ai/src/providers/anthropic.ts:2479` | `anthropic.ts:2483` `toWellFormedDeep`, then `packages/ai/src/providers/anthropic-client.ts:234` `JSON.stringify(params)` |
| OpenAI Chat Completions | `packages/ai/src/providers/openai-completions.ts:779` | `packages/ai/src/utils/openai-http.ts:91-94` |
| OpenAI Responses | `packages/ai/src/providers/openai-responses.ts:530` | same `openai-http.ts:91-94` |
| Azure OpenAI Responses | `packages/ai/src/providers/azure-openai-responses.ts:132` | same |
| OpenAI Codex (HTTP) | `packages/ai/src/providers/openai-codex-responses.ts:1952` | `openai-http.ts:91-94` |
| OpenAI Codex (WebSocket) | `packages/ai/src/providers/openai-codex-responses.ts:1824` | `openai-codex-responses.ts:3873` `streamRequest` on the raw socket |
| Google GenAI / Vertex | `packages/ai/src/providers/google-shared.ts:960` | `google-shared.ts:975` `JSON.stringify(paramsToWireBody(params))`, sent `google-shared.ts:977-982` |
| Google Gemini CLI | `packages/ai/src/providers/google-gemini-cli.ts:577` | — |
| Ollama | `packages/ai/src/providers/ollama.ts:564` | — |
| Amazon Bedrock | `packages/ai/src/providers/amazon-bedrock.ts:450` | `amazon-bedrock.ts:552` `fetchWithRetry` |
| Cursor | `packages/ai/src/providers/cursor.ts:5433` | `cursor.ts:759`/`:763` `http2.connect` |

**Providers with NO `onPayload` hook (MISSING):** `packages/ai/src/providers/devin.ts`, `packages/ai/src/providers/gitlab-duo-workflow.ts`, `packages/ai/src/providers/pi-native-client.ts` (explicitly excluded from the wire options at `pi-native-client.ts:46`).

**Host wiring:** `packages/coding-agent/src/sdk.ts:3466-3468` — `onPayload` forwards to `extensionRunner.emitBeforeProviderRequest(payload, model)` (`packages/coding-agent/src/extensibility/extensions/runner.ts:1665`). It is chained, not replaced, at `packages/coding-agent/src/session/session-provider-boundary.ts:178-188`.

**Conclusion:** `onPayload` is the last point where outbound content is still a **structured, typed object**. It is NOT universal (3 providers skip it) and it is an extension hook, not a policy chokepoint.

**Confidence:** HIGH

---

### 1.7 The HTTP client layer

**Finding:** There is one shared fetch decorator, `transportFetch`, plus three transports that never use it.

**Evidence:**
- `packages/ai/src/utils/transport-fetch.ts:25` — `export function transportFetch(model, fetchImpl)`; `transport-fetch.ts:47` — `withTransportFetch(model, options)`.
- `packages/ai/src/utils/transport-fetch.ts:29-30` — base is `coworkFetch` for official Anthropic Messages, otherwise `globalThis.fetch`.
- `packages/ai/src/utils/transport-fetch.ts:32-40` — the returned closure. Per call it applies the inference User-Agent (`:33`), `NODE_EXTRA_CA_CERTS` (`:34-35`), the per-provider proxy (`:36`), and `PI_REQ_DEBUG` recording (`:37-39`), then `return base(input, init)`.
- Only two call sites: `packages/ai/src/stream.ts:900` and `packages/ai/src/stream.ts:1488`. Idempotency is enforced by the `TRANSPORT_FETCH` symbol stamp (`transport-fetch.ts:9,26-27,42`).

Actual byte-emitting calls reached through `options.fetch`:

- `packages/ai/src/providers/anthropic-client.ts:285` — `await fetchFn(url, { method POST, headers, body, signal })`, where `fetchFn = opts.fetch ?? fetch` (`anthropic-client.ts:224`) and `body = JSON.stringify(params)` (`anthropic-client.ts:234`).
- `packages/ai/src/utils/openai-http.ts:90-94` — `postOpenAIStream` → `fetchWithRetry(init.url, { method: "POST", body: JSON.stringify(init.body) })`.
- `packages/ai/src/providers/google-shared.ts:976-982` — `fetchImpl = plan.fetch ?? options?.fetch ?? globalThis.fetch`, then POST.
- `packages/ai/src/providers/amazon-bedrock.ts:552-558` — `fetchWithRetry(url, { fetch: options.fetch })`.
- `packages/ai/src/providers/cowork-fetch.ts:214` — `coworkFetch`, the Anthropic-profiled `node:https` transport (`cowork-fetch.ts:161` `https.request`). It is the *base* of `transportFetch`, so it is still downstream of the decorator.

Transports that **never touch a `FetchImpl`**:

- `packages/ai/src/providers/cursor.ts:759` / `cursor.ts:763` — `http2.connect(baseUrl, ...)`. Raw HTTP/2, `options.fetch` is ignored.
- `packages/ai/src/providers/openai-codex-responses.ts:3717-3722` — `new WebSocket(this.#url, { headers, proxy })`. Frames are written directly to the socket.
- `packages/ai/src/providers/gitlab-duo-workflow.ts` — WebSocket-based flow transport.

**Confidence:** HIGH

---

## 2. THE LAST MODIFIABLE POINT (most important finding)

**Finding:** The last point in the runtime where outbound content can still be modified before transmission is the anonymous fetch closure returned by `transportFetch`.

**Evidence:** `packages/ai/src/utils/transport-fetch.ts:32-40`, inside `transportFetch` (`packages/ai/src/utils/transport-fetch.ts:25`):

```ts
const fetch: TransportFetch = async (input, init) => {         // :32
    init = withInferenceUserAgent(input, init);                // :33
    const extraCa = resolveExtraCa();                          // :34
    if (extraCa) init = withExtraCaInit(init, extraCa);        // :35
    if (proxyUrl) init = withProxyInit(input, init, proxyUrl); // :36
    if (!isRequestDebugEnabled()) return base(input, init);    // :37  <-- LAST WRITABLE MOMENT
    const session = await createFetchRequestDebugSession(input, init); // :38
    return session.wrapResponse(await base(input, init));       // :39
};
```

At line 37/39, `init.body` holds the **fully serialized JSON request body**. This is the last instruction under repo control before the runtime's socket write. A privacy filter would have to be inserted here (rewrite `init.body` before `base(input, init)`), or — if a structured rather than byte-level filter is wanted — at the `onPayload` invocations enumerated in §1.6.

**Runtime path to it:** `agent-loop.ts:1663 streamFunction` → `stream.ts:1440 streamSimple` → `stream.ts:1488 withTransportFetch` → `stream.ts:874 stream` → `stream.ts:900 withTransportFetch` (no-op, stamped) → `streamDispatch` switch → provider encoder → provider HTTP client → `options.fetch` === this closure.

**Observed behavior:** The closure today performs **no content inspection whatsoever**. It reads `init` only to append a UA header, CA bundle, and proxy. `createFetchRequestDebugSession` (`packages/ai/src/utils/request-debug.ts:85`) *reads* the body to disk under `PI_REQ_DEBUG`, but does not modify it.

**Conclusion:** `transportFetch`'s closure is the last modifiable point — **but it is not a complete boundary**. Content reaching Cursor (`cursor.ts:759`), Codex-over-WebSocket (`openai-codex-responses.ts:3717`), and GitLab Duo Workflow never passes through it, because those transports bypass `FetchImpl` entirely. And multiple subsystems (§3) never enter `packages/ai/src/stream.ts` at all.

**Confidence:** HIGH

**centralBoundary = `packages/ai/src/utils/transport-fetch.ts:transportFetch`** (closure at lines 32-40; effective write point line 37).

---

## 3. Every distinct outbound path to an external model

### 3.1 Main agent turn — THROUGH the boundary
`packages/coding-agent/src/sdk.ts:3564` → `sdk.ts:3580 settingsAwareStreamFn` → `packages/ai/src/stream.ts:1440 streamSimple` → `stream.ts:1488 withTransportFetch`. OBSERVED. **No bypass** (except when the model is Cursor/Codex-WS, whose transport skips the fetch layer while still passing `streamDispatch`).

### 3.2 Subagents (`packages/coding-agent/src/task`) — THROUGH the boundary
- `packages/coding-agent/src/task/executor.ts:3542` — `createAgentSession(buildSubagentSessionOptions(...))`; revive at `executor.ts:3580`.
- `packages/coding-agent/src/task/persisted-revive.ts:133` — `createAgentSession({...})`.
- Each rebuilds the full SDK session, so it re-enters `sdk.ts:3509` and gets its own `settingsAwareStreamFn`.
- **Caveat (OBSERVED):** grep for `obfuscator` / `secrets.enabled` in `packages/coding-agent/src/task/executor.ts` returns **no matches**. The subagent gets an obfuscator only because `createAgentSession` rebuilds one from settings at `sdk.ts:1481-1483`; nothing is inherited from the parent.

### 3.3 Advisors — THROUGH the boundary
`packages/coding-agent/src/sdk.ts:3785 advisorStreamFn: settingsAwareStreamFn` → consumed at `packages/coding-agent/src/session/session-advisors.ts:1010`, with `transformProviderContext` propagated at `session-advisors.ts:1014`. Extra advisor-only obfuscation at `packages/coding-agent/src/advisor/runtime.ts:722-736` and `advisor/delta-split.ts:70-90`.

### 3.4 Local compaction summarizer — THROUGH the boundary
`packages/agent/src/compaction/compaction.ts:993 instrumentedCompleteSimple` (also `:1204`, `:2036`, `:1109` handoff) → `packages/agent/src/telemetry.ts:1744` `const complete = span.completeImpl ?? completeSimple` → `packages/ai/src/stream.ts:1733 completeSimple` → `streamSimple`.
The coding agent overrides `completeImpl` to route through the session transport: `packages/coding-agent/src/session/session-maintenance.ts:2727-2730` (`this.#host.sideStreamFn(...)`), and `sideStreamFn` is `settingsAwareStreamFn` (`sdk.ts:3784`).

### 3.5 OpenAI remote compaction (V1) — **BYPASSES the boundary**
`packages/agent/src/compaction/compaction.ts:1765 requestOpenAiRemoteCompaction` → `packages/agent/src/compaction/openai.ts:760`, which sends at **`packages/agent/src/compaction/openai.ts:857`**: `await (opts?.fetch ?? fetch)(endpoint, { method: "POST", headers, body: stringifyJson(request) })`.
`summaryOptions.fetch` is `undefined` on this path (`compaction.ts:1773` passes `fetch: summaryOptions.fetch`, and `session-maintenance.ts:2698-2731` sets `completeImpl` but **never** `fetch`). So it lands on bare `globalThis.fetch`. It does call `transformMessages` (via `buildOpenAiNativeHistory`, `compaction/openai.ts:513`), but **never** `transportFetch`.

### 3.6 OpenAI/Codex remote compaction V2 streaming — **BYPASSES the boundary**
`packages/agent/src/compaction/compaction.ts:1720 requestCompactionV2Streaming` → `packages/agent/src/compaction/compaction-v2-streaming.ts:259`; `compaction-v2-streaming.ts:278` — `const fetchImpl = options?.fetch ?? globalThis.fetch;` → POST at **`compaction-v2-streaming.ts:371`**. Same `undefined` fetch on the session path.

### 3.7 Generic `compaction.remoteEndpoint` — **BYPASSES the boundary**
`packages/agent/src/compaction/openai.ts:925 requestRemoteCompaction` → POST at **`packages/agent/src/compaction/openai.ts:956`** via `(opts?.fetch ?? fetch)`. Called from `compaction.ts:985-987` and `compaction.ts:1196-1198`. This endpoint is **operator-configurable** (an arbitrary URL) and ships the raw serialized conversation (`compaction.ts:875 wholeConversation` via `serializeConversationForSummary`) as `request.prompt`.

### 3.8 Anthropic native (server-side) compaction — THROUGH the boundary
`packages/agent/src/compaction/anthropic.ts:252 requestAnthropicNativeCompaction` → `anthropic.ts:258 instrumentedCompleteSimple` → `completeSimple` → `streamSimple`. Unlike the OpenAI lanes, this one reuses the normal stream stack.

### 3.9 `packages/snapcompact` — NO network egress
Grep for `streamSimple|streamFn|completeSimple|fetch(` in `packages/snapcompact/src` returns **no matches** (MISSING). `snapcompact.compact` (`packages/snapcompact/src/snapcompact.ts:2038`) renders conversation text to PNG frames locally (`snapcompact.ts:1619 render`, `:1655 renderMany`). Its *output* (image blocks, `snapcompact.ts:1821 images`) is then carried by whichever path sends the context. Called from `packages/coding-agent/src/session/session-maintenance.ts:996`, `:3320`, `:3909`.

### 3.10 MCP servers (`packages/coding-agent/src/mcp`) — separate egress, not a model call
- HTTP transport: `packages/coding-agent/src/mcp/transports/http.ts:115 #fetch` → `packages/coding-agent/src/mcp/transports/header-policy.ts:103` / `:111` — plain `fetch(...)`.
- SSE transport: `packages/coding-agent/src/mcp/transports/sse.ts:59 #fetch` → `mcpFetch`.
- JSON-RPC probe: `packages/coding-agent/src/mcp/json-rpc.ts:93` — `await fetch(url, {...})`.
- OAuth/registry traffic: `mcp/oauth-flow.ts:506`, `mcp/oauth-discovery.ts:406`, `mcp/smithery-registry.ts:321`, `mcp/smithery-connect.ts:66`.
- **MCP sampling is not implemented**: `packages/coding-agent/src/mcp/types.ts:191` declares `sampling?: Record<string, never>` as a capability type only; grep for `createMessage` in `packages/coding-agent/src/mcp` returns **no matches** (MISSING). An MCP server cannot currently drive a model call back through omp.
- MCP tool ARGUMENTS and RESULTS still reach the model via the normal turn, so they are governed by §3.1. But MCP server traffic itself never passes `transportFetch`.

### 3.11 Extensions / custom provider APIs — **BYPASSES the boundary (by design)**
- `packages/coding-agent/src/config/model-registry.ts:2619-2623` — `registerCustomApi(config.api, streamSimple, sourceId, ...)` where `streamSimple` is **extension-supplied** (`model-registry.ts:2869`, public type at `packages/coding-agent/src/extensibility/extensions/types.ts:1560`).
- `packages/ai/src/api-registry.ts:72 registerCustomApi`, `:89 getCustomApi`.
- Dispatch: `packages/ai/src/stream.ts:903-906` returns `customApiProvider.stream(model, context, requestOptions)` **before** the built-in switch. `requestOptions.fetch` is the `transportFetch` closure, but an extension's implementation is free to ignore it and call `globalThis.fetch` directly. Nothing enforces use.
- `packages/coding-agent/src/extensibility/legacy-pi-ai-shim.ts:161` re-exports `streamSimple` directly to extension code.

### 3.12 Provider file / blob uploads — **BYPASSES the boundary**
Raw file bytes are POSTed to provider file APIs from outside pi-ai:
- `packages/coding-agent/src/blob-broker/provider-files-anthropic.ts:114` → `https://api.anthropic.com/v1/files` (`provider-files-anthropic.ts:6`).
- `packages/coding-agent/src/blob-broker/provider-files-openai.ts:91` → `https://api.openai.com/v1/files` (`provider-files-openai.ts:6`).
- `packages/coding-agent/src/blob-broker/provider-files-gemini.ts:95` and `:121` → `generativelanguage.googleapis.com` (`provider-files-gemini.ts:6`).
- Fetch source: `packages/coding-agent/src/blob-broker/uploader-runtime.ts:92-93` — `config.fetch ?? globalThis.fetch`. Never `transportFetch`.

### 3.13 Web search providers — **BYPASSES the central boundary**
- `packages/coding-agent/src/web/search/providers/perplexity.ts:518` and `:529` call `streamOpenAIResponses` / `streamOpenAICompletions` **directly**, skipping `stream.ts:874 stream` and therefore skipping `withTransportFetch`. They pass their own `fetch: fetchImpl` (`perplexity.ts:523`, `:534`).
- `packages/coding-agent/src/web/search/providers/gemini.ts:496`, `kagi.ts:95`, `kimi.ts:218` similarly inject their own `fetchImpl`.
- Scrapers use bare `fetch`: `web/scrapers/github.ts:133`, `web/scrapers/docs-rs.ts:389`, `web/scrapers/types.ts:169`, `web/scrapers/utils.ts:69`.

### 3.14 Background / async oneshot model calls — THROUGH `completeSimple`, but with NO session obfuscation
All of these call `completeSimple` (or `instrumentedCompleteSimple`) directly with content they assemble themselves, so they reach `transportFetch` — but they do **not** pass through `transformProviderContext` (§1.1) and therefore get **no secret obfuscation**:

| Subsystem | Call site |
|---|---|
| Session title generation | `packages/coding-agent/src/utils/title-generator.ts:284` |
| Auto-thinking classifier | `packages/coding-agent/src/auto-thinking/classifier.ts:148` |
| Commit message generation | `packages/coding-agent/src/utils/commit-message-generator.ts:110` |
| Conventional-commit inference | `packages/coding-agent/src/commit/conventional/inference.ts:119` |
| Changelog generation | `packages/coding-agent/src/commit/changelog/generate.ts:61` |
| Edit auto-repair | `packages/coding-agent/src/edit/auto-repair.ts:318` |
| Sharpshooter extract / consolidate | `packages/coding-agent/src/sharpshooter/extract.ts:216`, `sharpshooter/consolidate.ts:157` |
| TTS speech enhancer | `packages/coding-agent/src/tts/speech-enhancer.ts:88` |
| Image question / vision fallback | `packages/coding-agent/src/utils/image-question.ts:131`, `utils/image-vision-fallback.ts:131` |
| `read` tool image describe | `packages/coding-agent/src/tools/read.ts:731` |
| git-tui AI staging | `packages/coding-agent/src/git-tui/ai-stage.ts:230` |
| eval completion bridge | `packages/coding-agent/src/eval/completion-bridge.ts:166` |
| auth-gateway connectivity probe | `packages/coding-agent/src/cli/auth-gateway-cli.ts:505` |
| bench / dry-balance / if-bench | `cli/bench-cli.ts:917`, `cli/dry-balance-cli.ts:794`, `if-bench/index.ts:123` |

### 3.15 ADW workflow runner — subprocess, THROUGH a fresh session
`packages/coding-agent/src/adw/runner.ts:700 createExecutorSeatRunner` → `adw/runner.ts:763-773` `runSubprocess(buildSeatSpawnOptions(...))` or `adw/runner.ts:745` `runSubagentFollowUpTurn`. Each spawned seat is a new omp session and re-enters §3.1/§3.2 in its own process.

### 3.16 pi-native gateway transport — THROUGH the boundary, ships `context` verbatim
`packages/ai/src/providers/pi-native-client.ts:148 streamPiNative`; `pi-native-client.ts:179 const fetchImpl = options?.fetch ?? globalThis.fetch` (receives `transportFetch` when dispatched via `stream.ts`); body at `pi-native-client.ts:186-191` is `JSON.stringify({ modelId, context, options, stream: true })` — **the entire `Context` object, unencoded and unfiltered**, forwarded to an `omp auth-gateway` which then re-dispatches (`packages/ai/src/providers/pi-native-server.ts:100`).

---

## 4. Would a policy at one central point cover all paths?

# NO.

A filter installed at `packages/ai/src/utils/transport-fetch.ts:32-40` (the centralBoundary) would **not** cover all outbound traffic. The following are independent egress points that never execute that closure:

1. **OpenAI remote compaction V1** — `packages/agent/src/compaction/openai.ts:857` (`(opts?.fetch ?? fetch)`; `fetch` is `undefined` on the session path).
2. **Compaction V2 streaming** — `packages/agent/src/compaction/compaction-v2-streaming.ts:371` (fetch resolved at `:278` to `globalThis.fetch`).
3. **Generic `compaction.remoteEndpoint`** — `packages/agent/src/compaction/openai.ts:956`. Operator-configurable arbitrary URL carrying the serialized transcript.
4. **Cursor provider** — `packages/ai/src/providers/cursor.ts:759` / `:763`, raw `http2.connect`; `options.fetch` is never consulted.
5. **OpenAI Codex WebSocket transport** — `packages/ai/src/providers/openai-codex-responses.ts:3717`, `new WebSocket(...)`; frames written at `openai-codex-responses.ts:3873`.
6. **GitLab Duo Workflow** — WebSocket flow transport; goal transcript built at `packages/ai/src/providers/gitlab-duo-workflow.ts:2582`, explicitly noted at `:2583` as bypassing `transformMessages`.
7. **Extension-registered custom APIs** — `packages/ai/src/stream.ts:903-906` dispatches to third-party code (`packages/coding-agent/src/config/model-registry.ts:2621`); use of the injected `fetch` is voluntary.
8. **Provider file uploads** — `packages/coding-agent/src/blob-broker/provider-files-anthropic.ts:114`, `provider-files-openai.ts:91`, `provider-files-gemini.ts:95`/`:121`; fetch from `uploader-runtime.ts:93`.
9. **Web search model providers** — `packages/coding-agent/src/web/search/providers/perplexity.ts:518`/`:529` call provider encoders directly, skipping `stream.ts:874`.
10. **MCP server traffic** — `packages/coding-agent/src/mcp/transports/header-policy.ts:103`/`:111`, `mcp/json-rpc.ts:93`.
11. **Web scrapers** — `packages/coding-agent/src/web/scrapers/github.ts:133`, `docs-rs.ts:389`, `types.ts:169`, `utils.ts:69`.

Additionally, even for paths that DO traverse `transportFetch`, the **content-level** protections are not centralized either:

- Secret obfuscation lives at `packages/coding-agent/src/sdk.ts:3448` inside `transformProviderContext`, which is only invoked from `packages/agent/src/agent-loop.ts:1625-1627` and `packages/agent/src/agent.ts:801` — i.e. **agent-loop turns only**. Every oneshot in §3.14 skips it.
- Credential regex redaction lives at `packages/ai/src/providers/transform-messages.ts:599`, is per-encoder (§1.4 lists 3 providers that skip it), and is **off by default** (`transform-messages.ts:444`, `settings-schema.ts:5480`).

**Conclusion:** omp has **at least 11 independent egress points**. There is no single boundary. A privacy policy would need enforcement at both `transportFetch` (byte level, covers the fetch-based majority) **and** the non-fetch transports and out-of-band subsystems listed above.

**Confidence:** HIGH

---

## 5. Summary table

| Component | Provider | Protocol | Data sent | Filtering before send | Can bypass central filtering? |
|---|---|---|---|---|---|
| Main agent turn (`agent-loop.ts:1663`) | all built-ins | HTTPS/SSE via `transportFetch` | full `Context`: system prompt, all messages, tool defs, images | `transformProviderContext` (`sdk.ts:3447`) obfuscator + snapcompact + image clamp; `transformMessages` (`transform-messages.ts:599`, opt-in); `onPayload` | No (unless model is Cursor / Codex-WS) |
| Subagents (`task/executor.ts:3542`) | all built-ins | same | subagent prompt + own transcript | same, via its own `createAgentSession` | No |
| Persisted-agent revive (`task/persisted-revive.ts:133`) | all built-ins | same | replayed JSONL transcript | same | No |
| Advisors (`session-advisors.ts:1010`) | all built-ins | same | primary-session transcript delta | + advisor obfuscation (`advisor/runtime.ts:722`) | No |
| Local compaction summary (`compaction.ts:993`) | active/role model | same, via `sideStreamFn` (`session-maintenance.ts:2728`) | serialized full conversation text | `obfuscatePreparationForProvider` (`session-maintenance.ts:2699`); `convertToLlmForSideRequest` (`session-provider-boundary.ts:134`) | No |
| Anthropic native compaction (`compaction/anthropic.ts:258`) | anthropic | same | live-turn request shape + compact edit | same as local compaction | No |
| OpenAI remote compaction V1 (`compaction/openai.ts:857`) | openai / azure / codex | HTTPS POST, bare `fetch` | native Responses transcript (`buildOpenAiNativeHistory`) | `transformMessages` only (`compaction/openai.ts:513`) | **YES** |
| Compaction V2 streaming (`compaction-v2-streaming.ts:371`) | openai / codex | HTTPS POST, `globalThis.fetch` (`:278`) | Responses input items | `buildResponsesInput` (`openai-shared.ts:1938`) | **YES** |
| Generic remote compaction (`compaction/openai.ts:956`) | operator-configured URL | HTTPS POST, bare `fetch` | serialized transcript as `prompt` | none beyond serialization | **YES** |
| snapcompact (`snapcompact.ts:2038`) | — | none (local render) | — | n/a — local only | n/a (no egress) |
| Cursor (`cursor.ts:759`) | cursor | raw HTTP/2 (`http2.connect`) | protobuf blob store of full history | `onPayload` (`cursor.ts:5433`) only; no `transformMessages` | **YES** |
| Codex WebSocket (`openai-codex-responses.ts:3717`) | openai-codex | WSS frames | `response.create` with full input | `transformMessages` (`:4545`) + `onPayload` (`:1824`) | **YES** (transport only) |
| GitLab Duo Workflow (`gitlab-duo-workflow.ts:2582`) | gitlab-duo-agent | WebSocket | bare ChatML goal transcript | direct `redactSensitiveCredentials` (`:2587`, `:2589`); no `transformMessages` | **YES** |
| pi-native gateway (`pi-native-client.ts:186`) | omp auth-gateway | HTTPS SSE via `transportFetch` | entire raw `Context` object, unencoded | none | No (fetch) but no content filter |
| Custom/extension API (`stream.ts:903`) | extension-defined | extension-defined | full `Context` handed to third-party code | none enforced | **YES** |
| Provider file upload — Anthropic (`provider-files-anthropic.ts:114`) | anthropic | HTTPS multipart | raw file bytes | none | **YES** |
| Provider file upload — OpenAI (`provider-files-openai.ts:91`) | openai | HTTPS multipart | raw file bytes | none | **YES** |
| Provider file upload — Gemini (`provider-files-gemini.ts:95`) | google | HTTPS resumable | raw file bytes | none | **YES** |
| Web search — Perplexity (`web/search/providers/perplexity.ts:518`) | perplexity | HTTPS SSE, own `fetchImpl` | search query + context | `transformMessages` via encoder; no session obfuscation | **YES** |
| Web search — Gemini/Kagi/Kimi (`gemini.ts:496`, `kagi.ts:95`, `kimi.ts:218`) | google/kagi/moonshot | HTTPS, own `fetchImpl` | search query | none | **YES** |
| Web scrapers (`web/scrapers/github.ts:133`) | github / docs.rs / arbitrary | HTTPS GET, bare `fetch` | URL only | n/a | **YES** |
| MCP HTTP/SSE (`mcp/transports/header-policy.ts:103`) | arbitrary MCP server | HTTPS / SSE, bare `fetch` | JSON-RPC: tool args, session id | none | **YES** |
| MCP OAuth/registry (`mcp/oauth-flow.ts:506`, `mcp/smithery-registry.ts:321`) | auth servers / Smithery | HTTPS | credentials, server queries | none | **YES** |
| Title generation (`utils/title-generator.ts:284`) | role model | via `completeSimple` | first user message verbatim | none (no obfuscator) | No (fetch) / **YES** (content) |
| Auto-thinking classifier (`auto-thinking/classifier.ts:148`) | tiny/role model | via `completeSimple` | user prompt verbatim | none | No (fetch) / **YES** (content) |
| Commit/changelog generation (`commit-message-generator.ts:110`, `changelog/generate.ts:61`, `commit/conventional/inference.ts:119`) | role model | via `completeSimple` | git diff contents | none | No (fetch) / **YES** (content) |
| Edit auto-repair (`edit/auto-repair.ts:318`) | role model | via `completeSimple` | file contents + edit strings | none | No (fetch) / **YES** (content) |
| Sharpshooter (`sharpshooter/extract.ts:216`, `consolidate.ts:157`) | role model | via `completeSimple` | transcript excerpts | none | No (fetch) / **YES** (content) |
| TTS enhancer (`tts/speech-enhancer.ts:88`) | role model | via `completeSimple` | assistant output text | none | No (fetch) / **YES** (content) |
| Image question / vision fallback (`utils/image-question.ts:131`, `image-vision-fallback.ts:131`, `tools/read.ts:731`) | vision model | via `instrumentedCompleteSimple` | raw image bytes + prompt | none | No (fetch) / **YES** (content) |
| git-tui AI staging (`git-tui/ai-stage.ts:230`) | role model | via `completeSimple` | diff hunks | none | No (fetch) / **YES** (content) |
| eval completion bridge (`eval/completion-bridge.ts:166`) | requested model | via `instrumentedCompleteSimple` | agent-authored prompt | none | No (fetch) / **YES** (content) |
| Bench / dry-balance / if-bench (`cli/bench-cli.ts:917`, `cli/dry-balance-cli.ts:794`, `if-bench/index.ts:123`) | all | via `streamSimple` | synthetic benchmark prompts | n/a | No |
| ADW seats (`adw/runner.ts:763`) | per-seat model | subprocess → new session | phase prompt + diff (`adw/runner.ts:795`) | inherits §3.1 in the child process | No |

---

## 6. Discrepancies between documentation/comments and runtime

1. **`transport-fetch.ts:20-22` docstring** claims *"The one fetch every inference request goes through ... Providers never layer transport concerns themselves."*
   **Runtime contradicts this:** `packages/ai/src/providers/cursor.ts:759` uses `http2.connect` and `packages/ai/src/providers/openai-codex-responses.ts:3717` uses `new WebSocket` — neither consults `options.fetch`. OBSERVED. Confidence: HIGH.

2. **`transform-messages.ts:596-598` comment** says redaction exists to *"prevent security block errors from LLM providers (e.g. invalid_prompt)"* — i.e. it is a **provider-compatibility** measure, not a privacy control. The code matches the comment. Any reading of it as a privacy filter would be incorrect: it is off by default (`:444`) and covers 6 credential prefixes only (`:418-419`). OBSERVED. Confidence: HIGH.

3. **`packages/coding-agent/src/secrets/index.ts:206-212` comment** correctly documents that the pi-ai redaction is irreversible and that the obfuscator supersedes it — but the two systems are gated by the **same** setting (`secrets.enabled`, `settings.ts:3112`; `sdk.ts:1481`), so disabling one disables both. OBSERVED. Confidence: HIGH.

4. **Local execution is not local reduction.** `snapcompact` (`packages/snapcompact/src/snapcompact.ts:1619`) runs entirely locally, but its product is a set of PNG frames that are then *sent* (`snapcompact.ts:1821 images`). It is a re-encoding, not a reduction of what leaves the machine. Likewise every tool in `packages/coding-agent/src/tools` executes locally while its full output enters `context.messages` and ships. OBSERVED. Confidence: HIGH.
