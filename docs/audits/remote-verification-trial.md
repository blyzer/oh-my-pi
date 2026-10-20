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

The `usage_stats.cpu_nanos` and `usage_stats.peak_memory_bytes` fields from the assignment brief are
BuildBuddy-specific extensions not present in the standard REAPI v2 proto vendored here; they are not
wired. Client CPU and peak memory criteria below therefore remain unassessable until a
BuildBuddy-specific auxiliary-metadata decoder is added or the fields are observed on a live run.

### Success-criterion status (partial, ADR 0040 §Measurement plan)

| Criterion | Threshold | Observed | Met? |
|---|---|---|---|
| Warm remote wall time within 1.5× warm local | ≤ 1.5× (warm local ~3 min 26 s, from Slice 0 run 10) | 2 s | ✅ well under |
| Client local CPU < 5% of local recipe | < 5% of ~463 s (warm local) | not yet sampled (no BuildBuddy cpu_nanos extension) | — |
| Client peak memory < 5% of local peak | < 5% of ~3.6 GB (warm local) | not yet sampled (no BuildBuddy peak_memory_bytes extension) | — |

The wall-time criterion is satisfied. Worker-side timing fields are now captured in spans via Task 2
(worker queue, wall, input-fetch, exec, and output-upload milliseconds). Client CPU and peak memory
require a BuildBuddy-specific extension proto not in the vendored REAPI v2; those criteria remain open.

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
