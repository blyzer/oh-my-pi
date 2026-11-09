# Live BuildBuddy REAPI verification

## Scope and method

This audit covers the generic `ReapiBackend` in `crates/exec-reapi` against `grpcs://remote.buildbuddy.io` with an empty instance name. It is specific to the tested account and time.

The ignored integration tests use `Profile::buildbuddy()` and read `BUILDBUDDY_API_KEY` from the environment. They send the key in the `x-buildbuddy-api-key` gRPC metadata header. The tests construct empty input sets and upload only generated REAPI directory, command, and action messages. They do not upload the repository.

The requested platform has `OSFamily=linux`, `Arch=amd64`, and `workload-isolation-type=oci`. No `container-image` or `Pool` is specified. These are the requested properties, not a claim that the server accepted them.

The live tests use `CachePolicy::Off`. The echo and nonzero-exit actions request a 30-second timeout. The detach and cancel actions request 60 seconds. Cancellation uses the generic `CancelOperation` path, not BuildBuddy's feature-gated `CancelExecutions` RPC.

## Results

Recorded at 2026-11-09 03:37 UTC. The serial live run passed all five tests in 92.82 seconds. The prior capability-only run passed one test in 1.28 seconds.

| Test | Result | Observed evidence |
|---|---|---|
| `live_capabilities` | PASS | `GetCapabilities` returned OK. API versions were 2.0.0 through 2.11.0. `ReapiBackend` connected in 1.036 seconds and reported execution enabled, advertised ZSTD support for streaming and batch uploads, a 4,000,000-byte batch limit, no advertised blob limit, and read-write action cache support. |
| `live_execute_echo` | PASS | `Outcome::Exited { code: 0 }`; downloaded stdout was `live-reapi-echo\n`. The test took 1.897 seconds. |
| `live_execute_nonzero` | PASS | The typed outcome was `Outcome::Exited { code: 23 }`, not an execution error. The test took 1.201 seconds. |
| `live_detach_and_reattach` | PASS | The backend observed acceptance and execution, detached, dropped the first backend and stream, and waited three seconds. A new `ReapiBackend` connection reattached with the same opaque handle. It returned exit 0 and stdout `done\n` after 25.540 seconds from detach. The stream drop did not cancel the action. |
| `live_cancel` | PASS | After acceptance and execution, `cancel()` returned `Unsupported` at 1.652 seconds from the test start. The run later settled with `ExecutionError::TimedOut { limit: 60s }`, 60.442 seconds after the cancel call. |

The generic cancellation implementation maps gRPC `UNIMPLEMENTED` to `Unsupported`. The API does not expose the raw cancellation status message. The timeout result and elapsed time show that the action did not settle as cancelled; they do not provide continuous observation of the remote process.

The server accepted and ran actions with the requested platform properties. This does not independently establish the worker architecture or isolation enforcement. The backend reported `max_timeout=None`, which is its own capability representation, not a reported cloud maximum. The 60-second action was accepted and timed out, but the cloud maximum remains unknown.

The command below ran with full output rather than the host's condensed Cargo output:

```sh
CARGO_BUILD_JOBS=3 nice rtk bypass cargo test -p omp-exec-reapi --test live_buildbuddy -- --ignored --test-threads=1 --nocapture
```

`rustfmt --edition 2024 crates/exec-reapi/tests/live_buildbuddy.rs` completed before the run. Cargo emitted existing deprecation warnings in other crates and a missing-crate-documentation warning for the new integration test.


## ADR 0040 coverage and limits

- The short shell actions establish that raw REAPI execution works for this account. They do not settle the ADR's question about 15-20 minute actions or large inputs.
- The detach test supports Part I's `WaitExecution(name)` reattachment claim for a dropped stream and a new connection. It does not exercise process crash recovery or journal replay.
- The "whether ZSTD is enabled" item is settled at the capabilities-advertisement level. This run did not test compressed blob transfers.
- The "CancelOperation" item is settled for the tested endpoint and account at the backend interface. The observed outcome is `Unsupported`, which the generic implementation maps from `UNIMPLEMENTED`.


The ADR's [BuildBuddy coverage of REAPI](../adr/0040-remote-build-and-test-execution.md#buildbuddy-coverage-of-reapi) lists `Execute` and `WaitExecution`, `CancelOperation`, stdout and stderr digests, and `Action.timeout`. Its [Part I](../adr/0040-remote-build-and-test-execution.md#part-i--cancellation) says to reattach with `WaitExecution(name)` after disconnect and to map `UNIMPLEMENTED` cancellation to unsupported.

The [Not verified](../adr/0040-remote-build-and-test-execution.md#not-verified) list includes "maximum action timeout", "whether ZSTD is enabled", "live stdout streams", "CancelOperation", and "CancelExecutions outside the CLI". A successful short action does not establish the maximum timeout. Fetching stdout after settlement does not test live stdout streams. The feature-gated invocation cancellation RPC is outside this test.

The agent producer and `JobBoard::reattach_execution` were not exercised. That path requires an authorized execution and a journaled session, and `omp-agent` is not a dependency of the permitted `omp-exec-reapi` integration test. The existing agent test uses a fake backend, not this live server.

No managed arm64 executor, PTY, UI visibility, nested sandbox, output artifact, snapshot, or runner recycling behavior is tested here.
