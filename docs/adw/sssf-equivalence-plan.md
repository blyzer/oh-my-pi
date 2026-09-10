# ADW: SSSF equivalence and remaining execution gaps

## Scope and completion rule

Deliver an Oh My Pi-native factory whose deterministic control plane owns execution, correction and acceptance. Reuse `pi-tasks`, native isolation, the OMP agent roster and the existing trace; do not copy SSSF's Python runtime or introduce another agent framework.

Milestones 1–7 — **contracts and acceptance**, **reviewer-to-builder correction**, **explicit phase inputs with durable outputs**, **write scopes with evaluator protection**, the **reusable complete SDLC recipe**, **durable recovery with truthful traces** and **concurrent workflow DAGs** — are delivered and exercised locally. A passing unit suite alone does not establish factory equivalence.

SSSF's baseline is serial. Its complete starter composes planning, implementation, tests/fixes, review/revision, retesting, documentation and commits. Its quality commands require repository adaptation, its permission checks are not a sandbox, and reusing a session is not checkpoint replay. Concurrent workflow DAGs and durable recovery are requirements of our expanded scope, not capabilities to attribute to SSSF without evidence.

## Milestone 1: contracts and acceptance

**Status:** implemented and exercised locally. **Dependencies:** existing caller-run gate reports and omptype.

### Contract

- Apply custom `schema` validation to both agent and fusion-fuser envelopes through one submission path. Panel opinions remain prose.
- Deliver the declared output schema to the writer, without requiring a second hand-maintained copy in `prompt`.
- Add `verdict_consistent` as a caller-run gate backed by native omptype validation. Require `approved`, `blocking`, and `findings`; each finding has `requirement`, `met`, and optional `evidence`.
- Reject approval with blockers, approval with unmet requirements, and rejection without any stated problem. These checks establish consistency, not truth or completeness of the review.
- Add explicit workflow `acceptance: review`: all phases must pass and the **last accepted envelope** must be a coherent, approved review. Omitting it retains all-phases-success acceptance.
- A coherent `approved: false` report passes the consistency gate but refuses delivery; do not retry the reviewer simply to obtain approval. The reviewer-to-builder transition is milestone 2.
- Use the same final decision for the run summary, terminal trace and isolation landing. Reconstruct it from the engine's handoff on resume, not ephemeral flags.
- Keep JSON Schema import limitations explicit. Native predicates do not survive JSON Schema export. Full JSON Schema conformance is not a prerequisite for these named native rules.

**Targets:** `packages/coding-agent/src/adw/{schema,prompt,config,types,runner}.ts`, existing ADW tests, `docs/adw-workflows.md`.

### Exit evidence

1. A fuser emits a schema-invalid result, receives the violation, and corrects it without re-running the panel.
2. Contradictory approval fails the consistency gate; a corrected, coherent report passes.
3. Coherent rejection ends with passed review phase and unaccepted run, without applying an isolated patch.
4. Coherent approval accepts; a trailing code result or missing review does not reuse an earlier approval.
5. A recorded final review is re-evaluated after interruption before terminal settlement.
6. The generated writer prompt contains the actual schema and native-rule instructions; a real model run exercises the contract rather than only a scripted seat fixture.

### Recorded verification

- `bun test test/adw`: 123 passed, 0 failed, 310 assertions across 5 files, including parsing the updated review example.
- ADW integration scenarios exercise fuser correction without another panel, all three verdict contradictions, approved/rejected isolation landing, refusal of stale approval after a later envelope, and resume after a recorded final review.
- Real model: `openai-codex/gpt-5.4-mini`. Fusion run `adw-1576a70e1165b6ff` passed both contract gates and its review phase, then refused delivery for an unmet requirement. Agent run `adw-1576a73bb225b706` passed both gates and accepted a coherent approval.
- Both real writers emitted `audit_token: "from-declared-schema"`, a field/value supplied only through the declared custom schema. The real-model runs did not need correction; correction behavior is covered by the integration scenarios.
- The smoke command's transport timed out while the underlying runs continued. Completion and decisions were recovered from their persisted envelopes and native-decoded terminal traces; no duplicate model run was launched.
- `bun run check:types` passed. Targeted oxlint reported zero errors, with warnings for an existing zero-width character in a type comment and the intentional JSON Schema `then` keyword in a test.

## Milestone 2: reviewer-to-builder correction and reverification

**Status:** implemented and exercised locally. **Depends on:** milestone 1.

### Design and changes

