# workflows — declarative subagent DAGs (ADR 0049 D5 rescue)

Runs or resumes a budgeted subagent DAG in atomic topological waves via soft
device `workflow`.

Historical source: `git show 00611047c1^:examples/workflows/`.

## Surface

- Device `workflow@1` (family `agents`)
- Settings: `workflow` JSON (`name`, `nodes`, `edges`)
- Pure helpers: `omp_workflows.dag` (topo waves, fingerprint, parse)

## Gaps vs frozen API

- Typed journal receipts (`@omp.entry_kind` / `journal.append`) were removed.
  Resume state is a rebuildable cache under `omp.state_dir()/workflows/`. It is
  **not** session-journal truth across rewind/compaction.

## How to test

```sh
PYTHONPATH=extensions/workflows/src python3 -m unittest discover -s extensions/workflows/tests -v
```
