# 0062. Mnemopi extracts facts without a model: expiring facts, rules in English and Spanish, and a bounded Apple FM lane

Status: Proposed 2026-11-13
Date: 2026-11-13
Area: state

## Context

Citations are `path:line`; upstream paths carry the prefix `upstream:`; bare `store.rs`, `extract.rs`, `retain.rs`, `session.rs` and `config.rs` are in `crates/memory/src/`. Evidence and line-level detail: `docs/research/mnemopi-fact-extraction.md`.

- Upstream keeps a deterministic floor under LLM fact extraction. `heuristicExtractFacts` turns eight English first-person patterns into at most five facts (`upstream:packages/mnemopi/src/core/extraction.ts:276-315`); `extractAndStoreFacts` adds metric, date, version and relation patterns (`upstream:packages/mnemopi/src/core/beam/consolidate.ts:675-783`). Both run when no model resolves: `llmMode` is `none`, or `smol` with no `memory` role model (`upstream:packages/coding-agent/src/mnemopi/backend.ts:539,558-563`; `upstream:packages/mnemopi/src/core/extraction.ts:432-443`).
- omp2 has the seam and nothing behind it. `ExtractionLane` has no implementor and `pending_extractions` has no consumer (`extract.rs:30-35`, `store.rs:571-599`); no code outside `crates/memory/src` calls settled-turn or shutdown retention, and the finalizer slot `set_mnemopi` has no production registrant (`crates/driver/src/headless/finalize.rs:100-107`). `HostSettings::from_con` never sets `llm_mode`, so production is always `smol` (`crates/envd/src/host_settings.rs:204-234`; `config.rs:306`): `none`, `remote` and `local-memory-model` cannot be selected.
- With `llm_mode = none` retention stores the episode and no extraction job, and a test pins it (`session.rs:151,227,384`). Nothing derives a fact without a model.
- Facts have no end of validity. `search_facts` returns them forever (`store.rs:826-857`). They are immutable, so a newer fact cannot expire an older one (`store.rs:706`; `crates/envd/src/tool_url/memory.rs:195-197`).
- Upstream's rules are English first-person only, write key/value rows (`subject = key`, `predicate = factType`; `upstream:packages/mnemopi/src/core/beam/consolidate.ts:289-314`) and keep only counters for decisions (`:781-782`). omp2 indexes `subject predicate object` as text (`store.rs:1666`), so rows must be real triples.
- The only on-device text generator is the opt-in Apple Foundation Models route; no in-process generator exists for the GGUF memory models (`docs/adr/0023-tiny-local-model.md:26-29,70`). Its backend has a 4096-token window, a 30 s deadline, one non-queueing admission permit that fails with `ConcurrentRequests` when busy, typed error codes, and a stub off macOS (`crates/ai/src/local/applefm.rs:63-101,113-147,1386-1391`). `aux_lane.rs` already uses it to re-rank recall behind `apple-aux-memory` (`aux_lane.rs:1-46`); no CI job builds that feature.
- Schema changes today bump `SCHEMA_VERSION`, and a bump drops `memory_embeddings`, `triples`, `memory_links` and the FTS tables and locks older binaries out of the bank (`store.rs:1217-1234,1642-1658`).
- Owner decisions fixed before this ADR: (1) the rule extractor runs only when no memory model is set; (2) `valid_until` is set by both the rule extractor and the model's `FACT` lines through an optional sixth field, and five-field lines stay valid; (3) recall filters expired facts in `search_facts` and read-by-id still returns them.
- Prototype: local commit `daab333d13` (branch `feat/mnemopi-fact-extraction`, base `b8032a94ad`, not pushed) implements the column, the sixth field and the recall filter with 15 tests, and has no gate.

## Decision

**D1. Rules are a floor, used only when no memory model is available.** The rule extractor runs when (a) `llm_mode` is `none`, or (b) the configured lane reports `Unavailable` and the caller passes an explicit `FallbackPolicy::Rules`. It MUST NOT run beside an available model. A model that is available and fails transiently is retried and NEVER replaced by rules (`store.rs:712-715`). Rules MUST NOT call any model, hosted or local (ADR 0023 rule 4, `0023:36-38`: fallback is an explicit caller policy). This reads the owner's "no memory model is set" as upstream does (`backend.ts:539,558-563`); see O1.

