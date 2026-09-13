# 04 — Context Selection & Reduction Audit

**Scope:** local context filtering, pruning, compression, compaction, summarization, ranking, relevance selection, retrieval, chunking, deduplication, token budgeting, context injection, context-window management.
**Repo:** /Users/enverfrancisco/repositories/oh-my-pi @ 2026-09-12
**Method:** read-only source inspection. Every finding cites file:line + symbol. Docs/comments are not evidence; only runtime code paths are.

**Classification key**
- `LOCAL-DETERMINISTIC` — runs on this machine in ordinary code (TS/Rust/SQL). No model inference. Nothing leaves the machine to perform the reduction.
- `LOCAL-MODEL` — runs on this machine through an on-device model (ONNX / MLX / fastembed).
- `REMOTE-LLM` — the raw content is shipped to a remote provider, which returns the reduced form. Zero local reduction (or only a cap applied before shipping).

---

## 0. Executive answer

OMP has a **large and genuinely local deterministic reduction layer** (byte/line caps, tree-sitter structural summaries, supersede/useless pruning, shake elision, snapcompact rasterization, SQLite FTS5 + native MMR rerank) **and** a remote-LLM compaction layer that is the **default first choice**.

The honest split:

| Question | Answer |
|---|---|
| Does file reading send the whole file? | **B — selected sections**, with a **C — local structural summary** fast-path. Never the whole file by default. |
| Does compaction send raw history to a remote LLM? | **Yes, by default.** `DEFAULT_COMPACTION_METHOD_ORDER = ["remote","snapcompact","handoff","shake","soft"]` — `remote` is tried first. |
| Is there a local-model summarizer/ranker/classifier? | **Partially.** On-device models exist and are fully wired (ONNX + MLX + fastembed), but **every one of them is OFF by default** — defaults are `online` / `off`. |
| Local secret detection before egress? | Regex-only, on specific paths. **No local model does this.** See §5.4. |

---

## 1. File reading — THE CONCRETE ANSWER

### Answer: **B (selected sections) + C (local structural summary). Never A (full file).**

For a plain text/code file read with **no selector**, the pipeline is:

1. Whole file read into memory **locally only** when `fileSize <= 4 MiB` (`SNAPSHOT_MAX_BYTES`).
2. Binary sniff → refuse.
3. **Structural summary attempt (local tree-sitter, Rust).** If it parses and elides anything, the model receives the *summary*, not the file.
4. Otherwise a **line/byte-budgeted window** is emitted.

### Finding 1.1 — Read window budgets are local hard caps

**Evidence:**
- `packages/coding-agent/src/session/streaming-output.ts:9-10` — `DEFAULT_MAX_LINES = 3000`, `DEFAULT_MAX_BYTES = 50 * 1024`
- `packages/coding-agent/src/config/settings-schema.ts:3798-3800` — `"read.defaultLimit"` default `300`
- `packages/coding-agent/src/tools/read.ts:734-737` — `#defaultLimit = clamp(read.defaultLimit, 1, DEFAULT_MAX_LINES)`
- `packages/coding-agent/src/tools/read.ts:1771-1777` — `maxLinesToCollect = min(effectiveLimit + context, DEFAULT_MAX_LINES)`; `maxBytesForRead = max(DEFAULT_MAX_BYTES, maxLinesToCollect * 512)`

**Runtime path:** `ReadTool.execute` → `collectLineWindowFromBuffer` (`read.ts:282-370`) or `streamLinesFromFile` (`read.ts:380-568`).

**Observed behavior:** A bare `read foo.ts` emits **at most 300 lines** by default, hard-capped at 3000 lines, and byte-capped. Oversized single lines are refused with an explanatory notice rather than shipped (`read.ts:344-348`; `formatOmittedRequestedLineNotice` at `read.ts:267-274`). A 100 MB file never reaches the wire.

**Conclusion:** LOCAL-DETERMINISTIC. Byte/line selection happens in-process before anything is serialized into a tool result.
**Confidence: HIGH**

### Finding 1.2 — Structural summary mode is a local tree-sitter parser (the "C" answer)

**Evidence:**
- `packages/coding-agent/src/tools/read.ts:1662-1712` — the `parsed.kind === "none" && read.summarize.enabled` branch calling `trySummarize` then `renderSummary`
- `packages/coding-agent/src/tools/read-summary.ts:62-101` — `trySummarize()`; caps `MAX_SUMMARY_BYTES = 2 MiB` (line 36), `MAX_SUMMARY_LINES = 20_000` (line 37); calls `summarizeCode({code, path, minBodyLines, minCommentLines, unfoldUntilLines, unfoldLimitLines})`
- `packages/coding-agent/src/tools/read-summary.ts:103-199` — `renderSummary()` flattens kept/elided segments into text with `…` markers
- `crates/pi-natives/src/summary.rs:75-80` — `#[napi] pub fn summarize_code` delegating to `pi_ast::summary::summarize_code`
- `crates/pi-ast/src/summary.rs:162-200` — `summarize_code()`: `parse_cached(&source, language)` (tree-sitter), `collect_elidable_tree`, `select_folded_spans`, `build_segments`; returns `SummaryResult { parsed, elided, segments }`
- `crates/pi-ast/src/summary.rs:110-158` — `select_folded_spans`: pure BFS over the elidable-span forest, budgeted by `unfold_until` / `unfold_limit` visible-line targets

**Settings defaults** (`settings-schema.ts:3827-3900`): `read.summarize.enabled` **true**; `read.summarize.prose` false; `minBodyLines` 4; `minCommentLines` 6; `minTotalLines` 100; `unfoldUntil` 50; `unfoldLimit` 100.

**Observed behavior:** For any parseable source file >=100 lines and <=2 MiB, a selector-free `read` returns **declarations with bodies elided**, produced by an AST walk in Rust. The full source never leaves the process. Results are memoized per session in an LRU keyed by content hash (`read-summary.ts:26-35`); unusable results are memoized as `false` specifically so the full source is not retained.

**Conclusion:** LOCAL-DETERMINISTIC. This is the single strongest local-reduction mechanism in the codebase: source → local tree-sitter → declarations-only text → model.
**Confidence: HIGH**

### Finding 1.3 — Image-question mode on `read` IS a remote call

