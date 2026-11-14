---
name: omp-pre-pr
description: Use on a slice's diff before opening its PR (and before pushing fixes to one) to run this repo's real just checks, review the diff against AGENTS.md editing and error discipline, the naming and additive-only rules, and write the PR body with commands, exact results and what was not verified.
---

# omp pre-PR checklist

Sources: `justfile`, `AGENTS.md`, `tools/lintx/README.md`, `docs/adr/0035-rust-for-the-engine.md`,
`docs/adr/0049-tiered-integration-architecture.md`, `.github/workflows/ci.yml`,
`docs/BRANCH_PROTECTION.md`, brief `docs/research/upstream-gap-closure-brief.md`.

Base is `origin/omp2`. Diff: `git diff --stat origin/omp2...HEAD`, changed files
`git diff --name-only origin/omp2...HEAD`. Use `just`, not raw cargo (`AGENTS.md` "Commands").
One concern per tool call: each command below is its own call, never chained with an edit
(`AGENTS.md` "Editing Discipline").

## 1. Checks to run (record each command and its exact result)

| When | Recipe | What it gates |
|---|---|---|
| always | `just fmt-check` | `cargo fmt --check` + buf format; CI `Rust format`. Fix with `just fmt-rust` as its own call; never hand-format (`AGENTS.md` Style) |
| always | `just lint` | fmt-check, clippy `--workspace --all-targets`, proto-lint, lint-locked-maps, lintx-ratchet, lint-editing-discipline (`justfile:210`) |
| Rust changed | `just lintx <changed .rs paths>` | default lintx rules (long-path, std-path, tokio-path, relative-path, import-alias, arc-struct, mutex-arc, model-gate, model-table; `tools/lintx/src/main.rs:1-12`). Not a CI gate: any finding on a line the diff added is new and must go |
| always | `just lintx-ratchet` | ADR 0035 error ratchet per crate vs `tools/lintx/baselines/error-formatting.toml`; CI `Error-formatting ratchet (lintx)` |
| sites migrated | `just lintx-ratchet-update` | lowers the baseline; commit it with the migration. Raising it by hand is a review-reject (`tools/lintx/README.md`) |
| per changed crate | `just check-pkg <pkg>` then `just test-pkg <pkg>` | nextest plus `cargo test --doc`; never nextest alone (`AGENTS.md` "Runtime/Tooling") |
| Linux proof from macOS | `just remote-test-pkg <pkg>` | BuildBuddy runner over the checkout (`AGENTS.md:90-94`) |
| features, `Cargo.toml`, new crate | `just check-minimal-caps` | default `omp-app`, all `cap-*` off, capability crates absent from `omp-envd`; CI `Lint workspace (Linux)` |
| deps added | `just license-check` | `cargo deny` licences and sources; CI `License policy and release notices` |
| `.proto` changed | `just proto-breaking` | wire compatibility against `omp2` |
| `tools/lintx` changed | `just lintx-test` | lintx's own tests (separate workspace) |
| `extensions/` changed | `just test-extensions` | extension conformance |
| workflow or CI paths | `just check-ci-paths` | `ci.yml` vs `.github/ci-paths.txt` |
| user-visible / joined behaviour | the owning `just e2e-*` recipe (`e2e-core`, `e2e-p7`, `e2e-p9`, `e2e-p10`, `e2e-p11`, `e2e-p12`, `e2e-p14`, `e2e-tool-sources`) | proofs in `crates/e2e/tests`; `just e2e` runs them all |
| before the PR, broad | `just check`, `just test` | workspace typecheck; workspace tests excluding e2e |
| TUI | real PTY via `.omp/tools/tui.ts` | input, resize, quit cleanup (`AGENTS.md` "Testing & QA") |

`just setup-python` must have run once before anything links `omp-py` (`AGENTS.md` Commands).
Order: narrow test, affected crate, relevant e2e (`AGENTS.md:825`). A failing check is fixed, not
skipped; compiling alone is not done (`AGENTS.md` "Verification policy").

## 2. Diff review

**Editing discipline** (`AGENTS.md` "Editing Discipline (CRITICAL)"). Files changed only through
the editing tools (Claude Code: Edit, Write); no script that rewrites a tracked file, even inside a
build or test pipeline; bulk renames go to `sonic`. No skill or prompt text in the diff may show a
script-patch recipe (`just lint-editing-discipline` scans `AGENTS.md`, `docs`, `.omp`, `.claude`
and crate READMEs). Never revert or check out user edits (`AGENTS.md` "Working Style").

