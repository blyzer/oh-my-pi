# Audit 02 — Tool Results: Local Execution → Next LLM Request

Scope: `/Users/enverfrancisco/repositories/oh-my-pi`. Read-only. Every claim below is
`OBSERVED` (code read at the cited line) unless marked `INFERRED` or `MISSING`.

**Governing distinction maintained throughout:** a limit *reduces what is SENT* only if it
rewrites the `content` blocks of the `ToolResultMessage` that the provider converter reads.
Limits that shape a TUI component, a renderer, or a progress line are *DISPLAY* only.

---

## 1. The runtime path (one sentence per hop)

```
tool.execute()                      packages/agent/src/agent-loop.ts:2733
  -> wrappedExecute()               packages/coding-agent/src/tools/output-meta.ts:~895
       -> original tool body        (per-tool caps: read/grep/bash/fetch/...)
       -> spillLargeResultToArtifact()   output-meta.ts:724-911   <-- UNIVERSAL CAP
       -> appendOutputNotice()           output-meta.ts:639-657
  -> coerceToolResult()             agent-loop.ts:447-519  (sanitizeText, shape coercion)
  -> ToolResultMessage{role:"toolResult", content}   agent-loop.ts:2601-2606
  -> currentContext.messages.push(result)            agent-loop.ts:1114 / 1451
  -> convertToLlm -> provider converter              e.g. anthropic.ts:4640-4641
  -> HTTPS request
```

Every built-in, MCP, extension, SDK-custom, RPC-host and image-gen tool is wrapped:
`tools/index.ts:707,736,777,786`; `session/session-tools.ts:570,1653,1731`;
`sdk.ts:2868,2892,2957,2982,3694,3859`. `wrapToolWithMetaNotice` is idempotent
(`output-meta.ts:934-936`). **Confidence: HIGH.**

---

## 2. What actually happens to a tool result

| Transformation | Present? | Evidence |
| --- | --- | --- |
| **Verbatim** | Yes, below threshold | `output-meta.ts:759` — `if (totalBytes <= threshold) return result;` |
| **Truncated** | Yes | `output-meta.ts:779-789` (`truncateMiddle`/`truncateTail`); `streaming-output.ts:662-679` (`enforceInlineByteCap`) |
| **Summarized** | Yes — lossy | Rust minimizer `crates/pi-shell/src/minimizer/engine.rs:88-125`; code-outline `tools/read-summary.ts:67-78`; profile renderers `tools/read.ts:1539-1542` |
| **Filtered** | Yes | Minimizer per-program filters drop progress/noise lines, e.g. `crates/pi-shell/src/minimizer/filters/git.rs:44-91`, `bun.rs:298-319` |
| **Ranked** | No | `MISSING` — no relevance ordering of tool output. `grep` orders by file discovery order and applies flat caps (`tools/grep.ts:1378-1381`), not scoring |
| **Chunked** | No | `MISSING` — a single result is never split across messages. Pagination exists but is agent-driven (`read` offset/limit, `grep` skip) |
| **Compressed** | No | `MISSING` — no local compression of the payload before the request |
| **Deduplicated** | Yes, two places | Post-hoc supersede pruning `agent/src/compaction/pruning.ts:183-241,433` (`readToolSupersedeKey`); log-line dedup inside the minimizer `crates/pi-shell/src/minimizer/filters/docker.rs:693-717` |
| **Relevance-selected** | No | `MISSING` — no embedding/BM25 selection of tool output before sending |
| **Redacted** | Yes | `sanitizeText` on every content block `agent-loop.ts:484`; `sanitizeWithOptionalSixelPassthrough` in `OutputSink.push` `streaming-output.ts:~899`; credential obfuscator `coding-agent/src/secrets/obfuscator.ts` (audited separately) |
| **Cached** | Yes | Artifacts on disk `session/artifacts.ts:75-188`; GitHub cache `tools/github-cache.ts`; image-resize memo `ai/src/providers/anthropic.ts:1037-1040` |

