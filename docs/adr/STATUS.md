# ADR implementation status

Verified 2026-10-04 against `omp2` at `083b38fe7d` by reading code, protos, cvars and `git log` (no builds or test runs). Each ADR's own `## Status in omp` section holds the evidence behind its row. ADRs 0003, 0006, 0007, 0023 and 0028 were amended the same day and re-verified against `omp2` at `9b2d91fe9d`.

Statuses: **Implemented** (decision realized in code; remaining limits are listed in the ADR), **Partially implemented** (named parts missing), **Not started** (no ADR is in this state), **Superseded or diverged** (no ADR is classified wholly this way). Where code departs from part of a decision, the last column says `Yes`: the owner has to choose whether to change the code or the ADR. `Decided 2026-10-04` means the owner chose and the ADR's decision text now states the behavior in force, with an `## Amendment (2026-10-04)` section recording the change; any remaining gap is named in the row.

| ADR | Title | Status | Gap or divergence | Owner decision |
| --- | --- | --- | --- | --- |
| [0001](0001-design-envelope.md) | Four operating modes are the architecture tests | Partially implemented | Remote-driver and factory modes unmet: no network session attach, no fleet, ADW not journaled (see 0039). | - |
| [0002](0002-complexity-has-one-owner.md) | Push hard problems down into the engine | Implemented | Review-enforced; see 0035 for where discipline still relies on review. | - |
| [0003](0003-one-authoritative-session-tree.md) | One authoritative session tree; the journal is its patch stream | Implemented | Amended: the journal's 12 typed entry kinds (`patch@1` is one) are the decision; the invariant is one authority, a derived tree, deterministic replay. | Decided 2026-10-04 |
| [0004](0004-lifecycle-derives-from-the-tree.md) | Rewind, fork, resume, replication, and prompts derive from the tree | Implemented | Unverified: several app/driver readers call `Journal::scan` directly. | - |
| [0005](0005-controller-actor-separation.md) | Controller owns state; views are projections | Implemented | No web client exists in the tree (ADR lists one as a peer). | - |
| [0006](0006-host-policy-sandbox-stub.md) | Policy on the trusted host; an obedient bounded stub in the sandbox | Partially implemented | Amended: the eval parent bridge is a bounded exception and the minimized remote stub is deferred to 0039. Amended again: eval-level approval is the unit of prompts; nested `tool.<name>()` calls obey explicit `deny` and the invoking request's roster restrictions (Plan, Vibe, `sv_tools`, hook `enabled_tools`, Plan's plan-file scope) without prompting. Open: per-call admission, hooks and roster scope for subagent children (see the ADR). | Decided 2026-10-04 |
| [0007](0007-subagent-filesystem-isolation.md) | Subagents get a copy-on-write view and return a diff | Partially implemented | Amended: only per-file reflink with copy fallback exists; other backends are future targets. Open: gitignored files are not copied; the inert `sv_task_isolation_mode` convar awaits a code cleanup. | Decided 2026-10-04 |
| [0008](0008-tool-execution-is-a-state-stream.md) | A tool call is one element whose state streams; no three-callback contract | Implemented | None found. | - |
| [0009](0009-bound-output-once.md) | Output is bounded centrally; full results become artifacts | Implemented | None found. | - |
| [0010](0010-one-job-primitive.md) | One job primitive for tools, subagents, daemons, and background work | Implemented | None found. | - |
| [0011](0011-cancellation-needs-a-kill-boundary.md) | Cancellation is a runtime guarantee, not cooperative etiquette | Implemented | None found. | - |
| [0012](0012-convars.md) | Settings are convars: policy declared with the variable | Implemented | Scope and client-up replication are not per-variable flags. | - |
| [0013](0013-inheritance-by-seeding-and-cfg.md) | Children seed from the parent; cfg files, not per-setting inherit flags | Implemented | None found. | - |
| [0014](0014-command-stream-binds-and-aliases.md) | Binds, toggles, aliases, and profiles ride the command stream | Partially implemented | No remote channel for raw console lines; RPC exposes typed verbs that run single assignments. | - |
| [0015](0015-directors.md) | Directors own candidate yields | Implemented | None found. | - |
| [0016](0016-semantic-requests-cross-layers.md) | Directors state intent; inference chooses how to satisfy it | Implemented | None found. | - |
| [0017](0017-compatibility-as-structured-knowledge.md) | Model compatibility is compiled knowledge with explicit precedence | Implemented | None found. | - |
| [0018](0018-provider-is-more-than-stream.md) | A provider is shared infrastructure, not a `stream` function | Implemented | None found. | - |
| [0019](0019-forced-tool-call-escalation.md) | Forced tool calls escalate: soft prompt, free flag, then costly flag | Implemented | None found. | - |
| [0020](0020-charitable-argument-repair.md) | Validate the contract strictly, repair the model's dialect charitably | Implemented | None found. | - |
| [0021](0021-constrained-sampling-ownership.md) | Inference owns strict-schema budgets and grammar dialects | Partially implemented | Budget and grammar-dialect fallbacks not proved end to end; declaration-time `on_unsupported` error unverified. | - |
| [0022](0022-corrective-inference.md) | An adapter is complete when it yields one canonical turn | Implemented | None found. | - |
| [0023](0023-tiny-local-model.md) | An embedded tiny model handles harness chores | Partially implemented | Amended: the `tiny` role resolves to a configured (default online) model; an in-process generator is a future option. Open: only speech rewriting has a caller; stale comments and caller-less helpers await a code cleanup. | Decided 2026-10-04 |
| [0024](0024-small-permanent-roster.md) | Every permanent tool taxes every turn; the roster stays small and fixed | Partially implemented | Roster varies mid-session (`sv_tools`, Goal, `think`, `task` ceiling); decision rule 2 says it never does. | Yes |
| [0025](0025-long-tail-behind-stable-surfaces.md) | `dyn` and code surfaces carry the long tail | Partially implemented | No Eval `dyn` binding; Bash does not parse kitty/sixel passthrough into attachments. | - |
| [0026](0026-intent-and-versioned-tools.md) | Every tool carries `i`; every tool is versioned | Implemented | None found. | - |
| [0027](0027-read-materializes-resources.md) | `Read` materializes any resource; internal URL schemes | Implemented | None found. | - |
| [0028](0028-bash-is-an-in-process-interpreter.md) | `Bash` is a policy-aware in-process interpreter | Implemented | Amended: approval is one prompt for the denied path or network fact, then a rerun; it exists only when the sandbox is enabled (default off). | Decided 2026-10-04 |
| [0029](0029-autoqa-report-issue.md) | Agents get a bug-report path | Partially implemented | Misattribution filter and an AutoQA enable gate not found. | - |
| [0030](0030-one-pass-rendering-pipeline.md) | RichText streams through one pass; no `string[]` render | Implemented | Prior note's diff-truncation gap not reproduced. | - |
| [0031](0031-typed-component-model.md) | Typed `(Element, Props, Children)` markup for every surface | Partially implemented | No `{#if}`/`{#each}`/`{@render}` in runtime markup; no web projection; `layout!` never existed. | - |
| [0032](0032-presentation-policy-in-the-renderer.md) | Semantic colors, icons, charset, pacing belong to the renderer | Implemented | None found. | - |
| [0033](0033-verification-is-part-of-the-interface.md) | A debug protocol defines what the UI is | Implemented | None found. | - |
| [0034](0034-transcript-is-a-protocol.md) | Blocks, exactly-once history, append-only scrollback; TLA+-checked | Partially implemented | No full/compact/pulse geometry; `cl_resize_policy` convar still exists; TLC not run in CI. | Yes |
| [0035](0035-rust-for-the-engine.md) | Language choice is architecture; Rust for the engine | Partially implemented | About 175 error variants carry a bare `Str`/`String`; error text is formatted in many places. | Yes |
| [0036](0036-python-for-extensions.md) | Embedded Python for extensions, `@remote`, and `Eval` | Partially implemented | `@remote` placement beyond the host, scoped env handles and spill diversion unproved. | - |
| [0037](0037-acp-editor-io.md) | ACP editors supply the document base; writes commit through the authority, then sync back | Implemented | Plan steps 1-4 done (terminal cutover landed in PR #126); moves, deletes and notebooks are not written back. | - |
| [0038](0038-stream-rules-as-a-director.md) | Stream rules are a Director over a generic stream-watch hook | Partially implemented | Steps 1-5 done (`omp rules`, `stream_rule_triggered` rename and emitter, TUI/ACP/print/RPC surfaces, loop-level redirect test). Unproven: matcher proptests, rewind/interrupt/budget rows, P2/P5/P6; notices and the hook carry no matched excerpt. Open questions 1-8 resolved as proposed. | Decided 2026-10-04 |
| [0039](0039-remote-control-and-factory-modes.md) | Remote control is a session-control projection; the factory is a leased fleet over the one job primitive | Partially implemented | Only phase S0 done (PR #129); network session attach, TLS listener, daemon sessions and the fleet are not started. | - |

Totals: 24 implemented, 15 partially implemented.

## Code departing from the decision text (owner decision needed)

- [0024](0024-small-permanent-roster.md) Every permanent tool taxes every turn; the roster stays small and fixed: Roster varies mid-session (`sv_tools`, Goal, `think`, `task` ceiling); decision rule 2 says it never does.
- [0034](0034-transcript-is-a-protocol.md) Blocks, exactly-once history, append-only scrollback; TLA+-checked: No full/compact/pulse geometry; `cl_resize_policy` convar still exists; TLC not run in CI.
- [0035](0035-rust-for-the-engine.md) Language choice is architecture; Rust for the engine: About 175 error variants carry a bare `Str`/`String`; error text is formatted in many places.

## Resolved by amendment (2026-10-04)

The owner brought these decisions in line with the code. Each ADR's decision text was edited and an `## Amendment (2026-10-04)` section records what changed and why.

- [0003](0003-one-authoritative-session-tree.md): the journal's closed typed entry vocabulary (12 kinds, `patch@1` among them) is the decision; the tree stays derived and replay deterministic.
- [0006](0006-host-policy-sandbox-stub.md): the eval parent bridge (`ParentSessionHost`) is a bounded exception to 'the stub never calls the host'; the minimized stub for remote targets is deferred to 0039. Nested native-tool calls from eval obey explicit `deny` and the request's roster restrictions without prompting; eval-level approval stays the unit of prompts, and per-call admission is deferred until route- or session-scoped approvals exist. In production only `__workpool__` is bound; completion, agent, concurrency and budget have no production host.
- [0007](0007-subagent-filesystem-isolation.md): only per-file reflink with a copy fallback is implemented; other backends are future targets and not selectable. Code follow-up: remove the unimplemented `sv_task_isolation_mode` options. Open gap kept in the ADR: gitignored files are not copied.
- [0023](0023-tiny-local-model.md): the `tiny` role resolves to a configured model (default `commit`, then `smol`, an online model); an in-process tiny generator is a future option. Code follow-up: stale comments in `crates/ai/src/local/mod.rs`, caller-less title helpers, unread `ai_*_selector` convars.
- [0028](0028-bash-is-an-in-process-interpreter.md): approval is the sandbox-denial-and-rerun model with a path or network fact as the unit; capability-level, pre-execution approval is not claimed.

## Status changes against the previous notes

- Downgraded from Implemented: 0001 (remote-driver and factory modes only partly built), 0007 (isolation backends not wired), 0014 (no remote console-line channel), 0024 (roster changes mid-session), 0034 (viewport cutover not done, resize convar remains).
- Upgraded from Partial: 0027 and 0030 (the prior notes named no reproducible gap; the unverified item is stated in each).
- 0037: the note said the terminal cutover (step 4) was open; it landed in PR #126 and is verified in the tree.
- 0039: the note said 'not yet implemented'; phase S0 is done (PR #129).
- 0028: upgraded to Implemented by the 2026-10-04 amendment, which states the approval model that exists; the old gap note about network approval was stale and is removed.

`AGENTS.md` ('Control plane') now names the stream-rules Director (0038); the earlier note that it said otherwise is stale.
