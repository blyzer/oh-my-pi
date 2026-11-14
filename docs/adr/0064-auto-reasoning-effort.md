# 0064. Automatic reasoning effort is a Director that chooses effort only and never switches models

Status: Proposed 2026-11-14; EGM round 2 resolved, round 3 pending
Date: 2026-11-13 (revised 2026-11-14)
Area: control-plane

Research: [docs/research/auto-reasoning-effort.md](../research/auto-reasoning-effort.md) (Gap 5 of the upstream gap closure). Upstream pin `c308970263` (v18.20.0); omp2 base `origin/omp2 9cb6fd6389`. Citations: `upstream:` is the OMPv1 pin; bare paths are omp2. Source was read only; nothing was built or run for this record. This record states decisions; the note holds the evidence.

## Context

### Terms used here

- **Director** (ADR 0015): a standing behaviour stored as a `<director>` element in the session DOM. Hooks: `before_inference` (async, before each request; may make an isolated model call), `prepare_inference` (sync request edit), verdict hooks at a yield (`Pass` means "not mine") (`crates/contract/src/director.rs:630-690`). Directors nest: a newly engaged one becomes a child of the innermost active one (`crates/contract/src/director.rs:960-971`), and removing an element removes its subtree (`crates/dom/src/op.rs:47-48`).
- **Convar** (ADR 0012): a typed setting. Flags say where it may be set: `archive` (user config), `session` (journaled per session), `project` (repository cfg).
- **Bind / engagement layer**: a Director's `bind/<convar>` element property. Every active Director's binds form one control-plane layer above the user's own values; later layers win (`crates/con/src/ctx.rs:1060`). `value_below_engagements` is the value a bind shadows (`crates/con/src/ctx.rs:1169`).
- **`state/*`**: a Director's durable state properties on its element, written by a journaled `patch` of `Op::Set` operations (`crates/contract/src/director.rs:1628-1650`).
- **Policy clamp**: `convar_reasoning` in the driver, which maps an effort name to what the target model can take (`crates/driver/src/headless/kernel.rs:492-530`), followed by the router's own ceiling clamp.
- **Isolated call**: `execute_on(selector, request)` from a Director, served by the driver's `chat_on`, on another model or `@role` without changing the session's model (`crates/contract/src/director.rs:612-624`).
- **`@tiny`**: the role ADR 0023 reserves for harness chores; unassigned it resolves to `@commit`, then `@smol` (`crates/catalog/src/selection.rs:460-464`).

### What OMPv1 does

- `defaultThinkingLevel` accepts every effort plus `auto` (`upstream:packages/coding-agent/src/session/settings.ts:240-243`). `providers.autoThinkingMaxEffort` is `xhigh | max`, default `xhigh` (`upstream:packages/coding-agent/src/session/settings.ts:1156-1160`). `max` is reachable only when the setting says so and the model lists it (`upstream:packages/coding-agent/src/auto-thinking/classifier.ts:135-138`).
- Once per user turn, before the request, a judge answers one multiple-choice question on `low | medium | high | xhigh | max`, filtered to the efforts the session model supports (`upstream:packages/coding-agent/src/auto-thinking/classifier.ts:86-99`); with one eligible tier it asks nothing (`upstream:packages/coding-agent/src/auto-thinking/classifier.ts:169-177`). The judge role's defaults chain to `@tiny`, `@smol`, `@default` (`upstream:packages/coding-agent/src/config/model-resolver.ts:1280`).
- The judge input goes through the shared tiny-model preprocessor: ANSI, paired tag envelopes, fenced code and long hashes stripped, both ends kept, 2000 characters (`upstream:packages/coding-agent/src/tiny/message-preproc.ts:1-13`). The judge request runs with reasoning disabled, temperature 0, a 4096-token cap, and up to two parse retries with a forced answer tool (`upstream:packages/ai/src/judgment/chat.ts:28`, `upstream:packages/ai/src/judgment/chat.ts:50`, `upstream:packages/ai/src/judgment/chat.ts:78-80`).
- The result is clamped into the model's range, never below `low` unless the model has nothing at or above it, never above the ceiling, and `undefined` without a controllable effort (`upstream:packages/tui/src/thinking.ts:280-300`). A 4 s timeout; on failure the last level, or a provisional one capped at `xhigh` (`upstream:packages/coding-agent/src/session/model-controls.ts:657-667`, `upstream:packages/tui/src/thinking.ts:405-425`). A result that arrives after the user left `auto` is dropped (`upstream:packages/coding-agent/src/session/model-controls.ts:715-716`).
- `auto` is one value of the one selection; picking a level leaves it (`upstream:packages/coding-agent/src/session/model-controls.ts:586-598`). Only real user turns, and user-invoked skills, are classified (`upstream:packages/coding-agent/src/session/agent-session.ts:9645-9658`).
- It changes effort only, never the session model.

### What omp2 has