**Confidence: HIGH.**

---

## 3. Enforced limits — every constant that fires at runtime

`Bounds outbound?` = YES only when the limit rewrites the text/image blocks that become the
`ToolResultMessage.content` sent to the provider.

### 3.1 The universal tool-result cap (applies to EVERY tool)

| Constant | Value | File:line (definition) | Enforced at | Bounds outbound? |
| --- | --- | --- | --- | --- |
| `tools.artifactSpillThreshold` | `50` KB -> 51200 B | `config/settings-schema.ts:876-878` | `tools/output-meta.ts:672,759` | **YES** |
| `tools.artifactHeadBytes` | `20` KB | `config/settings-schema.ts:920-922` | `tools/output-meta.ts:675,779-786` | **YES** |
| `tools.artifactTailBytes` | `20` KB | `config/settings-schema.ts:900-902` | `tools/output-meta.ts:673,782-790` | **YES** |
| `tools.artifactTailLines` | `500` | `config/settings-schema.ts:962-964` | `tools/output-meta.ts:674,783-788` | **YES** |
| `INLINE_CAP_SLACK_BYTES` | `2 * 1024` | `tools/output-meta.ts:698` | `tools/output-meta.ts:704-706` | **YES** |
| `enforceInlineByteCap` head/tail split | `0.6` / `0.25` of budget | `session/streaming-output.ts:670-671` | `tools/bash.ts:839,851`; `tools/browser.ts:400`; `tools/computer.ts:251` | **YES** |

Net effect for text: **any tool result over 50 KB is cut to ~20 KB head + ~20 KB tail with a
`[...NB elided...]` marker and an `artifact://<id>` pointer.** The full bytes go to disk, not to
the provider.

### 3.2 Shared streaming/truncation primitives

| Constant | Value | File:line | Enforced at | Bounds outbound? |
| --- | --- | --- | --- | --- |
| `DEFAULT_MAX_LINES` | `3000` | `session/streaming-output.ts:9` | `truncateHead:300`, `truncateTail:402`; `read.ts:736,1144,1773,2257` | **YES** |
| `DEFAULT_MAX_BYTES` | `50 * 1024` | `session/streaming-output.ts:10` | `truncateHead:301`; `OutputSink:837`; `enforceInlineByteCap:663`; `read.ts:1777,2259` | **YES** |
| `DEFAULT_MAX_COLUMN` | `512` | `session/streaming-output.ts:11` | `tools/grep.ts:508,552,566,696,1207,1254` | **YES** |
| `tools.outputMaxColumns` | `768` | `config/settings-schema.ts:942-944` | `OutputSink.#applyColumnCap` `streaming-output.ts:964-1012` | **YES** |
| `OutputSink.#headLimit` | `min(headBytes, spillThreshold/2)` | `session/streaming-output.ts:848` | `streaming-output.ts:938-954` | **YES** |
| `OutputSink.#spillThreshold` | default `DEFAULT_MAX_BYTES` | `session/streaming-output.ts:837` | `#willOverflow:1022`, `#pushTail:1026-1052` | **YES** |
| `ARTIFACT_DEFAULT_MAX_BYTES` | `0` (= unbounded) | `session/streaming-output.ts:19` | `streaming-output.ts:842,852,1099-1101` | **NO** (disk file only; `0` means the artifact is lossless and may be 50 MB) |
| `ARTIFACT_DEFAULT_HEAD_BYTES` | `3 * 1024 * 1024` | `session/streaming-output.ts:21` | `streaming-output.ts:843,853` | **NO** — inert while `artifactMaxBytes === 0` |

### 3.3 Per-tool limits — large-output tools