- Distinguish three outcomes: malformed/inconsistent review (correct reviewer), valid rejection (revise implementation), and valid approval (continue).
- Add explicit review rejection routing to a named writer phase. Do not infer the target from an arbitrary earlier agent or instruct the reviewer to edit implementation.
- Extend the engine's transition contract rather than simulating a second workflow loop in prompts. Retain the rejected review, its blockers and unmet requirements as the builder's correction input.
- Maintain bounded revision attempts separately from format/gate correction attempts. Repeated rejection must terminate; it must not reset the builder's budget.
- Invalidate downstream quality/review results when their implementation input changes. Re-run checks against the revised code before accepting; no green result from before the edit may authorize delivery.
- Preserve phase history while marking which outcomes are current and which were invalidated. Reuse sessions where available; a fresh session still receives the complete correction.

**Targets:** `crates/pi-tasks/src/{phase,orchestrator,trace}.rs`, `crates/pi-natives/src/tasks.rs`, ADW config/runner/prompt and native bindings.

### Exit evidence

- A valid rejection calls the builder, not the reviewer again; a malformed verdict corrects the reviewer instead.
- The builder fixes one reported defect, then checks and review run again and approve.
- A revision introducing a new test failure cannot reach delivery using the previous passing result.
- Repeated rejection exhausts the declared revision budget with its reason preserved.
- Explicit routing fails at load time for unknown, non-writer or structurally invalid targets.

### Recorded verification

- Engine (Rust): `onReject {to, maxRevisions}` on `PhaseParams`/`TaskPhaseSpec`, `noteReviewDecision` drained per submission, `invalidated` on phase records, trace format v4 (36-byte records, u32 attempt at offset 32, `review_revision`/`review_exhausted`), older trace versions refused by path. `bun run test:rs`: 2816 passed, 0 failed, 5 skipped, including regressions for routing versus format correction, exhaustion independent of format attempts, non-replenishing grants, invalidated stale approvals, replay of grants/correction/invalidation, and u32 overflow halting.
- Caller (TypeScript): load-time route validation (unknown/self/`code`/forward/non-dependency targets, non-integer or out-of-range `maxRevisions`), `PhaseCheckResult {reports, review?}` with one envelope parse, runner submits the decision and refreshes fusion panels on revision but not on fuser format retries, driver `maxSteps` cap removed in favor of engine-bounded transitions. `bun test test/adw`: 147 passed, 0 failed. Typechecks for `pi-coding-agent` and `adw-web` passed; targeted oxlint zero errors with the two pre-existing warnings.
- Live run `adw-1576c1fd1ad542c4` (`anthropic/claude-sonnet-4-6`): the reviewer rejected coherently citing the seeded defect, the engine routed revision 1/1 to the builder with the full report, the builder fixed `migrate.js` in its granted attempt, check and review re-ran against the revised code and approved, and delivery was accepted with the first build/check/review triple marked invalidated. An earlier attempt on `openai-codex/gpt-5.4-mini` aborted on the provider's usage limit as an ordinary decided failure with a terminal trace.
- Integration scenarios additionally prove a revision that breaks a previously passing check halts on that failure without reusing the invalidated pass, exhaustion preserves the review's reason past the old driver step cap, and resume replays a consumed revision without replenishing it.

## Milestone 3: explicit phase inputs and durable outputs

**Status:** implemented and exercised locally. **Depends on:** milestone 2; provides the data contract required by DAG fan-in.

### Design and changes

- Address accepted envelopes by phase name and attempt/revision, not only `last_envelope`. Keep one authoritative store integrated with the engine trace.
- Let each phase declare which predecessor outputs it consumes. Validate references before execution; ordering dependencies and data dependencies must agree.
- Pass selected structured inputs to writers and deterministic code phases. A successful code phase must not erase a plan needed by a later phase.
- Make consumption of stale, rejected, missing or invalidated outputs fail explicitly. Keep historical outputs available for diagnostics without presenting them as current.
- Define final acceptance against a current review and its implementation/check revisions before supporting non-final review selectors. Do not make an earlier approval reusable after a writer changes its inputs.
- Record the selected input identities so resume and the viewer can explain which results authorized an execution.

**Targets:** engine envelope/orchestrator/trace, N-API handoff surface, ADW schema/prompt/runner.

### Exit evidence

- `plan -> code check -> build` delivers the plan to build when explicitly selected.
- A join consumes the named outputs of both predecessors, independent of incidental execution order.
- Rewinding a producer invalidates its consumers and prevents stale acceptance.
- Resume reconstructs the same selected outputs and refuses missing/corrupt authoritative data.

