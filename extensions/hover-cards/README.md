# hover-cards — declarative card chrome (ADR 0046 item 4)

Renders a focusable transcript card with host-owned `hover` / `lift` /
`anim` props, plus an `omp.ui.overlay` detail document. No terminal writes.

Historical source: `git show 00611047c1^:examples/hover-cards/`.

## Surface

- Soft device `hover_card@1` (family `hover-card`)
- `@omp.renderer` fold with declarative hover/lift props
- `detail_overlay` / `open_detail` via `omp.ui.overlay`

## Gaps vs frozen API

- An id-bearing focusable `<box>` is documented as emitting activation on
  click or Enter (`docs/py/07-ui.md`), but the frozen Python layer exposes no
  transcript activation payload, decorator, registration table, or dispatch
  callback. `open_detail()` is complete and smokeable, but the card's emitted
  activation cannot be connected to it without inventing an API. This is a
  frozen-vs-docs defect, not replaced with a raw-input or shortcut shim.

## How to test

```sh
PYTHONPATH=extensions/hover-cards/src python3 -m unittest discover -s extensions/hover-cards/tests -v
```
