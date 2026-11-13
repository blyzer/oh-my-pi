# 0061. Ratchet is a deterministic held-out gate on the autoresearch loop; Skillshare is covered by the signed index

Status: proposed
Date: 2026-11-13
Area: runtime

## Context

- Research: [docs/research/ratchet-skillshare.md](../research/ratchet-skillshare.md) (Gap 16 of the upstream gap closure). This record states the decisions; the note holds the evidence.
- OMPv1 `/ratchet` [code-v1, can1357/oh-my-pi `c308970263`, v18.20.0; `src/` is `packages/coding-agent/src/`]: `src/slash-commands/builtin-modes.ts:692` registers it; `prepareRatchet` (line 210) needs the `eval` and `task` tools (`RATCHET_REQUIRED_TOOLS`, line 204), enables a ratchet eval prelude for the session without persisting the setting, and renders a kickoff prompt. The model drives a global `ratchet(flow)` helper documented in `src/prompts/tools/ratchet.md`. The point: build a trusted eval for one LLM flow, then hillclimb a prompt or config surface one change per round, keeping only changes that also win on held-out cases. Everything the model must not argue with lives in `src/ratchet/ratchet.ts`: a frozen stratified split with a recorded seed (`splitCases` line 309, `seededRandom` line 297); content-hashed approvals for `inputs`, `grader` and `plan` (`RATCHET_STAGES` line 20, `stageDigest` line 384); the gate (`gateVariant` line 727) with completeness and held-out-leak checks, paired per-case deltas against the best round (`paired` line 632) using a Student-t half-width as the noise floor, and a decision of `baseline`, `keep`, `revert` or `rerun`; and a plateau stop (`flat_rounds >= stop.plateau`, line 929; `plateau >= 2` validated at line 285). A fresh analyzer sees train traces only. A model categorises stalled failures with `judge_batch` and a frozen five-way rubric.
- Skillshare [code-v1]: an npm-style registry at `https://skills.omp.sh` (`packages/wire/src/skillshare.ts:18`; the server is a separate Go service). Scoped immutable versions (`SKILL_SPEC_RE` line 26), SRI sha512 integrity verified before unpacking (`src/skillshare/installer.ts:102-113`), `skills.json` and `skills.lock.json`, a provider at discovery priority 95 under native skills at 100, script consent (`installer.ts:193`), a publish-time credential scan (`src/skillshare/pack.ts`, `CREDENTIAL_PATTERNS`), and actions `publish, version, tag, yank, deprecate, owner, token, import, install, update, uninstall, search, info, list` (`src/cli/skill-cli.ts:41`).
- omp2 ADW (`crates/adw`, `crates/driver/src/adw`) is a pure state machine with boolean acceptance: `PhaseKind` (`workflow.rs:29`), `OnReject::Retry|Correct|Halt` (`workflow.rs:44`), `Run::accept`/`reject` (`run.rs:354`, `396`), `replay` (`replay.rs:104`), a default cap of 3 attempts (`run.rs:15`, enforced at 317, 416, 430). It has no metric, split or best-so-far, and no re-dispatch of an accepted phase except when an upstream phase is re-accepted.
- The autoresearch Director (`crates/agent/src/directors/autoresearch.rs`, 106 lines) is a resume policy: it binds `ai_prompt_mode = autoresearch`, arms after a `log_experiment` call, and `evaluate` (line 87) returns `Done` at the iteration cap or `Continue` with a reminder. It advertises `init,run,log_experiment`, but no such tools exist in `crates/`. envd has reviewed isolation primitives (`crates/envd/src/vcs/git/mutation.rs`: `GitMutationConsumer::Autoresearch` line 177, `create_isolation_branch` 391, `commit_isolation` 407, `rollback_isolation` 432) with no callers.
- Other pieces that exist: an analyzer subagent through `agent(...)` in the eval prelude (`crates/envd/src/eval/python_prelude.py:841`); `@omp.prelude` helpers (`docs/py/16-prelude.md`); the `ask` tool; catalog pricing (`crates/catalog/src/pricing.rs`, `Pricing::cost` line 192).
- The signed extension index (`crates/ext/src/index.rs`): `SignedIndex` (180), `IndexRelease` (98) immutable with `yanked` and `attested`, `IndexArtifact` (76) hash-pinned with a publisher signature, `search` (383). `omp ext` (`crates/app/src/ext_cli/mod.rs`) has `install`, `uninstall`, `list`, `info`, `search` (308), `publish` (305), `lock` (275), `sync`, `upgrade` (281), `pin`, `index add|remove|list`. `ResourceFamily::Skills` is a package-owned family (`crates/ext/src/config.rs:609`). Skill discovery is ordered and first-wins, with extension skills below authored roots (`crates/driver/src/discovery/skills.rs:208-218`, collision skip at 260).
- Skill-only packages: the parse layer allows them (`DeploymentManifest.entry` defaults empty, `config.rs:1366`; `validate_static_row`, `config.rs:1906`), but the load path rejects an empty `entry` (`crates/driver/src/discovery/native.rs:506`, `crates/envd/src/worker.rs:3160`), and `omp ext new` (`ext_cli/mod.rs:1283`) only scaffolds Python.
- No shared credential-pattern set exists for `omp ext publish` (`crates/envd/src/security_scan.rs` is a repository-scan service; other hits are unrelated).

