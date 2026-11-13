# 0061. Ratchet: a host-owned held-out gate (omp-evalgate); Skillshare: covered by the signed index plus two small additions

Status: proposed
Date: 2026-11-13
Area: runtime

## Context

Research: [docs/research/ratchet-skillshare.md](../research/ratchet-skillshare.md) (Gap 16 of the upstream gap closure). This record states decisions; the note holds the evidence.

### OMPv1 `/ratchet`

All from can1357/oh-my-pi `c308970263` (v18.20.0); `src/` is `packages/coding-agent/src/`.
- `src/slash-commands/builtin-modes.ts:692` registers `/ratchet`. `prepareRatchet` (210) needs the `eval` and `task` tools (`RATCHET_REQUIRED_TOOLS`, 204), enables a ratchet eval prelude for the session without persisting the setting, and renders a kickoff prompt. The model drives a global `ratchet(flow)` helper (`src/ratchet/prelude-definition.ts`) documented in `src/prompts/tools/ratchet.md`.
- The point: build a trusted eval for one LLM flow, then hillclimb a prompt or config surface one change per round, keeping only changes that also win on held-out cases.
- Everything the model must not argue with is in `src/ratchet/ratchet.ts` (980 lines): the frozen split, the approvals, the gate, the plateau stop.

### What `ratchet.ts` defines (the test oracle for the crate below)

- Split (`splitCases`, 309): needs at least 4 cases; refuses after the baseline is gated; default test fraction 0.4; seeded PRNG `seededRandom` (297, mulberry32: add `0x6d2b79f5`, then the `imul` mixing steps, 300-304); stratified by primary tag: ids sorted, shuffled within a tag bucket (Fisher-Yates from the end), `Math.round(bucket.length * testFraction)` to test; if one side ends up empty, one case moves; both id lists are sorted; the seed is recorded in `split_seed`.
- Noise (`meanAndHalf`, 126): mean of per-case means; with n at least 2, half-width = `tCritical(n - 1) * sqrt(variance / n)` where variance divides by n - 1. `T_CRITICAL` (113-116) is the two-sided 95% Student-t table for df 1 to 30 (12.706, 4.303, 3.182, 2.776, 2.571, 2.447, 2.365, 2.306, 2.262, 2.228, 2.201, 2.179, 2.16, 2.145, 2.131, 2.12, 2.11, 2.101, 2.093, 2.086, 2.08, 2.074, 2.069, 2.064, 2.06, 2.056, 2.052, 2.048, 2.045, 2.042); df above 30 uses 1.96 (123).
- Paired delta (`paired`, 632): per-case candidate minus reference over shared ids; gain is the mean (negated for lower-is-better); verdict `up` when gain minus half-width is above 0, `down` when gain plus half-width is below 0, else `flat`. Lower is better by default for `cost_usd` and `latency_s` (111).
- Gate (`gateVariant`, 727-936). Preconditions: variant names match `baseline` or `v<N>` (110); every (case, rep) slot is scored or recorded as an error; two result rows for a slot are rejected; test-split traces are rejected. Then exactly one of:
  - `baseline`: records the round; warns when a unit-interval target mean is at least 0.95 ("near ceiling", 842) and when train and test means differ by more than the hypotenuse of their half-widths.
  - `errored_only_rerun`: slots that only errored force `rerun`.
  - Otherwise a comparison against the best round, in this order: (1) `guardrail_regressed`: a hold metric is `down` on train or test gives `revert`; (2) `target_regressed`: target `down` on train or test gives `revert`; (3) `keep`: target `up` on train and test; (4) `overfit_suspect`: train `up` only gives `revert`; (5) `test_up_train_flat`: test `up` only gives `rerun`; (6) `no_change_outside_noise` gives `revert`. A hold metric with fewer than 2 measured cases per split refuses and records nothing.
