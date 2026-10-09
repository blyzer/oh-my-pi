# Trial: verification recipes on a BuildBuddy remote runner (ADR 0040, Slices 0 and 0b)

This document records the operator trial ADR 0040 proposes before any product code: can a whole
verification recipe leave the developer's Mac, and what does it cost the Mac? It ran the heaviest
recipe, `just test-pkg omp-envd` (it links the embedded CPython into every test binary), on a
BuildBuddy Linux runner through `bb remote` (Slice 0), and probed the raw REAPI path with
`bb execute` (Slice 0b). The `just remote*` recipes in this change package what worked.

| | |
|---|---|
| **Date** | 2026-10-09 |
| **Measured commits** | `omp2` @ `6dab718d43` (runs 1-10) and the branch of this change (seed) |
| **Client** | `bb` 5.0.492 (release asset `bazel-5.0.492-darwin-arm64` from `buildbuddy-io/bazel`, sha256-checked) |
| **Runner** | BuildBuddy Cloud, Linux x86_64, Firecracker VM: `EstimatedCPU=8`, `EstimatedMemory=16GB`, `EstimatedFreeDiskBytes=40GB` |
| **Image** | runs 1-4: BuildBuddy's default (Ubuntu 20.04); runs 5-10: `gcr.io/flame-public/rbe-ubuntu24-04@sha256:f7db0d47…` |
| **Local host** | MacBook Pro `MacBookPro18,3` (M1 Pro), 16 GB RAM, macOS 27.0 |
| **Account** | an existing BuildBuddy organization (free tier), a dedicated API key kept in the macOS Keychain |

## Results

### Remote runs of `just test-pkg omp-envd`

