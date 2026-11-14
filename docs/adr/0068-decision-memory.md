# 0068. Decision memory: friction-gated project decision files as a separate subsystem; Hindsight stays an extension

Status: Proposed 2026-11-14; EGM round 1 resolved, rounds 2-3 pending
Date: 2026-11-13 (revised 2026-11-14)
Area: memory

Research: [docs/research/memory-backends.md](../research/memory-backends.md) (Gap 14 of the upstream gap closure). Upstream pin `c308970263` (v18.20.0); omp2 base `origin/omp2 9cb6fd6389`. Citations: `upstream:` is the OMPv1 pin; bare paths are omp2. This record states decisions; the note holds the evidence. Nothing was built or run for this revision; every omp2 claim was read in the tree.

## Context

### What OMPv1 does

- Sharpshooter is a project decision memory earned by friction, not by decision existence. Per-prompt extraction queues deltas; a shared consolidation pass applies them into three markdown files, `architecture.md`, `product.md` and `style.md`, each capped at 120 lines (`upstream:packages/coding-agent/src/sharpshooter/types.ts:1-8`, `upstream:packages/coding-agent/src/sharpshooter/types.ts:59-65`). The files live home-scoped under `<agentDir>/memories/sharpshooter/<bank>/`, never inside the project tree; beside them a file-per-delta queue, `state.json` and a lock (`upstream:packages/coding-agent/src/sharpshooter/paths.ts:1-11`). The bank id is derived from the working directory alone, `<basename>-<hash>` (`upstream:packages/coding-agent/src/sharpshooter/paths.ts:23-26`, `upstream:packages/coding-agent/src/mnemopi/config.ts:207-216`), so it does not depend on the Mnemopi backend being on.
- A delta has a kind, a timeless statement, an optional rejected alternative and rationale, a source, an `evidence` string and three friction flags (corrective, regression, subtle) (`upstream:packages/coding-agent/src/sharpshooter/types.ts:10-57`). The extraction input is an envelope: the current prompt, the previous human message cut to 400 characters and the last assistant message cut to 800, with fenced code replaced by `[code omitted]` (`upstream:packages/coding-agent/src/sharpshooter/extract.ts:80-84`, `upstream:packages/coding-agent/src/sharpshooter/extract.ts:103-110`, `upstream:packages/coding-agent/src/sharpshooter/extract.ts:140-162`). The host rejects any delta whose evidence is not an exact substring of the current prompt (`upstream:packages/coding-agent/src/sharpshooter/extract.ts:288`). Slash commands and prompts under the minimum length are skipped; one extraction runs at a time (`upstream:packages/coding-agent/src/sharpshooter/extract.ts:196-207`, `upstream:packages/coding-agent/src/sharpshooter/settings.ts:20-24`).
- Consolidation calls `replace_memory_files` once with the complete content of all three files, under a cross-process lock, with exponential backoff and a 120-line cap check (`upstream:packages/coding-agent/src/sharpshooter/consolidate.ts:117-123`, `upstream:packages/coding-agent/src/sharpshooter/consolidate.ts:144-145`, `upstream:packages/coding-agent/src/sharpshooter/consolidate.ts:335-336`). Its prompt admits a decision only on regression, subtlety or repetition, and excludes paths, line numbers and task state (`upstream:packages/coding-agent/src/prompts/memories/sharpshooter-consolidate-system.md:5-43`). The populated files are injected under a header saying to follow them unless the user overrides, cut to 15,000 tokens (`upstream:packages/coding-agent/src/sharpshooter/backend.ts:119-128`). About 1,100 lines of TypeScript.
- **Redaction upstream covers file writes only.** The extraction envelope is rendered and sent unredacted (`upstream:packages/coding-agent/src/sharpshooter/extract.ts:238-246`), and admitted deltas are queued unredacted (`upstream:packages/coding-agent/src/sharpshooter/extract.ts:270-273`); `extract.ts` imports no redactor. The consolidator redacts each file's content before the line-cap check and the write (`upstream:packages/coding-agent/src/sharpshooter/consolidate.ts:330-338`). The shared redactor matches fixed-prefix AWS, GitHub, npm, Slack and Google tokens (`upstream:packages/coding-agent/src/memory-backend/redact.ts:11-19`), keyword-led credential runs such as `secret_aB3dEfGh1JkLmN` (`upstream:packages/coding-agent/src/memory-backend/redact.ts:22-106`) and JWT-shaped triples (`upstream:packages/coding-agent/src/memory-backend/redact.ts:108-143`).
- Hindsight is a client for an external memory server, default `http://localhost:8888` (`upstream:docs/memory.md:134-142`): recall on the first turn, retain every Nth `agent_end`, an on-disk outbox, "mental models" (`upstream:packages/coding-agent/src/hindsight/backend.ts:1-8`, `upstream:packages/coding-agent/CHANGELOG.md:474`). About 3,700 lines of TypeScript.
- Upstream backends are exclusive: one `memory.backend` value picks one (`upstream:packages/coding-agent/src/memory-backend/resolve.ts:8-29`).

