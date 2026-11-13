# 0068. Decision memory: friction-gated project decision files as a separate subsystem; Hindsight stays an extension

Status: proposed
Date: 2026-11-13
Area: memory

Research: [docs/research/memory-backends.md](../research/memory-backends.md) (Gap 14 of the upstream gap closure). This record states decisions; the note holds the evidence.

## Context

### What OMPv1 does

- Sharpshooter is a project decision memory earned by friction, not by decision existence. Per-prompt extraction queues deltas; a shared consolidation pass applies them into three markdown files, `architecture.md`, `product.md` and `style.md`, each capped at 120 lines (`upstream:packages/coding-agent/src/sharpshooter/types.ts:1-8`, `upstream:packages/coding-agent/src/sharpshooter/types.ts:59-65`). The files live home-scoped under `<agentDir>/memories/sharpshooter/<bank>/`, keyed by the working directory, never inside the project tree; beside them a file-per-delta queue, `state.json` and a lock (`upstream:packages/coding-agent/src/sharpshooter/paths.ts:1-11`).
- A delta has a kind, a timeless statement, an optional rejected alternative and rationale, a source, an `evidence` string and three friction flags (corrective, regression, subtle) (`upstream:packages/coding-agent/src/sharpshooter/types.ts:10-57`). The host rejects any delta whose evidence is not an exact substring of the triggering prompt (`upstream:packages/coding-agent/src/sharpshooter/extract.ts:288-289`). Prompts under 16 characters and slash commands are skipped; one extraction runs at a time (`upstream:packages/coding-agent/src/sharpshooter/settings.ts:20-24`, `upstream:docs/memory.md:172`).
- Consolidation calls `replace_memory_files` once with the complete content of all three files, under a cross-process lock, with exponential backoff and a 120-line cap check (`upstream:packages/coding-agent/src/sharpshooter/consolidate.ts:117-123`, `upstream:packages/coding-agent/src/sharpshooter/consolidate.ts:144-145`, `upstream:packages/coding-agent/src/sharpshooter/consolidate.ts:335-336`). Its prompt admits a decision only on regression, subtlety or repetition, and excludes paths, line numbers and task state (`upstream:packages/coding-agent/src/prompts/memories/sharpshooter-consolidate-system.md:5-43`). The populated files are injected under a header saying to follow them unless the user overrides, cut to 15,000 tokens (`upstream:packages/coding-agent/src/sharpshooter/backend.ts:119-128`). About 1,100 lines of TypeScript.
- Hindsight is a client for an external memory server, default `http://localhost:8888` (`upstream:docs/memory.md:134-142`): recall on the first turn, retain every Nth `agent_end`, an on-disk outbox, "mental models" (`upstream:packages/coding-agent/src/hindsight/backend.ts:1-8`, `upstream:packages/coding-agent/CHANGELOG.md:474`). About 3,700 lines of TypeScript.
- Upstream backends are exclusive: one `memory.backend` value picks one (`upstream:packages/coding-agent/src/memory-backend/resolve.ts:8-29`). One shared redactor strips credential-shaped tokens from every backend's writes (`upstream:packages/coding-agent/src/memory-backend/redact.ts:1-7`).

### What omp2 has

- `omp-memory` is the default-off Mnemopi backend (`crates/memory/README.md:1-5`). Its selector is a closed enum, `Off` and `Mnemopi` (`crates/memory/src/config.rs:25-31`), mirrored by a private `RuntimeBackend` (`crates/memory/src/runtime.rs:248-251`) and the convar `ai_memory_backend` (`crates/envd/src/host_settings.rs:74-87`). The crate never reads settings or discovers Git roots (`crates/memory/src/lib.rs:1-6`).
- Prompt injection has three slots, `memory`, `standing` and `recall` (`crates/agent/src/prompt/mod.rs:138-147`). The standing text says memory is non-directive background that must never override the conversation or policy (`crates/memory/src/runtime.rs:566-569`).
- Reusable pieces: the `@memory` role (`crates/catalog/src/selection.rs:175-178`), isolated Director inference with `execute_on` (`crates/contract/src/director.rs:612-619`), the `omp-secrets` redactor (`crates/secrets/src/redact.rs:35`), the canonical project bank identity (`crates/memory/src/bank.rs:20-23`).
- Nothing for decision files exists in core (searched: `sharpshooter`, `decision`). The v1 importer refuses `hindsight` and `sharpshooter` as backends with a typed error (`crates/driver/src/legacy_settings.rs:446-449`); that stays.
- Extension seams: tools (`@omp.tool`), a `memory` prompt slot (`docs/py/00-overview.md:1157-1159`), hooks `before_agent_start`, `turn_end`, `agent_end` (`docs/py/05-hooks.md:1264-1329`), egress policy profiles (`docs/py/06-policy.md:1091`). Whether an extension can read item text to retain it is not shown there (UNVERIFIED).