**Evidence:** `packages/coding-agent/src/tools/read.ts:12` imports `completeSimple` from `@oh-my-pi/pi-ai`; `read.ts:730-732` injects it as `completeImageRequest`; `read.ts:1045-1055` — `askImageQuestion(session, resolved, imageInput, …, this.completeImageRequest)`; `packages/coding-agent/src/utils/image-question.ts:79` — `askImageQuestion`.

**Observed behavior:** `read img.png?q=…` uploads the image to a **remote vision model** and returns its text answer. This saves tokens for the *calling* model, but it is not local reduction: the bytes still leave the machine.

**Conclusion:** REMOTE-LLM.
**Confidence: HIGH**

### Finding 1.4 — Document/notebook/archive/SQLite/profile conversion

**Evidence:** `read.ts:1593-1625` (notebook `notebookToEditableText`, markit `convertFileWithMarkit`); `read.ts:148-150` (`MAX_PROFILE_SUMMARY_BYTES = 32 MiB`); `tools/read-archive.ts`, `tools/read-sqlite.ts`, `utils/cpuprofile.ts`, `utils/sample-profile.ts`.

**Observed behavior:** All conversions run locally; converted output is routed back through the same selector/line-budget path (`buildInMemorySelectorResult`), so the budgets in Finding 1.1 still bind.

**Conclusion:** LOCAL-DETERMINISTIC.
**Confidence: HIGH**

---

## 2. Compaction — does raw history leave the machine?

### 2.1 Method order and dispatch

**Evidence:** `packages/coding-agent/src/session/compaction-methods.ts:41-48` —
```ts
export const DEFAULT_COMPACTION_METHOD_ORDER: CompactionMethod[] = ["remote","snapcompact","handoff","shake","soft"];
```
- `settings-schema.ts:2674-2677` — `"compaction.methodOrder"` default `[...DEFAULT_COMPACTION_METHOD_ORDER]`; `2641-2643` — `compaction.enabled` default **true**
- `compaction-methods.ts:74-81` — `STRATEGY_BY_COMPACTION_METHOD`: `remote→context-full`, `snapcompact→snapcompact`, `handoff→handoff`, `soft→context-full`, `shake→shake`
- `packages/coding-agent/src/session/session-maintenance.ts:833-886` — the ordered attempt loop; on failure it recurses to `selectedMethodIndex + 1` (`1160-1174`)

**Conclusion:** The **first** method attempted on a default install is `remote`. Local methods (`snapcompact`, `shake`) are fallbacks 2 and 4.
**Confidence: HIGH**

### 2.2 `remote` — REMOTE-LLM. Providers named.

**Evidence:** `packages/agent/src/compaction/compaction.ts:228-238` — `shouldUseProviderNativeCompaction()` = `shouldUseOpenAiRemoteCompaction(model) || shouldUseCompactionV2Streaming(model) || shouldUseAnthropicNativeCompaction(model)`.

**Providers used, by lane:**

**1. Anthropic — server-side compaction beta `compact-2026-01-12`**
- `packages/ai/src/providers/anthropic-wire.ts:171-172` — `export const COMPACTION_BETA = "compact-2026-01-12"`
- `anthropic-wire.ts:296-304` — `CompactionEdit { type: "compact_20260112", trigger, pause_after_compaction, instructions }`
- `packages/agent/src/compaction/anthropic.ts:34` — `ANTHROPIC_COMPACTION_MIN_TRIGGER_TOKENS = 50_000`; `:42` — `ANTHROPIC_COMPACTION_MIN_CONTEXT_TOKENS = 55_000`
- `packages/ai/src/providers/anthropic.ts:1898-1911` — `buildAnthropicCompactionEdit`
- The module contract at `compaction/anthropic.ts:5-7` states it plainly: *"The compaction request is the live turn's own request shape — same system prompt, tools, and message history — plus the `compact_20260112` edit."*

**2. OpenAI Responses — `/responses/compact` (V1) and streaming `compaction_trigger` (V2)**
- `packages/agent/src/compaction/openai.ts:1-16` (module contract), `:66` `OPENAI_REMOTE_COMPACTION_PRESERVE_KEY`, `:74` `REMOTE_COMPACTION_TIMEOUT_MS = 300_000`
- `packages/agent/src/compaction/compaction-v2-streaming.ts` — `buildCompactionV2Request`, `requestCompactionV2Streaming`

**3. OpenAI Codex Responses**
- `compaction.ts:29-33` imports `createOpenAICodexCompactionRequestContext`, `buildTransformedCodexRequestBody`, `OpenAICodexCompactionBody`

**4. Generic self-hosted endpoint (`compaction.remoteEndpoint`)**
- `compaction.ts:982-989` — `requestRemoteCompaction(endpoint, { systemPrompt: SUMMARIZATION_SYSTEM_PROMPT, prompt: promptText, maxTokens }, …)`

**Runtime path:** `SessionMaintenance.compact` → `#compactWithFallbackModel` (`session-maintenance.ts:2674-2741`) → `compact(preparation, candidate, apiKey, …)` in `packages/agent/src/compaction/compaction.ts`. Candidate order comes from `resolveCompactionModelCandidates` (`session-maintenance.ts:2621-2658`): configured `compactionModel` → active model → every `MODEL_ROLE_IDS` role → largest-context available model.

**Observed behavior:** The conversation history is transmitted to the provider essentially intact. On the Anthropic lane it is deliberately **byte-identical to the live turn's prefix** so the prompt cache hits — `session-maintenance.ts:1097-1100` passes `remoteSystemPrompt: this.#host.agent.state.systemPrompt` explicitly for that reason. No local summarization precedes it.

**Conclusion:** **REMOTE-LLM. Raw history leaves the machine before being compressed.** Providers: **Anthropic (compact-2026-01-12 beta), OpenAI Responses (compact V1 + streaming V2), OpenAI Codex Responses, and any configured OpenAI-compatible `remoteEndpoint`.**
**Confidence: HIGH**

### 2.3 `soft` — REMOTE-LLM with a shallow local pre-pass

**Evidence:** `packages/agent/src/compaction/compaction.ts:857-935` — `generateSummary()`; `:935-1010` — `summarizeConversationWindow()` calling `instrumentedCompleteSimple(model, { systemPrompt: [SUMMARIZATION_SYSTEM_PROMPT], messages }, …)`.

