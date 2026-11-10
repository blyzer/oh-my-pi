# green-loop — affected-test feedback (ADR 0049 D5 rescue)

Records successful core `edit`/`write` paths, runs matched local test commands
after idle or via `/green`, and paints a status badge.

Historical source: `git show 00611047c1^:examples/green-loop/`.

## Surface

- Slash command `/green`
- Hooks: `tool_result` observe, `extension_activate`, `before_agent_start` review
- Settings: `idle`, `timeout`, `test_runner`, `affected`
- Pure helpers: `omp_green_loop.affected`

## Gaps vs frozen API

- Typed `@omp.entry_kind` / `omp.state` were removed. Path/run watermarks now live
  under `omp.state_dir()/green-loop/` (rebuildable cache). Session rewind does
  **not** restore green-loop truth the way journal entries once did — fail closed
  would leave `/green` useless, so the cache adaptation is explicit and documented.
- Status badge / slash command need a UI presentation host; not blocked on ADR
  0046 item 4 (those ports are sidebar/hover/welcome chrome).

## How to test

```sh
PYTHONPATH=extensions/green-loop/src python3 -m unittest discover -s extensions/green-loop/tests -v
```