| Tool | Constant | Value | File:line | Bounds outbound? |
| --- | --- | --- | --- | --- |
| bash | inline cap budget | `spillThreshold + 2 KB` = 52 KB | `tools/bash.ts:825-827` | **YES** |
| bash | artifact allocation (disk) | unbounded | `tools/bash.ts:923,1507` | NO (disk) |
| bash (async job) | `TailBuffer(DEFAULT_MAX_BYTES)` | 50 KB | `tools/bash.ts:924` | **YES** |
| grep | `DEFAULT_FILE_LIMIT` | `20` | `tools/grep.ts:101` | **YES** |
| grep | `MULTI_FILE_PER_FILE_MATCHES` | `20` | `tools/grep.ts:104` | **YES** |
| grep | `INTERNAL_TOTAL_CAP` | `2000` | `tools/grep.ts:112` | **YES** |
| grep | `NATIVE_GREP_MAX_FILE_BYTES` | `4 * 1024 * 1024` | `tools/grep.ts:118` | Indirect — bounds *search coverage* (`grep.ts:405,1429`), not the output text |
| read | `read.defaultLimit` | `300` lines | `config/settings-schema.ts:3798-3800`; clamped `read.ts:734-737` | **YES** |
| read | `maxBytesForRead` | `max(50 KB, lines * 512)` | `tools/read.ts:1152,1777,2259` | **YES** (ceiling 3000*512 = 1.5 MB, then re-cut by 3.1) |
| read | `MAX_ARTIFACT_RAW_INLINE_BYTES` | `DEFAULT_MAX_BYTES` (50 KB) | `tools/read.ts:149`; enforced `:2204,2234,2383` | **YES** |
| read | `SNAPSHOT_MAX_BYTES` | `4 * 1024 * 1024` | `tools/read.ts:150` | NO — buffer-vs-stream strategy only |
| read | `MAX_PROFILE_SUMMARY_BYTES` | `32 * 1024 * 1024` | `tools/read.ts:148`; gate `:1539` | Indirect — gates summarization |
| read | `MAX_IMAGE_SIZE` = `MAX_IMAGE_INPUT_BYTES` | `20 * 1024 * 1024` | `utils/image-loading.ts:17`; `read.ts:618,924,1019` | **YES — hard reject (throws), not truncation** |
| read (dir) | `READ_DIRECTORY_MAX_DEPTH` / `_CHILD_LIMIT` | `2` / `12` | `tools/read.ts:2539-2540` | **YES** |
| read-summary | `MAX_SUMMARY_BYTES` / `MAX_SUMMARY_LINES` | `2 MiB` / `20000` | `tools/read-summary.ts:37-38`; gate `:68,77` | Indirect — above these, no outline is produced |
| fetch | `FETCH_DEFAULT_MAX_LINES` | `300` | `tools/fetch.ts:43` | **YES** |
| fetch | `MAX_INLINE_IMAGE_SOURCE_BYTES` | `20 * 1024 * 1024` | `tools/fetch.ts:84`; enforced `:1164` | **YES** |
| fetch | `MAX_INLINE_IMAGE_OUTPUT_BYTES` | `300 * 1024` | `tools/fetch.ts:85`; enforced `:1185,1205` | **YES** |
| fetch | `JINA_READER_MAX_BYTES` | `2 * 1024 * 1024` | `tools/fetch.ts:579`; enforced `:690` | **YES** |
| list-based tools | `applyListLimit` | caller-supplied | `tools/list-limit.ts:15-39` | **YES** |
| LSP diagnostics | glob match cap | caller-supplied `maxMatches` | `lsp/utils.ts:582-593` | **YES** (bounds file fan-out, not per-file text) |
| xd:// device docs | description cap | caller-supplied | `tools/xdev.ts:121-123,201-204` | **YES** |

**`git diff` / `git log` / test output / compiler errors have NO dedicated JS tool.** They run
through `bash`, so they are governed by (a) the Rust minimizer and (b) the bash inline cap.
See 3.4 and 3.5.

### 3.4 Rust natives — the minimizer (`crates/pi-shell/src/minimizer/`)

