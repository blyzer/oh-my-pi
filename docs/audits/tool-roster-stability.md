# Audit: tool-roster stability vs ADR 0024

Date: 2026-10-04. Tree: `omp2` at `9b2d91fe9d`. Method: source reading only (no build, no test
run, no live provider traffic). Every claim below cites the code it rests on; provider-side cache
semantics that cannot be verified from this tree are labelled as such.

ADR 0024 rule 2: "The roster NEVER changes mid-session. Adding, removing, or reshaping a schema
after the first request is prohibited, because it invalidates the cached prefix." AGENTS.md
(Locked Deviations → Tools) restates it as "NEVER ... dynamic schema mutation (prompt-cache
invalidation)".

## TL;DR

- The wire roster is recomputed **on every inference request** in
  `Kernel::project_request` (`crates/agent/src/loop.rs:2211-2255`) and can then be filtered
  again by the `turn_start` hook (`loop.rs:1359-1406`). Director binds are re-derived per request
  as well (`directors.apply_binds`, `loop.rs:1355-1357`). Any input to that computation that
  changes between two requests changes the tool array, **including between two requests of the
  same turn**.
- I found **nine** live transitions (table below), four more than the ADR status note lists:
  the `turn_start` hook's `enabled_tools` filter, RPC `set_host_tools`, the per-batch workpool
  `yield` contract, and route-capability re-lowering on a model switch.
- For every provider whose cache semantics matter here (Anthropic Messages, Bedrock Converse,
  OpenAI Responses/Chat, Gemini), the tool array sits **in front of** the whole system prompt in
  the cached prefix. A roster change therefore costs more than a change to the Frozen prompt
  band: it misses the cache from the first byte.
- No test pins roster stability. `crates/e2e/tests/p5_prefix_stability.rs` hashes synthetic
  prompt bands only, and two agent tests pin the *opposite* behaviour
  (`crates/agent/tests/runtime_flags.rs:199`, `:307`).
- **Safety finding:** restrictions are enforced only by leaving tools out of the request.
  Dispatch resolves calls against the full registry (`loop.rs:2481-2485`, `:2591-2595`), and
  there is no plan-mode "read-only guard" in the dispatch path, although `plan.rs:12-14` refers to
  one. Today a call to a hidden tool is caught only incidentally, by the `omp-ai` recovery layer
  rejecting undeclared names (`crates/ai/src/layer/recover.rs:983-994`, `tool.not-declared` →
  `SemanticRetry`), and the model is never told why. A fixed superset roster removes that
  accidental guard, so **dispatch enforcement is a hard prerequisite**, not just a UX choice.
- Recommendation: (a) latch a wire roster once per *(session composition, route capabilities)*;
  (b) move every restriction (`sv_tools`, Plan and Vibe binds, the hook filter) to a per-request
  `ToolGate` checked at `ToolCallStarted`, before preview or execution; (c) settle each denied
  call with the existing typed `CallOutcome::policy_denied` / `omp_tool::PolicyDenied` outcome.
  Mounts that are user-initiated or set at composition (`think`, the `task` ceiling, `goal`) are
  latched at composition or handled as explicit boundaries. Estimate: about 5–7 engineer-days
  over 3 PRs, plus two owner decisions (Goal, ADR wording).

## 1. Every transition that changes the advertised tools

How a request's tools are built (`crates/agent/src/loop.rs:2211-2255`):

1. `registry.advertise(caps)` lowers every live `Presentation::Slot` entry, both native and
   host-tool rosters (`crates/tool/src/registry.rs:2809-2878`). It sorts by advertisement
   priority, then name, and truncates at `caps.maximum_tools`. Lowering depends on the route:
   `strict_schema` picks strict vs best-effort JSON schema, `grammar` picks a grammar constraint
   vs a JSON fallback, and `maximum_tools` drops low-priority slots (`registry.rs:3422-3500`).
   `caps` comes from `current_route()`, which follows the live model selection each request
   (`loop.rs:2113-2121`, `crates/agent/src/director.rs:248-259`).