### Recorded verification

- Engine (Rust): versioned envelope store (`envelopes/<phase>.<version>.json`, per-phase acceptance ordinal, clean cutover with no unversioned alias), `inputs` on `PhaseParams`/`TaskPhaseSpec`, dispatch-time selection returning `TaskStep.inputs` (`TaskPhaseInput { phase, version, summary, artifacts, notesForNextAgent, payloadJson }`), one `input_selected` event per input (trace format v5, layout unchanged, v3/v4 refused by reader and writer), invalidation clearing current pointers while acceptance ordinals keep counting, and start/resume-time input validation. Selection failures and resume over missing or unreadable consumed envelopes are decided `Mismatch` errors naming consumer, producer and version. `bun run test:rs`: 2822 passed, 0 failed, 5 skipped.
- Caller (TypeScript): load-time `inputs` validation naming phase and input for unknown/self/duplicate/later/non-dependency entries with the shared traversal; labelled per-producer prompt sections replacing the positional handoff for declaring phases (byte-identical prompts otherwise); `code` phases receive `$ADW_INPUTS` pointing at a per-dispatch JSON file in the run directory keyed by producer. `bun test test/adw`: 161 passed, 0 failed. Typechecks for `pi-coding-agent`, `adw-web` and `pi-natives` passed; targeted oxlint zero errors with one pre-existing warning.
- Live run `adw-1576dc956673a014` (`anthropic/claude-sonnet-4-6`, workflow `plan → relay → build`): the `relay` code phase gated on reading plan v1 through `$ADW_INPUTS` (exit 1 otherwise), `build` declared `inputs: [plan]` across the intervening code acceptance and produced `greeting.js` exporting the plan-named constant, and the decoded trace recorded exactly `relay←plan v1` and `build←plan v1` selections. The post-run assertion script initially failed on its own bare `@oh-my-pi/pi-natives` import resolving to a stale cache copy outside the workspace; the assertions were re-run from the workspace and passed — a fixture defect, not product behavior.
- Integration scenarios additionally prove a join consumes both named predecessors regardless of execution order, a review revision reselects the producer's new version and never re-presents the old one, resume reconstructs the same selected input, and a corrupted authoritative envelope refuses resume naming the producer.

## Milestone 4: write scope and evaluator protection

**Status:** implemented and exercised locally. **Depends on:** milestones 2–3 for attempt ownership and invalidation.

### Design and changes

- Define explicit repository-relative write scopes for each writer and protected paths for workflow definitions, evaluator scripts and configuration. Keep capabilities (tools) separate from allowed paths.
- Snapshot the repository state at the attempt boundary using existing native VCS/isolation facilities. Compare actual content and path changes; a file already dirty or previously claimed is not permission for arbitrary new edits.
- Keep `diff_matches_claims` as accountability, separate from authorization. Commit claims to accepted state only after the attempt passes; rejected claims must not expand later permission.
- Enforce on every exit, including malformed output, provider failure, timeout and cancellation. Roll back only unauthorized changes from the owned attempt, preserving the user's pre-existing bytes and index state.
- Handle rename/deletion, untracked files, symlinks and nested repositories explicitly. Where rollback cannot safely restore state, preserve the patch and fail closed; never hide the failure as acceptance.
- Preserve a durable run baseline across resume; do not relabel interrupted work as the user's initial dirtiness.

**Targets:** native task gates, `pi-vcs`/`pi-iso` integration points, ADW agent derivation and lifecycle cleanup.

### Exit evidence

- A planner cannot modify implementation; a reviewer cannot alter the evaluator; declaring the forbidden path does not authorize it.
- Unauthorized changes are caught on successful, malformed and cancelled turns.
- Pre-existing user edits survive rejection byte-for-byte; a same-size rewrite cannot evade detection.
- Rejected-attempt claims and resumed dirty files cannot become implicit authorization.

### Recorded verification

