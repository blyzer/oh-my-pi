# 0064. Automatic reasoning effort is a Director that chooses effort only and never switches models

Status: Proposed 2026-11-14; EGM round 1 resolved, rounds 2-3 pending
Date: 2026-11-13 (revised 2026-11-14)
Area: control-plane

Research: [docs/research/auto-reasoning-effort.md](../research/auto-reasoning-effort.md) (Gap 5 of the upstream gap closure). Upstream pin `c308970263` (v18.20.0); omp2 base `origin/omp2 9cb6fd6389`. Citations: `upstream:` is the OMPv1 pin; bare paths are omp2. Source was read only; nothing was built or run for this record. This record states decisions; the note holds the evidence.

## Context

### What OMPv1 does

- `defaultThinkingLevel` accepts every effort plus `auto` (`upstream:packages/coding-agent/src/session/settings.ts:240-243`). `providers.autoThinkingMaxEffort` is `xhigh | max`, default `xhigh` (`upstream:packages/coding-agent/src/session/settings.ts:1156-1160`). The classifier may reach `max` only when the setting says so and the model lists that tier (`upstream:packages/coding-agent/src/auto-thinking/classifier.ts:135-138`).
- Once per user turn, before the request, a judge resolved from the `judge` role answers one multiple-choice question on the ladder `low | medium | high | xhigh | max`, filtered to the efforts the session model supports (`upstream:packages/coding-agent/src/auto-thinking/classifier.ts:86-99`). With one eligible tier it asks nothing (`upstream:packages/coding-agent/src/auto-thinking/classifier.ts:169-177`). The judge input is cut to 2000 characters (`upstream:packages/coding-agent/src/tiny/message-preproc.ts:12`).
- The judge request itself runs with reasoning disabled, temperature 0 and a 4096-token output cap; a route that cannot disable reasoning falls to its lowest effort (`upstream:packages/ai/src/judgment/chat.ts:15-28`, `upstream:packages/ai/src/judgment/chat.ts:42`, `upstream:packages/ai/src/judgment/chat.ts:76-80`). It never inherits the session's effort.
- The result is clamped into the model's supported range, never below `low`, never above the ceiling, and is `undefined` when the model has no controllable effort, "so `auto` must not forward a concrete effort that would then trip `requireSupportedEffort` downstream" (`upstream:packages/tui/src/thinking.ts:280-300`). The session then applies it through the same clamp a user-chosen level takes (`clampThinkingLevelToCeiling`, `upstream:packages/coding-agent/src/session/model-controls.ts:718-724`).
- A 4 s timeout; on failure the last classified level, or a provisional one capped at `xhigh` (`upstream:packages/coding-agent/src/session/model-controls.ts:657-667`, `upstream:packages/tui/src/thinking.ts:410-425`). A result that arrives after the turn was superseded, or after the user left `auto`, is dropped (`upstream:packages/coding-agent/src/session/model-controls.ts:715-716`).
- Precedence: `auto` is one value of the one selection. Picking a concrete level leaves `auto` (`upstream:packages/coding-agent/src/session/model-controls.ts:586-598`). Only user turns are classified; synthetic and tool-continuation turns are not (`upstream:packages/coding-agent/src/session/agent-session.ts:9645-9658`).
- It changes effort only. It never changes the session model.

### What omp2 has