**Error discipline** (ADR 0035 Decision 2; `AGENTS.md` "Composition/errors/state"):
- library errors are `thiserror`, every variant `#[error("...")]`; no hand-written `impl Display` or
  `impl Error` on an error;
- no string payloads: no `Variant(Str)`, `Variant(String)`, `reason: Str` catch-alls; static
  messages are unit variants;
- no formatted errors: no `sf!("...: {error}")`, `format!`, `{error:?}`, `.to_string()` on an error;
  carry the inner error with `#[source]`/`#[from]` and identifying facts as named fields; render
  once at the app/miette boundary;
- no boxing to silence `result_large_err` / `large_enum_variant` (`AGENTS.md` "Type Size Discipline").
- A genuine render-once site may carry `// lintx-allow: error-format <reason>`, with the reason.

**Naming** (brief "Hard constraints"; `AGENTS.md` Conventions and Crates):
- no `pi` identity anywhere new: no `pi-*` crates, `pi_*` modules, `PI_*` env vars, `.pi` paths,
  pi-era settings keys, or `pi` in messages; scan added lines:
  `git diff -U0 origin/omp2...HEAD | grep -nE '^\+.*(\bpi[-_]|\bPI_|\.pi\b)'`;
- crates `crates/<dir>` with package `omp-<dir>`, workspace `version`/`edition`/`[lints]`, a
  README, a `description`, exactly one `area/*` entry in `.github/labeler.yml`;
- deps only via root `[workspace.dependencies]`, `{ workspace = true }`, never pinned;
- convars `sv_*`/`cl_*`/`ai_*` (`crates/con/README.md:48`); features `cap-*`; env vars `OMP_*`.

**Additive only** (brief "Hard constraints"). Read every removed or changed line (`^-`) in:
`crates/tools/src/roster.rs` (families, `name@rev`), `crates/tools/url-vocab.json` (URL schemes),
`omp_con::var!` declarations (convar names, types, defaults, flags), `crates/contract/src/hooks.rs`
(hooks, revisions), `crates/app/src/cli.rs` (CLI commands), skill discovery
(`crates/driver/src/discovery/skills.rs`) and `.omp/skills`, and any proto field. Any change to an
existing name, schema, revision or default is out of scope: stop the gap and record it as an ADR
open question. With the gate at its default, existing tests must pass unchanged.

**Architecture rules** that reviewers reject (`AGENTS.md` "Locked Deviations from pi", Conventions):
vendor code only in `extensions/` (ADR 0049 D1); no new roster family without an ADR 0024
justification; no model-name conditionals or hardcoded model tables in `.rs` (lintx `model-gate`
and `model-table` scan only `crates/inference/src`, which no longer exists, so check by hand);
typed serde/prost structs, no `json!` traversal; derived strum, no hand-written enum string tables;
`parking_lot` locks, `flume` channels; every public symbol documented.

**Records**: ADR `Status in omp`, its `docs/adr/STATUS.md` row and `docs/adr/TASKS.md` updated in
the same PR when code moves; ledger row in `docs/research/upstream-gap-closure.md` at gap close;
upstream origin in the ADR and the crate `NOTICE` / `THIRD-PARTY-NOTICES.txt` where the licence
requires (brief "Hard constraints").

## 3. PR body

The repo has no PR template (`.github/` holds none). Use `AGENTS.md` "Final report" (lines 789-798):
- **Summary**: the slice id and ADR (`ADR NNNN S2`), what changed and why, the gate and its default.
- **Changed files**.
- **Decisions and tradeoffs**, including review findings acted on.
- **Tests run**: every command from section 1 with its exact result (pass/fail, counts such as
  "omp-memory: 27 passed"), and where it ran (local macOS, remote Linux).
- **Not verified**: what was not run or cannot run here (macOS-only P12, no Windows job, a
  BuildBuddy key not available), intentionally skipped work, remaining blockers and risks.

Commit and PR trailers: the attribution lines the session specifies. One PR per slice; never
force-push, rewrite history or merge to `omp2` yourself (brief "Autonomy").