| Run | Image | Snapshot state | Runner setup | Recipe | Total (client wall) | Outcome |
|---|---|---|---|---|---|---|
| 3 | 20.04 | cold | 24 s (partial reuse) | ~6 min | 6 min 41 s | 1057 passed, 5 failed (Python 3.8) |
| 5 | 24.04 | cold | 1 min 47 s | 4 min 55 s | 6 min 53 s | all passed |
| 7 | 24.04 | cold, saves a snapshot | 1 min 43 s | 4 min 59 s | 6 min 54 s | all passed |
| 9 | 24.04 | cold (new key), saves a snapshot | 1 min 45 s | 5 min 25 s | 7 min 19 s | all passed |
| **10** | 24.04 | **warm** (resumed run 9's snapshot) | **1 s** | **3 min 12 s** (2 crates compiled) | **3 min 26 s** | all passed |

Runs 1, 2, 4, 6 and 8 failed on environment setup or missed the snapshot; the causes are under
"What it took". A cold run compiles 1048 crates; the warm run compiled 2.

### Cost on the Mac

The `bb` client's own usage, from `/usr/bin/time -l`, was the same in every run:

| | Per run |
|---|---|
| CPU (user + sys) | 0.7-1.5 s |
| Maximum resident set size | 95-104 MB |
| Swap, pageouts, disk writes | none attributable |

### Local baseline

The same recipe on the Mac, on a warm target directory, holding the machine-wide cargo lock so that
no other build overlapped (`/usr/bin/time -l`; swap from `sysctl vm.swapusage`, pageouts from
`vm_stat`):

| Case | Wall | CPU (user + sys) | Largest process (max RSS) | Swap | Pageouts |
|---|---|---|---|---|---|
| no change | 1 min 16 s | 68 s | 0.8 GB | unchanged | +97 |
| one-line change in `crates/envd/src/lib.rs` | **4 min 10 s** | **463 s** | **3.6 GB** (the link) | **+1.6 GB** | **+5341** |

`/usr/bin/time` reports the largest single process, not the summed peak of the concurrent `rustc`
and link processes, so the memory figure understates the load.

### The recipes, as shipped in this change

| Run | What | Snapshot | Runner setup | Recipe | Total | Mac CPU | Mac max RSS |
|---|---|---|---|---|---|---|---|
| seed | `just remote-seed` (default branch head) | cold, saves the base snapshot | 1 min 55 s | 5 min 42 s | 7 min 48 s | 1.2 s | ~100 MB |
| branch 1 | `just remote-test-pkg omp-envd`, branch + uncommitted change, started 27 s after the seed | cold (the seed's snapshot was not available yet) | 1 min 53 s | 5 min 42 s | 7 min 45 s | 1.4 s | ~97 MB |
| **branch 2** | same, eight minutes later | **warm**; compiled `omp-tools` and `omp-envd` only | **1 s** | **3 min 3 s** | **3 min 12 s** | **1.0 s** | **95 MB** |

**Comparison for the same edit-and-test loop** (a one-line change in envd, then
`test-pkg omp-envd`): 4 min 10 s locally with 463 s of CPU, a 3.6 GB link and 1.6 GB of new swap,
against 3 min 12 s remotely with 1 s of the Mac's CPU and no swap. The remote checkout has fresh
timestamps every run (`git clean` and checkout), and cargo still rebuilt only the changed crates:
the repository's `checksum-freshness` judges sources by content.

## What it took (and what failed first)

1. **`bb remote` needs a Bazel marker.** It refuses to start without `MODULE.bazel`; an empty,
   untracked file is enough. `scripts/remote-verify.sh` creates it for the run and removes it.
2. **Several git remotes.** With `origin`, `fork` and `upstream`, `bb` asks which one to use and
   dies without a terminal (`select git remote: EOF`). It reads the answer from
   `git config buildbuddy.remote-bazel-remote-name`.
3. **The default image is Ubuntu 20.04.** Its gcc 9 makes `aws-lc-sys` refuse to build (GCC bug
   95189), so `cargo install cargo-nextest` failed; nextest is now installed prebuilt. Its Python
   3.8 fails five `eval::process` tests, because `external_runner.py` needs 3.10. The 24.04 image
   (Python 3.12) passes them. The product does not check the interpreter's version when it
   discovers one; that is a follow-up.
4. **`fetch-python.sh` needs `uv`**, which the image lacks.
5. **`bb remote` runs `git clean -x -d --force` in the checkout before every run.** That deletes
   `target/` and `vendor/` there, so every run recompiled everything. The runner script keeps both
   under `$HOME` (`CARGO_TARGET_DIR=$HOME/.cache/omp-target`, `vendor` linked into
   `$HOME/.cache/omp-vendor`), which the VM snapshot carries.
6. **Every runner property is part of the snapshot key.** Runs 6 and 8 changed only the save or
   read policy and missed the previous snapshot. BuildBuddy's docs say so ("Setting platform
   properties will change the snapshot key"). The recipes therefore fix every property, and force
   a save with a header that is not part of the key:
   `--remote_run_header=x-buildbuddy-platform.remote-snapshot-save-policy=always`.
7. **`first-non-default-ref` saves only the first run of a branch.** The trial branch's only
   snapshot was its first (failed, 20.04) run. The recipes use BuildBuddy's recommended
   `none-available` with `local-first` reads, plus one seeded snapshot on the default branch
   (`just remote-seed`), which new branches resume from.
8. **A worktree's `vendor` symlink was not ignored** (`/vendor/` matches only a directory), so
   `bb` would mirror it as a dangling link. `.gitignore` now says `/vendor`.

A flaky test surfaced on the way: `editor_base::tests::a_disk_change_after_first_contact_is_merged_not_reverted`
failed once remotely (and once on a Mac before). Its cause, a race with the docserver's file
watcher in the test itself, is fixed in #224.

## Slice 0b: raw REAPI through `bb execute`

Four small actions (a two-file input root, `workload-isolation-type=firecracker`, the 24.04 image,
`recycle-runner=true` with a fixed `runner-recycling-key`), one after another:

| Question (ADR 0040, Slice 0b) | Answer |
|---|---|
| Does raw REAPI run arbitrary commands with an uploaded input root? | Yes. The exit code comes back in `ActionResult` (3 for `exit 3`). |
| Is the workspace path stable? | Yes: `/workspace`. Actions run as `root`. |
| Are input file timestamps preserved? | **No.** Every input file carries the time it was materialized, not the local one. |
| Is the runner recycled (`recycle-runner=true`)? | **Not in this probe.** All four actions booted fresh VMs (uptime 3-7 s, different `boot_id` and worker), and a marker file never survived. |
| Is a runner recycled after a non-zero exit? | Not observable, since none was recycled. |
| Queue and input timing (`ExecutedActionMetadata`) | queued → worker start ≈ 0.06 s; input fetch ≈ 0.04 s for the tiny root; first action ≈ 9 s from worker start to input fetch (VM boot). |
| Capabilities (`GetCapabilities`) | Not called in this pass. |

**Consequence for Slice 1.** ADR 0040's REAPI backend counts on runner recycling for warm state.
This probe saw none, so a raw-REAPI action of the envd recipe would be cold every time (about
7 minutes), and fresh timestamps mean cargo's freshness must rely on content checksums (the repo
already enables `checksum-freshness`). The warm runs measured here come from the runner snapshots
of BuildBuddy's Run API (`bb remote`), which is BuildBuddy-specific. Whether recycling works with
other properties (for example `preserve-workspace`) or on another plan is open.

## Cost on the BuildBuddy side

Snapshots are the expensive part. BuildBuddy's docs size one at the runner's memory plus disk
(16 GB + 40 GB here) and bill cache transfer for saving and reading them remotely. The trial saved
snapshots in runs 1, 7 and 9 and in the seed. The organization's usage page is the authority for
the actual transfer; it was not read programmatically.

## How to use it

```sh
# once: install bb and store a revocable key
security add-generic-password -a "$USER" -s buildbuddy-omp -w      # prompts for the key
just remote-seed                       # after toolchain or dependency changes: one snapshot upload
just remote-test-pkg omp-envd          # any branch, uncommitted changes included
just remote clippy                     # any recipe
```

`OMP_REMOTE_KEYCHAIN_SERVICE` names another Keychain service; `BUILDBUDDY_API_KEY` in the
environment wins over both. `bb` hands the key to every process of the remote run (ADR 0040,
Part H.2), so use a key you can revoke and a BuildBuddy organization whose cache no release depends
on.
