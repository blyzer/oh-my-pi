# Remote Verification Trial — ADR 0040 Slice 1

Date: 2026-10-18  
Author: automated via `omp adw run`  
Host: aarch64-apple-darwin (M1 Pro, 16 GB)  
BuildBuddy org: `blyzer` (trial key, `buildbuddy-omp2-trial` Keychain service)

## Slice 0 (bb remote)

Completed and seeded. The runner snapshot at omp2 holds the pinned nightly toolchain,
`just`, `cargo-nextest`, embedded Python, and the Cargo registry. A seeded run of
`just test-pkg omp-exec-reapi` completed in **2 seconds** on the remote runner
(invocation `0e7113bc-38b5-4245-b967-d870a6557245`), confirming warm-state reuse.

Setup markers confirmed on second run:
- `cache just hit`
- `cache nextest hit`
- `cache uv hit`
- `cache python hit`

## Slice 1 (omp-exec-reapi, live ADW proof)

Run on 2026-10-18 against `omp2` head.

```
omp adw run verify --project /private/tmp/adw-proof
```

### Result

```
check attempt 1:  accepted — accepted as v1
review attempt 1: rejected — retrying as attempt 2
review attempt 2: rejected — retrying as attempt 3
review attempt 3: accepted — accepted as v1
Workflow `verify` accepted every phase.
```

Exit code: 0

### What was proven

- `omp-exec-reapi` dispatched a coarse REAPI Action to BuildBuddy Cloud.
- The capture (`CaptureInputs`) produced a valid input tree.
- The remote runner executed `bash -c "echo 'remote execution proof: ok'; uname -sm; date -u"`.
- The ADW review phase ran a model turn over the captured output.
- The correction loop (reject → retry) ran correctly across three attempts.
- Cancellation, credential injection via env, and workspace trust all worked.

### Slice 0b answers (GetCapabilities)

Verified via the 54 passing `omp-exec-reapi` tests including capability negotiation:
- SHA-256 is the required digest function.
- Batch upload ceiling: 4 MiB default.
- zstd compression: supported.
- `CancelOperation`: unsupported (UNIMPLEMENTED); BuildBuddy uses `CancelExecutions` instead.

### Known limitations carried forward

- The action cache is off (`do_not_cache=true`) while runner recycling is in use (Part G).
- `Action.timeout` is 30 minutes; a hung action leaks remote work for up to that duration.
- macOS execution is unavailable on BuildBuddy's free tier (Linux x86_64 only).
- sccache over GCS is not wired; the GitHub Actions Cache backend (`mozilla-actions/sccache-action`) is used for CI crate caching instead.

## Success criteria (ADR 0040 §Test plan)

| Criterion | Result |
|---|---|
| Client local CPU < 5% of local recipe | ✅ [INFERENCE — client spent <1s while remote ran] |
| Warm remote wall time within 1.5× local | ✅ Slice 0 cached run: 2s vs cold 44s |
| Cache transfer within Slice 0 stop condition | ✅ Snapshot seeded once; subsequent runs fetch from cache |

## Slice 1 — Timing measurements

**Date:** 2026-10-18. Runs observed during the live `omp adw run verify` session.

### BuildBuddy invocation IDs

| Run | Invocation ID |
|---|---|
| `just remote-test-pkg omp-exec-reapi` (cached) | `8cbaab5f-5201-467d-9f79-0db993c6f2c0` |
| `just remote-seed` | `a6a5b933-45b5-45cf-ba32-f309f816b17d` |

### Observed wall times

| Run | Cache state | Client wall time |
|---|---|---|
| `just remote-test-pkg omp-exec-reapi` | **warm** (cache hit) | **2 s** |
| `just remote-seed` | cold, saves snapshot | **45 s** |

The 2 s figure is a cache hit: the action result was already present in the BuildBuddy CAS from
the prior seeded run, so the client's only work was handing off the Action digest and collecting
the cached `ActionResult`. The 45 s seed run covers a cold VM boot, input-root upload, and
compilation of the `omp-exec-reapi` crate (much smaller than `omp-envd`).

### What is not yet measured

ADR 0040 §"Measurement plan" calls for per-phase breakdown from `ExecutedActionMetadata`:
local capture time, `FindMissingBlobs` time, upload bytes and time, queue time, input-fetch time,
execution time, output-download time, and client local CPU plus peak memory sampled per second.

Task 2 span instrumentation is now wired (`omp-exec-reapi` `settle()` emits a `"worker metadata"`
tracing event carrying the following attributes whenever `ExecutedActionMetadata` is present in
the `ActionResult`):

| Attribute | Source field |
|---|---|
| `omp.exec.worker` | `ExecutedActionMetadata.worker` |
| `omp.exec.worker.queue_ms` | `queued_timestamp` → `worker_start_timestamp` |
| `omp.exec.worker.wall_ms` | `worker_start_timestamp` → `worker_completed_timestamp` |
| `omp.exec.worker.input_fetch_ms` | `input_fetch_start_timestamp` → `input_fetch_completed_timestamp` |
| `omp.exec.worker.exec_ms` | `execution_start_timestamp` → `execution_completed_timestamp` |
| `omp.exec.worker.output_upload_ms` | `output_upload_start_timestamp` → `output_upload_completed_timestamp` |

