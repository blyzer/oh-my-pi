# Gap 16: `/ratchet` and Skillshare against ADW, the autoresearch Director and the signed extension index

Date: 2026-11-13. Status: research, not a decision record (the decision is [ADR 0061](../adr/0061-ratchet-policy.md)). Slug: `ratchet-skillshare`.

Evidence labels: **[code-v1]** read in OMPv1, can1357/oh-my-pi `c308970263` (v18.20.0); `src/` means `packages/coding-agent/src/`. **[code]** read in omp2 at `f4186b783b`. **[doc]** read in a repository document. **[unverified]** not read or not run. All line citations were re-checked against both trees on 2026-11-13 after the first copies of this note were lost.

Classes follow the ledger in `docs/research/upstream-gap-closure.md`: **a** already covered, **b** not needed, **c** port, **d** propose.

Two unrelated features share one gap number. Classification comes first (section 3); proposals follow only for rows that are partial or absent.

## 1. Upstream

### `/ratchet`

[code-v1] `src/slash-commands/builtin-modes.ts:692` registers `/ratchet`. `prepareRatchet` (line 210) requires the `eval` and `task` tools (`RATCHET_REQUIRED_TOOLS`, line 204), enables the ratchet eval prelude for the session without persisting the setting, and renders `src/prompts/ratchet-kickoff.md`. The model drives everything through a global `ratchet(flow)` helper in the eval kernel (`src/ratchet/prelude-definition.ts`, `prelude.py`, `prelude.js`), documented in `src/prompts/tools/ratchet.md` (167 lines). The purpose: build a trusted eval for one LLM flow, then hillclimb a prompt or config surface one change per round, keeping only changes that also win on held-out cases. A user needs it because prompt tuning without a held-out split overfits and cannot tell a real gain from noise.

The pieces, from `src/ratchet/ratchet.ts` (980 lines):
1. **Host-owned state** in `.omp/ratchet/<flow>/_state.json`. The prompt's contract: "Host owns truth: approvals (content-hashed), the frozen split, completeness, held-out isolation, and keep/revert. NEVER hand-edit `_state.json`."
2. **Frozen stratified split.** `splitCases` (line 309) with a seeded PRNG (`seededRandom`, line 297, mulberry32) and a recorded `split_seed`; frozen after the baseline gate.
3. **Content-hashed approvals** for stages `inputs`, `grader`, `plan` (`RATCHET_STAGES`, line 20; `stageDigest`, line 384). A later edit makes an approval stale.
4. **The gate.** `gateVariant` (line 727):
   - Completeness: every (case, rep) scored or recorded in `errors.jsonl`; two result rows for one slot are rejected; test-split traces are rejected.
   - `meanAndHalf`: mean and a Student-t half-width, `tCritical(n-1) * sqrt(variance / n)`; fewer than 2 values gives no half-width.
   - `paired` (line 632): per-case differences against the best round; verdict `up` when gain minus half-width is above 0, `down` when gain plus half-width is below 0, else `flat`.
   - Decision: a guardrail verdict `down` on train or test gives `revert`; target `down` gives `revert`; target `up` on both splits gives `keep`; train `up` only gives `revert` ("overfit suspect"); test `up` only gives `rerun`; otherwise `revert`. Slots that only errored force `rerun`. A guardrail with fewer than 2 measured cases per split makes the gate refuse.
   - Baseline warnings: mean at or above 0.95 on a unit-interval metric ("near ceiling", line 842) and train/test means that differ beyond combined noise.
5. **Plateau stop.** `flat_rounds >= stop.plateau` (line 929), with `plateau >= 2` validated at line 285; `done` also on `stop.rounds`.
6. **Analyzer isolation.** A fresh subagent sees train rows and train traces only; test transcripts never exist.
7. **Stall categorisation** with `judge_batch` and a frozen five-way `choice` rubric: `artifact_gap`, `grader`, `harness`, `structural`, `variance` (prompt section "Stall").
8. **Revert** is model-run `git apply -R vN/change.patch`, obeying the gate's decision.
9. Cost is priced from the catalog by served model and usage (`calculateUsageCost` imported in `ratchet.ts`).
The prompt says it is "Adapted from the claude-api skill's build-eval, eval-audit, and hillclimb guides (Apache-2.0, github.com/anthropics/skills)."