- `ai_thinking` (default `high`, values `off` to `max`, no `auto`; `crates/agent/src/vars.rs:142-149`); `ai_thinking_ceiling` default `max` (`crates/catalog/src/settings.rs:1203-1206`).
- **Effort to wire.** Requests are built with `reasoning: Unset` (`crates/agent/src/loop.rs:2343`). `ProductionInference::apply_convars` fills a still-`Unset` reasoning from the effective `ai_thinking` through `convar_reasoning` (`crates/driver/src/headless/kernel.rs:342-348`). `convar_reasoning` gives `Unset` for a model without a thinking policy and the policy default when routing cannot resolve the effort (`crates/driver/src/headless/kernel.rs:497-523`). The router fails a request that carries an effort for a model without a policy (`crates/ai/src/router.rs:1168-1187`).
- **Binds** are re-derived from the elements on every derivation (`crates/contract/src/director.rs:900-936`); a `Null` bind property is skipped (`crates/contract/src/director.rs:235-246`). The kernel derives them before projecting (`crates/agent/src/loop.rs:1518-1521`), not after `before_inference` (`crates/agent/src/loop.rs:1592-1638`). The chat badge and RPC read the effective `ai_thinking` (`crates/chat/src/host.rs:951-953`, `crates/app/src/rpc_mode.rs:1156`).
- **Isolated calls.** `ProductionInference::chat_on` re-targets the shared client and restores it only after the await (`crates/driver/src/headless/kernel.rs:1822-1839`); the kernel drops a `before_inference` future on cancel and on any mailbox control message (`crates/agent/src/loop.rs:1592-1624`). `chat_on` gives an `Unset` reasoning the session's `ai_thinking` (`crates/driver/src/headless/kernel.rs:424-436`). Every existing isolated caller sends `Unset` (`crates/agent/src/directors/advisor.rs:697`, `crates/driver/src/headless/reflection.rs:140`).
- **Roles.** `@judge` (ADR 0053, proposed) falls back to the slow model (`crates/catalog/src/selection.rs:465`). ADR 0023 (accepted) requires small chores, naming "classify this prompt's difficulty", to go to `@tiny`, never to the frontier model by default, with a fixed output ladder and earliest-match parsing (`docs/adr/0023-tiny-local-model.md:9-10`, `docs/adr/0023-tiny-local-model.md:21-30`). `@tiny` already serves speech rewriting (`crates/driver/src/headless/kernel.rs:1957`). The upstream preprocessor is ported as `omp_ai::local::message_preproc::preprocess_tiny_message`, behind `omp-ai`'s dependency-free `local` feature (`crates/ai/src/local/message_preproc.rs:9-31`, `crates/ai/Cargo.toml:18`).
- **User turns.** A prompt is a `<user>` child of the turn (`crates/session/src/session.rs:632-638`); a user-invoked skill is `<user skill_prompt>` (`crates/session/src/session.rs:716-727`). Synthetic inputs are marked: steering asides `<user steering>` (`crates/agent/src/steering.rs:522-524`), queued steering `status=queued` (`crates/agent/src/steering.rs:295-297`), job results `<user async_result>` (`crates/agent/src/loop.rs:2119-2121`); custom messages are `<developer>` (`crates/session/src/custom_message.rs:169`). Continuations (`Verdict::Continue`, `Done` with continue) loop inside the same turn (`crates/contract/src/director.rs:1285-1292`).
- **Gates.** `RuntimeFlags` is read once at composition (`crates/agent/src/loop.rs:437-446`, `crates/driver/src/headless/kernel.rs:2969-2990`). The kernel engages compaction per turn when absent (`crates/agent/src/loop.rs:1419-1425`).
- **Children.** A child seeds every convar's effective value from the parent (`crates/con/src/ctx.rs:1215-1236`; ADR 0013), then `subagent.cfg`, the class cfg, then the spawner's explicit writes (`docs/adr/0013-inheritance-by-seeding-and-cfg.md:53-55`). The spawner writes the task's effort and its maximum to the child's class layer (`crates/driver/src/subagent/spawn.rs:1174-1225`).
- **Usage.** `ReceiptRole` is `Primary | Advisor` (`crates/journal/src/data.rs:630-636`); compaction reads the latest non-advisor usage as context size (`crates/agent/src/directors/compaction.rs:797-814`).
- The chat host writes `ai_thinking` on model switch, effort cycling and the picker, reading the effective value first (`crates/chat/src/host.rs:3956-3967`, `crates/chat/src/host.rs:4003-4009`, `crates/chat/src/host.rs:4044-4047`); `Plan` and `Prewalk` write it on handoff (`crates/agent/src/directors/plan.rs:137`, `crates/agent/src/directors/prewalk.rs:60`). No built-in Director binds `ai_thinking` (`crates/agent/src/directors/plan.rs:228-233`, `crates/agent/src/directors/vibe.rs:60-63`).

### Rules that bind

- ADR 0015 (Directors), 0012 (convars), 0013 (seeding order), 0017 (routes clamp), 0023 (chores on `@tiny`), 0035 (typed errors).
- Additive only, deterministic first, native naming (`docs/research/upstream-gap-closure-brief.md`, "Hard constraints").
- Effort only, never the session model (same brief, gap 5).

## Decision

**D1. Director `auto_effort`** in `crates/agent/src/directors/auto_effort.rs`, registered in `register_standard` (`crates/agent/src/directors/mod.rs:40`). No `Slot`, no yields, `Pass` from every verdict hook, no `prepare_inference`.

