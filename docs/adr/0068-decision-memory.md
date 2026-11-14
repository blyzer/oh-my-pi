# 0068. Decision memory: friction-gated project decision files as a separate subsystem; Hindsight stays an extension

Status: Proposed 2026-11-14; EGM rounds 1-2 resolved, round 3 pending
Date: 2026-11-13 (revised 2026-11-14)
Area: memory

Research: [docs/research/memory-backends.md](../research/memory-backends.md) (Gap 14 of the upstream gap closure). Upstream pin `c308970263` (v18.20.0); omp2 base `origin/omp2 4819cbb36c`. Citations: `upstream:` is the OMPv1 pin; bare paths are omp2. This record states decisions; the note holds the evidence. Nothing was built or run for this revision; every omp2 claim was read in the tree.

Terms. *EGM*: the Elephant-Goldfish design review (fresh-context reviewers). *Mnemopi*: omp2's default-off SQLite memory backend (`omp-memory`). *Convar*: a typed setting (ADR 0012); `ARCHIVE` persists it, `PROJECT` lets a repository's cfg set it. *`<meta>`*: the session DOM's metadata node, where `prompt-facts` is journaled. *`@memory`*: the catalog role selector that resolves the configured memory model, then `@commit`, then `@smol` (`crates/driver/src/headless/reflection.rs:20-23`). *Native hook host*: an in-process `NativeHookHost` attached to the session's hook gate (`crates/contract/src/hooks.rs:769-785`). *P5*: the prompt prefix-stability e2e proof (`crates/e2e/tests/p5_prefix_stability.rs`). *Session secret rules*: the user, project (trusted only), extension and environment secret declarations a session seals (`crates/driver/src/secrets/session.rs:20-60`).

## Context

### What OMPv1 does

