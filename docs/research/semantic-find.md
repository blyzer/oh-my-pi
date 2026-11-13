# Gap 3: semantic `find` on `omp-kb` and the judge role

Date: 2026-11-13. Status: research, not a decision record (the decision is [ADR 0060](../adr/0060-semantic-find.md)). Slug: `semantic-find`.

Evidence labels: **[code-v1]** read in OMPv1, can1357/oh-my-pi `c308970263` (v18.20.0), under `packages/coding-agent/src/` unless noted. **[code]** read in omp2 at `f4186b783b`. **[doc]** read in a repository document. **[derived]** computed from cited constants, not measured. **[unverified]** not read or not run. All upstream and omp2 line citations were re-checked against both trees on 2026-11-13 after the first copies of this note were lost.

## 1. Upstream

**What it is.** [code-v1] `tools/jfind/index.ts:68-69` defines `FindTool` with `name = "find"`: "semantic grep". Parameters are `query` (a behaviour description), `grep_keywords` (string array) and an optional `path`. It returns files and line ranges, at most 3 ranges per hit shown. It is not embedding-based: nothing is indexed or persisted, and every call walks the tree and asks a judge questions. A user needs it to ask "where is X implemented" in a large tree without guessing the identifiers `grep` requires.

**The cascade** (header of `tools/jfind/cascade.ts`):
1. Lexical prior. One native grep pass over the model's `grep_keywords` plus keywords derived from the query gives per-file counts; `tools/jfind/lexical.ts` turns them into IDF-weighted scores. No model calls.
2. Filename ranking. The top `CANDIDATES = 128` files are judged by name and tree position in batches of `NAME_BATCH = 64`. An optional "Spotlight" ranker can score them all in one request.
3. Passage sketching. The strongest `FILES = 20` files are read, cut into up to `WINDOWS = 24` windows of `WINDOW_BYTES = 8192`, and each window is judged from a `SKETCH_BYTES = 384` sketch. Sketches only route; below `CUTOFF = 0.45` a passage is dropped.
4. Verification. Up to `FULL_LIMIT = 40` complete passages are judged. A file is a hit at probability `THRESHOLD = 0.2` or above unless `find.hitThreshold` overrides it. `find.verifyModel` can send this phase to another model.

Limits [code-v1]: `FIND_TIMEOUT_MS = 20_000` (`index.ts:53`), `PARALLEL = 16` requests in flight, `SCAN_TIMEOUT_MS = 30_000` for the lexical scan. On timeout it returns the hits verified so far, or a `ToolError` telling the model to narrow `path` or use grep. Cost per full-repository call [derived]: filename ranking at most 2 requests; sketching at least 11 (480 cards at about 46 per request given `SKETCH_STATE_BYTES = 18_000`); verification roughly 14 to 27 (up to 40 passages at `VERIFY_STATE_BYTES = 24 * 1024`). Expect 25 to 40 judge requests.

**The pure parts, read in full.**
- `keywords.ts`: quoted phrases of at least 3 bytes stay whole; the rest is split on non-letter, non-digit, non-underscore characters; tokens under 3 bytes, stopwords and pure digits drop; a suffix stemmer strips `ing`, `ed`, `es` or `s` when at least 4 characters remain. The model's `grep_keywords` are appended lowercased and deduplicated.
- `passages.ts`: whole-line windows of at most `WINDOW_BYTES`, each line tagged `L<n>| `. A window scores `sum(weight_k * ln(1 + occurrences_k))`. `selectWindows` keeps the best `WINDOWS`; if every window scores zero it spreads evenly through the file instead of taking the opening bytes. `sketch` is a budgeted set of verbatim lines, ranked by keyword weight plus 0.1 for lines containing `(`, clipped to 180 bytes, emitted in file order; never a model summary. `mergeHeat` unions judged-positive spans at or above the threshold, never bridging an unjudged gap, and keeps the maximum probability.
- `text.ts`: budgets are UTF-8 bytes "(what the judge is billed on)", clipped on code-point boundaries. `readText` reads at most `maxBytes`, treats a NUL in the first 8 KB as binary, trims a truncated read to the last full line, and rejects blank files.
- `tree.ts` (header): the cascade works from one flat list of eligible files built up front, with deny-lists for build noise, lockfiles, binaries and "obvious credential material"; the scope may be a directory, one file or an internal URL; a `:N-M` selector is rejected.

**How v1 uses the judge role.** [code-v1] `index.ts:118` calls `resolveJudge` with purpose `"find"`, the session usage journal, telemetry, a shared answer cache and the session cost gate. Questions are "noul" questions returning absolute probabilities (`tools/jfind/questions.ts` header), so entries are thresholded independently. `judgment/index.ts:290` (`textJudgeRoleChain`): from the first native candidate on, only native candidates remain, because "a prompted model never stands in for a failed native judgment, whose calibrated probabilities it cannot reproduce". `kindOf` (`judgment/index.ts:265`) classifies candidates native, local or online by model API. `isFindEnabled` (`index.ts:61`) with the default `find.enabled: auto` (`tools/settings.ts:564`) enables the tool only when `hasNativeJudge` (`judgment/index.ts:392`) is true; `on` overrides, with a warning that other models calibrate differently and may need a higher `find.hitThreshold`.