**The local pre-pass is real but shallow:**
- `packages/agent/src/compaction/utils.ts:203` — `TOOL_RESULT_MAX_CHARS = 2000`; `:208-212` — `truncateToolResultForSummary()` clips every tool result to 2000 chars before serialization
- `utils.ts:240-249` — tool results flagged `useless` (and their paired calls) are **dropped entirely** from summary input
- `utils.ts:252-262` — for the `anthropic` dialect, `thinking` blocks are dropped from summary input
- `utils.ts:221-225` — `serializeConversationForSummary`; `:215` `escapeSummaryBoundaryTags` prevents boundary-tag injection
- `compaction.ts:826-857` — `planSummaryWindows()` splits an oversized conversation into budget-sized windows, folding the summary across them
- `compaction.ts:819-822` — `clampConversationToBudget()` hard-truncates with `[... N more characters truncated]`
- `compaction.ts:210,213-215` — `DEFAULT_RESERVE_TOKENS = 16384`, `MAX_SUMMARY_TOKENS = DEFAULT_RESERVE_TOKENS` caps the *output*

**Observed behavior:** A 100 MB history is reduced locally to a per-tool-result-2000-char transcript, **then that transcript is shipped whole** to a remote model which returns the summary. The compression work is remote.

**Conclusion:** REMOTE-LLM (with a LOCAL-DETERMINISTIC truncation/deduplication pre-pass).
**Confidence: HIGH**

### 2.4 `handoff` — REMOTE-LLM

**Evidence:** `compaction.ts:584` `HANDOFF_DOCUMENT_PROMPT`; `:1056-1061` `renderHandoffPrompt`; `:1109` and `:1116` — two `instrumentedCompleteSimple(model, context, requestOptions, { oneshotKind: "handoff" })` calls. `session-maintenance.ts:1571-1580` — `prepareCompaction(entries, resolveMethodSettings(compactionSettings, "handoff"), …)`.

**Observed behavior:** Retained history is sent to a model that writes a handoff document; the document becomes the compaction summary.

**Conclusion:** REMOTE-LLM.
**Confidence: HIGH**

### 2.5 `snapcompact` — LOCAL-DETERMINISTIC (the genuine local compressor)

**Evidence:** `packages/snapcompact/src/snapcompact.ts:2038-2187` — `export async function compact()`. Pipeline, entirely in-process:

1. `serializeConversation(llmMessages, options)` (`snapcompact.ts:933`) with local caps `TOOL_RESULT_MAX_CHARS = 2000` (`:737`), `TOOL_ARG_MAX_CHARS = 500` (`:741`), `TOOL_CALL_MAX_CHARS = 2000` (`:744`), `TRUNCATE_HEAD_RATIO = 0.6` (`:748`)
2. `normalize()` (`:1346`) — whitespace/zero-width collapse, `NEWLINE_GLYPH` (`:1146`) substitution
3. `elideDataUrls()` — strips embedded data URLs so a frame slice cannot split a payload
4. `planArchive()` (`:1960+`) — cell-aware pagination into frames; **foveation**: HQ edges (`HQ_EDGE_FRAMES = 3`, `:469`), dense LQ middle, oldest dense slice dropped when over budget
5. `render()` (`:1619-1625`) → `renderSnapcompactPng(text, nativeRenderOptions(shape, size))`

**The renderer is native Rust:** `crates/pi-natives/src/snapcompact.rs:1202-1213` — `#[napi] pub fn render_snapcompact_png` → `render_snapcompact_png_sync` → `render_bitmap` / `render_ttf_rgb` / `render_doc_bitmap` / `render_ttf_doc_rgb` (`:588`, `:677`, `:760`, `:858`). Pixel-font rasterization + PNG encode. **No network, no API key.**

**Local budgets:** `MAX_FRAMES_DEFAULT = 80` (`:464`), `FRAME_TOKEN_ESTIMATE = 5024` (`:475`), `FRAME_DATA_BYTES_ESTIMATE = 170_000` (`:481`), `FRAME_DATA_BYTES_BUDGET = 3_000_000` (`:488`), `maxFramesForDataBudget()` (`:491`), `PROVIDER_IMAGE_BUDGETS` (`:507-520`: anthropic 90, bedrock 90, openai 200, unknown floor 5 via `DEFAULT_PROVIDER_IMAGE_BUDGET` `:520`), `providerFrameBudget()` (`:528-530`).

**Important nuance:** the *output* (PNG frames, `images(archive)` at `:1821-1828`) is sent to the remote vision model on every rebuilt request. But the **reduction itself is 100% local** — this is exactly the `100MB → local renderer → 80 frames → Claude` shape, not `100MB → Claude → summary`.

**Local gating** (all in `session-maintenance.ts`): requires a vision model (`:938-941`); refuses above an unrenderable-character ratio (`:958-962`); refuses when kept history alone exceeds the budget (`:987-990`); refuses when standing image payload exceeds the per-request budget (`:1012-1016`); refuses when it "would not reduce context" (`:1041-1042`) or cannot get under the limit (`:1050-1053`).

**Conclusion:** LOCAL-DETERMINISTIC. No LLM call, no API key.
**Confidence: HIGH**

### 2.6 `snapcompact-inline` — LOCAL-DETERMINISTIC per-request transform

**Evidence:** `packages/coding-agent/src/session/snapcompact-inline.ts:1-83`. Runs inside the agent loop's `transformProviderContext` hook — after persisted history is converted to the outgoing `Context`, before the provider stream call. Swaps the system prompt, loaded context files, and large historical tool results for PNG frames.
**Gates:** `MAX_SYSTEM_PROMPT_FRAMES = 6` (`:52`), `MIN_TOOL_RESULT_TOKENS = 3000` (`:55`), `SAVINGS_MARGIN = 0.9` (`:57`), `passesSavingsGate()` (`:72-74`), `countMessageImages()` (`:62-70`).

**Conclusion:** LOCAL-DETERMINISTIC.
**Confidence: HIGH**

### 2.7 `shake` — LOCAL-DETERMINISTIC elision