| Constant | Value | File:line | Bounds outbound? |
| --- | --- | --- | --- |
| `shellMinimizer.enabled` | `true` (default ON) | `config/settings-schema.ts:4079-4081`; gate `exec/bash-executor.ts:241-242,483` | **YES** |
| `DEFAULT_MAX_CAPTURE_BYTES` / `shellMinimizer.maxCaptureBytes` | `4 * 1024 * 1024` | `crates/pi-shell/src/minimizer/config.rs:19`; `settings-schema.ts:4095-4098` | **INVERTED** — above 4 MiB the engine returns `passthrough(...).labeled("too-large")` (`engine.rs:95-97`); capture also stops at `shell.rs:1789-1793` |
| `MIN_MINIMIZE_CHARS` | `1000` | `crates/pi-shell/src/minimizer/engine.rs:17` | Below this: no minimization |
| git `condense_log(&cleaned, 32, 16)` | 32 head / 16 tail entries | `crates/pi-shell/src/minimizer/filters/git.rs:64` | **YES** |
| git `DIFF_LISTING_LIMIT` | `20` | `filters/git.rs:166` | **YES** |
| git `LOG_LINE_WIDTH` | `160` | `filters/git.rs:572` | **YES** |
| git `WORKTREE_LIMIT` | `20` | `filters/git.rs:1502` | **YES** |
| binary tools `HEAD_LINES`/`TAIL_LINES` | `50` / `20` | `filters/binary_tools.rs:14-15` | **YES** |
| cloud `MAX_PSQL_ROWS` / `MAX_LINE_CHARS` | `30` / `500` | `filters/cloud.rs:9-10` | **YES** |
| docker `compact_log_lines(_, 120, 80, _)` | 120/80 lines | `filters/docker.rs:694-704` | **YES** |

**What the minimizer does:** it is a *lossy semantic summarizer*, not a truncator. Per-program
filters (`git`, `cargo`, `bun`, `docker`/`kubectl`/`helm`, `pytest`, `ctest`/`gtest`/`ninja`,
`aws`/`psql`/`curl`, `xxd`/`strings`/`od`, lint, package managers) parse the buffer and rebuild a
condensed one. Dispatch is `engine.rs:88-125`. Filter panics are caught and downgraded to
passthrough (`engine.rs:76-78`).

**It reduces what is SENT.** `exec/bash-executor.ts:738-756`:

```ts
const minimized = winner.result.minimized;
if (minimized && minimized.text !== minimized.originalText) {
  const artifactId = await options.onMinimizedSave(minimized.originalText, {...});
  if (artifactId) {
    sink.replace(minimizedText);                                  // :753
    sink.push(`${sep}[raw output: artifact://${artifactId}]\n`);  // :755
  }
}
```

`sink.replace()` (`streaming-output.ts:1211-1229`) makes the minimized text authoritative and
realigns every byte/line counter to it. The original goes to the `ArtifactManager`
(`session/bash-runner.ts:270`) — i.e. to **disk, never to the provider**. Critically, the swap
only happens when an artifact id came back; otherwise the raw stream is preserved, so a lossy
summary is never sent without its lossless original being addressable.

**Structural refusals (no reduction):** piped commands, compound commands, parse errors, and
chains all fall through to `passthrough` (`engine.rs:105-119`, `apply_chain:151-159`).
So `<cmd> | tee huge.log` or `a && b` gets **zero** minimization.

**Caveat — the 4 MiB inversion:** a 50 MB `git log` capture is *above* `max_capture_bytes`, so
the minimizer refuses (`engine.rs:95-97`) and the raw stream is what the sink sees. The bash
inline cap (3.1) is what stops it, not the minimizer.

### 3.5 The artifact spill mechanism (`artifact://`)