- Native: new `TaskWriteGuard` (`crates/pi-natives/src/write_guard.rs`) — durable run baseline persisted to the run directory and loaded on resume; per-attempt content-addressed snapshots (streaming xxh64+length, so same-size rewrites are caught; APFS/reflink-cheap object store; begin manifest persisted so settlement survives process death); classification against `writes`/`protected` with implicit `.omp/adw/**` and protection beating authorization; restoration covering modification, creation, deletion, symlink retargets and rename halves, with `.git` internals excluded and gitignored paths outside jurisdiction; unsafe restorations preserved as a git-apply-consumable patch and reported unrecoverable for a fail-closed halt. `Gate::accept` hook added to `pi-tasks`; `diff_matches_claims` commits claims to the shared accepted set only on engine acceptance. `bun run test:rs`: 2834 passed, 0 failed, 5 skipped (one delivered test fixture needed a Debug derive and one an artifact to satisfy the milestone-1 vacuity rule; both fixed before the green run).
- Caller: `writes` (agent/fusion only) and `protected` validated at load (blank/duplicate globs, code-phase writes rejected; glob syntax validated natively at run start naming the pattern); writers see their scope in their prompt; the runner opens the boundary before the panel, settles on every exit including the crash path, reports violations as a `write_scope` gate rejection after the revert, settles a dead attempt's leftover boundary on resume before opening a new one, and halts fail-closed on unrecoverable restorations. `bun test test/adw`: 173 passed, 0 failed. Typechecks for the three touched packages passed; targeted oxlint zero errors with the one pre-existing warning.
- Live run `adw-1576e735b58dcf21` (`anthropic/claude-sonnet-4-6`): the writer was instructed to tamper with the protected evaluator `scripts/verify.sh` on its first attempt; the guard reverted it byte-for-byte, the rejection named the path and the scope, the second attempt complied and was accepted, and the operator's pre-existing scratch file survived untouched.
- Integration scenarios additionally prove a same-size out-of-scope rewrite is reverted and corrected, protection beats a declared write while authorized work and user dirt survive, an unauthorized creation is removed when the writer crashes mid-turn, and a directory squatting a guarded file is fully reverted. The dead-attempt-leftover resume path and the unrecoverable/patch path are exercised natively (crash-manifest reload; missing snapshot object), not at the driver level.

## Milestone 5: reusable complete SDLC recipe

**Status:** implemented and exercised locally. **Depends on:** milestones 1–4.

### Design and changes

- Ship a complete recipe using OMP's existing roster and configuration precedence: request, plan, implementation, actual checks, review/revision, documentation, final acceptance and controlled delivery.
- Bind test/lint/typecheck/build commands explicitly to the target repository. No placeholder commands or invented package-manager defaults that report green without running checks.
- Ensure all code/documentation edits precede the final checks and approval, or explicitly invalidate and repeat them. Finalization may commit an accepted owned diff; it must not introduce new unreviewed file edits.
- Define the deliverable (patch or scoped commit) and preserve rejected work without landing it. Never stage unrelated user changes with an unscoped `git add -A`.
- Provide an installation/adaptation path that reuses OMP instead of stamping another engine. Preserve local workflow and prompt customization.
- Remote PR creation, push and deployment require an explicit workflow/user authorization; they are not prerequisites for local SSSF equivalence.

**Targets:** `docs/adw/examples/`, workflow documentation and the existing ADW command/runner finalization surface where required.

### Exit evidence

Run the recipe in a real disposable application repository: introduce a defect, observe test failure and correction, then reviewer rejection and revision, rerun checks, generate documentation and deliver only after approval. Repeat with permanent rejection and prove the original checkout and unrelated dirty files remain unchanged.

### Recorded verification

- Shipped `docs/adw/examples/sdlc.yml` (`plan → build → docs → verify → review`) composing inputs, write scopes, protected paths, `onFail: correct`, `onReject` and `acceptance: review` + `isolation: true` as the delivery contract; its `verify` command deliberately exits 1 naming itself until adapted to the target repository's real entry points. The examples-parsing test covers it; `bun test test/adw`: 174 passed, 0 failed.
- Exercised end to end in a real disposable bun application repository (real `bun test` suite). Approval run `adw-1576ed6f1882c529` (`anthropic/claude-sonnet-4-6`): the staged implementation defect failed `bun test` with the failure text reaching the builder as a correction; the corrected code passed; the reviewer coherently rejected citing line-level evidence for the one unmet requirement; the revision fixed it; docs, checks and review re-ran and approved; the diff was applied to the checkout, where the new behavior and the full suite were re-verified, with the operator's scratch file and the protected `package.json` untouched.
- Permanent-rejection run `adw-1576ee55c902c535`: the reviewer rejected every pass; the revision budget exhausted with the review's reason preserved; nothing was applied, the original implementation survived byte-for-byte, the only untracked paths remained the operator's own, and the rejected work was preserved as a patch containing the feature.
- The first live run exposed and fixed a real defect: accepted phases without the `diff_matches_claims` gate never committed their claims, so `build` was blamed for `plan`'s PLAN.md. Claims now commit on every engine acceptance (`bun run test:rs`: 2835 passed, 0 failed, 5 skipped, including the new gate-less-claims regression); the fix was confirmed in the passing rerun. Also fixed `ship.yml` naming the read-only `scout` as a writing phase's owner.
- Delivery remains local: no commit, stage, push, PR or deployment anywhere in the pipeline, matching the plan's authorization boundary.