## Decision

### Skillshare

**D1. Skillshare is covered; do not build a registry.** Distribution, immutability, integrity, lock, upgrade, search, publish, yank and consent already exist in the signed extension index and `omp ext`, with a stronger trust model (Ed25519 publisher keys, key rotation, a signed index, capability digests). Skills already ship as the `Skills` resource family inside extensions.

**D2. Not adopted, with the reason.**
- The registry server `skills.omp.sh`, scopes bound to Stencil accounts, publish tokens, download counts and README rendering: a hosted service and an account system, not engine code; omp2's index is static signed snapshots (`IndexConfigEntry` HTTPS URLs).
- `omp skill import <file.skill>`: it exists to publish a Claude `.skill` archive to the registry; without the registry it has no purpose, and Claude plugin skills are already read through `claude_plugin.rs`.
- Verified provenance through GitHub OIDC, dist-tags and `deprecate`: server-side or registry concerns; the index has `attested` and `yanked` and a publisher key.
- A second integrity or signing scheme, and a `skills.json`/`skills.lock.json` pair beside `omp ext lock`: duplicates stronger machinery.

**D3. Two small additive changes to `omp ext`.**
- **Publish-time credential scan.** `omp ext publish` (and `--dry-run`) scans the files it packs for credential-shaped strings and reports findings. A small pattern list owned by `omp-ext` (no shared set exists). Opt-in by flag `--scan-secrets` and by default-off convar `sv_ext_publish_scan`, so existing publish behaviour is unchanged; with the scan on, a finding fails the publish unless `--allow-secrets` is passed.
- **Skill-only packaging.** An `omp ext new --skills <id>` template (new flag; today's `ext new` is unchanged) that emits a manifest with `skills` static rows, a `SKILL.md` and a documented stub `entry` module, so a skill pack passes today's load path. An empty-`entry` load path (relaxing `native.rs:506` and `worker.rs:3160` and skipping the extension-host spawn) is not decided here; see O5.

### Ratchet

**D4. Ratchet is a deterministic gate plus a prompt mode; it is not a second Director, workflow engine or tool.** The gate decides keep, revert or rerun from recorded result rows by rule. A model never makes the decision; a model appears only in stall categorisation (D10), behind the typed-judgment prerequisite of ADR 0060.

**D5. The gate and state live in a new pure Rust crate, `omp-ratchet`.** In the style of `omp-adw`: no I/O, no clock, no spawning; every transition a function of recorded facts. Ported logic: seeded stratified split frozen after the baseline gate; paired per-case deltas against the best round; noise floor as a Student-t half-width (`meanAndHalf`); the decision table (guardrail `down` gives `revert`; target `down` gives `revert`; target `up` on train and test gives `keep`; train `up` only gives `revert` as overfit-suspect; test `up` only gives `rerun`; otherwise `revert`; slots that only errored force `rerun`); completeness and held-out-leak checks; the near-ceiling and train/test-mismatch warnings; plateau and round-count stop. The gate takes cost from `omp-catalog` `Pricing::cost` with user price overrides only for models the catalog cannot price. The statistics are computed in integer or fixed-order floating point with a fixed reduction order so results replay identically.

**D6. The model cannot hold or edit the verdict.** State is written only by the host. The model-facing surface is a core eval prelude helper, `ratchet(flow)` (async methods `init`, `plan`, `split`, `approve`, `check`, `gate`, `train`, `status`), forwarding to the host through the same bridge `agent()` uses (`python_prelude.py:841`). It is a prelude helper, not a tool, so it adds no wire schema and no roster family (ADR 0024). It is not an extension prelude: an extension helper could write its own state, which would defeat "host owns truth". Approvals go through `ask` and are bound to content hashes; an edit makes them stale. Headless sessions without `ask` never build or approve.

**D7. `/ratchet` arms a prompt mode.** `/ratchet` requires the `eval` and `task` tools, enables the helper for the session without persisting the setting, and binds `ai_prompt_mode` to a `ratchet` mode, the pattern the autoresearch Director uses (`autoresearch.rs` `binds`). The prompt is written fresh for omp2 from the contract, not copied from v1 (see license).

**D8. The loop is the existing Director, not a new one.** The autoresearch Director gains a ratchet engagement: it continues while the gate's latest recorded outcome says not done, stops on `plateau` or the round cap, and keeps its user-input yield. How the Director learns the outcome is O3. Keep and revert are ordinary patch operations the model runs after the gate decides (v1 uses `git apply -R` on the round's patch), so ratchet does not depend on the isolation primitives or on the missing autoresearch tools (O1).