### Rules that bind

- ADR 0049: vendor integrations live in extensions; Hindsight is one. ADR 0036: extensions are Python.
- Additive only: `ai_memory_backend`'s values do not change.
- Deterministic rules first: the evidence-substring gate, the 120-line cap and the exclusions are rules; the model only drafts and consolidates.

## Decision

**D1. A separate subsystem, not a third backend value.** Decision memory runs beside Mnemopi, controlled by the default-off convar `sv_decision_memory`. It does not touch `ai_memory_backend`, `MemoryBackend` or `RuntimeBackend`. Upstream's exclusivity is a limit, not a feature: the two do different jobs.

**D2. Storage outside the working tree.** A bank directory under the user config root keyed by the canonical project identity omp-memory already derives. Three files, a 120-line cap, a file-per-delta queue with atomic writes, `state.json` and a lock. A repository can never plant decision files.

**D3. Extraction by a standing Director.** Main agent only. Each user prompt goes to the `@memory` role through the isolated inference path with a forced `record_deltas` schema. Slash commands and short prompts are skipped; credential-shaped text is redacted with the `omp-secrets` redactor before sending; a delta whose evidence is not an exact substring of the prompt is dropped; one extraction at a time. Failure never fails the turn.

**D4. Consolidation on demand and at session close.** One pass through `@memory` with a forced `replace_memory_files` call and the admission-law prompt, run by `/memory sync` and at session close. No background timer in the first cut. Exponential backoff after failures; a reply over the cap, or not a single complete file set, leaves files and queue untouched.

**D5. Injection in its own slot with its own header.** The decision files are the user's own earlier statements, so a directive header is defensible, but it is a different trust stance from Mnemopi's non-directive standing text. The slot says: follow these unless the user overrides. It is bounded by a token limit and unchanged byte for byte when no files exist.

**D6. Controls.** `/memory decisions` views, `/memory queue` lists pending deltas, `/memory sync` consolidates, `/memory clear` removes only this project's bank, and a status line reports file sizes, queue depth and the last consolidation.

**D7. Hindsight is an extension only.** No core slice. It is a vendor integration (ADR 0049). A Python extension with a small HTTP client, `recall`, `reflect` and `retain` tools and a `memory` prompt slot is the shape. If a spike finds a missing seam, that becomes its own decision.

**D8. Gates.** Feature `cap-decision-memory` and default-off `sv_decision_memory`.

## Rejected options

- **A third `ai_memory_backend` value.** It changes an existing convar's accepted values and makes decision files exclusive with Mnemopi.
- **Decision files inside the project tree.** A hostile repository could plant directive text.
- **A Python extension for the decision files.** Extraction and consolidation need isolated model calls and atomic writes; whether an extension can make them is UNVERIFIED, and directive prompt text from an extension is a larger trust question.
- **Port Hindsight into core.** Rejected by ADR 0049.
- **A background consolidation timer in the first cut.** Deterministic triggers first.
- **Keep decisions in `AGENTS.md` only.** Honest for users who write them by hand; it loses the automatic, friction-gated part.

## Slices

Each slice is one PR; tests are written first and not edited to pass.

