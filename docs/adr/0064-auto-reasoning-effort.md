# 0064. Automatic reasoning effort is a Director that chooses effort only and never switches models

Status: proposed
Date: 2026-11-13
Area: runtime

Research: [docs/research/auto-reasoning-effort.md](../research/auto-reasoning-effort.md) (Gap 5 of the upstream gap closure). This record states decisions; the note holds the evidence.

## Context

### What OMPv1 does

- `defaultThinkingLevel` accepts every effort plus `auto`; the default is `high` (`upstream:packages/coding-agent/src/session/settings.ts:242`). `providers.autoThinkingMaxEffort` is `xhigh` or `max`, default `xhigh`, and raises the ceiling the classifier may reach (`upstream:packages/coding-agent/src/session/settings.ts:1156`). The effective ceiling is `max` only when the setting says so and the model lists that tier (`upstream:packages/coding-agent/src/auto-thinking/classifier.ts:135-138`).
- A per-prompt classifier asks a judge, resolved from the `judge` role, one multiple-choice question on a ladder of `low`, `medium`, `high`, `xhigh`, `max`. A very small local judge gets a coarser three-way question mapped to `low`, `high`, `xhigh` (`upstream:packages/coding-agent/src/auto-thinking/classifier.ts:9-12`, `upstream:packages/coding-agent/src/auto-thinking/classifier.ts:40-44`). It skips the judge when only one tier exists (`upstream:packages/coding-agent/src/auto-thinking/classifier.ts:169-177`). The result is clamped into the model's supported range, never below `low` and never above the ceiling (`upstream:packages/tui/src/thinking.ts:289`).
- It runs once per user turn, before the request, with a 4 s timeout, and never throws into the turn; on failure it keeps the last level, or a provisional one capped at `xhigh` before the first resolution (`upstream:packages/coding-agent/src/session/model-controls.ts:657`, `upstream:packages/coding-agent/src/session/model-controls.ts:667`, `upstream:packages/tui/src/thinking.ts:421`). Synthetic and tool-continuation turns are not classified (`upstream:packages/coding-agent/src/session/agent-session.ts:9651`).
- It changes effort only. It never changes the model.

### What omp2 has

- The session selection is the `ai_thinking` convar with `off` through `max`; it has no `auto` value (`crates/agent/src/vars.rs:145`). The catalog has `ai_default_thinking` and a universal `ai_thinking_ceiling` that defaults to `Max` (`crates/catalog/src/settings.rs:1191`, `crates/catalog/src/settings.rs:1203`).
- The router clamps every request: `clamp_thinking_effort` bounds the requested effort by a configured ceiling and by the model's own (`crates/catalog/src/thinking.rs:320`, `crates/ai/src/router.rs:682-687`). It never chooses an effort.
- Directors are the mechanism for standing behaviour. The trait has `binds`, `before_inference` (a cold async call with an isolated inference service), `prepare_inference` and `observe_turn` (`crates/contract/src/director.rs:630-690`). `execute_on(selector, request)` runs an isolated request on any model or `@role` without touching the live route (`crates/contract/src/director.rs:612-619`), and the advisor already uses it (`crates/agent/src/directors/advisor.rs:344`). A `@judge` role exists and falls back to the slow selection (`crates/catalog/src/selection.rs:175-178`, `crates/catalog/src/selection.rs:465`).
- `Prewalk` and `Plan` write `ai_model` and `ai_thinking` on handoff (`crates/agent/src/directors/prewalk.rs:60`, `crates/agent/src/directors/plan.rs:137`). `ModelWatch` already treats a Director-supplied `ai_thinking` as a known case (`crates/agent/src/model_watch.rs:88`).

### Rules that bind

- ADR 0015: Directors own candidate yields and standing behaviours. ADR 0012: settings are convars. ADR 0017: routes clamp what they cannot honour.
- Additive only: `ai_thinking` is an existing convar; changing its accepted values needs the owner (O1).
- Deterministic rules first, a model only where rules cannot.
- Assignment constraint: the Director chooses effort only and never switches the model mid-session.

## Decision

**D1. A standing Director, `auto_effort`,** in `crates/agent/src/directors/auto_effort.rs`, registered in `register_standard`. It claims no slot, never owns a yield and only ever returns `Pass`.

**D2. Once per user turn.** It acts on the first request of a user turn only; tool-continuation requests return unchanged. The effort inside a turn is constant, which protects the provider's cached prefix. The last classified turn id lives in Director state, so a journal replay yields the same effort with no judge call.

**D3. Deterministic first.** Empty or very short prompts and bare slash commands map to `low` without a model call. Otherwise one isolated request through `execute_on("@judge", ...)` asks for one label of `low | medium | high | xhigh | max`, with the rubric in a prompt file. `max` is offered only when the auto ceiling is `max` and the model lists it. The first slice always asks the five-way question.

**D4. Failure is quiet.** A 4 s timeout; on timeout, error or an unparseable answer the Director keeps the previous auto choice, or a provisional effort on the first turn, and journals a debug notice. It never fails the turn.

**D5. Effort only, enforced.** The Director never writes `ai_model`, never reads or writes a model role, and never uses `execute_on` for the main route. Its only auxiliary model is `@judge`, on the isolated path, with tokens journaled as judge usage. A test fails if any effect of this Director names a write other than the effort.

