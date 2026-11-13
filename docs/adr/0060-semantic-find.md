# 0060. Semantic file search is a bounded builtin device over a judge, not an index and not a wire slot

Status: proposed
Date: 2026-11-13
Area: tools

## Context

- Research: [docs/research/semantic-find.md](../research/semantic-find.md) (Gap 3 of the upstream gap closure). This record states the decision; the note holds the evidence and the v1 reading notes.
- OMPv1 has a semantic grep tool named `find` [code-v1, can1357/oh-my-pi `c308970263`, v18.20.0]: `tools/jfind/index.ts:68-69` (`FindTool`, `name = "find"`). It takes `query`, `grep_keywords` and an optional `path` and returns files with line ranges. It is not an embedding index and persists nothing. It runs a cascade documented at the top of `tools/jfind/cascade.ts`: a lexical prior (one native grep pass, IDF-weighted file scores, `lexical.ts`); filename ranking of the top `CANDIDATES = 128` files in batches of `NAME_BATCH = 64`; passage sketching of the strongest `FILES = 20` files (`WINDOWS = 24` windows of `WINDOW_BYTES = 8192`, sketches of `SKETCH_BYTES = 384`, cutoff `CUTOFF = 0.45`); and verification of up to `FULL_LIMIT = 40` complete passages at hit threshold `THRESHOLD = 0.2`. The call has a budget `FIND_TIMEOUT_MS = 20_000` (`index.ts:53`) and returns the hits verified so far on timeout.
- v1 asks the `judge` model role (`index.ts:118`, `resolveJudge`, purpose `"find"`) and depends on calibrated probabilities. `judgment/index.ts:290` (`textJudgeRoleChain`) refuses to substitute a prompted model for a failed native judge; `kindOf` (`judgment/index.ts:265`) classifies candidates native, local or online; `isFindEnabled` (`index.ts:61`) with the default `find.enabled: auto` (`tools/settings.ts:564`) turns the tool off unless `hasNativeJudge` (`judgment/index.ts:392`) is true.
- omp2 has no semantic search. `crates/tools/src/glob.rs` is path matching (`glob@1`). `crates/app/src/cli.rs:284` maps the tool name `find` to `glob`, so `find` is taken; existing names are unchangeable (ADR 0026: durable identity is `name@rev`).
- `omp-kb` (`crates/kb`) is a symbol graph: `Symbol { name, definitions, references, resolution }`, path, line and column locations, call and implementation edges. `Index::query` (`crates/kb/src/graph.rs:583`) keeps a symbol only when its name or id equals the needle exactly. It stores no file content, docs or embeddings. Freshness works (`Store::stale` `store.rs:137`, `build` 168, `update` 263, `refresh` 301) but coverage needs external SCIP producers (`producer.rs`), and its only consumer is the CLI (`crates/app/src/kb_cmd.rs`). `omp-kb` depends only on `omp-ast` and `omp-core`, so an `omp-tools` to `omp-kb` edge creates no cycle (`cargo metadata --offline --no-deps`, 2026-11-13).
- The `judge` role exists as a selector: `crates/catalog/src/selection.rs:177` lists it and `selection.rs:465` resolves `"slow" | "plan" | "advisor" | "judge"` to `find_slow`, so `@judge` is a chat model, the same as `@slow`. Its only consumer is the ADW review phase (`crates/driver/src/adw/production.rs:322-328`). ADR 0053 D3 defines it and its status section marks the judge role Not started.
- There is no typed probability-judgment API. `crates/ai/src/call.rs:1039` carries `top_logprobs`, but only the OpenAI chat codec puts it on the wire (`codec/openai_chat.rs:627-628`); Anthropic, Bedrock, Ollama, GitLab and Cursor reject it (for example `codec/anthropic.rs:1364`), Gemini and OpenAI Responses drop it (`codec/gemini.rs:1222`, `codec/openai_responses.rs:3224`), and no response-side decoding exists outside the router's `chat.logprobs` capability check (`crates/ai/src/router.rs:877`).
- Building blocks that exist: `omp_grep` (`crates/grep/src/lib.rs`: `CompiledGrep` 368, streaming `grep_stream` 779); `omp_walker` (already a dependency, `crates/tools/Cargo.toml:57`; hidden and gitignore options, `node_modules` skip at `crates/walker/src/cache.rs:205`, scan cache `cache.rs:56`; no lockfile, binary or credential filter); the injected host-capability pattern (`SubagentSpawner`, `crates/tools/src/task.rs:321`); declared inference ceilings (`InferenceEffects { max_requests, max_usd }`, `crates/tool/src/lib.rs:453`).
- **ADR 0024 constrains how a new tool may exist.** The permanent wire roster must be small and fixed; a tool earns a slot only when used on most turns, and "rarely used harness capabilities NEVER enter the permanent roster" but ride `dyn` (ADR 0024 rules 3 and 4; ADR 0025). Semantic search is a rare, expensive call, so it does not qualify for a slot. The roster table already has the right shape: `PresentationClass::Device` is "Always a catalog device" and `ClaimsClass::BuiltinDevice` gives enhancement precedence (`crates/tools/src/roster.rs:74-98`); `security_scan` is such a row. `Family` variants are a closed match, so a builtin device still needs a new `Family` entry (`roster.rs` header), but it costs no wire schema slot. Python extension devices (ADR 0036) are the other no-slot path.