2. `goal` is removed unless a Goal Director is active, not done and not dropped (`loop.rs:2213-2220`).
3. `retain` keeps only names in `sv_tools` (`loop.rs:2223-2225`, `crates/agent/src/vars.rs:265-271`, `:343-349`).
4. Any session tool whose `advertised()` is false is withheld; today that is `task`
   (`loop.rs:2228`, `crates/agent/src/dispatch.rs:1346-1354`).
5. The hidden `goal` is appended via `advertise_selected` while a goal is visible (`loop.rs:2232-2238`).
6. The hidden `think` is appended while `ai_external_thinking` is on (`loop.rs:2241-2250`).
7. After `finish_request`, the `turn_start` lifecycle hook may return `enabled_tools`, and the
   request's tools are intersected with it (`loop.rs:1359-1406`; INTERSECT semantics documented
   in `docs/py/05-hooks.md:763`, `:1392`).

All of this runs once per request inside the request loop, after `directors.apply_binds(...)`
(`loop.rs:1355-1358`).

| # | Transition | Trigger (evidence) | What changes on the wire | Can happen between two requests of one turn? |
|---|---|---|---|---|
| T1 | `sv_tools` allowlist | Plan bind `PLAN_TOOLS` (13 names, `crates/agent/src/directors/plan.rs:15-29`, `:183-191`). Vibe bind `VIBE_TOOLS` (9 names, `directors/vibe.rs:11-12`, `:58-64`). Any con write: console `sv_tools [...]`, RPC/extension `omp.con`, Python `@omp.director` binds. `--tools` / agent cfg at launch (`crates/driver/src/v1_import/agents.rs:843`). | Tool **names** (array membership). Shrinks from the full roster to 13 or 9 names, and back on exit. | **Yes.** Binds are re-derived per request. Plan's yolo hand-off returns `Verdict::Done` + `continuing_after_exit` with an `ai_model` write (`plan.rs:87-97`), so the next request **in the same turn** has the full roster on a different model. A con write that commits mid-turn takes effect on the next request. Engaging via `/plan` or `/vibe` normally lands at a turn boundary. |
| T2 | Goal mount | `/goal` (`crates/app/src/chat_control.rs:3904`, `:3991`, `:4036`), the `goal` tool's `create/complete/drop/resume` ops (`crates/driver/src/headless/goal.rs:100-160`), pause on interrupt or session selection (ADR 0015 notes), `cl_goal_enabled` (`crates/chat/src/settings.rs:368`, default **true**; removed at turn start when off, `loop.rs:1248-1257`). | Adds or removes the `goal` **name and schema**, appended at the end of the array. | **Yes.** `goal complete` / `drop` called by the model patches `state/done` / `dropped` mid-turn, and the next request in the same turn drops `goal`. Engage and pause happen at boundaries. |
| T3 | `think` mount | `ai_external_thinking` (`crates/ai/src/settings.rs:1183-1194`, flag `archive`, settings UI "External Thinking", `--external-thinking` at `crates/app/src/chat_cmd.rs:894-896`). The tool is registered `Hidden` (`crates/envd/src/tools.rs:3769-3778`). | Adds or removes the `think` name and schema. It also flips `request.reasoning` (`crates/driver/src/headless/kernel.rs:313-321`), so the reasoning field changes in the same request. | **Yes**, on any con write. The model watch observes it immediately (`crates/agent/src/model_watch.rs:62-67`). |
| T4 | `task` recursion ceiling | `TaskSessionTool::advertised()` → `task_withheld(&parent_ctx)` reads the **live** `sv_task_max_recursion_depth` (archive, settings UI) against `sv_task_recursion_depth` (`crates/driver/src/subagent/spawn.rs:263-269`, `subagent/settings.rs:141-160`, `:546-557`). | Adds or removes the `task` name and schema. | **Yes**, if the user edits the max depth mid-session. A child's depth is fixed at spawn (`spawn.rs:1068-1097`), and resuming a child is a session switch. The trait doc says the answer "must derive from session-scoped state" (`dispatch.rs:790-799`), but nothing enforces that. |
| T5 | `turn_start` hook `enabled_tools` | Any Python extension gating `turn_start` (`loop.rs:1359-1406`). | Removes **names** (intersection). | **Yes**, per request. The payload's `prompt_changed` / `toolset_changed` are hard-coded `requests_started == 0` (`loop.rs:1384-1385`), so extensions are told the toolset never changes. |
| T6 | RPC host tools | RPC `set_host_tools` → `Registry::replace_host_tools` under a `RwLock` (`crates/app/src/rpc_mode.rs:1846-1889`, `crates/tool/src/registry.rs:1845-1880`). Accepted at any time. | Host tool **names, descriptions and schemas**, replaced wholesale. | **Yes.** It is not gated on turn state, and the next `advertise()` reads the new roster. A host tool removed while the model is mid-call fails `resolved_identity`, and the error propagates as `KernelError` (`loop.rs:2485`, `:2595`). |
| T7 | Workpool worker `yield` contract | Each batch: `install_workpool_yield_contract` + `Kernel::replace_tool_registry` (`crates/driver/src/subagent/workpool_runtime.rs:417-432`, `crates/driver/src/headless/kernel.rs:1737-1757`). | The `yield` **schema**: `key.enum = [1..=batch_len]` (`crates/tools/src/yield_tool.rs:278-300`). Byte-identical for equal batch sizes; differs when the size differs (typically the last batch). | No. Turn boundary only (`debug_assert!(!turn_active)`, `loop.rs:644-647`). |
| T8 | Route / model switch | `/model`, `ai_model` binds (Plan `@plan`, prewalk, yolo hand-off), role remaps, `turn_start` REPLACE of model/route (`docs/py/05-hooks.md:1392`). `current_route()` is re-resolved per request. | Same names, but **schema shape** changes when caps differ: strict on/off, grammar vs JSON fallback, `maximum_tools` truncation. | **Yes** (yolo hand-off; any con write). The recovery-middleware fallback serves another model on the same lowered tools, so the codec shape changes but the names do not. |
| T9 | `tool_choice` (forced call) | `ForceTool::prepare_inference` sets `tool_choice = Named(..)` and `forced_call` (`crates/agent/src/directors/force_tool.rs:147-170`), from Plan's decision gate, autolearn (`loop.rs:1756-1768`) and the workpool yield ladder. | Not the roster: a sibling request field. | Yes, per request while engaged. Listed because some providers count it in cache invalidation (§2). |

