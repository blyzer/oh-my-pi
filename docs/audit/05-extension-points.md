# 05 — Extension Points: can an existing OMP hook see and alter the complete outbound request?

Scope: read-only audit of `/Users/enverfrancisco/repositories/oh-my-pi`.
Question answered for the parent: **does OMP already contain the machinery a local
context/privacy layer needs, under different names — or is a genuinely new boundary required?**

Evidence rule: only runtime implementation counts. Docs and comments are quoted only when
flagged as a discrepancy. Every finding cites `file:line` + symbol and is marked OBSERVED or MISSING.

---

## 0. TL;DR — the headline answer

**YES — one existing extension point already sees and can replace the entire, final,
provider-specific outbound request body: the `before_provider_request` extension event,
plumbed through `SimpleStreamOptions.onPayload`.**

- Definition: `packages/coding-agent/src/extensibility/extensions/types.ts:753` `BeforeProviderRequestEvent`
- Dispatcher: `packages/coding-agent/src/extensibility/extensions/runner.ts:1665` `ExtensionRunner.emitBeforeProviderRequest`
- Wiring: `packages/coding-agent/src/sdk.ts:3466-3468` — `const onPayload = async (payload, model) => extensionRunner.emitBeforeProviderRequest(payload, model)`
- Transport contract: `packages/ai/src/types.ts:551` — `onPayload?: (payload: unknown, model?: Model<Api>) => unknown | undefined | Promise<unknown | undefined>`
- Provider call sites (the actual wire body, immediately before serialization) — see §3.

It is **observe + modify + per-model/per-provider**. It is **not** a blocking gate and it has
**no provenance**: by that point the request is an opaque provider-shaped JSON blob with no
record of which tool produced which byte. It also does **not** cover subagents or ADW seats as a
single boundary — each spawned session installs its own copy, and restricted sessions install none (§6).

So the machinery for *interception* exists. The machinery for *provenance-aware policy* and for
*process-wide enforcement* does not. A new boundary is required for those properties only; the
wire-level interception itself should be reused, not rebuilt.

---

## 1. Capability table

`Observe` = handler receives the data. `Whole req?` = can it see the *complete* outbound request
(system prompt + all messages + tools + sampling params). `Modify` = its return value is applied.
`Block` = can refuse/abort the operation. `Prov.` = can it tell which tool produced a given piece
of content. `Per-prov/model` = can policy differ per provider or model.

