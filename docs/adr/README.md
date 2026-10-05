# Architecture Decision Records

Owner decisions for the omp harness, one file per decision. Each record states the forces that
made the decision necessary, the decision itself, and what it commits the codebase to. A record that
is replaced gets a new record and a `Superseded by` line. A record whose decision the owner revises
in place (for example to match what the code does) keeps its number and gets a dated
`## Amendment (YYYY-MM-DD)` section saying what changed and why; the history stays in git.

Source: most records distil "The Harness Playbook" (blog, 2026-09-02) and cross-reference the crates
that implement them. `AGENTS.md` "Locked Deviations from pi" is the enforcement summary; these
records are the reasoning behind it.

## Format

```
# NNNN. Title

Status: accepted | proposed | superseded by NNNN
Date: YYYY-MM-DD
Area: foundations | state | runtime | control-plane | inference | tools | interface | stack

## Context      — the forces, with evidence (what broke in omp v1 / pi, measurements)
## Decision     — what we do, stated as rules; call sites / shapes where useful
## Consequences — what becomes easy, what becomes prohibited, costs accepted
## Amendment (YYYY-MM-DD) — optional; owner-decided in-place revision of the Decision, with why
## Status in omp — crates/files implementing it; gaps marked plainly
## References   — related records, code, external prior art
```

Rules: `Decision` is normative (MUST/NEVER); `Context` is evidence, not opinion; `Status in omp`
names real paths, leads with one status label, and says "not yet implemented" or "unverified" where true.

## Implementation status

[STATUS.md](STATUS.md) is the one-table view of how far each record is implemented, with the gap and
the records where code departs from the decision text. Each record's `## Status in omp` section opens
with `Status: Implemented | Partially implemented | Not started | Superseded or diverged`, then
bullets of evidence (paths, symbols), then divergences. Update the section and its STATUS.md row in the
same change that moves the code.

## Index

### Foundations
- [0001](0001-design-envelope.md) — Four operating modes are the architecture tests
- [0002](0002-complexity-has-one-owner.md) — Push hard problems down into the engine

### State
- [0003](0003-one-authoritative-session-tree.md) — One authoritative session tree, derived from the journal
- [0004](0004-lifecycle-derives-from-the-tree.md) — Rewind, fork, resume, replication, and prompts derive from the tree
- [0005](0005-controller-actor-separation.md) — Controller owns state; views are projections

### Runtime
- [0006](0006-host-policy-sandbox-stub.md) — Policy on the trusted host; an obedient bounded stub in the sandbox
- [0007](0007-subagent-filesystem-isolation.md) — Subagents get a copy-on-write view and return a diff
- [0008](0008-tool-execution-is-a-state-stream.md) — A tool call is one element whose state streams; no three-callback contract
- [0009](0009-bound-output-once.md) — Output is bounded centrally; full results become artifacts
- [0010](0010-one-job-primitive.md) — One job primitive for tools, subagents, daemons, and background work
- [0011](0011-cancellation-needs-a-kill-boundary.md) — Cancellation is a runtime guarantee, not cooperative etiquette
- [0039](0039-remote-control-and-factory-modes.md) — Remote control is a session-control projection; the factory is a leased fleet over the one job primitive (proposed)

### Control plane
- [0012](0012-convars.md) — Settings are convars: policy declared with the variable
- [0013](0013-inheritance-by-seeding-and-cfg.md) — Children seed from the parent; cfg files, not per-setting inherit flags
- [0014](0014-command-stream-binds-and-aliases.md) — Binds, toggles, aliases, and profiles ride the command stream
- [0015](0015-directors.md) — Directors own candidate yields
- [0016](0016-semantic-requests-cross-layers.md) — Directors state intent; inference chooses how to satisfy it
- [0038](0038-stream-rules-as-a-director.md) — Stream rules are a Director over a generic stream-watch hook; a match redirects the response (proposed)

### Inference
- [0017](0017-compatibility-as-structured-knowledge.md) — Model compatibility is compiled knowledge with explicit precedence
- [0018](0018-provider-is-more-than-stream.md) — A provider is shared infrastructure, not a `stream` function
- [0019](0019-forced-tool-call-escalation.md) — Forced tool calls escalate: soft prompt, free flag, then costly flag
- [0020](0020-charitable-argument-repair.md) — Validate the contract strictly, repair the model's dialect charitably
- [0021](0021-constrained-sampling-ownership.md) — Inference owns strict-schema budgets and grammar dialects
- [0022](0022-corrective-inference.md) — An adapter is complete when it yields one canonical turn
- [0023](0023-tiny-local-model.md) — A dedicated tiny model role handles harness chores

### Tool surface
- [0024](0024-small-permanent-roster.md) — Every permanent tool taxes every turn; the roster stays small and fixed
- [0025](0025-long-tail-behind-stable-surfaces.md) — `dyn` and code surfaces carry the long tail
- [0026](0026-intent-and-versioned-tools.md) — Every tool carries `i`; every tool is versioned
- [0027](0027-read-materializes-resources.md) — `Read` materializes any resource; internal URL schemes
- [0028](0028-bash-is-an-in-process-interpreter.md) — `Bash` is a policy-aware in-process interpreter
- [0029](0029-autoqa-report-issue.md) — Agents get a bug-report path

### Interface
- [0030](0030-one-pass-rendering-pipeline.md) — RichText streams through one pass; no `string[]` render
- [0031](0031-typed-component-model.md) — Typed `(Element, Props, Children)` markup for every surface
- [0032](0032-presentation-policy-in-the-renderer.md) — Semantic colors, icons, charset, pacing belong to the renderer
- [0033](0033-verification-is-part-of-the-interface.md) — A debug protocol defines what the UI is
- [0034](0034-transcript-is-a-protocol.md) — Blocks, exactly-once history, append-only scrollback; TLA+-checked
- [0037](0037-acp-editor-io.md) — ACP editors supply the document base; writes commit through the authority, then sync back

### Stack
- [0035](0035-rust-for-the-engine.md) — Language choice is architecture; Rust for the engine
- [0036](0036-python-for-extensions.md) — Embedded Python for extensions, `@remote`, and `Eval`