**Evidence:** `packages/agent/src/compaction/shake.ts`
- `collectShakeRegions()` (`:308-380`) — walks entries, computes an `accumulatedAfter[]` token suffix, skips entries before `keepBoundaryId`, honors `protectTokens` and `protectedTools`; `useless`-flagged results bypass the protect window
- `scanTextForBlockRanges()` (`:155-220`) — pure string scan for fenced code blocks and top-level XML spans; `mergeRanges()` (`:228-241`) keeps the outermost
- `applyShakeRegion()` / `applyShakeRegions()` (`:437-475`) — in-place placeholder replacement, non-text blocks preserved, `prunedAt` stamped, message cache invalidated
- Presets: `DEFAULT_SHAKE_CONFIG` (`:46-51`; protect 16k, minSavings 4k, fenceMinTokens 400), `AGGRESSIVE_SHAKE_CONFIG` (`:59-64`), `RESCUE_SHAKE_CONFIG` (`:67-74`)
- Offload target: `session-maintenance.ts:3119-3123, 3154-3158` — elided content goes to one `artifact://` blob behind a recoverable placeholder. **Local disk, not the network.**

**Conclusion:** LOCAL-DETERMINISTIC. No model involved.
**Confidence: HIGH**

---

## 3. Pruning / deduplication (per-turn, outside compaction)

### Finding 3.1 — Supersede pruning (deduplication of repeated reads)

**Evidence:** `packages/agent/src/compaction/pruning.ts:238-300+` — `pruneSupersededToolResults()`; `collectSupersededResults()` (`:176-215`) keyed by `SupersedeKeyFn` (`:80`); `SUPERSEDED_NOTICE = "[Superseded by a newer read of this file]"` (`:65`). Wired at `session-maintenance.ts:551-556` with `supersedeKey: supersedeReads ? readToolSupersedeKey : undefined`. Default `compaction.supersedeReads` **true** (`settings-schema.ts:2835-2838`).

**Cache-awareness (local heuristic):** `computeMessageSuffixTokens()` (`:145-156`) + `DEFAULT_SUFFIX_TOKEN_LIMIT = 8_000` (`:107`) + idle flush (`:108`, overridden to `PRUNE_IDLE_FLUSH_MS = 90 min` at `session-maintenance.ts:194`). Prunes only in the cheap-to-recache tail, or once the provider cache is cold.

**Observed behavior:** Re-reading the same file replaces the older result with a ~8-token placeholder. The read→edit→read loop stops accumulating duplicate file bodies.

**Conclusion:** LOCAL-DETERMINISTIC deduplication.
**Confidence: HIGH**

### Finding 3.2 — "Useless" tool-result elision

**Evidence:** `pruning.ts:66` `USELESS_NOTICE = "[Uneventful result elided]"`; `collectUselessResults()` (`:223-238`). Default `compaction.dropUseless` **true** (`settings-schema.ts:2846-2849`).
Flag producers, each setting `useless: true` deterministically at result construction: `tools/memory-recall.ts:45-47, 82-84` (zero hits); `tools/vibe.ts:203-218` (nothing settled); `hub/messaging.ts:414-415, 441-442` (clean wait timeout / empty inbox); `hub/jobs.ts:320-321, 351-352, 365-366`; `tools/gh-run-watch.ts:1023-1024`; `advisor/advise-tool.ts:281-282, 297-298`; `lsp/tool.ts:1513-1514`. Builder support at `tools/tool-result.ts:94-95`.

**Conclusion:** LOCAL-DETERMINISTIC.
**Confidence: HIGH**

### Finding 3.3 — Age-based output pruning

**Evidence:** `pruning.ts:54-59` — `DEFAULT_PRUNE_CONFIG { protectTokens: 40_000, minimumSavings: 20_000, protectedTools: ["skill", isSkillReadToolResult], pruneUseless: true }`; `MIN_PRUNE_TOKENS = 50` (`:122`); `createPrunedNotice()` (`:110-112`) → `[Output truncated - N tokens]`. Invoked from `session-maintenance.ts:507-512` (`pruneToolOutputs`).

**Conclusion:** LOCAL-DETERMINISTIC.
**Confidence: HIGH**

### Finding 3.4 — Protection matchers (relevance whitelist)

**Evidence:** `packages/agent/src/compaction/tool-protection.ts` — `isProtectedToolResult`, `isSkillReadToolResult`, `isArtifactRecoveryToolResult`, `collectToolCallsById`. Plan-mode protection layered via `createPlanReadMatcher` (`plan-mode/plan-protection.ts`, used at `session-maintenance.ts:552`).

**Observed behavior:** Rule-based, not semantic. Skill reads and artifact-recovery reads are exempt from pruning/shaking.

**Conclusion:** LOCAL-DETERMINISTIC.
**Confidence: HIGH**

---

## 4. Token budgeting and context-window management

### Finding 4.1 — Tokenizer is native and local

**Evidence:** `packages/agent/src/tokenizer.ts:131-175` — `class Tokenizer`; `countTokensNat()` (`:70-88`) calls `natives.countTokens(text, encoding)` (Rust/NAPI, tiktoken-family encodings). Modes `strict | approximate | upperbound` (`:37`); `approximate` falls back to bytes/4, `upperbound` to raw byte length (never undercounts). `checkTokenBudget()` (`:168-175`) probes the cheap upper bound first and pays for an exact count only when it busts.

**Conclusion:** LOCAL-DETERMINISTIC. No remote token counting anywhere in the reduction path.
**Confidence: HIGH**

### Finding 4.2 — Cut-point selection

**Evidence:** `packages/agent/src/compaction/compaction.ts:509-573` — `findCutPoint()`: walks backwards accumulating `tokenizer.countMessage(entry.message)` until `keepRecentTokens`, snaps to a valid cut point, never cuts at a tool result, then absorbs preceding non-message entries. Defaults: `keepRecentTokens: 20000`, `DEFAULT_RESERVE_TOKENS = 16384` (`:210`), `DEFAULT_COMPACTION_SETTINGS` (`:219-230`).

**Conclusion:** LOCAL-DETERMINISTIC.
**Confidence: HIGH**

### Finding 4.3 — Threshold / overflow classification

**Evidence:** `calculateContextTokens()` / `calculatePromptTokens()` (`compaction.ts:245-280`); `packages/ai/src/error/flags.ts:813` `isUsageBackedContextOverflow()`, `:827` `isPayloadRejection()`; `flags.ts:86-94` — local regex table classifying provider overflow strings (OpenRouter, llama.cpp `n_ctx`, LM Studio, MiniMax, Kimi). `session-maintenance.ts:200-210` — `COMPACTION_RECOVERY_BAND = 0.8` hysteresis; `:213` `PAYLOAD_REJECTION_OCCUPANCY_CEILING = 0.9`.

**Conclusion:** LOCAL-DETERMINISTIC.
**Confidence: HIGH**

