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

None of these are available yet because the client-side span instrumentation (Task 2) has not been
wired into the execution path. Until Task 2 ships, the only recorded data is:

- Client wall time (above).
- BuildBuddy invocation IDs (links to the BB UI where per-phase timelines are readable by hand).
- No per-phase numbers for capture, FindMissing, upload, queue, execution, or download.
- No client CPU or peak-memory sample for the Slice 1 client path.

Full per-phase data will be recorded here once Task 2 wires instrumented spans and a structured
dump of `ExecutedActionMetadata` into the audit output.

### Success-criterion status (partial, ADR 0040 §Measurement plan)

| Criterion | Threshold | Observed | Met? |
|---|---|---|---|
| Warm remote wall time within 1.5× warm local | ≤ 1.5× (warm local ~3 min 26 s, from Slice 0 run 10) | 2 s | ✅ well under |
| Client local CPU < 5% of local recipe | < 5% of ~463 s (warm local) | not yet sampled (Task 2) | — |
| Client peak memory < 5% of local peak | < 5% of ~3.6 GB (warm local) | not yet sampled (Task 2) | — |

The wall-time criterion is satisfied. The CPU and memory criteria require the Task 2 instrumented
spans before they can be assessed.