Verified **not** to change the roster mid-session, because each is read only at composition or
lives off-wire:

- MCP servers: reached through the `dyn` shell builtin (`crates/envd/src/exec.rs:730`). They are
  never registered as `Slot`, and nothing in `crates/envd/src/mcp/` registers or replaces registry
  tools.
- Extension worker tools: registered once at composition as `Presentation::Device` (as `Slot` only
  under `ToolsPolicy::ToolOnly` flattening; `crates/envd/src/tools.rs:3863-3874`, `:5554-5560`).
- `task`/`hub` dynamic tools: composition only (`crates/driver/src/headless/kernel.rs:1900-1919`).
- `sv_tools_enabled` (`/computer on|off`, `crates/chat/src/commands/misc.rs:445-466`), edit
  dialect (`configured_model_edit_revision`, `crates/envd/src/tools.rs:3916-3926`), `sv_eval_py`,
  and similar settings: read by `ToolSettings::from_con` at composition (`crates/envd/src/server.rs:3040-3093`).
  A mid-session write has no roster effect until the next composition. That is compliant, but
  `/computer` reports "enabled" while the tool is "not registered".
- Approvals (`sv_tools_approval_mode`, `sv_tools_approval`): enforced at dispatch through
  `ToolAdmission` (`crates/agent/src/dispatch.rs:1870-1898`;
  `SettingsAdmission` in `crates/driver/src/headless/kernel.rs:687-735`). This is the precedent
  the proposed design generalises.

Two adjacent observations about how the roster relates to the prompt:

