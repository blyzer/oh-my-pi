# Measurement: sccache across targets, worktrees and change switches on Apple silicon

This document asks whether a shared local sccache pays off where the persistent
CI target dir does not reach:
- a fresh target dir;
- a second worktree;
- switching away from a change and back.

It records one measurement on the self-hosted macOS runner. It changes nothing
and recommends no configuration. Companion to
[`cranelift-panic-cleanup-mac-followup.md`](cranelift-panic-cleanup-mac-followup.md),
which ran on the same host.

| | |
|---|---|
| **Date** | 2026-09-23 |
| **Run** | [sccache benchmark, run 35920487190](https://github.com/blyzer/oh-my-pi/actions/runs/35920487190), 21:11–23:28 UTC; results in its `bench-sccache` artifact (`meta.json`, `results.jsonl`, `summary.md`) |
| **Measured commit** | `3cc542235` on the experimental branch `claude/llvm-dev-benchmark`: `omp2` @ `58adc196f` plus benchmark tooling only. The build uses `omp2`'s dev profile unchanged (members on Cranelift, `incremental = false`). |
| **Tool** | `scripts/bench-sccache.py` and `.github/workflows/bench-sccache.yml` at that commit. Neither file is in `omp2`. 2 repetitions. |
| **sccache** | 0.18.0, the release archive for aarch64-apple-darwin, checksum-pinned, local disk cache |
| **Host** | MacBook Pro `MacBookPro18,3` (M1 Pro: 8 performance + 2 efficiency cores), 16 GB RAM, macOS 27.0; the self-hosted CI runner, on AC power |

## Method

The build is `cargo build -p omp-app --bin omp`.

**Arms.** Each arm starts from an empty cache.

| Arm | Compiler wrapper |
|---|---|
| `none` | none; the control |
| `sccache` | `RUSTC_WRAPPER=sccache`, default hashing, so absolute paths must match for a hit |
| `sccache-basedirs` | the same, plus `SCCACHE_BASEDIRS` set to both worktree roots |

**Steps, in order, for every arm:**

1. **fresh-target-empty-cache:** worktree A, an empty target dir, an empty cache.
2. **fresh-target-warm-cache:** worktree A, the target dir deleted, the cache from step 1. In `none` this is simply a second cold build.
3. **second-worktree:** worktree B, a `git worktree` of the same commit at another absolute path, with its own empty target dir and the shared cache.
4. **switch-to-change:** worktree A with its warm target dir. A one-line comment is appended to `crates/core/src/lib.rs`, which rebuilds 41 units.
5. **switch-back:** worktree A with the edit reverted. The target dir now holds the edited build; the cache still holds the original.

**Recorded per step:**
- wall time;
- units cargo compiled;
- sccache hits, misses and non-cacheable calls for that step alone;
- cache size and target size.

**Controls:**
- Arms alternate order between repetitions (none, sccache, basedirs, then the reverse).
- The target dir is passed with `--target-dir`, not `CARGO_TARGET_DIR`. sccache hashes `CARGO_*` environment variables verbatim, so an absolute path there would make every compilation in worktree B miss. A Linux smoke run showed exactly that before this was fixed.
- Target dirs are named `*.noindex`, so Spotlight skipped them. This was a temporary control for this run only. Other scanners may still have read them.
- The sccache server ran on its own port and cache directory, and was stopped afterwards.
- Crates were fetched once, untimed.

## Results

**Means over 2 repetitions** (seconds). The sccache columns add hits / misses / non-cacheable calls in parentheses.

| Step | none | sccache | sccache-basedirs |
|---|---:|---:|---:|
| fresh-target-empty-cache | 422.7 ± 218.0 | 395.3 ± 48.6 (0 / 1212 / 227) | 505.3 ± 141.3 (0 / 1212 / 227) |
| fresh-target-warm-cache | 296.2 ± 49.7 | **131.3 ± 21.9** (1212 / 0 / 227) | **143.0 ± 3.3** (1212 / 0 / 227) |
| second-worktree | 358.2 ± 17.4 | 305.8 ± 52.0 (**854 / 358** / 227) | 379.1 ± 55.6 (**854 / 358** / 227) |
| switch-to-change | 177.7 ± 41.1 | **246.7 ± 17.2** (0 / 42 / 2) | **240.2 ± 22.1** (0 / 42 / 2) |
| switch-back | 126.8 ± 15.1 | **63.7 ± 1.4** (42 / 0 / 2) | **64.1 ± 6.6** (42 / 0 / 2) |

- **Sizes:** the cache reached 1.49 GB in each sccache arm. The target dir after one cold build was 5.5 GB.
- **Hit counts:** identical in both repetitions for every step. Only the one miss in repetition 1's warm step differs.

**Per repetition** (seconds):

| Step | none r1 | none r2 | sccache r1 | sccache r2 | basedirs r1 | basedirs r2 |
|---|---:|---:|---:|---:|---:|---:|
| fresh-target-empty-cache | 576.9 | 268.6 | 360.9 | 429.6 | 405.3 | 605.2 |
| fresh-target-warm-cache | 261.0 | 331.3 | 146.8 | 115.9 | 145.4 | 140.7 |
| second-worktree | 345.8 | 370.5 | 342.6 | 269.0 | 339.8 | 418.5 |
| switch-to-change | 148.7 | 206.8 | 258.9 | 234.6 | 255.8 | 224.6 |
| switch-back | 116.2 | 137.5 | 62.7 | 64.7 | 68.8 | 59.5 |

## Reading

**[M] Measured here.**

1. **A fresh target dir at the same path, with a warm cache, builds in 131–143 s.** Without sccache, the six cold builds of the `none` arm took 261–577 s (mean 359 s).
   - Every compilation hits: 1212 of 1212.
   - What remains is the 227 non-cacheable calls (binaries, proc-macros, build scripts) and linking the embedded CPython.
   - This is the case sccache clearly wins: a `cargo clean`, or a target dir discarded at the same path.
2. **Returning to content already built (B→A) halves the rebuild:** 64 s against 127 s. All 42 compilations hit.
3. **Building content never seen before costs more with sccache:** 240–247 s against 178 s, about +35%. Every compilation misses, and sccache adds its hashing and cache writes on top. The Linux smoke measurement showed the same direction (+22%).
4. **A second worktree hits only 70% (854 of 1212), and its wall time does not clearly improve:**
   - `sccache`: 306 s, but 269–343 s across the two repetitions;
   - `sccache-basedirs`: 379 s;
   - `none`: 358 s.
5. **`SCCACHE_BASEDIRS` changes nothing here.** Both sccache arms recorded the same 854 hits and 358 misses in the second worktree. [I] The misses are likely compilations whose hash includes an absolute path in the environment, such as `CARGO_MANIFEST_DIR` for workspace members and `OUT_DIR` for crates with build scripts. Base directories normalize arguments and sources, not the environment. This was not verified crate by crate.

**Not concluded.**

6. **Cold builds without any cache are as noisy as in the dev-backend benchmark.** The same configuration took 261–577 s. Single steps with two repetitions carry wide error bars. Of the findings above, only 1, 2 and the hit counts are robust. 3 and 4 are directional.
7. **Only one host was measured,** a 16 GB laptop used as a CI runner, not a developer workflow.

## What this does and does not settle

- **Self-hosted CI runner, serving several PRs:**
  - sccache would pay only when a build returns to content it has already seen (finding 2).
  - Each new PR head is mostly new content, which pays the miss penalty (finding 3).
  - The persistent target dir already covers the common case.
  - Nothing here supports adding sccache to CI.
- **Local development with several worktrees:**
  - The expected win does not materialize as configured: 30% of compilations miss in a second worktree whatever `SCCACHE_BASEDIRS` says.
  - Anyone making it worthwhile would first have to identify and remove the path-dependent misses (finding 5), then measure again.
- **Clean rebuilds in the same checkout:** sccache helps clearly (finding 1).
- **Not settled:** the net effect for a real mix of hits and misses, and whether the second-worktree misses can be removed.
