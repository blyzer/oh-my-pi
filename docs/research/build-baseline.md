# OMP² Build Baseline: Local Loop and CI Durations

Status: research (Phase B baseline in `omp2-build-acceleration.md` §7–8). Measurement only; no build
or CI configuration changed.
Date: 2026-10-11

Scope:
- Local timings of the inner loop at `origin/omp2` `d133d2e429`: clean `just check`, warm no-op,
  incremental checks after a one-line edit in a leaf, a mid-graph and a top crate, a cold `omp`
  build with its `--timings` critical path, clippy and `just test`.
- CI per-job and per-step durations (median, p90) and cache restore state from 30 recent
  successful `ci.yml` runs.
- How concurrent load on the shared Mac contaminated the local runs, and which numbers survive.

The CI critical path (`changes` → `format` → `rust`), the cache key policy, the build-script mtime
cascade and the nextest archive analysis are covered in
[`ci-test-acceleration.md`](ci-test-acceleration.md) (CiAccelSynth). They are not repeated here.
This report adds the per-step medians it cites only where the plan needs them, the local numbers,
and the pooled cache-hit counts.

Evidence labels:
- **OBSERVED**: ran or read this session; command or path cited.
- **V**: checked against upstream docs this session.
- **[INFERENCE]**: reasoning, not measured.
- **UNVERIFIED**: could not be checked.

## Reproduce

`scripts/build-baseline.sh` (new; bash, python3, `gh`; no new dependencies):

| Command | Produces |
|---|---|
| `scripts/build-baseline.sh` | All 12 local scenarios in order, then `summary.md` |
| `scripts/build-baseline.sh check-edit-core build-cold …` | Only the named scenarios |
| `scripts/build-baseline.sh summary` | Tables rebuilt from an existing `results.tsv` and `cargo-timing.html` |
| `BASELINE_CI_BEFORE=2026-10-11T10:30:00Z scripts/build-baseline.sh ci` | The CI tables in this report (`$OUT/ci/summary.md`), from the same runs |

What the script does:
- Uses `CARGO_TARGET_DIR=/private/tmp/omp-build-baseline-target` (`BASELINE_TARGET_DIR`).
- Repeats each warm or incremental scenario `BASELINE_REPS=3` times.
- Edits are a comment line appended to `crates/{core,chat,app}/src/lib.rs`, restored from a copy
  after every repetition, and on exit or interrupt.
- Runs the exact commands the `just` recipes run (`justfile:check`, `build`, `clippy`, `test`).
  Clippy uses its own `target/clippy` sub-directory, as `justfile:clippy` does.
- Times each attempt with `/usr/bin/time -l` and records wall, user+sys and max RSS.
- Every 5 s it samples the load average, foreign compiler processes and foreign %CPU. A process is
  foreign when it is not a descendant of the script.
- Waits up to 60 s for the load average to drop below `hw.ncpu` before each attempt.
- Marks each attempt contaminated with two flags:
  - flag 1: the load average at start is above `ncpu`;
  - flag 2: a foreign compiler was seen, or foreign CPU went above 300 %.
- Repeats a contaminated attempt once and records both attempts. The summary uses the last
  attempt of each repetition.
- When `/tmp/claude-501/omp2-cargo.lock` exists, runs under `lockf -k` on it. It did not exist
  during either run (see Contamination).

## Host and setup

| Item | Value |
|---|---|
| Host | MacBook Pro `MacBookPro18,3`: M1 Pro, 10 CPUs (8P + 2E), 16 GB RAM, macOS 27.0 (OBSERVED, `sysctl`) |
| Toolchain | `rustc 1.99.0-nightly (1a98b1e13 2026-08-07)`, cargo `c79e8f894`, cargo-nextest 0.9.146 |
| Commit | `d133d2e429` (`origin/omp2`, "Merge pull request #253 from blyzer/ci/protobuf-gate") |
| Dev profile | members on Cranelift, deps on LLVM, `incremental = false`, `codegen-units = 256`, `-Z threads=8`, `checksum-freshness` (`.cargo/config.toml`, `Cargo.toml [profile.dev]`) |
| Setup | `just setup-python` (untimed): 38 s, `vendor/` 531 MB |
| Swap at start | 23.5 GB total, 22.8 GB used (`sysctl vm.swapusage`) |
| Graph position (OBSERVED, `cargo metadata`) | `omp-core`: 0 workspace deps, 42 workspace dependents. `omp-chat`: 31 deps, 2 dependents (`omp-app`, `omp-e2e`). `omp-app`: 45 deps, 1 dependent (`omp-e2e`). 48 members, 1258 packages. |