## Decision

**D1. A new builtin device, `locate@1`, reached as `dyn locate`, finds files and line ranges from a description.** The name `find` cannot be reused (CLI alias to `glob`, `cli.rs:284`); `locate` is not a current builtin name or alias, and slice 4 re-checks that against the roster before registering it. It is a roster row with `PresentationClass::Device` and `ClaimsClass::BuiltinDevice`: no permanent wire schema, no per-turn cost, listed in the live catalog only when enabled. Parameters follow v1: `query`, `grep_keywords`, optional `path`. Effects: documents read plus inference (`Effects.documents`, `Effects.inference`, `crates/tool/src/lib.rs:508`).

**D2. A new `Family` is justified, a Python extension is not.** Against the hard constraint (a new family only when no existing family, `dyn` device or extension can carry it): no existing family searches by meaning; `glob` and `grep` are exact; and an extension cannot carry it today because ADR 0036 makes extensions Python-only, the cascade is a tight loop over file bytes plus a judge, and extensions have no judge capability [unverified: no extension judge API was found]. A Python device remains the recorded alternative (below).

**D3. Port the v1 cascade; build no index.** The device is stateless: lexical prior, filename ranking, sketch routing, verification, in the same four waves with bounded-parallel dispatch (16 requests in flight). Constants stay in code except the knobs in D6.

**D4. `omp-kb` is not the retrieval engine.** Its exact-name query cannot match a description. It may later supply a read-only lexical prior (slice 6): exact-name hits for the model's `grep_keywords` raise a file's lexical score, only when an index already exists and `Store::stale` is false. The device never calls `build`, `update` or `refresh`.

**D5. The judge is an injected trait.** `Judge` lives in `omp-tools` beside `SubagentSpawner`; the driver implements it on the `@judge` role. `omp-tools` stays free of provider code. A judge call receives only typed state and questions, never the session transcript (mirrors ADR 0053 D2). The row's install phase (Composer, as `task` and `hub` at `roster.rs:887`, or another) is chosen in slice 4 from what injects the driver-owned judge [unverified whether a `Device` row may use Composer].

**D6. A typed judgment answer is the prerequisite; uncalibrated judges are marked.** The first backend is prompted chat on `@judge` returning a yes/no with a stated confidence per question, flagged `calibrated: false`. Log-probabilities are not used (Context). The device ships default off and never auto-enables; v1's `auto` mode is not adopted.

