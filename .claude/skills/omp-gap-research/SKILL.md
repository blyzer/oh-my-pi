---
name: omp-gap-research
description: Use when starting or resuming an upstream gap (OMPv1 c308970263 to omp2) from docs/research/upstream-gap-closure-brief.md, to run the Study, Inventory, Classify and Research steps and write docs/research/<gap>.md in this repo's research-note format.
---

# omp gap research

Development tooling for Claude Code; not an omp2 runtime skill (brief, Step 0 item 5).
Authority order: the brief `docs/research/upstream-gap-closure-brief.md`, then `AGENTS.md`,
`PHILOSOPHY.md` and the ADRs. Re-read the brief and the ledger
`docs/research/upstream-gap-closure.md` at the start of every gap and after every compaction
(brief, "Autonomy"; `.claude/context-after-compact.md` last line).

## Before the first gap

- Resume from the ledger: continue at the first row whose Status is not closed (brief, "Autonomy").
- Upstream is read from git, never from memory (brief, "Autonomy": "Never describe upstream from
  memory"). Check the pin is local: `git cat-file -t c308970263`. If it is missing:
  `git fetch https://github.com/can1357/oh-my-pi main`.
- Read-only commands for upstream:
  - `git show c308970263:<path>` (pipe to `cat -n` for line numbers);
  - `git ls-tree -r --name-only c308970263 -- <dir>`;
  - `git grep -n '<pattern>' c308970263 -- <pathspec>`.
- Survey by delegation, decide personally (`PHILOSOPHY.md` "How to choose" 4). The brief names the
  Explore subagent for the inventory step (brief, "Skills to use, by stage").

## Method (brief, "Method, per gap", steps 1-5)

1. **Study.** Read the upstream implementation at `c308970263` and state in two or three sentences
   what it does and why a user needs it. Read it "in extreme detail", tests included
   (`AGENTS.md` "Porting (from pi)" 1-2).
2. **Inventory.** Search this repo for what already exists: crates, `extensions/`, convars
   (`omp_con::var!` blocks, e.g. `crates/envd/src/host_settings.rs`), hooks
   (`crates/contract/src/hooks.rs`), roster (`crates/tools/src/roster.rs`), URL vocabulary
   (`crates/tools/url-vocab.json`), importer `legacy.path` mappings, ADRs and `docs/research/`.
   Cite files and lines. Say "none" explicitly when nothing exists (see `shell-builtins.md` §2,
   "No importer stub").
3. **Classify** as exactly one of: **a** already covered (name the equivalent; no code change);
   **b** not needed (say why and what the user does instead); **c** port (upstream behaviour in
   omp2's shape); **d** propose (a better design for omp2). When a gap has independent parts, split
   it and give each part one class (practice: ledger rows "c (rsync b)", "a (readers) + c";
   `subagent-isolation.md` "Classes: (a) c ... (b) b").
4. **Research** (c and d only). Compare at least two options, always including "build on existing
   omp2 code" versus "port"; choose one with reasons. Lettered options with cost and fit, then a
   `Choice:` line (`shell-builtins.md` §4; `subagent-isolation.md` §4 uses a table).
5. **Document.** Write `docs/research/<gap>.md` (template below). Classes a and b get the note only;
   c and d continue with the `omp-adr` skill. Update the ledger row in the same commit.

## Constraint check (record it in the note, section 6)

Each is a hard constraint from the brief, "Hard constraints":
- Additive only: no change to behaviour, names, schemas or revisions of existing tools, skills,
  convars, hooks, URL schemes or CLI commands. A gap that needs one stops; the conflict becomes an
  ADR open question.
- New roster family only when no family, `dyn` device or extension can carry it, justified against
  ADR 0024 (`docs/adr/0024-small-permanent-roster.md` Decision 3-5).
- Vendor integrations are extensions, never core (ADR 0049 D1, `0049-tiered-integration-architecture.md:47-52`).
- Deterministic first: rules and native code trigger, gate and extract; a model only interprets.
- Locked decisions are not reopened (ADR 0036 Python-only extensions, ADR 0024 roster, ADR 0006
  sandbox policy; also `AGENTS.md` "Locked Deviations from pi"). A collision is "not adopted" with
  the reason.
- No new language runtime; no `pi` identity (`pi-*`, `pi_*`, `PI_*`, `.pi`, pi-era settings keys);
  `OMP_*` env vars only (`AGENTS.md` Conventions, lines 132-135).
- If an OMPv1 tool was used to study the gap, note it in the note as evidence of the gap's value
  (brief, "Using OMPv1 as a development aid").

## Citation rules (from the existing notes)

- `path:line` or `path:start-end`; a bare `:N` continues the file just cited
  (`mnemopi-fact-extraction.md` lines 3-5).
- Upstream paths carry the prefix `upstream:` and mean the pin `c308970263`; bare paths are omp2.
  Other repos get their own prefix with a commit (`ovnfs@c43f05a:`, `subagent-isolation.md` line 6).
- State the omp2 base commit (`origin/omp2 <sha>`) the inventory was read against.
- Mark anything not read from source `[INFERENCE]`; say plainly what was read but not built, run or
  timed (`docs/adr/0071-subagent-isolation.md` line 7; `PHILOSOPHY.md` "Keep an honest ledger").

## Research-note template

Headings match `docs/research/shell-builtins.md` and `subagent-isolation.md` (`mnemopi-fact-extraction.md`
uses the same seven sections with sub-headings).

```markdown
# Gap N: <title> (`<slug>`)

Scope: <what upstream pieces this covers, with upstream paths>.
Class: <a|b|c|d> (or "Classes: (a) c ... (b) b" for a split gap).
ADR: <NNNN (`docs/adr/NNNN-<slug>.md`) | none (class a/b)>. Upstream pin: `c308970263` (v18.20.0); omp2 base: `origin/omp2 <sha>`.
Citations: `upstream:` is the OMPv1 pin; bare paths are omp2. <What was read only; what was run.>

Answer in brief: <one paragraph: decision per part>.

## 1. Upstream
<Study: what it does and why a user needs it; sizes, features, tests, with upstream:path:line.>

## 2. omp2 inventory
<Bulleted findings with path:line; "none" where nothing exists.>

## 3. Class
<The class per part, with the reason; for b, what the user does instead.>

## 4. Options
<A, B, C ... each with cost and fit; "build on omp2 code" vs "port" always present. Choice: X.>

## 5. Plan
<Draft slices (one PR each), gate, acceptance per slice; superseded by the ADR once written.>

## 6. Open questions
<Numbered, each with a recommendation; ends with the constraint check above.>

## 7. Upstream origin and license
<Files a port draws from; licence and copyright lines (upstream:LICENSE:N); NOTICE needs.>
```

## Ledger row

`docs/research/upstream-gap-closure.md` columns are
`| # | Gap | Class | Research | ADR | Status | PRs |` (line 10). Link the note as
`[<slug>](<slug>.md)` and the ADR as `[NNNN](../adr/NNNN-<slug>.md)`. Keep the ledger current after
every step and commit and push it (brief, "Autonomy"). Stop a gap, not the run, when blocked:
record the blocker in Status and move on.