### Skillshare

[code-v1] An npm-style registry for skills at `https://skills.omp.sh` (`packages/wire/src/skillshare.ts:18`). The wire file says the server is a Go service (`stencil/apps/skills`), not part of the repository.
- **Naming:** `@scope/name[@version|range|tag]` (`SKILL_SPEC_RE`, line 26). A scope is a Stencil username bound to the account's immutable `sub`. Versions are immutable. Registry fields ride in `SKILL.md` `metadata` (`version`, `keywords`, `repository`, `homepage`).
- **Package:** a gzip tar of regular files, sorted, mtime 0, with limits in `SKILL_LIMITS` (5 MiB compressed). `src/skillshare/pack.ts` packs, honours `.skillignore`, and scans for credential-shaped strings with `CREDENTIAL_PATTERNS` (imported from `src/secrets/patterns`); `--allow-secrets` overrides.
- **Integrity and provenance:** publish carries an SRI sha512 in `X-Skill-Integrity`. `src/skillshare/installer.ts` verifies it before unpacking (`storeSkillVersion`, lines 102-113: "integrity mismatch ...; nothing was installed"). Provenance is "reported" or "verified" through a GitHub Actions OIDC token checked by the server (`SkillVerifiedProvenance`).
- **Install state:** `skills.json` and `skills.lock.json` (project `.omp/` or the agent dir) and a shared store `<config root>/skillshare/@scope/name/<version>/` (`src/skillshare/manifest.ts`). A registry integrity that differs from the lock is an error (`installer.ts:288-290`).
- **Discovery:** `src/discovery/skillshare.ts` is a provider at priority 95, just under native skills (100), so authored skills win name collisions.
- **Consent:** a version that ships executables or anything under `scripts/` (`hasScripts`) needs approval or `--yes` (`ScriptApprovalRequest`, `installer.ts:193`).
- **Surface:** `omp skill <action>` with actions `publish, version, tag, yank, deprecate, owner, token, import, install, update, uninstall, search, info, list` (`src/cli/skill-cli.ts:41`); `import <file.skill>` publishes a Claude `.skill` archive (`skill-cli.ts:99`, `handleImport` at 449). A TUI `/skills` command (`src/slash-commands/builtin-skills.ts:66`).

## 2. omp2 inventory

**ADW (`crates/adw`, `crates/driver/src/adw`).** [code] A pure state machine: "named phases with declared dependencies" (`crates/adw/src/lib.rs`). `PhaseKind` is `Agent`, `Code` ("a deterministic command whose exit status decides the outcome") or `Review` (`workflow.rs:29`). `OnReject` is `Retry`, `Correct` (rewind to the producing phase) or `Halt` (`workflow.rs:44`). `Run::accept` and `reject` (`run.rs:354`, `396`) and `replay` (`replay.rs:104`) make runs reconstructible. The attempt cap is `DEFAULT_MAX_ATTEMPTS = 3` (`run.rs:15`), `Run::with_max_attempts` (`run.rs:255`), enforced at lines 317, 416 and 430. `Run::accept` marks a phase `Passed` and publishes a version; nothing re-dispatches an accepted phase except `invalidate_dependents` when an upstream phase is re-accepted, so the domain has no "accept, then climb again" loop. Acceptance is host-owned: gate results "arrive here as observed evidence" (`crates/driver/src/adw/mod.rs` header). The durable record is `omp-journal::runlog`. ADR 0053 adds the `@judge` role and independent verdicts and is Not started for those parts [doc]. What ADW decides is boolean: accepted or rejected. It has no metric, split, statistics or best-so-far.

**The autoresearch Director (`crates/agent/src/directors/autoresearch.rs`).** [code] 106 lines. It binds `ai_prompt_mode = autoresearch` (`crates/agent/prompts/modes/autoresearch.md`, 10 lines, a hypothesis-and-experiment prompt), claims `Slot::Mode` and `Slot::Worktree`, and sets `armed` after a turn calls `log_experiment` (`observe_turn`). `evaluate` (line 87) returns `Done` at the iteration cap, otherwise `Continue` with the reminder "Resume the logged autoresearch experiment." unless user input is queued. It is a resume policy. Its `state()` advertises the tools string `init,run,log_experiment`, but no tool with those names exists in `crates/`; the only references are the Director and `crates/agent/tests/directors/misc.rs`.