## Local results

Run 2 is the measurement of record: 13:35–15:57 UTC, `results.tsv` in
`$OUT=/private/tmp/omp-build-baseline-target/baseline2`. Columns:
- **Median** is over the 3 repetitions, last attempt of each.
- **All-attempt** columns include the contaminated first attempts, for spread.
- **Clean** counts repetitions whose last attempt had no flag.
- **Units** counts `Compiling`/`Checking` lines.

| Scenario | Command | Median s | Min–max s (all attempts) | Clean / reps | Units | user+sys s (median) | Max RSS MB |
|---|---|---|---|---|---|---|---|
| check clean | `just check` (empty target) | 160.6 (n=1; attempt 1: 154.7) | 154.7–160.6 | 0/1 | 983 | 724.6 | 2270 |
| check no-op | `just check` | 8.1 | 5.2–31.1 | 1/3 | 0 | 2.3 | 253 |
| check, edit `omp-core` (leaf) | `just check` | **69.6** | 68.2–70.9 | 1/3 | 43 | 202.6 | 2949 |
| check, edit `omp-chat` (mid) | `just check` | **13.3** | 12.8–15.7 | 2/3 | 3 | 21.1 | 1382 |
| check, edit `omp-app` (top) | `just check` | **10.4** | 10.1–11.7 | 1/3 | 2 | 15.2 | 1389 |
| build `omp` cold | `cargo build -p omp-app --bin omp --locked --timings` | 359.4 (n=1; attempt 1: 337.4) | 337.4–359.4 | 0/1 | 944 | 1048.3 | 3994 |
| build no-op | `just build` | **4.9** | 4.9–36.6 | 3/3 | 0 | 1.6 | 249 |
| build, edit `omp-app` | `just build` | **42.0** | 40.6–42.8 | 3/3 | 1 | 40.6 | 4279 |
| clippy cold | `CARGO_TARGET_DIR=…/clippy cargo clippy --workspace --all-targets --locked` | 242.2 (n=1; attempt 1: 232.3) | 232.3–242.2 | 0/1 | 996 | 1202.4 | 2244 |
| clippy no-op | same | 12.7 | 11.3–48.2 | 0/3 | 0 | 3.9 | 276 |
| `just test` cold | nextest `--workspace --exclude omp-e2e`, then `cargo test --doc` | **1277.1** (attempt 1) | — | 0/1 | 112 compiled for `test` | 2880.1 | 3423 |
| `just test` warm | same | 408.2 | 395.4–424.3 | 0/3 | 0 | 809.0 | 1395 |

Notes:
- **Spread.** The warm and incremental arms are tight. Excluding first attempts that ran right
  after a cold phase, max − min is ≤ 3 s, except `test-warm` at 29 s. **[INFERENCE]** The
  scenarios sit on the same few seconds whether or not they were flagged, so the flags in run 2
  did not move these numbers (see Contamination).
- **`test-cold` attempt 2 (418 s) is not cold.** The first attempt had already built the test
  binaries, so attempt 2 equals a warm run. The script now measures `test-cold` once and does not
  retry it. The cold number of record is attempt 1: 1277 s, which compares with CI's
  `Test workspace` median of 34.5 min (below).
- **`just test` decomposition** (OBSERVED, `logs/test-*.log`):
  - Cold: the nextest test-profile build finished in 14 min 29 s.
  - Warm: it finished in 8.5–9.0 s.
  - Every run executed `10234 tests run: 10234 passed (2 slow), 12 skipped`, with nextest
    reporting a summary of 282–305 s.
  - The doc-test build `Finished … in 22–28 s`. 189 doctests ran, for 0.9 s summed run time.
  - [INFERENCE] The rest of a warm run (~80–90 s) is rustdoc compiling and linking the doctests
    per crate.
  - So warm `just test` is ~70 % nextest execution, not compilation.
- **Leaf vs. top edit.** One comment line in `omp-core` costs 70 s of `check` (43 units: all 42
  dependents plus itself). The same line in `omp-chat` or `omp-app` costs 10–13 s (2–3 units).
  The `omp-app` edit under `just build` costs 42 s. That is 1 unit, i.e. the `omp` bin link
  (the statically linked CPython, `.cargo/config.toml` comment on ld64.lld), plus `omp-app`
  codegen on Cranelift.