- The session selection is the `ai_thinking` convar, default `high`, values `off` to `max`, no `auto` (`crates/agent/src/vars.rs:142-149`). The catalog has `ai_default_thinking` (legacy path `defaultThinkingLevel`) and a universal `ai_thinking_ceiling`, default `max` (`crates/catalog/src/settings.rs:1191-1206`).
- **How `ai_thinking` reaches the wire** (open question O2 of the first draft, now traced). The kernel builds each request with `reasoning: Setting::Unset` (`crates/agent/src/loop.rs:2343`), runs Directors' `before_inference` (`crates/agent/src/loop.rs:1592`) and `prepare_inference` (`crates/agent/src/loop.rs:1656`), then calls `chat` (`crates/agent/src/loop.rs:1682`). `ProductionInference::apply_convars` fills the reasoning **only when it is still `Unset`**, from the effective `ai_thinking`, through `convar_reasoning` (`crates/driver/src/headless/kernel.rs:342-348`). `convar_reasoning` is the policy clamp: no catalog spec or no thinking policy gives `Unset`; an effort the model's routing cannot resolve falls to the policy default (`crates/driver/src/headless/kernel.rs:492-530`). The router then clamps again by the configured ceiling (`crates/ai/src/router.rs:682-687`), and **fails the request** with `CapabilityMismatch` `thinking-policy-unavailable` when a request carries an effort but the model has no thinking policy (`crates/ai/src/router.rs:1168-1187`), or `thinking-selection-unsupported` when routing cannot resolve it (`crates/ai/src/router.rs:688-701`).
- **Binds.** A Director's binds are `bind/*` properties on its `<director>` element; the stack projects them into control-plane engagement layers on every derivation, so rewind and resume re-derive them (`crates/contract/src/director.rs:900-936`). Later layers take precedence (`crates/con/src/ctx.rs:1060`); `value_below_engagements` gives the value a bind shadows (`crates/con/src/ctx.rs:1169-1179`); `engagement_owner` names the layer that supplies a value (`crates/con/src/ctx.rs:1181-1188`). A bind whose property is `Null` is skipped (`crates/contract/src/director.rs:235-246`), so a Director can change or clear a bind with one journaled `Op::Set` (`crates/dom/src/op.rs:49-57`). The kernel derives binds before projecting a request (`crates/agent/src/loop.rs:1518-1521`) but not again after `before_inference`.
- The chat badge reads the effective `ai_thinking` (`crates/chat/src/host.rs:951-953`), as does RPC (`crates/app/src/rpc_mode.rs:1156`); `ModelWatch` watches `ai_thinking` and records whether a bind supplies it (`crates/agent/src/model_watch.rs:60-90`).
- **Isolated calls.** `execute_on(selector, request)` (`crates/contract/src/director.rs:612-624`) reaches `ProductionInference::chat_on`, which re-targets the shared client's call metadata, awaits `execute`, and only then restores it (`crates/driver/src/headless/kernel.rs:1822-1839`). If that future is dropped mid-await the restore never runs, and `apply_convars` re-targets only when `ai_model` names a model other than `self.model`, which `chat_on` never changed (`crates/driver/src/headless/kernel.rs:330-341`). The kernel drops a `before_inference` future on cancel and on any mailbox control message (steering, approval, pause), then loops (`crates/agent/src/loop.rs:1592-1624`). `chat_on` also gives an `Unset` reasoning the session's `ai_thinking` (`default_reasoning`, `crates/driver/src/headless/kernel.rs:424-436`, called at `crates/driver/src/headless/kernel.rs:1831`). Every existing isolated caller sends `Unset`: the advisor (`crates/agent/src/directors/advisor.rs:697`), memory reflection (`crates/driver/src/headless/reflection.rs:140`) and the speech rewriter's gateway path (`crates/driver/src/headless/kernel.rs:1957`). The `AuxiliaryInference` path builds a fresh route per call and has no such leak (`crates/driver/src/headless/kernel.rs:2036-2068`). Stacks without a catalog run `chat_on` on the live route (`crates/agent/src/loop.rs:122-133`).
- **Route facts** carry no thinking ladder (`crates/contract/src/director.rs:266-287`; built at `crates/driver/src/headless/kernel.rs:3554`). `identity` is `None` for gateways and headless tests (`crates/contract/src/director.rs:283-285`).
- **Usage.** A receipt's journaled role is `Primary | Advisor` only (`crates/journal/src/data.rs:630-636`). Compaction reads the latest non-advisor usage in a turn as context occupancy (`crates/agent/src/directors/compaction.rs:797-814`), so a judge receipt journaled as `Primary` would be read as the context size.
- `Slot` is `mode | loop | tool_choice | worktree` (`crates/contract/src/director.rs:190-199`); effort is not a slot. `Plan` and `Prewalk` write `ai_thinking` to the session layer on handoff (`crates/agent/src/directors/plan.rs:137`, `crates/agent/src/directors/prewalk.rs:60`). The kernel engages `CompactionDirector` itself when its flag is on and none is active or queued (`crates/agent/src/loop.rs:1419-1425`), and removes a disabled `goal` the same way (`crates/agent/src/loop.rs:1409-1418`).
- `ai_external_thinking` makes the driver drop provider reasoning altogether (`crates/driver/src/headless/kernel.rs:344-346`).

### Rules that bind