**Git isolation for experiments (`crates/envd/src/vcs/git/mutation.rs`).** [code] A reviewed, closed consumer enum (`GitMutationConsumer::Autoresearch`, line 177) and fixed record types with deliberately no free-form commit message: `IsolationCommit::{AutoresearchBaseline, AutoresearchHarness, AutoresearchRun { description, metrics_json }}`. Operations: `create_isolation_branch` (391; branches must start with `autoresearch/`), `commit_isolation` (407), `rollback_isolation` (432; restores tracked paths, removes untracked ones). A search of `crates/` outside `mutation.rs` finds no callers. The keep/revert substrate exists; nothing drives it.

**Eval kernel and helpers.** [code] `crates/envd/src/eval/python_prelude.py:841` defines `agent(prompt, *, agent, name, effort, outputSchema, schemaMode, isolated, apply, merge, handle)`, so a schema-returning analyzer subagent exists. There is no `judge_batch` and no `ratchet` there. `docs/py/16-prelude.md`: an extension publishes a named synchronous helper through `@omp.prelude`; it is not a tool and is not shown to the model as a schema. Other tools: `ask` (`crates/tools/src/ask.rs`), `checkpoint`, `manage_skill` (`crates/tools/src/manage_skill.rs`: managed skills never override authored ones, `manage_skill.rs:104`). Catalog pricing: `crates/catalog/src/pricing.rs`, `Pricing` (line 114) with `cost(UsageDimensions) -> NanoUsd` (line 192), integer-only, tiered and service-tier aware.

**The signed extension index (`crates/ext/src/index.rs`) and `omp ext`.** [code] `SignedIndex` (line 180) verifies against a pinned index key. `IndexExtension` has an Ed25519 `publisher_key` and optional `key_rotation` proof. `IndexRelease` (line 98) is immutable and carries `manifest_digest`, capability digests, `requires`, `capabilities`, `features`, `declarations`, `attested`, `yanked`, `shadows`. `IndexArtifact` (line 76) is a hash-pinned wheel per target (BLAKE3, SHA-256, byte length, and a publisher signature over both hashes and the capability-graph digest). `search` (line 383) matches id or description and skips yanked or, on request, unattested releases. `omp ext` (`crates/app/src/ext_cli/mod.rs`) provides `install` (resolve, verify, consent), `uninstall`, `list`, `info`, `search` (308), `publish` (305; `--attest`, `--dry-run`), `lock` (275), `sync`, `upgrade` (281), `pin`/`unpin`, `features`, `config`, `link`, `new` and `index add|remove|list`. `ResourceFamily::Skills` is one of four package-owned families (`crates/ext/src/config.rs:609`). Skill discovery (`crates/driver/src/discovery/skills.rs:1-12`, `208-218`, `260`) is ordered and first-wins: native project and user roots, Claude plugins, marketplace plugins and manifest-sealed extension roots (extension skills sit below authored and custom roots and above the managed-skills root); collisions are skipped with a diagnostic.

**Skill-only packaging.** [code] The parse layer allows a content-only package: `DeploymentManifest.entry` is `#[serde(default)]` (`config.rs:1366`) and `validate_static_row` (`config.rs:1906`) accepts `skills` rows that carry only a path and "cannot carry executable fields". The load path does not: `validate_manifest` in `crates/driver/src/discovery/native.rs:506` rejects an empty `entry` as `MissingIdentity`, and `validate_extension_spec` in `crates/envd/src/worker.rs:3160` requires a nonempty `manifest.entry`. `omp ext new` (`ext_cli/mod.rs:1283`) takes only an id and scaffolds a Python hard-tool and hook example.

**Credential scanning.** [code] No shared credential-pattern set exists for `omp ext publish`. `crates/envd/src/security_scan.rs` is a repository-scan service; the `AKIA` hits in `crates/ai/src/auth/aws.rs` and `crates/minimizer/src/filters/cloud.rs` are unrelated. `omp ext publish` has no scan; its only secret handling is for settings (`crates/ext/src/config.rs`, `ESettingSecret`).

## 3. Class

### Skillshare: class a, with two small d items