### What omp2 has

- `omp-memory` is the default-off Mnemopi backend (`crates/memory/README.md:1-5`). Its selector is a closed enum, `Off` and `Mnemopi` (`crates/memory/src/config.rs:25-31`), mirrored by a private `RuntimeBackend` (`crates/memory/src/runtime.rs:248-251`) and the convar `ai_memory_backend` (`crates/envd/src/host_settings.rs:74-87`). The crate never reads settings or discovers Git roots (`crates/memory/src/lib.rs:1-6`).
- **Project identity exists only while Mnemopi is on.** Mnemopi's bank identity is the primary Git root from the Environment's repository snapshot, else the workspace root (`crates/memory/src/bank.rs:49-60`, `crates/memory/src/bank.rs:80-97`). With `Off` the runtime returns before resolving any scope (`crates/memory/src/runtime.rs:265-274`), and envd does not even take the repository snapshot (`crates/envd/src/server/session.rs:45-54`). The snapshot type carries `primary_root`, "canonical identity shared by linked working trees" (`crates/envd/src/vcs/mod.rs:24-37`). The project state bucket `<data>/projects/<sha256>` is keyed by the canonical working directory, not the Git root (`crates/env/src/project_state.rs:83-89`); Mnemopi keeps its files below it (`crates/envd/src/memory.rs:155-160`).
- **Nothing writes the `memory` prompt property.** The prompt reads a `memory` map with three keys, `memory`, `standing` and `recall` (`crates/agent/src/prompt/mod.rs:139-149`); `memory` and `standing` render in the Dynamic band, `recall` in the Volatile band (`crates/agent/src/prompt/mod.rs:99-106`). The props come from the `prompt-facts` JSON on `<meta>` (`crates/agent/src/prompt/projection.rs:15-31`). The only production writer of `prompt-facts` is `install_prompt_facts` (`crates/driver/src/headless/kernel.rs:4043-4129`, called at `crates/driver/src/headless/kernel.rs:2920-2928` and `crates/driver/src/headless/kernel.rs:3416-3424`), and it inserts no `memory` key. Only the prompt golden test sets one (`crates/agent/tests/prompt_golden.rs:228-232`). `prompt_snapshot` is called only by the envd wrapper and `/memory view` (`crates/envd/src/memory.rs:111-117`, `crates/app/src/chat_services/misc.rs:109-112`). So Mnemopi injection is inactive today.
- The slot catalog is fixed: `Memory` "compaction-epoch memory", `Standing` "compaction-epoch standing instructions", `Recall` "per-turn recall" (`crates/contract/src/slots.rs:45-50`); `Dynamic` means "changes at a compaction or reset boundary" (`crates/contract/src/slots.rs:14-15`). Extension prompt contributions are decoded in envd (`crates/envd/src/exthost/dispatch.rs:1119-1144`), but `extension_prompt_provider` has no caller outside envd (`crates/envd/src/lib.rs:1476-1478`, `crates/envd/src/server/session.rs:1117-1118`); I found none in agent or driver.
- **`/memory` already has five operations**: `view`, `stats`, `diagnose`, `clear` and `enqueue` (`crates/chat/src/overlays/services.rs:776-790`, `crates/chat/src/commands/misc.rs:419-430`). `clear` clears the Mnemopi bank and `enqueue` enqueues Mnemopi consolidation (`crates/app/src/chat_services/misc.rs:140-147`). No `/decisions` command exists (the command definitions in `crates/chat/src/commands/` and the list in `crates/chat/tests/pi_commands.rs:50-75`).
- **The `omp-secrets` redactor is rule-driven.** `SecretRedactor::new(rules)` lowers every rule to an irreversible one (`crates/secrets/src/redact.rs:24-37`). The built-in rules match GitHub, GitLab, OpenAI and Anthropic token prefixes only (`crates/secrets/src/builtins.rs:7-29`); AWS, npm, Slack, Google, JWT and keyword-led runs are not covered. Export builds the redactor from the session's secret rules (`crates/driver/src/share.rs:476`); raw provider capture builds it from the built-ins (`crates/ai/src/transport/capture.rs:107`).
- Reusable pieces: the `@memory` role (`crates/catalog/src/selection.rs:175-178`), isolated Director inference with `execute_on` (`crates/contract/src/director.rs:612-619`), the environment-owned memory runtime pattern (`crates/envd/src/lib.rs:1336-1339`, `crates/envd/src/memory.rs:171-190`).
- Nothing for decision files exists in core (searched `sharpshooter`, `decision`, `decisions`). The v1 importer refuses `hindsight` and `sharpshooter` as backends with a typed error (`crates/driver/src/legacy_settings.rs:446-449`); that stays.
- Extension seams: tools (`@omp.tool`), a `memory` prompt slot (`docs/py/00-overview.md:1157-1159`), hooks `before_agent_start`, `turn_end`, `agent_end` (`docs/py/05-hooks.md:1264-1329`), egress policy profiles (`docs/py/06-policy.md:1091`).