**D2. Gate and engagement.** New convar `ai_auto_effort` (bool, default off, flags `archive | session`, no `project`: a repository must not turn on model calls), declared in `crates/agent/src/vars.rs` beside `ai_thinking`. It is read live from the control plane, never through `RuntimeFlags`.
- At each turn start, beside the compaction check (`crates/agent/src/loop.rs:1419-1425`), the kernel engages `auto_effort` when `ai_auto_effort` is on and none is active or queued. The kernel never removes it.
- The Director reads `ai_auto_effort` on every request; when off it is inert (D3). Turning the gate off takes effect on the next request, on again on the next user turn.
- If an outer Director's exit removes it with its subtree, the next turn start re-engages it with fresh state; the remaining requests of that turn run at the user's value.

**D3. Inert.** The Director makes no judge call and sets `bind/ai_thinking` to `Null` (if not already) when any holds: `ai_auto_effort` is off; `ai_external_thinking` is on (`crates/driver/src/headless/kernel.rs:344-346`); the ladder L (D5) is empty; another active Director's element carries a non-`Null` `bind/ai_thinking`. The last check uses a new `omp_contract::director::bind_holders(dom, name)`, which walks the active chain as `DirectorStack::from_dom` does (`crates/contract/src/director.rs:846-861`) and yields the handles whose `bind/<name>` is set. Inert stretches keep `state/effort`.

**D4. Precedence.** One rule, no pin detection: while `ai_auto_effort` is on and the Director is not inert, its bind supplies `ai_thinking`. Every write of `ai_thinking` (user, picker, effort cycling, model-switch clamp, `Plan`/`Prewalk` handoff, cfg, spawner) lands below the bind: it seeds F (D5) and applies whenever the Director is inert, but does not override the auto choice. To fix a level, the user turns `ai_auto_effort` off. This deviates from upstream, where picking a level leaves `auto` (`upstream:packages/coding-agent/src/session/model-controls.ts:586-598`); making the picker or cycling also clear the gate changes existing host behaviour and is O2.

**D5. Ladder and fallback.** For the request's route:
- L = the route's `RouteFacts::thinking_efforts` (D8) at or below `min(ai_auto_effort_ceiling, ai_thinking_ceiling)`, then restricted to `low` and above when any such tier remains (else the sub-`low` tiers stay, `upstream:packages/tui/src/thinking.ts:296-298`).
- `clamp_L(x)` = the greatest element of L at or below x, else the lowest element of L.
- **F**, the fallback effort, = `clamp_L(state/effort)` when set, else `clamp_L(value_below_engagements("ai_thinking"))`. F is re-clamped on every request, so a model or ceiling change never leaves an out-of-ladder bind.

**D6. Classification, once per user turn.** In `before_inference`:
1. Run D3; if inert, stop.
2. The turn is classifiable only when it has no `<assistant>` child yet and its first `<user>` child carries none of `steering`, `async_result`, `status=queued` (a `skill_prompt` user counts). Otherwise set the bind to F and stop. Its text, through `preprocess_tiny_message`, is t; attachments are ignored.
3. Deterministic shortcuts, no judge call: L has one element → that element; t is empty → F; `state/cooldown > 0` → F, and decrement it; `state/attempted` equals this turn → F (a dropped earlier attempt, D7).
4. Otherwise the judge (D7).
The effort is written as `bind/ai_thinking` with `state/effort` and `state/turn` in one patch. Later requests of the turn re-run step 1 and re-apply F's clamp only. A journal replay re-derives the bind and makes no call, because `state/turn` or `state/attempted` already names the turn.

**D7. The judge call.**
- Selector `@tiny` (ADR 0023), never `@judge`.
- Before the call, one patch sets `state/attempted` to the turn, so a future the kernel drops (`crates/agent/src/loop.rs:1592-1624`) costs at most one call per turn.
- Prompt: `crates/agent/prompts/auto_effort/judge.md` (instructions, adapted from `upstream:packages/coding-agent/src/prompts/judge/auto-thinking.json` `level`) and `crates/agent/prompts/auto_effort/criteria.md` (one `label: criterion` line per label), both `include_str!`; the Rust renderer appends the criteria of the labels in L only and t inside `<request>` tags.
- Request: explicit `off` effort (S0b resolves it for the judge model), temperature 0, `max_output_tokens` 4096, no tools.
- Parse, earliest match (ADR 0023): split the answer on whitespace, strip surrounding punctuation, lowercase; the first word equal to a label in L wins. None → unparseable. One attempt: upstream's two forced-tool retries do not fit the 4 s budget.
- `tokio::time::timeout` of 4 s around opening the stream and draining it; usage comes from the `Completed` event, as the advisor reads it (`crates/agent/src/directors/advisor.rs:753`).
- After the call, re-run D3 before writing: if the gate went off or another bind appeared during the await, drop the result (as `upstream:packages/coding-agent/src/session/model-controls.ts:715-716`).
- Any timeout, error or unparseable answer → F; `state/failures` += 1; at 3 consecutive failures, `state/cooldown` = 10 user turns and `state/failures` = 0. A success resets `state/failures`.
- Counters in state: `judge_calls`, `judge_fallbacks`, `judge_tokens_in`, `judge_tokens_out`. No `TurnReceipt` (O5), no journaled notice; a `tracing::debug!` event names the outcome.