- The system prompt's tool-conditional sections (`{% if "edit" in tools %}` …,
  `crates/agent/prompts/system/tool-policy.md:8-97`, `workflow.md:33-43`, `runtime.md:39-46`)
  read the `tools` template prop. That prop comes **only** from the `ToolRoster` DOM component
  (`crates/agent/src/prompt/projection.rs:33-37`), which is populated only by a `dynamic-tools`
  tool outcome (`crates/session/src/components/lifecycle.rs:84-106`). No production tool emits
  one; only `crates/session/tests/appendix_a.rs` does. Otherwise the default is an empty list
  (`projection.rs:154`). The prompt text is therefore roster-independent today: stable, but those
  guidance blocks never render in production. This needs a follow-up, and the fix must feed them
  the *latched* roster, not the per-request one.
- `toolset_hash` and `prompt_hash_of` use `std`'s `DefaultHasher` (`loop.rs:4000-4024`). By the
  AGENTS hashing rule they should migrate to `omp_core::fast_hash64` on touch.

## 2. Prompt-cache consequences

### Where the tool array sits in the prefix

- **Anthropic Messages** (`crates/ai/src/codec/anthropic.rs:1383-1410`): system-role messages
  go to `body.system`, tools to `body.tools`. Anthropic documents the cache prefix order as
  `tools → system → messages`, so the tool array comes before Frozen band byte 0.
  - Breakpoints: with `ai_cache_retention auto` (the default, `crates/catalog/src/settings.rs:1352-1365`),
    `InferenceSettings::apply_chat_request` leaves `cache_retention` unset
    (`crates/ai/src/settings.rs:560-566`), and the codec emits **no** `cache_control`
    (`anthropic.rs:1363`, `:1696-1702`). Whether the provider caches anything without markers is
    provider-side behaviour I could not verify offline.
  - With `short`/`long`, the codec stamps `cache_control` on **every** text, tool, tool-use and
    tool-result block (`anthropic.rs:1408`, `:1531-1576`). That appears to exceed Anthropic's
    documented 4-breakpoint limit; I found no budgeting pass. **Adjacent suspected defect:
    verify against the live API.**
- **Bedrock Converse** (`crates/ai/src/codec/bedrock.rs:559-577`): explicit checkpoints. The
  catalog says `prompt-cache-mode "explicit"` with `prompt-cache-maximum-checkpoints 4`
  (`crates/catalog/compat/classes/anthropic.kdl:199-258`); the code caps them at 2, one after
  the last user message and one after `system`. `toolConfig` is ahead of both checkpoints in the
  cached prefix, so a tool change misses both.
- **OpenAI Responses / Chat**: automatic prefix caching keyed by `prompt_cache_key`
  (`crates/ai/src/codec/openai_responses.rs:3168-3200`). OpenAI documents tool definitions as
  part of the cached prefix, ahead of messages, so a tool change misses from the start.
  Server-state continuation (`previous_response_id`) binding validity does not consider tools
  (`crates/ai/src/session/binding.rs:231-250`, `:300-350`); tools are re-sent per request either
  way.
- **Gemini**: implicit and explicit context caching both include tools and system instruction
  (`crates/ai/src/codec/gemini.rs:957`, `CachePoint` handling). Same consequence.

### Relation to the scribe bands

`CanonicalPromptSource::candidate` renders Frozen (conventions, role, runtime, workflow,
delivery), then Stable (tool-policy, custom, append), then Dynamic, then Volatile, as separate
system items (`crates/agent/src/prompt/mod.rs:52-90`, slot classes at
`crates/agent/src/prompt/slots.rs:391-406`). The tool array is not a band. On every provider
above it sits *in front of* the Frozen band, so a roster transition is strictly worse than
editing the Frozen band: it misses the cache for tools, every band, and the entire transcript.

### Consequence per transition

