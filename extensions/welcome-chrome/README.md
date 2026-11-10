# welcome-chrome — startup header chrome (ADR 0046 item 4)

Mounts a retained `Slot.HEADER` document with tips and activity state, offers
recent sessions through native `ui.select`, and sets a sanitized title via
`ui.set_title`. Never enters terminal modes.

Historical source: `git show 00611047c1^:examples/welcome-chrome/`.

## Surface

- Hooks: `session_start` / `agent_start` / `agent_end` / `session_shutdown`
- `ui.mount(Slot.HEADER, …)` + `ui.set_title`
- `omp.sessions.list` / `resume` through the native picker

## Gaps vs frozen API

None.

## How to test

```sh
PYTHONPATH=extensions/welcome-chrome/src python3 -m unittest discover -s extensions/welcome-chrome/tests -v
```