- **Disk.** Target sizes: 1.9 GB after `check`, 5.3 GB after the cold build, 7.2 GB with clippy,
  22.8 GB after `just test` (`disk.tsv`). [INFERENCE] The jump to 22.8 GB comes from the 79 test
  binaries that link `omp-py`, each carrying the embedded CPython.

### Cold `omp` build: critical path and top crates (`cargo build --timings`)

Source: `cargo-timing.html` of run 2, attempt 2 (`UNIT_DATA`, `CONCURRENCY_DATA`, `CPU_USAGE`),
parsed by `scripts/build-baseline.sh summary`. The build had 1343 units and ended at 358.5 s, with
2707 s of summed unit time; 412 s of that is `omp-*` members.

Top 10 units by duration:

| # | Unit | Target | Duration s | Start s |
|---|---|---|---|---|
| 1 | omp-ai 2.0.0 | lib | 59.0 | 94.8 |
| 2 | objc2-app-kit 0.3.2 | lib | 42.0 | 103.3 |
| 3 | omp-proto 2.0.0 | lib | 35.9 | 59.0 |
| 4 | lopdf 0.42.0 | lib | 31.4 | 174.5 |
| 5 | omp-envd 2.0.0 | lib | 30.8 | 276.4 |
| 6 | omp-app 2.0.0 | `omp` bin | 30.8 | 327.8 |
| 7 | read-fonts 0.39.2 | lib | 29.9 | 153.4 |
| 8 | image 0.25.10 | lib | 26.9 | 105.6 |
| 9 | skrifa 0.42.1 | lib | 24.7 | 187.9 |
| 10 | jj-lib 0.44.0 | lib | 24.3 | 245.7 |

Critical path: a walk back from the last unit through `unblocked_units` / `unblocked_rmeta_units`.

| # | Unit | Target | Start s | Duration s |
|---|---|---|---|---|
| 1 | fallible-streaming-iterator | lib | 68.0 | 0.2 |
| 2 | rusqlite | lib | 68.9 | 2.4 |
| 3 | omp-memory | lib | 256.0 | 3.1 |
| 4 | omp-tools | lib | 259.7 | 19.8 |
| 5 | omp-envd | lib | 276.4 | 30.8 |
| 6 | omp-driver | lib | 296.7 | 15.8 |
| 7 | omp-app | lib | 307.9 | 19.9 |
| 8 | omp-app | `omp` bin | 327.8 | 30.8 |

Concurrency over time (OBSERVED, `CONCURRENCY_DATA` and `CPU_USAGE`, averaged per window):

| Window s | Active units | Waiting units | CPU % |
|---|---|---|---|
| 0–60 | 10.0 | 184 | 97 |
| 60–120 | 10.0 | 195 | 88 |
| 120–180 | 10.0 | 161 | 70 |
| 180–256 | 10.0 | 83 | 55 |
| 256–300 | 9.4 | 14 | 84 |
| 300–360 | 0.8 | 0.2 | 42 |

Reading:
- Up to ~256 s the build is throughput-bound: 10 jobs are always active and there is a queue.
- The last ~100 s is a serial member chain:
  `omp-memory → omp-tools → omp-envd → omp-driver → omp-app` lib → `omp` bin.
  - It accounts for 120 s of unit time.
  - The final 60 s average 0.8 active units.
- `rusqlite` appears on the path only because it was `omp-memory`'s last unblocker.
  `omp-memory` itself waited for a free slot until 256 s.
- [INFERENCE] Shortening that tail means splitting `omp-envd`/`omp-tools`/`omp-driver`, or moving
  the bin link off the path. A faster dependency build does not shorten it.
- [INFERENCE] `omp-ai` (59 s) and `omp-proto` (36 s) are the largest member units in the parallel
  phase.

## CI baseline (`ci.yml`)

Generated with `BASELINE_CI_BEFORE=2026-10-11T10:30:00Z scripts/build-baseline.sh ci`, which reads
the Actions API (`/actions/workflows/ci.yml/runs`, `/runs/{id}/jobs`, `/jobs/{id}/logs`).

