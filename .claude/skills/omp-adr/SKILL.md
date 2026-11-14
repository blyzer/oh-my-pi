---
name: omp-adr
description: Use when a gap is classified c (port) or d (propose), or any design needs a record, to write a new ADR under docs/adr/ in this repo's format, number it, run and record the EGM review, and add its STATUS.md row (and TASKS.md entry when code moves).
---

# omp ADR

Sources: `docs/adr/README.md` (format and rules), `docs/adr/STATUS.md`, `docs/adr/TASKS.md`, the
brief `docs/research/upstream-gap-closure-brief.md` ("Method" steps 5-6, "Design review"), and the
gap-era records 0053-0071 as worked examples.

## Number

- Next free number = highest `NNNN-*.md` prefix in `docs/adr/` + 1:
  `ls docs/adr | grep -o '^[0-9]\{4\}' | sort -u | tail -1`. At omp2 `9cb6fd6389` the highest is
  0071 and 0001-0071 has no gap, so the next is 0072.
- Not every numbered file is a record: `0049-ci-dyn-device-schema.md` and
  `0049-forge-dyn-device-schema.md` are ADR 0049 companion schemas; `docs/adr/0034/` is a directory.
- Also check the ledger's ADR column (`docs/research/upstream-gap-closure.md`) for a number already
  claimed by a gap whose file is not yet written.
- File name: `NNNN-<kebab-slug>.md`, ideally the gap's research slug.

## Header (README "Format"; recent practice)

```markdown
# NNNN. <Decision stated as a sentence>

Status: proposed
Date: YYYY-MM-DD
Area: <area>

Research: [docs/research/<slug>.md](../research/<slug>.md) (Gap N of the upstream gap closure). Upstream pin `c308970263` (v18.20.0); omp2 base `origin/omp2 <sha>`. Citations: `upstream:` is the OMPv1 pin; bare paths are omp2. <What was read, built, run.>
```

- README vocabulary: `Status: accepted | proposed | superseded by NNNN`. Of 0051-0071, most write
  `Status: proposed`; 0054, 0055, 0059 and 0062 write `Status: Proposed 2026-11-13`; 0050 writes
  `accepted by the owner on 2026-11-11, with conditions (...)`. Use `proposed`; the date is on the
  `Date:` line and the review state goes in STATUS.md.
- README Areas: `foundations | state | runtime | control-plane | inference | tools | interface | stack`.
  Gap ADRs also used `ai` (0059), `ui` (0066), `memory` (0068). Prefer a README area.

## Sections, in the order the gap ADRs use (0054, 0062, 0071)

1. `## Context` - evidence, not opinion (README "Rules"): what upstream does (`upstream:path:line`),
   what omp2 has (`path:line`), the rules that bind (0071 "### Rules that bind").
2. `## Decision` - normative MUST/NEVER (README "Rules"), as bold numbered items `**D1. ...**`.
   Name every new convar, feature, crate and file. State gates: default-off convar for behaviour,
   `cap-*` only for build weight (see `omp-slice-plan`).
3. `## Alternatives considered` / `## Rejected options` - table `| Option | Why not |`.
4. `## Slices` - one PR each, with gate and acceptance tests (format in `omp-slice-plan`).
5. `## Upstream origin and license` - files drawn from, licence with `upstream:LICENSE:N`
   copyright lines, NOTICE / `THIRD-PARTY-NOTICES.txt` needs (brief "Hard constraints": record the
   upstream origin of each port in its ADR and NOTICE where required).
6. `## Consequences` - what becomes easy, prohibited, costs accepted (README "Format").
7. `## Open questions for the owner` - `**O1. Question?**` plus `Recommendation:`. An
   additive-rule conflict lands here and stops that gap (brief "Hard constraints"). A gap ADR
   records, it does not edit other ADRs (0071 Consequences: "This record does not edit ADR 0007").
8. `## What the review changed` - see below.
9. `## Acceptance tests` - numbered, each naming its slice, observable and checkable.
10. `## Status in omp` - opens with exactly one label (README "Implementation status"):
    `**Status: Implemented | Partially implemented | Not started | Superseded or diverged.**`, then
    evidence bullets with real paths and symbols, then divergences; say "not yet implemented" or
    "unverified" where true (README "Rules").
11. `## References` - related ADRs (always the ones checked: 0006, 0012, 0024, 0035, 0036, 0049
    as relevant), code paths, research note.

Revising an existing record in place needs the owner: a dated `## Amendment (YYYY-MM-DD)`;
replacing it is a new record plus a `Superseded by` line (README, opening paragraph).

## EGM review (brief, "Design review: Elephant-Goldfish Model")

- Scope: classes c and d. Skip for a and b and for data or small mechanical gaps (6, 7, 8, 15).
- Reviewers are fresh subagents given only the ADR path and the files it cites; never a fork, never
  your own context. Personas (ADR 0053 D7): zoom-out, comprehension, design critic, implementation
  readiness, then a synthesis pass. Vendored files: `.claude/skills/agents/{zoom_out,comprehension,
  design_critic,implementation_readiness,synthesizer}.md`; loop: `.claude/skills/egm-review-loop/`.
- Verify every finding against source before accepting or rejecting it; cite `path:line` either way.
  At most three rounds. A blocker still open after round three: stop at the ADR, list it for the
  owner, move on. No fresh contexts available: mark the ADR "unreviewed" and do not implement.
- Clean review: implement only behind a default-off gate and record that the design was
  machine-reviewed and not owner-approved.

`## What the review changed` (brief, Method step 6) lists each finding, whether it was confirmed
against source, and what changed. Shape it like 0054's `## Review (EGM, 2026-11-13)`: one line on
reviewers and verdict, then bullets `Accepted unchanged`, `<Finding>, verified in this tree:
<path:line>; resolved by Dn`, `Corrected against this tree`, `Not settled` (-> open questions),
and the line "Machine-reviewed (EGM), not owner-approved."

## STATUS.md row

Columns (`docs/adr/STATUS.md:11`):
`| ADR | Title | Status | Gap or divergence | Owner decision |`. Insert in number order after the
last row (0071 at line 83), before the `Totals:` line. Shape used by 0054-0071:

```markdown
| [NNNN](NNNN-<slug>.md) | <short title> | Proposed YYYY-MM-DD; reviewed (EGM)[: <verdict>]; not started | <what is missing today> | Open: O1 <topic>, O2 <topic> |
```

- Review state wording in use: `reviewed (EGM)`, `reviewed (EGM): changes required`,
  `reviewed (EGM): research-only confirmed`, `EGM review in progress`; use `unreviewed` when no
  Goldfish review ran (brief).
- Implementation labels come from line 7: Implemented, Partially implemented, Not started,
  Superseded or diverged. Owner column: `-`, `Open: ...`, or `Decided YYYY-MM-DD`.

## When TASKS.md changes

- `docs/adr/TASKS.md` is the active queue for unresolved ADR work and owner decisions
  (README "Implementation status"). Its vocabulary: DONE, OPEN, BLOCKED, PARTIAL, HISTORICAL
  (TASKS.md "Authority").
- A docs-only proposal adds the ADR and its STATUS.md row (brief step 5); 0054-0071 added no
  TASKS.md entry.
- When a slice moves code: update the ADR's `Status in omp`, its STATUS.md row and TASKS.md in the
  same change (README "Implementation status"; TASKS.md "Validation rule for future sessions").
- An owner decision that blocks implementation becomes a BLOCKED item in TASKS.md.
- At gap close also update the ADR status and append or update the ledger row (brief step 9).