| # | Consequence when it fires | Frequency |
|---|---|---|
| T1 | Full prefix miss on entry and again on exit. If `@plan` resolves to the session's current model, the roster is the dominant invalidator for Plan. If `@plan` names a different model, the cache is per-model anyway and roster stability buys nothing on entry, but the original model's cache can still be warm on return only if the roster is byte-identical then (it is today, because exit resets `sv_tools` to empty). Vibe has no model bind, so it always pays both misses. | Per mode engagement; any user `sv_tools` write. |
| T2 | Full miss on goal mount, on unmount (complete, drop, pause), and on remount (resume). The Goal Director's `prepare_inference` also **prepends** a system message at index 0 (`crates/agent/src/director.rs:1326-1335`, `directors/goal.rs:284-296`), in front of the Frozen band, and its text flips between `active` and `budget-limit`. So the system prefix is unstable for the whole goal lifetime, independent of the roster. | Per goal lifecycle edge. |
| T3 | Full miss. The simultaneous reasoning change also invalidates Anthropic message caches (provider-documented). | Rare; settings toggle. |
| T4 | Full miss. | Rare; settings edit. |
| T5 | Full miss per request whenever the filter result differs from the previous request. The extension is told `toolset_changed: false`. | Extension-defined; potentially every request. |
| T6 | Full miss; can land mid-turn. | Client-defined. |
| T7 | Full miss for a long-lived worker session whenever batch size changes. | Usually the last batch. |
| T8 | Moot for the cache (a different model is a different cache). Switching back re-lowers to identical bytes, because lowering is deterministic in `(registry, caps)`. | Per model switch. |
| T9 | Not a roster change. Anthropic documents `tool_choice` changes as invalidating the messages segment while tools and system stay cached. | While a ForceTool is engaged. |

Related prefix defects (not roster, but same ADR spirit):

- Mode prompts (`ai_prompt_mode`) are pushed as a System message *after* the Volatile band and
  before the thread (`loop.rs:2168-2186`). The comment says they join "the stable band", but on
  Anthropic they land at the end of `body.system`, so engaging Plan/Vibe still invalidates system
  and messages even with a fixed roster.
- Prewalk also uses `prepend_system` (`crates/agent/src/directors/prewalk.rs:85-94`).

### Tests

- `crates/e2e/tests/p5_prefix_stability.rs` builds a synthetic `SlotAssembler` with four
  constant slots and asserts band hashes (`:38-98`), plus snapcompact band preservation
  (`:100-200`). It never runs a kernel request, never inspects `ChatRequest.tools`, and covers
  none of T1–T9.
- `crates/e2e/tests/p4_schema_isolation.rs` checks that only the live revision's schema is
  advertised. It runs one request and does not compare requests.
- Tests that pin the ADR-violating behaviour (they must be rewritten):
  - `crates/agent/tests/runtime_flags.rs:199` (`goal_tool_roster_follows_the_durable_engagement_state`)
  - `:238` (`disabled_goal_is_removed_before_inference_while_enabled_goal_remains`)
  - `:307` (`session_tool_withholding_follows_its_state_per_request`)
  - `crates/driver/src/headless/kernel.rs:4049-4073` (per-batch yield contract)
  - `crates/agent/tests/directors/plan.rs:56-68` asserts the bind values only, which is fine to keep.

## 3. Proposed design

### Principle

Wire roster = f(session composition, route capabilities) only. Everything else that is currently
expressed by omitting a tool becomes a typed refusal at dispatch.

1. **Latched `WireRoster`.** On the first `project_request` of a kernel/session pair, compute
   the advertised set:
   - all `Slot` entries,
   - ∩ the **composition-time** `sv_tools` (so `--tools` and agent-cfg narrowing stay legal,
     because they happen before the first request),
   - ∪ composition-latched hidden mounts (`think` if `ai_external_thinking` was on; `goal` per
     owner decision below),
   - minus session tools withheld at composition (`task` at the ceiling).

   Lower it once per caps key `(strict_schema, grammar, maximum_tools)` and keep it as
   `Arc<[ToolDefinition]>` in the kernel. `ChatRequest.tools` is already `Arc<[ToolDefinition]>`
   (`loop.rs:2144`), so each request is an O(1) clone. This also removes today's per-request
   re-lowering, which clones every schema (`registry.rs:3428`), a small allocation-discipline win.
   Re-lower only when the caps key changes (T8, an explicit boundary). Record a roster
   fingerprint (`Hash32` over the serialized definitions) on the first request, and make
   `turn_start.toolset_changed` the real comparison.
2. **`ToolGate`, snapshotted per request.** Build it from the live `sv_tools` (with origin:
   user, or Director bind `plan`/`vibe`/extension) and the hook's `enabled_tools`. Snapshot it at
   request projection, next to the request, so a con write racing the stream cannot change the
   verdict for calls the model sampled under the previous instructions.
