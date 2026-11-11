# pr-review — parallel model-tiered PR review (ADR 0049 D5 rescue)

Python package under `extensions/` (not a Cargo member). Soft `review` device
spawns one budgeted subagent per configured area over a bounded `git diff`,
validates findings, and publishes comments through `review/publish` behind a
Core-owned approval ticket.

Historical source: `git show 00611047c1^:examples/pr-review/`.

## Surface

- Device `review@1` / subtool `review/publish`
- Settings: `areas` (model tier + hard budget per area)
- Pure helpers: `omp_pr_review.findings` (schema, parse, comment)

## Gaps vs frozen API

- `Device.subtool` still accepts only `name` (no place/effects overrides). The
  APPROVAL hook gates `review/publish` independently.
- Progress rail uses `ui.Slot.SIDEBAR_RIGHT` (incidental UI, not a UI-bound
  port waiting on ADR 0046 item 4). Manifest capabilities list envd DATA grants
  only (`env.exec`, `env.blob`); spawn/UI are host features.

## How to test

```sh
PYTHONPATH=extensions/pr-review/src python3 -m unittest discover -s extensions/pr-review/tests -v
```