**D8. Applying the choice: a bind, resolved by the policy clamp.** The Director's only output is its `bind/ai_thinking`. It returns `Prepared::Unchanged`: the projected request does not depend on `ai_thinking`, which is read at `chat` time (`crates/agent/src/loop.rs:2343`, `crates/driver/src/headless/kernel.rs:342-348`). The kernel re-derives binds once more after the preflight: one `directors.apply_binds(session.dom(), con)` after the `Prepared::Rebuild` block closes (`crates/agent/src/loop.rs:1629-1638`) and before `response_ordinal` is computed; a no-op when nothing changed (`crates/con/src/ctx.rs:1123-1137`). The effective `ai_thinking` then reaches the wire through `convar_reasoning` exactly like a user value. New field `RouteFacts::thinking_efforts: Option<Arc<[ThinkingEffort]>>`, filled by the driver's `route_facts` from the model's thinking policy (`crates/driver/src/headless/kernel.rs:3554`), `None` without a catalog route (so gateways and headless tests are inert).

**D9. Effort only, enforced.** Never writes `ai_model` or any session convar, never reads or writes a model role, never sets `ChatRequest::reasoning` on the main request, binds nothing but `ai_thinking`, calls only `@tiny`. Tests fail on any other `bind/*`, any `DirectorEffect` write, or a different main-request target with the gate on versus off.

**D10. Ceiling.** New convar `ai_auto_effort_ceiling` (`ThinkingEffort`, default `xhigh`, flags `archive`, `legacy.path` `providers.autoThinkingMaxEffort`), in `crates/catalog/src/settings.rs` beside `ai_thinking_ceiling`, with a `validate:` function (as `AI_THINKING_BUDGETS`, `crates/catalog/src/settings.rs:1208-1214`) that accepts only `xhigh` and `max`.

**D11. Children.** When the parent's `ai_auto_effort` is on, `configure_child_route` (`crates/driver/src/subagent/spawn.rs:1174`) also writes `ai_auto_effort 0` in the child's class layer: the spawner-explicit position of ADR 0013's order (`docs/adr/0013-inheritance-by-seeding-and-cfg.md:53-55`), not a private inheritance rule. A child starts from the parent's effective `ai_thinking` (the auto effort at spawn), then its cfg, class and task effort as today, and never classifies. Upstream's `solutionSpace` input for task turns is not ported (O3).

## Not ported from upstream

The judgment cache; the on-device three-bucket question; `solutionSpace` classification of task turns (D11); upstream's provisional level (D5's F uses the user's value instead); parse retries (D7); `ultrathink`/`chillax` (O8); asking the judge when the only tier is `max` (D6 takes it).

## Rejected options

| Option | Why not |
|---|---|
| Set `req.reasoning` in `prepare_inference` | Skips `convar_reasoning` (`crates/driver/src/headless/kernel.rs:346`); a policy-less model then fails the turn (`crates/ai/src/router.rs:1179-1185`). |
| Session-layer write of `ai_thinking` | Overwrites the user's value and commits only at a yield boundary. |
| `@judge` as the classifier | Falls back to the slow model (`crates/catalog/src/selection.rs:465`); ADR 0023 puts this chore on `@tiny`. |
| Pin on a changed value below the bind (round-1 D4.2) | Model switches, cycling and handoffs write `ai_thinking` (`crates/chat/src/host.rs:3956-3967`), so routine actions would end auto; an equal-value pick is invisible. |
| Kernel removes the Director when the gate is off | Removal takes later-engaged Directors nested under it (`crates/contract/src/director.rs:1054-1061`). |
| Engage once at composition, as stream rules are (`crates/driver/src/headless/kernel.rs:2911-2918`) | An outer Director's exit would remove it for the rest of the process; per-turn engagement recovers. |
| A short-prompt word/grapheme rule (round-1 D6) | Upstream has none (`upstream:packages/coding-agent/src/auto-thinking/classifier.ts:147-197`); whitespace word counts fail on unspaced scripts; savings unmeasured. |
| A new `Slot::Effort` | Effort owners write or bind, they do not claim; a conflict queues rather than yields. |
| Router-side size rule; session-layer TS port; switching models | Cannot read the prompt; races Directors; out of scope by the assignment. |

## Slices

One PR each; tests written first and not edited to pass. Verify with `just test-pkg <crate>` for the named crates, then `just lint`.

**S0a. Drop-safe `chat_on`** (`crates/driver`). `ProductionInference::chat_on` restores the live call metadata on every exit, including drop. It fixes the advisor's exposure too and may land as a standalone fix.
- Tests (unit, `crates/driver/src/headless/kernel.rs` tests, with a never-answering local TCP provider as in `crates/driver/tests/provider_rejection_surfaces.rs`): `dropped_chat_on_restores_the_live_target`, `timed_out_chat_on_restores_the_live_target`, `completed_chat_on_still_restores`.
- Acceptance: `crates/agent/tests/advisor.rs` passes unchanged. Gate: none (O7).