### Rules that bind

- ADR 0049: vendor integrations live in extensions; Hindsight is one. ADR 0036: extensions are Python.
- Additive only: `ai_memory_backend`'s values, the `/memory` command and its five operations, `MemoryOp`, the `SlotId` catalog and the built-in secret rules do not change.
- ADR 0012: a convar a hostile repository could abuse never carries the `PROJECT` flag (`docs/adr/0012-convars.md:77`).
- ADR 0035: library errors are `thiserror` enums with typed sources, never stringified.
- Deterministic rules first: the evidence-substring gate, redaction, the line cap and the exclusions are rules; the model only drafts and consolidates.

## Decision

**D1. A separate subsystem, not a third backend value.** Decision memory runs beside Mnemopi. It MUST NOT touch `ai_memory_backend`, `MemoryBackend`, `RuntimeBackend` or the `omp-memory` crate. It works the same whether Mnemopi is off or on.

**D2. Project identity that does not depend on Mnemopi.** The identity root is `RepositorySnapshot.primary_root` when the snapshot has one, else the canonical workspace root, the same rule as `BankScope::resolve` (`crates/memory/src/bank.rs:80-84`), so linked worktrees share one bank. When `sv_decision_memory` is on, envd MUST take the repository snapshot itself; it MUST NOT start Mnemopi or depend on `ai_memory_backend` to get it. The bank directory is `<data>/decisions/<hex>/`, where `<hex>` is the 64-digit `Hash32` hex of the identity root's bytes, the encoding `omp_env::project_state::directory` uses (`crates/env/src/project_state.rs:83-89`). A digest name cannot carry a path separator, so no directory name can escape the root. The bank holds `architecture.md`, `product.md`, `style.md`, `queue/<session>/<ts>-<nonce>.json`, `state.json`, `identity.json` (the identity root, for status) and a lock file. It lives in the data directory like Mnemopi's state, never in the working tree and never in the user config root. Upstream keys on the working directory; keying on the primary root is a deliberate change so worktrees of one repository share decisions.

**D3. Redaction at every boundary a decision crosses.** One redactor, built once per session, is applied:
- R1 to the whole extraction envelope (current prompt, previous human message, assistant context) before it is sent to the model. The evidence-substring check runs against the redacted prompt.
- R2 to every free-text field of an admitted delta (statement, rejected alternative, rationale, evidence) before the queue file is written.
- R3 to every file in a consolidation reply before the line-cap check and the atomic write, as upstream does.
- R4 to the injected text when it is read, because the files can be edited by hand.

