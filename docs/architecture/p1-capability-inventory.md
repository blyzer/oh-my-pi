# P1 — OMP² capability inventory

Status: classified, not migrated.
Source: `/Users/enverfrancisco/repositories/omp2`, branch `omp2`, `1a6d7cddce`.
Semantic machine: `/Users/enverfrancisco/repositories/semant`, `main` at `13e44c4`.

This inventory does not move code. It says which omp² capability stays, which one the semantic machine must own, and which overlap is unresolved.

## Stage card

Purpose. Stop the harness and the semantic machine from each becoming the other.

Inputs. omp² crate list, `docs/architecture/crates.md`, ADRs 0001–0036, the permanent tool roster in `crates/tools/src/lib.rs`. Semant ADRs 0001–0005 and the 25-point rename.

Outputs. One class per capability below.

State ownership. omp² keeps the session journal, the DOM, and host effects. Semant keeps snapshot ids, evidence, and code facts. Neither store is the other's memory.

Implementation language. Both are Rust. omp² also embeds CPython for extensions (`crates/py`, ADR 0036). Semant ADR 0004 still says a Python extension is a subprocess and that worker does not exist.

Public contract. The model-facing roster stays the one in `BUILTIN_TOOL_IDENTITIES`. A semantic operation does not earn a new permanent tool. ADR 0024 forbids mid-session roster changes.

Dependencies. P14L and P25 wait on this inventory. The semant kernel line does not.

Failure boundary. A semant failure is evidence or a verdict. An omp² failure is a tool verdict, a cancelled process tree, or a journal entry. One must not be rewritten as the other.

Security. omp² owns admission, sandbox, secrets, and redaction. A later `code.*` call lowers into those checks. It does not bypass them.

Tests. No new test in this stage. The classification is wrong if a later change adds a permanent semant tool, or teaches `semant` to call a model.

Invariants. Retrieval in either tree is not semantic truth. Only the semant fact store writes code facts. Only the omp² harness calls a model.

Exit criteria. Every row below has one class. Met by this document.

Deferred. The actual bridge, roster wiring, and any deletion of `grep` / `ast_grep` / `ast_edit`.

## Classification

| Capability | Where it lives now | Class |
|---|---|---|
| Model providers, routes, codecs, catalog | `catalog`, inference crates, ADR 0016–0022 | KEEP IN OMP² |
| Sessions, journal, DOM, replay | `session`, `journal`, `dom` | KEEP IN OMP² |
| Context budget and compaction | `snapcompact`, scribe, session projections | KEEP IN OMP² |
| Permanent tool roster and versioned contracts | `tool`, `tools`, ADR 0024–0027 | KEEP IN OMP² |
| Filesystem materialization (`read`) | `tools` read, ADR 0027 | KEEP IN OMP² |
| In-process shell and process trees | `shell`, `shell-builtins`, ADR 0028 | KEEP IN OMP² |
| Sandbox and path confinement | `sandbox`, envd | KEEP IN OMP² |
| Secrets, redaction, DLP-shaped rules | `secrets`, observability | KEEP IN OMP² |
| Subagents and task spawn | `driver`, `task` tool | KEEP IN OMP² |
| Conversation memory (Mnemopi) | `memory` | KEEP IN OMP² |
| Telemetry and receipts of turns | `observability`, `journal` | KEEP IN OMP² |
| Cancellation and kill boundary | `agent`, ADR 0011 | KEEP IN OMP² |
| Harness checkpoints and rewind | `checkpoint`, `rewind` tools | KEEP IN OMP² |
| User interaction, TUI, desktop, computer use | `tui`, `gui`, `desktop`, `computer` | KEEP IN OMP² |
| Extension host and embedded Python | `ext`, `py`, ADR 0036 | KEEP IN OMP² |
| MCP and other long-tail devices | `dyn` / device bus, ADR 0025 | KEEP IN OMP² |
| Git and Jujutsu operations | `vcs` | KEEP IN OMP² |
| ADW phase graphs | `adw` | RESEARCH |
| Lexical search (`grep`, `glob`) | `tools`, `walker` | REFACTOR |
| Structural search and edit (`ast_grep`, `ast_edit`, `ast`) | `tools`, `omp-ast` | REFACTOR |
| Text edit and write | `edit`, `write` tool | REFACTOR |
| Code facts, SCIP, evidence, query judgment | not in omp²; `semant` | MOVE TO SEMANTIC MACHINE |
| Symbol rename and blob-id apply | not in omp²; `semant` plan/flow | MOVE TO SEMANTIC MACHINE |
| Semantic verification and done-tree | not in omp²; `semant` verify/log | MOVE TO SEMANTIC MACHINE |
| Computed code context (callers, impact) | brief of one hit in `semant`; no omp² equivalent | MOVE TO SEMANTIC MACHINE |
| Embeddings as semantic truth | `memory` isolates embeddings; semant has an `Embedding` method label and no index | RESEARCH |
| BEAM / Gleam control plane | absent in both | REMOVE |
| Glean / Angle service | absent in both | REMOVE |
| A second permanent code-tool roster | would violate ADR 0024 | REMOVE |

KEEP means the harness still owns the mechanism after the bridge exists.
MOVE means semant is the owner and omp² must not grow a second implementation.
REFACTOR means the tool stays as an escape hatch or a lowering target, and a semantic request must not be implemented again inside it.
RESEARCH means the overlap is real and this inventory does not pick a winner.
REMOVE means do not introduce it. Nothing already shipping is deleted by this document.

## Boundary this inventory enforces

omp² may ask `code.query`, `code.explain`, `code.change`, and `code.verify` later. Those names are the ADR 0002 API in semant, and they are not tools in `BUILTIN_TOOL_IDENTITIES` today.

Until that API exists, `grep`, `read`, `edit`, `ast_grep`, and `ast_edit` remain the model's code tools. Replacing them is P14L, after semant can answer more than one Rust rename.

`adw` sequences developer-workflow phases. Semant Flow sequences one rename. They are not the same language. Do not fold Flow into `adw` or `adw` into CSL.

Python stays in omp² as the embedded extension runtime. Semant's subprocess adapter is a different contract and stays unbuilt until an analyzer must run outside that embed.