| Upstream capability | omp2 equivalent | Class |
|---|---|---|
| Registry server `skills.omp.sh` (Go, `stencil/apps/skills`) | Static signed index snapshots at HTTPS URLs (`IndexConfigEntry`); no server in this repo | **b**: a hosted service, not engine code |
| Scopes bound to Stencil accounts, tokens, download counts, README rendering, web UI | none | **b** |
| Scoped immutable versions, ranges, dist-tags | Immutable `IndexRelease`, exact PEP 440 versions, `pin`; dist-tags absent | **a** for immutability and pinning; dist-tags **b** |
| Integrity (SRI sha512 checked before unpack) | BLAKE3 and SHA-256 per artifact plus a publisher Ed25519 signature over both hashes and the capability digest, key pinning and rotation, signed index | **a**, stronger: no port |
| Install, update, uninstall, list, search, info | `omp ext install`, `upgrade`, `uninstall`, `list`, `search`, `info` | **a** |
| `skills.json` and `skills.lock.json`, shared store | `omp ext lock`, `sync` (`crates/ext/src/lock.rs`) | **a** |
| Project-over-user discovery, native wins (priority 95 vs 100) | Ordered first-wins skill discovery (section 2); managed never overrides authored | **a** |
| Script consent (`hasScripts`) | Install consents to declared capabilities; code runs in a containment tier | **a**, different model, stronger |
| Yank, deprecate | `yanked` flag; no deprecation message | **a** for yank; deprecate **b** |
| Verified provenance through GitHub OIDC | `attested` flag and publisher key; no OIDC | **b**: a concern of whoever builds the index |
| `omp skill import <file.skill>` | none; Claude plugin skills are read through `claude_plugin.rs` | **b**: only useful with a registry |
| Publish-time credential scan (`pack.ts`) | none for `omp ext publish` | **d** (small) |
| Skill-only package (a directory with `SKILL.md`, no code) | Parse layer allows it; load path requires a Python `entry`; `ext new` scaffolds only Python | **d** (small): a stub-entry template first; an empty-`entry` load path only if the owner wants it |

### `/ratchet`

| Upstream capability | omp2 equivalent | Class |
|---|---|---|
| `/ratchet` command arming a prompt mode | A Director `binds` `ai_prompt_mode` (the autoresearch pattern) | **d**, trivial once the rest exists |
| Host owns the verdict; the model cannot argue with it | ADW acceptance is host-owned (`adw/mod.rs` header; ADR 0052 D4 via ADR 0053 D6) | **a** as a principle |
| Resume loop with a cap | `Autoresearch::evaluate` resume and iteration cap | **a** |
| Retry with feedback, rewind to producer | `OnReject::Retry`, `Correct`, `Halt` | **a** |
| Keep or revert changes atomically | envd `create_isolation_branch`, `commit_isolation`, `rollback_isolation` | **a** for the primitives, but unwired (no callers) |
| Analyzer in a fresh context, schema output | `agent()` prelude helper, `task` tool; ADR 0053 D2 independence | **a** |
| Catalog-priced cost | `Pricing::cost` (`pricing.rs:192`) | **a** |
| Frozen stratified seeded split | none | **d** |
| Paired case-level delta, noise floor, keep/revert/rerun, completeness and held-out-leak checks | none; ADW gates are boolean | **d** (core of the feature) |
| Plateau and rounds stop | Director has an iteration cap only | **d** |
| Content-hashed approvals for inputs, grader, plan | `ask` exists; hashing and staleness do not | **d** |
| `judge_batch` stall categorisation | no typed judgment API (see `semantic-find.md`) | **d**, blocked on the same prerequisite |
| Reuse an existing eval; headless rules | n/a | **b** |

### Overlap, stated plainly

- Autoresearch (benchmark-driven code experiments with keep or discard) and ratchet (eval-driven tuning of an LLM flow with held-out validation) share one thing: the keep/revert loop. Ratchet's value is the statistics and held-out discipline around it. A second Director would duplicate the loop for no gain.
- The autoresearch tool layer (`init`, `run`, `log_experiment`) is advertised but absent and the isolation primitives have no callers. That is a gap in autoresearch itself, not in Gap 16. Ratchet does not need those tools: v1 reverts by reverse-applying the round's patch, which the model can do with existing tools, so ratchet can be built without finishing autoresearch.
- ADW can express one round (an `Agent` change phase, a `Code` eval-and-gate phase, `OnReject::Correct`), but with a default cap of 3 attempts and no loop after accept, the multi-round climb needs a new run per round with best-so-far held elsewhere. Forcing ratchet into ADW would stretch a state machine that is deliberately small.