- Counters: `keep` and `baseline` record the best round and reset `flat_rounds`; `revert` increments it; `rerun` leaves it. `plateau` is `flat_rounds >= stop.plateau` (929); the default is 3 (237) and values below 2 are rejected (285). `done` is `plateau` or the round cap `stop.rounds`.
- Approval digests (`stageDigest`, 384-414; `hashPaths`, 366-381): stages `inputs`, `grader`, `plan` (`RATCHET_STAGES`, 20). `inputs` and `grader` hash SHA-256 over the sorted configured paths: for a missing path `missing:<rel>\0`; otherwise for each file under it `<posix relative path>\0`, the file bytes, `\0`. `plan` hashes the goal, reps, stop rule, command, change paths, off-limits, train ids, test ids, and price overrides sorted by model.
- The prompt calls itself "Adapted from the claude-api skill's build-eval, eval-audit, and hillclimb guides (Apache-2.0, github.com/anthropics/skills)".

### OMPv1 Skillshare

- A registry at `https://skills.omp.sh` (`packages/wire/src/skillshare.ts:18`; the server is a separate Go service). Scoped immutable versions (`SKILL_SPEC_RE`, 26), SRI sha512 integrity verified before unpacking (`src/skillshare/installer.ts:102-113`), `skills.json` and `skills.lock.json`, a discovery provider at priority 95 under native skills at 100, script consent (`installer.ts:193`), and a publish-time credential scan (`src/skillshare/pack.ts` with `CREDENTIAL_PATTERNS` from `src/secrets/patterns.ts`). Actions: `publish, version, tag, yank, deprecate, owner, token, import, install, update, uninstall, search, info, list` (`src/cli/skill-cli.ts:41`).

### What omp2 has

- ADW (`crates/adw`): pure state machine, boolean acceptance (`workflow.rs:29` `PhaseKind`, `44` `OnReject`; `run.rs:354`, `396` accept and reject; default cap of 3 attempts at `run.rs:15`). No metric, split or best-so-far, and no re-dispatch of an accepted phase.
- The autoresearch Director (`crates/agent/src/directors/autoresearch.rs`, 106 lines): resume policy; `evaluate` at line 87; it advertises tools `init,run,log_experiment` that do not exist. envd's isolation primitives (`crates/envd/src/vcs/git/mutation.rs`: `create_isolation_branch` 391, `commit_isolation` 407, `rollback_isolation` 432) have no callers.
- Dependency direction, from `cargo metadata --offline --no-deps` on 2026-11-13: `omp-agent` does not depend on `omp-driver` or `omp-envd`; `omp-envd` does not depend on `omp-agent` (ADR 0046 item 2); `omp-driver` depends on both and on `omp-adw`; `omp-adw` depends only on `omp-core`; `omp-contract` depends on `omp-ai`, `omp-catalog`, `omp-dom`, `omp-ext`, `omp-journal`, `omp-session`, `omp-tool` among others.
- ADR 0006 Decision: the host owns session state, inference, policy, approval and journaling; the eval parent bridge is a bounded exception whose operation set "is closed: `__completion__`, `__agent__`, `__workpool__`, `__concurrency__`, `__budget__`, plus extension prelude helpers. Adding one is an amendment to this record". `crates/envd/src/eval/bridge.rs:44-48` lists those names; `crates/envd/src/eval/python_prelude.py` defines the helpers over `_bridge_call` (`completion` at 824, `agent` at 841, `_Budget` at 1112). `crates/envd/src/write_scope.rs` confines writers only for plan-file or read-only-ceiling invocations; there is no project-path deny.
- Model-facing description of a core prelude helper: `EVAL_AGENT_HELPER` in `crates/tools/src/eval.rs:95` (tested at 2361-2371). `PreludeHelperDescription` (`eval.rs:129`) only describes extension helpers.
- Convars are declared with `omp_con::var!` (e.g. `sv_browser_enabled`, `crates/envd/src/browser_daemon.rs:62-70`; `crates/agent/src/vars.rs:245`). `register_dynamic_var` at `crates/agent/src/loop.rs:5277` appears only in a test module.
- A tool named `ratchet` already exists in the repository as a dev lint: `tools/lintx/src/ratchet.rs`, a per-crate count ratchet for error-formatting rules.
- No `ask`-style user prompt exists on the eval bridge: `python_prelude.py` has no approval call; `ask` is a model tool (`crates/tools/src/ask.rs`).
- Catalog pricing: `crates/catalog/src/pricing.rs`, `Pricing::cost(UsageDimensions) -> NanoUsd` (192). How a served model id maps to a `Pricing` was not traced.
- Signed extension index: `crates/ext/src/index.rs` (`SignedIndex` 180, `IndexRelease` 98, `IndexArtifact` 76, `search` 383) and `crates/ext/src/trust.rs` (publisher keys, rotation, signature verification); `omp ext` in `crates/app/src/ext_cli/mod.rs`: `enum ExtCommand` at 241, `Lock` 275, `Upgrade` 281, `Publish(ExtPublishArgs)` 305, `Search` 308, `struct ExtNewArgs` 417, `struct ExtPublishArgs` 638. `ResourceFamily::Skills`: `crates/ext/src/config.rs:609`. Skill discovery is first-wins with extension skills below authored roots (`crates/driver/src/discovery/skills.rs:208-218`, collision skip at 260).
- Skill-only packages: the parse layer allows them (`config.rs:1366` default-empty `entry`; `validate_static_row` at 1906) but `validate_manifest` in `crates/driver/src/discovery/native.rs:506` rejects an empty `entry` and `crates/envd/src/worker.rs:3160` requires a nonempty one; `omp ext new` (`ext_cli/mod.rs:1283`) scaffolds only Python.
- Credential patterns: `crates/secrets/src/builtins.rs:7-13` defines `CREDENTIAL_PATTERN` (GitHub `gh[opusr]_`, `github_pat_`, GitLab `glpat-`, OpenAI `sk-proj-`/`sk-`, Anthropic `sk-ant-`) used by `credential_rules()` for masking. `omp-secrets` depends only on `omp-core`; `omp-app` does not depend on it today. Upstream's list is wider: `secrets/patterns.ts:60-96` also has AWS keys, Google API keys, Slack, npm, Stripe, Hugging Face, SendGrid, JWT, Bearer tokens and PEM private keys.

