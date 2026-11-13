# 0056. Consult a saved session read-only; subagent progress is opt-in and measured

Status: proposed
Date: 2026-11-13
Area: tools

Research: [subagent-tools](../research/subagent-tools.md). Ledger gap 9.

## Context

- Upstream OMPv1 (can1357/oh-my-pi `c308970263`, paths under `packages/coding-agent/src`, written `up:`) has two subagent features omp2 lacks. `/seance` consults a saved session: it resolves a session id or path "without opening its writer" (`up:task/seance.ts:110`), forks the source (`:32`, `fork: { sourceFile }`) and runs the bundled `seance` agent with only `read, grep, glob` (`up:prompts/agents/seance.md:4`) and everything else off (`up:task/seance-policy.ts:6`). The **completion probe** asks a running subagent, on a 2, 5, 10, 30, 60 minute backoff (`up:task/completion-probe.ts:37`), for `{"pct","stage","watch"}` through a side turn that reuses the live prompt cache; it is on by default (`up:task/settings.ts:543`) for the main agent's direct subagents in the interactive TUI (`completion-probe.ts:45`).
- omp2 already has the safety-critical primitives for `/seance`. `Journal::scan` reads committed entries "without taking the writer lock or truncating a torn tail" (`crates/journal/src/lib.rs:186`); `Journal::open` takes the writer lock (`lib.rs:124`). `Kernel::fork` copies a journal and its session-local files and opens the copy (`crates/driver/src/headless/kernel.rs:3451`). `KernelOptions` has `fork` (145), `ephemeral` (149), `no_tools` (151) and `tools: Option<Vec<Str>>` (153).
- The saved model is recoverable: `msg.assistant.start` records `model`, `provider` and `route` (`crates/journal/src/data.rs:479`). The role name is not saved, only the concrete model.
- omp2 has no command, agent or task field for it. `ChildRequest` is `deny_unknown_fields` with `task, name, agent, effort, outputSchema, schemaMode, isolated` (`crates/tools/src/task.rs:49-50`). Adding `sourceSession` or `model` would change the `task` tool's schema; ADR 0024 rule 2 and the additive-only rule forbid that.
- `history://` cannot stand in: it reads live sessions only ("Session `{base}` is not live.", `crates/envd/src/tool_url.rs:126-130`).
- `/btw` is the nearest precedent for a chat command that runs a child: it composes a fresh ephemeral, tool-less kernel and pastes the transcript in as text (`crates/app/src/chat_services/session_ops.rs:249-275`; trait `crates/chat/src/overlays/services.rs:1408`; panel `overlays/side.rs:1`). It does not reuse the live session's prompt cache or tool roster, which is what the upstream probe relies on.
- No part of this is vendor-specific, adds a roster tool, or needs a second language runtime.

## Decision

**D1. `/seance` is a user command, not a `task` field.** `/seance [session id|path] [--model selector]` is added to the chat commands. It does not touch the `task` tool, the roster or any schema. The model cannot consult a session; only the user can.

**D2. The source is read, never opened for write.** A session id or path resolves to a journal in the project sessions directory; validation and reading use `Journal::scan`. The saved model is the `model`/`provider` of the last `msg.assistant.start`. A journal that cannot be scanned, or an id that is ambiguous or unknown, is refused with a message; nothing is guessed. A v1 session is not read directly: the message names the importer (`crates/app/src/session_import/convert/omp1.rs`).

**D3. The consult runs on a fork with three tools.** The child is composed with `fork: Some(source)` and `tools: Some([read, grep, glob])`, the seance prompt supplied through `PromptOverrides` (`kernel.rs:47`), and no MCP, extensions, skills or spawns. The source file's bytes never change. The consult's answer shows in the side panel `/btw` uses.

**D4. Model choice.** Default: the saved concrete model, if it resolves and has configured auth. `--model` overrides it. With neither, the command fails and says to pass `--model`. The role is not recovered because the journal does not store it.

**D5. A custom agent never shadows the consult.** The seance prompt is bundled data, not a user-overridable role (the upstream refusal at `up:slash-commands/seance.ts:100` is therefore not needed).