- ADR 0015: standing behaviour is a Director; binds live on its element. ADR 0012: settings are convars. ADR 0017: routes clamp what they cannot honour. ADR 0035: typed errors.
- Additive only: `ai_thinking`'s accepted values, the journal's `ReceiptRole` and existing importer mappings stay as they are; any change goes to the owner (O1, O5, O6).
- Deterministic rules first, a model only where rules cannot.
- Assignment: the Director chooses effort only and never switches the session model.

## Decision

**D1. A standing Director, `auto_effort`,** in `crates/agent/src/directors/auto_effort.rs`, registered in `register_standard` (`crates/agent/src/directors/mod.rs:40`). It claims no `Slot`, never owns a yield and returns `Pass` from every verdict hook.

**D2. Gate and engagement.** A new default-off convar `sv_auto_effort` (bool, flags `archive | session`, declared in `crates/agent/src/vars.rs` beside `ai_thinking`; no `project` flag, because a repository must not be able to turn on paid judge calls). At the start of every turn, beside the compaction check (`crates/agent/src/loop.rs:1419-1425`), the kernel MUST engage `auto_effort` when `sv_auto_effort` is on and none is active or queued, and MUST remove it with one `Op::Rm` when `sv_auto_effort` is off and one is present (the `goal` pattern, `crates/agent/src/loop.rs:1409-1418`). With no control plane composed the kernel never engages it. Turning the convar off and on again re-engages it with fresh state.

**D3. Inert conditions.** On each request the Director first checks, in this order, and when any holds it makes no judge call and clears its bind (sets `bind/ai_thinking` to `Null` if it is not already):
1. `ai_external_thinking` is on (the driver drops reasoning, `crates/driver/src/headless/kernel.rs:344-346`);
2. the route has no thinking ladder (`RouteFacts::thinking_efforts`, D8, is `None` or empty): gateways, headless tests and models without a controllable effort, as upstream (`upstream:packages/tui/src/thinking.ts:280-287`);
3. the Director is pinned (D4);
4. another active Director's element carries a non-`Null` `bind/ai_thinking` (D4).