## Decision

### Skillshare (class a for the core; two real additions)

**D1. The signed-index core is covered; two items are real gaps.** Class a: distribution, immutable versions, integrity, lock, upgrade, search, publish, yank, consent, discovery order. Those are provided by `crates/ext/src/index.rs` and `crates/ext/src/trust.rs` plus `omp ext`, with a stronger trust model than Skillshare's SRI (Ed25519 publisher keys with rotation, a signed index, capability digests). Class d, two items: skill-only packaging (the load path rejects an empty `entry`) and a publish-time credential scan. This record does not present Skillshare as one class.

**D2. Not adopted, with reasons.**
- The registry server `skills.omp.sh`, Stencil-bound scopes, publish tokens, download counts, README rendering: a hosted service and an account system, not engine code; omp2's index is static signed snapshots.
- `omp skill import <file.skill>`: exists to publish a Claude archive to the registry; Claude plugin skills are already read through `claude_plugin.rs`.
- GitHub OIDC verified provenance, dist-tags and `deprecate`: registry-side concerns; the index has `attested`, `yanked` and a publisher key.
- A second integrity or signing scheme, and a `skills.json`/`skills.lock.json` pair beside `omp ext lock`: the signed index is stronger and already present.

**D3. S1: publish-time credential scan.**
- The pattern source is `omp-secrets`. `credential_rules()` keeps its masking behaviour unchanged (existing behaviour is not altered). A new additive function `publish_scan_patterns()` in `crates/secrets/src/builtins.rs` returns the existing `CREDENTIAL_PATTERN` plus the upstream shapes not yet covered (AWS, Google, Slack, npm, Stripe, Hugging Face, SendGrid, JWT, Bearer, PEM private key), written in Rust with upstream `secrets/patterns.ts:60-96` as the reference and no generic keyword or entropy rules (upstream's own comment: a coding agent must still read identifiers like `tokenizer`).
- `omp-app` gains an `omp-secrets` dependency (no cycle: `omp-secrets` depends on `omp-core` only).
- The CLI change: `ExtPublishArgs` (`ext_cli/mod.rs:638`) gains `--scan-secrets` and `--allow-secrets`; `ExtCommand::Publish(ExtPublishArgs)` (`305`) is otherwise unchanged. Default-off convar `sv_ext_publish_scan` makes the scan the default. With neither, publish output is unchanged.
- A finding fails the publish unless `--allow-secrets`; `--dry-run` reports without uploading.

**D4. S2: skill-only packages.** `omp ext new --skills <id>` (a new field on `ExtNewArgs`, `ext_cli/mod.rs:417`; `ext new` without it is unchanged) emits a manifest with `skills` static rows, a `SKILL.md` and a documented stub `entry` module, so a skill pack passes today's `validate_manifest`. Relaxing `native.rs:506` and `worker.rs:3160` so an empty `entry` is legal, skipping the extension-host spawn, is a load-path change and is not decided here (O8).

### Ratchet

**D5. Names.** The crate is `omp-evalgate` (`crates/evalgate`), the eval prelude helper is `evalgate(flow)`, the prompt mode is `evalgate`, the convars are `sv_evalgate_*`, the host bridge op is `__evalgate__`. This avoids the collision with `tools/lintx/src/ratchet.rs`. The user-facing slash command keeps the `/ratchet` spelling that v1 users know and maps to the `evalgate` mode (O7).

**D6. A pure crate, justified by dependency direction.** The gate has two consumers that cannot depend on each other: the host bridge handler (driver side, which also needs `omp-envd`) and the Director (`omp-agent`). `omp-agent` and `omp-envd` do not depend on one another, so shared logic must sit below both. `omp-evalgate` therefore depends only on `omp-core` and serde, like `omp-adw`. It is not placed in `omp-contract`: that crate carries the wire contract and already pulls in ai, catalog, dom, ext, journal, session and tool. Rejected, as before: a second Director, a second workflow engine, ratchet as an ADW recipe (boolean acceptance, default 3 attempts, no loop after accept).

**D7. Evidence is host-owned, outside the workspace.**
- Authoritative state lives in host-owned session storage, not in the project tree: flow definition, frozen split, approvals and their digests, the round table, the best round, and a hashed snapshot of the result rows each round was gated on. ADR 0006 already keeps session state on the host and out of the sandbox.
- The workspace holds only what the user's runner needs: cases, harness, and runner output (`results.jsonl`, `errors.jsonl`, train `traces/`). At gate time the helper sends the rows through the bridged op; the host snapshots and hashes them; later workspace edits do not change a recorded round. Approval digests are computed by the host reading the workspace files at approve and gate time.
- Limits, stated plainly: the gate trusts rows the runner wrote. A model that controls the runner could write fake rows; the gate cannot detect that. What it does enforce: the approved command and harness digests, completeness against approved reps and split ids, and refusal of test-split traces. The model also cannot edit the split, approvals, rounds or best-so-far, because it has no write path to the host store. Held-out case contents remain readable in the workspace, as in v1 (O1).

**D8. A new bridged operation, with an ADR 0006 amendment as a prerequisite.** The bridge's operation set is closed (ADR 0006 Decision, bridge item 4). `__evalgate__` must be added by amending ADR 0006 before slice R2. Proposed amendment text:
  "The eval parent bridge gains one operation, `__evalgate__`, dispatched only when the grant's capability set names it. The callee is host code. It reads and writes the flow store under host-owned session storage; the stub supplies only flow name, variant name, bounded result rows and a requested stage, and receives only typed results. Approval for a stage is asked by the host, never by the stub. The operation is withheld when `sv_evalgate_enabled` is false."
Registration points: the name in `bridge.rs` beside `BUDGET` (48); a capability bit beside `with_budget()` (998); `evalgate` added to `PRELUDE_RESERVED_NAMES` (`bridge.rs:54`) so extensions cannot shadow it.

**D9. Where `evalgate(flow)` is defined and how it is described.** The helper is defined in `crates/envd/src/eval/python_prelude.py` beside `completion` (824) and `_Budget` (1112), over `_bridge_call("__evalgate__", ...)`, with async methods `init`, `plan`, `split`, `approve`, `check`, `gate`, `train`, `status`. It is described to the model by a new const in `crates/tools/src/eval.rs` beside `EVAL_AGENT_HELPER` (95), emitted only when the helper is available, following the available/unavailable tests at 2361-2371. `PreludeHelperDescription` is not used: it is for extension helpers. The host side is a new method on the `ParentSessionHost` trait (`crates/envd/src/eval/bridge.rs`) that the driver implements, because only the driver can see both the storage and the Director.

**D10. Approvals and pricing are slices to build, not existing APIs.**
- Approvals: no user-prompt call exists on the eval bridge. R2 builds a host-side stage approval that raises a question to the user through the host's existing ask or approval machinery. Whether a bridge handler can raise that dialog is unverified and is O4. Headless sessions with no such channel refuse to build or approve.
- Pricing: `Pricing::cost` exists (`crates/catalog/src/pricing.rs:192`). Mapping a row's served model and usage to a `Pricing` is built in R2; user price overrides (v1's `prices`) remain for models the catalog cannot price. A cost guardrail on an unpriced model makes the gate refuse, as v1 does.

