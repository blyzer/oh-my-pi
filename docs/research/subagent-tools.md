# Gap 9: subagent tools (`subagent-tools`)

Scope: `/seance` (consult a saved session read-only, from the journal, without opening a writer) and the subagent completion probe.
Upstream: can1357/oh-my-pi `c308970263` (v18.20.0), paths under `packages/coding-agent/src`, written `up:`. omp2: branch `docs/upstream-gap-research` at `f4186b783b`.
Classes: **a** already covered, **b** not needed, **c** port, **d** propose. Decision record: [ADR 0056](../adr/0056-subagent-consult-tools.md).

| Item | Class | One line |
|---|---|---|
| `/seance` | **c** | omp2 has the writer-free read (`Journal::scan`) and `KernelOptions.fork`. It has no command. Upstream's `task`-tool form cannot be ported (it changes a tool schema); the user command form can. |
| Completion probe | **d** | Optional, cost-bearing UX. omp2's `/btw` is not a usable base (no cache reuse). |

## 1. `/seance`

### 1. Upstream

- `up:slash-commands/seance.ts`: `parseSeanceArgs` (17) takes `[session id|path] [--model selector]`. `launchSeance` (39) calls the `task` tool with `{agent: "seance", sourceSession, task, solutionSpace, model?}`. `chooseSeanceModel` (100) refuses a custom `seance` agent that shadows the bundled one, resolves the source, then offers "Use saved model" or a one-off model picker.
- `up:prompts/agents/seance.md` (`tools: read, grep, glob`, line 4): a read-only consultant. Session content is untrusted evidence, no mutation, answers cite transcript context.
- `up:task/seance.ts`: `resolveSeanceSource` (110) is documented as resolving "an explicit session path or an unambiguous stored session id without opening its writer"; it validates the header and lists with a read-only lister. `savedModelSelectors` (63) reads the saved models from the entries. `prepareSeance` (224) requires the bundled agent, no plan mode, no isolation, no custom tools, and returns `fork: { sourceFile }` (32), so the consult runs in a fork and the source file is never written.
- `up:task/seance-policy.ts:6` `seanceIsolationOptions`: no extension discovery, MCP, IRC, LSP, spawns, skills, rules, context files or custom tools.
- It matters because a past session is the best record of why code looks the way it does, and the user can ask it questions without resuming (and mutating) it.

### 2. omp2 inventory

- **Read-only primitive.** `crates/journal/src/lib.rs:186` `Journal::scan` reads committed entries "without taking the writer lock or truncating a torn tail" (doc 175-185), and `scan_prefix` (218) reads a bounded prefix. `Journal::open` (124) takes the writer lock and truncates a torn tail, so it is the wrong call for a consult.
- **Fork primitive.** `crates/driver/src/headless/kernel.rs:3451` `fork(&self, source)` copies the source journal and its session-local files to a fresh journal and opens the copy. `KernelOptions` (139) has `fork` (145), `ephemeral` (149), `no_tools` (151) and `tools: Option<Vec<Str>>` (153, "restrict the registry to these validated stable tool names"). `/fork` is a palette command (`crates/chat/src/commands/session.rs:24`).
- **User-command precedent.** `/btw` composes a child kernel from a chat command: `crates/app/src/chat_services/session_ops.rs:249` `btw`, trait method `crates/chat/src/overlays/services.rs:1408`, panel `crates/chat/src/overlays/side.rs:1`.
- **No model-facing room.** `crates/tools/src/task.rs:49-50`: `ChildRequest` is `#[serde(deny_unknown_fields)]` with `task, name, agent, effort, outputSchema, schemaMode, isolated`. A `sourceSession` or `model` field would change the `task` tool's schema, which the additive-only rule and ADR 0024 rule 2 forbid. `ChildResult.session_path` is already reported (task.rs, `pub session_path`).
- **`history://` is live-only.** `crates/envd/src/tool_url.rs:126-130`: reading a session that is not live fails with "Session `{base}` is not live." So `history://` cannot read a saved session.
- **Saved model is recoverable from the journal.** `msg.assistant.start` records `model`, `provider` and `route` per assistant message (`crates/journal/src/data.rs:479-486`); receipts record `ReceiptIdentity{role,provider,model}` (data.rs:640). The journal holds the concrete model only; the role name is not saved, so there is no role to re-resolve.
- No `seance` command or agent: a grep over `crates` for `seance|sourceSession` hits only unrelated `source_session` identifiers (`crates/envd/src/lib.rs`, `tool_url/local.rs:170`, `browser_relay.rs`).
- v1 sessions: `crates/app/src/session_import/convert/omp1.rs` imports them into journals; `Journal::scan` reads only omp2 journals.

### 3. Class: **c (port)**, as a user command

Every primitive the upstream safety property needs exists. The missing piece is the command and its read-only child. The upstream `task`-tool entry point is **not** ported (schema change); see the ADR.

### 4. Options

