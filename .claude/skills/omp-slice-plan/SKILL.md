---
name: omp-slice-plan
description: Use after an ADR's review (or for a class c/d gap's plan) to cut the work into one-PR slices with ids, scope, gate, acceptance criteria, the proving tests at the right seam (unit, crate integration, e2e proof, golden journal), dependency order, and what each slice leaves green.
---

# omp slice plan

Sources: brief `docs/research/upstream-gap-closure-brief.md` ("Method" steps 7-8, "Hard
constraints"), `AGENTS.md` ("Testing & QA", "Commands", "Working Style"), the slice sections of
ADRs 0054, 0062 and 0071, `crates/e2e/README.md`, `crates/session/tests/golden_journals.rs`.

## Slice entry

Write the plan in the ADR's `## Slices` section, and add each slice as a todo sub-task (brief
step 7). Two shapes exist: a table `| # | Contents | Gate | Acceptance tests |` (0054, 0062) or
bold `**S1. <title>.**` paragraphs with `Tests:`, `Acceptance:`, `Gate:` bullets (0071). Each
slice states:

- **Id** `S1`, `S2`, ... (0071) and a one-line title. Ids never renumber once a PR cites them.
- **Scope**: files, types, convars, crates touched; what it ports (`upstream:path:line`).
- **Gate**: the convar or feature, its default, and why (below). "None: no caller until Sn" is a
  valid gate only when stated (0062 slice 5; 0071 S1, S2).
- **Tests**: named test functions (0071 S1 lists them), at the seam below.
- **Acceptance**: observable outcomes, gate off and gate on. Gate off must prove nothing changed
  (0062 slice 1: "Gate off: the 12 existing `omp-memory` tests pass unchanged"; 0071 S3: existing
  tests "pass unchanged").
- **Depends on**: earlier slice ids; safety slices precede the slices that rely on them (0054 slice
  2, the write seam, before cp, dd, tar).
- **Verification command**: e.g. `just test-pkg omp-shell-builtins` (0054 Slices); Linux remote
  `just remote-test-pkg <pkg>` (`AGENTS.md` Commands, lines 90-94).

Rule from 0071 Slices: tests are written first and not edited to pass.

## Size and order

- One PR per slice, one branch per gap (brief step 8). A slice touches one owning seam where
  possible; the parent owns shared contracts (`AGENTS.md` "Execution policy").
- Order: gate first (0054 slice 1), then safety seams, then behaviour, then wiring.
- Done means compiles, wired and exercised (`AGENTS.md` "Working Style"; `PHILOSOPHY.md`
  "Finished means running"). An inert slice must still be exercised by its own tests at the crate
  boundary, and the plan names the slice that wires it.

## Gating (additive-only rule)

- Behaviour change lands behind a **default-off convar** until its acceptance tests pass (brief
  step 8; Design review item 4). Declare it once with `omp_con::var!` beside its neighbours (e.g.
  `crates/envd/src/host_settings.rs:67`, `SV_WORKTREE_BASE` at `:139`), flags from
  `crates/con/src/spec.rs` (`ARCHIVE`, `SESSION`, `PROJECT`, ...).
- Names: subsystem prefixes `ai_*`, `cl_*`, `sv_*` (`crates/con/README.md:48`; ADR 0012 itself does
  not define them). In the tree `sv_*` is host or environment policy declared in `crates/envd`
  (`host_settings.rs`, `exec_settings/`); `cl_*` is client presentation (`crates/chat`,
  `crates/driver/src/keybindings`). New gap convars use `sv_*` (ADR 0062 D7, 0054 D2, 0071 S3).
- No `PROJECT` flag on anything a hostile repository could turn into code execution, a weaker
  sandbox or a path the user did not grant (`docs/adr/0012-convars.md:77`; ADR 0054 D2).
- A v1 setting maps through the importer's `"legacy.path"` key (`host_settings.rs:85`; 0071 S3),
  never a `pi` alias name.