3. **Enforce at `ToolCallStarted`**, and on the non-streamed `ToolCallReady` and resume paths
   (`loop.rs:2479`, `:2591`, `:1168`). That is before `dispatcher.prepare` and argument feeding,
   so a denied `edit` never produces a speculative preview. Denied calls are still journaled as
   calls (faithful raw journaling) and settle with
   `CallOutcome::policy_denied(Abort::Skipped { reason }, PolicyDenied { code, rules, decision_id })`.
   That outcome already exists (`crates/tool/src/lib.rs:1345-1346`, `:1800-1813`) but has no
   committer helper today; add `Committer::commit_policy_denied` beside `commit_abort`
   (`dispatch.rs:2843-2857`). The gate applies to **all** units (native, session tools, host
   tools, workers), unlike `ToolAdmission`, which only sees `Unit::Native` (`dispatch.rs:1874-1876`).
4. **A name declared on the wire that no longer resolves** settles as a typed
   `tool.roster.unavailable` denial instead of a `KernelError`. That happens after a host roster
   replacement, or a latched mount whose backing tool is gone. Undeclared names stay with the
   existing recovery-layer rejection.

The denial reason comes from a typed enum (no string payloads, per AGENTS) and is rendered once,
at the projection boundary. Model-visible text, as projected through `Abort::render`:

```
skipped: `edit` is not available while plan mode is active. No action was taken.
Available now: read, grep, glob, ast_grep, lsp, web_search, think, todo, write, ask, task, hub, yield.
Write the plan to <plan file>, then present it for a decision.
```

Durable evidence:

```json
{"kind":"policy_denied","code":"tool.roster.restricted",
 "rules":["director:plan/sv_tools"],"decision_id":"<call_id>"}
```

The other codes:

- `tool.roster.restricted` with `rules: ["sv_tools"]` for a user allowlist.
- `tool.roster.hook` with `rules: ["hook:turn_start:<extension>"]`.
- `tool.roster.unavailable` for T6 removals.

The existing tool-specific faults already cover their cases and need no new text:
`goal::Fault::NoGoal` / `InvalidTransition` (`crates/driver/src/headless/goal.rs:139-160`) and
`SpawnError::RecursionDepth` from `admit_batch` (`crates/driver/src/subagent/spawn.rs:718-730`).

### Treatment per transition