Two samples (OBSERVED):
- **push-omp2:** the 15 most recent successful omp2 pushes, 2026-10-09T16:40Z..2026-10-11T10:21Z,
  runs 38131682411 … 37960854650. `rust` ran in 5 of them. In the other 10, `push_verified`
  skipped it.
- **pr-with-rust:** the 15 most recent successful PR runs in which `rust` ran and passed,
  2026-10-10T08:05Z..2026-10-11T10:09Z, runs 38130932087 … 38036680296.

All jobs ran on hosted runners. `rust` ran on `macos-15` (runner names "GitHub Actions 10000…").
No job ran on a self-hosted runner.

### Job durations (minutes)

| Job | push n | push median | push p90 | PR n | PR median | PR p90 |
|---|---|---|---|---|---|---|
| Rust workspace and acceptance proofs (macOS) | 5 | **54.6** | 64.5 | 15 | **56.4** | 73.5 |
| Terminal proof P7 (Linux PTY) | 15 | 7.6 | 10.0 | 15 | 6.6 | 10.1 |
| Lint workspace (Linux) | 15 | 7.0 | 8.4 | 15 | 4.7 | 8.3 |
| Acceptance proofs P9, P10, tool sources (Linux) | 15 | 5.8 | 9.2 | 15 | 5.5 | 7.7 |
| License policy and release notices | 15 | 4.6 | 5.5 | 15 | 0.5 | 5.5 |
| Runtime symbol and dependency contracts | 15 | 2.4 | 2.5 | 15 | 2.3 | 2.5 |
| Rust format | 15 | 1.5 | 1.6 | 15 | 1.5 | 1.6 |
| Error-formatting ratchet (lintx) | 15 | 0.7 | 0.7 | 15 | 0.4 | 0.7 |
| Run, from start to last update | 15 | 11.4 | 60.2 | 15 | 58.3 | 75.2 |

Reading the table:
- Queue time from job creation to start: median 2–8 s for every job.
- The push run median (11.4 min) is low because `rust` was skipped in 10 of the 15 pushes.

### Steps of the heavy jobs (both samples pooled; minutes)

| Job / step | n | Median | p90 | Max |
|---|---|---|---|---|
| **rust**: Cache Cargo (restore) | 20 | 0.0 | 2.3 | 2.9 |
| rust: Lint workspace (`cargo clippy --workspace --all-targets`) | 20 | **11.0** | 15.3 | 15.4 |
| rust: Test workspace (nextest, includes the test-profile build) | 20 | **34.5** | 47.1 | 49.6 |
| rust: Test doctests | 20 | 0.9 | 1.2 | 1.4 |
| rust: Compile acceptance proofs | 20 | 4.3 | 5.4 | 5.8 |
| rust: P1–P6 / P7 / P12 / P8 | 20 | 0.2 / 1.6 / <0.1 / <0.1 | — | — |
| rust: Install native build tools | 20 | 0.5 | 0.6 | 0.7 |
| rust: Post Cache Cargo (save) | 20 | 1.7 | 2.2 | 3.0 |
| **lint_linux**: Lint workspace | 30 | 4.3 | 7.3 | 7.8 |
| **p7_linux**: Run P7 (build + run) | 30 | 5.4 | 9.1 | 9.4 |
| **p9_p10_linux**: Run P9/P10/P11 (build + run) | 30 | 4.0 | 7.8 | 8.1 |
| **licenses**: Install pinned compliance tools (only on a miss) | 17 | 4.9 | 5.1 | 5.2 |

`rust` compile phases from `Finished` lines in the job logs (OBSERVED, all 20 `rust` jobs):

| Phase | Cache miss (n=14): median (min–max) | Prefix hit (n=6): median (min–max) |
|---|---|---|
| clippy `dev` | 11.6 min (6.2–15.4) | 6.6 min (5.0–8.6) |
| nextest `test` build | 32.1 min (14.6–41.7) | 27.6 min (21.6–43.3) |
| e2e `test` build | 4.3 min (1.8–5.8) | 4.1 min (2.9–4.8) |

The local M1 Pro (10 CPUs) runs the same work faster than hosted `macos-15`:

| Work | Local | CI miss median | Ratio |
|---|---|---|---|
| Clippy cold | 4.0 min | 11.6 min | ≈3× |
| Test-profile build, cold | 14.5 min | 32.1 min | ≈2.2× |

