# 0060. Semantic file search: measure first, then a bounded T1 device over a judge; no index, no wire slot

Status: proposed
Date: 2026-11-13
Area: tools

## Context

Research: [docs/research/semantic-find.md](../research/semantic-find.md) (Gap 3 of the upstream gap closure). This record states decisions; the note holds the evidence.

### What OMPv1 does

- v1 has a semantic grep tool named `find` [code-v1, can1357/oh-my-pi `c308970263`, v18.20.0]: `tools/jfind/index.ts:68-69` (`FindTool`, `name = "find"`). Inputs: `query`, `grep_keywords`, optional `path`. Output: files with line ranges. It persists nothing and builds no index.
- The cascade is in `tools/jfind/cascade.ts` (509 lines, read in full for this record):
  - Lexical prior: one native grep pass over the keywords (case-insensitive, per matching line), file score from `lexical.ts`.
  - Wave 1, filename ranking: top `CANDIDATES = 128` files by lexical score, in batches of `NAME_BATCH = 64`, judged by name and tree position.
  - Selection: the 2 best lexical files are always read; the rest follow name score, then lexical score, up to `FILES = 20`.
  - Wave 2, sketches: each read file (up to `READ_LIMIT = 4 MiB`) is cut into at most `WINDOWS = 24` windows of `WINDOW_BYTES = 8192`; each window becomes a `SKETCH_BYTES = 384` sketch; cards go to the judge in batches of at most `SKETCH_CARDS_MAX = 48`. A failed or unanswered card scores 1 ("failure is unknown, never grounds for a negative judgment"). Cards below `CUTOFF = 0.45` are dropped; at most `FULL_LIMIT = 40` survive.
  - Wave 3, verification: surviving full passages, grouped per file, at most `VERIFY_STATE_BYTES / WINDOW_BYTES` = 3 per request. A file is a hit when its maximum passage probability reaches `THRESHOLD = 0.2` (or `find.hitThreshold`); ranges are merged with `mergeHeat`.
  - Budgets: `FIND_TIMEOUT_MS = 20_000` (`index.ts:53`), `PARALLEL = 16`, `SCAN_TIMEOUT_MS = 30_000`. A deadline returns the hits verified so far.
- v1 asks the `judge` role (`index.ts:118`, `resolveJudge`) and needs absolute probabilities per entry ("noul" questions, `questions.ts`). `judgment/index.ts:290` (`textJudgeRoleChain`) refuses to let a prompted model stand in for a native judge. `isFindEnabled` (`index.ts:61`) with `find.enabled: auto` (`tools/settings.ts:564`) enables the tool only when `hasNativeJudge` (`judgment/index.ts:392`) is true.
- Lexical formulas, read from `lexical.ts`:
  - `idf_k = clamp(ln((filesScanned + 1) / (df_k + 1)), 0.5, 6)`, where `df_k` is the number of files with a nonzero count for keyword k.
  - `fileScore = sum over k of idf_k * (2 * inPath_k + ln(1 + count_k))`, where `inPath_k` is 1 when the lowercased relative path contains keyword k, and `count_k` is the number of occurrences of k on lines that matched.
  - Candidates sort by score descending, ties by relative path.

### What omp2 has

