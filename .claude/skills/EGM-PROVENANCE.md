# Elephant-Goldfish Model skills: provenance

- Source: https://github.com/google/elephant-goldfish-model
- Commit: `964a4f1ecd333961375c35cb37ed09364dab5e4e` (2026-11-13, "Add experimental
  Windsurf (Devin Desktop) adapter (#379)")
- Licence: Apache License 2.0, copied unchanged as `EGM-LICENSE` (renamed so it is not read
  as the licence of the `omp-*` skills beside it). The source repository has no `NOTICE`
  file at this commit, so none is copied.

## What was copied, unchanged

| Here | Source path |
| --- | --- |
| `egm-brainstorm/`, `egm-draft/`, `egm-resolve/`, `egm-review/`, `egm-review-loop/`, `egm-bootstrap/`, `egm-drift-report/` | `skills/<same name>/` |
| `agents/*.md` (the reviewer personas the skills name, e.g. `zoom_out.md`, `comprehension.md`, `design_critic.md`, `implementation_readiness.md`, `synthesizer.md`) | `agents/` |
| `resources/` (`execution_instructions.md`, `document_standards/*.md`) | `resources/` |

The skills refer to `agents/<name>.md` and `resources/...` relative to the plugin root. Here
that root is `.claude/skills/`. Their `../../docs/codex.md` link is for Codex only and was not
copied.

## Scope

Development tooling for Claude Code sessions working on this repository. Nothing in omp2's
build, tests or runtime reads these files (ADR 0053 O3), and they are not omp2 runtime skills:
they live under `.claude/`, never `.omp/`.