**D9. Autoresearch itself is finished separately, in its own change.** The advertised `init`, `run`, `log_experiment` tools do not exist and the isolation primitives have no callers. That is a defect or gap in autoresearch, not part of Gap 16.

**D10. Stall categorisation comes last.** A frozen five-way rubric (`artifact_gap`, `grader`, `harness`, `structural`, `variance`) judged by a model over each remaining train failure, behind the typed-judgment API of ADR 0060 slice 3 and a default-off convar. Until then the prompt tells the model to categorise by reading, and the gate is unaffected.

## Rejected options

- **A registry client for `skills.omp.sh`.** A hosted dependency, an account system and a second signing scheme (D2).
- **A second `skills.lock.json` or integrity scheme.** The signed index is stronger.
- **Ratchet as a Python extension helper (`@omp.prelude`, ADR 0036).** The helper could write its own state; the trust model fails.
- **Ratchet as ADW recipes only.** Boolean acceptance, a default cap of 3 attempts and no re-dispatch after accept give no statistics, no split and no best-so-far.
- **A second Director or a ratchet workflow engine.** Duplicates the keep/revert loop.
- **Ratchet as a new tool.** A tool would take a wire slot (ADR 0024) and the verdict must not be model-callable state.
- **Make ratchet wait on the autoresearch tool layer.** Ratchet does not use those tools (D8).
- **Copy v1's prompt.** It is adapted from Apache-2.0 material (see license).

## Slices

Each slice is one PR; tests are written first and not edited to pass. `omp-ratchet` is not linked into `omp-app` until slice R2, so R0 and R1 are unreachable and need no gate.