- Naming: `crates/tools/src/glob.rs` is path matching. `crates/app/src/cli.rs:284` maps the tool name `find` to `glob`, so `find` is taken. Existing names are unchangeable (ADR 0026). No builtin named `locate` exists; the only hit in `crates/` is `crates/driver/src/v1_import/locate.rs`, a path resolver.
- `omp-kb` (`crates/kb`) is a symbol graph. `Index::query` (`crates/kb/src/graph.rs:583`) matches a symbol only when its name or id equals the needle. No file content, no embeddings; only consumer is `crates/app/src/kb_cmd.rs`. `omp-kb` depends on `omp-ast` and `omp-core` only, so a dependency from a tool crate creates no cycle (`cargo metadata --offline --no-deps`, 2026-11-13).
- Judge role: `crates/catalog/src/selection.rs:177` lists `judge`. `selection.rs:458-465`: a role with no selectors resolves by role name, and `"slow" | "plan" | "advisor" | "judge"` goes to `find_slow`. The test at `selection.rs:1243` asserts `@judge` and `@slow` select the same model when the role is unset. The only consumer is the ADW review phase (`crates/driver/src/adw/production.rs:322-328`). ADR 0053 D3 defines the role; its status section says Not started.
- No typed probability judgment exists. `crates/ai/src/call.rs:1039` carries `top_logprobs`, but only the OpenAI chat codec sends it (`codec/openai_chat.rs:627-628`); Anthropic, Bedrock, Ollama, GitLab and Cursor reject it (e.g. `codec/anthropic.rs:1364`); Gemini and OpenAI Responses drop it (`codec/gemini.rs:1222`, `codec/openai_responses.rs:3224`). Nothing decodes log-probabilities (router capability check only, `router.rs:877`).
- Building blocks: `omp_grep` (`crates/grep/src/lib.rs`: `GrepSink` at 321, one `matched` call per match with `path`, `line_number`, `line_bytes`; `CompiledGrep` 368; `grep_stream` 779). `omp_walker` (`crates/tools/Cargo.toml:57`; gitignore and hidden options; `node_modules` skip at `crates/walker/src/cache.rs:205`; no lockfile, binary or credential filter). `DEFAULT_UPLOAD_DENY` (`crates/exec/src/deny.rs:16`, 21 name patterns: `.env`, `*.pem`, `id_rsa*`, `.aws/**`, `*.tfstate` and more) is the existing never-upload list.
- Budgets: `InferenceEffects { max_requests, max_usd }` (`crates/tool/src/lib.rs:453`). Approval: any non-empty inference effect raises the call to the Exec tier (`crates/contract/src/posture.rs:382-391`). Enforcement: `crates/ai/src/layer/budget.rs` has `InferenceBudget { max_requests, max_input_tokens, max_output_tokens, max_wall_time, max_usd }` (line 24) and an `InferenceLedger` keyed by attributed extension and turn or session, with the request check at line 169. Policies are set from extension envelopes (`crates/ai/src/registry.rs:397-410`). Builtin calls attribute to `core` by default (`crates/ai/src/call.rs:200`), so how a builtin tool gets its own ceiling in that ledger is unverified.

### Placement rules that bind

- ADR 0024: the permanent wire roster is small and fixed; rarely used capabilities ride `dyn` and never take a slot (rules 3 and 4).
- ADR 0049 D1: every capability sits in the lowest tier that satisfies it, and "new capabilities default to T2" (a first-party extension, Python, ADR 0036). Promotion to T1 needs an evidenced reason from the T1 test: names no vendor and needs a first-class scheme, repository-lock coordination, a hot-path budget, or raw credentials. A T1 capability is one crate behind a Cargo feature `cap-<name>`, default-on in `omp-app` only. "Tier is not roster": T1 tools still ride `dyn`.
- The roster table already supports slot-free builtins: `PresentationClass::Device` ("Always a catalog device") and `ClaimsClass::BuiltinDevice` (`crates/tools/src/roster.rs:74-98`); `security_scan` and `manage_skill` are such rows. A Composer-phase row is registered by the driver (`crates/driver/src/roster.rs:10-60`: `ComposerHost`, `register_composer`), because the driver owns model routes; envd does not depend on `omp-agent` (ADR 0046 item 2).

## Decision

**D1. Measure before porting (S0).** A throwaway spike decides whether any of this ships. It runs the v1 cascade in Python on the eval kernel (`completion(model="slow")`, `grep`, `glob`; `crates/envd/src/eval/python_prelude.py:824`) over a fixed query set against this repository, and compares it with `grep` alone. Go if all hold: the cascade finds the expected file in its top 5 for at least 20 percentage points more queries than `grep` alone; median cost per call is at most US$0.10; 95th-percentile wall time is at most 20 s. No-go closes the gap as class b ("not needed: the user uses grep, glob and a read-only task subagent") and nothing below is built. The spike's artifacts are not product code and live under `docs/research/semantic-find-spike/`, like `docs/research/adw-on-eval/`.