**D7. Gating and knobs are convars (ADR 0012), default off.** `sv_locate_enabled` (bool, default `false`, the roster row's gate), `sv_locate_hit_threshold` (float; initial default 0.5, reset by slice 5 from measurement because v1's 0.2 suits a native judge), `sv_locate_verify_model` (string, empty means the judge role verifies too, as v1's `find.verifyModel`). No `cap-*` feature in slices 0 to 5 (ADR 0049 reserves them for optional dependency edges); slice 6 adds `cap-kb` because it adds the `omp-kb` edge.

**D8. Bounded and declared.** The device declares `InferenceEffects { max_requests, max_usd }`, enforces the request ceiling, ends at a 20 s deadline with the verified hits (or a tool error telling the model to narrow `path` or use `grep`), and records every judge request and the resolved `@judge` route in the session journal.

**D9. Eligibility is decided before content reaches a judge.** Honour gitignore and hidden options through `omp_walker` and add what it lacks: a deny-list for build output, lockfiles and credential material (for example `.env*`, `*.pem`, `id_rsa*`), and v1's content test that a NUL in the first 8 KB means binary (`tools/jfind/text.ts`). Denied files are never read into a judge request.

## Rejected options

- **A new permanent wire-slot tool.** Violates ADR 0024 rules 3 and 5: not used on most turns, per-turn cost unpaid for.
- **Python extension device (ADR 0036).** No judge capability for extensions was found; file bytes and a hot loop would cross the bridge; ADR 0035 keeps engine logic in Rust. Revisit if an extension judge API appears.
- **Build on `omp-kb` as the candidate generator.** Exact symbol names only; cannot find a behaviour described in prose; needs external producers; no tool consumer.
- **Embedding index** (`omp-ai` `Embed`, `crates/ai/src/client.rs:70`; the `omp-memory` embedding worker). A persistent vector store brings freshness, storage and model-choice decisions larger than this gap; the cascade needs none. Revisit only if slice 5 measures the cascade as too costly.
- **Log-probability judge now.** One codec sends the field and none decodes the answer. It belongs to the typed-judgment work, not a shortcut around it.
- **Port Spotlight.** v1's ranker calls a hosted Stencil service (`spotlight/types.ts:20`). A hosted dependency is not engine code.
- **Reuse the name `find` or add a semantic flag to `grep`/`glob`.** Breaks the alias and the immutable-name rule, and hides inference spend inside a tool declared as search.
- **Auto-enable like v1.** omp2 has no native judge kind to detect, so `auto` would silently mean "off" or "on regardless of calibration".
- **Do nothing.** A read-only `task` subagent can explore semantically and stays the fallback, but it has no request budget, no deterministic line-range report and no per-call cost record.

## Slices

Each slice is one PR. Acceptance tests are written before the code and are not edited to pass.

0. **Pure ports, unreachable from any roster row.** Keyword derivation, byte clipping, windowing, window selection, sketching and heat merging as deterministic functions in `omp-tools` (v1: `keywords.ts`, `text.ts`, `passages.ts`). Tests: quoted phrases of at least 3 bytes stay whole; tokens under 3 bytes, stopwords and pure digits drop; the stemmer strips `ing`, `ed`, `es` or `s` only when at least 4 characters remain (`spawned` to `spawn`); no window exceeds its byte budget, tags map to real line numbers, and an oversize line clips on a code-point boundary; with all scores zero, selection spreads evenly instead of taking the opening bytes; heat merging never bridges an unjudged gap and keeps the maximum probability; every sketch line is a verbatim substring of its source.
1. **Lexical prior and eligibility, unreachable.** Per-file, per-keyword counts from one `omp_grep` pass through a `GrepSink`, IDF-weighted file score, and the D9 filter. Port `lexical.ts` in full first: only its header was read for this record. Tests: denied names and lockfiles never appear; a NUL in the first 8 KB rejects as binary and a blank file as empty; a gitignored file is excluded by default; scores are identical across runs.
2. **Cascade with a recording fake judge, unreachable.** The `Judge` trait, the four phases, budgets, partial results and failure reporting. Tests: wire state is built with ordered keys (deterministic bytes); sketch answers only route, so a reported range always comes from a complete verified passage; a deadline returns the hits verified so far, and with none a tool error; the request ceiling stops dispatch and flags the result partial; a judge failing every request yields a tool error, failing some a result listing at most five distinct failures; a judge input never contains a denied path or its content.
3. **Typed judgment in `omp-ai` and the driver `Judge`, behind `sv_locate_enabled` = false.** A request type (state plus named questions) with per-question probability answers; a prompted-chat backend on the `@judge` route flagged `calibrated: false`; usage attributed to the journal. It reuses the `@judge` resolution of `selection.rs:465` and ADR 0053 D3 rather than forking it. Tests: an unparseable answer counts as a failed question, never a probability; the request carries only state and questions; route and usage are recorded per request.
4. **The device `locate@1`, behind `sv_locate_enabled` (default false).** New `Family` entry, `Device` presentation, `BuiltinDevice` claim, `InferenceEffects` ceiling, a model-facing prompt written for omp2. Existing roster inventory assertions (the Composer list `["task", "hub"]` at `crates/tools/src/roster.rs:1868` if Composer is used, and the listed-identity count) gain the new row; that is the only change to an existing test and is called out in review. Tests: with the convar false `locate` is absent from the catalog and the registry; with it true `dyn locate` lists it, `locate@1` is recorded in the call identity and no wire schema slot is added (the roster-prefix test is unchanged); the CLI tool-list parser does not alias `locate`.
5. **Measurement gate.** A checked-in query fixture against this repository with expected files, run against `grep` alone and `locate`; record judge requests, spend and hit quality; set the `sv_locate_hit_threshold` default from the result. Acceptance: fixture and recorded results are in the PR. Whether to keep default off is the owner's call from those numbers.
6. **Optional `omp-kb` prior, behind `cap-kb` (off by default) and `sv_locate_kb_prior` (default false).** Tests: with no index, results equal the no-prior run; with a fresh index, a file defining a symbol that matches a keyword outranks an otherwise equal file; with a stale index the prior is ignored; the device never invokes `build`, `update` or `refresh`.

## Upstream origin and license

Logic is ported from can1357/oh-my-pi `c308970263` (v18.20.0): `packages/coding-agent/src/tools/jfind/` and `judgment/`. Upstream is MIT (`LICENSE`: © 2025 Mario Zechner, © 2025-2026 Can Bölük, © 2026 Stencil Labs, Inc.); omp2's `LICENSE` is MIT with only © 2026 Stencil Labs, Inc. A substantial port keeps the upstream permission and copyright notice, so each ported module names its origin path and commit in its header. The device prompt and judge rubric (`prompts/tools/find.md`, `prompts/judge/find.json`) are written fresh for omp2. Spotlight is not ported. No `pi` identity appears in ported names.

## Consequences

- A new device exists but is off until the owner enables it, and it adds no wire schema, so the prompt-cache prefix is unchanged (ADR 0024).
- Cost per call is a bounded number of judge requests (derived estimate for a full-repository v1 call: 25 to 40; slice 5 supplies the measured figure).
- File content goes to the user-configured `@judge` route; denied files never leave (D9).
- Hits from an uncalibrated judge are less comparable than v1's; the threshold convar and the `calibrated: false` flag make that visible.
- One closed-match `Family` entry and the roster inventory assertions change (slice 4).
- No index is stored, so nothing needs freshness. Recall is exactly as good as the judge and the lexical prior.

## Open questions for the owner

- **O1. Device name.** Recommendation: `locate`, confirmed against the roster in slice 4; none of the builtin names or aliases found collides.
- **O2. Default hit threshold for an uncalibrated judge.** Recommendation: 0.5 initially, replaced by the slice 5 measurement. v1's 0.2 is tuned to a native judge.
- **O3. Is selecting `@judge` consent for sending file content to that provider?** Recommendation: yes; the role is user-configured and the resolved route is recorded in every result. No new prompt, unless the approval system already has an inference-egress consent that `InferenceEffects` feeds (not traced).
- **O4. Is an `auto` enable mode wanted?** Recommendation: not until a calibrated route exists; decide then with the slice 5 data.
- **O5. Do upstream copyright lines belong in the root `LICENSE`?** Recommendation: yes; MIT ports must carry the notice, and header comments cover only the ported files.
- **O6. ADR 0053 says the judge role is Not started, yet the selector exists (`selection.rs:177`, `465`; `production.rs:322-328`).** Recommendation: treat the selector as landed for slice 3 and update ADR 0053's status text in its own change; this record does not edit it.
- **O7. May a `Device`-presented row use the Composer install phase?** Recommendation: if not, inject the judge through a driver bridge like the other builtin devices; settle in slice 4.

## Status in omp

**Status: Not started.** **Implemented and reusable:** `omp_grep` and `omp_walker`; the `@judge` role selector (chat-model backed); the `SubagentSpawner` injection pattern; roster `Device` rows; `InferenceEffects` ceilings; `omp-kb` (unused by any tool). **Unverified:** v1's `lexical.ts` scoring body, `questions.ts` rubric text and `prompts/judge/find.json` (only headers or nothing read); the approval path for inference egress (O3); whether a `GrepSink` yields per-keyword counts without a second pass; whether extensions have any judge capability.

## References

[docs/research/semantic-find.md](../research/semantic-find.md); ADR 0012 (convars); ADR 0024 and 0025 (small roster, `dyn`); ADR 0026 (versioned tools); ADR 0035 and 0036 (Rust engine, Python extensions); ADR 0046 (roster composition); ADR 0049 (`cap-*` features); ADR 0053 (`@judge` role); `crates/kb`; `crates/grep/src/lib.rs`; `crates/tools/src/roster.rs`; `crates/tools/src/task.rs`; `crates/tool/src/lib.rs`; can1357/oh-my-pi `c308970263` `packages/coding-agent/src/tools/jfind/` and `judgment/`.