- **R0. `omp-ratchet` pure gate.** Split, paired deltas, noise floor, decision, completeness, plateau. Tests: the same seed and case list give the same split, stratified by tag and frozen after baseline; `paired` gives `up`, `down`, `flat` at the boundary where gain minus half-width crosses 0 (hand-computed t values); each of the six decision branches; a guardrail with fewer than 2 measured cases per split refuses and records nothing; two result rows for one slot are rejected; a slot with only an error forces `rerun`; a test-split trace is rejected; near-ceiling and train/test-mismatch warnings; `flat_rounds` increments on `revert`, resets on `keep` and `baseline`, is unchanged on `rerun`; `plateau < 2` is rejected; `stop.rounds` ends the climb; results are bit-identical across runs.
- **R1. State and approvals.** On-disk layout `.omp/ratchet/<flow>/` (`_state.json`, `baseline/`, `vN/` with `results.jsonl`, optional `errors.jsonl`, `traces/` train only), content-hashed approvals, atomic writes. Tests: an edit to the cases, grader or plan makes its approval stale; changing cases after baseline requires a new flow; a corrupt or hand-edited `_state.json` is refused; the split cannot change after baseline.
- **R2. The `ratchet(flow)` helper, behind `sv_ratchet_enabled` (default false).** Core eval prelude facade over the host, approvals through `ask`. Tests: with the convar false the helper does not exist; with it true each method round-trips; no method returns or accepts a verdict from the model; headless without `ask` refuses to build or approve; pricing comes from the catalog and a model it cannot price requires an override or the gate refuses a cost guardrail.
- **R3. `/ratchet` and the prompt mode, behind `sv_ratchet_enabled`.** Tests: the command refuses without `eval` and `task` and does not persist the setting; it binds `ai_prompt_mode` to `ratchet`; the mode prompt names the stops and the one-line status contract.
- **R4. Director engagement, behind `sv_ratchet_enabled`.** Tests in the existing Director harness (`crates/agent/tests/directors/misc.rs` pattern): continues while not done; stops at plateau and at the round cap; yields to queued user input; existing autoresearch tests pass unchanged.
- **R5. Stall categorisation, behind `sv_ratchet_stall_judge` (default false) and ADR 0060 slice 3.** Tests: a frozen rubric; low-confidence verdicts are surfaced, not auto-applied; the gate decision is identical with the convar on or off.
- **S1. Publish-time credential scan, behind `--scan-secrets` and `sv_ext_publish_scan` (default false).** Tests: a wheel containing a key-shaped string is reported under the flag and fails the publish unless `--allow-secrets`; with neither, publish output is unchanged byte for byte; `--dry-run` reports without uploading.
- **S2. `omp ext new --skills` template.** Tests: the scaffold passes `validate_manifest` and discovery lists its skill; `ext new` without the flag is unchanged; the scaffold declares no `hard` or `hook` rows. Whether install, lock and sync work end to end with the stub was not run and is part of the acceptance test.

## Upstream origin and license

Ratchet logic is ported from can1357/oh-my-pi `c308970263`: `src/ratchet/ratchet.ts`, `src/ratchet/prelude-definition.ts`, `src/slash-commands/builtin-modes.ts` (`prepareRatchet`). Upstream is MIT (© 2025 Mario Zechner, © 2025-2026 Can Bölük, © 2026 Stencil Labs, Inc.); omp2's `LICENSE` is MIT, © 2026 Stencil Labs, Inc. Ported modules name their origin path and commit in their headers. `src/prompts/tools/ratchet.md` states it is "Adapted from the claude-api skill's build-eval, eval-audit, and hillclimb guides (Apache-2.0, github.com/anthropics/skills)"; the omp2 prompt is written fresh from the contract to avoid carrying that wording, and if any is reused it needs Apache-2.0 attribution. Skillshare files (`src/skillshare/*`, `packages/wire/src/skillshare.ts`) are not ported; the credential-shape idea in `pack.ts` is reimplemented against omp2's own pattern list. No `pi` identity appears in ported names: crate `omp-ratchet`, convars `sv_ratchet_*` and `sv_ext_publish_scan`.

## Consequences

