# 0024. Every permanent tool taxes every turn; the roster stays small and fixed

Status: accepted
Date: 2026-09-02
Area: tools

## Context

A user reported that omp was slower than Codex on the same task — not in tokens, in wall-clock.
Measured (task `sol`, median of 6 fresh-session runs, codex-cli 0.144 and pi as external
references on the same prompt), it was true, and by almost 2×.

The culprit was the tool roster, not the prompt. Cutting omp to five essential tools brought the
median wall-clock to 36.6s, ahead of Codex (42.2s) and pi (37.0s). The mechanism: a tool schema is
not just description text charged as prefix tokens. With most frontier providers the tool grammar
participates in generation — the sampler is steered toward valid JSON for every declared tool on
every turn — so each additional permanent tool slows every response whether or not it is called.

Two existing answers both fail this measurement:

- **Dynamic tool discovery** (pi's `loadMode`, MCP-style late binding) keeps the grammar small
  until a tool is needed, then mutates the roster. Every roster change is a change to the request
  prefix, which invalidates the prompt cache for the rest of the session.
- **Permanent MCP tools** put every server's operation set into the grammar. pi's judgement that
  MCPs are badly designed and do not belong in the permanent layer is shared here; the user who
  wants a Figma MCP still needs a way to reach it.

The target that satisfies both the inference constraint and the user is a stable, tiny grammar with
a long tail reachable through ordinary composition (0025).

## Decision

1. The permanent, model-facing tool roster MUST be small and fixed for the life of a session.
   Membership is decided at session composition, not by the model or by discovery.
2. The roster NEVER changes mid-session. Adding, removing, or reshaping a schema after the first
   request is prohibited, because it invalidates the cached prefix.
3. A tool earns a roster slot only when it is used on most turns of most sessions, or when its
   argument shape must be sampled under a schema (0021). "The model might need it" is not a
   reason.
4. MCP servers, extension-provided operations, and rarely used harness capabilities NEVER enter the
   permanent roster. They ride the stable surfaces of 0025 (`dyn`, code surfaces) whose schemas are
   already in the roster.
5. Roster composition is a measured decision: the wall-clock benchmark above is the acceptance
   test, and a proposal that adds a permanent tool MUST show the per-turn cost is paid for.

## Consequences

- Prompt-cache hit rate is a property of the session, not of the model's browsing behaviour; the
  request prefix is identical from turn one to turn N.
- Optional capability is unlimited in count and free at rest: a thousand `dyn` devices cost the
  grammar nothing (0025).
- Prohibited: pi-style discoverable tools, per-turn schema mutation, and "load on demand" of any
  kind that touches the wire roster.
- Cost accepted: reaching a long-tail operation is one hop further (list, `--help`, invoke) than a
  direct tool call would be, and the harness must synthesize good CLI ergonomics from schemas so
  that hop is cheap.

## Status in omp

**Status: Partially implemented.** The native roster is a fixed identity set with the long tail behind `dyn`, but the advertised roster is recomputed per request and does change mid-session. (Verified 2026-10-04 against `omp2` at `083b38fe7d`.)

- Fixed identity set: `builtin_tool_identities()` in `crates/tools/src/lib.rs` (32 families, 6 hidden); MCP, media and security devices ride `dyn` (0025).
- Divergence from rule 2: `ProjectedRequest` assembly in `crates/agent/src/loop.rs` (around line 2213) re-derives the tool list on every request: it applies the `sv_tools` allowlist (so Plan/Vibe Director binds restrict it mid-session), mounts the hidden `goal` tool while a Goal Director is active, adds `think` when `AI_EXTERNAL_THINKING` is set, and withholds `task` at the recursion ceiling. Each change alters the tool prefix.
- No test pins roster stability; `crates/e2e/tests/p5_prefix_stability.rs` covers prompt bands only. Owner decision: accept Director-driven roster changes (and say so in the ADR) or freeze the roster at composition.
- Not verified: the 'wall-clock benchmark' acceptance test named in the ADR.

## References

- The Harness Playbook, "The tool surface" — "Every schema has a tax"
- 0025 (where the long tail goes), 0021 (constrained sampling budgets), 0017 (compatibility)
- `AGENTS.md` — Locked Deviations from pi (Tools)
- `crates/tools/src/device.rs`, `crates/tool/src/registry.rs`
