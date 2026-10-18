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
