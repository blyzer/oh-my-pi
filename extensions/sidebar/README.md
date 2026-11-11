# sidebar — retained session rail (ADR 0046 item 4)

Mounts one `Slot.SIDEBAR_RIGHT` rail with live session-tree and usage facts.
Never touches terminal geometry or output.

Historical source: `git show 00611047c1^:examples/sidebar/`.

## Surface

- Hook `extension_activate` seeds the rail
- Tree-scoped telemetry (`session_start` / `turn_*` / `session_end`) patches keyed nodes
- `ui.mount` / `SlotHandle.patch` with `SlotOptions(width=34, min_width=120, …)`

## Gaps vs frozen API

- `omp.telemetry` concrete event types (`SessionStart`, …) are specified in
  `docs/py/10-telemetry.md` but omitted from the frozen export list; the port
  accepts documented event objects as `object` and reads their fields without
  public concrete annotations.

## How to test

```sh
PYTHONPATH=extensions/sidebar/src python3 -m unittest discover -s extensions/sidebar/tests -v
```
