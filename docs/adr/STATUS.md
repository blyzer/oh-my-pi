# ADR implementation status

Verified 2026-10-04 against `omp2` at `083b38fe7d` by reading code, protos, cvars and `git log` (no builds or test runs). Each ADR's own `## Status in omp` section holds the evidence behind its row.

Statuses: **Implemented** (decision realized in code; remaining limits are listed in the ADR), **Partially implemented** (named parts missing), **Not started** (no ADR is in this state), **Superseded or diverged** (no ADR is classified wholly this way). Where code departs from part of a decision, the last column says `Yes`: the owner has to choose whether to change the code or the ADR.

| ADR | Title | Status | Gap or divergence | Owner decision |
| --- | --- | --- | --- | --- |
| [0001](0001-design-envelope.md) | Four operating modes are the architecture tests | Partially implemented | Remote-driver and factory modes unmet: no network session attach, no fleet, ADW not journaled (see 0039). | - |
| [0002](0002-complexity-has-one-owner.md) | Push hard problems down into the engine | Implemented | Review-enforced; see 0035 for where discipline still relies on review. | - |
| [0003](0003-one-authoritative-session-tree.md) | One authoritative session tree; the journal is its patch stream | Implemented | Journal has 12 typed entry kinds besides `patch@1`; decision text says no second vocabulary. | Yes |
| [0004](0004-lifecycle-derives-from-the-tree.md) | Rewind, fork, resume, replication, and prompts derive from the tree | Implemented | Unverified: several app/driver readers call `Journal::scan` directly. | - |
| [0005](0005-controller-actor-separation.md) | Controller owns state; views are projections | Implemented | No web client exists in the tree (ADR lists one as a peer). | - |
| [0006](0006-host-policy-sandbox-stub.md) | Policy on the trusted host; an obedient bounded stub in the sandbox | Partially implemented | No separately minimized stub for remote/container/VM; eval parent bridge lets stub-side code call the host. | Yes |
| [0007](0007-subagent-filesystem-isolation.md) | Subagents get a copy-on-write view and return a diff | Partially implemented | `sv_task_isolation_mode` backends (overlayfs, btrfs, zfs, ...) have no implementation; only per-file reflink with copy fallback exists. | Yes |
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
| [0023](0023-tiny-local-model.md) | An embedded tiny model handles harness chores | Partially implemented | No in-process tiny text generator; default is the `online` role; no caller uses the title validator. | Yes |
| [0024](0024-small-permanent-roster.md) | Every permanent tool taxes every turn; the roster stays small and fixed | Partially implemented | Roster varies mid-session (`sv_tools`, Goal, `think`, `task` ceiling); decision rule 2 says it never does. | Yes |
| [0025](0025-long-tail-behind-stable-surfaces.md) | `dyn` and code surfaces carry the long tail | Partially implemented | No Eval `dyn` binding; Bash does not parse kitty/sixel passthrough into attachments. | - |
| [0026](0026-intent-and-versioned-tools.md) | Every tool carries `i`; every tool is versioned | Implemented | None found. | - |
| [0027](0027-read-materializes-resources.md) | `Read` materializes any resource; internal URL schemes | Implemented | None found. | - |
| [0028](0028-bash-is-an-in-process-interpreter.md) | `Bash` is a policy-aware in-process interpreter | Partially implemented | Approval unit is a path/network fact after a sandbox denial, not a capability such as git push. | Yes |
| [0029](0029-autoqa-report-issue.md) | Agents get a bug-report path | Partially implemented | Misattribution filter and an AutoQA enable gate not found. | - |
| [0030](0030-one-pass-rendering-pipeline.md) | RichText streams through one pass; no `string[]` render | Implemented | Prior note's diff-truncation gap not reproduced. | - |
| [0031](0031-typed-component-model.md) | Typed `(Element, Props, Children)` markup for every surface | Partially implemented | No `{#if}`/`{#each}`/`{@render}` in runtime markup; no web projection; `layout!` never existed. | - |
| [0032](0032-presentation-policy-in-the-renderer.md) | Semantic colors, icons, charset, pacing belong to the renderer | Implemented | None found. | - |
| [0033](0033-verification-is-part-of-the-interface.md) | A debug protocol defines what the UI is | Implemented | None found. | - |
| [0034](0034-transcript-is-a-protocol.md) | Blocks, exactly-once history, append-only scrollback; TLA+-checked | Partially implemented | No full/compact/pulse geometry; `cl_resize_policy` convar still exists; TLC not run in CI. | Yes |
| [0035](0035-rust-for-the-engine.md) | Language choice is architecture; Rust for the engine | Partially implemented | About 175 error variants carry a bare `Str`/`String`; error text is formatted in many places. | Yes |
| [0036](0036-python-for-extensions.md) | Embedded Python for extensions, `@remote`, and `Eval` | Partially implemented | `@remote` placement beyond the host, scoped env handles and spill diversion unproved. | - |
| [0037](0037-acp-editor-io.md) | ACP editors supply the document base; writes commit through the authority, then sync back | Implemented | Plan steps 1-4 done (terminal cutover landed in PR #126); moves, deletes and notebooks are not written back. | - |
| [0038](0038-stream-rules-as-a-director.md) | Stream rules are a Director over a generic stream-watch hook | Partially implemented | Step 5 open: `ttsr_triggered` emitter, `omp rules` CLI, TUI/ACP/print surfaces; redirect has no loop-level test. | - |
| [0039](0039-remote-control-and-factory-modes.md) | Remote control is a session-control projection; the factory is a leased fleet over the one job primitive | Partially implemented | Only phase S0 done (PR #129); network session attach, TLS listener, daemon sessions and the fleet are not started. | - |

Totals: 23 implemented, 16 partially implemented.

## Code departing from the decision text (owner decision needed)

- [0003](0003-one-authoritative-session-tree.md) One authoritative session tree; the journal is its patch stream: Journal has 12 typed entry kinds besides `patch@1`; decision text says no second vocabulary.
- [0006](0006-host-policy-sandbox-stub.md) Policy on the trusted host; an obedient bounded stub in the sandbox: No separately minimized stub for remote/container/VM; eval parent bridge lets stub-side code call the host.
- [0007](0007-subagent-filesystem-isolation.md) Subagents get a copy-on-write view and return a diff: `sv_task_isolation_mode` backends (overlayfs, btrfs, zfs, ...) have no implementation; only per-file reflink with copy fallback exists.
- [0023](0023-tiny-local-model.md) An embedded tiny model handles harness chores: No in-process tiny text generator; default is the `online` role; no caller uses the title validator.
- [0024](0024-small-permanent-roster.md) Every permanent tool taxes every turn; the roster stays small and fixed: Roster varies mid-session (`sv_tools`, Goal, `think`, `task` ceiling); decision rule 2 says it never does.
- [0028](0028-bash-is-an-in-process-interpreter.md) `Bash` is a policy-aware in-process interpreter: Approval unit is a path/network fact after a sandbox denial, not a capability such as git push.
- [0034](0034-transcript-is-a-protocol.md) Blocks, exactly-once history, append-only scrollback; TLA+-checked: No full/compact/pulse geometry; `cl_resize_policy` convar still exists; TLC not run in CI.
- [0035](0035-rust-for-the-engine.md) Language choice is architecture; Rust for the engine: About 175 error variants carry a bare `Str`/`String`; error text is formatted in many places.

## Status changes against the previous notes

- Downgraded from Implemented: 0001 (remote-driver and factory modes only partly built), 0007 (isolation backends not wired), 0014 (no remote console-line channel), 0024 (roster changes mid-session), 0034 (viewport cutover not done, resize convar remains).
- Upgraded from Partial: 0027 and 0030 (the prior notes named no reproducible gap; the unverified item is stated in each).
- 0037: the note said the terminal cutover (step 4) was open; it landed in PR #126 and is verified in the tree.
- 0039: the note said 'not yet implemented'; phase S0 is done (PR #129).
- 0028: the prior gap 'no distinct network-request approval' is stale; the real departure is the approval model.

`AGENTS.md` ('Control plane') still says no stream-rule Director exists (0038); that is outside `docs/adr` and not changed here.