The rule set is the `omp-secrets` built-ins (`credential_rules`), the session's secret rules (as export uses them), and memory rules owned by the new crate: irreversible regex rules for the upstream fixed-prefix tokens (AWS, npm, Slack, Google), a JWT-triple matcher and a linear keyword-run scanner ported from `upstream:packages/coding-agent/src/memory-backend/redact.ts:22-143`. Matches become `[REDACTED]`. The built-in rules in `crates/secrets/src/builtins.rs` do not change. Redaction does not cover personal data, file paths or prose that describes a secret; the user docs MUST say redaction is a floor, not a guarantee.

**D4. Extraction by a standing Director.** Main agent only. Each committed user prompt goes to the `@memory` role through `execute_on` with a forced `record_deltas` call. Slash commands and prompts under `sv_decision_memory_min_prompt` characters (default 16, floor 2, counted in graphemes) are skipped. The envelope is bounded as upstream (400 and 800 characters, fenced code omitted) and redacted (R1). A delta whose evidence is not an exact substring of the redacted prompt is dropped. One extraction runs at a time; a prompt arriving meanwhile is held in a one-slot pending buffer, newest wins (O6). Failure never fails the turn and never reaches the transcript.

**D5. Consolidation on demand and at session close.** One pass through `@memory` with a forced `replace_memory_files` call and the admission-law prompt, run by `/decisions sync` and at session close. No background timer in the first cut. The pass holds the bank lock. A reply that is not exactly one complete set of the three files, that has a file over 120 lines after redaction, or that would empty all three non-empty files leaves files and queue untouched. Failures back off exponentially in `state.json`. Consumed queue files are removed only after the atomic file replacement.

**D6. Injection: a fourth key of the `memory` prompt property, in the Dynamic band.**
- Producer (the missing piece today). When `sv_decision_memory` is on and the kernel's agent is the main agent, `install_prompt_facts` writes `memory.decisions` into the journaled `prompt-facts`, from the decision store's bounded injection text. With the convar off, with no populated files, or in a subagent, it writes no `memory` key at all, so `prompt-facts` and the prompt are byte for byte what they are today. It MUST NOT write `memory.memory`, `memory.standing` or `memory.recall`: turning on Mnemopi injection is a separate decision (O9), because it would change the prompt of every user who has `ai_memory_backend = mnemopi`.
- Projection. `memory_entries` reads a fourth key, `decisions`. Its text renders in the Dynamic band before `memory` and `standing`, so Mnemopi's non-directive standing text stays next to the Mnemopi block it governs. The text is captured once per composition (session start or resume) and does not change within a session; a `/decisions sync` takes effect in the next session. That keeps the Dynamic band stable between compactions.
- Slot. No new `SlotId`. The content is "standing instructions" in the catalog's sense (`crates/contract/src/slots.rs:47-48`), but the production projection does not route `memory` through the slot catalog, and adding a catalog id would widen what extensions may declare (O10).
- Framing. The text is wrapped in `<project-decisions>` with a header from a static prompt file in the new crate: these are decisions the user made in earlier sessions in this project; follow them unless the user overrides them in this conversation; system and tool policy still win. It is cut at `sv_decision_memory_tokens` (default 4,000; bytes = tokens × 4, the approximation Mnemopi uses at `crates/memory/src/runtime.rs:565`), at a UTF-8 boundary, with a truncation marker.

**D7. Controls under a new `/decisions` command.** `/decisions` and `/decisions show` show the injected text and the three files; `/decisions queue` lists pending deltas; `/decisions sync` consolidates now; `/decisions clear` removes only this project's bank after a confirmation; `/decisions status` reports the identity root, file line counts, queue depth and the last consolidation. The existing `/memory` command, its five operations, its usage text and `MemoryOp` MUST NOT change; `/memory view` keeps showing the Mnemopi snapshot only. If the owner prefers the operations under `/memory`, that edits an existing command and needs a ruling (O11).