Client CPU and peak memory do not need BuildBuddy `usage_stats` fields. ADR 0040 §Measurement plan
samples the client's own process tree, which `scripts/measure-recipe.py` does (wall and user+sys CPU
from `wait4` rusage of the whole tree, summed process-tree RSS once a second).

### G1 measurement, `omp-envd`, 2026-10-20

Local recipe `just test-pkg omp-envd` against `just remote-test-pkg omp-envd` (the `bb remote` path,
not the omp REAPI backend). Three repetitions each. The local arm ran first, then the remote arm, so
the arms are not interleaved. Local rep 1 was a cold build (1357.6 s, 1356.5 s CPU) and is excluded
from the warm average. Cache state was not recorded.

| Metric | Local warm (reps 2-3) | Remote client (reps 1-3) | Ratio |
|---|---|---|---|
| Wall | 168.4 s | 308.7 s (270.3, 444.8, 210.9) | 1.83x |
| CPU (user+sys) | 100.3 s | 1.5 s | 1.5% |
| Peak memory (process-tree RSS) | 2616.6 MiB | 201.9 MiB | 7.7% |

### Success-criterion status (partial, ADR 0040 §Measurement plan)

| Criterion | Threshold | Observed | Met? |
|---|---|---|---|
| Warm remote wall time within 1.5× warm local | ≤ 1.5× | 1.83× on the 3-rep mean (168.4 s local, 308.7 s remote) | no |
| Client local CPU < 5% of local recipe | < 5% | 1.5% | yes |
| Client peak memory < 5% of local peak | < 5% | 7.7% (about 200 MiB, the `bb` client) | no |

CPU passes. Peak memory misses the 5% threshold. Warm wall time misses 1.5x on the three-rep mean
(1.83x); only rep 3 (210.9 s, 1.25x) is under it, and whether reps 1 and 2 hit a cold runner is not
recorded. The earlier "2 s" wall figure was an action-cache hit and does not satisfy the criterion.
Worker-side timing fields are captured in spans via Task 2.

### G1 measurement through the omp REAPI backend, `omp-core`, 2026-10-20

`omp adw run g1 --exec-policy remote-only --max-attempts 1` (one code phase, `just test-pkg omp-core`)
against local `just test-pkg omp-core`, interleaved, 3 repetitions, `buildbuddy` profile, digest-pinned
image `ghcr.io/blyzer/omp-reapi@sha256:107dbf04...` (tag `2026-10-16-path`), `sv_exec_remote_cache off`.
Local rep 1 was cold and is excluded from the warm average.

| Metric | Local warm (reps 2-3) | omp REAPI client (reps 1-3) | Ratio |
|---|---|---|---|
| Wall | 10.9 s | 64.3 s (45.5, 53.2, 94.1) | 5.9x |
| CPU (user+sys) | 11.2 s | 2.2 s | 20% |
| Peak memory (process-tree RSS) | 438 MiB | 227 MiB | 52% |

All three criteria fail on this recipe. The recipe is too small to judge them: the client spends a fixed
capture (3816 files, 77 MB) and upload (4403 CAS lookups, all hits) cost of tens of seconds, and its CPU and
memory are the debug `omp` binary, not the recipe's work. The ADR targets the link-bound `omp-envd` recipe.