**D4. Precedence**, highest first. Each rule is deterministic and tested.
1. **Another Director's `ai_thinking` bind** wins. Because engagement order decides layer order (`crates/con/src/ctx.rs:1060`) and the auto Director may sit inner, it does not rely on order: under D3.4 it clears its own bind, so the other bind supplies the value.
2. **A level chosen while engaged** wins and pins. State `base` records `value_below_engagements("ai_thinking")` (`crates/con/src/ctx.rs:1169`) whenever the Director writes its bind. If, on any later request, the value below engagements differs from `base`, a user, a cfg script or a `Plan`/`Prewalk` handoff (`crates/agent/src/directors/plan.rs:137`) chose a level: the Director sets `state/pinned = true`, clears its bind and stays inert until it is re-engaged (D2). This mirrors upstream leaving `auto` when a level is picked (`upstream:packages/coding-agent/src/session/model-controls.ts:586-598`). Writing the same value as `base` is not detected; the documented way to pin is `sv_auto_effort 0`.
3. **The auto choice** applies otherwise, through the bind.
4. **The value below engagements** (the user's own `ai_thinking`, default `high`) applies while the bind is `Null`: before the first classification, when inert, and when pinned.

**D5. When it classifies.** Classification happens in `before_inference` on the first request of a user turn only: when `state/turn` differs from `cx.turn` and the turn holds no assistant response yet. Every later request of the turn (tool continuations) re-runs only the D3/D4 checks, so the effort is constant inside a turn unless a pin lands. The judge call and every resulting write land in **one** `patch` after the judge returns; a future the kernel drops (`crates/agent/src/loop.rs:1592-1624`) journals nothing, and the next loop iteration classifies again. A journal replay re-derives the bind from the element and makes no judge call, because `state/turn` already names the turn.

**D6. Deterministic pre-pass**, on the text of the latest user message, trimmed (`t`), and the eligible ladder `L` = the route's `thinking_efforts` restricted to `low ..= min(ai_auto_thinking_ceiling, ai_thinking_ceiling)`:
- `L` is empty: inert for this turn (no bind change).
- `L` has one element: that element, no judge call (`upstream:packages/coding-agent/src/auto-thinking/classifier.ts:169-177`).
- `t` is empty, or `t` has no line break, at most **2** whitespace-separated words and at most **24** grapheme clusters (counted with `xutf`): **keep the previous auto effort** (the fallback F of D7 on the first turn), no judge call. Acknowledgements such as `ok`, `sí, dale` or `continue` keep the effort the work already had instead of dropping it to `low`.
- Otherwise: the judge (D7).
The two thresholds are named constants (`SHORT_PROMPT_MAX_WORDS = 2`, `SHORT_PROMPT_MAX_GRAPHEMES = 24`), not convars; S2 pins both boundaries.

**D7. The judge call.** One isolated request through `cx.inference.execute_on("@judge", request)`:
- The prompt names the ladder `L` only (so `max` appears only when the auto ceiling is `max` and the model lists it), carries `t` cut to 2000 grapheme clusters (`upstream:packages/coding-agent/src/tiny/message-preproc.ts:12`), and asks for exactly one label. The rubric lives in a prompt file under the agent's prompt directory, adapted from `upstream:packages/coding-agent/src/prompts/judge/auto-thinking.json`.
- The request sets its **own** effort: `reasoning` is an explicit `off` effort, never `Unset`, so it never inherits `ai_thinking` (S0b makes the driver resolve it for the judge model). Temperature 0, `max_output_tokens` 4096, no tools (`upstream:packages/ai/src/judgment/chat.ts:28`, `upstream:packages/ai/src/judgment/chat.ts:76-80`).
- Parse: trim, lowercase, take the first whitespace-delimited token, strip surrounding punctuation; it MUST equal a label in `L`. Anything else is unparseable.
- A 4 s bound (`tokio::time::timeout`) around opening the stream and reading it to the end.
- On timeout, error, `TargetNotFound`, or an unparseable or out-of-ladder answer: the effort is F = the previous auto effort, or on the first turn the value below engagements clamped into `L` (greatest element at or below it, else the lowest element of `L`). A debug notice is journaled in the same patch. The turn never fails.
- Judge usage is recorded in Director state (`state/judge_tokens_in`, `state/judge_tokens_out`, `state/judge_cost_nano_usd`, cumulative), **not** as a `TurnReceipt`, because the journal has no judge receipt role and a `Primary` receipt would be read as context occupancy (`crates/journal/src/data.rs:630-636`, `crates/agent/src/directors/compaction.rs:797-814`). See O5.

**D8. How the choice is applied: a bind, resolved by the same clamp as a user choice.** The Director writes the chosen effort name to its own `bind/ai_thinking` property (one `Op::Set` in the D5 patch) and returns `Prepared::Rebuild` when the bind changed. The kernel MUST re-derive binds after the preflight, before `prepare_inference` and `chat` (one `apply_binds` call after `crates/agent/src/loop.rs:1624`; a no-op when nothing changed, `crates/con/src/ctx.rs:1123-1137`). The effective `ai_thinking` then enters the request at `apply_convars` exactly as a user-set value does: through `convar_reasoning` and the router's clamp (`crates/driver/src/headless/kernel.rs:342-348`, `crates/driver/src/headless/kernel.rs:492-530`, `crates/ai/src/router.rs:682-687`). The Director MUST NOT set `ChatRequest::reasoning` on the main request and has no `prepare_inference`. A model without a thinking policy therefore gets `Unset`, never an effort the router rejects. The badge and RPC show the concrete effort because they read the effective value; `ModelWatch` reports the change as Director-supplied.
- New field `RouteFacts::thinking_efforts: Option<Arc<[ThinkingEffort]>>`, filled by the driver's `route_facts` from the model's thinking policy (`crates/driver/src/headless/kernel.rs:3554`), `None` without a catalog route. It is read for D3 and D6 only; the clamp stays in the driver and router.

**D9. Effort only, enforced.** The Director never writes `ai_model`, never reads or writes a model role, never writes any session-layer convar, never sets `ChatRequest::reasoning` on the main request, and its only bind is `ai_thinking`. Its only auxiliary model is `@judge`, on the isolated path. A test fails if the Director's element carries any `bind/*` other than `bind/ai_thinking`, if any of its `DirectorEffect`s carries a write, or if the main request's target model differs between a session with `sv_auto_effort` on and off.

**D10. The ceiling.** A new convar `ai_auto_thinking_ceiling` (`ThinkingEffort`, accepted `xhigh | max`, default `xhigh`, flags `archive`, `legacy.path` `providers.autoThinkingMaxEffort`), declared in `crates/catalog/src/settings.rs` beside `ai_thinking_ceiling`. It caps only the Director's choice; the global `ai_thinking_ceiling` still applies downstream. It does not inherit the global default `max`.

## Rejected options

| Option | Why not |
|---|---|
| Set `req.reasoning` in `prepare_inference` (the first draft's D7) | Skips `convar_reasoning`, because `apply_convars` fills only `Unset` (`crates/driver/src/headless/kernel.rs:346`); on a model without a thinking policy the router fails the turn (`crates/ai/src/router.rs:1179-1185`). The Director has no catalog to clamp itself (`crates/contract/src/director.rs:266-287`). |
| Session-layer write of `ai_thinking` from a `DirectorEffect` | Overwrites the user's own value and commits at a yield or settled-turn boundary, not before the request. |
| A new `Slot::Effort` claimed by effort owners | `Plan` and `Prewalk` write rather than bind, so they would never claim it; a claim conflict queues the Director instead of leaving it inert; and it changes an existing enum. D3.4 and D4 give the same precedence additively. |
| A router-side rule from request size or tool count | Cannot read what the prompt asks; puts policy in a layer that only clamps. Kept as the D6 pre-pass, which never lowers effort on its own. |
| Short prompts map to `low` (the first draft's D3) | Upstream has no such rule (`upstream:packages/coding-agent/src/auto-thinking/classifier.ts:147-197`); "fix it" is short and may be hard. Keeping the previous effort costs nothing and never under-thinks a continuation. |
| Port the TypeScript classifier into a session layer | omp2 runs turns in the agent kernel; a second place that sets effort would race Directors and `ModelWatch`. |
| Switch to a stronger model for hard prompts | Out of scope by the assignment; breaks the cached prefix and route identity. |

## Slices

Each slice is one PR; tests are written first and not edited to pass. Verification: `just test-pkg <crate>` for the named crates, then `just lint`.

**S0a. Drop-safe isolated calls** (`crates/driver`). `ProductionInference::chat_on` restores the live call metadata on every exit, including when its future is dropped mid-await (a restore guard owned by the future).
- Tests (`crates/driver`, fake transport that never answers): `dropped_chat_on_restores_the_live_target` (drop the `chat_on` future after it re-targets; the next `chat` goes to the live model), `timed_out_chat_on_restores_the_live_target` (same through `tokio::time::timeout`), `completed_chat_on_still_restores` (regression for today's path).
- Acceptance: the advisor tests (`crates/agent/tests/advisor.rs`) pass unchanged.
- Gate: none; it fixes an existing leak and changes nothing on a path that completes. See O7.
- Depends on: nothing.

**S0b. Isolated calls resolve an explicit effort through the policy clamp** (`crates/driver`). In `chat_on` (both `ProductionInference` and `AuxiliaryInference`), a request whose `reasoning` names an effort is re-resolved for the target model through `convar_reasoning`; when that gives no selection, the model's lowest listed effort; a model without a thinking policy gets `Unset`. An `Unset` request keeps today's `default_reasoning`.
- Tests: `explicit_off_on_isolated_call_ignores_ai_thinking` (`ai_thinking xhigh`, request `off` → wire carries `off`), `explicit_effort_on_policyless_model_is_unset` (no `thinking-policy-unavailable` error), `explicit_off_on_reasoning_required_model_takes_its_lowest_effort`, `unset_isolated_call_still_inherits_ai_thinking` (regression for the advisor).
- Gate: none; no existing caller sends an explicit effort (Context: advisor, reflection, speech rewriter).
- Depends on: nothing.

**S1. Settings, no behaviour.** `sv_auto_effort` and `ai_auto_thinking_ceiling` (D2, D10); nothing reads them yet.
- Tests: `auto_effort_defaults_off`, `auto_ceiling_parses_xhigh_and_max_and_rejects_others`, `auto_ceiling_defaults_to_xhigh_not_max`, `legacy_path_maps_providers_auto_thinking_max_effort`, `sv_auto_effort_rejects_project_scope`.
- Gate: `sv_auto_effort` default off.

**S2. The Director, engagement, precedence and pre-pass; no judge yet** (`crates/agent`, `crates/contract`, `crates/driver`). D1-D6, D8 and D9 with the judge branch returning F. Adds `RouteFacts::thinking_efforts` and the driver fill, and the kernel's `apply_binds` after the preflight.
- Tests (`crates/agent/tests/directors/auto_effort.rs`, fake `ErasedInference` that counts calls):
  - engagement: `engaged_when_gate_on_and_absent`, `removed_when_gate_off`, `reengagement_resets_pinned`, `never_engaged_without_con`;
  - inert: `inert_under_external_thinking`, `inert_without_thinking_ladder`, `yields_to_another_directors_thinking_bind`;
  - precedence: `user_level_set_while_engaged_pins_and_clears_bind`, `plan_handoff_effort_pins`, `same_value_write_is_not_a_pin` (documents the limit);
  - pre-pass boundaries: `single_tier_ladder_needs_no_judge`, `empty_prompt_keeps_previous_effort`, `two_words_keep_previous_effort` (`yes please`), `three_words_go_to_the_judge` (`fix the bug`), `twenty_five_graphemes_go_to_the_judge`, `line_break_goes_to_the_judge`;
  - turn shape: `tool_continuation_never_reclassifies`, `effort_is_constant_inside_a_turn`, `dropped_preflight_journals_nothing_and_reclassifies`;
  - invariant: `only_bind_is_ai_thinking`, `no_effect_carries_a_write`, `main_request_reasoning_is_never_set_by_the_director`, `director_claims_no_slot`.
- Driver test (`crates/driver`): `bound_effort_on_policyless_model_sends_no_reasoning` (the D8 clamp, the round-1 finding), `bound_effort_reaches_the_wire_through_convar_reasoning`, `bind_written_in_preflight_applies_to_the_same_request`.
- Acceptance: gate off, every existing Director and kernel test passes unchanged; the extra `apply_binds` is a no-op for them.
- Gate: `sv_auto_effort`. Depends on: S1.

**S3. The `@judge` call** (`crates/agent`). D7: rubric prompt file, one isolated request, explicit `off` effort, parse, 4 s bound, fallback F, usage in state.
- Tests (fake judge): `each_label_maps_to_its_effort`, `ladder_offers_max_only_when_ceiling_is_max_and_model_lists_it`, `label_outside_ladder_falls_back`, `unparseable_answer_falls_back`, `timeout_falls_back_without_failing_the_turn`, `target_not_found_falls_back`, `first_turn_fallback_clamps_user_value_into_ladder`, `judge_request_carries_explicit_off_effort`, `judge_input_is_cut_to_2000_graphemes`, `replay_gives_same_effort_with_no_judge_call`, `judge_usage_is_recorded_in_state_not_as_a_receipt`.
- Driver test: `judge_timeout_leaves_main_route_on_session_model` (needs S0a).
- Prompt golden: the rendered judge prompt in `crates/agent/tests/prompt_golden.rs`.
- Gate: `sv_auto_effort`. Depends on: S0a, S0b, S2.

**S4. Surfaces and joined-system proof.**
- Tests: `badge_shows_concrete_auto_effort` (`crates/chat`, host.rs:953 path), `rpc_state_reports_concrete_auto_effort` (`crates/app`, rpc_mode.rs:1156 path), `model_watch_reports_auto_effort_as_director_supplied` (`crates/agent/tests/model_watch.rs`).
- E2E: a case in the P9 proof (`crates/e2e/tests/p9_extension_control.rs`, Director control) that runs two user turns with a scripted judge, asserts the provider saw the classified effort on the first request of each turn and the same effort on its tool continuations, kills nothing; and a P6 case (`crates/e2e/tests/p6_crash_resume.rs`) that resumes mid-turn and asserts no second judge call.
- Golden journal: one new corpus family `auto_effort` (bind change plus state patch) in `crates/session/tests/golden_journals.rs`, so the fold of a bind `Op::Set` stays pinned.
- Gate: `sv_auto_effort`. Depends on: S3.

Estimate: 4 to 6 days in total. [INFERENCE]

## Upstream origin and license

A port draws from `upstream:packages/coding-agent/src/auto-thinking/classifier.ts`, the rubric `upstream:packages/coding-agent/src/prompts/judge/auto-thinking.json`, the clamp and provisional rules in `upstream:packages/tui/src/thinking.ts`, and the judge request settings in `upstream:packages/ai/src/judgment/chat.ts`. TypeScript, rewritten in Rust. Upstream's `LICENSE` is MIT (copyright Mario Zechner, Can Bölük, Stencil Labs, Inc.; `upstream:LICENSE:1-5`). Keep the notice for any copied rubric text. No third-party code.

## Consequences

- With `sv_auto_effort` on, a user turn whose prompt passes the pre-pass costs one short judge call and up to 4 s before the first request.
- Effort can change between turns, which may break a provider's prefix cache on some routes; it never changes inside a turn except when a pin lands.
- Isolated calls that inherit `ai_thinking` (the advisor, reflection) follow the auto effort while it is bound. That is the effective value by design; it is noted here because it is new.
- `@judge` falls back to the slow selection (`crates/catalog/src/selection.rs:465`), which may be expensive.
- Judge spend is visible in Director state but not in session usage totals until O5 is decided.
- S0a and S0b touch existing driver code ungated; both are invisible on every path that works today (O7).

## Open questions for the owner

- **O1. Is `auto` an accepted value of `ai_thinking`, or a separate switch?** Recommendation: the separate convar `sv_auto_effort` (D2), so an existing convar's values do not change.
- **O2.** Closed: traced in Context; settled by D8.
- **O3. Should a child of an `auto` parent classify, or inherit?** A child seeds from the parent's effective values (ADR 0013), so today it starts at the parent's current auto effort. Recommendation: keep that and never engage `auto_effort` in a child unless the child's own cfg turns it on; upstream's `solutionSpace` input is not ported in this record.
- **O4. A dedicated cheap judge role?** Recommendation: reuse `@judge`; add a role only if cost shows up.
- **O5. A judge receipt role.** Adding `ReceiptRole::Judge` (`crates/journal/src/data.rs:630-636`) would put judge spend in usage totals, but it changes a journaled enum. Recommendation: owner decides; D7 keeps usage in state meanwhile.
- **O6. Import `defaultThinkingLevel: auto`.** Mapping it to `sv_auto_effort 1` changes how an existing legacy key imports (`crates/catalog/src/settings.rs:1199`). Not designed here; recommendation: a follow-up importer change once O1 is decided.
- **O7. Ungated fixes to existing code.** S0a and S0b change `ProductionInference::chat_on` without a gate. Recommendation: accept; a drop guard behind a convar would leave the advisor's existing leak in place.
- **O8.** `ultrathink` and `chillax` keyword overrides: out of scope, a separate decision.

## What the review changed

### Round 1 (Goldfish, 2026-11-13; resolved 2026-11-14)

Five findings, recorded in `docs/adr/STATUS.md` row 0064. Each was checked against this tree and the upstream pin.

- **D7 bypasses the policy clamp, so a turn can fail.** Confirmed: `apply_convars` resolves reasoning only when it is `Unset` (`crates/driver/src/headless/kernel.rs:346`), so an effort set in `prepare_inference` (`crates/agent/src/loop.rs:1656`) skips `convar_reasoning` (`crates/driver/src/headless/kernel.rs:497-523`); the router then fails a model without a thinking policy (`crates/ai/src/router.rs:1179-1185`); upstream guards the same case (`upstream:packages/tui/src/thinking.ts:283-287`). Changed: D8 applies the choice as an `ai_thinking` bind resolved by the same path as a user value; the Director never sets request reasoning; kernel re-derives binds after the preflight; S2 driver tests.
- **A `chat_on` drop leaves the client on the `@judge` model.** Confirmed: the restore follows the await (`crates/driver/src/headless/kernel.rs:1832-1837`), `apply_convars` does not re-target an unchanged `ai_model` (`crates/driver/src/headless/kernel.rs:330-341`), and the kernel drops preflight futures on cancel and control messages (`crates/agent/src/loop.rs:1592-1624`); D4's own timeout would drop it too. It affects the advisor today. Changed: new slice S0a (drop-safe restore) before the judge slice; D5 journals in one patch after the call; S3 test `judge_timeout_leaves_main_route_on_session_model`; O7.
- **The judge inherits `ai_thinking`.** Confirmed: `default_reasoning` fills an `Unset` isolated request from `ai_thinking` (`crates/driver/src/headless/kernel.rs:424-436`, `crates/driver/src/headless/kernel.rs:1831`); upstream disables judge reasoning (`upstream:packages/ai/src/judgment/chat.ts:42`, `upstream:packages/ai/src/judgment/chat.ts:79`). Under D8 it would even inherit the auto choice. Changed: D7 sets an explicit `off` effort; S0b makes isolated calls resolve an explicit effort through the policy clamp; S3 test `judge_request_carries_explicit_off_effort`.
- **Engagement gate and precedence were unspecified.** Confirmed: the first draft said only "registered in `register_standard`" and the note said "like compaction" (`crates/agent/src/loop.rs:1419-1425`). One part rejected: the finding speaks of other Directors "claiming the slot", but effort is not a `Slot` (`crates/contract/src/director.rs:190-199`); effort owners are binds, ranked by layer order (`crates/con/src/ctx.rs:1060`), and session writes from `Plan`/`Prewalk` (`crates/agent/src/directors/plan.rs:137`). Changed: D2 (kernel engage and remove), D3 (inert conditions), D4 (four-level precedence with pinning), rejected option `Slot::Effort`; S2 engagement, inert and precedence tests.
- **The S2 thresholds were unspecified.** Confirmed: the first draft's D3 and test `short_prompt_resolves_to_low_without_a_judge_call` named no number. Changed: D6 fixes `SHORT_PROMPT_MAX_WORDS = 2` and `SHORT_PROMPT_MAX_GRAPHEMES = 24` and, since upstream has no such rule (`upstream:packages/coding-agent/src/auto-thinking/classifier.ts:147-197`), short prompts now keep the previous effort instead of dropping to `low`; S2 boundary tests on both sides of each threshold.
- **Corrected against this tree while revising** (not round-1 findings): the first draft's test `judge_usage_is_journaled_as_judge` assumed a judge receipt role that does not exist (`crates/journal/src/data.rs:630-636`), and a `Primary` receipt would skew compaction (`crates/agent/src/directors/compaction.rs:797-814`): D7 keeps usage in state, O5. The first draft's "binds look fixed at node build time" was wrong: binds are re-derived from the element (`crates/contract/src/director.rs:900-936`). Area changed from `runtime` to `control-plane` (ADR README areas).

### Rounds 2-3

Pending: fresh-context Goldfish reviewers have not yet read this revision. Not machine-reviewed, not owner-approved.

## Acceptance tests

1. (S0a) A dropped or timed-out isolated call leaves the next main request on the session model.
2. (S0b) An isolated call with an explicit effort carries that effort resolved for its own model, never `ai_thinking`, and never fails on a model without a thinking policy.
3. (S1) `sv_auto_effort` defaults off; `ai_auto_thinking_ceiling` defaults `xhigh` and imports from `providers.autoThinkingMaxEffort`.
4. (S2) With the gate off, nothing changes. With it on, the bound effort reaches the wire only through `convar_reasoning`; a model without a thinking policy gets no reasoning; precedence and pre-pass boundaries hold as D4 and D6 state.
5. (S3) Every judge failure falls back to F without failing the turn; the judge request carries `off`; replay makes no judge call.
6. (S4) Badge, RPC and `ModelWatch` show a concrete effort, never `auto`; P9 and P6 cases pass; the golden journal family folds.

## Status in omp

**Status: Not started.** Reusable today: the Director trait and registry (`crates/contract/src/director.rs:630`), bind derivation (`crates/contract/src/director.rs:900-936`), `execute_on` (`crates/contract/src/director.rs:619`), the `@judge` role (`crates/catalog/src/selection.rs:175-178`), `convar_reasoning` (`crates/driver/src/headless/kernel.rs:492`), `clamp_thinking_effort` (`crates/catalog/src/thinking.rs:320`), `ModelWatch` (`crates/agent/src/model_watch.rs`). UNVERIFIED: how a remote gateway resolves an explicit effort (D3.2 keeps the Director inert there).

## References

[docs/research/auto-reasoning-effort.md](../research/auto-reasoning-effort.md); ADR 0012 (convars); ADR 0013 (seeding); ADR 0015 (Directors); ADR 0017 (clamping); ADR 0035 (errors); ADR 0053 (the judge role). Checked for collision: ADR 0006, 0024, 0036 (none: no tool, sandbox or extension-runtime change); ADR 0049 (no vendor code, core stays vendor-neutral).
