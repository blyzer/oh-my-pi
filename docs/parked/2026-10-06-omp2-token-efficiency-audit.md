# omp2 token-efficiency audit vs Claude Code (parked)

Status: **parked report, nothing here is approved or implemented.** Resume after the
current "default behaviour" tuning of omp2 is done. Not authoritative: `PLAN.md` and
code outrank it. Recorded 2026-10-06 from a read-only audit of `origin/omp2` plus
Anthropic and Claude Code (CC) docs.

Labels: VERIFIED = read in code or docs; INFERRED = reasoned or estimated (chars/4).
Not built or run: no binary, and live API behaviour for >4 markers or no markers was
not exercised. `docs/audits/tool-roster-stability.md` predates the latched roster and
is partly stale.

## Findings

| # | Finding | Evidence |
|---|---|---|
| F1 | `ai_cache_retention auto` maps to `Setting::Unset`; the Anthropic codec then emits **no** `cache_control`, so a default session is probably uncached. `short`/`long` stamp every block and tool def; the API allows 4 breakpoints. | `crates/ai/src/settings.rs:560-566`; `crates/ai/src/codec/anthropic.rs:1363,1408,1531-1576,1696-1702` |
| F2 | Branch `claude/anthropic-cache-breakpoints` caps markers at 4, but `auto` still places none (its own comment). | worktree diff, `budget_cache_breakpoints` |
| F3 | Volatile band (`turn: N`, todos, date) is a system block before the messages, so every user turn and todo update rewrites the message prefix. CC puts dynamic data in messages. | `crates/agent/src/loop.rs:2386-2409`; `prompts/system/status.md`; `prompt/projection.rs:41-44`; `p5_prefix_stability.rs` proves band-hash independence only |
| F4 | Compaction request puts its own instruction before the live system prompt and sends no tools, so it never hits the parent's cache. CC reuses the parent system/tools/history. | `crates/agent/src/directors/compaction.rs:706-750` |
| F5 | No stale tool-result clearing; each inline result up to 50 KiB (~12.5k tokens) is replayed until compaction. Only `clear_thinking keep:all` is sent. | `crates/session/src/projection.rs:302-312`; `anthropic.rs:496-503,1459-1466` |
| F6 | Fixed prefix ~9-12k tokens; ~20k in this repo because AGENTS.md (43.6 KB, ~11k tokens) is injected verbatim with no cap. Tool schema tax 273 B per tool (`i`, `notrunc`). | `prompts/system/project.md`; `crates/tool/src/lib.rs:94-125` |
| F7 | Default effort `high`; subagent `max_effort` = Max, no ceiling. CC defaults to `medium` on Opus 5.5 / Sonnet 5.5. | `catalog/src/settings.rs:403`; `agent/src/vars.rs:161`; `driver/src/subagent/settings.rs:421-426` |
| F8 | `/stats` cache rate omits `cache_write` from the denominator. | `crates/chat/src/overlays/stats.rs:44-48,106-108` |
| F9 | Setting text promises "bounded keep-alive refreshes while idle"; no implementation found. | `crates/catalog/src/settings.rs:1304-1306` |

Already better than CC: latched roster with long tail behind a `dyn` device, banded
prompt with facts journaled once, compaction at ~68% of window (CC at the boundary),
spill gate with `artifact://`, isolated subagents, per-inference receipts.

## Ranked savings (candidates, none approved)

1. Make Anthropic caching happen by default and enforce the 4-breakpoint budget
   (merge the breakpoint branch; `Auto` places markers or uses top-level automatic
   `cache_control` plus one explicit marker). ~10x prefix cost drop in tool loops (INFERRED).
2. Move the Volatile band out of the system prefix (keep only `date`); deliver `turn`,
   todos, directors as a trailing reminder on the latest user message or tool result.
   Update goldens; extend `p5_prefix_stability.rs` to message-prefix stability.
3. Make the compaction request cache-compatible (same system and tools, instruction
   as the last user message, markers).
4. Stale tool-result clearing: `ContextEdit::ClearToolUses20250919` (trigger 60-80k,
   keep 5, clear_at_least ~20k, exclude edit/todo/yield); stub to `artifact://` on
   other routes. Each clear invalidates from the first cleared block, so batch it.
5. Lower default reasoning (`medium`) and subagent `max_effort` default; set at session
   start only (effort changes can invalidate cache).
6. Trim the fixed prefix (cap and dedupe context files, shorten `notrunc`, shrink edit
   and read descriptions).
7. Subagent fan-out: stagger the first child so siblings read its prefix; low default
   child effort; cheaper child model; lower `soft_request_budget` (200).
8. Spill gate inline cap 50 KiB -> 16-20 KiB with read-range hint; TTL by context
   (`1h` when idle gaps >5 min); implement or delete the stale keep-alive text.

## Measure first (existing telemetry, no new infra)

Data: `turn.receipt@1` per inference (`tokens_in` uncached, `tokens_out`, `cache_read`,
`cache_write`, cost, ttft); `prompt_hash` and `prompt_head_tokens` in `ContextFacts`.

1. Scripted 10-turn Anthropic session on defaults: expect `cache_read == 0 &&
   cache_write == 0` if F1 holds. Repeat with `ai_cache_retention short`; record any 400.
2. Per-request hit = `cache_read / (tokens_in + cache_read + cache_write)`; rewrite =
   `cache_write / (same)`; mark user-turn boundaries.
3. Classify large `cache_write` with small `cache_read`: first request of a turn (F3),
   after a todo update, model/effort switch, >5 min idle, after compaction.
4. Fixed prefix exactly via `count_tokens` lowering (`anthropic.rs:1180`), per agent class.
5. Group tool-result bytes by tool (p50/p95, share of final-request tokens) to set
   trigger/keep and the gate size.
6. `tokens_out` vs `reasoning_tokens` at `high` vs `medium` on the same 5 prompts;
   child totals via `ChildResult.tokens_in/out`.
7. Compaction receipts: expect `cache_read ~ 0` now, `~ context size` after the fix.
8. Targets: >=85-90% of input from cache on tool-loop turns; rewrites only at
   compaction or model change.

Sources: platform.claude.com/docs/en/build-with-claude/{prompt-caching,context-editing,
compaction}, .../agents-and-tools/tool-use/{tool-search-tool,memory-tool};
code.claude.com/docs/en/{costs,prompt-caching,model-config,context-window};
claude.dev/blog/lessons-from-building-claude-code-prompt-caching-is-everything/.