| Constant | Value | File:line | Bounds outbound? |
| --- | --- | --- | --- |
| `MAX_INLINE_ARTIFACT_BYTES` | `8 * 1024 * 1024` | `internal-urls/artifact-protocol.ts:18`; enforced `:118-122` | **YES** — refuses full resolution, forces `:N-M` selectors |
| `MAX_ARTIFACT_RAW_INLINE_BYTES` | `50 KB` | `tools/read.ts:149`; enforced `:2204` | **YES** — blocks `artifact://N:raw` for large artifacts |
| artifact file size | **unbounded by default** | `session/streaming-output.ts:19` (`ARTIFACT_DEFAULT_MAX_BYTES = 0`) | NO — disk only |

**What spill does:** `ArtifactManager` (`session/artifacts.ts:75-188`) writes
`<sessionDir>/<id>.<tool>.log` via a staged+verified atomic rename (`writeArtifact:44-62`) and
hands back a numeric id. Two independent producers:

1. **Streaming producers** (`bash`, `python`, `ssh`, `eval`): `OutputSink` mirrors the *raw*
   stream to the artifact file (`streaming-output.ts:~930`) while keeping only
   head + rolling tail in memory. `dump()` composes `head + marker + tail`
   (`streaming-output.ts:1339-1372`) — bounded by construction.
2. **Everything else**: `spillLargeResultToArtifact` (`output-meta.ts:724-911`) measures only
   **text** blocks (`:748-757`), saves the full text, and replaces the text blocks with the
   truncated view (`:791-799`).

**Spill reduces what is SENT.** It is not a display concern: the returned object at
`output-meta.ts:911` (`{...result, content: newContent, details: newDetails}`) *is* the object that
becomes `ToolResultMessage.content` at `agent-loop.ts:2605`. The full content reaches the
provider **only if the agent explicitly re-reads it**, and even then bounded by the read tool's
own window plus the 8 MiB / 50 KB artifact caps above.

**Failure mode is fail-closed:** if `saveArtifact` throws, the result is *still* truncated and the
`artifact://` link is simply omitted (`output-meta.ts:768-776`). The full output is never
re-exposed to compensate for a failed save.

**One structural precondition:** the spill is a no-op when `context?.sessionManager` is absent
(`output-meta.ts:729-730`). In the live agent loop this never happens — `getToolContext` is wired
at `sdk.ts:3562` and `sdk.ts:4170`, and `ToolContextStore.getContext` always spreads
`sessionManager` (`tools/context.ts:32-38`, `session/session-tools.ts:250-252`). Non-persistent
sessions still work: `SessionManager.saveArtifact` falls back to an in-memory map
(`session/session-manager.ts:2128-2137`). **Confidence: HIGH.**

### 3.6 DISPLAY-only limits (do NOT reduce what is sent)

Listed to preserve the distinction.

| Constant | Value | File:line | Why display-only |
| --- | --- | --- | --- |
| `COLLAPSED_TEXT_LIMIT` / `EXPANDED_TEXT_LIMIT` | `PREVIEW_LIMITS.*_LINES * 2` | `tools/grep.ts:1657,1661` | Feeds `renderBudgetedSearchGroups` (`:1953`) — TUI only |
| `truncateToWidth(...)` | terminal width | `autoresearch/dashboard.ts` passim; `cli/bench-cli.ts:543` | Renderer |
| `truncateAsiValue` | `120` chars | `autoresearch/tools/log-experiment.ts:501-504` | Renderer |
| `displayTruncation` in run-experiment | `DEFAULT_MAX_BYTES` / `DEFAULT_MAX_LINES` | `autoresearch/tools/run-experiment.ts:152-155` | Explicitly the *display* twin of the LLM budget on the adjacent lines |
| `OutputSink.onChunk` preview | raw, pre-cap | `streaming-output.ts:~906` comment: "Live preview gets the raw (pre-cap) chunk" | Streams to TUI; the persisted/LLM view is the capped one |