**D8. Gates.** Behaviour is gated by the default-off convar `sv_decision_memory` (bool, `ARCHIVE`, no `PROJECT` flag: a repository must not be able to switch on directive injection or the sending of prompts to a model), declared in `crates/envd/src/host_settings.rs` with `"ui.tab": "memory"`. Its companions `sv_decision_memory_tokens` and `sv_decision_memory_min_prompt` carry the same flags. No `cap-*` feature: the new crate adds no dependency weight beyond crates core already links (`omp-core`, `omp-secrets`, `serde`), and ADR 0049 uses `cap-*` for build weight (`docs/adr/0049-tiered-integration-architecture.md:47`). No legacy `"legacy.path"` mapping: v1's `sharpshooter` backend value stays refused by the importer.

**D9. Code home.** A new crate `crates/decisions` (`omp-decisions`) owns the bank layout, identity digest, delta and file types, queue, lock, the redaction rules, the line cap, consolidation reply validation and the injection text. Like `omp-memory` it reads no settings and probes no Git. envd composes one store per session beside the memory runtime and exposes it as `environment.decision_store()`; the driver reads it at composition; the Director and `/decisions` reach it the same way the memory runtime is reached (`crates/envd/src/lib.rs:1336-1339`, `crates/app/src/session_runner.rs:65`). Errors are one `thiserror` enum in the crate (ADR 0035).

**D10. Hindsight is an extension only.** No core slice. It is a vendor integration (ADR 0049). A Python extension with a small HTTP client, `recall`, `reflect` and `retain` tools and a `memory` prompt slot is the shape. Extension prompt contributions do not reach the production prompt today (Context), so the spike must confirm the seam first (O8).

## Rejected options

| Option | Why not |
|---|---|
| A third `ai_memory_backend` value | Changes an existing convar's accepted values and makes decision files exclusive with Mnemopi. |
| Reuse Mnemopi's `BankScope` / `omp-memory` for identity | Only resolved when Mnemopi is on (`crates/memory/src/runtime.rs:265-274`); depending on it couples the two subsystems. |
| Key the bank on the `project_state` bucket (working directory) | Linked worktrees and subdirectories would get separate decision sets (`crates/env/src/project_state.rs:83-89`). |
| Decision files inside the project tree | A hostile repository could plant directive text. |
| New `/memory decisions`, `/memory queue`, `/memory sync`; `/memory clear` clearing decisions | Edits an existing command; `clear` and `enqueue` already mean Mnemopi operations (`crates/app/src/chat_services/misc.rs:140-147`). |
| Write the whole `memory` property, Mnemopi included, in S0 | Silently turns on Mnemopi injection for existing users. |
| A new `SlotId::Decisions` | Widens the slot vocabulary extensions may declare; not needed by the production projection. |
| Add the upstream patterns to `omp-secrets` built-ins | Changes export and raw-capture redaction, which is existing behaviour. |
| Redact only file writes, as upstream | Prompts and queued deltas would carry raw credentials to the model and to disk. |
| A Python extension for the decision files | Needs isolated model calls, atomic writes and first-party directive prompt text; not shown to be available to extensions. |
| Port Hindsight into core | Rejected by ADR 0049. |
| A background consolidation timer in the first cut | Deterministic triggers first. |
| Keep decisions in `AGENTS.md` only | Loses the automatic, friction-gated part. |

## Slices

Each slice is one PR; tests are written first and not edited to pass. Every slice leaves `just lint`, `just test` and `just check-minimal-caps` green; with `sv_decision_memory` off the existing prompt goldens and `/memory` tests pass unchanged.

**S0. Gate and prompt producer.**
- Scope: the three convars in `crates/envd/src/host_settings.rs`; `install_prompt_facts` gains an optional decisions text input and writes `memory.decisions` only when it is `Some`, the agent is main and the convar is on; `memory_entries` reads `decisions` and renders it first in the Dynamic band. S0 feeds the input from a test seam only; S2 feeds it from the store.
- Tests: `crates/agent/tests/prompt_golden.rs`: `decisions_render_once_in_the_dynamic_band_before_memory_and_standing`, `no_decisions_key_leaves_the_prompt_unchanged_byte_for_byte`; `crates/driver` (headless composition): `gate_off_writes_no_memory_key_into_prompt_facts`, `gate_on_writes_only_memory_decisions`, `subagent_composition_writes_no_decisions`.
- Acceptance: with the gate off the existing goldens pass unchanged and `prompt-facts` has no `memory` key; with the gate on and a fixed text, the text appears once, in the Dynamic band, before `memory`.
- Gate: `sv_decision_memory`, default off. Verification: `just test-pkg omp-agent`, `just test-pkg omp-driver`.