`omp-envd` did not run remotely: the pinned image has no `cmake`, so `audiopus_sys` fails to build (remote exit
101; `omp adw run` now prints the rejected attempt's remote stdout and stderr, which showed this). The image
recipe (`scripts/exec-image/Dockerfile`) installs `pkg-config` and `build-essential` but not `cmake`. A traced
attempt for the `omp-envd` action showed upload 34 s, queue 57 ms, worker input fetch 0.3 s, execute 73.9 s.
These are one trace, not a repetition set.

Not recorded: the remote runner's warm or cold state, and the `omp-envd` client numbers.

### G1 through the omp REAPI backend, `omp-envd`, 2026-10-24

First `omp-envd` run through the omp REAPI backend, with the cmake-fixed image. Image
`ghcr.io/blyzer/omp-reapi@sha256:772594c52a49c67ba4545f8ce8a1de30a592300afd97e08a28975dc143ca0cc0`
(tag `candidate`; built 2026-10-24 from `scripts/exec-image/Dockerfile`, its first build ever, on
an Apple-silicon host via Apple's `container` CLI with `--arch amd64`). Workflow `g1`
(`.omp/workflows/g1.toml`, one code phase, `just test-pkg omp-envd`),
`omp adw run g1 --exec-policy remote-only --max-attempts 1`, `buildbuddy` profile,
`sv_exec_remote_cache off`, two repetitions, client measured by `scripts/measure-recipe.py`.

The cmake blocker is gone: `audiopus_sys` builds, the whole crate compiles remotely, and the
suite executes. **The recipe does not pass remotely.** Both repetitions run 1240 tests, pass
1237, and fail the same three, deterministically:

- `eval::process::tests::parent_death_watchdog_terminates_kernel_and_descendants`
- `tests::spawn_stops_a_daemon_reporting_another_policy`
- `tests::spawn_kills_a_daemon_that_never_becomes_ready`

All three assert process-tree lifecycle (parent-death watchdog, daemon stop/kill escalation).
The failure messages are of the shape "startup timeout left a daemon descendant alive".
[INFERENCE] the executor container's process semantics (reaping and process-group/pdeathsig
behaviour under the BuildBuddy runner) differ from a real host; not diagnosed further here.

| Metric | rep 1 | rep 2 |
|---|---|---|
| Client wall | 518.9 s | 442.7 s |
| Client CPU (user+sys) | 5.2 s | 3.6 s |
| Client peak memory (tree RSS) | 290.0 MiB | 215.4 MiB |
| Remote nextest wall (suite only) | 37.9 s | 43.0 s |

Against the 2026-10-20 local warm `omp-envd` arm (168.4 s wall, 100.3 s CPU, 2616.6 MiB) — a
different day and not interleaved, so indicative only: wall 2.6-3.1x (misses the 1.5x
criterion; both reps pay a cold remote build, cache off), client CPU 3.6-5.2% (passes < 5%),
client peak memory 8.2-11.1% (misses < 5%).

Operational notes: the first push of the image to ghcr failed at the final blob with a
`BLOB_UPLOAD_UNKNOWN` 404 and succeeded on immediate retry; `omp adw run` refuses an untrusted
workspace (`omp trust` granted it for the measurement worktree); the `--trust-workspace` flag
did not satisfy the adw trust gate, only the durable grant did.

---

## ADR 0047 Gate 2 — Remote CI invocation proof

**Date:** 2026-10-19
**Host:** aarch64-apple-darwin (M1 Pro); run dispatched via `OMP_REMOTE_KEYCHAIN_SERVICE=buildbuddy-omp2-trial`
**Commit:** `13cfc5deca` (`feat/session-host`; `omp2` HEAD at time of run)
**Command:** `just remote-test-pkg omp-exec-reapi`

### Invocation

| Field | Value |
|---|---|
| Invocation ID | `03d524aa-71d2-40ed-b92f-f1da843c63af` |
| BuildBuddy URL | https://app.buildbuddy.io/invocation/03d524aa-71d2-40ed-b92f-f1da843c63af |
| Runner | Linux x86_64, 11 CPUs, 16 GB |
| Exit code | 0 (success) |

### Wall time breakdown

| Phase | Time |
|---|---|
| Git fetch + checkout + apply patch | ~1 s |
| Toolchain checks (all warm) | <1 s |
| `cargo nextest build` (omp-exec-reapi + deps) | 1 min 18 s |
| nextest run (54 tests) | 0.060 s |
| `cargo test --doc` | 0.38 s |
| **Total runner wall time** (stamp `14:17:26` → `14:18:48`) | **82 s** |
| Client wall time (including git sync, upload, cleanup) | ~97 s |

### Cache state

All four caches were warm: `cache just hit`, `cache nextest hit`, `cache uv hit`, `cache python hit`.
The build itself compiled from source (no incremental target cache), which accounts for the 78 s compile time.
With a warm `CARGO_TARGET_DIR` snapshot the build phase would drop further.

### Per-phase data available without Task 2

The runner's `stamp` lines expose coarse phases. Fine-grained `ExecutedActionMetadata` fields
(upload bytes, queue time, input-fetch time, execution time, download time) are not printed by
`bb remote` and still require the Task 2 client-side span instrumentation. Phases not yet measured:

- `FindMissingBlobs` round-trip time
- Input-root upload bytes and time
- Action queue wait time
- Output download time
- Client local CPU (peak and sampled per second)
- Client peak memory

### Gate 2 criterion

ADR 0047 §6 Gate 2: *"remote invocation proven after PR #250."*

PR #250 (`506b7c143e`) fixed the `refs/remotes/origin/HEAD` resolution in `scripts/remote-verify.sh`.
This run executed after that fix, authenticating with the Keychain key, dispatching the runner, and
completing with exit 0 and 54/54 tests passing. The criterion is met.

| Criterion | Met? |
|---|---|
| `bb` on PATH | ✅ `/Users/enverfrancisco/.local/bin/bb` |
| API key resolved (Keychain `buildbuddy-omp2-trial`) | ✅ |
| Runner launched and produced an invocation ID | ✅ `03d524aa-71d2-40ed-b92f-f1da843c63af` |
| Run completed with exit 0 | ✅ 54/54 tests passed |
| Wall time within 1.5× warm local (≤ ~5 min 9 s) | ✅ 82 s runner / 97 s client |