### Finding 4.4 — Append-only context mode (prefix stability)

**Evidence:** `packages/agent/src/append-only-context.ts:219-225` — `AppendOnlyContextManager.syncMessages` finds the longest byte-stable prefix instead of clearing the log on any in-place rewrite. `packages/coding-agent/src/config/append-only-context-mode.ts:11-15`.

**Observed behavior:** Not a reduction mechanism — a re-prefill-avoidance mechanism. Recorded because it constrains what the reducers in §3 are permitted to rewrite.

**Conclusion:** LOCAL-DETERMINISTIC (not a reducer).
**Confidence: HIGH**

---

## 5. Tool-output capping

> Per the audit constraint: local execution is not local reduction. These qualify as **reduction** because the cap is applied to the bytes *before* they are placed in the tool result that ships.

### Finding 5.1 — Inline byte cap with local artifact spill

**Evidence:** `packages/coding-agent/src/session/streaming-output.ts:662-684` — `enforceInlineByteCap()`: keeps ~60% head + ~25% tail on line boundaries (never splitting a multi-byte UTF-8 sequence), inserts `[…NB elided…]`, and appends `[raw output: artifact://<id>]` when `saveArtifact` returns an id. The full text is written to a **local** artifact file. Wired in `tools/bash.ts:839, 851` via `resolveInlineByteCapBudget(this.session.settings)` and `saveBashOriginalArtifact`.

**Observed behavior:** A bash command emitting 100 MB ships <=~50 KB to the model; the rest stays on disk, recoverable via `artifact://`.

**Conclusion:** LOCAL-DETERMINISTIC.
**Confidence: HIGH**

### Finding 5.2 — Streaming tail buffer

**Evidence:** `streaming-output.ts:688-760` — `class TailBuffer`, ring-style, `maxBytes`-bounded, lazy join, `#compact()` past `MAX_PENDING = 10`. `bash.ts:924` — `new TailBuffer(DEFAULT_MAX_BYTES)`. Artifact windows: `ARTIFACT_DEFAULT_HEAD_BYTES = 3 MiB` (`streaming-output.ts:21`), `ARTIFACT_DEFAULT_MAX_BYTES = 0` (unbounded on disk, `:19`).

**Conclusion:** LOCAL-DETERMINISTIC.
**Confidence: HIGH**

### Finding 5.3 — Grep column and result truncation

**Evidence:** `streaming-output.ts:11` — `DEFAULT_MAX_COLUMN = 512`. `tools/grep.ts:552, 566` — `truncateLine(line, DEFAULT_MAX_COLUMN)` per match line; `:1595` — `truncateHead(rawOutput, …)`; `:1598-1605` aggregates `fileLimitReached / perFileLimitReached / totalMatchLimitReached / result.limitReached / truncation.truncated / linesTruncated` into a single `truncated` flag surfaced in `details`.

**Observed behavior:** Grep's 10,000 lines **do** get reduced before being sent — per-line to 512 columns, per-result by head truncation and file/match window limits.

**Conclusion:** LOCAL-DETERMINISTIC.
**Confidence: HIGH**

### Finding 5.4 — Secret redaction before egress (pointer; owned by the SecretsPrivacy audit)

**Evidence:** `packages/coding-agent/src/secrets/obfuscator.ts` — `SecretObfuscator` with `#redactRegexMatchOutsidePlaceholders` (`:361`, `:678`); `secrets/regex.ts:7` `compileSecretRegex`; `secrets/message-transform.ts`. Applied to compaction input at `session-maintenance.ts:2700-2702` — `obfuscatePreparationForProvider(preparation)` and `obfuscateTextForProvider(customInstructions)`.
Separate, weaker regex redaction on memory writes: `memories/index.ts:1134-1140` and `sharpshooter/consolidate.ts:309-313` — pattern `/(?:sk|pk|rk|tok|key|secret|token|password)[-_A-Za-z0-9]{12,}/g`. `share.redactSecrets` default **true** (`settings-schema.ts:2554-2557`).

**Conclusion:** LOCAL-DETERMINISTIC (regex). **No local model performs secret detection — MISSING.**
**Confidence: HIGH**

---

## 6. Retrieval / ranking (mnemopi memory backend)

> Gate: `memory.backend` default is **`off`** (`settings-schema.ts:3107-3110`). Everything in this section is dormant on a default install.

### Finding 6.1 — SQLite FTS5 lexical retrieval

**Evidence:** `packages/mnemopi/src/core/beam/schema.ts:131-167` — `CREATE VIRTUAL TABLE fts_episodes USING fts5(content, content='episodic_memory', content_rowid='rowid')`, `fts_working`, plus insert/delete/update triggers; `:363-377` — `fts_facts`.
`core/beam/recall.ts:540-575` — `ftsRows()` issuing `MATCH ? ORDER BY f.rank LIMIT ?` with a correlated `EXISTS` visibility filter (chosen over `IN (SELECT…)` for a measured 9.19ms→0.042ms win).
`core/beam/helpers.ts:295-326` — `ftsSearch`, `ftsSearchWorking`, `cjkLikeSearch` fallback; `:252-254` `buildFtsQuery`.
`util/regex.ts:150-160` — `ftsQueryTerms`; `:4-90` — `FACT_MATCH_STOPWORDS`, `recallTokens`.

**Conclusion:** LOCAL-DETERMINISTIC retrieval (BM25-family ranking inside SQLite FTS5).
**Confidence: HIGH**

### Finding 6.2 — Hybrid score fusion and query-intent weighting

**Evidence:** `core/query-intent.ts:69-76` — `INTENT_WEIGHTS` table (temporal / factual / entity / preference / procedural / general → vec/fts/importance biases); `:78` `classifyIntent(query)` — a **rule/regex classifier, not a model**; `:107-139` normalizes the three weights to sum 1. `config.ts:240-257` — `ftsWeight` default 0.3, plus `vectorWeight` / `importanceWeight`. `core/beam/helpers.ts:99-110` — weight resolution with env overrides. `core/polyphonic-recall.ts` — 4-voice reciprocal rank fusion, default **off** (`settings-schema.ts:3276-3279`).

**Conclusion:** LOCAL-DETERMINISTIC ranking. `classifyIntent` is explicitly a **local rule-based classifier, not a local model**.
**Confidence: HIGH**

### Finding 6.3 — MMR reranking in native Rust