## 4. Options (`/ratchet` and the two skillshare d items)

| Option | Fit | Cost and risk |
|---|---|---|
| Ratchet A: a pure decision crate in Rust (split, gate, approvals, plateau), driven from the autoresearch Director and a thin prelude facade | Matches ADR 0035 (engine logic in Rust), ADW's pure-state-machine style and the "host owns truth" rule; state not writable by the model | New crate plus the autoresearch tool layer it needs |
| Ratchet B: a Python extension shipping an `@omp.prelude` helper (ADR 0036) | Fast to write; no core change | A helper that can write its own state cannot enforce "never hand-edit"; trust model fails |
| Ratchet C: model ADW workflow recipes only | No new code | No statistics, no split, no held-out guard; the gap stays |
| Ratchet D: a second Director or workflow engine | none | Duplicates the loop; rejected |
| Skillshare E: build a registry client for `skills.omp.sh` | Matches v1 | A hosted dependency; second signing scheme; rejected (class b) |
| Skillshare F: extend `omp ext` (scan on publish; skill-only template) | Smallest; reuses signed index | Two small changes |

**Choice:** Ratchet A and Skillshare F. The kernel facade is a prelude helper, so the model talks to the gate but cannot edit its state.

## 5. Plan

The ordering question and the PR-sized slices with acceptance tests are in [ADR 0061](../adr/0061-ratchet-policy.md). Shape:
- **Autoresearch (independent, owner-scheduled):** finish the init, run and log_experiment tools and wire the isolation primitives; ratchet does not depend on it.
- **Ratchet:** pure gate crate (split, paired deltas, noise floor, decision, plateau) with exhaustive unit tests; state and approval digests; the prelude facade and prompt mode; a Director plateau stop; stall categorisation last, blocked on the typed-judgment prerequisite from ADR 0060.
- **Skillshare:** publish-time credential scan in `omp-ext`; a stub-entry `omp ext new` skill template; empty-`entry` load path only on owner request.

## 6. Open questions

- Should autoresearch's tool layer and isolation wiring be finished before ratchet starts? Recommendation: no, ratchet does not depend on them (patch-based revert); build the pure gate and state first, schedule autoresearch as an independent change. ADR 0061 O1 gives the full order.
- Gate and state in Rust or a Python extension (trust)? Recommendation: Rust, with a prelude facade.
- Does the gate take prices from `omp-catalog`'s `Pricing` (exists) or from user overrides like v1's `prices` for models the catalog cannot price? Recommendation: catalog first, overrides for the rest.
- One ADW `Run` per ratchet round, or keep the gate outside ADW entirely? Recommendation: outside ADW.
- v1's `import` action is class b; not read beyond `handleImport`.
- Conflicts with hard constraints or locked ADRs: none found. The ratchet gate is deterministic code (a model only categorises stalls, behind the same prerequisite as ADR 0060); the proposals add no roster family and ship default off.
- Whether the `omp ext` install path works end to end with a stub-entry skill pack was not run [unverified].

## 7. Upstream origin and license

Ratchet draws from `src/ratchet/ratchet.ts`, `src/ratchet/prelude-definition.ts`, `src/slash-commands/builtin-modes.ts` (`prepareRatchet`), `src/prompts/ratchet-kickoff.md` and `src/prompts/tools/ratchet.md`. The prompt is adapted from Anthropic's claude-api skill (Apache-2.0, github.com/anthropics/skills), so any reuse of its wording carries Apache-2.0 attribution in addition to the MIT notice; the recommendation is to write omp2's prompt fresh from the contract rather than copy. Skillshare files (`src/skillshare/*`, `packages/wire/src/skillshare.ts`) are not ported. Upstream license: MIT (© 2025 Mario Zechner, © 2025-2026 Can Bölük, © 2026 Stencil Labs, Inc.); omp2's `LICENSE` is MIT, © 2026 Stencil Labs, Inc.