**D6. Settings, additive.** A new default-off convar `sv_auto_effort` turns the Director on. A new convar `ai_auto_thinking_ceiling` (`xhigh | max`, default `xhigh`, legacy path `providers.autoThinkingMaxEffort`) caps the Director's choice and is itself capped by `ai_thinking_ceiling`. The existing `ai_thinking` convar keeps its values; see O1 for `auto` as a value.

**D7. How the choice is applied.** Store it in Director state and apply it by editing the request's reasoning field in `prepare_inference` on every request of the turn, publishing it through `ModelWatch` for the badge and RPC. Slice 1 must first trace how `ai_thinking` reaches the router and confirm this (O2).

## Rejected options

- **A router-side rule from request size or tool count.** Deterministic, but it cannot read what the prompt asks, and it puts policy in a layer that today only clamps. Kept as the heuristic first pass inside D3, not as the answer.
- **Port the TypeScript classifier into a session layer.** omp2 runs turns in the agent kernel; a second place that sets effort would race Directors and `ModelWatch`.
- **Switch to a stronger model for hard prompts.** Out of scope by the assignment, and it breaks the cached prefix and route identity.
- **Write `ai_thinking` from the Director's effect.** It would overwrite the user's `auto` selection, and effects commit at a yield or settled-turn boundary, not before the request.

## Slices

Each slice is one PR; tests are written first and not edited to pass.

**S1. Settings, no behaviour.** `sv_auto_effort` and `ai_auto_thinking_ceiling`; nothing reads them yet.
- Tests: `ceiling_parses_xhigh_and_max`, `auto_ceiling_is_capped_by_global_ceiling`, `auto_ceiling_defaults_to_xhigh_not_max`, `legacy_path_maps_providers_auto_thinking_max_effort`.
- Gate: `sv_auto_effort` default off.

**S2. The Director with the heuristic pass.**
- Tests, fake `ErasedInference`: `short_prompt_resolves_to_low_without_a_judge_call`, `tool_continuation_request_never_reclassifies`, `effort_is_constant_inside_a_turn`, `effect_never_contains_an_ai_model_write` (fails if any write name other than the effort appears; compare `crates/agent/tests/directors/plan.rs:89`), `director_claims_no_slot`.
- Gate: `sv_auto_effort`.

**S3. The `@judge` call.** Rubric prompt file; one isolated request; 4 s timeout; journaled usage.
- Tests: `each_label_maps_to_its_effort`, `max_accepted_only_when_ceiling_is_max_and_model_lists_it`, `unsupported_label_keeps_previous_level`, `timeout_falls_back_without_failing_the_turn`, `unparseable_answer_falls_back`, `replay_gives_same_effort_with_no_judge_call`, `judge_usage_is_journaled_as_judge`.

**S4. Surfaces.** The chat badge and RPC show the resolved effort, never the string `auto`.
- Tests: a session with `auto` reports a concrete effort at `crates/chat/src/host.rs:953` and `crates/app/src/rpc_mode.rs:1156`.

Estimate: 3 to 5 days in total. [INFERENCE]

## Upstream origin and license

A port draws from `upstream:packages/coding-agent/src/auto-thinking/classifier.ts`, the rubric `upstream:packages/coding-agent/src/prompts/judge/auto-thinking.json` and the clamp functions in `upstream:packages/tui/src/thinking.ts`. TypeScript, rewritten in Rust. Upstream's `LICENSE` is MIT (copyright Mario Zechner, Can Bölük, Stencil Labs, Inc.; `upstream:LICENSE:1-5`). Keep the notice for any copied rubric text. No third-party code.

## Consequences

- With `sv_auto_effort` on, each user turn may cost one short judge call and up to 4 s; the heuristic pass and the single-tier shortcut limit it.
- Effort can change between turns, which may break a provider's prefix cache on some routes. It never changes inside a turn.
- The `@judge` fallback to the slow selection can be expensive; users may want a cheap judge role.

## Open questions for the owner

- **O1.** Is `auto` an accepted value of `ai_thinking`, or a separate switch? Recommendation: the separate convar `sv_auto_effort`, so an existing convar's values do not change; map `defaultThinkingLevel: auto` to it in the legacy importer.
- **O2.** How does `ai_thinking` reach the router? Not traced. Recommendation: trace it in S1 and settle bind versus request edit there; the invariant in D5 holds either way.
- **O3.** Should a child of an `auto` parent classify, or inherit the delegator's `solutionSpace`? Recommendation: inherit the parent's current effort at spawn and never classify; revisit if children run long.
- **O4.** A dedicated cheap judge role? Recommendation: reuse `@judge` first; add a role only if cost shows up.
- **O5.** `ultrathink` and `chillax`-style keyword overrides. Recommendation: out of scope; a separate decision.

## Status in omp

**Status: Not started.** Reusable today: the Director trait and registry, `execute_on`, the `@judge` role, `clamp_thinking_effort`, `ModelWatch`.

## References

[docs/research/auto-reasoning-effort.md](../research/auto-reasoning-effort.md); ADR 0015 (Directors); ADR 0012 (convars); ADR 0017 (clamping); ADR 0053 (the judge role).