**D2. Facts may expire.** `facts.valid_until INTEGER NULL` holds the exclusive end of validity in UTC epoch milliseconds. Producers are the rule extractor and the optional sixth field of a `FACT` line: RFC 3339, or `YYYY-MM-DD` meaning valid through that UTC day. Five-field lines stay valid; a malformed sixth field rejects the line as a malformed confidence does. `search_facts` excludes facts with `valid_until <= now` in its `WHERE` clause, ahead of `LIMIT`, so expired rows cannot use up result slots; read-by-id still returns them. The filter and the sixth field are gated by `sv_memory_fact_expiry`; recording a `valid_until` from a typed producer is not gated, because the column is inert while the filter is off.

**D3. The column ships without a `SCHEMA_VERSION` bump.** `facts.valid_until` is added by `add_column_if_missing` on every open (`store.rs:1345-1372`) and `SCHEMA_VERSION` stays 3. A bump would run `RESET_REBUILDABLE_INDEXES`, so every bank loses its embeddings, triples and links and must re-embed, and the vector and graph generations reset; and a bump makes every older binary refuse the bank (`store.rs:1217-1234,1642-1658`), which breaks rollback and any older omp process sharing it (the store is documented "Multiprocess-safe", `store.rs:1`). An additive nullable column that no statement, trigger or index touches needs neither: an older binary's explicit-column INSERT and SELECT keep working, and it simply does not filter. The open already holds an exclusive transaction (`store.rs:1215`), so concurrent first opens add the column once. Rule for later changes: a nullable column nothing else touches is added on open; a change to meaning, a derived index or a constraint MUST bump. This departs from the bump-per-change path (`store.rs:1220-1223`) and needs the owner's confirmation (O3).

**D4. Rule output is typed, closed and bounded.** Triples over a closed predicate vocabulary: `name`, `is`, `works_at`, `lives_in`, `uses`, `prefers`, `dislikes`, `instruction`, `version`, `depends_on`, `dev_depends_on`, `requires_runtime`, `decided`, `rejected`, `stopped_using`, `deadline`, `status`. Subjects are `user`, a package or tool name, or `project`. Ids are `rule_` plus 24 hex of `Hash32` over source id, rule id and the triple, so replays are idempotent under `INSERT OR IGNORE` (`store.rs:733-737`). At most 32 facts per source memory; subject at most 128 bytes, object at most 240. Confidence 0.9 structured, 0.7 prose, 0.6 decisions and bounds. Rules take time as an argument and MUST NOT read a clock. Precision over recall: questions, hypotheticals, negations, fenced code and clauses near a secret word produce nothing.

**D5. Structured tool results first.** The extractor takes typed `ToolObservation { tool, target, output }` inputs and parses them with named-field parsers over typed serde structs (`package.json`, `Cargo.toml`, version banners of a closed tool set) before any prose rule; text a parser consumed is excluded from the prose pass. Selection keys on tool and target and MUST NOT sniff content. Free-text fields and credential-like keys are never read. Tool output feeds rules only, never a model lane: `ExtractionRequest.input` stays user-authored only (`extract.rs:22`).

**D6. English and Spanish are data tables.** Families: identity and preference, versions, dependencies, decisions, supersession, explicit bounds. Neutral Spanish, matched accent-insensitively. Dates are ISO and English or Spanish month names; numeric and relative dates are not parsed in v1 (O5).