**D11. Gate and flag.** `sv_evalgate_enabled` (bool, default false) declared with `omp_con::var!` in `omp-envd` beside the eval bridge. The helper, the bridge op and `/ratchet` do not exist while it is false. `sv_evalgate_stall_judge` (default false) gates R5. No roster family, no tool, no wire schema (ADR 0024); no Cargo feature is needed because `omp-evalgate` adds no optional dependency edge (ADR 0049 reserves `cap-*` for those).

**D12. Keep and revert are the model's patch operations after the gate's decision.** v1 reverts with `git apply -R` of the round's patch. The gate records only what it is told through `gate()`, and best-so-far moves only on `keep` or `baseline`, so a model that ignores `revert` cannot improve its recorded score. Ratchet does not depend on autoresearch's missing tools or on the isolation primitives.

**D13. The Director loop depends on a spike.** The existing autoresearch Director would gain an evalgate engagement that continues while the latest recorded outcome says not done. That requires a Director to read a typed journal entry written by a bridge handler. Spike E1 proves or disproves it before R4. If it cannot, the continuation is dropped: the op's return value carries `done`, the prompt mode tells the model to stop, and ratchet still works.

**D14. Autoresearch is finished separately.** Its advertised tools and the unwired isolation primitives are a gap of their own; the owner schedules it; ratchet does not wait on it.