## Milestone 6: durable recovery and truthful traces

**Status:** implemented and exercised locally. **Depends on:** milestones 1–5 so the persisted contract covers actual transitions and delivery.

### Design and changes

- Persist the workflow/contract version, execution graph, active attempt, budgets, pending correction, selected outputs, original VCS baseline and accepted claims. Resume must reject incompatible configuration instead of silently substituting it.
- Reconstruct correction text and input revisions after rejection/rewind. Counting attempts without restoring the diagnostic is insufficient.
- Persist or safely reattach the isolated workspace; do not recreate it from the base and skip phases whose work has disappeared.
- Define recoverable boundaries around gate submission, phase acceptance and final delivery. Detect an already-applied result; do not blindly repeat a commit or patch application after a crash.
- Make the viewer use the current terminal state after resume/rewind while retaining earlier terminal records as history.
- Correct per-attempt usage accounting when a continued session reports cumulative usage; retain unknown usage as unknown. Do not present the same tokens repeatedly as new charges.

**Targets:** engine trace/replay, native baseline/claim restoration, ADW run state and isolation lifecycle, `packages/adw-web`.

### Exit evidence

Interrupt after a gate failure, after a review rejection, after a rewind, after the last accepted phase and around patch delivery. Resume must preserve corrections, budgets, selected inputs and baseline; it must neither skip missing work nor deliver twice. The viewer must agree with the resumed run's final result.

### Recorded verification

- Engine (Rust): `correction_pending` trace kind (format v6, layout unchanged, v3–v5 refused by reader and writer) persists in-place rejection diagnostics; replay restores the byte-identical correction and clears it on acceptance. `TaskRun::resume` refuses a `maxAttempts` differing from the traced base naming both numbers. `claimsFile` persists the accepted `diff_matches_claims` set (sorted JSON, atomic rename); resume loads it instead of recapturing dirt, so a crashed attempt's undeclared edit is flagged while original dirt and accepted claims stay admitted; a corrupt existing file errors rather than silently substituting recapture, and a missing file keeps the legacy path. `bun run test:rs`: 2839 passed, 0 failed, 5 skipped.
- Driver: `workflow.json` persisted at start and deep-compared on resume (drift refused naming the record); `delivery.json` records each settlement before anything else touches the tree, `settleIsolation` returns the recorded outcome instead of re-applying, and a settled run refuses resume; isolated runs reattach the surviving sandbox from `isolation.json`, re-claiming ownership, and refuse when it is gone rather than rebuilding from base; an accepted run that died inside the delivery window settles from the record without re-running phases. Viewer reports the LAST terminal record as current. `bun test test/adw`: 178 passed, 0 failed; typechecks clean.
- Live (`anthropic/claude-sonnet-4-6`): run `adw-1576f3419e75ac3f` was SIGKILLed mid-`build` after `plan` accepted inside an APFS sandbox; a fresh process resumed, reattached the same sandbox (the builder read the pre-kill PLAN.md and copied its marker), re-ran only `build`, accepted, applied the delivery exactly once, and a second resume was refused with the settled-delivery message.
- Interrupt boundaries covered across suites: after a gate failure (correction restored), after a review rejection and after a rewind (milestone-2 replay tests), after the last accepted phase (milestone-1 resume test), and around patch delivery (marker + recovery). Per-attempt usage was inspected rather than changed: the executor builds a fresh monitor per follow-up turn, so a continued session already reports per-turn usage, and all-zero provider usage remains marked unaccounted.

## Milestone 7: concurrent workflow DAGs

**Status:** implemented and exercised locally. **Depends on:** milestones 2–4 and 6.

### Design and changes