**S1. Storage and rules, no model.** A new small crate or a module near `omp-memory` (O3).
- Tests: `delta_round_trips`, `crash_between_apply_and_unlink_redelivers_and_applying_twice_gives_the_same_files`, `two_processes_cannot_consolidate_at_once`, `file_over_120_lines_is_rejected`, `bank_id_from_a_hostile_directory_name_cannot_escape_the_bank_root`.
- Gate: `cap-decision-memory`.

**S2. Read path.** The prompt slot and `/memory decisions`.
- Tests: extend the prompt golden tests (`crates/agent/tests/prompt_golden.rs:229`): `files_present_show_the_slot_once_in_the_same_place_every_turn`, `no_files_leaves_the_prompt_unchanged_byte_for_byte`, `oversize_file_is_truncated_at_the_limit_with_a_marker`.

**S3. Extraction Director.** Tests with a fake `ErasedInference`: `valid_delta_lands_in_the_queue`, `delta_with_invented_evidence_is_dropped`, `token_shaped_secret_reaches_the_model_redacted`, `subagent_session_queues_nothing`, `extraction_failure_never_fails_the_turn`, `second_prompt_during_extraction_is_queued_not_dropped` (O6).

**S4. Consolidation and controls.** Tests: `fake_consolidator_reply_is_applied_atomically_and_consumed_queue_files_removed`, `reply_over_the_cap_leaves_files_and_queue_untouched`, `reply_that_is_not_one_complete_file_set_is_rejected`, `repeated_failure_backs_off`, `clear_removes_only_this_projects_bank`, plus a golden test of the consolidation prompt.

**H. Hindsight extension (outside core).** E1 a spike extension registering the three tools over HTTP; E2 a prompt slot for first-turn recall; E3 auto-retain on `turn_end` if item text is readable. Each needs an egress grant and a secret for the token.

## Upstream origin and license

A port of the decision files draws from `upstream:packages/coding-agent/src/sharpshooter/`, the prompt `upstream:packages/coding-agent/src/prompts/memories/sharpshooter-consolidate-system.md` and the patterns in `upstream:packages/coding-agent/src/memory-backend/redact.ts`. TypeScript and Markdown under MIT (`upstream:LICENSE:1-5`; copyright Mario Zechner, Can Bölük, Stencil Labs, Inc.). Keep the notice for the prompt text. Hindsight: nothing in core; the upstream client (`upstream:packages/coding-agent/src/hindsight/client.ts`) is MIT and could seed an extension. The Hindsight server is a separate product; its licence was not checked (UNVERIFIED) and does not bind a client that only calls its HTTP API.

## Consequences

- Projects accumulate a short, reviewed set of decisions that agents follow unless told otherwise.
- Prompts leave the machine for extraction; redaction is a floor, not a guarantee. The docs must say so.
- A directive slot is a new trust stance next to Mnemopi's non-directive one.
- Hindsight users keep their server and use an extension.

## Open questions for the owner

- **O1.** Directive versus non-directive wording. Recommendation: directive, in its own slot, because the content is the user's own prior statement and is friction-gated.
- **O2.** Convar versus enum value. Recommendation: the convar.
- **O3.** New crate or inside `omp-memory`. Recommendation: a new small crate; `omp-memory` is Mnemopi-specific and does not read settings.
- **O4.** Role: `@memory` or a dedicated setting. Recommendation: `@memory`.
- **O5.** Consolidation trigger. Recommendation: `/memory sync` and session close first; add a timer only on demand.
- **O6.** Prompts that arrive during an extraction. Recommendation: queue them, not drop.
- **O7.** Token limit. Upstream injects up to 15,000 tokens. Recommendation: a smaller default, 4,000, as a convar.
- **O8.** Can an extension read item text, register tools without clashing with core names, and hold a secret and an egress grant? Not verified. Recommendation: answer in the Hindsight spike before promising the extension.

## Status in omp

**Status: Not started.** Reusable today: the bank identity, the `@memory` role, isolated Director inference, the `omp-secrets` redactor, the prompt slot machinery.

## References

[docs/research/memory-backends.md](../research/memory-backends.md); ADR 0049 (vendors as extensions); ADR 0036 (Python-only extensions); ADR 0015 (Directors); ADR 0012 (convars).

