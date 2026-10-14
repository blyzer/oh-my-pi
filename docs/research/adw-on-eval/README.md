# ADW on eval: plan, fan out, gate, retry

`adw_on_eval.py` is one omp2 `eval` cell (Python) that does what a small `omp adw` workflow does:
a plan step, a workpool fan-out of writers, a deterministic gate per file, and a bounded retry that
carries the gate's feedback into the next attempt. `greet-files.toml` is the same task declared for
`omp adw run`. `docs/research/adw-on-eval-vs-omp-adw.md` compares them with citations.

The task is deliberately tiny: write `greet.py` and `farewell.py`, each starting with a header
comment `# <file>: <purpose>`, each gated on a check of that first line.

## Status: the eval script has NOT run

**UNVERIFIED.** The pool path of `adw_on_eval.py` was never executed against omp2. The built binary
(`target/debug/omp`, `omp/2.0.0`) refuses every eval cell in a normal session, because the eval
bridge's parent binding is keyed by the journal file stem while the cell's principal is a SHA-256 of
the journal path (`docs/research/adw-on-eval-vs-omp-adw.md`, section "Why the eval script did not
run"). Only its control flow was exercised, against a fake pool, which proves nothing about omp2.
`greet-files.toml` did run under `omp adw run`; that transcript is in the comparison note.

This directory also does not meet `examples/README.md`'s rule that an example "runs against the
current frozen surface" and ships an `omp.toml`. It is a script for the `eval` tool, not an
extension, so an `omp.toml` would describe nothing; and it cannot claim to run. Treat it as a
design sketch checked against the source, not as a working example.

## Run it (once eval cells can bind their parent)

In a session with the eval tool, run the file as a cell:

```python
%run docs/research/adw-on-eval/adw_on_eval.py
```

It needs:

- Python eval enabled (`--py-eval`, or `eval` in the tool roster). By default `dyn eval` is denied by
  `tools.approval` unless a cfg line `sv_tools_approval {eval allow}` allows it.
- The `workpool` helper, which exists only when the session's parent host advertises `__workpool__`
  (`crates/envd/src/eval/python_prelude.py:918`). It uses nothing else: `agent()`, `completion()` and
  `judge` have no production host (`WorkpoolSessionHost`,
  `crates/driver/src/subagent/workpool_scheduler.rs:815-885`), so even the plan step is a pool item.
- A project that is a git repository: workers run in isolated worktrees and their patches are
  applied back on retirement (`crates/driver/src/subagent/workpool_runtime.rs:396,519`).
- A model for the `task` agent. The tests for this note used only
  `omniroute/cohere/command-a-plus-05-2026` and `omniroute/cohere/command-r7b-12-2024`.

## Run the `omp adw` side

```sh
mkdir -p <project>/.omp/workflows && cp greet-files.toml <project>/.omp/workflows/
omp adw run greet-files --project <project> -m omniroute/cohere/command-a-plus-05-2026
```

Code phases run only in a trusted workspace (`crates/driver/src/adw/production.rs:381`). A fresh
`/tmp` repo is untrusted, and the run stops with `workspace is not trusted; adw code phases are
withheld`. Trust it with an exact `[[workspace_trust]]` row in `<data>/ext/grants.toml`, whose
digest binds the workflow file itself. `-m` does not override a user `ai_model` cfg line for model
phases; see the note.

## What it does not guarantee

It has the kernel's authority, no sandboxed gate, no journaled transitions, no resume, and no
posture check. See the comparison note for the full list.