- Replace the single cursor with explicit pending/ready/running/passed/rejected/invalidated phase states. `dependsOn` currently provides serial topological ordering, not concurrent scheduling.
- Dispatch independent ready phases under a shared concurrency limit. Resolve fan-in through named, versioned outputs from milestone 3.
- Give each concurrent writer its own isolated workspace and declared write scope. Use one integration owner to combine accepted patches, detect conflicts and trigger combined verification.
- Keep the existing parallel read-only fusion panel as a distinct mechanism; do not substitute per-seat prompts for DAG execution.
- Cancellation and failed dependencies must stop or invalidate affected work without discarding unrelated evidence. Persist the active set for recovery.
- Trace causality and simultaneous execution, not an artificial serial timeline. Report actual per-seat model and per-attempt usage.

**Targets:** engine scheduling/trace, N-API execution contract, ADW dispatcher, isolation/integration and trace viewer.

### Exit evidence

Two independent phases demonstrably overlap; their join runs only after both accepted outputs exist. Concurrent writers cannot overwrite one another. A conflicting integration refuses delivery, a failed prerequisite blocks descendants, and a resumed run neither duplicates completed work nor exceeds its concurrency limit.

### Recorded verification

- Engine (Rust): the cursor is replaced by per-phase pending/ready/running/passed/rejected/invalidated states with `Step::Wait`, phase-named submission APIs that refuse any phase not Running, `Retry.invalidated` carrying the target's transitive dependent closure, `set_phase_root` so gates evaluate in a writer's ephemeral workspace, and trace `phase_invalidated` (kind 16, format v6, layout unchanged; empty owner = crash reset on resume, named owner = supersession). Replay restores every in-flight dispatch as pending exactly once, retaining spent attempts, corrections and accepted evidence; a completed independent flight may still submit after a sibling halts the run, and its envelope persists while the run stays rejected. `bun run test:rs`: 2848 passed, 0 failed, 5 skipped.
- Driver (TypeScript): workflow `concurrency` 1–8 (default 1) validated at load — `> 1` requires `isolation: true` and explicit `writes` on every writer (`[]` denies all writes; guard `allowed` omission remains the serial unrestricted legacy). Each concurrent writer runs in its own cloned workspace with its own write guard; one integration owner applies accepted diffs serially onto the private run root through a durable journal (`integration.json` with prepared/applying/integrated/rejected states and a guard boundary), so an interrupted apply is finished or restored exactly once on resume and a rejected result never reaches the integrated tree. Overlapping accepted diffs refuse delivery terminally with both patches preserved; `code` phases drain all flights and run alone; invalidation aborts, drains and preserves stale flights before their replacements dispatch. `dependsOn` absent means declaration predecessor, `[]` independence; `onReject`/`inputs` validation is purely causal (transitive dependency), since topological "earlier" is undefined between independent phases. `bun test test/adw`: 196 passed, 0 failed (includes overlap/join, rejected-patch isolation, conflict refusal, exhausted-dependency evidence, late-writer draining and bounded resume regressions).
- Write guard: snapshots and settlement ignore `.gitignore` so protected or scoped files cannot be smuggled through ignore rules (`.git` internals and guard state stay excluded); attempt manifests bumped to version 2 so pre-cutover unfinished snapshots fail closed instead of misreading ignored files as creations.
- Viewer: phase instances group interleaved events by open instance, timeline bars share one origin so overlap renders as overlap, `phase_invalidated` marks exactly the superseded closure, `run_resumed` clears a stale terminal verdict, selected inputs link the producing instance, and per-seat token rows name the actual resolved model.
- Live (`anthropic/claude-sonnet-4-6`): run `adw-157729caffb91189` (`left ∥ right → join`, `concurrency: 2`) dispatched both writers before either finished (trace: both `phase_started` precede either accepted `phase_finished`), the `code` join ran only after both, consumed exactly `left v1` and `right v1` via `$ADW_INPUTS`, verified the combined tree (`left + right === 42`), delivery applied once to the checkout and the operator's protected scratch file survived byte-for-byte. Per-seat usage recorded ~81k tokens each with the resolved model id. The viewer rendered the overlapping bars, the join's causal input links and the accepted verdict against this real trace.
- Environment note: the workstation had no Rust toolchain; `rustup` (pinned `nightly-2026-08-08`) and `cargo-nextest` were installed to run the native suite. An interleaved execution of the ADW suite while cargo saturated all cores produced load-induced timing failures; the suite passes fully unloaded and in isolation.

## Delivery checklist

- Each milestone has an exercised success path and the named refusal/recovery paths.
- Documentation distinguishes implemented behavior, verified limitations and planned work.
- No milestone completion claim silently stands in for full SSSF equivalence.
- Keep changes local until publication is requested; no automatic push, PR or deployment is implied by this plan.