**S0b. Explicit effort on `ProductionInference::chat_on`** (`crates/driver`). A request whose `reasoning` names an effort is re-resolved for the target model by a new `isolated_reasoning(catalog, model, requested: &ReasoningRequest)`: no policy → `Unset`; the effort if `policy.supports` it (`crates/catalog/src/thinking.rs:279-284`); else, for `off`, the lowest listed effort (`policy.efforts` is never empty, `crates/catalog/src/thinking.rs:251-254`); else `clamp_thinking_effort`; never `policy.default_level`; if routing still cannot resolve it, `Unset`. Only the effort is rewritten; visibility, `max_tokens` and `preserve_signatures` are kept. `Unset` requests keep `default_reasoning`.
- Tests (beside `crates/driver/src/headless/kernel.rs:5049-5097`): `explicit_off_ignores_ai_thinking`, `explicit_effort_on_policyless_model_is_unset`, `explicit_off_on_effort_required_model_takes_lowest_listed`, `explicit_off_never_takes_default_level`, `explicit_effort_keeps_visibility_and_budget`, `unset_isolated_call_still_inherits_ai_thinking`.
- Gate: none; no existing caller sends an explicit effort.

**S1. Settings** (`crates/agent`, `crates/catalog`). D2 and D10; nothing reads them.
- Tests: `ai_auto_effort_defaults_off` (`crates/agent/tests/directors/auto_effort.rs`); `auto_effort_ceiling_accepts_only_xhigh_and_max`, `auto_effort_ceiling_defaults_to_xhigh`, `legacy_path_maps_providers_auto_thinking_max_effort` (`crates/catalog/src/settings.rs` tests); `ai_auto_effort_is_refused_in_project_cfg` (`crates/driver/tests/project_cfg_authority.rs`).
- Gate: `ai_auto_effort` default off.

**S2. Director without the judge** (`crates/agent`, `crates/contract`, `crates/driver`). D1-D6, D8, D9, D11; the judge branch returns F and counts `state/judge_skipped`. Adds `RouteFacts::thinking_efforts`, `bind_holders`, the kernel engage and the extra `apply_binds`, and the spawner write.
- Tests (`crates/agent/tests/directors/auto_effort.rs`, registered in `crates/agent/tests/directors/main.rs`; fake `ErasedInference` counting calls; route facts with a ladder):
  - gate: `engaged_at_turn_start_when_on`, `never_removed_when_off`, `gate_read_live_not_from_runtime_flags`, `reengaged_after_outer_director_exit`;
  - inert: `inert_when_gate_off_clears_bind`, `inert_under_external_thinking`, `inert_without_ladder`, `yields_to_another_directors_thinking_bind`;
  - precedence: `user_write_below_bind_seeds_fallback_only`, `plan_handoff_effort_is_shadowed_while_on`;
  - turn shape: `steering_aside_is_not_classified`, `async_result_turn_is_not_classified`, `skill_prompt_is_classified`, `continuation_never_reclassifies`, `effort_is_constant_inside_a_turn`;
  - ladder: `single_tier_needs_no_judge`, `sub_low_only_ladder_takes_its_tier`, `empty_prompt_takes_fallback`, `fallback_reclamped_after_model_change`;
  - invariant: `only_bind_is_ai_thinking`, `no_effect_carries_a_write`, `main_request_reasoning_never_set`, `claims_no_slot`.
- Driver tests: `bound_effort_on_policyless_model_sends_no_reasoning`, `bound_effort_reaches_wire_through_convar_reasoning`, `bind_written_in_preflight_applies_to_same_request`, `child_of_auto_parent_spawns_with_gate_off` (`crates/driver/tests/subagent_cfg.rs`).
- Acceptance: gate off, all existing Director, kernel and spawn tests pass unchanged. Gate: `ai_auto_effort`. Depends on: S1.

**S3. The `@tiny` judge** (`crates/agent`; `omp-agent` enables `omp-ai/local`). D7.
- Tests (fake `ErasedInference` that records selector and request and answers from a script): `judge_selector_is_tiny`, `each_label_maps_to_its_effort`, `ladder_offers_max_only_when_ceiling_and_model_allow`, `earliest_label_wins`, `label_outside_ladder_falls_back`, `unparseable_falls_back`, `timeout_falls_back_without_failing_the_turn`, `three_failures_start_a_ten_turn_cooldown`, `dropped_attempt_is_not_retried_in_the_turn`, `gate_off_during_await_drops_result`, `judge_request_carries_explicit_off`, `judge_input_is_preprocessed`, `classifiable_prompt_calls_the_judge_once`, `replay_makes_no_judge_call`, `usage_counters_come_from_completed_event`.
- Snapshot: `insta` of the rendered prompt for L = `low..xhigh` and `low..max`, in the module's tests.
- Driver test: `judge_timeout_leaves_main_route_on_session_model` (needs S0a).
- Gate: `ai_auto_effort`. Depends on: S0a, S0b, S2.

**S4. Surfaces and joined proof.**
- Tests: `badge_shows_bound_auto_effort` (`crates/chat`), `rpc_state_reports_bound_auto_effort` (`crates/app`), `model_watch_reports_auto_effort_as_director_supplied` (`crates/agent/tests/model_watch.rs`).
- Joined proof, driver seam: a kernel composed over `ProductionInference` against a local TCP provider (the patterns of `crates/driver/tests/approval_authority.rs:322` and `crates/driver/tests/provider_rejection_surfaces.rs`), two user turns, a scripted `@tiny` answer, asserting the classified effort on each turn's first request and its tool continuation, and the session model on every main request. UNVERIFIED that both patterns combine in one test; if they do not, S4 adds the missing support helper first.
- E2E: a P6 case (`crates/e2e/tests/p6_crash_resume.rs`, in-process `Kernel` as in its `redirect_crash` module at `:615-632`) with `ScriptedInference` extended to report route facts with a ladder and to answer `chat_on("@tiny")` from its own script: kill after the classification patch, resume, assert no second judge call and the same bind.
- Golden journal: a new `auto_effort` family in `crates/session/tests/golden_journals.rs` (bind `Op::Set` plus state patch).
- Gate: `ai_auto_effort`. Depends on: S3.