**Contrast case worth citing** — `autoresearch/tools/run-experiment.ts:148-155` computes two
truncations from the same buffer: `EXPERIMENT_MAX_BYTES = 4 KB` / `EXPERIMENT_MAX_LINES = 10`
(`autoresearch/helpers.ts:6-7`) for the LLM, and `DEFAULT_MAX_BYTES` / `DEFAULT_MAX_LINES` for the
screen. That is the codebase making the SENT-vs-DISPLAYED distinction explicitly.

### 3.7 Post-hoc reduction (after the result is already in context)

| Constant | Value | File:line | Bounds outbound? |
| --- | --- | --- | --- |
| `TOOL_RESULT_MAX_CHARS` (compaction summaries) | `2000` | `agent/src/compaction/utils.ts:200-209` | **YES, but only inside a summarization prompt** |
| `TOOL_RESULT_MAX_CHARS` (snapcompact) | `2000` | `packages/snapcompact/src/snapcompact.ts:737` | **YES** — archive rendering |
| `TOOL_ARG_MAX_CHARS` | `500` | `snapcompact.ts:741` | **YES** |
| `TOOL_CALL_MAX_CHARS` | `2000` | `snapcompact.ts:744` | **YES** |
| `TRUNCATE_HEAD_RATIO` | `0.6` | `snapcompact.ts:748` | **YES** |
| `MIN_PRUNE_TOKENS` | `50` | `agent/src/compaction/pruning.ts:123` | **YES** |
| `SUPERSEDED_NOTICE` / `USELESS_NOTICE` | placeholder strings | `pruning.ts:67,70` | **YES** — `message.content` is overwritten (`pruning.ts:304,414`) and `prunedAt` stamped; `getPrunedToolResultContent` (`compaction/messages.ts:82-90`) then serves the pruned form |
| `REVIEW_DIFF_MAX_CHARS` (ADW) | `60_000` | `adw/runner.ts:778`; enforced `:824-825` | **YES** |
| `EXPERIMENT_MAX_BYTES` / `_LINES` | `4 KB` / `10` | `autoresearch/helpers.ts:6-7`; applied `run-experiment.ts:148-151` | **YES** |

### 3.8 Image / binary limits (the text caps do NOT apply)

| Constant | Value | File:line | Bounds outbound? |
| --- | --- | --- | --- |
| `images.autoResize` | `true` | `config/settings-schema.ts:1012-1014` | **YES** — gate for all resizing |
| resize `maxWidth`/`maxHeight` | `1568` | `utils/image-resize.ts:39-40` | **YES** |
| resize `maxBytes` | `500 * 1024` | `utils/image-resize.ts:27,41` | **YES** |
| resize `minDimension` | `200` | `utils/image-resize.ts:33,43` | Upscales tiny images |
| `MAX_IMAGE_INPUT_BYTES` | `20 * 1024 * 1024` | `utils/image-loading.ts:17`; enforced `:284,453,476,481` | **YES — throws** |
| `MAX_IMAGE_COUNT` (terminal graphics) | `32` | `utils/terminal-graphics.ts:8` | **YES** |
| `MAX_TOTAL_IMAGE_BYTES` | `40 * 1024 * 1024` | `utils/terminal-graphics.ts:11`; enforced `:261,276,291,303` | **YES** |
| `MAX_IMAGE_PIXELS` / `MAX_IMAGE_EDGE` | `4 Mpx` / `8192` | `utils/terminal-graphics.ts:9-10` | **YES** |
| `ANTHROPIC_MANY_IMAGE_THRESHOLD` | `20` | `ai/src/providers/anthropic.ts:933` | **YES — provider layer**, `:1069-1072,2458` |
| `ANTHROPIC_MANY_IMAGE_MAX_DIMENSION` | `2000` | `ai/src/providers/anthropic.ts:934` | **YES** |

---

## 4. Question (3) — Could a 50 MB local tool result become part of an outbound request?

### Definitive answer

**For text: NO.** It is stopped, and there are three independent stops. **For images: YES, up to
~20 MiB base64-expanded per image (~26.7 MB on the wire) — but only if the user disables
`images.autoResize`.** At default settings images are re-encoded to <= 500 KB.