Spotlight [code-v1]: a hosted Stencil service (`DEFAULT_SPOTLIGHT_URL = https://cloud.stencil.so/v1/spotlight`, `spotlight/types.ts:20`) under `spotlight.experiment.*` settings.

## 2. omp2 inventory

**`glob`, not `find`.** [code] `crates/tools/src/glob.rs` is "Workspace path matching with mtime-ranked grouped output" (`glob@1`, limit 200, 5 s timeout). `crates/app/src/cli.rs:284` maps the tool name `find` to `glob`. A semantic tool cannot take the name `find`.

**`omp-kb` is a symbol graph.** [code] `crates/kb/Cargo.toml`: "Revision-aware derived code knowledge base from SCIP producers and tree-sitter". `crates/kb/src/graph.rs`: `Symbol { name, definitions, references, resolution }` with path, line and column locations, plus call and implementation edges. `Index::query` (`graph.rs:583`) keeps a symbol only when `symbol.name == needle` or its id equals `needle`: exact match, no substring, no text.
- No file content, doc comments or embeddings.
- Freshness works: `Store::stale` (`store.rs:137`), `build` (168), `update` (263), `refresh` (301); the cache lives in the worktree's Git directory (`crates/kb/README.md`).
- Coverage needs external SCIP producers (`crates/kb/src/producer.rs`: Rust, TypeScript, Python argument builders; `Run::Absent` when none is installed) with a tree-sitter fallback (`Resolution::Syntactic`, `graph/syntax.rs`).
- Its only consumer is the CLI, `crates/app/src/kb_cmd.rs` (`omp kb update|query|refresh`). No tool depends on it.
- `docs/knowledge-base/README.md` is a different artefact (per-file rust-analyzer outlines); its own limits section says Tier 1 is not type-aware and collisions return multi-hit lists.
- Dependency feasibility [code], `cargo metadata --offline --no-deps`: `omp-kb` depends on `omp-ast` and `omp-core` only; `omp-tools` already depends on both; `omp-kb` does not reach `omp-tools`, `omp-ai` or `omp-agent`. Adding the edge creates no cycle.

**The judge role.** [code] `crates/catalog/src/selection.rs:177` lists `judge` among role ids; `selection.rs:465` resolves `"slow" | "plan" | "advisor" | "judge"` to `find_slow`, so `@judge` is a chat model, the same as `@slow`. Its consumer is the ADW review phase (`crates/driver/src/adw/production.rs:322-328`). ADR 0053 D3 defines the role and its status section says the judge role is Not started [doc].

**No typed probability judgment.** [code] `crates/ai/src/answer.rs` has typed unary and streaming answers and `crates/ai/src/client.rs:70` an `Embed` operation, but nothing named noul, judgment or judge_batch exists in `crates/ai` (the `judgment` hits in `crates/tool/src/pins.rs` and `crates/agent/src/dispatch.rs` are the tool-pin policy check). `crates/ai/src/call.rs:1039` carries `top_logprobs: Option<u8>`, but only the OpenAI chat codec puts it on the wire (`codec/openai_chat.rs:627-628`). Anthropic, Bedrock, Ollama, GitLab and Cursor reject it (for example `codec/anthropic.rs:1364`); Gemini and OpenAI Responses drop it (`codec/gemini.rs:1222`, `codec/openai_responses.rs:3224`). No response-side decoding of log-probabilities exists in `crates/ai/src` outside the router's `chat.logprobs` capability check (`router.rs:877`). A log-probability judge is therefore not available today.

**How a tool reaches a model, and what a new tool costs.**
- Host capabilities are injected: `SubagentSpawner` (`crates/tools/src/task.rs:321`). Tools declare effects; `InferenceEffects { max_requests, max_usd }` (`crates/tool/src/lib.rs:453`) is the declared inference ceiling (used by `memory.rs` and `eval.rs`).
- Building blocks: `omp_walker` (already a tools dependency, `crates/tools/Cargo.toml:57`; hidden and gitignore options, `node_modules` skip at `crates/walker/src/cache.rs:205`, scan cache at `cache.rs:56`; no lockfile, binary or credential filter found). `omp_grep` (`crates/grep/src/lib.rs`): ripgrep engine with PCRE2 fallback, `CompiledGrep` (368) and streaming `grep_stream` (779) with a `GrepSink` that can produce per-file, per-keyword counts.
- ADR 0024 keeps the permanent roster small: a tool takes a wire slot only if used on most turns, and rarely used capabilities "NEVER enter the permanent roster" but ride `dyn` (ADR 0025). The roster table already supports this: `PresentationClass::Device` is "Always a catalog device" and `ClaimsClass::BuiltinDevice` gives enhancement precedence (`crates/tools/src/roster.rs:74-98`); `security_scan` is such a row (`roster.rs` near line 566). So semantic search can be a Rust builtin device behind `dyn` with no wire schema slot.
- The roster has a Composer phase for driver-owned tools (`roster.rs:887`: `Family::Task | Family::Hub`), with an inventory test asserting exactly `["task", "hub"]` (`roster.rs:1868`).