Estimate: 4 to 6 days. [INFERENCE]

## Upstream origin and license

Drawn from `upstream:packages/coding-agent/src/auto-thinking/classifier.ts`, the rubric `upstream:packages/coding-agent/src/prompts/judge/auto-thinking.json`, `upstream:packages/tui/src/thinking.ts` (clamp rules) and `upstream:packages/ai/src/judgment/chat.ts` (judge request settings). TypeScript, rewritten in Rust. MIT (copyright Mario Zechner, Can Bölük, Stencil Labs, Inc.; `upstream:LICENSE:1-5`); keep the notice with the adapted rubric text. No third-party code.

## Consequences

- Cost, with the gate on: per classifiable user turn, one `@tiny` call of about 1.2k input tokens (rubric plus at most 2000 characters) and a few output tokens, up to 4 s before the first request. [INFERENCE] After three failures the judge is skipped for ten turns.
- Effort may change between user turns, which can break a provider's prefix cache on some routes; it does not change inside a turn unless the Director turns inert or is removed mid-turn.
- While on, effort cycling, the picker and `Plan`/`Prewalk` handoff efforts change only the stored value under the bind; the cycle's notice names a level that does not apply (`crates/chat/src/host.rs:4003-4013`). Documented; O2.
- The badge shows a concrete effort but not that auto chose it. Decided here to add no indicator; O6.
- Isolated calls that inherit `ai_thinking` (advisor, reflection) follow the auto effort while bound.
- Privacy: with the gate on, up to 2000 characters of each prompt go to the `@tiny` route, which may be another provider or account than the session's. The gate is opt-in and documented as such.
- Judge spend is in Director state only, not session usage totals (O5).
- S0a and S0b change existing driver code ungated; both are invisible on every path that works today (O7).
- The kernel's turn start gains a third feature-specific engagement beside goal and compaction (`crates/agent/src/loop.rs:1409-1425`); a generic "standing Director with a gate" registry hook would replace all three but changes the registry API, so it is left for a separate record.

## Open questions for the owner

