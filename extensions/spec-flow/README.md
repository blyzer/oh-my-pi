# spec-flow — artifact-gated specification workflow (ADR 0049 D5 rescue)

Soft `flow` device with `start` / `advance` / `status` / `archive` subtools.
Phase machine + artifact paths; documents go through `omp.env.docs` leases;
archive spills a blob via `omp.env.blobs.put`.

Historical source: `git show 00611047c1^:examples/spec-flow/`.

## Surface

- Device `flow@1` (family `spec`) + subtools
- Pure helpers: `omp_spec_flow.phases`

## Gaps vs frozen API

- Typed `@omp.entry_kind` / `journal.append` / `append_atomic` were removed.
  Transitions live under `omp.state_dir()/spec-flow/` (rebuildable). A full
  Component + closed-kind DOM rewrite is deferred; this is an explicit Gap, not
  a silent no-op — mutating tools still run, but rewind will not restore phase
  the way journal truth once did.
- `Device.subtool` name-only signature Gap unchanged (typed child bodies still
  dispatch).

## How to test

```sh
PYTHONPATH=extensions/spec-flow/src python3 -m unittest discover -s extensions/spec-flow/tests -v
```