## Order of work

1. **P0 (prerequisites, no code).** Owner accepts the ADR 0006 amendment (D8) and the host-owned evidence layout (D7). R0 may proceed in parallel because it touches neither.
2. **R0.** The pure crate.
3. **E1.** The Director spike (D13). Output: a short result note; no product code.
4. **R1.** Host storage and approval digests, after P0.
5. **R2.** The bridge op, the helper, approvals, pricing, behind `sv_evalgate_enabled`.
6. **R3.** `/ratchet` and the prompt mode.
7. **R4.** The Director engagement, only if E1 passed.
8. **R5.** Stall categorisation, after ADR 0060 slice L5 (typed judgment).
9. **S1 and S2.** Independent of ratchet; any time.
10. Autoresearch's tools and isolation wiring: independent, owner-scheduled.

## Slices

Each slice is one PR; tests are written first and not edited to pass.

**R0. `omp-evalgate`, pure.**
- Placement: workspace member `crates/evalgate` (`members = ["crates/*"]` already covers it); a dependency entry in root `Cargo.toml` beside `omp-kb` (line 136); `[lints] workspace = true`; dependencies `omp-core`, serde, serde_json only. No I/O, no clock, no spawning.
- Oracle: `ratchet.ts` at `c308970263`. Golden vectors are produced once by running the upstream functions (`seededRandom`, `splitCases`, `meanAndHalf`, `paired`, `gateVariant`'s decision logic) over fixed inputs and checked in as JSON with a header naming the upstream file and commit. Floating-point operations follow upstream order (sum, then divide; variance divides by n - 1) so results are bit-identical.
- Tests: `mulberry32_matches_upstream_vectors`, `split_is_deterministic_for_a_seed`, `split_is_stratified_by_tag`, `split_refuses_fewer_than_four_cases`, `split_refuses_after_baseline`, `split_moves_one_case_when_a_side_is_empty`, `half_width_uses_t_table_then_1_96`, `paired_verdict_up_down_flat_at_the_boundary`, `lower_is_better_for_cost_and_latency_by_default`, `errored_only_slots_force_rerun`, `decision_guardrail_regressed_reverts`, `decision_target_regressed_reverts`, `decision_keep_needs_train_and_test_up`, `decision_overfit_suspect_reverts`, `decision_test_up_train_flat_reruns`, `decision_no_change_outside_noise_reverts`, `unmeasured_guardrail_refuses_and_records_nothing`, `duplicate_slot_is_rejected`, `test_trace_is_rejected`, `baseline_near_ceiling_warns`, `baseline_split_mismatch_warns`, `plateau_counter_rules` (keep and baseline reset, revert increments, rerun unchanged), `plateau_below_two_is_rejected`, `round_cap_ends_the_climb`, `results_are_bit_identical_across_runs`.

**E1. Director spike.** Prove a typed journal entry written by a host-side bridge handler is readable by the Director in the existing Director test harness (`crates/agent/tests/directors/misc.rs` pattern). Acceptance: a passing throwaway test or a written reason it cannot work, and the R4 design chosen from it.

**R1. Host-owned store and approval digests.**
- Digest bytes: for `inputs` and `grader`, SHA-256 over the sorted configured paths; a missing path contributes `missing:<rel>\0`; otherwise each file contributes `<posix relative path>\0`, its bytes, `\0`, files in sorted relative-path order. For `plan`: SHA-256 over a fixed canonical layout (`omp-evalgate` defines it; it need not equal v1's JSON bytes, only the same staleness behaviour) of goal, reps, stop rule, command, change paths, off-limits, train ids, test ids, and price overrides sorted by model.
- Storage: host-owned, outside the workspace, keyed by project and flow, atomic writes. The exact directory comes from `omp_core::dirs` and is chosen in this slice [API not traced].
- Tests: `edit_to_cases_makes_inputs_approval_stale`, `edit_to_harness_makes_grader_approval_stale`, `plan_change_makes_plan_approval_stale`, `missing_path_hashes_differently_from_empty_file`, `digest_ignores_file_order_on_disk`, `split_cannot_change_after_baseline`, `changing_cases_after_baseline_needs_a_new_flow`, `corrupt_state_is_refused`, `state_is_not_under_the_workspace`, `recorded_round_ignores_later_edits_to_results_files`.

**R2. `__evalgate__` and `evalgate(flow)`, behind `sv_evalgate_enabled` (default false).** Requires the ADR 0006 amendment.
- Pieces: the op and capability bit in `bridge.rs`; the reserved name; the Python helper; the description const in `tools/src/eval.rs`; the `ParentSessionHost` method and its driver implementation; stage approval (D10); pricing (D10).
- Tests: `helper_and_op_absent_when_convar_false`, `op_withheld_unless_grant_names_it`, `each_method_round_trips`, `no_method_accepts_a_verdict_from_the_stub`, `model_cannot_write_the_host_store`, `headless_without_a_dialog_refuses_to_build_or_approve`, `unpriced_model_under_cost_guardrail_refuses`, `extension_cannot_shadow_evalgate`.

**R3. `/ratchet` and the prompt mode, behind `sv_evalgate_enabled`.**
- The slash command requires the `eval` and `task` tools, enables the helper for the session without persisting the setting, binds `ai_prompt_mode` to `evalgate` (the pattern `Autoresearch` uses in its `binds`), and shows the mode prompt, written fresh for omp2.
- Tests: `refuses_without_eval_and_task`, `does_not_persist_the_setting`, `binds_prompt_mode`, `mode_prompt_names_the_stops_and_the_status_line`.

**R4. Director engagement, only if E1 passed, behind `sv_evalgate_enabled`.** Tests in the Director harness: `continues_while_not_done`, `stops_at_plateau`, `stops_at_round_cap`, `yields_to_queued_user_input`; the existing autoresearch tests pass unchanged.

**R5. Stall categorisation, behind `sv_evalgate_stall_judge` (default false) and ADR 0060 slice L5.** Frozen five-way rubric (`artifact_gap`, `grader`, `harness`, `structural`, `variance`). Tests: `rubric_is_frozen`, `low_confidence_verdicts_are_surfaced_not_applied`, `gate_decision_identical_with_convar_on_or_off`.

**S1. Publish-time credential scan.** `publish_scan_patterns()` in `omp-secrets`; `--scan-secrets` and `--allow-secrets` on `ExtPublishArgs`; `sv_ext_publish_scan` (default false).
- Tests: `credential_rules_masking_is_unchanged` (existing behaviour), `each_pattern_matches_a_positive_sample_and_skips_identifiers`, `scan_reports_under_the_flag`, `finding_fails_publish_unless_allow_secrets`, `no_flag_publish_output_is_unchanged`, `dry_run_reports_without_uploading`.

**S2. `omp ext new --skills`.** Tests: `scaffold_passes_validate_manifest`, `discovery_lists_the_scaffolded_skill`, `scaffold_declares_no_hard_or_hook_rows`, `ext_new_without_the_flag_is_unchanged`, and an end-to-end `install_lock_sync_with_a_stub_entry_skill_pack` (not run before this record).

## Upstream origin and license

Ratchet logic is ported from `src/ratchet/ratchet.ts`, `src/ratchet/prelude-definition.ts` and `src/slash-commands/builtin-modes.ts` (`prepareRatchet`) at `c308970263`. Upstream is MIT (© 2025 Mario Zechner, © 2025-2026 Can Bölük, © 2026 Stencil Labs, Inc.); omp2's `LICENSE` is MIT with © 2026 Stencil Labs, Inc. only. Ported modules name their origin path and commit in their headers. `src/prompts/tools/ratchet.md` says it is adapted from Apache-2.0 material (anthropics/skills); the omp2 prompt is written fresh from the contract; any reused wording needs Apache-2.0 attribution. Skillshare files are not ported. The credential patterns are re-expressed in Rust from `secrets/patterns.ts` (MIT, same notice). No `pi` identity appears in names: `omp-evalgate`, `sv_evalgate_*`, `sv_ext_publish_scan`.

## Consequences

- Skillshare needs two small `omp ext` additions and no new engine; the signed index remains the one distribution path.
- Ratchet adds one pure crate, one bridged operation (after an ADR 0006 amendment), one prelude helper and one prompt mode, all default off. Nothing joins the wire roster.
- The verdict, split, approvals and best-so-far are host-owned and cannot be edited by the model; the gate's one trust limit is the runner's rows (D7).
- The Director gains an engagement only if the spike passes.

## Review (EGM, 2026-11-13)

Critic verdict, and resolution:
- Project-path deny does not exist (ADR 0006, `write_scope.rs`): accepted. D7 moves the verdict and its inputs into host-owned storage and D8 adds a bridged operation, with the ADR 0006 amendment as prerequisite P0.
- Order: R1 no longer lays files out in the tree; P0 comes first; E1 runs before R4 (Order of work).
- Skillshare headline: D1 states class a for the signed-index core (`trust.rs`, `index.rs`) plus two class d items.
- New crate by dependency direction: D6.
- Name collision with `tools/lintx/src/ratchet.rs`: D5 renames to `omp-evalgate`.

Readiness verdict (provisional; the reviewer read fragments), and resolution:
- R0: the six comparison branches are named, the constants and their upstream lines are given, the crate placement is named, `ratchet.ts` is the oracle (Context, R0).
- R1: the digest bytes are defined with `stageDigest` as reference; layout moved out of the workspace (R1).
- R2: the helper's definition and description points are named (D8, D9); approvals and pricing are marked slices to build (D10); the convar uses `sv_*` under the new name via `omp_con::var!` (D11). The `register_dynamic_var` call at `loop.rs:5277` is test code and is not the production pattern.
- S1: the pattern set comes from `omp-secrets` plus an additive function (D3); the command surface is `ExtCommand::Publish(ExtPublishArgs)` (`ext_cli/mod.rs:305`, `638`). A correction to the research note: it said no shared pattern set exists; `crates/secrets/src/builtins.rs:7-13` has a narrower one.

## Open questions for the owner

- **O1. Held-out case contents stay readable in the workspace, as in v1.** Recommendation: accept for now and say so in the mode prompt; stronger isolation (running the grader host-side or in a separate sandbox) is a later ADR.
- **O2. Does the owner accept the ADR 0006 amendment adding `__evalgate__` (D8)?** It gates R2.
- **O3. Where exactly does host storage live (R1)?** Recommendation: under the host data directory from `omp_core::dirs`, keyed by project and flow, so flows survive session resume.
- **O4. Can a bridge handler raise a user dialog?** Not traced. Recommendation: if not, approvals use an ask-style round trip through the model's session, with the digest checked by the host.
- **O5. How does a served model id map to a `Pricing`?** Recommendation: add a lookup in `omp-catalog` in R2 if none exists.
- **O6. E1 outcome decides R4.** Recommendation: if the spike fails, drop the Director engagement; the gate is complete without it.
- **O7. Keep the `/ratchet` spelling for users?** Recommendation: yes; internal names are `evalgate`.
- **O8. Skill-only packages: stub entry or an empty-`entry` load path?** Recommendation: stub entry (S2); the load-path change needs its own decision and a test that no extension host starts.
- **O9. Root `LICENSE` upstream notices.** Same as ADR 0060 O5.

## Status in omp

**Status: Not started.** Reusable: ADW acceptance and replay; the autoresearch Director's resume loop; `agent()` and `completion()` in the eval prelude; `Pricing::cost`; the signed extension index and `omp ext`; `omp-secrets` credential rules. Not implemented: the gate, the store, the bridge op, the helper, `/ratchet`, the credential scan on publish, the skill-only template, autoresearch's tools. Unverified: whether a bridge handler can raise a dialog (O4); the host storage directory API (O3); the served-model-to-pricing lookup (O5); whether a Director can read a handler-written journal entry (E1); install, lock and sync with a stub-entry skill pack.

## References

[docs/research/ratchet-skillshare.md](../research/ratchet-skillshare.md); ADR 0003 (session tree); ADR 0006 (host owns state; closed bridge set); ADR 0015 (Directors); ADR 0024 (small roster); ADR 0035 and 0036 (Rust engine, Python extensions); ADR 0046 (envd and agent do not depend on each other); ADR 0049 (`cap-*` features); ADR 0051 to 0053 (ADW); ADR 0060 (typed judgment); `crates/adw`; `crates/agent/src/directors/autoresearch.rs`; `crates/envd/src/eval/bridge.rs`; `crates/envd/src/eval/python_prelude.py`; `crates/tools/src/eval.rs`; `crates/secrets/src/builtins.rs`; `crates/ext/src/index.rs`; `crates/ext/src/trust.rs`; `crates/app/src/ext_cli/mod.rs`; can1357/oh-my-pi `c308970263` `packages/coding-agent/src/ratchet/`, `skillshare/`, `secrets/patterns.ts` and `packages/wire/src/skillshare.ts`.