**Evidence:** `packages/mnemopi/src/core/mmr.ts:1` — `import { mmrRerankIndices } from "@oh-my-pi/pi-natives"`; `:24-60` — `mmrRerank()` calling `mmrRerankIndices(contents, scores, lambdaParam, nativeLimit)`; `:84-89` — JS fallback implementing `lambda*relevance - (1-lambda)*maxSimilarity`; `:11-22` — `jaccardSimilarity`. `crates/pi-natives/src/vectors.rs:264-268` — `#[napi] pub fn mmr_rerank_indices(contents, scores, lambda_param, …)`.

**Conclusion:** LOCAL-DETERMINISTIC reranker (diversity-aware, no model).
**Confidence: HIGH**

### Finding 6.4 — Recall injection is token-budgeted locally

**Evidence:** `packages/coding-agent/src/mnemopi/backend.ts:144-146` — `truncateApproxTokens(rendered, settings.get("mnemopi.injectionTokenLimit"))`. Defaults: `mnemopi.injectionTokenLimit = 5000` (`settings-schema.ts:3416`), `recallLimit` 8, `recallContextTurns` 3, `recallMaxQueryChars` 4000 (`:3413-3415`).

**Conclusion:** LOCAL-DETERMINISTIC context-injection budgeting.
**Confidence: HIGH**

### Finding 6.5 — Embedding input clipping / chunking