**D7. Four gates, each default-off or default-unchanged.** `sv_memory_fact_expiry` (bool, off), `sv_memory_rule_facts` (bool, off), `sv_memory_llm_mode` (default `smol`, today's value; offers `none` and `smol`, and `local-memory-model` with slice 6) and `sv_memory_local_extraction` (bool, off). They use the `sv_` prefix as omp2-native host policy; the `ai_mnemopi_*` family carries pi aliases (`host_settings.rs:85`). They are declared beside `SV_WORKTREE_BASE` in `crates/envd/src/host_settings.rs` (`:139`), read in `HostSettings::from_con`, and reach the runtime as `#[serde(default)]` fields of `MnemopiSettings` (pattern `config.rs:196-198`); `omp-memory` gains no `omp-con` dependency. With every gate at its default, behaviour is byte-identical to today.

**D8. Typed lane faults and a bounded worker.** `LaneFault { Unavailable, Rejected, Transient }` travels in a new typed `Error` variant (ADR 0035, `docs/adr/0035-rust-for-the-engine.md:54-55`; no string payload) beside the coarse `AuxiliaryCompletion` (`crates/memory/src/lib.rs:63-65`). A bounded worker function drains `pending_extractions` and applies the policy: `Unavailable` with `FallbackPolicy::Rules` runs the rules and completes the job; `Transient` leaves the job queued; `Rejected` runs the rules if allowed, else acknowledges the job with no facts so it cannot loop. The worker is library code until a host composes it (O2).

**D9. `local-memory-model` uses the existing Apple FM backend, one bounded request per chunk.** A module beside `aux_lane.rs`, behind `apple-aux-memory`, calls `AppleFm::generate` (admission, 30 s deadline, cancellation, token cap), not `AppleFmSession::respond` (`applefm.rs:428-435,507-537`). Input is split on paragraph then sentence boundaries into chunks of at most 4 KiB, at most 8 per job. Each request has a fixed, golden-tested system prompt (the `FACT` protocol, the D4 vocabulary, and the sixth field only when expiry is on), temperature 0 and `max_tokens(512)`. Only lines starting with `FACT` and a TAB are kept, so `parse_facts` stays the single parser. Faults: model unavailable maps to `Unavailable`; `ContextOverflow` splits the chunk once, then skips it; guardrail, locale and decoding failures skip the chunk; timeout, rate limit, busy permit, runtime failure and cancellation map to `Transient`. A busy permit never blocks: the job stays queued behind the user's chat. The 4 KiB figure is conservative and unmeasured (O9).

**D10. Facts stay immutable.** There is no mutation path. A newer fact does not expire an older one; the rules emit `stopped_using` and `uses` facts instead.

## Alternatives considered

- **Literal port of the upstream patterns.** Rejected: English only, key/value rows that would index as "version version 1.2.3", no decisions or dependencies.
- **Rules as an `ExtractionLane` emitting `FACT` text.** Rejected: it formats and re-parses on every pass, loses typed fields, and `none` mode has no lane.
- **A model for Spanish and technical facts.** Rejected: against deterministic-first and D1, and useless offline.
- **A Python extension.** Rejected: the work belongs in the retention transaction in Rust, and an extension runs on the embedded interpreter outside it (ADR 0036, `docs/adr/0036-python-for-extensions.md:37`).
- **Bump `SCHEMA_VERSION` to 4; a side table; expiry in text.** Rejected (D3): re-embedding and lock-out; a join on every recall; unqueryable.
- **Sniff `Tool`-role text.** Rejected: `RetentionMessage` carries no tool name or path (`retain.rs:43-52`).
- **`AppleFmSession::respond` or the `local` provider route.** Rejected: no token cap or cancellation and a hard-wired scoring prompt; or chat-event streaming for a one-shot job (`crates/driver/src/registry.rs:1589-1604`).
- **One enum convar instead of four.** Kept as O8.

## Slices

Each slice is one PR. Rust verification is remote (`just remote-plain-test omp-memory` and a clippy run, `AGENTS.md:90-94`). The prototype is slice 1's starting point; the whole plan, with acceptance detail, is in the research note section 5.

| # | Contents | Gate | Acceptance tests |
|---|---|---|---|
| 1 | Rebase `daab333d13` onto current omp2; add `sv_memory_fact_expiry`, `MnemopiSettings.fact_expiry`, a `BankStore` builder and a `parse_facts` options argument | `sv_memory_fact_expiry` | Gate off: the 12 existing `omp-memory` tests pass unchanged, a six-field line is rejected, an expired row is still recalled. Gate on: the prototype's 15 tests. `SCHEMA_VERSION` stays 3. The old eight-column INSERT works on a migrated table. Two stores opening a pre-column bank at once leave one column. |
| 2 | Rule core, English identity and preference, inline in `none` mode through `RetainedWindow`; `regex` dependency; `sv_memory_llm_mode` | `sv_memory_rule_facts` | Gate off: `disabled_extraction_lane_retains_without_enqueuing` (`session.rs:384`) and all tests unchanged. On with `none`: the upstream vector yields three triples. On with `smol`: no rule facts, job enqueued as today. Replays leave one row per fact. Eviction removes rule facts. Adversarial inputs stay within 32 facts. |
| 3 | `ToolObservation` and parsers for `package.json`, `Cargo.toml`, version banners | `sv_memory_rule_facts` | Golden facts per parser. Manifest text yields no prose facts. Malformed input yields nothing and does not fail retention. Credential-like fields never appear in a fact. |
| 4 | English and Spanish technical, decision, supersession and bound rules; absolute dates; `valid_until` from rules | `sv_memory_rule_facts` (recall filter: `sv_memory_fact_expiry`) | Paired English and Spanish vectors; negatives for questions, negation, hypotheticals, secrets and IP addresses; date constants computed independently (2027-03-15 gives 1805155200000); upstream version regressions. |
| 5 | `LaneFault`, `FallbackPolicy`, bounded worker, extractor field on `ExtractionReport` | none: no caller until slice 6 | With a fake lane: each fault maps as D8; cancellation leaves jobs queued; budgets honoured; replays create no duplicates; `Unavailable` never reaches another lane. |
| 6 | Apple FM lane (`aux_extract.rs`); `local-memory-model` in `sv_memory_llm_mode`; CI recipe for `--features apple-aux-memory` | `sv_memory_local_extraction` and `apple-aux-memory` | Fake generator on Linux: fault table, chunk bounds and reassembly, prompt golden, busy permit never blocks, non-`FACT` lines dropped, `Unavailable` runs rules. macOS-only `#[ignore]` device smoke. |

### The prototype mapped onto the slices

Commit `daab333d13` is +446/-18 in `crates/memory/src/{extract,store}.rs`; its base is `b8032a94ad`, so the rebase onto current omp2 (`f4186b783b` or later) is part of slice 1. BuildBuddy, 2026-11-13: `omp-memory` 12 passed on the base and 27 with the commit; clippy `-p omp-memory --all-targets` exit 0; one later clippy-driven test fix is not re-run.

| Piece of the commit | File | Slice | State |
|---|---|---|---|
| `ExtractedFact.valid_until`, `NewFact.valid_until` (UTC epoch ms, exclusive end) | `extract.rs`, `store.rs` | 1 | In the commit |
| Sixth `FACT` field: five or six fields; RFC 3339 or `YYYY-MM-DD` to the next UTC midnight; empty, `-`, `none`, `null`, `never`, `n/a` mean no expiry; anything else rejects the line | `extract.rs` | 1 | In the commit; needs the gate |
| `facts.valid_until INTEGER` in the DDL and `add_missing_fact_columns` on every open, no version bump | `store.rs` | 1 | In the commit (D3) |
| Nine-column INSERT that writes `valid_until` | `store.rs` | 1 | In the commit |
| `search_facts_at(query, limit, now_ms)` with `(valid_until IS NULL OR valid_until > ?3)` ahead of `LIMIT`; read-by-id unchanged | `store.rs` | 1 | In the commit; filter needs the gate |
| 15 tests: 10 parser and lane tests, 5 store tests (strict boundary, `LIMIT` not starved, read-by-id, migration of an existing table without index reset, legacy-table migration) | both | 1 | In the commit |
| `sv_memory_fact_expiry`, `MnemopiSettings.fact_expiry`, `BankStore` builder, `parse_facts` options | `host_settings.rs`, `config.rs`, `store.rs`, `extract.rs` | 1 | **Missing: no gate.** Slice 1 adds it before any behaviour lands |
| Gate-off identity test, old-INSERT compatibility test, concurrent-open test, rebase | `store.rs`, `extract.rs` | 1 | Missing |
| Rule extractor, vocabulary, English identity and preference, `sv_memory_rule_facts`, `sv_memory_llm_mode` | new `extract/rules/` | 2 | Not in the commit |
| Structured tool observations | new | 3 | Not in the commit |
| Spanish and technical rules, dates | new | 4 | Not in the commit |
| Worker, lane faults, fallback policy | new | 5 | Not in the commit |
| Apple FM lane, `sv_memory_local_extraction`, CI recipe | new | 6 | Not in the commit |

## Consequences

- A user with no model, or with the model off, gets recallable facts in English and Spanish, including versions, dependencies and decisions read from tool results. Both producers can expire facts. The Apple FM lane is bounded (4 KiB chunks, 8 per job, 30 s each) and cannot starve chat. With every gate at its default nothing changes.
- Prohibited: rules calling any model; mutating a fact; tool output reaching a model lane; a JS/TS rule engine; falling back from the local lane to a hosted model.
- Costs accepted: a `regex` dependency in `omp-memory` (a workspace dependency, `Cargo.toml:215`); four convars; a column added without a version bump, which sets the D3 precedent; older binaries ignore expiry; the predicate vocabulary becomes a contract that any model prompt must follow; the 32-fact cap, the confidence bands and the 4 KiB chunk are starting values; Spanish only.
- Slices 2 to 6 are tested at the `omp-memory` boundary, but a user sees nothing until a host composes retention, the worker and a lane (O2).

## Open questions for the owner

- **O1. What "no memory model is set" means.** D1 reads it as `llm_mode = none` or a lane that reports `Unavailable` under an explicit policy, as upstream does. The narrow reading (`none` only) leaves the rules unreachable under the default `smol`. Recommendation: D1.
- **O2. Who wires retention, the worker and a lane?** No code calls settled-turn or shutdown retention, no worker drains the queue, and `llm_mode` has no control (`crates/envd/src/memory.rs:8-9` says lanes live in a driver composition that does not exist). Recommendation: a separate change in the driver composition; this ADR stops at the `omp-memory` boundary.
- **O3. No `SCHEMA_VERSION` bump (D3).** Recommendation: confirm. Alternative: bump to 4 and accept the index reset and the older-binary lock-out.
- **O4. Date-only bounds are UTC.** "Valid through that day" ends at the next UTC midnight. Alternative: local time.
- **O5. Numeric and relative dates are out of v1.** Recommendation: revisit when `RetentionMessage` carries message timestamps (`retain.rs:43-52`).
- **O6. ADR 0049 and a vendor name in a core crate.** The lane follows the `aux_lane.rs` precedent because `omp-ai` cannot depend on `omp-memory` and the driver has no memory composition (`docs/adr/0049-tiered-integration-architecture.md:15,47`). Recommendation: precedent now; move behind a host-injected lane with O2.
- **O7. Tool observations.** No producer exists. Provenance rides the `rule_` id prefix; a `facts.extractor` column is the alternative. Recommendation: the prefix.
- **O8. Gate granularity.** Four convars, or one enum. Recommendation: four, each independently reversible.
- **O9. Apple FM needs device evidence:** guardrail mode (`permissive` stays off), chunk size and token budget (no tokenizer; the backend estimates bytes divided by four, `crates/ai/src/local/applefm/platform.rs:823-833`), and rule coverage for skipped chunks. Measure with `crates/ai/examples/applefm_smoke.rs` before moving a default.
- **O10. Languages.** Spanish only. Upstream's detector also knows Russian and German (`upstream:packages/mnemopi/src/core/beam/consolidate.ts:555-612`); each language needs reviewed vectors.
- **O11. Default `FallbackPolicy`.** `Rules` when `sv_memory_rule_facts` is on, else retry only. Recommendation: confirm.

## Acceptance tests

1. With all four gates at default, behaviour is identical to today: the 12 existing `omp-memory` tests pass; `parse_facts` rejects six fields; `search_facts` does not filter; no rule facts; extraction jobs are enqueued exactly as before.
2. With `sv_memory_fact_expiry` on: `valid_until == now` is expired; expired facts are hidden from recall ahead of `LIMIT`; read-by-id returns them; five-field lines parse unchanged; six-field lines parse; malformed sixth fields reject.
3. Migration: `SCHEMA_VERSION` stays 3; the column is added once, also when two stores open the bank at once; `memory_embeddings` is untouched; the old INSERT and SELECT statements work.
4. With `sv_memory_rule_facts` on and `none`: the upstream English vectors (`upstream:packages/mnemopi/test/extraction.test.ts:93-103,205-227`) and their Spanish equivalents yield the expected triples; with `smol` no rule facts appear; replays are idempotent; eviction removes rule facts.
5. Guards and bounds: questions, negation, hypotheticals, fenced code and secret-bearing clauses yield nothing; adversarial inputs stay within 32 facts.
6. Structured parsers: golden facts; a manifest yields no prose facts; malformed JSON or TOML yields nothing; credential-like fields never reach a fact.
7. English and Spanish technical vectors (versions, dependencies, decisions, supersession, bounds) pass with independently computed date constants, and upstream's version regressions (`upstream:packages/mnemopi/test/beam-consolidate-unit.test.ts:424,442,459,476,490`) pass.
8. Worker with a fake lane: every fault maps as D8; cancellation leaves jobs queued; budgets hold; `Unavailable` never reaches another lane.
9. Apple FM lane with a fake generator: chunk bounds and reassembly, golden prompt, busy permit never blocks, non-`FACT` lines dropped, unavailability runs the rules and no other model.
10. CI builds and tests `omp-memory` with `--features apple-aux-memory`.
11. A macOS device run records chunk token behaviour (manual, `#[ignore]`).
12. Grep checks: no identifier, message or convar in new code uses `pi` as a product name; every new convar is `sv_memory_*`; parsers use typed serde structs (`AGENTS.md:526-527`); no error variant carries a string payload.

## Status in omp

**Status: Not started.** The seam exists unwired: `ExtractionRequest`, `ExtractionLane`, `parse_facts` and `extract_and_store` (`extract.rs:19-136`), the queue (`store.rs:571-599,716-760`), and the retention gate `llm_mode != None` (`session.rs:151,227`). Nothing consumes the queue, implements the lane, calls retention, or controls `llm_mode` (research note 2.1, 2.3). A local, unmerged, ungated prototype of slice 1 exists as `daab333d13` (base `b8032a94ad`). Nothing of slices 2 to 6 exists.

## Upstream origin and license

Draws from `upstream:packages/mnemopi/src/core/extraction.ts:264-315` and `upstream:packages/mnemopi/src/core/beam/consolidate.ts:66-88,675-783`, and ports the vectors in `upstream:packages/mnemopi/test/extraction.test.ts` and `upstream:packages/mnemopi/test/beam-consolidate-unit.test.ts`. The Spanish rules, the structured parsers, the expiry handling, the worker and the Apple FM lane are original work.

License: MIT. `upstream:LICENSE:1-4` (Copyright 2025 Mario Zechner; 2025-2026 Can Bölük), `upstream:packages/mnemopi/LICENSE:1-4` (2025-2026 Can Bölük; 2026 Stencil Labs, Inc.), `upstream:packages/mnemopi/package.json:11`. There is no NOTICE file. omp2 is MIT, Copyright 2026 Stencil Labs, Inc. (`LICENSE:3`), and records ported lineages in `THIRD-PARTY-NOTICES.txt` (`:1498-1512`; ADR 0054 does the same, `docs/adr/0054-shell-builtins.md:106`). The slice that lands the ported patterns adds an `omp-memory` attribution notice naming the upstream files and both copyright lines.

## References

ADR 0006 (policy on the host; the rules execute nothing and read no files); ADR 0023 (tiny local model; fallback is an explicit policy); ADR 0024 (no tool or roster change); ADR 0035 (Rust for the engine, typed errors); ADR 0036 (Python extensions); ADR 0042 (extends the Apple FM boundary); ADR 0049 (vendor names leave core crates); ADR 0054 (gate and notice precedent); `docs/research/mnemopi-fact-extraction.md`; `AGENTS.md:526-527,553-557`.