**D2. If go, the tier is T1, justified, with T2 as the recorded default.** ADR 0049 D1 says new capabilities default to T2. The spike also runs the cascade as a pure Python helper (that is what it is), so its results answer the T1 test directly:
- A T2 Python implementation reads and greps through the tool bridge per call; the T1 test's "hot-path budget" applies when the lexical pass over a whole tree plus reading up to 20 files per call must run in-process through `omp_grep` and `omp_walker`. If the spike shows the Python version within the 20 s p95 limit, promotion fails the T1 test and the capability stays a T2 extension (an extension helper, no core change); this ADR then closes with the spike result and its slices are dropped.
- Promotion is recorded in the crate README with the measured evidence, as ADR 0049 D1 requires. A T1 crate also needs the host-owned judge route (ADR 0006: the host owns inference), which an extension reaches only through `completion()` tiers, not `@judge`.

**D3. Shape if promoted.** A new crate `omp-locate` (workspace member `crates/locate`, entry in root `Cargo.toml` beside `omp-kb` at line 136) holds the pure cascade and the `Judge` trait. A tool module `crates/tools/src/locate.rs` wraps it as the builtin device `locate@1`, behind the Cargo feature `cap-locate` (`omp-tools`: `cap-locate = ["dep:omp-locate"]`, forwarded by `omp-envd` and `omp-app` the way `cap-forge` is, `crates/app/Cargo.toml:42`). It is a roster row with `PresentationClass::Device` and `ClaimsClass::BuiltinDevice`: reached as `dyn locate`, no wire schema, no per-turn cost. The name is `locate`; `find` keeps its alias to `glob`.

**D4. Off by default through an additive gate that does not read `sv_tools_enabled`.** The roster gate reads `sv_tools_enabled`: a row's `settings_key` is its family name when the row is built with `settings = true`, and `admit()` calls `host.tool_enabled(key)` (`crates/tools/src/roster.rs`, `admit` at line 999). The trait says "Missing keys are enabled" and `crates/envd/src/tool_settings.rs:623` is `unwrap_or(true)`, so a gated row would be on by default. Decision:
- The `locate` row is built with `settings = false` (as `task` and `hub` at `roster.rs:519-541`, and `manage_skill`), so `sv_tools_enabled` is never consulted and its semantics are untouched.
- The Composer arm registers the tool only when the host answers a new `ComposerInstall::locate_enabled()`, mirroring the existing `py_eval_enabled()` precedent (`roster.rs:1139`, `1173`, used at `1525` and `1661`). `crates/driver/src/roster.rs` `ComposerHost` answers it from the convar `sv_locate_enabled` (bool, default `false`), passed into `composer_dynamic_tools` by the caller. Its `tool_enabled` already returns true for everything (`driver/src/roster.rs:21`), which is why the extra method is needed. Reading the convar from the session context at that call site is unverified plumbing and is part of slice L5.
- ADR 0024 rule 2: the roster never changes mid-session, so flipping the convar takes effect in the next session.
- Test `fresh_config_does_not_offer_locate`: with no config, `composer_dynamic_tools` returns no `locate` tool, `dyn` lists no `locate` device, and the registry has no `locate` identity. A second test, `sv_locate_enabled_registers_one_device`, covers the on path.