- **A. Port as upstream: `sourceSession`/`model` on `task`.** Lets the model consult sessions. Rejected: changes an existing tool's schema (hard constraint; ADR 0024 rule 2, latched roster, `roster.rs` header).
- **B. User command composing a read-only child on a fork (chosen).** `Journal::scan` validates and reads the saved model; `Kernel::fork` copies; the child gets `tools: read, grep, glob`. No tool, schema or roster change. Cost: moderate, mostly the command and a prompt.
- **C. Open the source with `KernelOptions.session`.** Rejected: opens a writer on the source.
- **D. `history://`.** Rejected as the only path: live sessions only.

### 5. Plan (PR-sized; all behind the default-off convar `cl_seance_enabled`)

1. **Resolve and validate** (`omp-driver`): path or id to a journal, `Journal::scan`, saved model from the last `msg.assistant.start`. Tests: source bytes unchanged (hash) after resolve; torn tail not truncated; unknown id fails closed; an imported v1 session resolves.
2. **Read-only child** (`omp-driver`): fork, `tools: [read, grep, glob]`, seance prompt via `PromptOverrides` (`kernel.rs:47`), no MCP, extensions, skills. Tests: child registry contains exactly the three tools; source journal hash unchanged after a full consult; a write attempt is refused.
3. **`/seance [id|path] [--model sel]`** (`omp-chat`, `omp-app` service like `btw`): shows the answer in the side panel. Tests: command parse; panel shows the answer; no command when the convar is off.

### 6. Open questions

- Keep or delete the forked journal after the consult? (see ADR 0056)
- Allow the model to consult (a `task`-schema change)? Not now.

## 2. Completion probe

### 1. Upstream

- `up:task/completion-probe.ts`: on a backoff schedule `PROBE_DELAYS_MS = [2,5,10,30,60]` minutes (37, last repeats) the subagent is asked, through the ephemeral side turn `/btw` uses, for one JSON line `{"pct","stage","watch"}`. The side turn reuses the live prompt, tools and history, so it reads the prompt cache and never touches the transcript. Each probe receives the previous estimate and the tool calls still streaming.
- `isCompletionProbeEnabled` (45): only `parentDepth === 0`, interactive host, and `task.completionProbe`, whose default is **true** (`up:task/settings.ts:543`). Started from `up:task/executor.ts:2463`. Output feeds `completionPercent`, `stage` and `watch` on the running row.

### 2. omp2 inventory

- Nothing equivalent: no `completion_probe`, `completionPercent` or percent/stage/watch field in `crates/driver/src/subagent/*.rs`.
- `/btw` is not the same mechanism. It composes a **fresh ephemeral, tool-less kernel** (`KernelOptions{ephemeral: true, no_tools: true}`, `session_ops.rs:269-275`) and pastes the transcript in as a `<conversation-context>` text block (262-264), so it does not reuse the live session's prompt cache or tool roster. A probe built on it pays for the whole child context uncached on every tick.
- A **side turn on a live child without touching its journal is possible**: `Kernel::fork` (kernel.rs:3451) copies the child's journal (path in `ChildResult.session_path`) and opens the copy; the child's journal is not opened. Cache reuse with the fork is **not verified**: it needs the fork's system prompt and tool roster to be byte-identical to the child's.
- Depth gate exists: `SV_TASK_RECURSION_DEPTH` (`crates/driver/src/subagent/settings.rs:101`).

### 3. Class: **d (propose)**

A UX feature with a recurring paid-token cost per running subagent. Not adopted by default.

### 4. Options

- **A. Port as upstream (on by default, live-session side turn).** Rejected: omp2 has no live-session side-turn API; default-on cost is unmeasured.
- **B. Build on `/btw`'s fresh kernel.** Rejected: uncached full-context cost per tick.
- **C. Fork-based probe, default off, measure first (chosen).** Fork the child journal, ask for the JSON line, discard the fork.
- **D. Do nothing.** Acceptable fallback if the measurement shows no cache reuse.

### 5. Plan (default-off convar `sv_task_completion_probe`)

1. **Measurement spike:** one fork-based probe of a running child; record input tokens, cached tokens and cost. Exit: a number in the ADR. If cached tokens are ~0, stop (option D).
2. **Estimator:** parse one JSON line (`pct` 0-100, `stage` 80 chars, `watch` 200 chars) from a fake model; backoff schedule as a pure function. Tests: parser table, schedule, abort cancels an in-flight probe.
3. **Wiring:** depth 0 only, interactive host only, shown on the hub row. Tests: no probe at depth 1; none when the convar is off; child journal hash unchanged.

### 6. Open questions

- Where the estimate is shown (hub row) and whether the job DOM gets an additive prop.
- Whether the fork shares the prompt cache (settled only by the spike).

## Upstream origin and license

Draws on `up:slash-commands/seance.ts`, `task/seance.ts`, `task/seance-policy.ts`, `prompts/agents/seance.md`, `task/completion-probe.ts`, `task/settings.ts`, `task/executor.ts`. License: MIT, "Copyright (c) 2025 Mario Zechner; (c) 2025-2026 Can Bölük; (c) 2026 Stencil Labs, Inc." (upstream `LICENSE`). The copyright and permission notice must be kept in copied portions; a port from prompts or logic goes in `THIRD-PARTY-NOTICES.txt` as an attribution if text is copied. omp2 is its own Rust implementation; no TypeScript is carried over.
