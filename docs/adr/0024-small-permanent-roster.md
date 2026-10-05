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

## Amendment (2026-10-05)

The owner decided how the one roster exception is bounded. Rule 2 stands as written for every
tool, with one sanctioned, monotonic exception: the hidden `goal` tool is not in the roster until
the user first engages a goal (`/goal`). It is mounted at the next turn boundary after that
engagement and then stays in the roster, byte for byte, for the rest of the session, through
completion, drop, pause, and resume (those become typed `goal` faults on an inactive goal, not an
unmount). The model cannot mount it: a call to a mount the session never mounted is refused at
dispatch, and `create` with no goal present is refused as `UserOnly`. The cost is at most one
cache miss per session, instead of two or more per goal lifecycle. The roster is otherwise a
function of the session's composition and the route's lowering capabilities only. The mechanism:

- `omp-agent` latches a wire roster on the first request of a kernel/session pair: every slot
  tool, intersected with the `sv_tools` the session was composed with (`--tools`, agent cfg),
  plus `think` when `ai_external_thinking` was on at composition, plus `goal` once mounted, minus
  session tools that withhold their declaration when it latches (`task` at the recursion ceiling).
  It is lowered once per capability key (strict schema, grammar, tool-count budget) and shared by
  every request.
- Everything that used to be expressed by omitting a tool is a refusal at dispatch: Plan and Vibe
  binds, later `sv_tools` writes, and the `turn_start` hook's `enabled_tools`. A refused call is
  journaled with a typed `tool.roster.restricted` policy denial whose text lists the tools still
  callable. A tool the wire declared but the registry no longer resolves settles as
  `tool.roster.unavailable`.
- A subagent spawned under plan mode carries a host-set read-only ceiling (`sv_tools_read_only`,
  ADR 0006). It never changes for the life of that child session, so it is latched into the
  child's wire roster at its first request (the read-only tools plus any mounts) and also refuses
  at dispatch. This is the one case where a restriction shapes the wire, and it is legitimate
  because the child's roster is fixed from its first request. The parent's own wire roster is
  never narrowed by Plan.
- Explicit, accepted boundaries: a model switch to a route with different lowering capabilities
  re-lowers the same names once (the cache is per model anyway); RPC `set_host_tools` is accepted
  only between turns and the next request re-lowers; workpool `yield` has a batch-independent
  schema. `turn_start.toolset_changed` is true exactly at these boundaries.

## Status in omp

**Status: Implemented, with the prompt prefix not yet stable.** The tool array is stable for the life of a session (verified 2026-10-05 against the PR that latches the wire roster; see `docs/audits/tool-roster-stability.md` for the transition table T1-T9).

- Fixed identity set: `builtin_tool_identities()` in `crates/tools/src/lib.rs` (32 families, 6 hidden); MCP, media and security devices ride `dyn` (0025).
- Latch: `crates/agent/src/roster.rs` (`WireRoster`), consulted by `Kernel::project_request` in `crates/agent/src/loop.rs`; restrictions in `crates/tool/src/restrictions.rs`.
- Proof: `crates/e2e/tests/p5_prefix_stability.rs` drives a real kernel and compares the `tools` bytes of every request across Plan, Vibe, `sv_tools` writes, `think`, the `task` ceiling, a hook narrowing `enabled_tools`, goal engage/complete/drop/resume, and a model switch.
- Not fixed by this work, so a green proof is not "Plan and Goal are cache-neutral": the Goal and prewalk Directors still prepend a system message at index 0 whose text changes, and mode prompts (`ai_prompt_mode`) are appended to the system prefix. Tool-conditional prompt sections still never render in production, and the Anthropic codec's `cache_control` breakpoint count is unreviewed.
- Not verified: the 'wall-clock benchmark' acceptance test named in the ADR, before and after the full-roster grammar in Plan and Vibe.

## References

- The Harness Playbook, "The tool surface" — "Every schema has a tax"
- 0025 (where the long tail goes), 0021 (constrained sampling budgets), 0017 (compatibility)
- `AGENTS.md` — Locked Deviations from pi (Tools)
- `crates/tools/src/device.rs`, `crates/tool/src/registry.rs`