- Sharpshooter is a project decision memory earned by friction, not by decision existence. Per-prompt extraction queues deltas; consolidation applies them into `architecture.md`, `product.md` and `style.md`, each capped at 120 lines (`upstream:packages/coding-agent/src/sharpshooter/types.ts:1-8`, `upstream:packages/coding-agent/src/sharpshooter/types.ts:59-65`). Files live home-scoped under `<agentDir>/memories/sharpshooter/<bank>/`, never in the project tree (`upstream:packages/coding-agent/src/sharpshooter/paths.ts:1-11`). The bank id is `<basename>-<hash>` of the working directory (`upstream:packages/coding-agent/src/sharpshooter/paths.ts:23-26`, `upstream:packages/coding-agent/src/mnemopi/config.ts:207-216`).
- A delta has a kind, a timeless statement, an optional rejected alternative and rationale, a source (`explicit_user`, or `contextual_resolution`: a short reply resolved against the assistant's question), an `evidence` string and three friction flags (`upstream:packages/coding-agent/src/sharpshooter/types.ts:10-57`).
- Extraction runs in the background, one at a time per session; a prompt arriving while one runs is not extracted (`upstream:packages/coding-agent/src/sharpshooter/extract.ts:185-223`, `:196`). Slash commands and prompts under 16 characters are skipped (`:199-207`). The envelope is the current prompt, the previous human message cut to 400 characters and the last assistant message cut to 800, fenced code omitted (`:80-84`, `:103-110`, `:140-162`). The request uses `@smol` unless `sharpshooter.model` is set (`upstream:packages/coding-agent/src/sharpshooter/settings.ts:7-16`, `extract.ts:165-183`), forces `record_deltas`, 2,048 output tokens, low reasoning, transient retry (`extract.ts:238-257`). A delta whose evidence is not a substring of the current prompt is dropped (`extract.ts:288`). Session disposal waits up to 5 s for an in-flight extraction (`extract.ts:62-78`).
- Consolidation runs from a one-minute scheduler once `intervalMinutes` (default 5) have passed (`upstream:packages/coding-agent/src/sharpshooter/scheduler.ts:11`, `:40-41`). After failures it waits `max(1 min, interval) × 2^(failures-1)`, at most 6 h (`upstream:packages/coding-agent/src/sharpshooter/consolidate.ts:47-49`, `:131-146`). Its input is the three files, a digest of the project's docs and the queued deltas (`upstream:packages/coding-agent/src/prompts/memories/sharpshooter-consolidate-input.md:1-25`). It forces `replace_memory_files` with medium reasoning (`consolidate.ts:37`, `:51-60`). The *admission law* in its system prompt admits a decision only when its lineage shows a regression, a subtlety not evident from code, or repetition; it excludes paths, line numbers and task state (`upstream:packages/coding-agent/src/prompts/memories/sharpshooter-consolidate-system.md:5-43`). That gate is model-judged; no code reads the friction flags. Validation rejects a file over 120 lines after redaction and an all-empty reply when any current file is non-empty (`consolidate.ts:317-343`). Files are renamed one after another, so a crash can leave a mixed set (`consolidate.ts:351-362`).
- Injection joins the populated files under "follow them unless the user overrides", cut to 15,000 tokens (`upstream:packages/coding-agent/src/sharpshooter/backend.ts:119-128`).
- Redaction covers file writes only: the envelope goes out unredacted (`extract.ts:238-246`) and deltas are queued unredacted (`extract.ts:270-273`); the consolidator redacts each file before the cap check (`consolidate.ts:330-338`) with patterns for AWS, GitHub, npm, Slack and Google tokens, keyword-led runs and JWTs (`upstream:packages/coding-agent/src/memory-backend/redact.ts:11-149`).
- Hindsight is a client for an external memory server (`upstream:docs/memory.md:134-142`).

### What omp2 has

- `omp-memory` (Mnemopi) has a closed `Off | Mnemopi` selector (`crates/memory/src/config.rs:25-31`) and the convar `ai_memory_backend` (`crates/envd/src/host_settings.rs:74-87`). With `Off` no bank scope is resolved (`crates/memory/src/runtime.rs:265-274`) and envd takes no repository snapshot (`crates/envd/src/server/session.rs:45-54`). The memory-tab convars all use the `ai_` prefix (`crates/envd/src/host_settings.rs:74`, `:89`, `:107`).
- Repository identity: `omp_vcs::detect` opens a repository and `primary_root()` returns the primary checkout root, shared by linked worktrees, without a status walk (`crates/envd/src/vcs/mod.rs:52-65`, `crates/vcs/src/git/mod.rs:99-115`). The driver depends on `omp-vcs` (`crates/driver/Cargo.toml:74`).
- **Nothing writes Mnemopi's `memory` prompt property.** The prompt reads `memory.{memory,standing,recall}` from the `prompt-facts` JSON (`crates/agent/src/prompt/mod.rs:99-106`, `:139-149`, `crates/agent/src/prompt/projection.rs:15-31`). `install_prompt_facts` (`crates/driver/src/headless/kernel.rs:4043-4129`) is the only production writer; it has three production callers, `compose_kernel` (`:2921`), `SessionHome::create` (`:3417`) and `SessionHome::open` (`:3436`), and inserts no `memory` key. `SessionHome` serves `/new`, `/resume`, `/fork` and `/drop` (`:3213-3218`).
- **Hook seams.** `before_agent_start` is admitted once per user submission, before the turn, with the submission `text` (`crates/agent/src/loop.rs:1076-1091`). A native host attaches with `HookGate::attach_native` (`crates/contract/src/hooks.rs:1282-1290`), as the plugin hook host does in `compose_kernel` (`crates/driver/src/headless/kernel.rs:3079-3098`). `session_switched` reaches every host on a switch and must not block (`crates/contract/src/hooks.rs:778-784`). `session_shutdown` has a 1.5 s budget (`crates/contract/src/hooks.rs:569-571`).
- **Directors are the wrong seam for extraction.** Director inference is borrowed inside an awaited `before_inference` whose error fails the turn (`crates/agent/src/loop.rs:1592-1602`); the kernel drops the future on interrupt (`crates/contract/src/director.rs:671-675`); ADR 0015 says behaviour that only observes one turn MUST NOT be a Director (`docs/adr/0015-directors.md:87-89`).
- **Driver-owned auxiliary inference exists.** `compose_kernel` binds memory reflection to `inference.auxiliary_inference()` (`crates/driver/src/headless/kernel.rs:2841-2845`, `:2233`); the host clones it per request and calls `chat_on("@memory", …)` (`crates/driver/src/headless/reflection.rs:45-69`). Requests carry a named-tool intent (`crates/ai/src/call.rs:1023`) and forced-call ladder state (`:1044`).
- **`/memory` has five operations**, all Mnemopi: `view`, `stats`, `diagnose`, `clear`, `enqueue` (`crates/chat/src/overlays/services.rs:776-790`, `crates/chat/src/commands/misc.rs:419-430`, `crates/app/src/chat_services/misc.rs:105-149`). `/memory clear` has no confirmation (`misc.rs:140-143`). No `/decisions` command exists.
- **`omp-secrets`.** `SecretRule` kinds are `Plain | Regex` (`crates/secrets/src/rule.rs:10-18`). `SecretRedactor::new` lowers every rule to `Replace` with no replacement (`crates/secrets/src/redact.rs:24-37`, `crates/secrets/src/rule.rs:129-136`), and a `Replace` rule without a replacement emits a derived look-alike, not a fixed marker (`crates/secrets/src/obfuscator.rs:253-263`). Built-in rules cover GitHub, GitLab, OpenAI and Anthropic only (`crates/secrets/src/builtins.rs:7-29`). The session snapshot has a generation (`crates/driver/src/secrets/session.rs:20-60`). Provider-bound obfuscation (`crates/ai/src/layer/secrets.rs`) has no production caller (grep for `obfuscate_provider_messages`).
- **Other learning paths.** Auto-Learn nudges a capture turn at stop and writes model-generated managed skills (`crates/envd/src/host_settings.rs:106-128`, `crates/contract/src/managed_skills.rs:1-9`); skills are listed and loaded on demand, not injected as standing text. Mnemopi has an `ExtractionLane` for facts (`crates/memory/src/extract.rs:30-35`), inactive when its LLM mode is none.
- Extension prompt contributions have no consumer outside envd (`crates/envd/src/lib.rs:1476-1478`). The v1 importer refuses `sharpshooter` and `hindsight` (`crates/driver/src/legacy_settings.rs:446-449`).

### Rules that bind

- Additive only: `ai_memory_backend`, `/memory`, `MemoryOp`, the `SlotId` catalog, `credential_rules()` and the `before_agent_start` payload do not change.
- ADR 0015: a hook that observes one turn is not a Director. ADR 0049: vendors are extensions; new capabilities default to T2 and T1 needs an evidenced admission reason (`docs/adr/0049-tiered-integration-architecture.md:47`, `:53`). ADR 0012: no `PROJECT` flag on a convar a repository could abuse (`docs/adr/0012-convars.md:77`). ADR 0019: forced calls are caller intents with an escalation ladder. ADR 0035: typed `thiserror` errors. ADR 0036: extensions are Python.
- Deterministic first: identity, skipping, the evidence check, redaction, caps, validation, atomic commit and acceptance are code; extraction and the admission law are model-judged.

## Decision

**D1. A separate subsystem.** Decision memory runs beside Mnemopi and works whether Mnemopi is off or on. It MUST NOT touch `ai_memory_backend`, `MemoryBackend`, `omp-memory` or envd. It is not a managed-skill kind: decisions are short, always-injected, user-accepted project rules, while managed skills are model-written procedures loaded on demand. It is not an `ExtractionLane` consumer: that lane extracts recall facts for a backend that is off by default.

**D2. Project identity.** At composition the driver opens the repository at the canonical working directory with `omp_vcs::detect` and takes `primary_root()`; no status walk. It accepts that root only when the working directory lies inside it, or when the root's Git common directory lists this checkout as a linked worktree; otherwise (for example a `.git` file pointing at another repository) it uses the checkout root. With no repository it uses the canonical working directory. The bank is `<data>/decisions/<hex>/`, `<hex>` the 64-digit `Hash32` hex of the identity root. A failure to resolve identity or open the bank logs a warning and disables decision memory for that session; it never fails composition.

**D3. Bank layout and atomic commit.**
- `generations/<n>/{architecture,product,style}.md`: complete file sets, never edited in place.
- `queue/<id>.json`: one redacted delta per file, `id` a ULID.
- `state.json`: `{ "v": 1, "latest": n|null, "accepted": n|null, "consumed": [ids], "last_ok_ms", "failures", "last_error_ms", "contributors": { "<n>": [ids] } }`.
- `lock`: held with `std::fs::File::try_lock` for a consolidation pass, as the journal does (`crates/journal/src/lib.rs:507`); the OS releases it if the process dies.
- Commit order: write and fsync generation `n+1`; replace `state.json` by write-temp-and-rename with `latest = n+1` and the consumed ids; then unlink those queue files. On open, queue files listed in `consumed` are unlinked without re-sending. A crash before the rename leaves the previous generation current and the queue intact; a crash after it re-sends nothing.
- Delta JSON is omp2's own schema, snake_case, with `v: 1` and `ts_ms` (u64). It is not upstream's file format.

**D4. Redaction.** `omp-secrets` gains, additively: `memory_redaction_rules()` (`Replace` rules with the fixed replacement `[REDACTED]` for the built-in token shapes and upstream's AWS, npm, Slack, Google and JWT shapes; the regex engine is linear-time, `crates/secrets/src/rule.rs:16-17`), a linear `redact_keyword_runs` scanner ported from `upstream:packages/coding-agent/src/memory-backend/redact.ts:22-106`, `SecretRule::into_redaction(replacement)` (a public sibling of the crate-private `into_irreversible`), and a `MemoryRedactor` that runs keyword runs, then the memory rules, then the session's secret rules lowered with `into_redaction("[REDACTED]")`. `credential_rules()` and `SecretRedactor` do not change. The driver's decision worker owns one `MemoryRedactor` and rebuilds it when the session snapshot's generation changes. It is applied at three boundaries:
- R1: the extraction envelope, before it leaves the process. The evidence check runs against the redacted prompt.
- R3: each file of a consolidation reply, before the line-cap check.
- R4: every read of generation files (consolidation input, injection, `/decisions show`), because files can be edited by hand. Redact first, then cut.

Deltas are not redacted again: they are built from redacted text. Redaction does not cover personal data, paths or a secret described in prose, and does not protect the raw session journal or the main provider request. User docs MUST say so and MUST name the `@memory` route as an extra egress.

**D5. Extraction: a native hook host feeding a driver worker.**
- Seam. `compose_kernel` attaches a `DecisionHost` with `admission_gate.attach_native(host, &[HookEventId::HookEventBeforeAgentStart])`, beside the plugin hook host (`crates/driver/src/headless/kernel.rs:3079-3098`). Its `decide` reads `text`, applies the skip rules, `try_send`s `(text, previous_prompt)` into a `flume::bounded(1)` channel and returns `NativeReply::defer()` without awaiting. A full channel drops the prompt (upstream parity). It never edits the payload or adds context.
- Runner. A `DecisionWorker` task spawned by `compose_kernel` owns a clone of `inference.auxiliary_inference()`, the store and the redactor. Each extraction is one isolated `chat_on("@memory", request)`, as reflection does. The worker is cancelled by the session's cancellation, not by turn interrupts; an in-flight call at session end is abandoned (no flush; upstream waits 5 s).
- When. Only when `ai_decision_memory` is on, tools are enabled, the session is not ephemeral and `kernel_agent(options.agent, dom) == MAIN_AGENT` (`crates/driver/src/headless/kernel.rs:3984-3990`, `crates/driver/src/subagent/mod.rs:22`). Skipped: text whose first non-blank character is `/`, text under `ai_decision_memory_min_prompt` graphemes (default 16, counted with `xutf`), and every prompt after `ai_decision_memory_max_extractions` (default 40) in one session.
- Envelope. The current prompt, its first 4,000 characters (Unicode scalar values), and the previous prompt the host saw, its first 400; fenced code replaced by `[code omitted]`; then R1. There is no assistant context, so `contextual_resolution` deltas are not produced in the first cut.
- Request. Static prompt files ported from `sharpshooter-extract-system.md` and `sharpshooter-extract-input.md`; a `record_deltas` tool schema in `omp-decisions`; a named-tool intent escalated per ADR 0019; 2,048 output tokens; low reasoning; one transient retry. A reply without the call is a failure.
- Admission. A delta is queued only when its kind and source are valid, its statement is non-empty and its evidence is at least 8 characters and an exact substring of the redacted prompt.
- A failure is logged at debug level and counted in `/decisions status`; it never fails the turn and never reaches the transcript. Each call records a tracing span with token usage.

**D6. Consolidation: the same worker.**
- Triggers. `/decisions sync` runs a pass now, bypassing interval and backoff. An automatic pass runs at worker start and on `session_switched` when the queue is non-empty and `now ≥ max(last_ok + interval, last_error + backoff)`, with `interval = ai_decision_memory_interval` (default 5 min) and `backoff = max(1 min, interval) × 2^(failures-1)`, at most 6 h (upstream `consolidate.ts:131-146`). Nothing runs on `session_shutdown`.
- Lock busy: `/decisions sync` reports busy; an automatic pass is skipped.
- Input. The latest generation's files (R4), queued deltas oldest first up to 64 KiB (the rest waits for the next pass), and, only in a trusted workspace, the context files already in `prompt-facts`, cut to 8 KiB, labelled "already documented, exclude".
- Request. Static prompt files ported from `sharpshooter-consolidate-system.md` and `sharpshooter-consolidate-input.md`; a `replace_memory_files` tool; medium reasoning; output tokens `min(model maximum, 8,192)`.
- Validation. Exactly one entry per file (stricter than upstream, which accepts a subset); R3; at most 120 lines per file; an all-empty reply is rejected when any current file is non-empty. Any failure records `failures` and `last_error_ms` and changes nothing else.

**D7. Acceptance before injection.** A pass produces `latest`; injection uses `accepted`. `/decisions accept` sets `accepted = latest`. `/decisions show` shows the accepted files, the diff to `latest`, and for `latest` the contributing deltas' evidence and session ids (`contributors`). `ai_decision_memory_auto_accept` (default false) accepts every pass. This puts the user, not only a model, between a prompt and directive text.

**D8. Injection.**
- Producer. `install_prompt_facts` gains a `decisions: Option<&str>` parameter and writes a top-level `decisions` key only when it is `Some`. Its three production callers pass the store's injection text when the gate is on and the agent is `MAIN_AGENT`; `SessionHome` carries an `Option<Arc<DecisionStore>>` for its two. Every other case passes `None`, leaving `prompt-facts` and the prompt byte for byte as today. It writes no `memory` key.
- Capture. The text is read whenever `install_prompt_facts` runs (composition, `/new`, `/resume`, `/fork`), never between, so the Dynamic band is fixed for a session's life; an acceptance reaches the next composition or switch.
- Projection. `CanonicalPromptSource` renders `decisions` first in the Dynamic band, before Mnemopi's `memory` and `standing`. No new `SlotId`.
- Framing. `<project-decisions>` with a header from a static prompt file: decisions the user accepted in earlier sessions of this project, not long-term memory; follow them unless the user overrides them in this conversation; system and tool policy win. Cut at `ai_decision_memory_tokens` (default 4,000, range 256-16,000; bytes = tokens × 4 as Mnemopi does, `crates/memory/src/runtime.rs:565`) at a UTF-8 boundary with a marker.

**D9. Controls.** A new `/decisions [show|accept|sync|clear|status]` command, `show` by default: a strum `DecisionsOp` enum beside `MemoryOp`, a `Services::decisions(op)` method, output in a `ReportPanel` as `/memory` does. `clear` removes only this project's bank, without confirmation, as `/memory clear` does. `status` reports the identity root, generations, queue depth, failures, next automatic pass, extractions this session and the resolved `@memory` model. `/memory` does not change.

**D10. Gates and settings.** Declared with `omp_con::var!` in a new `crates/driver/src/decisions.rs`, `"ui.tab": "memory"`, `"ui.group": "Decision memory"`, all `ARCHIVE` and none `PROJECT` (a repository must not switch on directive injection or prompt egress): `ai_decision_memory` (bool, default false, "Decision memory (experimental)"), `ai_decision_memory_tokens`, `ai_decision_memory_min_prompt`, `ai_decision_memory_max_extractions`, `ai_decision_memory_interval`, `ai_decision_memory_auto_accept`. No `cap-*` feature (O14). No `legacy.path`; v1 decision files are not imported, and the docs explain copying a v1 bank's three files into a generation by hand.

**D11. Code home.** A new crate `crates/decisions` (`omp-decisions`): types, layout, `state.json`, queue, lock, reply validation, injection text, prompt files and tool schemas. Dependencies: `omp-core`, `omp-secrets`, `serde`, `serde_json`, `thiserror`, `xutf`. It reads no settings and probes no Git. The driver owns identity, the host, the worker and the convars; chat and app own the command. No envd change, no new tool, no roster change (ADR 0024).

**D12. Hindsight is an extension only** (ADR 0049); nothing in core. Whether its prompt slot can reach the prompt is O8.

## Rejected options

| Option | Why not |
|---|---|
| A third `ai_memory_backend` value | Changes an existing convar and makes the two subsystems exclusive. |
| A Director for extraction | ADR 0015:87-89; its inference runs inline in the turn and dies on interrupt (`crates/agent/src/loop.rs:1592-1602`, `crates/contract/src/director.rs:671-675`). |
| Consolidating on `session_shutdown` | 1.5 s budget (`crates/contract/src/hooks.rs:571`); a model pass does not fit. |
| Identity from Mnemopi's `BankScope` or a full `vcs::snapshot` | The first exists only with Mnemopi on; the second walks worktree status at every start. |
| The `project_state` bucket | Keyed by working directory, so worktrees would not share (`crates/env/src/project_state.rs:83-89`). |
| Files in the project tree | A repository could plant directive text. |
| Controls under `/memory` | Edits an existing command whose `clear` and `enqueue` mean Mnemopi. |
| `memory.decisions` inside Mnemopi's map | Couples two producers in one property. |
| Redaction grammar inside `omp-decisions` | `omp-secrets` owns credential grammar; an opt-in set there changes no caller. |
| Changing `credential_rules()` or `SecretRedactor` | Changes export and capture output. |
| Upstream's assistant-context envelope | Lets a short reply promote assistant text that repository content can steer. |
| A pending buffer for prompts during extraction | Not upstream behaviour and no evidence it is needed. |
| Python extension for decision files | The `omp.*` API has no prompt-facts producer or auxiliary inference handle, and extension prompt slots do not reach the prompt (`crates/envd/src/lib.rs:1476-1478`). |
| Background timer | Triggers are composition, switch and command. |
| `AGENTS.md` only | Loses the automatic part. |

## Slices

One PR each; tests first, not edited to pass. Every slice leaves `just lint`, `just test` and `just check-minimal-caps` green; with `ai_decision_memory` off the existing prompt goldens, `/memory` tests and driver composition tests pass unchanged.

**S0. Gate and prompt producer.** Convars of D10; the `decisions` parameter of `install_prompt_facts` and its three callers (passing `None` until S3); `CanonicalPromptSource` renders `decisions`.
- Tests: `crates/agent/tests/prompt_golden.rs` `decisions_render_once_first_in_the_dynamic_band`, `no_decisions_key_leaves_the_prompt_unchanged_byte_for_byte`; driver `prompt_facts_without_decisions_have_no_decisions_key`, `subagent_composition_passes_no_decisions`.
- Verification: `just test-pkg omp-agent`, `just test-pkg omp-driver`.

**S1. Memory redaction in `omp-secrets`.** D4's additive API.
- Tests: `memory_rules_replace_every_shape_with_the_fixed_marker` (AWS, npm, Slack, Google, JWT, GitHub, GitLab, OpenAI, Anthropic), `keyword_run_is_redacted_and_authentication_is_not`, `keyword_scanner_is_linear_on_a_long_run`, `session_rule_lowered_with_into_redaction_emits_the_marker`, `credential_rules_and_secret_redactor_output_unchanged`.
- Verification: `just test-pkg omp-secrets`.

**S2. `omp-decisions` store.** D3, D5 admission, D6 validation, D8 injection text; README states the tier reasoning (O14) and NOTICE the upstream origin.
- Tests: `delta_round_trips_in_the_v1_schema`, `crash_before_state_rename_leaves_previous_generation_and_queue`, `crash_after_state_rename_resends_no_consumed_delta`, `second_lock_holder_is_refused`, `reply_missing_or_duplicating_a_file_is_rejected`, `file_over_120_lines_after_redaction_is_rejected`, `all_empty_reply_with_a_non_empty_current_file_is_rejected`, `evidence_shorter_than_8_or_not_a_substring_is_rejected`, `injection_redacts_then_cuts_at_a_utf8_boundary`, `consolidation_input_is_capped_at_64_kib_oldest_first`.
- Gate: none; no caller until S3.

**S3. Identity, read path, `/decisions show|accept|status`.** D2; the driver opens the store and feeds S0; `SessionHome` carries it; the command.
- Tests: driver `identity_is_shared_by_linked_worktrees`, `foreign_gitdir_falls_back_to_the_checkout_root`, `no_repository_uses_the_canonical_cwd`, `identity_failure_disables_decisions_without_failing_composition`, `accepted_generation_reaches_prompt_facts_redacted`, `unaccepted_latest_is_not_injected`, `session_home_create_and_open_inject_decisions`; app `decisions_show_reports_accepted_latest_and_contributors`, `memory_view_output_is_unchanged`; e2e `p5_decisions_block_keeps_the_prefix_stable_across_turns`.
- Golden journal: one new family for `prompt-facts` with `decisions` (append-only corpus).

**S4. Extraction host and worker.** D5.
- Tests with a fake auxiliary inference: `before_agent_start_returns_defer_without_awaiting_the_model`, `valid_delta_lands_in_the_queue`, `invented_or_short_evidence_is_dropped`, `envelope_secret_reaches_the_model_as_the_marker`, `slash_and_short_prompts_are_skipped`, `prompt_during_extraction_is_dropped`, `extraction_cap_stops_after_the_limit`, `subagent_and_ephemeral_sessions_attach_no_host`, `extraction_failure_never_fails_the_turn`, `turn_interrupt_does_not_cancel_extraction`; a golden of the extraction prompt.

**S5. Consolidation and `/decisions sync|clear`.** D6.
- Tests: `sync_bypasses_interval_and_backoff`, `automatic_pass_waits_for_interval_and_backoff`, `valid_reply_commits_a_new_latest_generation`, `invalid_reply_changes_only_failure_state`, `reply_secret_is_redacted_before_the_cap_check`, `docs_digest_is_sent_only_in_a_trusted_workspace`, `busy_lock_reports_busy`, `decisions_clear_removes_only_this_bank`, `memory_clear_leaves_the_decision_bank`; a golden of the consolidation prompts.

## Upstream origin and license

Drawn from `upstream:packages/coding-agent/src/sharpshooter/`, the prompts `upstream:packages/coding-agent/src/prompts/memories/sharpshooter-extract-system.md`, `sharpshooter-extract-input.md`, `sharpshooter-consolidate-system.md` and `sharpshooter-consolidate-input.md`, and `upstream:packages/coding-agent/src/memory-backend/redact.ts`. MIT (`upstream:LICENSE:1-5`; copyright Mario Zechner, Can Bölük, Stencil Labs, Inc.). The notice goes in the NOTICE files of `omp-decisions` (prompts) and `omp-secrets` (redaction logic).

## Consequences

- Projects accumulate a short set of user-accepted decisions, shared across worktrees, followed unless the user overrides.
- Each eligible prompt costs one `@memory` call, capped per session; prompts leave the machine to that route, redacted first.
- A directive block is a new trust stance; acceptance and framing keep it apart from Mnemopi's non-directive text.
- Decision text is journaled in `prompt-facts`, so exports and session files contain it, redacted.
- Accepted changes reach the prompt at the next composition or session switch.

## Open questions for the owner

- **O1. Directive wording?** Recommendation: directive, framed, subordinate to policy and the current user.
- **O2. Convar or enum value?** Recommendation: the convar.
- **O3. New crate?** Recommendation: `omp-decisions` (D11).
- **O4. `@memory` or a dedicated setting (upstream defaults to `smol`)?** Recommendation: `@memory`.
- **O5. Triggers?** Recommendation: D6; a scheduler only on demand.
- **O6. Drop or hold prompts during an extraction?** Recommendation: drop (upstream).
- **O7. Token limit?** Recommendation: 4,000.
- **O8. Hindsight seams?** An extension prompt slot has no consumer today. Recommendation: settle in a separate Hindsight record.
- **O9. Write Mnemopi's snapshot into `memory`?** That turns on injection for every Mnemopi user. Recommendation: a separate decision.
- **O10. Own `SlotId`?** Recommendation: no.
- **O11. Controls under `/memory`?** Recommendation: `/decisions`.
- **O12. Inject into subagents?** Recommendation: not in the first cut.
- **O13. Promote memory rules into the export and capture redactors?** Recommendation: separately.
- **O14. ADR 0049 tier?** Not a vendor integration; T2 fails (see rejected Python extension); no T1 admission reason applies cleanly. Recommendation: core and uncapped like `omp-memory` (`crates/app/Cargo.toml:97`), recorded in the README.
- **O15. Acceptance default?** Recommendation: `ai_decision_memory_auto_accept = false`.
- **O16. Command-injected prompts.** Text a command submits (for example `/security review`, `crates/chat/src/commands/misc.rs:409-412`) arrives at `before_agent_start` like typed text, `source: "interactive"` (`crates/agent/src/loop.rs:1084`). Recommendation: accept in the first cut (the evidence gate and acceptance bound the effect); a source field would change the payload.
- **O17. Wire provider-bound obfuscation for auxiliary requests?** Recommendation: a separate change.

## What the review changed

### Round 1 (EGM, 2026-11-13; resolved 2026-11-14)

Four findings, all confirmed; none rejected. Round 2 later revised some of these resolutions (see below).

- **S0 producer and slot design.** Confirmed: nothing writes `memory` (`crates/driver/src/headless/kernel.rs:4043-4129`; reader `crates/agent/src/prompt/mod.rs:139-149`). Changed: named the producer, band and capture point; no new `SlotId`; S0 became gate plus producer.
- **Project identity when Mnemopi is off.** Confirmed (`crates/memory/src/runtime.rs:265-274`, `crates/envd/src/server/session.rs:45-54`). Changed: identity from the primary Git root, bank `<data>/decisions/<hex>/`.
- **`/memory clear` and `/memory view` collisions.** Confirmed (`crates/chat/src/commands/misc.rs:419-430`, `crates/app/src/chat_services/misc.rs:140-147`). Changed: a new `/decisions` command; `/memory` unchanged.
- **Redaction scope.** Confirmed (`crates/secrets/src/builtins.rs:7-29`; `upstream:packages/coding-agent/src/sharpshooter/consolidate.ts:330-338`). Changed: redaction at named boundaries.

### Round 2 (EGM, 2026-11-14)

Four fresh reviewers and a synthesizer: 1 blocker, 15 major, 18 minor. Each was checked in source.

| Id | Confirmed in source | Change, or reason not taken |
|---|---|---|
| B1 Director cannot run extraction | Yes: `crates/agent/src/loop.rs:1592-1602`, `crates/contract/src/director.rs:671-675`, `docs/adr/0015-directors.md:87-89` | D5: native hook host on `before_agent_start` (`crates/agent/src/loop.rs:1076-1091`, attached via `crates/contract/src/hooks.rs:1282`) feeding a driver `DecisionWorker` on `auxiliary_inference()` (`crates/driver/src/headless/kernel.rs:2233`, `:2841-2845`). No Director. |
| M1 No consolidation runner; shutdown seam too short | Yes: `crates/contract/src/hooks.rs:569-571`, `crates/driver/src/plugin_hooks.rs:35-36` | D6: same worker; triggers are `/decisions sync`, worker start and `session_switched`; never shutdown; main, non-ephemeral, tools-enabled only. |
| M2 Backoff without interval | Yes: `upstream:…/consolidate.ts:131-146`, `scheduler.ts:11` | D6: interval convar (5 min), upstream backoff formula, sync bypasses both. |
| M3 Output is not `[REDACTED]`; scanners are not rules | Yes: `crates/secrets/src/redact.rs:24-37`, `rule.rs:129-136`, `obfuscator.rs:253-263`, `rule.rs:10-18` | D4: explicit `[REDACTED]` replacement rules, a procedural scanner, a defined pass order in `MemoryRedactor`, owned by the worker. |
| M4 Grammar belongs in `omp-secrets` | Yes: `crates/secrets/src/builtins.rs:16-29` | D4 and S1: opt-in API in `omp-secrets`; rejected-options row updated. |
| M5 R1/R2 protect little | Partly: provider obfuscation unwired (`crates/ai/src/layer/secrets.rs`, no caller) | R2 dropped (deltas come from redacted text). R1 kept: `@memory` can be a different provider, and the cost is small. Limits stated in D4; O17 added. |
| M6 Where the redactor is built | Yes: `crates/driver/src/secrets/session.rs:20-60` | D4: the driver worker builds it from the session snapshot and rebuilds on generation change. The exact composition point holding the snapshot is not yet located (UNVERIFIED). |
| M7 Consolidation input unspecified | Yes: `upstream:…/sharpshooter-consolidate-input.md:1-25` | D6: files (R4), deltas capped at 64 KiB, docs digest only in trusted workspaces. |
| M8 Prompts and request shape missing | Yes: `upstream:…/extract.ts:238-257`; prompt files listed at the pin | D5, D6, Upstream origin: four prompt files, tool schemas in `omp-decisions`, tokens, reasoning, retry, ADR 0019 intent (`crates/ai/src/call.rs:1023`, `:1044`). |
| M9 No atomic set commit | Yes: `upstream:…/consolidate.ts:351-362` | D3: generation directories, state swap with consumed ids, `File::try_lock`; tests renamed to the real property. |
| M10 Prompt-injection channel into directive text | Yes: `upstream:…/extract.ts:151-153`, `:288`, `types.ts:19-23` | D7 acceptance; D5 drops assistant context (no `contextual_resolution`) and requires 8-character evidence; provenance in `show`. |
| M11 No cost model | Yes: `upstream:…/extract.ts:137` | D5: 4,000-character envelope, per-session cap, tracing usage, `status` names the route; D4 docs name the egress. |
| M12 Missing call sites; "main agent"; capture | Yes: `crates/driver/src/headless/kernel.rs:2921`, `:3417`, `:3436`, `:3984-3990` | D8: parameter on all three, `SessionHome` carries the store, predicate `kernel_agent == MAIN_AGENT`, capture redefined per composition and switch. |
| M13 envd wiring | Yes: `crates/envd/src/server/session.rs:39-60` | D1, D11: no envd change; the driver owns the store and has `data_dir` (`crates/driver/src/headless/kernel.rs:3090`); identity via `omp_vcs::detect` without a status walk; failure degrades. |
| M14 Third learning pipeline | Yes: `crates/envd/src/host_settings.rs:106-128`, `crates/memory/src/extract.rs:30-35` | D1 states why neither existing path fits. |
| M15 ADR 0049 misread | Yes: `docs/adr/0049-tiered-integration-architecture.md:47`, `:53` | D10 no longer cites ADR 0049 for build weight; O14 records the tier question. |
| m1 Pending buffer | Yes: `upstream:…/extract.ts:196` | Dropped; drop on full channel. |
| m2 Upstream deviations unstated | Yes | Context lists scheduler, `smol` default and drop. |
| m3 Empty-reply rule | Yes: `upstream:…/consolidate.ts:317-343` | D6 wording fixed; full-set rule flagged as stricter. |
| m4 "Admission law" undefined | Yes | Defined in Context; model-judged parts named in Rules that bind. |
| m5 STATUS.md disagrees | Yes: `docs/adr/STATUS.md:80` | Not changed here: the ledger is owned by the orchestrator, who updates it after the review. |
| m6 Undefined terms; "standing" overloaded | Yes | Terms paragraph added; "standing Director" gone. |
| m7 `memory` map sharing; framing | Yes: `crates/agent/src/prompt/mod.rs:139-149` | D8: top-level `decisions` key, rendered first; header says it is not long-term memory. |
| m8 Prefix, UI metadata, deps | Yes: `crates/envd/src/host_settings.rs:74`, `:89`, `:107` | D10 uses `ai_` names, `ui.group`, `ui.label`; D11 lists deps. |
| m9 Overlapping subcommands | Yes | `queue` folded into `status`; `accept` added. |
| m10 Command plumbing; confirmation | Yes: `crates/chat/src/overlays/services.rs:776-790`, `crates/app/src/chat_services/misc.rs:140-143` | D9 names `DecisionsOp`, `Services::decisions`, `ReportPanel`; confirmation dropped to match `/memory clear`. |
| m11 Hindsight scope creep | Yes | D12 is one line; steps removed; O8 kept. |
| m12 Implementation detail in decisions | Partly | `identity.json` removed. Layout and schema stay in D3 because the crash tests depend on them. |
| m13 Crafted `.git` aliasing | Yes: `crates/vcs/src/git/mod.rs:99-115` | D2 accepts the primary root only when the cwd is inside it or a verified linked worktree. |
| m14 v1 files abandoned | Yes: `crates/driver/src/legacy_settings.rs:446-449` | D10: not imported; docs explain a manual copy. |
| m15 Slash, canned prompts, counting | Partly: `crates/agent/src/loop.rs:1084` carries no source distinction | Counting units defined in D5; previous prompt is the host's own; canned prompts recorded as O16. |
| m16 Order of cut and redaction; bounds | Yes | D4 and D8: redact then cut; range 256-16,000. |
| m17 Delta serialization | Yes | D3: omp2 schema, snake_case, `v: 1`, `ts_ms`. |
| m18 P5 test name | Yes: `crates/e2e/tests/p5_prefix_stability.rs:54` | S3 test renamed `p5_…`. |

Round 3 has not run. Machine-reviewed in part (EGM rounds 1-2), not owner-approved.

## Acceptance tests

1. (S0) With `ai_decision_memory` off, existing prompt goldens and `prompt-facts` are unchanged.
2. (S0) A `decisions` fact renders once, first in the Dynamic band; subagents get none.
3. (S1) Every listed token shape becomes `[REDACTED]`; `credential_rules()` and `SecretRedactor` output are unchanged.
4. (S2) A crash at either commit step leaves a consistent generation and re-sends no consumed delta.
5. (S3) Linked worktrees share a bank; a foreign `gitdir` does not; only the accepted generation reaches the prompt; P5 holds.
6. (S4) `before_agent_start` returns without awaiting the model; invented, short or secret-bearing input never reaches the queue unredacted; a failure or an interrupt does not affect the turn.
7. (S5) Sync bypasses interval and backoff; an invalid reply changes only failure state; `/decisions clear` and `/memory clear` leave each other's state alone.

## Status in omp

**Status: Not started.** Reusable: `omp_vcs::detect`, the `before_agent_start` native hook seam, auxiliary inference and `@memory`, the `omp-secrets` rule engine, `prompt-facts` and the Dynamic band. Not yet implemented: D1-D11. Mnemopi injection stays inactive.

## References

[docs/research/memory-backends.md](../research/memory-backends.md); ADR 0012 (convars); ADR 0015 (Directors and hooks); ADR 0019 (forced calls); ADR 0024 (roster); ADR 0035 (errors); ADR 0036 (Python extensions); ADR 0049 (tiers, vendors as extensions).