| # | Extension point | Where (file:line / symbol) | Before provider call? | Observe | Whole req? | Modify | Block | Prov. | Per-prov/model |
|---|---|---|---|---|---|---|---|---|---|
| 1 | `before_provider_request` | `extensions/runner.ts:1665` `emitBeforeProviderRequest`; wired `sdk.ts:3466` | **YES** — inside the provider fn, last step before serialization | **YES** | **YES** (final wire body) | **YES** (non-`undefined` return replaces payload; chains across extensions) | NO (a throw only logs and drops that handler's edit) | **NO** | **YES** — `model` passed as 2nd arg |
| 2 | `context` | `extensions/runner.ts:1618` `emitContext`; wired `sdk.ts:3414` `transformContext` | YES — per LLM call, before `convertToLlm` | messages only | NO (no system prompt, no tools, no sampling params) | YES (`{messages}` replaces) | NO | partial — `ToolResultMessage.toolName` (`packages/ai/src/types.ts:1080`) survives | **NO** — `createContext()` called with no model (`runner.ts:1619`) |
| 3 | `transformProviderContext` | `sdk.ts:3447`; applied `packages/agent/src/agent-loop.ts:1625-1627` | YES | `Context` = systemPrompt + messages + tools | **YES** (structurally complete, pre-serialization) | YES (returns new `Context`) | NO | partial (`toolName` on tool results) | **YES** — `(context, model)` |
| 4 | `before_agent_start` | `extensions/runner.ts:1715` `emitBeforeAgentStart` | YES — once per user prompt | prompt + images + `systemPrompt: string[]` | NO (no history, no tools) | YES — injects a message **and may replace the system prompt** (`runner.ts:1749-1753`) | NO | NO | NO |
| 5 | `tool_call` | `extensions/runner.ts:1470` `emitToolCall`; gate `extensions/wrapper.ts:213`; loop path `session/agent-session.ts:4032` | before the **tool**, not before the provider | toolName + toolCallId + input | NO | YES (`input` replacement, revalidated against schema) | **YES** (`{block:true, reason}`; fail-**closed** on timeout, `runner.ts:1465`) | **YES** (it *is* the tool) | NO |
| 6 | `tool_result` | `extensions/runner.ts:1412` `emitToolResult`; `wrapper.ts:373` | before the result enters history → before any later provider call | content + details + isError + input | NO | **YES** (`{content, details, isError}` replaces) | n/a (tool already ran) | **YES** (discriminated on `toolName` with typed details, `hooks/types.ts:330-380`) | NO |
| 7 | `after_provider_response` | `extensions/runner.ts:1697` `emitAfterProviderResponse` | NO — after headers | status/headers/requestId/metadata | n/a | NO | NO | NO | YES (`model` threaded) |
| 8 | `session_before_compact` / `session.compacting` | `session/session-maintenance.ts:903,1215,1340,2786,3784` | at compaction time | `CompactionPreparation`, branch entries, messages | NO | YES (`{compaction}`; `{prompt, context, preserveData}`) | YES (`{cancel:true}`) | partial | NO |
| 9 | `session_stop` | `session/agent-session.ts:4122` `emitSessionStop` | at turn settle | full `messages[]` | NO | appends continuation context only | forces continuation, not a block | NO | NO |
| 10 | `input` | `extensions/runner.ts:1603` | before prompt submission | text + images + source | NO | YES (`{handled}`) | YES (consumes input) | n/a | NO |
| 11 | `user_bash` / `user_python` | `session/bash-runner.ts:81`, `session/eval-runner.ts:56` | before user-initiated exec | command / code | NO | YES | YES | YES | NO |
| 12 | Approval / permission system | `tools/approval.ts` `resolveApproval` / `requiresApproval`; enforced `extensions/wrapper.ts:250-345` | before the **tool** | tool name + args + tier | NO | NO | **YES** (`deny` throws `denyError`) | YES | NO — keyed by tool name only |
| 13 | Bash pattern policy | `tools/bash.ts:598-649` `getBashApprovalPatternRules` | before shell exec | command string | NO | NO | **YES** (`policy:"deny"`) | YES | NO |
| 14 | Secret obfuscator | `secrets/obfuscator.ts:62` `SecretObfuscator`; applied `sdk.ts:3448` and `sdk.ts:3412` | YES — first transform in `transformProviderContext` | all message text | text of whole context | YES (substitution only) | NO | NO | NO (uniform) |
| 15 | `snapcompact` inline | `session/snapcompact-inline.ts`; `sdk.ts:3449` | YES | context | YES | YES (rasterizes tool results to PNG) | NO | NO | **YES** (`transform(ctx, transformModel)`) |
| 16 | Legacy `HookRunner` (`pi.on` hooks) | `extensibility/hooks/runner.ts:48` | — | — | — | — | — | — | — |
| | ↳ **never instantiated in `src/`** — only `test/compaction-hooks.test.ts:110`, `test/hook-tool-wrapper-input.test.ts:48` | | **DEAD at runtime** | | | | | | |
| 17 | Shell `hooks/pre` + `hooks/post` capability | `capability/hook.ts:27` `hookCapability`; providers in `discovery/{builtin,claude,claude-plugins,codex,omp-plugins}.ts` | — | — | — | — | — | — | — |
| | ↳ **only `.ts`/`.js` entries are ever loaded**, as extension modules — `extensions/loader.ts:699-704` filtered by `isExtensionFile` (`loader.ts:511`) | | shell hooks are **discovered but never executed** | | | | | | |
| 18 | MCP header policy | `mcp/transports/header-policy.ts` `mergeMCPHeaders`, `mcpFetch` | before MCP HTTP call | headers + origin only | NO — never sees request content | headers only | refuses method-changing cross-origin redirects | NO | n/a |
| 19 | `pi-native` gateway client | `packages/ai/src/providers/pi-native-client.ts:45-48` `NON_WIRE_KEYS` | — | — | — | — | — | — | — |
| | ↳ **strips `onPayload`/`onResponse`/`onSseEvent` from the wire** — `pi-native-client.ts:62` `buildWireOptions`, body built `:186-189` | | **the hook never reaches the remote gateway** | | | | | | |

---

## 2. Finding: `before_provider_request` is the only complete-request interception point

**Evidence:**
- `packages/coding-agent/src/extensibility/extensions/types.ts:752-756` — `BeforeProviderRequestEvent { type; payload: unknown }`
- `packages/coding-agent/src/extensibility/extensions/types.ts:1121` — `export type BeforeProviderRequestEventResult = unknown;`
- `packages/coding-agent/src/extensibility/extensions/runner.ts:1664-1696` — `emitBeforeProviderRequest`
- `packages/coding-agent/src/sdk.ts:3466-3468`, threaded at `sdk.ts:3542` and `sdk.ts:3782`
- `packages/coding-agent/src/session/agent-session.ts:742, 1468, 1652, 1772` — `#onPayload` carried into the loop and side-channels
- `packages/ai/src/types.ts:549-551` — the `onPayload` contract

**Runtime path:**
`AgentSession` → `agent-loop.ts prepareProviderCall` → provider `stream*()` → provider builds its
native params object → `await options?.onPayload?.(params, model)` → a non-`undefined` return
replaces `params` → serialize → HTTP.

**Observed behavior:**
- Chaining is sequential and cumulative: `runner.ts:1667-1692` feeds `currentPayload` forward, so
  extension N sees extension N-1's rewrite. Pinned by `test/extensions-runner.test.ts:708-736`
  (`chain: [...(payload.chain ?? []), "ext1"]` then `"ext2"`).
- Per-model context: `emitBeforeProviderRequest(payload, model)` calls `this.createContext(model)`
  (`runner.ts:1666`), so `ctx.model` / `ctx.models.current()` reflect the *request's* model, not the
  session model — pinned by `test/extensions-runner.test.ts:634-660`.
- **It cannot block.** A throwing handler is caught inside `#runHandlerWithTimeout`
  (`runner.ts:1313-1343`), routed to `emitError`, and `emitBeforeProviderRequest` passes no
  `onFailure` callback → `handlerResult === undefined` → `currentPayload` is left unchanged and the
  request **proceeds**. `test/extensions-runner.test.ts:741-776` asserts exactly this: a throwing
  handler yields one error record while the next extension's `preserved: true` still ships. A
  privacy layer wanting fail-closed semantics cannot obtain it here — even throwing from every
  handler still sends the original payload.
- Handlers are time-bounded (`extensionHandlerTimeoutMs`, `runner.ts:1678`); a timeout is
  fail-**open** for this event. Contrast `emitToolCall`, which is explicitly fail-**closed**
  (`runner.ts:1465` — "On-timeout policy: **fail-closed** (return `{ block: true }`)").
- The payload is the *provider-native* body (`MessageCreateParamsStreaming`,
  `GenerateContentParameters`, `ConverseStreamRequest`, …), typed `unknown`. There is no normalized
  cross-provider view and no provenance metadata attached.

**Conclusion:** complete-request observe + modify + per-model. No block, no provenance.
**Confidence: HIGH.**

---

## 3. Provider coverage of `onPayload` — does the hook actually fire everywhere?

OBSERVED — grep over `packages/ai/src/providers`:

| Provider module | `onPayload` fired at | Replacement applied? |
|---|---|---|
| `anthropic.ts` | `:2479` | YES (`nextParams = replacementPayload`) |
| `openai-responses.ts` | `:530` | YES |
| `openai-completions.ts` | `:779` | YES — regression-guarded; `packages/ai/test/openai-completions-on-payload.test.ts:1-5` exists *because* this provider once ignored the return value |
| `openai-codex-responses.ts` | `:1824` (WebSocket frame), `:1952` (HTTP body) | YES, both |
| `google-shared.ts` (google / vertex) | `:960` | YES |
| `google-gemini-cli.ts` | `:577` | YES |
| `azure-openai-responses.ts` | `:132` | YES |
| `amazon-bedrock.ts` | `:450` | YES |
| `ollama.ts` | `:564` | YES |
| `cursor.ts` | `:5433` | YES |
| `gitlab-duo.ts` | `:161, 201, 235` | forwarded to the delegate provider |
| `openai-anthropic-shim.ts` | `:102, 142` | forwarded |
| `devin.ts` | **MISSING** — no `onPayload` anywhere in the module | **NO HOOK** |
| `pi-native-client.ts` | `:45-48` `NON_WIRE_KEYS` strips it; `:62` `buildWireOptions`; body `:186-189` | **hook is local-only; the gateway never receives it and builds the request server-side** |

`docs/extensions.md:304` claims the replacement is *"applied by every provider that fires the hook,
which is all of them except `devin-agent`"*. That matches `devin.ts`, but omits the pi-native
gateway path, where the callback is deliberately stripped as non-serializable. For a privacy layer
this is a real hole, not a documentation nit.
**Confidence: HIGH.**

---

## 4. Full event list an extension can subscribe to (read from API types + emit sites, not docs)

Source of truth: the `on(...)` overload set at
`packages/coding-agent/src/extensibility/extensions/types.ts:1244-1300`, cross-checked against the
`ExtensionEvent` union at `types.ts:1076-1110` and the `emit` call sites.

**51 events.** Grouped:

*Resources / session (13):* `resources_discover`, `session_start`, `session_before_switch`,
`session_switch`, `session_before_branch`, `session_branch`, `session_before_compact`,
`session.compacting`, `session_compact`, `session_shutdown`, `session_before_tree`, `session_tree`,
`session_stop`.

*Context / provider (3):* `context`, **`before_provider_request`**, `after_provider_response`.

*Agent / turn / message (9):* `before_agent_start`, `agent_start`, `agent_end`, `turn_start`,
`turn_end`, `message_start`, `message_update`, `message_end`, `goal_updated`.

*Tool (7):* `tool_execution_start`, `tool_execution_update`, `tool_execution_end`, `tool_call`,
`tool_result`, `tool_approval_requested`, `tool_approval_resolved`.

*Maintenance / retry (6):* `auto_compaction_start`, `auto_compaction_end`, `auto_retry_start`,
`auto_retry_end`, `retry_fallback_applied`, `retry_fallback_succeeded`.

*Reminders / other (7):* `ttsr_triggered`, `todo_reminder`, `credential_disabled`, `input`,
`user_bash`, `user_python`, `mcp_notification`.

Events whose **return value changes behavior** — the only ones a policy layer can act through:
`before_provider_request` (payload), `context` (messages), `before_agent_start`
(message + **systemPrompt**), `tool_call` (`block` / `input`), `tool_result`
(`content` / `details` / `isError`), `input` (`handled`), `user_bash` / `user_python`,
`session_before_switch` / `_branch` / `_compact` / `_tree` (`cancel`), `session.compacting`
(`prompt` / `context` / `preserveData`), `session_stop` (`continue` / `decision`),
`resources_discover`.

The hook API (`HookAPI`, `hooks/types.ts:475-505`) is a strict subset — 24 events — and notably
**lacks `before_provider_request` entirely**. Since `HookRunner` is never constructed in `src/`
(§5a), that surface is unreachable regardless.
**Confidence: HIGH.**

---

## 5. Two extension points that look real in the tree but are dead at runtime

### 5a. `HookRunner` / `HookToolWrapper` — legacy, never instantiated in `src/`

**Evidence:** `extensibility/hooks/runner.ts:48` `class HookRunner`;
`extensibility/hooks/tool-wrapper.ts:19` `class HookToolWrapper`.
Grep for `new HookRunner(` / `new HookToolWrapper(` across the repo returns **only tests**:
`test/compaction-hooks.test.ts:110`, `test/hook-tool-wrapper-input.test.ts:48, 69, 80, 89`.
No `src/` module imports `extensibility/hooks/runner` or `.../tool-wrapper`; the only `src/` imports
of `extensibility/hooks` pull *types* for custom-command / custom-tool signatures
(`extensibility/custom-commands/types.ts:11`, `custom-tools/types.ts:27`,
`session/agent-session.ts:149`, `modes/components/hook-message.ts:3`).
`interactiveMode.initializeHookRunner` (`modes/interactive-mode.ts:5432`) is a misnomer: it
delegates to `extension-ui-controller.ts:395`, whose first line is
`const extensionRunner = this.ctx.session.extensionRunner` (`:396`) — it initializes the
**ExtensionRunner**.

**Conclusion:** the hook subsystem is a vestigial parallel implementation. Any plan to "reuse OMP
hooks" would be building against dead code. `docs/hooks.md:10-12` is honest about this.
**Confidence: HIGH.**

### 5b. Shell `hooks/pre|post/*` — discovered, shown in the UI, never executed

**Evidence:** `capability/hook.ts:27` defines `hookCapability` with `type: "pre" | "post"` and a
`tool` field; five discovery providers populate it (`discovery/builtin.ts:725`,
`discovery/claude.ts:588`, `discovery/claude-plugins.ts:717`, `discovery/codex.ts:572`,
`discovery/omp-plugins.ts:388` — the last walks `hooks/pre/` and `hooks/post/` at
`omp-plugins.ts:159-181`).
The **only runtime consumer** is `extensibility/extensions/loader.ts:698-705`, which takes
`hooks.items`, maps to `hook.path`, then
`.filter(hookPath => isExtensionFile(path.basename(hookPath)))` — and `isExtensionFile`
(`loader.ts:511-513`) is `name.endsWith(".ts") || name.endsWith(".js")`.
A shell script in `.omp/hooks/pre/` is discovered, rendered in the extensions dashboard
(`modes/components/extensions/state-manager.ts:251-255`), and **silently never run**. There is no
`PreToolUse` / `PostToolUse` executor anywhere in `src/`.

**Conclusion:** MISSING — OMP has no Claude-Code-style shell hook execution.
**Confidence: HIGH.**

---

## 6. Scope: does one extension cover subagents, ADW seats, and MCP?

**Main agent loop — YES.** `ExtensionRunner` is constructed *unconditionally* at `sdk.ts:2802`,
even with zero extensions loaded, because `ExtensionToolWrapper` is the sole approval-gate site
(`sdk.ts:2795-2799`; every registry tool wrapped at `sdk.ts:2921-2923`).

**Subagents — PER-SESSION RE-INSTANTIATION, not one boundary.**
- Each spawned subagent calls `createAgentSession` (`task/executor.ts:3542`, revival `:3580`) and
  therefore builds **its own** `ExtensionRunner` and its own `onPayload` closure.
- Extensions are *re-bound*, not shared: `task/executor.ts:497-503` forwards
  `preloadedExtensionPaths` / `preloadedPreparedExtensions` so the child "skips the extension FS
  scan; the subagent then re-binds each extension against its own `ExtensionAPI` (cwd, eventBus,
  runtime)".
- `task/executor.ts:3670-3725` re-runs `extensionRunner.initialize(...)` and re-emits
  `session_start` for the child.
- **Restricted / read-only subagents load NO extensions at all.**
  `sdk.ts:2157-2161`:
  `if (restrictToolNames) { extensionPaths = []; extensionsResult = await loadExtensions([], cwd, eventBus); }`
  and `task/executor.ts:3463-3465` forces `preloadedExtensionPaths: []` /
  `preloadedPreparedExtensions: []` when `restrictToolNames`.
  ADW panel seats are spawned `readOnly: true` → `restrictToolNames: seat.readOnly`
  (`adw/runner.ts:663`), and the ADW classifier with `restrictToolNames: true`
  (`adw/classify.ts:168`).
  **So an ADW panel seat's provider requests pass through NO `before_provider_request` handler.**
  This is the single most important scope hole for a privacy layer.
- `restrictToolNames` also disables MCP entirely (`sdk.ts:1982`, `task/executor.ts:3378`).

**Out-of-process subagents.** `runSubprocess` (`adw/runner.ts:762`, `adw/classify.ts:161`) launches
a separate OS process per seat. A same-process interceptor is structurally incapable of covering
these; only disk-level installation (the extension being re-discovered by the child) reaches them —
and `restrictToolNames` disables exactly that.

**MCP servers — NO content interception.** The only MCP policy surface is
`mcp/transports/header-policy.ts` (`mergeMCPHeaders`, `withoutHeader`, `mcpFetch`), governing header
precedence and cross-origin redirect refusal. It never inspects request *content*. MCP **tool
results** are covered indirectly: MCP tools are wrapped by `ExtensionToolWrapper`
(`session/session-tools.ts:1652-1657`, `sdk.ts:2867-2869`), so `tool_call` / `tool_result` fire for
them. MCP **sampling** (`createMessage`) is not implemented — `mcp/types.ts:191` declares the
`sampling` capability field and nothing else in the repo references it. No MCP server can currently
make the host call an LLM on its behalf, which closes that exfiltration path by omission rather than
by policy.

**Side-channel requests.** Advisors and compaction route through
`session/session-provider-boundary.ts:146-190` `prepareSimpleStreamOptions`, which *composes* the
session `onPayload` with any request-local one (`:178-188`) — covered.
But `commit/conventional/generate.ts:169, 198, 273, 346`, `auto-thinking/classifier.ts:196`,
`edit/auto-repair.ts:246` and `web/search/providers/perplexity.ts:535` construct their own stream
options; grepping `onPayload|obfuscat|transformProviderContext` under `src/commit`,
`src/auto-thinking`, `src/edit` returns **no hook wiring** (only the unrelated `parseJsonPayload`).
Perplexity additionally *uses* `onPayload` for its own purpose (`:535`), which a naive
session-level install would clobber.

**Conclusion: PARTIAL coverage. Confidence: HIGH.**

---

## 7. Provenance — the capability that is genuinely absent

**MISSING.** Nothing in the outbound path carries "this span of bytes came from tool X, call Y".

- At `tool_result` time provenance is rich: the event is a discriminated union on `toolName` with
  typed `details` (`hooks/types.ts:330-380` — `BashToolResultEvent`, `ReadToolResultEvent`,
  `GrepToolResultEvent`, …), plus `toolCallId`.
- Once merged into history only `ToolResultMessage.toolName` + `toolCallId` survive
  (`packages/ai/src/types.ts:1077-1083`). That is message-granular, not span-granular: a compaction
  summary, or an assistant message quoting tool output, carries nothing.
- At the provider boundary even that is reduced. Anthropic emits
  `{ type: "tool_result", tool_use_id, content }` (`packages/ai/src/providers/anthropic-wire.ts:64-69`)
  — `tool_use_id` correlates back to the call, but the tool *name* is absent from the wire block, and
  text the model quoted from a tool is indistinguishable from text it authored.
- `before_provider_request` receives `payload: unknown` with **zero** side-channel metadata.

**Conclusion:** a `before_provider_request` handler can rewrite anything but cannot answer "is this
line the output of `bash`, or the user's own prose?" without re-deriving it. Carrying provenance
forward from `tool_result` through history into the payload is new plumbing, not a new hook.
**Confidence: HIGH.**

---

## 8. Per-provider / per-model policy

PRESENT where it exists:
- `before_provider_request`: `model` is the second argument of `onPayload` and is threaded into the
  handler context (`runner.ts:1665-1666`). `packages/coding-agent/CHANGELOG.md:1183` records that
  `after_provider_response` had to be *fixed* to match — evidence this threading is load-bearing.
- `transformProviderContext(context, model)` — `sdk.ts:3447`, applied `agent-loop.ts:1625`.
- `snapcompactInline.transform(transformed, transformModel)` — `sdk.ts:3449`.

MISSING:
- `emitContext` calls `this.createContext()` with **no model** (`runner.ts:1619`) — a `context`
  handler cannot tell which provider the messages are headed to.
- `tools/approval.ts` keys policy purely on tool name and tier (`resolveApproval`, `ApprovalMode`);
  no provider/model dimension exists anywhere in the approval system.
- The secret obfuscator is uniform: `buildSecretObfuscator(cwd, agentDir, keyDir)`
  (`secrets/index.ts:247`) takes no model, and `obfuscateProviderContext(obfuscator, context)`
  (`secrets/message-transform.ts:323`) takes no model. "Redact more for provider A than provider B"
  is not expressible.
**Confidence: HIGH.**

---

## 9. Is there anything resembling a policy engine?

The closest candidates, and what each actually is:

1. **Tool approval** (`tools/approval.ts`) — a genuine three-level precedence engine: tool-declared
   `approval(args)` decision → user `tools.approval.<key>` → `ApprovalMode` tier comparison
   (`resolveApproval`, `:118-210`). Tiers `read|write|exec` (`:31-35`); modes
   `always-ask|write|yolo` (`:37-41`). **Can deny** (`denyError`, `:216`). Enforced in exactly one
   place — `ExtensionToolWrapper.execute` (`wrapper.ts:203-345`) — which is why `sdk.ts:2795-2799`
   constructs the runner unconditionally. Note `wrapper.ts:205`: the mode defaults to **`yolo`** when
   no settings are present. It gates *actions*, never *content*, and never the outbound request.
2. **Bash pattern rules** (`tools/bash.ts:224-300, 598-660`) — glob `allow|deny|prompt` matched per
   shell segment, `deny` beating `prompt`, and `allow` required to match the whole chain
   (`:289-300`). A real deny-list, scoped to one tool.
3. **ADW write scope / `TaskWriteGuard`** (`adw/prompt.ts` `writeScope`, `adw/integration.ts`) —
   post-hoc filesystem enforcement: out-of-scope writes are rolled back, protected globs refused.
4. **`task/read-only-policy.ts` / `task/spawn-policy.ts`** — `READ_ONLY_TOOL_NAMES` set and spawn
   frontmatter resolution. Name-list gating; no evaluation.
5. **Rules / TTSR** (`capability/rule.ts`) — `condition` regexes plus `astCondition` ast-grep
   patterns matched against the model's *output stream* to interrupt generation. This is the only
   pattern-matching-over-content engine in the codebase, and it points the **wrong way**: inbound
   from the model, not outbound to it.

**Conclusion:** OMP has several action-scoped policy engines and zero content-scoped outbound policy
engine. **Confidence: HIGH.**

---

## 10. ADW: does it reduce information locally, or only organise phases?

**Finding: ADW is overwhelmingly an orchestration and acceptance layer. It performs three small,
fixed truncations and no content-aware reduction — and for fusion workflows it materially
*increases* the volume of data leaving the machine.**

**What ADW actually builds:**
- `adw/runner.ts:1-14` (module doc, corroborated by the code): `TaskRun` (Rust, `crates/pi-tasks`,
  via `@oh-my-pi/pi-natives`) owns sequencing, retries and acceptance; the TS driver only spawns the
  model for an `agent` phase, fans out and merges a `fusion` phase, and runs the command for a
  `code` phase.
- Prompt construction is pure string concatenation of declared sections:
  `adw/prompt.ts` `buildPhasePrompt` (`# Request`, `# Your phase`, inputs/handoff, review diff,
  `# Phase instructions`, write scope, output contract, correction), plus `buildPanelPrompt` and
  `buildFusionPrompt`. No summarization, no dropping, no dedup.
- Context between phases travels as a **handoff envelope**, never a shared conversation
  (`adw/prompt.ts` `renderHandoff` → `summary`, `notes`, `artifacts`, `payloadJson`). The envelope is
  produced by the *model itself* (`ENVELOPE_CONTRACT_BUNDLED`, `adw/prompt.ts:19-37`) and parsed by
  the Rust engine (`crates/pi-tasks/src/envelope.rs:24-32`;
  `crates/pi-tasks/src/orchestrator.rs:714` `submit_agent_output`).

**The only three local reductions — all fixed-size truncations:**

| What | Where | Bound |
|---|---|---|
| Command stdout/stderr quoted into the next correction | `adw/runner.ts:86` `COMMAND_SUMMARY_LIMIT`; `:210-215` `summarizeOutput` | 2 000 chars, **tail** (`combined.slice(-COMMAND_SUMMARY_LIMIT)`) |
| Review diff handed to a verdict-gated phase | `adw/runner.ts:778` `REVIEW_DIFF_MAX_CHARS`; `:796-828` `collectReviewDiff` | 60 000 chars, head-slice, with a `truncated` flag rendered into the prompt (`adw/prompt.ts` `renderReviewDiff`) |
| Structured phase inputs for `code` phases | `adw/runner.ts:1385-1408` `buildCodeEnv` | written to a file and passed as the `ADW_INPUTS` env var — keeps them off argv **and** out of any prompt |

That is the complete list. `summarizeOutput` is `slice(-2000)`; `collectReviewDiff` is
`slice(0, 60_000)`. Neither consults a model, an index, or any relevance signal.

**Where ADW does reduce, it is by architecture, not by filtering** — stated precisely because this
is easy to overclaim:
- Phases share no conversation, so a later phase does not inherit the earlier phase's transcript.
  Only the envelope crosses. That genuinely bounds per-request context size.
- Panel seats are read-only with a four-tool allowlist
  (`adw/runner.ts:91` `PANEL_TOOLS = ["read", "grep", "glob", "yield"]`).
- Panel opinions are cached across fuser retries (`adw/runner.ts:1199` `panelCache`): a retry re-runs
  the fuser, not the N panel models. This avoids *re-sending*; it does not reduce.
- The classifier is deliberately given only a catalogue summary, never the repo
  (`adw/classify.ts` `describeCatalogue` — workflow names, descriptions, phase names, gate names).

**Where ADW amplifies:** a fusion phase sends the same question to N panel seats concurrently
(`mapWithLimit`, `adw/runner.ts:1253`), then feeds every opinion **verbatim** to the fuser
(`buildFusionPrompt`, `adw/prompt.ts:236-266`, which pushes
`## ${owner} (${model})` followed by the full `opinion.text`). One user request becomes N+1 provider
requests, and the (N+1)-th contains the concatenated full text of the first N. Each seat is a
separate `runSubprocess` with its own session and its own tool traffic.

**Artifacts:** ADW shares the parent's `ArtifactManager` (`adw/runner.ts:116-120, 674-678`) so
concurrent seats share an id space. Artifact spilling itself is the generic large-output mechanism
(`tools/output-meta.ts:933` `wrapToolWithMetaNotice`), not an ADW invention — ADW only prevents
seats from overwriting each other's spilled output.

**Conclusion:** ADW **organises phases and bounds context by isolation**; it applies three hard
truncations and no content-aware reduction. It is not prior art for local context reduction.
**Confidence: HIGH.**

---

## 11. The outbound pipeline, in execution order

```
user prompt
  |
  +- input                       (ext; can consume)
  +- before_agent_start          (ext; may REPLACE systemPrompt, inject a message)
  +- agent_start
  |
  +- per turn -----------------------------------------------------------------
       |
       +- transformContext                       sdk.ts:3414
       |    \- emitContext  -> `context` event   (messages only, NO model)
       |
       +- convertToLlm                           sdk.ts:3410
       |    \- filterProviderReplayMessages + obfuscateMessages
       |
       +- [agent-loop.ts:1625] transformProviderContext(context, model)   sdk.ts:3447
       |    +- obfuscateProviderContext       secrets  <- only content-aware local filter
       |    +- snapcompactInline.transform    tool results -> PNG frames
       |    +- clampProviderContextImages
       |    +- normalizeProviderContextImagesForModel
       |    +- dropUnreadableContextImages
       |    +- blobBroker.decorateContext     inline base64 -> broker URL
       |    \- dateCwdReminder.transform
       |
       +- provider builds native params  (anthropic.ts:2459, openai-responses.ts ~520, ...)
       |
       +- ** onPayload(params, model) --> emitBeforeProviderRequest **
       |     LAST LOCAL POINT. Sees and replaces the ENTIRE wire body. Cannot block.
       |     Absent for: devin-agent; pi-native gateway (stripped by NON_WIRE_KEYS).
       |
       +- HTTP / WebSocket  ------------------------------------> provider
       |
       +- onResponse -> after_provider_response   (headers only, read-only)
       \- message_start/update/end -> tool_call -> [approval gate] -> exec -> tool_result
```

---

## 12. Answer to the acceptance question

> Can any existing extension point see **and** alter the **complete** outbound request?

**YES — exactly one: `before_provider_request` / `onPayload`.** It is the final pre-serialization
hook inside each provider, receives the complete provider-native body, its return value replaces
that body, handlers chain cumulatively, and it is model-aware.

The qualifications that decide reuse-vs-build:

| Property a local context/privacy layer needs | Status in OMP today |
|---|---|
| See the complete outbound request | **PRESENT** — `before_provider_request` |
| Modify the complete outbound request | **PRESENT** — same hook |
| Policy differs per provider / model | **PRESENT** — `model` threaded into the handler |
| **Block or refuse a request** | **ABSENT** — a throwing handler is caught (`runner.ts:1313-1343`) and the request ships unmodified; timeouts fail open |
| **Provenance (which tool produced this text)** | **ABSENT** at the payload boundary; exists only at `tool_result` and is discarded downstream |
| **Covers every session in the process** | **ABSENT** — per-session runner; `restrictToolNames` (all ADW panel seats, the ADW classifier, read-only subagents) loads **zero** extensions |
| **Covers out-of-process subagents** | **ABSENT** — `runSubprocess` seats are separate processes |
| **Covers the pi-native gateway path** | **ABSENT** — `NON_WIRE_KEYS` strips the callback |
| **Covers ad-hoc side-channel calls** | **ABSENT** — `commit/`, `auto-thinking/`, `edit/auto-repair` build their own stream options |
| Content-scoped policy engine | **ABSENT** — every policy engine (`approval.ts`, `bash.patterns`, `TaskWriteGuard`, `read-only-policy`) gates *actions*; TTSR rules match *inbound* model output |
| Local content reduction before send | **PRESENT but narrow** — `SecretObfuscator` (substitution only), `snapcompact` (text -> PNG), image clamping/dropping. ADW adds only two fixed truncations (2 KB / 60 KB) |

**Verdict for the parent:** OMP already contains the *interception* machinery, under the name
`before_provider_request` / `onPayload`, and a new subsystem should reuse it rather than duplicate
it. What OMP does **not** contain is (a) a fail-closed blocking semantic at that point,
(b) provenance carried from tool output through history to the wire, and (c) a boundary that is
process-wide rather than per-session. The third gap is structural, because the harness deliberately
constructs restricted sessions that load no extensions at all. A genuinely new boundary is required
for exactly those three properties — not for the interception itself.

---

## 13. Discrepancies between documentation and code

1. `docs/extensions.md:304` states the `before_provider_request` replacement is *"applied by every
   provider that fires the hook, which is all of them except `devin-agent`"*. That matches
   `devin.ts`, but omits `packages/ai/src/providers/pi-native-client.ts:45-48`, where `onPayload` is
   listed in `NON_WIRE_KEYS` and therefore never reaches the gateway that assembles the request.
   OBSERVED.
2. `docs/hooks.md:10-12` correctly states the hook subsystem is legacy and that tools are wrapped by
   `ExtensionToolWrapper` — but `docs/config-usage.md:313` and `docs/skills/authoring-hooks.md:127-129`
   still describe `discoverAndLoadHooks()` and `HookToolWrapper` behavior as if live. No `src/`
   caller exists for either; both are referenced only from `test/`. OBSERVED.
3. `capability/hook.ts` models shell `pre`/`post` hooks per tool, five discovery providers populate
   the capability, and the extensions dashboard renders them
   (`modes/components/extensions/state-manager.ts:251-255`) — yet
   `extensions/loader.ts:699-704` loads only `.ts`/`.js` entries. A user's shell hook appears in the
   UI and is never executed, silently. OBSERVED.
