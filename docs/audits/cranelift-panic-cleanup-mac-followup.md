# Follow-up measurement: dev codegen backend on Apple silicon

A follow-up to [`cranelift-panic-cleanup.md`](cranelift-panic-cleanup.md). That
audit is closed and is not edited here. Its §7 left one decision to the owner:
should the dev `omp` binary also build with LLVM? It measured the cost only on a
4-core x86_64 host and asked for numbers from the team's aarch64 Macs. This
document records those numbers. It does not make the decision.

| | |
|---|---|
| **Date** | 2026-09-23 |
| **Run** | [Dev backend benchmark, run 35898527316](https://github.com/blyzer/oh-my-pi/actions/runs/35898527316), 17:57–20:11 UTC; results in its `bench-dev-backend` artifact (`meta.json`, `results.jsonl`, `summary.md`) |
| **Measured commit** | `c57359a77` on the experimental branch `claude/llvm-dev-benchmark`: `omp2` @ `58adc196f` plus the benchmark script and workflow only. No crate source or build configuration differs from `omp2`. |
| **Toolchain** | `nightly-2026-08-08` (as pinned), cargo-nextest 0.9.143 |
| **Host** | MacBook Pro `MacBookPro18,3` (M1 Pro: 8 performance + 2 efficiency cores), **16 GB RAM**, macOS 27.0; the self-hosted CI runner, on AC power; `pmset` reported no thermal or performance warning at the start |
| **Tool** | `scripts/bench-dev-backend.py` on that branch, 3 repetitions |

## Method

- Two variants, selected only on the command line:
  - `cranelift`: the repository's configuration, with members on Cranelift.
  - `llvm`: `--config 'profile.dev.codegen-backend="llvm"'`.
- Six scenarios per variant and repetition, run in this order:
  1. **cold**: an empty target dir, then `cargo build -p omp-app --bin omp`.
  2. **edit-core**: a one-line comment appended to `crates/core/src/lib.rs`, then a rebuild of `omp`.
  3. **edit-app**: the same with `crates/app/src/main.rs`.
  4. **test-after-dev**: `cargo nextest run -p omp-e2e --tests --no-run`, the first test build after the dev build.
  5. **loop-dev**: another `omp-core` edit, then a rebuild of `omp`.
  6. **loop-test**: another test build. Steps 5 and 6 together are one turn of the "`cargo run`, then `just test`" loop.
- Controls:
  - Variants alternate order per repetition (C,L; L,C; C,L).
  - Edited files are restored byte for byte.
  - Crates are fetched once, untimed.
  - `RUSTC_WRAPPER` is cleared, so no compiler cache takes part.
- The page cache cannot be dropped without root on macOS, and was not.
- Neither Spotlight nor other scanners were excluded from the target dirs.
- "Units" is the number of `Compiling` lines cargo printed.

## Results

Mean ± sd over 3 repetitions (seconds):

| Scenario | Cranelift | LLVM | LLVM vs Cranelift | Units C / L | Audit, Linux 4 vCPU |
|---|---:|---:|---:|---:|---:|
| cold | 535.0 ± 166.7 | 588.6 ± 242.5 | +10.0% | 944 / 944 | +1.2% |
| edit-core | 161.6 ± 2.5 | 174.7 ± 14.4 | **+8.1%** | 41 / 41 | +2.0% |
| edit-app | 40.5 ± 3.8 | 36.2 ± 3.2 | −10.5% | 1 / 1 | — |
| test-after-dev | 238.2 ± 59.7 | 165.0 ± 19.5 | **−30.7%** | **51 / 17** | +81 s for the LLVM test profile |
| loop-dev | 187.6 ± 42.0 | 152.4 ± 17.6 | −18.7% | 41 / 41 | — |
| loop-test | 165.4 ± 1.3 | 153.8 ± 15.9 | −7.0% | **41 / 11** | — |

- One loop turn (loop-dev plus loop-test): 352.9 s with Cranelift, 306.3 s with LLVM.
- Target dir after all scenarios: 10.4 GB with Cranelift, 9.6 GB with LLVM.

Per-repetition values for cold:

| Repetition | Order | Cranelift | LLVM |
|---|---|---:|---:|
| 1 | C, L | 684.5 | 726.1 |
| 2 | L, C | 355.3 | 308.6 |
| 3 | C, L | 565.2 | 730.9 |

## Reading

**[M] Measured here.**

1. **Test builds in a Cranelift dev tree recompile the members.** The first test build after a dev build compiles 51 units with Cranelift and 17 with LLVM. Each later turn compiles 41 against 11. The audit inferred this double compile; this run counts it directly.
2. **The first test build after a dev build costs about 73 s more with Cranelift** (238 s against 165 s). Two of three repetitions show it clearly (272.7 s and 272.6 s against 143–181 s). The audit measured +81 s for the same transition on Linux.
3. **A deep edit rebuilds `omp` about 13 s (+8%) slower with LLVM.** This is the most stable measurement here: Cranelift's sd is 2.5 s. It is larger than the +2% measured on Linux.
4. **Per loop turn, the test rebuild barely benefits.** With LLVM it compiles 30 fewer units but finishes only about 12 s (7%) faster. Linking the `omp-e2e` test binaries dominates that step, and each links the embedded CPython.

**Not concluded.**

5. **Cold builds carry no signal.** The same configuration took anywhere from 309 s to 731 s. Repetition 1 was slow for both variants and repetition 2 fast for both, so the spread comes from the host, not the backend. The loop-dev spread (sd 18–42 s) likely has the same cause.
   - **[I] Candidate causes, not verified:**
     - memory pressure: 16 GB for 10 parallel rustc processes and ~650 MB links;
     - Spotlight or endpoint-security scans of the fresh target dirs;
     - interactive use of the laptop during the run.

## What this does and does not settle

- **Settled [M]:** a homogeneous LLVM tree removes a considerable amount of recompilation between dev and test builds, and clearly speeds up the first dev→test transition.
- **Settled [M]:** it costs about 13 s after each deep edit when only `omp` is rebuilt.
- **Not settled:** cold build time, and the net effect per loop turn. To close the decision later, repeat only cold, loop-dev and loop-test, on an idle Mac. Spotlight exclusion (`*.noindex` target dirs) is a temporary experimental control there, not a project requirement. Endpoint-security scanners may still interfere even with Spotlight excluded.