[INFERENCE] The hosted runner is the slower machine. Its core count and RAM are not logged
(UNVERIFIED; `ci-test-acceleration.md` "Gaps" notes the same).

### Cache restore state (both samples pooled)

How each state was classified:
- **exact**: the primary key was restored.
- **prefix**: a `restore-keys` fallback was restored. Cargo keys end in `${{ github.sha }}`, so
  this is the best a first attempt can get.
- **miss**: `Cache not found for input keys`.

| Job | Event | exact | prefix | miss | Units compiled, median (hit / miss) | Job min, median (hit / miss) | "Unable to reserve cache" |
|---|---|---|---|---|---|---|---|
| rust (macOS) | PR | 0 | 5 | 10 | 48 / 1900 | 50.5 / 60.6 | 0 |
| rust (macOS) | push | 0 | 1 | 4 | 48 / 1900 | 67.2 / 54.4 | 0 |
| lint_linux | PR | 0 | 10 | 5 | 24 / 1030 | 4.4 / 8.3 | 2 |
| lint_linux | push | 0 | 5 | 10 | 24 / 1030 | 4.5 / 8.3 | 6 |
| p7_linux | PR | 0 | 12 | 3 | 22 / 968 | 6.5 / 10.3 | 3 |
| p7_linux | push | 0 | 6 | 9 | 22 / 968 | 5.8 / 9.0 | 8 |
| p9_p10_linux | PR | 0 | 13 | 2 | 23 / 969 | 5.4 / 9.0 | 1 |
| p9_p10_linux | push | 0 | 9 | 6 | 23 / 969 | 5.5 / 9.1 | 6 |
| licenses (compliance tools) | PR | 9 | — | 6 | 0 / 437 | 0.5 / 5.3 | 0 |
| licenses (compliance tools) | push | 4 | — | 11 | 0 / 437 | 0.5 / 5.3 | 0 |
| lintx | PR | 12 | — | 3 | 0 / 35 | 0.4 / 0.7 | 0 |
| lintx | push | 4 | — | 11 | 0 / 35 | 0.4 / 0.7 | 0 |

Findings (OBSERVED unless marked):
- **The `rust` job misses its Cargo cache 14 times in 20.** On a miss it compiles 1900 units; on a
  prefix hit, 48.
  - A prefix hit restores a ~4.97 GB entry in 1.6–2.9 min (93–172 s), against ~0 s on a miss.
  - The job spends 1.3–3.0 min (76–177 s) in `Post Cache Cargo` either way.
  - A hit does not reliably shorten the job. Medians are 50.5 vs 60.6 min on PRs, and on push the
    single hit was the slowest run (67.2 min).
  - The prefix-hit `Test workspace` step still spent 21.6–43.3 min in the test build. That is
    consistent with the build-script mtime cascade in `ci-test-acceleration.md` §3.
- **Pushes to omp2 miss far more than PRs.**
  - Linux Cargo jobs miss 25 of 45 on push vs 10 of 45 on PRs.
  - `licenses` and `lintx`, whose keys contain no SHA, miss 11 of 15 on push.
  - [INFERENCE] This matches the 10 GB repository cache eviction recorded in
    `omp2-build-acceleration.md` item 40. A key without a SHA that misses can only have been
    evicted.
- **A miss costs Linux jobs about 4 min.** The jobs take 8.3–10.3 min on a miss and 4.4–6.5 min on
  a hit. A hit compiles ~22–24 units (members only).
- **`licenses` spends 4.9 min reinstalling the pinned tools on every miss** (17 of 30 jobs), for a
  0.2 min check.
- **`Unable to reserve cache` appears 26 times in 90 Linux jobs.** Parallel jobs race to save the
  same `python-*` key (also `omp2-build-acceleration.md` item 40).

The cache policy changes are in `ci-test-acceleration.md` §4 and its Diffs 2–4.

## Contamination

The task required flagging local runs where the load average exceeds `ncpu` and re-running them
once. The script records both flags per attempt and re-runs once. What happened:

**Run 1 (10:43–13:29 UTC; `/private/tmp/omp-bb-run1/results.tsv`): rejected.**
- `BazelPocResearch` was compiling with Bazel `rules_rust` at the same time. `ps` showed
  `bazel(omp-bazel-poc)` and its `rustc`/`process_wrapper` children (OBSERVED).