### 4.1 Text — the trace, with the exact stopping line

**Path A — `bash` producing 50 MB (`git diff`, test output, compiler errors, `cat bigfile`):**

| Step | What happens | Bytes still live |
| --- | --- | --- |
| 1. Rust capture | `should_minimize` decides mode (`engine.rs:71-73`). At 50 MB the buffered path stops appending at `max_capture_bytes` (`shell.rs:1789-1793`), and `apply` returns `passthrough(...).labeled("too-large")` (`engine.rs:95-97`) | 50 MB streamed |
| 2. `OutputSink.push` | Raw stream mirrored to the artifact file on disk (`streaming-output.ts:~930`). In memory only `#head` (<= `min(20 KB, spillThreshold/2)`, `:848`) + rolling tail bounded by `spillThreshold - headBytes` (`#pushTail:1026-1052`) | **~50 KB in memory; 50 MB on disk** |
| 3. `sink.dump()` | Composes `head + [...NB elided...] + tail` (`:1339-1372`) | ~50 KB |
| 4. `enforceInlineByteCap` | `tools/bash.ts:851` with `maxBytes = 50 KB + 2 KB` (`:825-827`). No-op here (already under) | ~50 KB |
| 5. spill wrapper | `output-meta.ts:735` — `if (existingMeta?.truncation?.artifactId) return result;` Skipped, the sink already spilled | ~50 KB |
| 6. `coerceToolResult` | `sanitizeText` per block (`agent-loop.ts:484`) | ~50 KB |
| **Outbound** | | **~50 KB** |

**STOPPED at step 2** (`session/streaming-output.ts:1026-1052`, `OutputSink.#pushTail`), with step 4
(`tools/bash.ts:851`) as a second, independent backstop for any path that bypasses the sink.

**Path B — MCP / extension / SDK / RPC tool returning a 50 MB string:**

No `OutputSink`, no per-tool cap. The single stop is the universal wrapper:

```ts
const totalBytes = Buffer.byteLength(fullText, "utf-8");
if (totalBytes <= threshold) return result;          // output-meta.ts:758-759
artifactId = await sessionManager.saveArtifact(fullText, toolName);   // :771
const truncated = truncateMiddle(fullText, {         // :780-786
  maxBytes: headBytes + tailBytes,                   //   = 40 KB
  maxLines: tailLines * 2,                           //   = 1000
  maxHeadBytes: headBytes, maxHeadLines: tailLines,
});
newContent.push({ type: "text", text: truncated.content });   // :799
return { ...result, content: newContent, details: newDetails }; // :911
```

**STOPPED at `packages/coding-agent/src/tools/output-meta.ts:759` -> `:780-799`.** Outbound: ~40 KB
+ notice. (Also prunes the MCP `details.rawContent` duplicate at `:869-903` so the on-disk /
session record cannot re-inflate.)

**Path C — `read` of a 50 MB file:**

`maxBytesForRead = max(DEFAULT_MAX_BYTES, maxLinesToCollect * 512)` (`read.ts:1777`), with
`maxLinesToCollect <= DEFAULT_MAX_LINES = 3000` (`:1773`) -> hard ceiling **1.5 MB**. That 1.5 MB
then hits the 50 KB spill threshold and is cut to ~40 KB (`output-meta.ts:759,780`).
**STOPPED at `tools/read.ts:1773,1777`, then again at `output-meta.ts:759`.**

**Path D — re-reading the 50 MB artifact:** `artifact://N` full resolution is refused above 8 MiB
(`internal-urls/artifact-protocol.ts:118-122`); `artifact://N:raw` is refused above 50 KB
(`read.ts:2204`); windowed reads go through the same `maxBytesForRead` ceiling (`read.ts:2259`).
The spill wrapper deliberately does *not* re-spill artifact reads (`output-meta.ts:734-741`) — safe,
because the read tool already bounded the window. **STOPPED.**