- `cap-*` Cargo features gate **build weight**, not behaviour: T1 crates behind `cap-<name>`,
  default-on in `omp-app` (ADR 0049 D1 table and D3.5; `crates/app/Cargo.toml` `default`). Add one
  only for new dependency weight (0054 D8 `cap-shell-archive`; 0071 S1 "It is not a `cap-*`
  feature"), forwarded through `omp-envd` and `omp-app` Cargo features, and keep
  `just check-minimal-caps` passing.
- Vendor code goes to `extensions/`, never a core crate (ADR 0049 D1).

## Proving tests, by seam (`AGENTS.md` "Testing & QA", lines 800-825)

- **Unit, in `src`**: private behaviour, colocated `#[cfg(test)]`. Proptest for encoding, slicing,
  replay and round-trip invariants; `insta` snapshots for parser output, reviewed, never accepted
  blindly.
- **Crate integration, `crates/<crate>/tests`**: public contracts and cross-module behaviour, at
  the owning seam: `crates/envd` (environment host, extension host, Python worker), `crates/env`
  (client/protocol), `crates/driver` (headless/session composition), `crates/app` (CLI,
  presentation, protocol adapters). Prefer these over mocks of production authority.
- **E2E proof, `crates/e2e/tests`** (`crates/e2e/README.md` "Proofs"): for user-visible or
  joined-system behaviour, extend the proof that owns the property. P1 document leases; P2
  cancellation; P3 detached jobs; P4 schema isolation / `tool.call@1` rev; P5 prompt-band prefix;
  P6 crash/resume; P7 real-PTY TUI and Tern (`p7_tui`, `p7_tsp`); P8 non-gating recorder; P9
  isolated worktrees and extension Directors/Components; P10 tool lift; P11 collaboration; P12
  daemon approvals (macOS, Seatbelt); P14 session attach; `tool_sources` source routing. P13
  (`p13_remote_exec.rs`) is not gated in CI (`AGENTS.md:99-100`). Bounded waits and RAII-owned
  processes (`AGENTS.md:817-818`). A new proof file needs a `just e2e-*` recipe and a `ci.yml` job
  step ("keep it and `just e2e` in step", `AGENTS.md:100-101`).
- **Golden journal**: `crates/session/tests/golden_journals.rs` replays the checked-in corpus
  `fixtures/journal/<name>.oms` + `<name>.snapshot.json` and requires the fold to equal the snapshot.
  A slice that adds a journal kind or payload field adds a family to `corpus()` (the array length is
  `MIN_FIXTURES`) and commits the new pair, written by the ignored test
  `cargo nextest run -p omp-session --test golden_journals --run-ignored ignored-only regenerate_golden_journals`.
  Never regenerate an existing pair: the corpus is append-only (file header, lines 11-13; one
  fixture per durable behaviour family, ADR 0048 B5 in `TASKS.md`).
- **Prompt goldens** (insta): `crates/agent/tests/prompt_golden.rs`,
  `crates/app/tests/snapshots/subagent_prompt_golden__*.snap`, for slices that change model-facing
  prompt text.
- **TUI**: real-PTY proof via `.omp/tools/tui.ts` (or `OMP_TUI_DEBUG` / `OMP_TTY`) for input,
  resize and clean quit, every slice that touches it (`AGENTS.md` "Testing & QA").
- **Platform**: a capability only one OS can prove says so and gives the other OS a fallback test
  (0071 Consequences: macOS proves clone success, Linux the fallback). No Windows job exists.

Coverage means changed behaviour defended: branch edges, precedence, state transitions,
malformed input, cancellation, recovery; no numeric target (`AGENTS.md:823-825`). Order: narrow
test, affected crate, relevant e2e proof.

## What every slice leaves green

- `just lint` (fmt-check, clippy, proto-lint, lint-locked-maps, lintx-ratchet,
  lint-editing-discipline; `justfile:210`), `just test` (nextest plus doctests), the e2e proofs it
  touches, and `just check-minimal-caps` when features change (brief "Hard constraints"; `justfile`).
- The required checks in `docs/BRANCH_PROTECTION.md` "Required status checks".
- No baseline raised in `tools/lintx/baselines/error-formatting.toml` (`tools/lintx/README.md`).
- The ADR `Status in omp`, its STATUS.md row and TASKS.md updated in the same PR when code moves
  (`docs/adr/README.md` "Implementation status").
- Run `omp-pre-pr` on the slice's diff before opening its PR.