**S1. `omp-decisions`: storage, identity, redaction, rules; no model, no wiring.**
- Scope: new crate (README, `.github/labeler.yml` `area/*` entry), types for the delta exactly as `upstream:packages/coding-agent/src/sharpshooter/types.ts:10-57`, the bank layout of D2, queue, lock, the redaction rules of D3, reply validation of D5, the injection text and its truncation of D6. Upstream origin noted in the crate README and NOTICE.
- Tests (unit, temp dirs): `delta_round_trips`, `bank_dir_is_the_identity_digest_for_any_directory_name`, `linked_worktree_and_primary_root_map_to_one_bank`, `no_repository_falls_back_to_the_workspace_root`, `crash_between_apply_and_unlink_redelivers_and_applying_twice_gives_the_same_files`, `two_processes_cannot_consolidate_at_once`, `file_over_120_lines_after_redaction_is_rejected`, `reply_missing_a_file_or_duplicating_one_is_rejected`, `reply_emptying_all_files_is_rejected`, `upstream_token_shapes_are_redacted` (AWS, npm, Slack, Google, JWT, keyword run, plus the built-in GitHub/OpenAI/Anthropic shapes), `plain_words_like_authentication_are_not_redacted`, `keyword_scanner_is_linear_on_a_long_token_run`, `injection_is_cut_at_the_limit_on_a_utf8_boundary_with_a_marker`.
- Gate: none; no caller until S2.

**S2. Read path wiring and `/decisions show|status`.**
- Scope: envd composes the store when `sv_decision_memory` is on (taking `vcs::snapshot` itself, independent of `ai_memory_backend`); the driver feeds S0's input from it; the new `/decisions` command with `show` and `status`.
- Tests: `crates/envd`: `store_starts_with_mnemopi_off`, `gate_off_takes_no_snapshot_and_opens_no_files`; `crates/driver`: `populated_files_reach_prompt_facts_redacted`; `crates/app`: `decisions_show_lists_files_and_injected_text`, `memory_view_output_is_unchanged_with_decisions_present`; e2e: extend `crates/e2e/tests/p5_prefix_stability.rs` with `decisions_block_keeps_the_prefix_stable_across_turns`.
- Golden journal: one new family in `crates/session/tests/golden_journals.rs` for `prompt-facts` carrying `memory.decisions` (append-only corpus).
- Depends on S0, S1.

**S3. Extraction Director.**
- Scope: a standing Director (ADR 0015) engaged by `compose_kernel` for the main agent when the gate is on; envelope, R1, R2, the evidence check, one-in-flight with the pending buffer.
- Tests with a fake `ErasedInference`: `valid_delta_lands_in_the_queue`, `delta_with_invented_evidence_is_dropped`, `envelope_secret_reaches_the_model_redacted_and_evidence_matches_the_redacted_prompt`, `queued_delta_fields_are_redacted`, `slash_command_and_short_prompt_are_skipped`, `subagent_session_queues_nothing`, `extraction_failure_never_fails_the_turn`, `second_prompt_during_extraction_is_held_not_dropped`.
- Depends on S1, S2.

**S4. Consolidation and the remaining controls.**
- Scope: the consolidation pass of D5, its prompt as a static file (golden-tested), the session-close trigger, `/decisions queue|sync|clear`.
- Tests: `fake_consolidator_reply_is_applied_atomically_and_consumed_queue_files_removed`, `reply_over_the_cap_leaves_files_and_queue_untouched`, `reply_secret_is_redacted_before_the_cap_check_and_write`, `repeated_failure_backs_off`, `decisions_clear_removes_only_this_projects_bank`, `memory_clear_leaves_the_decision_bank_untouched`, plus a golden of the consolidation prompt.
- Depends on S1, S3.

**H. Hindsight extension (outside core).** E0 confirm that an extension `memory` prompt slot reaches the production prompt (O8); E1 a spike extension registering the three tools over HTTP; E2 a prompt slot for first-turn recall; E3 auto-retain on `turn_end` if item text is readable. Each needs an egress grant and a secret for the token.

## Upstream origin and license