- Skillshare needs no new engine code beyond two small `omp ext` additions; the registry stays unbuilt and the signed index remains the one distribution path.
- Ratchet adds one pure crate and a core prelude helper with a default-off convar. Nothing joins the wire roster, so the prompt-cache prefix is unchanged (ADR 0024).
- The verdict is reproducible and not model-controllable; cost and the held-out discipline are enforced by rule.
- The Director gains a second engagement mode; its existing tests must keep passing.
- Autoresearch's missing tools stay a separate, still-open gap (D9).

## Open questions for the owner

- **O1. Order of work, given that autoresearch's tool layer is absent and its isolation primitives are unwired.** Recommendation: (1) R0 and R1, the pure gate and state, now, since they depend on neither; (2) R2 to R4 next, using patch-based revert through existing tools (no isolation dependency); (3) the autoresearch `init`/`run`/`log_experiment` tools and isolation wiring as an independent change, scheduled by the owner and not blocking ratchet; (4) R5 last, after ADR 0060 slice 3. Reason: ratchet needs none of the missing autoresearch pieces, and gating it on them would stall a deterministic, testable core on an unrelated unfinished feature. If the owner prefers one coherent experiment story, swap (3) before (2) and have ratchet commit rounds through `IsolationCommit::AutoresearchRun` (`metrics_json`), which needs no new variant.
- **O2. Where does the verdict live so the model cannot edit it?** Recommendation: the host owns `_state.json` and the model's write policy denies `.omp/ratchet/*/_state.json` and the approval records; if ADR 0006's policy cannot express a path deny for the project tree, R1 adds the narrowest rule and says so. How that policy is expressed was not traced.
- **O3. How does the Director learn the gate outcome?** Recommendation: each `gate()` call writes a typed journal entry (ADR 0003: the tree is the authority) that the Director reads like it reads `log_experiment`. Whether an eval-helper call can emit such an entry, or whether it needs a host-side event, was not checked.
- **O4. Price source.** Recommendation: catalog `Pricing` first, user overrides only for models it cannot price (v1's `prices`).
- **O5. Skill-only packages: stub `entry` or an empty-`entry` load path?** Recommendation: the stub-entry template only (S2); it changes nothing in the engine. Relaxing `native.rs:506` and `worker.rs:3160` together is a load-path change that needs its own decision and a test that no extension host starts for a content-only package.
- **O6. Should the ledger count Skillshare as class a?** Recommendation: yes, with D3 as two small d items attached to it.
- **O7. Root `LICENSE` and upstream notices.** Same question and recommendation as ADR 0060 O5.

## Status in omp

**Status: Not started.** **Implemented and reusable:** ADW's boolean acceptance and replay; the autoresearch Director's resume loop; envd's isolation primitives (unwired); `agent()` in the eval prelude; `ask`; catalog `Pricing`; the signed extension index and `omp ext` (`install`, `lock`, `upgrade`, `publish`, `search`, `pin`). **Not implemented:** the split, gate, approvals, plateau stop, `/ratchet`, any credential scan on publish, a skill-only template, autoresearch's tools. **Unverified:** how the write policy can deny a project path (O2); how the Director reads gate outcomes (O3); whether install, lock and sync work with a stub-entry skill pack; v1's `prelude-definition.ts` and `ratchet-kickoff.md` were not read in full.

## References

[docs/research/ratchet-skillshare.md](../research/ratchet-skillshare.md); ADR 0003 (session tree); ADR 0006 (host policy); ADR 0015 (Directors); ADR 0024 (small roster); ADR 0035 and 0036 (Rust engine, Python extensions); ADR 0049 (`cap-*` features); ADR 0051 to 0053 (ADW, executor contract, independent verdicts); ADR 0060 (typed judgment, `@judge`); `crates/adw`; `crates/agent/src/directors/autoresearch.rs`; `crates/envd/src/vcs/git/mutation.rs`; `crates/ext/src/index.rs`; `crates/app/src/ext_cli/mod.rs`; can1357/oh-my-pi `c308970263` `packages/coding-agent/src/ratchet/`, `skillshare/` and `packages/wire/src/skillshare.ts`.
