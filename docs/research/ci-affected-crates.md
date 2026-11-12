# Affected-crates test scope (opt-in)

By default a pull request that touches Rust runs every test in the workspace. `CI_AFFECTED_ONLY` narrows the **test execution** steps of the `rust` job to the crates a change can reach. It is off until you set the repository variable, and it never applies to a push to `omp2`, a `merge_group` run or a manual dispatch, which always run the full suite.

## How it works

`scripts/affected-crates.py` reads `cargo metadata --locked`, maps the changed files (`git diff --name-only --no-renames BASE...HEAD`) to workspace crates, and adds every reverse dependency (normal, build and dev). It also treats a crate as a dependent of another when its Rust sources name that crate's directory (`../../proto/`, `"crates", "py"` and similar), because several crates read each other's files at run time and Cargo does not see that. It reports **all** crates when a change touches `Cargo.toml` (root), `Cargo.lock`, `rust-toolchain.toml`, `.cargo/`, `.config/`, `rustfmt.toml`, `clippy.toml`, `deny.toml`, `about.toml`, `fixtures/`, `examples/`, `extensions/`, `tools/` or `docs/py/`. A path that belongs to no crate selects nothing only when it is a known non-Rust path (`docs/` except `docs/py/`, `scripts/`, `.github/`, `justfile`, root Markdown and licence files, editor and git config); any other path outside every crate selects every crate, so a new top-level directory is never dropped silently.

When the variable is `true`, the `affected` job runs on pull requests and publishes `scope` (`all`, `subset` or `none`) and `args` (`-p crate -p crate`). The `Test workspace` and `Test doctests` steps use them: `subset` runs only those crates, `none` skips the step, anything else (including an `affected` job that was skipped or failed) runs the whole workspace as before. Doctests run only for selected crates that have a library target (`doc_args`); a selection with none skips the step. The build step, lint, the Linux proofs and the `omp-e2e` acceptance proofs are not narrowed. A narrowed run does **not** record the `proved-tree` notice, so the push to `omp2` still runs the full suite instead of treating the tree as already proved. While the variable is on, the `rust` job also waits for the `affected` job (a checkout with full history and one `cargo metadata`), which adds that time to the pull request run.

## Measured on this workspace (62 members, `omp-e2e` excluded)

Command: `cargo metadata --format-version 1 --locked > /tmp/real-metadata.json` (9.8 s on the author's Mac), then `echo <path> | python3 scripts/affected-crates.py --metadata /tmp/real-metadata.json --files /dev/stdin --exclude omp-e2e`.

| Changed file | Scope | Crates |
|---|---|---|
| `crates/exec-ssh/src/lib.rs` | subset | 1 |
| `crates/contract/src/lib.rs` | subset | 16 |
| `crates/core/src/lib.rs` | subset | 57 |
| `docs/adr/x.md` | none | 0 |
| `justfile` | none | 0 |
| `Cargo.lock` | all | 61 |

These are crate counts, not minutes: no CI time saving has been measured.

## Trade-off

A narrowed run can miss a failure in a crate the script did not select. The known blind spots: a dependency that exists only through a path or an environment variable the source scan cannot see, a test that depends on a file outside every crate and outside the "report all" list, and a crate that is affected only by a feature change in a dependency's manifest outside its own directory (which the lockfile rule covers only when the lockfile changes). The push to `omp2` runs the full suite, so such a miss shows up there, not on the pull request.

## Turn it on and off

On: `gh variable set CI_AFFECTED_ONLY --body true`. Off: `gh variable delete CI_AFFECTED_ONLY` (or set it to anything but `true`). No workflow edit is needed either way. `scripts/check-ci-paths.py` fails if the `affected` job loses its opt-in variable or its pull-request-only condition, and `scripts/test_check_ci_paths.py` checks that the test steps fall back to the full workspace for an empty or unknown scope.