**Evidence:** `packages/mnemopi/src/core/embeddings.ts:206-216` — `effectiveMaxInputChars()` default **8192**; `:229-236` — `clipToWindow()` head/tail split with `EMBEDDING_ELISION_MARKER`; `:249-276` — `capInputs()` with a logged trim summary (silent truncation was bug #3126).

**Conclusion:** LOCAL-DETERMINISTIC chunking.
**Confidence: HIGH**

---

## 7. Local models — what EXISTS vs what RUNS

### Finding 7.1 — Local inference runtimes ARE present

**ONNX Runtime (`onnxruntime-node`)**
- `package.json:57` — catalog pin `onnxruntime-node 1.26.0`
- `packages/coding-agent/scripts/bundle-dist.ts:16` — `ALWAYS_EXTERNAL = ["@oh-my-pi/pi-natives", "@huggingface/transformers", "fastembed", "onnxruntime-node"]`
- `packages/coding-agent/scripts/compile-binary.ts:8` — `COMPILED_EXTERNAL_DEPENDENCIES = ["fastembed", "onnxruntime-node"]`
- `packages/coding-agent/src/subprocess/worker-runtime.ts:31, 462-463` — side-install of `@huggingface/transformers` + `onnxruntime-node`, incl. CUDA provider files
- `packages/coding-agent/src/tiny/device.ts:26-27, 86-109` — device enum: `cpu, gpu, mlx, metal, webgpu, cuda, dml, **coreml**, auto, wasm, webnn, webnn-gpu, webnn-npu`

**MLX (Apple silicon)**
- `packages/coding-agent/src/tiny/device.ts:7` — `export const MLX_DEVICE = "mlx"`; `:40-42` — `tinyMlxSupported()` = darwin + arm64
- `packages/coding-agent/src/tiny/mlx-runtime.ts:13-26` — pinned `MLX_LM_VERSION = "0.31.3"`, private venv under the agent cache, `getTinyMlxModelDir`; `:45-66` — `uv pip install mlx-lm==…` with a system-python fallback
- `packages/coding-agent/src/tiny/mlx-server.py:1-21` — per-model MLX worker speaking the same JSON-lines protocol as the ONNX worker
- `packages/coding-agent/src/tiny/title-client.ts:167-171` `tinyWorkerUsesMlx()`, `:398-410` MLX spawn, `:555-559` fallback to ONNX CPU on bootstrap failure
- `packages/coding-agent/src/tiny/title-protocol.ts:32-36` — `TinyWorkerBackend = "onnx" | "mlx"`

**Local embeddings (`fastembed`)**
- `packages/mnemopi/package.json:48-57` — `fastembed 2.1.0` + `onnxruntime-node 1.21.0` as **optional peers**
- `packages/mnemopi/src/core/fastembed-runtime.ts:44-92` — runtime resolution and native-module path setup
- `packages/mnemopi/src/core/embeddings.ts:133-165` — `defaultLocalModelInitializer()` with corrupt-blob quarantine (`:92-95`) and incomplete-cache heal (`:112-126`); `:396-423` — `getLocalModel()`; `:379-388` — `KNOWN_MODEL_NAMES` (bge-small/base/large en+zh, multilingual-e5-large, all-MiniLM-L6-v2)
- `packages/mnemopi/src/embed-client.ts:20-23` — embeddings run in a **subprocess** (Bun/Windows segfault, issue #3031)

**Local model registry**
- `packages/coding-agent/src/tiny/models.ts:26-48` — title models: LFM2.5-230M / LFM2.5-350M / Falcon-H1-Tiny-90M, each with a 4-bit `mlxRepo`
- `tiny/models.ts:113-158` — memory/classifier models: Qwen3-1.7B (MLX-only; `onnxUnsupportedReason` at `:120-121`), Llama-3.2-3B, Gemma-3-1B, Qwen2.5-1.5B, LFM2-1.2B

**Local ASR (adjacent, not a context reducer):** `packages/coding-agent/src/stt/models.ts:60-84` — whisper-base / small / large-v3-turbo via transformers.js or sherpa.

### Finding 7.2 — But EVERY local model is OFF by default

| Capability | Setting | Default | Effective behavior |
|---|---|---|---|
| Session titles | `providers.tinyModel` (`settings-schema.ts:5729-5732`) | `ONLINE_TINY_TITLE_MODEL_KEY` = `"online"` | **Remote** TINY/@smol role |
| Thinking-difficulty classifier | `providers.autoThinkingModel` (`:5783-5786`) | `ONLINE_AUTO_THINKING_MODEL_KEY` | **Remote** |
| Unexpected-stop classifier | `providers.unexpectedStopModel` (`:5839-5842`) | `ONLINE_MEMORY_MODEL_KEY` = `"online"` | **Remote** (and feature default is `mechanical`, `:5814-5817`, so neither runs) |
| Memory backend (any retrieval) | `memory.backend` (`:3107-3110`) | **`"off"`** | No retrieval, no embeddings |
| Mnemopi extraction LLM | `mnemopi.llmMode` (`:3356-3359`) | `"smol"` | **Remote** |
| Local embeddings | gated by `memory.backend != off` + `mnemopi.noEmbeddings` (`:3310-3313`) | off by consequence | — |
| Inference device | `providers.tinyModelDevice` (`:5742-5745`) | `TINY_MODEL_DEVICE_DEFAULT` (CPU-only ONNX) | MLX / CoreML are opt-in |

**Dual-path proof** — online and local branches coexist in each classifier; the default selects online:
- `auto-thinking/classifier.ts:149-189` `classifyOnline()` → `completeSimple(model, …)` **remote**; `:191-213` `classifyLocal()` → `tinyModelClient.complete(modelKey, builtPrompt, …)` **local**
- `session/unexpected-stop-classifier.ts:92-133` `classifyOnline()` **remote**; `:135-151` `classifyLocal()` **local**
- `utils/title-generator.ts:194-206` — `tinyTitleClient.generate(...)`, reached only when `providers.tinyModel` names a local key; `:206-208` logs and skips rather than falling back online
- `coding-agent/src/mnemopi/backend.ts:535-539` — `tinyModelClient.complete(memoryModel, …)` for local memory extraction/consolidation
- Client wiring: `tiny/title-client.ts:811-814` — `tinyTitleClient`, aliased as `tinyModelClient`

### Finding 7.3 — The MISSING answers

Searched and confirmed absent as *runtime, default-on* mechanisms:

- **Local-model summarization of conversation history.** `MISSING.` No code path routes compaction summarization through `tinyModelClient`. `compaction.ts` reaches only `instrumentedCompleteSimple`, `requestRemoteCompaction`, or a native provider lane. The only local compaction is `snapcompact` (rasterizer) and `shake` (elision) — neither is a model.
- **Local reranker model (cross-encoder / neural).** `MISSING.` The only reranker is deterministic MMR in `crates/pi-natives/src/vectors.rs:265`.
- **Local-model secret detection.** `MISSING.` Redaction is regex-only (Finding 5.4).
- **Local embeddings for code/repo retrieval.** `MISSING.` `fastembed` is scoped to mnemopi *memory* only. File/code search is `grep` / `glob` / `ast-grep` — lexical and structural, never embedding-based.
- **Local semantic relevance selection over tool results before egress.** `MISSING.` Selection is positional (recency windows), rule-based (supersede keys, `useless` flags, protected-tool whitelists) and size-based — never semantic.
- **llama.cpp / Ollama / LM Studio as a *reduction* engine.** `MISSING as a reducer.` They appear throughout `packages/ai` only as **inference providers** for the main agent loop (`providers/ollama.ts`, overflow regexes at `error/flags.ts:87-91`, prefix-cache notes at `config/append-only-context-mode.ts:11-15`). Pointing OMP at a local llama.cpp endpoint makes the *whole agent* local; it does not add a local reduction stage.
- **Core ML.** Present only as an ONNX execution-provider enum value (`tiny/device.ts:27, 106`), whose own option text says "opt-in; can fail to load". No default use.

**Conclusion:** LOCAL-MODEL capability is **built and shipped but dormant**. On a default install, **zero local models perform summarization, ranking, secret detection, classification, embeddings or retrieval.**
**Confidence: HIGH**

---

## 8. Master classification table

| # | Mechanism | Key symbol | Evidence | Class |
|---|---|---|---|---|
| 1 | Read line/byte window | `collectLineWindowFromBuffer`, `streamLinesFromFile` | `tools/read.ts:282,380`; `streaming-output.ts:9-10` | **LOCAL-DETERMINISTIC** |
| 2 | Read structural summary | `trySummarize` → `summarize_code` | `read-summary.ts:62`; `crates/pi-ast/src/summary.rs:163` | **LOCAL-DETERMINISTIC** |
| 3 | Read image-question | `askImageQuestion` | `read.ts:1046`; `utils/image-question.ts:79` | **REMOTE-LLM** |
| 4 | Compaction `remote` — Anthropic | `requestAnthropicNativeCompaction` | `compaction/anthropic.ts`; `ai/providers/anthropic-wire.ts:172` | **REMOTE-LLM** |
| 5 | Compaction `remote` — OpenAI V1/V2/Codex | `requestOpenAiRemoteCompaction`, `requestCompactionV2Streaming` | `compaction/openai.ts`, `compaction-v2-streaming.ts` | **REMOTE-LLM** |
| 6 | Compaction `remote` — generic endpoint | `requestRemoteCompaction` | `compaction.ts:982-989` | **REMOTE-LLM** |
| 7 | Compaction `soft` | `generateSummary`, `summarizeConversationWindow` | `compaction.ts:857,935` | **REMOTE-LLM** |
| 8 | └ its local pre-pass | `truncateToolResultForSummary`, `serializeConversation` | `compaction/utils.ts:203-262` | **LOCAL-DETERMINISTIC** |
| 9 | Compaction `handoff` | `renderHandoffPrompt` + `instrumentedCompleteSimple` | `compaction.ts:1056,1109` | **REMOTE-LLM** |
| 10 | Compaction `snapcompact` | `snapcompact.compact` → `render_snapcompact_png` | `snapcompact.ts:2038`; `crates/pi-natives/src/snapcompact.rs:1203` | **LOCAL-DETERMINISTIC** |
| 11 | Snapcompact inline imaging | `planInlineSwaps` | `session/snapcompact-inline.ts:1-83` | **LOCAL-DETERMINISTIC** |
| 12 | Compaction `shake` | `collectShakeRegions`, `applyShakeRegions` | `compaction/shake.ts:308,466` | **LOCAL-DETERMINISTIC** |
| 13 | Supersede dedup | `pruneSupersededToolResults` | `compaction/pruning.ts:238` | **LOCAL-DETERMINISTIC** |
| 14 | Useless-result elision | `collectUselessResults` | `pruning.ts:223` | **LOCAL-DETERMINISTIC** |
| 15 | Age-based output pruning | `pruneToolOutputs` | `pruning.ts:54` | **LOCAL-DETERMINISTIC** |
| 16 | Tool-result protection | `isProtectedToolResult` | `compaction/tool-protection.ts` | **LOCAL-DETERMINISTIC** |
| 17 | Token counting | `Tokenizer.countTokens` → `natives.countTokens` | `agent/src/tokenizer.ts:76,152` | **LOCAL-DETERMINISTIC** |
| 18 | Cut-point selection | `findCutPoint` | `compaction.ts:509` | **LOCAL-DETERMINISTIC** |
| 19 | Overflow / 413 classification | `isUsageBackedContextOverflow`, `isPayloadRejection` | `ai/src/error/flags.ts:813,827` | **LOCAL-DETERMINISTIC** |
| 20 | Bash inline byte cap + artifact spill | `enforceInlineByteCap`, `TailBuffer` | `streaming-output.ts:662,690` | **LOCAL-DETERMINISTIC** |
| 21 | Grep column/head truncation | `truncateLine`, `truncateHead` | `tools/grep.ts:552,1595` | **LOCAL-DETERMINISTIC** |
| 22 | Secret obfuscation pre-egress | `SecretObfuscator` | `secrets/obfuscator.ts`; `session-maintenance.ts:2700` | **LOCAL-DETERMINISTIC** |
| 23 | FTS5 memory retrieval | `ftsSearch`, `ftsRows` | `mnemopi/core/beam/helpers.ts:295`, `recall.ts:540` | **LOCAL-DETERMINISTIC** |
| 24 | Hybrid weight fusion / intent | `classifyIntent`, `INTENT_WEIGHTS` | `mnemopi/core/query-intent.ts:69,78` | **LOCAL-DETERMINISTIC** |
| 25 | MMR rerank | `mmrRerankIndices` | `mnemopi/core/mmr.ts:1`; `crates/pi-natives/src/vectors.rs:265` | **LOCAL-DETERMINISTIC** |
| 26 | Recall injection budget | `truncateApproxTokens` | `coding-agent/src/mnemopi/backend.ts:145` | **LOCAL-DETERMINISTIC** |
| 27 | Embedding input clipping | `capInputs`, `clipToWindow` | `mnemopi/core/embeddings.ts:229,249` | **LOCAL-DETERMINISTIC** |
| 28 | Memory embeddings (fastembed/ONNX) | `defaultLocalModelInitializer` | `mnemopi/core/embeddings.ts:133` | **LOCAL-MODEL** *(default OFF — `memory.backend: off`)* |
| 29 | Session-title generation | `tinyTitleClient.generate` | `tiny/title-client.ts:811`; `utils/title-generator.ts:195` | **LOCAL-MODEL** *(default `online` → REMOTE-LLM)* |
| 30 | Thinking-difficulty classifier | `classifyLocal` / `classifyOnline` | `auto-thinking/classifier.ts:191` / `:149` | **LOCAL-MODEL** *(default `online` → REMOTE-LLM)* |
| 31 | Unexpected-stop classifier | `classifyLocal` / `classifyOnline` | `session/unexpected-stop-classifier.ts:135` / `:92` | **LOCAL-MODEL** *(default `online`; feature default `mechanical`)* |
| 32 | Memory extraction / consolidation | `tinyModelClient.complete` | `coding-agent/src/mnemopi/backend.ts:536` | **LOCAL-MODEL** *(default `mnemopi.llmMode: smol` → REMOTE-LLM)* |
| 33 | Mnemopi remote embedding API | `embedApi` | `mnemopi/core/embeddings.ts:426-470` | **REMOTE** (non-LLM HTTP embedding endpoint) |

---

## 9. Documentation vs. code discrepancies

**D1 — none.** `packages/snapcompact/src/snapcompact.ts:41-46` claims "The whole pass is local and deterministic — no LLM call, no API key". **Verified accurate** against `compact()` (`:2038-2187`) and `render_snapcompact_png` (`crates/pi-natives/src/snapcompact.rs:1203`).

**D2 — disclosure gap.** `compaction-methods.ts:27-30` advertises snapcompact as "no LLM call", but nothing in the user-facing setting text for `compaction.methodOrder` (`settings-schema.ts:2674-2677`) discloses that `remote` is ordered **first** by default. Not a code/doc contradiction; a framing gap that understates how often full history egresses.

**D3 — candid, and it settles §2.** `compaction/anthropic.ts:5-7` states the compaction request is "the live turn's own request shape — same system prompt, tools, and message history". Consistent with `session-maintenance.ts:1097-1100`. Quoted because it answers the raw-history-egress question unambiguously in the code's own words.

**D4 — footgun.** `settings-schema.ts:3238` describes `mnemopi.embeddingVariant` as "Local embedding model family", but `embeddings.ts:396-400` `getLocalModel()` returns `null` whenever `isApiModel(defaultModel())` is true, and `isApiModel` (`:328-341`) returns true for **any configured non-OpenRouter `apiUrl`**. Setting `mnemopi.embeddingApiUrl` silently converts "local embeddings" into remote HTTP embedding calls, with no warning in the setting text.

**D5 — stale constant.** `packages/mnemopi/src/core/local-llm.ts:27-30` still defines GGUF defaults (`TheBloke/TinyLlama-1.1B-Chat-v1.0-GGUF`, `MNEMOPI_LLM_REPO` / `MNEMOPI_LLM_FILE`), implying an in-process GGUF loader. No llama.cpp binding is imported anywhere in `packages/mnemopi`; the live paths are `callHostLlm` / `completeSimple` (`local-llm.ts:1-19`). Flagged, not fixed (read-only audit).

---

## 10. Bottom line for the parent audit

**Genuinely local, ranked by bytes prevented from leaving:**
1. `read` line/byte budgeting + tree-sitter structural summary (§1.1, §1.2) — biggest win, hottest path, **on by default**.
2. Bash/grep output caps with local artifact spill (§5.1–5.3).
3. Supersede / useless / age pruning (§3) — **on by default**.
4. `snapcompact` + `shake` (§2.5–2.7) — real local compaction, but **ranked below `remote`**.

**Not local:**
- Compaction on a default install: `remote` first → **raw history to Anthropic / OpenAI / OpenAI-Codex / configured endpoint**.
- `soft` and `handoff` fallbacks: also remote.
- Every classifier and the title generator: default `online`.

**Built but dormant:** the entire `tiny/` on-device stack (ONNX + MLX + CoreML enum) and mnemopi's fastembed embeddings. Reordering `compaction.methodOrder` to `["snapcompact","shake","handoff","remote","soft"]` and pointing `providers.tinyModel` / `providers.autoThinkingModel` / `providers.unexpectedStopModel` at local keys would move a substantial fraction of the §8 table from REMOTE-LLM to LOCAL — **without building a single new subsystem.**