**Searches that found no omp2 equivalent.** Semantic search, ratchet-like judge-ranked retrieval and a `Judge` trait: none exist.

## 3. Class

**d (propose), narrowly.** omp2 has a path matcher (`glob`), an exact symbol index (`omp-kb`) and a chat-backed `@judge` alias, and none of them answers "which files implement X" from a description. Not class c, because the v1 tool depends on a native probability judge omp2 lacks and a straight port would ship permanently off; the proposal sequences the pure ports first and the judge prerequisite second.

## 4. Options

| Option | What it is | Fit | Cost and main risk |
|---|---|---|---|
| A. Port the v1 cascade as a builtin `dyn` device | Rust, stateless, `Judge` injected like `SubagentSpawner`, no wire slot | Reproduces the behaviour; matches ADR 0024 and 0025; no index to keep fresh | 25 to 40 judge requests per full call; needs a typed judgment API |
| B. Build on `omp-kb` | KB queries as candidate generator | Reuses freshness | Exact symbol names only; cannot match prose; needs installed producers |
| C. Embedding index | `omp-ai` `Embed` or the `omp-memory` embedding worker plus a vector store | Cheap per query | New persistent index, freshness, model and storage choices: larger than the gap |
| D. Python extension device | An `@omp.tool` soft device (ADR 0036) | No core change | No judge capability for extensions [unverified]; file content and a hot loop through the bridge; v1 is TypeScript and ADR 0035 wants engine logic in Rust |
| E. New wire-slot tool `locate` | Permanent roster tool | Simplest to discover | Violates ADR 0024 rules 3 and 5 (not used on most turns; per-turn cost unproven) |
| F. Do nothing | `grep`, `glob`, `omp kb query`, a read-only `task` subagent | Zero cost | Leaves the capability absent; the subagent has no request budget or per-call cost record |

**Choice: A.** B fails on the data model, C is a separate project, D lacks the capability, E is barred by ADR 0024. A is the only option that reproduces v1 within the locked ADRs. `omp-kb` may supply an optional lexical prior later (slice 6), never retrieval.

## 5. Plan

All slices ship behind `sv_locate_enabled` (bool, default false), except the last, which also adds the feature `cap-kb`. Detailed acceptance tests are in ADR 0060.

0. Pure ports (keywords, byte clipping, windowing, window selection, sketching, heat merging): deterministic string functions with unit tests; no gate needed because nothing reaches them.
1. Lexical prior and eligibility filter on `omp_grep` and `omp_walker` (lockfile, binary, credential deny-list; NUL probe).
2. The cascade against a recording fake judge: budgets, deadline, partial results, failure reporting.
3. Typed judgment in `omp-ai` and the driver `Judge` on the `@judge` route, flagged `calibrated: false`.
4. The builtin device `locate@1` (`PresentationClass::Device`, `ClaimsClass::BuiltinDevice`), behind `sv_locate_enabled`; the Composer inventory test changes.
5. Measurement gate on a fixed query set against `grep`; set the hit-threshold default.
6. Optional `omp-kb` prior behind `cap-kb` and `sv_locate_kb_prior`.

## 6. Open questions

- The device name (`locate`) and whether the `find` → `glob` alias stays. Recommendation: name `locate`, keep the alias (names are unchangeable).
- Which model routes can supply calibrated probabilities: effectively none today (section 2). Recommendation: ship prompted-chat, flagged uncalibrated, default off.
- Is selecting `@judge` consent for sending file content to that provider? Recommendation: yes; record the route in every result. Whether the approval path has an inference-egress consent that `InferenceEffects` feeds was not traced.
- Whether `Present::Device` rows can use the Composer phase [unverified]; if not, the driver needs another injection point.
- Conflict check: no hard constraint or locked ADR is violated by option A. ADR 0053's "Not started" status for `@judge` is stale relative to the selector in code; fixing it is a separate change.

## 7. Upstream origin and license

Files a port draws from (can1357/oh-my-pi `c308970263`): `tools/jfind/{index,cascade,lexical,keywords,passages,questions,text,tree}.ts`, `judgment/index.ts`, `prompts/tools/find.md`, `prompts/judge/find.json` (rubric rewritten, not copied). Spotlight is not ported. License: MIT (upstream `LICENSE`: © 2025 Mario Zechner, © 2025-2026 Can Bölük, © 2026 Stencil Labs, Inc.). omp2's `LICENSE` is MIT, © 2026 Stencil Labs, Inc. only. Ported files keep the upstream notice in their headers; whether the root `LICENSE` should list the upstream lines is an owner question (ADR 0060 O5).