- **O1. `auto` as a value of `ai_thinking`, or the separate gate?** Recommendation: the gate `ai_auto_effort` (D2), so an existing convar's values do not change.
- **O2. Should picking or cycling a level turn `ai_auto_effort` off, as upstream leaves `auto`?** It changes existing chat-host behaviour (`crates/chat/src/host.rs:4003-4009`). Recommendation: yes, in a follow-up change once the Director has landed.
- **O3. Should task children classify (upstream's `solutionSpace`)?** D11 keeps them off. Recommendation: revisit with a measured benefit.
- **O4.** Closed in round 2: the classifier uses `@tiny` (D7).
- **O5. A judge receipt role.** `ReceiptRole::Judge` would put spend in usage totals but changes a journaled enum (`crates/journal/src/data.rs:630-636`). Recommendation: owner decides; counters stay in state meanwhile.
- **O6. Map `defaultThinkingLevel: auto` in the importer, and show "auto" on the badge?** Both change existing surfaces (`crates/catalog/src/settings.rs:1199`, `crates/chat/src/host.rs:951-953`). Recommendation: follow-ups after O1.
- **O7. Ungated fixes S0a and S0b.** Recommendation: accept; gating a drop guard would keep the advisor's leak.
- **O8.** `ultrathink` and `chillax`: out of scope.

## What the review changed

### Round 1 (Goldfish, 2026-11-13; resolved 2026-11-14)

Five findings, recorded in `docs/adr/STATUS.md` row 0064, each checked against this tree and the upstream pin. Some round-1 resolutions were replaced in round 2; the replacement is named.

- **D7 bypasses the policy clamp, so a turn can fail.** Confirmed (`crates/driver/src/headless/kernel.rs:346`, `crates/driver/src/headless/kernel.rs:497-523`, `crates/ai/src/router.rs:1179-1185`, `upstream:packages/tui/src/thinking.ts:283-287`). Changed: the choice is an `ai_thinking` bind resolved by `convar_reasoning` (now D8).
- **A `chat_on` drop leaves the client on the judge's model.** Confirmed (`crates/driver/src/headless/kernel.rs:1832-1837`, `crates/driver/src/headless/kernel.rs:330-341`, `crates/agent/src/loop.rs:1592-1624`). Changed: slice S0a.
- **The judge inherits `ai_thinking`.** Confirmed (`crates/driver/src/headless/kernel.rs:424-436`; upstream disables it, `upstream:packages/ai/src/judgment/chat.ts:80`). Changed: explicit `off` effort and slice S0b.
- **Engagement gate and precedence unspecified.** Confirmed; the "slot" framing rejected, since effort is not a `Slot` (`crates/contract/src/director.rs:190-199`). Changed: gate, inert conditions and precedence (replaced in round 2 by D2-D4).
- **S2 thresholds unspecified.** Confirmed. Changed: two named thresholds (removed in round 2, M9).
- Corrected while revising: no judge receipt role exists (`crates/journal/src/data.rs:630-636`); binds are re-derived from the element (`crates/contract/src/director.rs:900-936`).

### Round 2 (Goldfish, 2026-11-14: 2 blockers, 13 majors, 14 minors)

Four fresh-context reviewers and a synthesis. Every finding checked against source.

- **B1. `@judge` resolves to the slow model and contradicts accepted ADR 0023.** Confirmed: `crates/catalog/src/selection.rs:465`; `docs/adr/0023-tiny-local-model.md:9-10`, `:21-23`; upstream's judge chains to `@tiny` (`upstream:packages/coding-agent/src/config/model-resolver.ts:1280`). ADR 0023 is respected, not reopened. Changed: D7 uses `@tiny`; earliest-match parsing per ADR 0023 rule 2; O4 closed; dependency on ADR 0053 removed.
- **B2. Removing or nesting the Director can destroy other Directors and reset a pin.** Confirmed: parent is the innermost active (`crates/contract/src/director.rs:960-971`), `exit` and `Done` remove the subtree (`crates/contract/src/director.rs:1054-1061`, `:1285-1288`; `crates/dom/src/op.rs:47-48`). Changed: the kernel never removes it (D2); pin state no longer exists (D4); removal by an outer exit costs only state and recovers at the next turn start (D2, Consequences).
- **M1. Gate not a live read; reset depends on turn timing.** Confirmed: `RuntimeFlags` is a composition snapshot (`crates/driver/src/headless/kernel.rs:2969-2990`). Changed: D2 reads `ai_auto_effort` live per request; no reset concept remains.
- **M2. Value-diff pin detection gives false positives and misses equal picks.** Confirmed: `crates/chat/src/host.rs:3956-3967`, `:4003-4009`; `crates/agent/src/directors/plan.rs:137`. Changed: D4 replaces detection with one rule (bind wins while on; the gate is the pin); the deviation from upstream is recorded; O2.
- **M3. A level chosen during the judge await is shadowed; `base` read late.** Partly confirmed (`crates/agent/src/loop.rs:1592-1600`). `base` no longer exists (D4), and under D4 a level write is shadowed by design. Accepted part: D7 re-checks the gate and other binds after the await and drops the result.
- **M4. "User turn" undefined.** Confirmed: steering, async results, queued steering and custom messages are distinct nodes (`crates/agent/src/steering.rs:295-297`, `:522-524`; `crates/agent/src/loop.rs:2119-2121`; `crates/session/src/custom_message.rs:169`). Changed: D6 step 2 names the discriminator; S2 turn-shape tests.
- **M5. Children inherit the gate and override the spawner's effort.** Confirmed: `crates/con/src/ctx.rs:1215-1236`; `crates/driver/src/subagent/spawn.rs:1174-1225`; ADR 0013 forbids private inheritance rules (`docs/adr/0013-inheritance-by-seeding-and-cfg.md:53-55`). Changed: D11 has the spawner write the gate off, in ADR 0013's spawner position; O3 narrowed.
- **M6. No state schema; F has no storage.** Confirmed. Changed: D5 defines F and L once; D6 and D7 name every `state/*` key (`effort`, `turn`, `attempted`, `failures`, `cooldown`, counters); inert stretches keep `state/effort`.
- **M7. S0b's fallback does not match `convar_reasoning`.** Confirmed: on a resolve error it returns `policy.default_level` (`crates/driver/src/headless/kernel.rs:519-522`; `crates/catalog/src/thinking.rs:279-284`). Changed: S0b specifies `isolated_reasoning` with its order, never the default level, and the preserved fields.
- **M8. Judge input weaker than upstream; wrong citations.** Confirmed: `upstream:packages/coding-agent/src/tiny/message-preproc.ts:1-13`; omp2 already has `preprocess_tiny_message` (`crates/ai/src/local/message_preproc.rs:17-31`). Changed: D7 reuses it; one attempt recorded as a deliberate deviation; citations fixed.
- **M9. Short-prompt rule invented, CJK hole, false claim.** Confirmed: whitespace words fail on unspaced scripts; upstream has no rule (`upstream:packages/coding-agent/src/auto-thinking/classifier.ts:147-197`). Changed: rule removed; only the single-tier and empty-prompt shortcuts remain; the false claim is gone.
- **M10. No cost estimate or breaker; failures silent and repeated.** Confirmed (`crates/agent/src/loop.rs:1592-1624`). Changed: D7 breaker (3 failures → 10-turn cooldown), one attempt per turn via `state/attempted`, counters, cost estimate in Consequences. A metrics counter through `omp-observability` is not added: `omp-agent` does not depend on it (`crates/agent/Cargo.toml`), and the state counters are replay-honest.
- **M11. `Prepared::Rebuild` unjustified; `apply_binds` placement ambiguous.** Confirmed (`crates/agent/src/loop.rs:1629-1638`, `:2343`). Changed: D8 returns `Unchanged` and names the placement. Rejected part: recomputing route facts after `apply_binds`; the route at `crates/agent/src/loop.rs:1459` predates the existing bind derivation at `:1520` for every Director, and a stale ladder can only offer labels that `convar_reasoning` then clamps for the actual model (`crates/driver/src/headless/kernel.rs:342-348`).
- **M12. E2E cases name a harness that cannot exercise them.** Confirmed: P9 drives a bare stack with default route facts (`crates/e2e/tests/p9_extension_control.rs:63-75`). Changed: S4 moves the wire proof to the driver seam and the replay proof to P6's in-process kernel, with the needed `ScriptedInference` extension named; P9 dropped.
- **M13. Vocabulary undefined.** Confirmed. Changed: "Terms used here" in Context.
- **m1. Inert vs empty ladder; ladder edges.** Confirmed (`upstream:packages/coding-agent/src/auto-thinking/classifier.ts:169-177`). Changed: empty L is inert and clears the bind (D3); F re-clamped every request (D5); sub-`low` ladders follow upstream; the `max`-only edge is listed under "Not ported".
- **m2. No mechanism for reading other binds.** Confirmed: `MutDirectorCx` has the DOM only (`crates/contract/src/director.rs:532-560`). Changed: `bind_holders` helper (D3). The rule is kept because Python extension Directors can declare binds (ADR 0015).
- **m3. Usage ledger and notice unspecified.** Confirmed. Changed: usage from the `Completed` event, no cost field, no journaled notice (D7).
- **m4. Prompt file and renderer unspecified; wrong golden file.** Confirmed: `crates/agent/tests/prompt_golden.rs:1-12` tests slot assembly. Changed: two `.md` files under `crates/agent/prompts/auto_effort/`, Rust renderer, `insta` snapshot in the module (S3).
- **m5. Ceiling validation, naming, test files.** Confirmed. Changed: `validate:` function (D10), convars renamed `ai_auto_effort` and `ai_auto_effort_ceiling` to share the `ai_*` prefix of the inference convars, S1 tests name their files. Rejected part: dropping the ceiling convar; it maps a real upstream setting (`upstream:packages/coding-agent/src/session/settings.ts:1156-1160`).
- **m6. A third hardcoded engage in the kernel.** Confirmed (`crates/agent/src/loop.rs:1409-1425`). Kept, following that precedent; the generic alternative is recorded in Consequences.
- **m7. S0a and S0b scope.** Confirmed. Changed: S0b scoped to `ProductionInference::chat_on`; S0a may land standalone.
- **m8. Users cannot tell auto from inert.** Confirmed (`crates/chat/src/host.rs:951-953`). Recorded as a decision in Consequences; O6.
- **m9. F used before defined.** Confirmed. Changed: D5 defines it first.
- **m10. "No judge yet" tests unclear.** Confirmed. Changed: S2 counts `state/judge_skipped`; judge-routing tests moved to S3.
- **m11. Judge input privacy.** Confirmed. Changed: Consequences.
- **m12. STATUS.md row stale.** Rejected for this change: the orchestrator owns `docs/adr/STATUS.md` and updates the row after the review rounds.
- **m13. Altitude: test inventory and parse rules in the record.** Rejected: this repo's gap ADRs list named tests in their slices (`docs/adr/0071-subagent-isolation.md:91`, `:101`). Undefined terms are covered by M13; "Rules that bind" now cites the brief.
- **m14. Upstream divergences not listed.** Confirmed. Changed: "Not ported from upstream".

Machine-reviewed (EGM, 2 rounds so far), not owner-approved.

## Acceptance tests

1. (S0a) A dropped or timed-out isolated call leaves the next main request on the session model.
2. (S0b) An isolated call with an explicit effort carries that effort resolved for its own model, never `ai_thinking`, never the policy default, and never fails on a policy-less model.
3. (S1) `ai_auto_effort` defaults off and is refused in project cfg; `ai_auto_effort_ceiling` defaults `xhigh`, accepts only `xhigh | max`, and imports from `providers.autoThinkingMaxEffort`.
4. (S2) Gate off: nothing changes. Gate on: the bound effort reaches the wire only through `convar_reasoning`; synthetic turns and continuations are not classified; children spawn with the gate off.
5. (S3) The judge is `@tiny` with an explicit `off`; every failure falls back to F without failing the turn; the breaker and one-attempt rule hold; replay makes no call.
6. (S4) Badge, RPC and `ModelWatch` show the bound effort; the driver joined proof, the P6 case and the golden family pass.

## Status in omp

**Status: Not started.** Reusable today: the Director trait and registry (`crates/contract/src/director.rs:630`), bind derivation (`crates/contract/src/director.rs:900-936`), `execute_on` (`crates/contract/src/director.rs:619`), the `@tiny` role (`crates/catalog/src/selection.rs:460-464`), `preprocess_tiny_message` (`crates/ai/src/local/message_preproc.rs:17`), `convar_reasoning` (`crates/driver/src/headless/kernel.rs:492`), `ModelWatch` (`crates/agent/src/model_watch.rs`). UNVERIFIED: how a remote gateway resolves an explicit effort (D8 keeps the Director inert there).

## References

[docs/research/auto-reasoning-effort.md](../research/auto-reasoning-effort.md); ADR 0012, 0013, 0015, 0017, 0023, 0035. Checked for collision: ADR 0006, 0024, 0036 (no tool, sandbox or extension-runtime change); ADR 0049 (no vendor code); ADR 0053 (`@judge` not used).