| # | Treatment | Notes |
|---|---|---|
| T1 `sv_tools` / Plan / Vibe | **Dispatch gate.** The wire keeps the latched superset. | The mode prompt already describes the mode; add the allowed list to the Plan/Vibe mode prompt text so most calls never reach the gate. A later write can only **narrow** relative to the latched set. Widening beyond the wire roster is ignored for advertising and logged; a new session is needed. This also makes the "read-only guard" that `plan.rs:12-14` claims real. |
| T2 Goal | **Owner decision.** (A) Strict ADR: latch `goal` into the roster at composition when `cl_goal_enabled` (default true) and let `NoGoal` / `InvalidTransition` faults handle the inactive state. This puts a permanent schema tax on every session, which ADR rule 3/5 require measuring. (B) Pragmatic: a **sticky** mount at the first user-initiated engagement, never removed in that session; complete, drop and pause become faults. At most one miss per session instead of two or more per goal lifecycle; needs an ADR 0024 amendment for "monotonic, user-initiated mounts at a turn boundary". | With (A), gate model-initiated `create` (typed denial `goal.create.user_only`) unless the owner wants the model to open goals unprompted. Independently, move the Goal/prewalk `prepend_system` text out of index 0 (follow-up). |
| T3 `think` | **Latch at composition.** | A mid-session toggle still flips provider reasoning (unavoidable). `think` membership changes only at the next session boundary; the settings UI notes "applies to new sessions". If off mid-session, `think` stays advertised and is a harmless scratchpad. |
| T4 `task` ceiling | **Latch** `advertised()` once at composition (cache the bool in the dispatcher when the session tool is installed). | A runtime max-depth change is already enforced by `admit_batch` → `RecursionDepth` fault. That fault is built by stringifying the error (`spawn.rs:281-287`, `Str::new(source.to_string())`), an AGENTS error-rule violation to fix on touch. |
| T5 hook `enabled_tools` | **Dispatch gate** (same INTERSECT, applied to calls). | Update `docs/py/05-hooks.md:763`, `:1392` and the Python stub docs. Make `toolset_changed` truthful. |
| T6 RPC `set_host_tools` | **Explicit boundary.** Accept only while idle (reject `invalid_state` during a turn); the next request re-lowers and reports `toolset_changed: true`; document the cache cost in the RPC protocol docs. | Removing a tool that was in flight settles `tool.roster.unavailable`. |
| T7 workpool `yield` | **Stabilise the schema.** Replace `key.enum: [1..=n]` with `"minimum": 1` (optionally a `maximum` equal to the pool's fixed batch size). | Runtime validation already "strictly closes keys" (`yield_tool.rs:312-315`), so an out-of-range key returns its existing typed fault. The constraint is `None`, so no strict-sampling guarantee is lost. Smallest fix in the set. |
| T8 model / route | **Explicit boundary (keep).** | The per-model cache makes it moot. Lowering is deterministic, so switching back hits a warm cache within TTL. Watch `maximum_tools` routes: a larger latched superset can be truncated there by priority, which is deterministic but should be logged once. |
| T9 `tool_choice` | **No change.** | Not a roster change. |

### Risks

- **Model confusion with denied but advertised tools.** The model sees `edit` in plan mode and
  may call it. Mitigations: the allowed list in the mode prompt; denial text naming what is
  allowed and what to do next; a telemetry counter on `tool.roster.restricted`. Ironically, today
  the model gets *no* explanation: the recovery layer silently re-samples (`SemanticRetry`).
- **Losing the accidental guard.** As noted, the recovery layer's `tool.not-declared` rejection is
  today's only enforcement, so the gate must ship in the same PR as the latched roster. The gate
  must cover session and host tools, and must run before speculative previews (the edit tool's
  ArgFeed → preview path).
- **Grammar tax in Plan/Vibe.** Per-request sampling cost in those modes rises to the normal-mode
  roster size (ADR 0024's own mechanism), traded for cache hits. Re-run the ADR's wall-clock
  benchmark for a Plan session before and after.
- **Subagent ceilings.** Children are composed with their depth, so latching is exact for them.
  The only behaviour change is that editing max depth mid-session no longer adds or removes
  `task` from a running session's wire roster; `admit_batch` still refuses beyond the ceiling.
- **Mount-only tools (`goal`).** Option (A) requires the tool to behave sensibly with no goal
  (it does: `Fault::NoGoal`). Option (B) needs the ADR amendment. Neither should keep today's
  unmount-on-complete.
- **Concurrent con writes.** The snapshotted `ToolGate` avoids "sampled under plan, dispatched
  after exit" races. Journal the gate's rule ids in the denial, so replay is deterministic.
- **Prefix not fully fixed by this work.** Mode prompts and Goal/prewalk `prepend_system` still
  move system text (see §2). Say so in the ADR status, so a green P5 is not read as "Plan is now
  cache-neutral".

### Blast radius

- `crates/agent/src/loop.rs`: `project_request`, the `turn_start` gate, the three
  identity-resolution sites, `toolset_changed`.
- `crates/agent/src/dispatch.rs`: `ToolGate`, `commit_policy_denied`, latched `withholds`.
- `crates/agent/src/vars.rs`: `SV_TOOLS` doc becomes "dispatch allowlist; the wire roster is
  latched at composition".
- `crates/agent/src/directors/{plan,vibe}.rs`: mode-prompt allowed list (or a generated notice).
- `crates/driver/src/headless/kernel.rs`: composition latches for `think`/`goal`.
- `crates/driver/src/subagent/spawn.rs`: `task` latch.
- `crates/tools/src/yield_tool.rs`: stable workpool schema.
- `crates/app/src/rpc_mode.rs`: idle-only `set_host_tools`.
- Docs: `docs/adr/0024-*.md` (status, plus an amendment if option B), `docs/adr/0015-*.md`
  (Goal roster note), `docs/py/05-hooks.md`, `crates/py/python/omp/events.py` docstring.

## 4. PR-sized plan with tests at the owning seams

**PR 1: latched wire roster plus dispatch gate** (`omp-agent`, `omp-tool`). About 2.5–3 days.

- `WireRoster` latch keyed by caps; `ToolGate` snapshot per request; gate at the three resolution
  sites; `commit_policy_denied`; `tool.roster.unavailable` instead of `KernelError`; truthful
  `toolset_changed`.
- Tests in `crates/agent/tests`:
  - Rewrite `runtime_flags.rs:199`, `:238` and `:307` to assert the wire roster is
    **constant** across turns, plus a denial outcome when the gated tool is called.
  - New `directors/plan.rs` case: with Plan engaged, a scripted `edit` call is journaled with
    `code = tool.roster.restricted`, runs no preview and has no effect, while `read` succeeds.
  - Hook case: `enabled_tools` filters calls, not `requests[i].tools`.
- **P5 extension** (`crates/e2e/tests/p5_prefix_stability.rs`):
  `p5_tool_roster_bytes_are_constant_across_session_transitions`. Run a real `Kernel` with
  `ScriptedInference` (the P4 pattern) over these steps:
  - turn 1, normal;
  - engage Plan, then turn 2 calls `edit` (denied) and `read`;
  - yolo-style exit mid-turn on the **same** caps;
  - Vibe engage and exit;
  - a user `sv_tools` write;
  - a `sv_task_max_recursion_depth 0` write.

  Assert that every `requests[i].tools`, serialized, is byte-identical. Assert that the denial
  text reaches `requests[i+1].messages`. Add a negative control: a caps change (strict on/off)
  produces a different roster, documenting T8 as the one allowed boundary. Include a mid-turn
  `sv_tools` write between two requests of one turn.

**PR 2: composition latches and boundary transports** (`omp-driver`, `omp-tools`, `omp-app`).
About 2 days.

- `think` and `task` latches; Goal option A or B as decided; workpool `yield` stable schema; RPC
  idle-only `set_host_tools`.
- Tests:
  - `crates/driver` composition tests for each latch: toggle the convar after composition and
    assert the roster is unchanged and a dispatch fault is produced.
  - Update `crates/driver/src/headless/kernel.rs:4049-4073` so batches of size 3 and 1 yield an
    identical `yield` schema, and an out-of-range key is a typed fault.
  - `crates/tools` unit test for the schema.
  - `crates/app/tests/rpc_spine.rs`: `set_host_tools` during an active turn is rejected with
    `invalid_state`; while idle it is accepted and the next turn reports `toolset_changed: true`.
  - Add a T2 Goal step to the P5 roster proof (engage, complete mid-turn, resume).

**PR 3: docs and measurement.** About 1 day of work, plus benchmark wall-clock time.

- ADR 0024 "Status in omp" updated, and the amendment if option B; `docs/py/05-hooks.md`.
- Re-run the ADR 0024 wall-clock benchmark for (i) a normal session and (ii) a Plan session,
  before and after. Record the measured Goal schema tax to justify option A or B (ADR rule 5).

**Follow-ups, out of scope but found here:**

1. Move Goal/prewalk `prepend_system` and the mode prompts out of the system prefix, into the
   structured notices channel.
2. Verify the Anthropic codec's every-block `cache_control` stamping against the 4-breakpoint
   limit, and map bands to breakpoints. With `auto`, decide whether Anthropic should get explicit
   breakpoints at all.
3. Feed the latched roster to the `tools` template prop, so the tool-policy prompt sections
   render.
4. `/computer` status text.

**Honest estimate:** 5–7 engineer-days for PRs 1–3, assuming the Goal and ADR-wording decisions
are made up front. PR 1 carries most of the risk, in the gate placement relative to streaming
previews and in the rewritten agent tests; PR 2 is mostly mechanical. The prompt-prefix
follow-ups (item 1) are a separate 2–3-day effort, and the end-to-end cache-hit improvement for
Plan/Goal is only realised once they land too.