**D5. The judge route is `@judge`, with the `@slow` fallback as the defined behaviour, and the fallback fails closed.**
- `locate` resolves the `@judge` role. Unset `@judge` falls back to `@slow` (`selection.rs:465`, test at `1243`; ADR 0053 D3). That is the defined behaviour.
- A fallback route can be a different provider from the session's, and sends file bytes there. So: when `@judge` has selectors (the user configured it), `locate` proceeds. When it has none (fallback), `locate` refuses with an error naming the resolved route and what would be sent, unless the convar `sv_locate_allow_slow_fallback` (bool, default false) is true. The slice that adds the judge also adds a small `judge_route()` in `omp-catalog` returning the route and whether the role was explicit (derived from `selectors.is_empty()` at `selection.rs:458`).
- Approval: the declared inference effect makes every call Exec tier (`posture.rs:382-391`), but the prompt is generic. The device's result details always carry the resolved route and the file-bytes total sent. Whether a tool can supply a route-naming approval summary was not traced; the fail-closed rule above does not depend on it.

**D6. The judge answer is an absolute probability per entry.** Not yes/no plus confidence. v1's cascade thresholds absolute probabilities (`CUTOFF = 0.45`, `THRESHOLD = 0.2`). The prompted-chat backend returns a JSON object mapping entry id to an integer 0 to 100; the probability is n/100. An unparseable or missing entry is a failed question: sketch cards score 1 (v1 rule), name and verification entries count as unanswered, never as 0. How thresholds apply:
- `CUTOFF = 0.45` (constant) routes sketches to verification.
- A file is a hit when its maximum verified-passage probability is at least `sv_locate_hit_threshold` (float; default 0.5 until the spike sets it; v1's 0.2 suits its native judge).
- The backend is flagged `calibrated: false` in result details. No log-probabilities (Context).

**D7. Three ceilings, enforced in the cascade before each request is built.**
- Requests: 64 (`sv_locate_max_requests`). Derived worst case: 2 name batches + 11 sketch batches (480 cards at about 46 per batch given an 18,000-byte state budget) + at most 40 verification requests = 53, plus headroom.
- Spend: US$0.25 per call (`sv_locate_max_usd`). Derived, with an assumed input price: the byte ceiling below is about 147k tokens at 4 bytes per token; at US$1.50 per million input tokens that is US$0.22. Slice L6 replaces both numbers with measurements.
- Bytes: 589,824 (576 KiB, `sv_locate_max_bytes`) of file-derived content per call, counting every byte of sketch, passage and file-name state sent to the judge. Derived from v1's worst case: 40 verified passages at 8,192 B is 327,680 B; 480 sketches at 384 B is 184,320 B; file-name listings and framing take the rest. A request that would cross a ceiling is not sent; the result is flagged partial. Request count and spend alone do not bound content exposure, which is why bytes get their own ceiling.
- The device also declares `InferenceEffects { max_requests: 64, max_usd }` so the approval system sees an inference effect. `crates/ai/src/layer/budget.rs` (`InferenceBudget`, `InferenceLedger`, request check at line 169) is the backstop where a ceiling can be registered for the builtin's attribution; wiring a builtin into that ledger is unverified and is part of slice L5. The cascade's own accounting is the primary enforcement.

**D8. Eligibility is decided before content reaches a judge.** `omp_walker` supplies gitignore and hidden handling. On top of it:
- The deny list is `DEFAULT_UPLOAD_DENY` (`crates/exec/src/deny.rs:16`), not a new list. `omp-locate` takes the patterns as a parameter and the driver passes the constant, because `omp-tools` does not depend on `omp-exec`.
- v1's content test: a NUL in the first 8 KB is binary; blank files are rejected; lockfiles and build output are excluded by name.
- Denied files are excluded from candidates and never read.

**D9. `omp-kb` is not the retrieval engine.** An optional lexical prior (slice L8) may boost files defining a symbol that exactly matches a keyword, only when a fresh index exists. It never builds or updates an index.

**D10. A deadline of 20 s.** On expiry the device returns verified hits, or a tool error telling the model to narrow `path` or use `grep`.

## Rejected options

- A permanent wire-slot tool: ADR 0024 rules 3 and 5.
- Embedding index (`crates/ai/src/client.rs:70` `Embed`, the `omp-memory` embedding worker): freshness, storage and model choice are larger than this gap; the cascade needs none. Revisit only if the measurements say the cascade costs too much.
- Building retrieval on `omp-kb`: exact symbol names only.
- Log-probability judging: one codec sends the field, none decodes it.
- Port v1 Spotlight: it calls a hosted Stencil service (`spotlight/types.ts:20`).
- Reuse the name `find` or add a semantic flag to `grep`/`glob`: breaks the alias and hides inference spend in a search tool.
- Auto-enable like v1: omp2 has no native judge kind to detect.
- A T1 crate without the spike: ADR 0049 D1 makes T2 the default and the T1 test needs evidence.
- Do nothing: that is the no-go outcome of D1, not a separate option.

## Slices

Each slice is one PR; tests are written first and not edited to pass. Slices after S0 happen only on a go.

**S0. Measurement spike (no product code).**
- Artifacts under `docs/research/semantic-find-spike/`: a query fixture (at least 20 queries about this repository, each with the expected file or files and the reason it is hard for `grep`), the Python cascade, and a results file (judge requests, bytes sent, spend, wall time, top-5 hit per query for the cascade and for `grep`).
- Acceptance: the fixture, script and results are in the PR; the go or no-go call is written against the D1 thresholds; the T1 test (D2) is answered from the results.

**L1. Crate `omp-locate`: pure functions.** Keyword derivation, byte clipping, windowing, window selection, sketching, heat merging, IDF and file score. No I/O.
- Tests: `keywords_keep_quoted_phrases_whole` (at least 3 bytes), `keywords_drop_short_stopword_and_digit_tokens`, `stem_needs_four_character_base` (`spawned` to `spawn`), `windows_respect_byte_budget_and_line_tags`, `oversize_line_clips_on_code_point_boundary`, `select_windows_spreads_when_all_scores_zero`, `sketch_lines_are_verbatim_and_budgeted`, `merge_heat_never_bridges_gap_and_keeps_max`, `idf_is_clamped_to_half_and_six`, `file_score_counts_path_hit_as_two_log_units`, `candidates_sort_by_score_then_path`.
- Oracle: v1's `keywords.ts`, `passages.ts`, `text.ts`, `lexical.ts`.

**L2. Lexical counting over `omp_grep`.** A `GrepSink` that counts keyword occurrences per file per matching line.
- `GrepSink::matched` is called per match, so a line with several matches arrives several times; the sink dedupes on (path, line_number) before counting occurrences in the line.
- Tests: `sink_counts_each_line_once_with_all_keyword_occurrences`, `sink_is_case_insensitive`, `sink_honours_cancellation_control`, and a golden test of scores on a small fixture tree compared with hand-computed `fileScore`.

**L3. Eligibility.**
- Tests: `deny_patterns_exclude_env_pem_and_ssh_paths` (every `DEFAULT_UPLOAD_DENY` entry is exercised), `nul_in_first_8k_is_binary`, `blank_file_is_empty`, `lockfiles_and_build_output_are_excluded`, `gitignored_file_is_excluded_by_default`, `denied_file_is_never_read` (instrumented reader).

**L4. Cascade against a recording fake judge.** The `Judge` trait, the four waves, selection rules (2 best lexical files always read), ceilings, deadline, partial results.
- Tests: `wire_state_has_ordered_keys`, `unanswered_sketch_scores_one`, `sketch_below_cutoff_is_dropped`, `at_most_40_passages_verified`, `three_passages_per_verification_request`, `hit_is_max_passage_probability_at_threshold`, `request_ceiling_stops_dispatch_and_flags_partial`, `byte_ceiling_stops_dispatch_and_flags_partial`, `usd_ceiling_stops_dispatch_and_flags_partial`, `deadline_returns_verified_hits`, `deadline_with_no_hits_is_a_tool_error`, `all_requests_failing_is_a_tool_error`, `judge_input_never_contains_denied_path_or_bytes`.

**L5. Typed judgment and the driver judge.** A request type (state plus named questions) in `omp-ai` with per-entry probability answers; the prompted-chat backend on the `@judge` route; `judge_route()` in `omp-catalog`; fail-closed fallback; usage in the journal.
- Tests: `unparseable_answer_is_a_failed_question_not_a_probability`, `request_carries_only_state_and_questions`, `unset_judge_refuses_without_fallback_flag`, `explicit_judge_proceeds`, `fallback_flag_allows_slow_route`, `route_and_bytes_recorded_in_result_details`.

**L6. The device, behind `cap-locate` and `sv_locate_enabled` (default false).** Roster and driver changes:
- Row values, following the `security_scan` and `manage_skill` precedent (`roster.rs:566-575`, `620-631`): `same(Family::Locate, false /* hidden */, true /* listed */, Phase::Composer, true /* protect_core */, Present::Device, Claim::BuiltinDevice, false /* settings: sv_tools_enabled never consulted */, BridgeNeeds::WORKSPACE)`. The bridge bits only document the row.
- Match sites that need an arm for `Family::Locate` (each currently lists `Task` and `Hub` or `Glob`): the `Family` enum (`roster.rs:168`); the table (beside `roster.rs:519-541`, under `#[cfg(feature = "cap-locate")]`); `phase_of` (`887`, Composer); `install_session_row` no-op list (`1328-1349`); `install_composer` (`1361-1405`, the real arm); `declare_row` no-op list (`1548`); `construct_row` no-op list (`1685`). `ComposerInstall` (`1107`) gains `locate_enabled()` and a judge accessor; `crates/driver/src/roster.rs` implements both.
- Existing tests that must change: the Composer inventory (`roster.rs:1868`, `assert_eq!(composer, ["task", "hub"])` becomes `["task", "hub", "locate"]` with the feature on), the protect list (`roster.rs:~1830`), and the listed-identity checks. These are the only edits to existing tests and are called out in review.
- Tests: `fresh_config_does_not_offer_locate`, `sv_locate_enabled_registers_one_device`, `locate_row_is_a_device_not_a_slot` (`registration(row, policy)` gives Device presentation and BuiltinDevice claims; the wire roster prefix is unchanged), `locate_is_absent_without_cap_locate`, `cli_tool_list_does_not_alias_locate`, `sv_tools_enabled_does_not_gate_locate`.

**L7. Calibration.** Reuse the S0 fixture against the built device; replace the placeholder ceilings (64 requests, US$0.25, 576 KiB) and the default hit threshold with measured values. Owner decides whether default-off stays.

**L8. Optional `omp-kb` prior, behind `cap-kb` (off by default) and `sv_locate_kb_prior` (default false).**
- Tests: `no_index_equals_no_prior`, `fresh_index_boosts_file_defining_matching_symbol`, `stale_index_is_ignored`, `never_calls_build_update_or_refresh`.

## Upstream origin and license

Ported from can1357/oh-my-pi `c308970263` (v18.20.0), `packages/coding-agent/src/tools/jfind/` and `judgment/`. Upstream is MIT (© 2025 Mario Zechner, © 2025-2026 Can Bölük, © 2026 Stencil Labs, Inc.); omp2's `LICENSE` is MIT with © 2026 Stencil Labs, Inc. only. A substantial port keeps the upstream permission and copyright notice: each ported module names its origin path and commit in its header. The tool prompt and judge rubric (`prompts/tools/find.md`, `prompts/judge/find.json`) are written fresh. Spotlight is not ported. No `pi` identity in names.

## Consequences

- Nothing ships unless the spike shows the cascade beats `grep` at a bearable cost; a no-go costs one PR of research artifacts.
- If it ships, there is no wire schema and no per-turn cost; the device is off until a user turns it on, and a user who has not configured `@judge` is refused rather than silently sent to `@slow`.
- Content exposure is bounded three ways (requests, spend, bytes) and the deny list is the repository's existing one.
- One closed-match `Family` entry, one new crate, one Cargo feature, and three existing roster assertions change.

## Review (EGM, 2026-11-13)

Readiness reviewer (fresh context), and how each finding is resolved:
- BLOCKING, gate mechanism: confirmed. `sv_tools_enabled` defaults to enabled for a missing key (`tool_settings.rs:623`). Resolved in D4: ungated row (`settings = false`), Composer arm gated by `locate_enabled()`, new test `fresh_config_does_not_offer_locate`.
- Slice 1 was not buildable: filled with L1 to L3 (formulas, `GrepSink` dedupe, `DEFAULT_UPLOAD_DENY`) and the roster values, match sites and named tests in L6.
- Slice 2 ceiling had no number: D7 gives 64 requests, US$0.25, 576 KiB, with derivations; calibration is L7.
- Slice 3 decisions: (a) budgets, D7; (b) `@judge` with the `@slow` fallback as defined behaviour, fail closed, D5; (c) answer shape is absolute probability, D6.
- IDF formula missing: added to Context and tested in L1.

Design critic and zoom-out:
- Confirmed: Device-row family is justified under ADR 0024/0025; no `locate` name collision with a builtin; v1 constants match upstream.
- Consent (O3): approval does gate (`posture.rs:382-391`) but is generic. D5: fail closed on the fallback, route and bytes in result details, route-naming approval text left as an open question.
- Per-call byte ceiling: D7.
- Deny list: D8 reuses `DEFAULT_UPLOAD_DENY`.
- Budget enforcement site: D7 names `crates/ai/src/layer/budget.rs` and marks the builtin wiring unverified.
- Install phase settled now: Composer, because the driver owns the model routes (Context, D3, D4).
- Extension placement rejection: D2 cites ADR 0049 D1 and makes T2 the default pending evidence, instead of rejecting it outright.
- Measurement spike first: S0 and D1.

## Open questions for the owner

- **O1. Spike thresholds.** Go if top-5 recall improves by at least 20 percentage points over `grep`, median cost at most US$0.10, p95 wall time at most 20 s. Recommendation: use these; the owner may adjust before S0 starts.
- **O2. Is the `locate` name acceptable?** Recommendation: yes.
- **O3. Route-naming approval text.** Can a tool supply an approval summary that names the resolved route and that file content leaves the machine? Not traced. Recommendation: if yes, add it; if no, keep the result-details route record and the fail-closed rule.
- **O4. How does a builtin get its own ceiling in `InferenceLedger`?** Attribution defaults to `core`. Recommendation: rely on the cascade's own accounting; add a ledger policy only if the attribution is cheap to scope.
- **O5. Root `LICENSE` and upstream notices.** Recommendation: list the upstream copyright lines.
- **O6. ADR 0053 status says the judge role is Not started although the selector exists.** Recommendation: correct that text in its own change; this record does not edit it.
- **O7. Should `cap-locate` be default-on in `omp-app` (ADR 0049's T1 rule)?** Recommendation: yes; the runtime convar keeps it off.

## Status in omp

**Status: Not started.** Reusable today: `omp_grep` and `omp_walker`; `DEFAULT_UPLOAD_DENY`; the `@judge` selector (chat-backed); Device roster rows; Composer registration; `InferenceEffects` and the budget ledger; `omp-kb` (unused by tools). Unverified: the convar plumbing into `composer_dynamic_tools`; a route-naming approval summary; a builtin's ledger attribution; extension access to a judge role.

## References

[docs/research/semantic-find.md](../research/semantic-find.md); ADR 0006 (host owns inference); ADR 0012 (convars); ADR 0024 and 0025 (small roster, `dyn`); ADR 0026 (versioned tools); ADR 0035 and 0036 (Rust engine, Python extensions); ADR 0046 (roster composition); ADR 0049 (tiers, `cap-*`); ADR 0053 (`@judge` role); can1357/oh-my-pi `c308970263` `packages/coding-agent/src/tools/jfind/` and `judgment/`.
