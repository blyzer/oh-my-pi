# 0016. Directors state intent; inference chooses how to satisfy it

Status: accepted
Date: 2026-09-02
Area: control-plane

## Context

`ForceTool` (0015) expresses one semantic request: "the next successful turn must call `write`."
As written in the playbook it is a Director with `prepare_inference` returning
`request.with_tool_choice(self.tool)` and an `on_yield` that pops on success, continues with a
reminder while retries remain, and fails when they are exhausted.

It does not know whether the selected provider has a native `tool_choice`, whether forcing
destroys the prompt cache on this route, or whether a local model needs an extra prompt to comply.
When omp v1 and pi let the caller carry that knowledge, provider names leaked into control-plane
code and every behavior that wanted to force a tool re-learned the same quirks (0017).

## Decision

The control plane MUST state semantic intent only: force this capability, enforce this output
shape, count these tokens, require a yield. It NEVER names a provider feature, a wire field, a
cache strategy, or a model-specific prompt.

The inference layer MUST own the translation: capability lookup, cost (cache penalties, extra
requests), and escalation (soft prompt, free native flag, costly native flag — 0019). It returns
the same canonical outcome regardless of which strategy satisfied the request, so a Director's
`until` predicate is evaluated on the turn, not on how the turn was obtained.

A request that inference cannot satisfy on the current route is reported to the Director as a
failure of the request, never silently downgraded.

## Consequences

- A Director written once works across incompatible models and providers.
- Provider knowledge stays compiled in one place (0017, 0018); control-plane code has no
  provider branches to review.
- Prohibited: `if provider == …` in Directors or extensions; passing raw `tool_choice` or grammar
  dialects up from the control plane.
- Cost accepted: inference carries an escalation ladder and its bookkeeping; a Director cannot
  micro-manage cost, only state intent and retry budget.

## Status in omp

**Status: Implemented.** Directors state a forced-call intent and the inference layer picks the mechanism. (Verified 2026-10-04 against `omp2` at `083b38fe7d`.)

- `crates/agent/src/directors/force_tool.rs` sets only `Setting::Require`/`Prefer(ToolChoice::Named(..))` and the slot claim; it returns `Verdict::Fail` when the request cannot be satisfied.
- Translation lives in `crates/ai/src/plan.rs` (`forced_call_ladder`) and `crates/ai/src/provider/builtin.rs` (`forced_call_operation`), driven by catalog capability bits (0019).

## References

- The Harness Playbook, "The control plane" → "Hooks, Directors, and inference" (closing
  paragraph); "The inference" opening lesson
- 0015 (Directors), 0017 (compatibility as structured knowledge), 0018 (provider infrastructure),
  0019 (forced-call escalation), 0021 (constrained sampling ownership)
- `crates/agent/src/directors/force_tool.rs` (the semantic force intent), `crates/ai/src/call.rs`,
  `crates/ai/src/plan.rs`, `crates/ai/src/codec/openai_chat.rs`. The former
  `crates/agent/src/regime.rs` and `crates/agent/src/tool_choice.rs` were removed by `d98ed242f5`;
  the `tool_choice` claim is now the Director `Slot::ToolChoice` in `crates/agent/src/director.rs`