A port of the decision files draws from `upstream:packages/coding-agent/src/sharpshooter/`, the prompt `upstream:packages/coding-agent/src/prompts/memories/sharpshooter-consolidate-system.md` and the patterns and scanners in `upstream:packages/coding-agent/src/memory-backend/redact.ts`. TypeScript and Markdown under MIT (`upstream:LICENSE:1-5`; copyright Mario Zechner, Can Bölük, Stencil Labs, Inc.). Keep the notice for the prompt text and the ported redaction logic in the crate's NOTICE. Hindsight: nothing in core; the upstream client (`upstream:packages/coding-agent/src/hindsight/client.ts`) is MIT and could seed an extension. The Hindsight server is a separate product; its licence was not checked (UNVERIFIED) and does not bind a client that only calls its HTTP API.

## Consequences

- Projects accumulate a short, reviewed set of decisions that agents follow unless told otherwise, shared across worktrees of one repository.
- Prompts leave the machine for extraction; redaction happens before they do, but it is a floor, not a guarantee. The docs must say so.
- A directive block is a new trust stance next to Mnemopi's non-directive one; the framing keeps the two apart.
- Decision text is journaled in `prompt-facts`, so it appears in exports and session files, already redacted.
- A consolidation mid-session reaches the prompt only in the next session.
- Hindsight users keep their server and use an extension.

## Open questions for the owner

- **O1. Directive versus non-directive wording?** Recommendation: directive, in its own framed block, subordinate to system policy and the current user.
- **O2. Convar versus enum value?** Recommendation: the convar.
- **O3. New crate or inside `omp-memory`?** Recommendation: the new crate `omp-decisions` (D9).
- **O4. Role: `@memory` or a dedicated setting?** Recommendation: `@memory`.
- **O5. Consolidation trigger?** Recommendation: `/decisions sync` and session close first; a timer only on demand.
- **O6. Prompts that arrive during an extraction?** Recommendation: hold one, newest wins.
- **O7. Token limit?** Upstream injects up to 15,000 tokens. Recommendation: 4,000 by default, as `sv_decision_memory_tokens`.
- **O8. Can an extension's `memory` prompt slot reach the production prompt, can it read item text, register tools without clashing with core names, and hold a secret and an egress grant?** `extension_prompt_provider` has no consumer outside envd today. Recommendation: answer in the Hindsight spike (E0) before promising the extension.
- **O9. Should Mnemopi's snapshot also be written into the `memory` property?** That turns on injection for every `ai_memory_backend = mnemopi` user, an existing-behaviour change. Recommendation: a separate decision and PR, not part of this ADR.
- **O10. Should decisions get their own `SlotId`?** Recommendation: no; revisit if extensions need to target it.
- **O11. Should the controls live under `/memory` instead of `/decisions`?** That edits an existing command. Recommendation: `/decisions`.
- **O12. Should subagents receive the decisions block?** Recommendation: main agent only in the first cut; revisit with usage.
- **O13. Should the upstream token patterns become `omp-secrets` built-ins?** That changes export and capture redaction. Recommendation: keep them in `omp-decisions` now; promote in a separate change.

## What the review changed

### Round 1 (EGM, 2026-11-13; resolved 2026-11-14)

Round 1 reported four findings (recorded in `docs/adr/STATUS.md`, row 0068). All four were confirmed against source; none was rejected.

