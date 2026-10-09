# Remote build and test execution: where it stands and what comes next (2026-10-09)

Companion to [ADR 0040](../adr/0040-remote-build-and-test-execution.md) (proposed, merged in #212)
and to the trial record [`docs/audits/remote-verification-trial.md`](../audits/remote-verification-trial.md)
(in #226). This note is the hand-off for whoever continues the work: what is done, what was learned,
and the decision Slice 1 is waiting on.

## Goal (owner, 2026-10-08)

Move part of the build and test load off the developer's Mac (16 GB, it swaps under parallel cargo
builds) to BuildBuddy, without migrating to Bazel and without making BuildBuddy a hard dependency.
omp owns the execution abstraction (BuildSystem adapter → execution plan → router → backend);
BuildBuddy is one replaceable backend. `BuildSystem != ExecutionBackend`. Future ADW products may use
Cargo, Maven, Gradle, Go, .NET, npm/pnpm, Python or Bazel.

## Done

| Step | Where | State |
|---|---|---|
| Discovery, BuildBuddy and REAPI research, three strategies, decision | ADR 0040 (#212) | merged, status proposed |
| Slice 0: operator trial with `bb remote` | `docs/audits/remote-verification-trial.md` (#226) | measured |
| Recipes that package Slice 0 | `just remote`, `just remote-test-pkg`, `just remote-seed`; `scripts/remote-verify.sh` and `scripts/remote-verify-runner.sh` (#226) | live-tested |
| Slice 0b: raw-REAPI probe with `bb execute` | same audit, "Slice 0b" section | measured |
| Slice 1 (first product code) | not started | waits on the decision below |

### The numbers that matter

The same loop (a one-line change in `crates/envd`, then `just test-pkg omp-envd`):

| | Local Mac | Remote, warm (`just remote-test-pkg omp-envd`) |
|---|---|---|
| Wall | 4 min 10 s | 3 min 12 s |
| Mac CPU | 463 s | 1.0 s |
| Mac largest process | 3.6 GB (the link with CPython) | 95 MB |
| Mac swap | +1.6 GB | none |

A cold remote run (new snapshot key, or no snapshot yet) takes about 7 to 8 minutes; the Mac still
spends about 1 s of CPU on it.

### Setup on the owner's machine (not in the repository)

- `bb` 5.0.492 at `~/.local/bin/bb`: the release asset `bazel-5.0.492-darwin-arm64` from
  github.com/buildbuddy-io/bazel, sha256-checked. The `buildbuddy-io/tap` Homebrew tap does not
  exist.
- The API key is in the macOS Keychain, service `buildbuddy-omp2-trial`. The recipes read
  `OMP_REMOTE_KEYCHAIN_SERVICE` (default `buildbuddy-omp`), so either set that variable or store the
  key again under the default name. `BUILDBUDDY_API_KEY` in the environment wins.
- The BuildBuddy organization is the owner's existing one, shared with Nostro and the v1 repo.
  `blyzer/oh-my-pi` is linked to BuildBuddy's GitHub App. The owner reported no usage this month
  before the trial; the trial saved several snapshots (runs 1, 7, 9 and the seed), so check
  "Usage" on app.buildbuddy.io before seeding again.
- A trial worktree `omp2-wt-bb` (branch `rbe-trial`, pushed) and `~/rbe-trial.sh` can be deleted
  now that the recipes exist.

## What the trial taught (keep these, they cost time)

1. **Every runner property is part of BuildBuddy's snapshot key.** Change one (even the save or read
   policy) and the next run is cold. Force a snapshot save with a header instead, which is not part
   of the key: `--remote_run_header=x-buildbuddy-platform.remote-snapshot-save-policy=always`.
2. **`bb remote` runs `git clean -x -d --force` in the checkout before every run.** Anything that
   must survive (the cargo target dir, the CPython bundle) lives under `$HOME` on the runner.
3. **`first-non-default-ref` saves only a branch's first run.** The recipes use BuildBuddy's
   recommended `none-available` with `local-first` reads, plus one seed on the default branch
   (`just remote-seed`, which uses `--run_from_branch`). A run started right after a seed may not
   see the new snapshot yet, because the upload takes a while.
4. **The default runner image is Ubuntu 20.04.** Its gcc 9 cannot build `aws-lc-sys` (so nextest is
   installed prebuilt), and its Python 3.8 fails five `eval::process` tests. Use BuildBuddy's 24.04
   image, pinned by digest in `scripts/remote-verify.sh`.
5. **`bb remote` needs a `MODULE.bazel` marker** and asks which git remote to use when there are
   several. The launcher handles both.
6. **Cargo freshness survives fresh checkouts** because the repository enables
   `checksum-freshness`: the warm run rebuilt only the changed crates.
7. **The key is visible to every process of a `bb remote` run** (ADR 0040, Part H.2). Use a
   revocable key, and never let that organization's action cache decide a verdict.

## Decision waiting for Slice 1

ADR 0040's Slice 1 (crates `omp-exec` and `omp-exec-reapi`, envd capture, `omp adw run` wiring) is a
generic REAPI client. It assumed BuildBuddy recycles runners, so that actions start warm. **The
Slice 0b probe saw no recycling.** Four `bb execute` actions with `recycle-runner=true` and a fixed
`runner-recycling-key` each booted a fresh VM. Input timestamps were not preserved either. So a raw
REAPI action of the envd recipe would be cold every time, about 7 minutes. The warm 3-minute runs
come from the snapshots of BuildBuddy's Run API (`bb remote`), which is BuildBuddy-specific.

Options:

| Option | What | Warm on BuildBuddy | Portability |
|---|---|---|---|
| A | Slice 1 as written: a generic REAPI backend | no, cold every run (~7 min) unless recycling works | high: any REAPI server |
| B | Same domain and router, but the first backend drives BuildBuddy's Run API (snapshots) | yes (~3 min) | BuildBuddy-specific, behind the same abstraction; the generic REAPI backend follows |
| C | First probe recycling further, then choose A or B | n/a | n/a |

**Recommendation: C, then decide.** It is about 30 minutes of work and cheap. Probe recycling with
other properties: `preserve-workspace=true`, the default isolation instead of Firecracker, a longer
gap between actions, and the same properties `bb remote` sends. Read
`server/remote_execution/` and the runner pool in the BuildBuddy source pinned by ADR 0040 for the
recycling conditions. Record the outcome as an ADR 0040 amendment before writing Slice 1 code.

Whatever is chosen, Slice 1 must also:

- give `omp adw run` an envd environment, apply the user cfg (`process_ctx_with`) and wire Ctrl-C to
  the cancellation token (today it uses `Ctx::new()` and its token never fires: audit F11 and ADR
  0040 Part A.3);
- capture inputs with git semantics: tracked plus untracked-unignored files, minus deleted files,
  with secret-pattern exclusion and a content scan before hashing (ADR 0040 Part D);
- cancel through BuildBuddy's per-invocation cancel, because dropping the stream does not cancel and
  BuildBuddy has no `CancelOperation` (ADR 0040 Part I);
- keep the key out of argv and the environment of actions (gRPC metadata only, Part F).

The full PR plan (1a-1d), test plan and measurement plan are in ADR 0040 under "Implementation
plan", "Test plan" and "Measurement plan".

## Open questions for the owner

- Which Slice 1 option (A, B, or C first)?
- Should a remote Linux verification become a CI gate? The Linux workspace test suite has never
  run in CI: CI runs it only on macOS, although `AGENTS.md` says otherwise.
- A dedicated BuildBuddy organization for omp, so its cache and quota stay separate from Nostro's?
- The self-hosted GitHub runners on the Mac (OmnIus, vostroRD) load it when they get jobs; they are
  outside omp, but they compete for the same 16 GB.