- The load average reached 76–150 on 10 CPUs.
- `check-edit-core` measured 101–142 s under that load, against 68–71 s in run 2.
- `check-edit-chat` and `check-edit-app` measured up to 2.7× run 2.
- The detector of that version recognised only foreign `rustc` processes, and with a path
  heuristic. It missed several attempts, so the script was fixed:
  - process-tree ancestry replaces the path heuristic;
  - Bazel servers count as foreign compilers;
  - the foreign %CPU column was added.
- After `BazelPocResearch` confirmed that its builds and Bazel servers were shut down, the whole
  suite was re-run as run 2.
- Two run-1 numbers taken in a quiet window agree with run 2: `check-clean` 182 s (start load
  8.8) and `clippy-cold` 238 s (start load 8.2).

**Run 2 (13:35–15:57 UTC): the measurement of record, still flagged.**
- 0 foreign compiler processes were seen in any attempt (`foreign_max` = 0 throughout; OBSERVED).
- Flag 1 fired because the load average stayed high on an otherwise quiet machine:
  - 16–25 with no build running (OBSERVED, six 20 s samples at 13:30 UTC, summed `ps` CPU ≈3 %);
  - swap at 22.8–25.5 GB used of 23.5–26.6 GB.
  - [INFERENCE] On macOS the load average counts threads blocked on I/O and swap-in, so on this
    16 GB host it is a weak signal.
- Flag 2 fired on foreign %CPU of 200–410 % in quiet phases and 480–890 % during cold builds and
  tests.
  - OBSERVED `/bin/ps` samples: WindowServer ~35 %, the Sophos endpoint scan extension and
    Detection/CryptoGuard 20–64 %, the `omp` daemon ~25 %, and occasional `node`/`kimi` spikes.
  - [INFERENCE] The antivirus load rises with the build's file writes, so part of flag 2 is caused
    by the build itself.
- Why run 2 is accepted anyway:
  - repeated attempts agree within ≤ 3 s, flagged or not;
  - no competing compiler was ever seen;
  - the only clean-by-both-flags arms (`build-noop` 4.9 s, `build-edit-app` 42.0 s) are in line
    with their flagged neighbours.
- Contaminated outliers that were superseded by their retry:
  - `check-noop` rep 1: 31.1 s → 8.2 s;
  - `build-noop` rep 1: 36.6 s → 5.9 s;
  - `clippy-noop` rep 1: 48.2 s → 11.3 s.
  - Each was the first command after a cold phase, with a start load of 19–29.
- After run 2 ended, a `rust-analyzer` flycheck (`cargo check --message-format=json` on the main
  checkout) and its `clippy-driver` were seen (OBSERVED, ~16:05 UTC). They did not overlap any
  timed attempt.

The machine-wide lock `/tmp/claude-501/omp2-cargo.lock` (convention in
`docs/parked/2026-10-09-handoff-pending-work.md`) existed at 10:35 UTC and was gone by 10:47 UTC.
Neither run held it.

[INFERENCE] A cleaner baseline needs a host with free RAM (no 23 GB swap) and the AV exclusion
for the target directory. The script's flags will show whether such a run is clean.

## Comparison with earlier records

| Measure | Earlier record (`omp2-build-acceleration.md` §7) | This run |
|---|---|---|
| Cold `omp` build, M1 Pro | 309–731 s | 337–359 s |
| Edit-core loop | 352.9 s (different arm: `build` + `e2e-build`) | `check` after a core edit: 70 s (not comparable) |
| `just test`, `just clippy` locally | unmeasured | test cold 1277 s, warm 408 s; clippy cold 232–242 s, no-op 11–15 s |
| Combined peak RSS | unmeasured | max RSS of a single process tree (`time -l`): 4.3 GB on `build-edit-app` (the link), 3.4 GB on `test-cold` |

## Gaps

- Local cold arms have n=1 per run (2 attempts each). Spread for cold builds is not established.
- The `just test` cold number has one attempt, and that attempt was flagged.
- `time -l` max RSS covers the largest single process, not the sum of the build tree. Combined RSS
  is still unmeasured.
- The hosted `macos-15` runner's CPU count and RAM are UNVERIFIED (not logged).
- The CI samples predate the `ci/test-acceleration` changes (run 38133768415 already shows a
  `Lint workspace (macOS)` job). Re-run `scripts/build-baseline.sh ci` without
  `BASELINE_CI_BEFORE` once they land.