- **S0 producer and slot design.** Confirmed: the prompt reads `memory.{memory,standing,recall}` from `prompt-facts` (`crates/agent/src/prompt/mod.rs:139-149`, `crates/agent/src/prompt/projection.rs:15-31`), and the only production writer, `install_prompt_facts`, inserts no `memory` key (`crates/driver/src/headless/kernel.rs:4043-4129`). The earlier text's "UNVERIFIED" is now observed. The slot catalog was checked (`crates/contract/src/slots.rs:45-50`). Changed: D6 names the producer (`install_prompt_facts`, main agent, gate on), the key (`memory.decisions`), the band (Dynamic, before `memory` and `standing`), the capture point (once per composition) and why no new `SlotId`; S0 now ships the gate and the producer and says it writes no Mnemopi keys; O9, O10, O12 added.
- **Project identity when Mnemopi is off.** Confirmed: with `Off` the runtime resolves no bank scope (`crates/memory/src/runtime.rs:265-274`) and envd takes no repository snapshot (`crates/envd/src/server/session.rs:45-54`), so "the identity omp-memory already derives" does not exist then. Changed: D2 derives the identity itself from `RepositorySnapshot.primary_root` or the workspace root (`crates/envd/src/vcs/mod.rs:24-37`), stores the bank at `<data>/decisions/<hex>/`, and replaces the "hostile directory name" test with digest tests; the config-root location was dropped for the data directory.
- **`/memory clear` and `/memory view` collide with existing commands.** Confirmed: `/memory` already accepts `view|stats|diagnose|clear|enqueue` (`crates/chat/src/commands/misc.rs:419-430`, `crates/chat/src/overlays/services.rs:776-790`), and `clear` and `enqueue` act on Mnemopi (`crates/app/src/chat_services/misc.rs:140-147`). Changed: D7 moves every control to a new `/decisions` command and states that `/memory` and `MemoryOp` do not change; S2 and S4 add `memory_view_output_is_unchanged_with_decisions_present` and `memory_clear_leaves_the_decision_bank_untouched`; O11 records the alternative.
- **Redaction scope.** Confirmed: the `omp-secrets` built-ins cover only GitHub, GitLab, OpenAI and Anthropic shapes (`crates/secrets/src/builtins.rs:7-29`), and upstream redacts only consolidated file writes (`upstream:packages/coding-agent/src/sharpshooter/consolidate.ts:330-338`; no redactor in `extract.ts`, envelope sent at `upstream:packages/coding-agent/src/sharpshooter/extract.ts:238-246`). The earlier D3 named only the prompt. Changed: D3 now names four boundaries (envelope, queued delta, file write, injection read), the rule set, where the evidence check runs, and what redaction does not cover; S1, S3 and S4 carry the redaction tests; O13 added.

Corrected against this tree while resolving round 1:
- Gate: dropped the `cap-decision-memory` feature; the convar `sv_decision_memory` is the gate, without the `PROJECT` flag (`docs/adr/0012-convars.md:77`), because the crate adds no build weight (D8).
- The earlier research gap table said `/memory view` was the only operation; it has five.
- Extension prompt contributions have no consumer outside envd (`crates/envd/src/lib.rs:1476-1478`); folded into O8 and H/E0.

Rounds 2 and 3 have not run. This design is not reviewed beyond round 1 and not owner-approved.

## Acceptance tests

1. (S0) With `sv_decision_memory` off, every existing prompt golden passes unchanged and `prompt-facts` has no `memory` key.
2. (S0) With the gate on and a decisions text, the prompt shows it once, in the Dynamic band, before `memory` and `standing`; a subagent composition shows none.
3. (S1) The bank directory for an identity root is its 64-hex digest, linked worktrees share it, and no directory name changes the path shape.
4. (S1) Every upstream token shape and the built-in shapes are redacted; ordinary words are not; the keyword scanner is linear.
5. (S2) With Mnemopi off and the gate on, populated files reach the prompt, redacted, and `/memory view` output is unchanged.
6. (S2) P5 prefix stability holds with the decisions block present.
7. (S3) A delta with invented evidence is dropped; a secret in the envelope reaches the fake model redacted; a subagent queues nothing; a failure does not fail the turn.
8. (S4) A valid reply replaces the files atomically and removes consumed queue files; an invalid reply changes nothing; `/decisions clear` and `/memory clear` each leave the other's state alone.

## Status in omp

**Status: Not started.** Reusable today: the repository snapshot (`crates/envd/src/vcs/mod.rs:24-37`), the `@memory` role, isolated Director inference, the `omp-secrets` redactor, the prompt-facts and Dynamic-band machinery. Not yet implemented: everything in D1-D9. Mnemopi prompt injection is inactive (no `memory` producer); this ADR does not change that.

## References

[docs/research/memory-backends.md](../research/memory-backends.md); ADR 0012 (convars, `PROJECT` flag); ADR 0015 (Directors); ADR 0024 (roster: no new tool family is added); ADR 0035 (errors); ADR 0036 (Python-only extensions); ADR 0049 (vendors as extensions, `cap-*` for build weight).