**D6. The completion probe is proposed, not adopted.** It ships only if a measurement shows a fork-based probe reuses the child's prompt cache. The probe asks a running child's fork, not the child: `Kernel::fork` copies the child's journal (path in `ChildResult.session_path`) and the child's own journal is never opened. It is **off by default** (upstream's default is on), depth 0 only (`SV_TASK_RECURSION_DEPTH`, `crates/driver/src/subagent/settings.rs:101`), interactive host only.

**D7. Probe output is data.** One JSON line, `pct` 0-100, `stage` at most 80 characters, `watch` at most 200 characters, parsed by a pure function; an unparseable reply is dropped, never shown.

## Options rejected

- **Port upstream as written (`sourceSession`, `model` and `solutionSpace` on `task`).** Changes an existing tool's schema (hard constraint; ADR 0024 rule 2, `crates/agent/src/roster.rs` header). The model-facing form is recorded as O3.
- **Open the source session with `KernelOptions.session`.** Opens a writer on the source and appends to it.
- **Read through `history://`.** Live sessions only.
- **Build the probe on `/btw`'s fresh kernel.** Pays for the whole child context uncached on every tick.
- **Probe on the live child's own session.** omp2 has no live-session side-turn API; adding one is a larger change than a fork.
- **Probe on by default, as upstream.** The cost is unmeasured.

## Consequences

- A user can ask a past session why it did something without resuming it. The source journal is byte-identical afterwards.
- The consult forks a journal. Until O1 is decided the fork lands in the sessions directory.
- The probe may be dropped if the measurement shows no cache reuse; D6 says so in advance.

## Slices

One PR each. `cl_seance_enabled` (bool, default false) gates S1-S3; `sv_task_completion_probe` (bool, default false) gates P1-P3. Convar prefixes follow ADR 0012 (`cl_` client, `sv_` session). No `cap-*` feature is needed: no new heavy dependency.

| Slice | Content | Acceptance | Tests |
|---|---|---|---|
| S1 | Resolve and validate a source (`omp-driver`) | Id or path to a journal; saved model from the last `msg.assistant.start`; unknown, ambiguous or unreadable fails closed | Hash of the source unchanged after resolve; torn tail not truncated; unknown id refused; an imported v1 session resolves |
| S2 | Read-only child on a fork | Registry is exactly `read, grep, glob`; no MCP, extensions, skills, spawns | Registry-contents assertion; hash unchanged after a full consult; a write attempt is refused |
| S3 | `/seance [id or path] [--model]` (`omp-chat` command, `omp-app` service like `btw`) | Answer shown in the side panel; absent when the convar is off | Parse table; panel shows the answer; command not in the palette when off |
| P1 | Measurement spike | One fork-based probe of a running child records input tokens, cached tokens and cost; the result is written into this ADR | A number, not a test. If cached tokens are about zero, stop and mark D6 rejected |
| P2 | Estimator (pure) | Parse one JSON line to `pct, stage, watch`; backoff as a pure function | Parser table with oversized and malformed input; schedule test; abort cancels an in-flight probe |
| P3 | Wiring | Depth 0 and interactive only; estimate shown on the hub row | No probe at depth 1 or when off; child journal hash unchanged |

## Open questions

- **O1.** Keep or delete the forked journal after a consult? Recommendation: delete it when the panel closes; the answer is already on screen, and a kept fork would clutter `/resume`.
- **O2.** Does a fork-based probe share the child's prompt cache? The code cannot settle it: it needs the fork's system prompt and tool roster to be byte-identical to the child's. Recommendation: P1 answers it before any P2 work.
- **O3.** Should the model be able to consult a session (a `task@2` revision with `sourceSession`)? Recommendation: no. It is a schema change, and the user command covers the need.
- **O4.** Where is the estimate shown, and does it need an additive job DOM property? Recommendation: the `/hub` row, with one additive property.

Settled by the code, not open: a side turn on a live child is possible without opening its journal, by fork; the saved model is the concrete model in the journal (D2, D4).

## Acceptance tests

1. The source journal's bytes are identical before and after a consult, including when the source has a torn tail.
2. The consult child's registry contains exactly `read`, `grep`, `glob`.
3. With `cl_seance_enabled` off, `/seance` is not offered and the `task` schema is byte-identical to today's.
4. A session with no resolvable saved model fails with the `--model` message.
5. The probe never starts at depth 1, on a non-interactive host, or with the convar off.
6. An unparseable probe reply shows nothing.

## Status in omp

**Not started.** Implemented and reusable: `Journal::scan`, `Kernel::fork`, `KernelOptions.tools`, the `/btw` side panel. Not verified: the cache behaviour of a fork (O2); how the `task` tool resolves agent names for a child other than `task` (`crates/driver/src/subagent/spawn.rs`), which this design avoids.

## Upstream origin and license

Draws on `up:slash-commands/seance.ts`, `task/seance.ts`, `task/seance-policy.ts`, `prompts/agents/seance.md`, `task/completion-probe.ts`, `task/settings.ts`, `task/executor.ts`. License: MIT, "Copyright (c) 2025 Mario Zechner; (c) 2025-2026 Can Bölük; (c) 2026 Stencil Labs, Inc." (upstream `LICENSE`). omp2 re-implements the behaviour in Rust; no TypeScript is carried over. If the seance prompt text is copied, keep the notice and list it in `THIRD-PARTY-NOTICES.txt`.

## References

ADR 0006 (host owns policy; the child runs host-side with a bounded registry); ADR 0012 (convars); ADR 0024 (roster, schemas latched); ADR 0036 (no second runtime); ADR 0049 (no vendor code in core); `crates/journal/src/lib.rs`; `crates/driver/src/headless/kernel.rs`; `crates/app/src/chat_services/session_ops.rs`; research note [subagent-tools](../research/subagent-tools.md).