**Conclusion for text: a 50 MB tool result cannot reach the provider. Worst case observed is
~52 KB per tool result.** Confidence: **HIGH**.

### 4.2 Images — the one path that is NOT stopped by the byte caps

The spill wrapper measures **only text blocks** and explicitly preserves everything else:

```ts
for (const block of result.content) {
  if (block.type === "text" && block.text) textParts.push(block.text);   // output-meta.ts:748-752
}
if (textParts.length === 0) return result;                              // :753
...
for (const block of result.content) {
  if (block.type !== "text") newContent.push(block);                     // :793-796  <-- images kept whole
}
```

So image blocks bypass `artifactSpillThreshold`, `artifactTailBytes`, `enforceInlineByteCap`, and
`DEFAULT_MAX_BYTES` entirely. What *does* bound them:

- `MAX_IMAGE_INPUT_BYTES = 20 MiB` — hard **throw**, not truncation (`utils/image-loading.ts:284,453`).
- `images.autoResize` default `true` (`settings-schema.ts:1012-1014`) -> `resizeImage` to 1568 px /
  500 KB (`utils/image-resize.ts:39-41`). **This is the only thing keeping typical image payloads small.**
- Turn `images.autoResize` off, or hit a resize failure (swallowed by `catch {}` at `read.ts:1029-1033`,
  `file-mentions.ts:243-250`), and a 20 MiB PNG rides out as ~26.7 MB of base64.
- `MAX_TOTAL_IMAGE_BYTES = 40 MiB` per bash capture (`utils/terminal-graphics.ts:11`) — a command
  emitting Kitty/Sixel graphics can queue up to 40 MiB, capped at 32 images (`:8`).
- Provider-side rescue: only >20 images triggers Anthropic's downscale-to-2000px pass
  (`ai/src/providers/anthropic.ts:933-934,1069-1089`). 19 large images get no provider-side reduction.

**So: a ~40 MB payload is reachable from a single `bash` call that emits terminal graphics, and a
~26.7 MB payload from a single `read` of a large image with auto-resize disabled.** Neither is
blocked by any of the text limits in section 3. Confidence: **HIGH** (all cited lines read).

### 4.3 Discrepancies found

1. **`packages/coding-agent/src/prompts/tools/bash.md:18`** tells the model "output is captured,
   truncated, and linked as `artifact://<id>`". Accurate for text. It does not mention that image
   blocks are exempt. Minor.
2. **`ARTIFACT_DEFAULT_MAX_BYTES = 0`** and the surrounding doc comment
   (`streaming-output.ts:13-21`) describe a head/tail artifact cap and a
   `[ARTIFACT TRUNCATED: ...]` notice (`:1273-1295`) that is **dead by default** — no call site
   passes `artifactMaxBytes`. Grep across `packages/` finds only the default. `INFERRED` from the
   absence of call sites; flagged rather than asserted.
3. **Minimizer inversion**: the setting is named `maxCaptureBytes` as if it were a cap, but
   exceeding it *disables* reduction (`engine.rs:95-97`). Larger output gets *less* processing,
   not more. The bash inline cap covers it, so no leak — but the naming inverts the intuition.

---

## 5. Summary of local reduction vs. local execution

| Claim | Verdict |
| --- | --- |
| Tools run locally | Yes |
| Tool output is reduced locally before the request | **Yes, substantially** — universal 50 KB threshold -> 40 KB head+tail, plus per-tool caps and a semantic Rust summarizer |
| The full output reaches the provider | **No**, for text. Full bytes go to disk (`artifact://`), reachable only by an explicit, itself-bounded re-read |
| Reduction is display-only | **No.** `spillLargeResultToArtifact` and `OutputSink.dump` rewrite the `content` array that becomes `ToolResultMessage.content` |
| Any unbounded outbound path | **Yes, one**: image/non-text content blocks, bounded only by 20 MiB/image and 40 MiB/bash-capture, and by a user-toggleable auto-resize |
