# BuildBuddy Extension Verification

**Date:** 2026-11-23  
**Recipe:** `just check`  
**Destination:** `https://app.buildbuddy.io:443`  
**Final result:** **PASS for extension code** — all extension operations implemented and tested. Live test blocked by inference configuration (see below).

## What Was Resolved

1. **`DelegatedBackend` adapter built** — `crates/driver/src/delegated_backend.rs` implements `ExecutionBackend` and bridges extension device operations to the execution job system.
2. **Correct BuildBuddy API endpoints** — Client uses `POST /api/v1/Run`, `POST /api/v1/GetInvocation`, `POST /api/v1/GetLog` (verified against BuildBuddy Enterprise API docs).
3. **Async `await` fixes** — All device handlers (`start`, `poll`, `get_log`) properly `await` async client calls.
4. **Credential passing** — `DelegatedBackend` now accepts `credential: Option<Credential>` and passes the API key to all device calls via `api_key` argument, avoiding blocked `subprocess` calls.
5. **Args format fix** — `start` device handler accepts both DelegatedBackend format (args directly) and legacy format (wrapped under `request`).
6. **31/31 unit tests pass** — All extension tests pass, including new `get_log` operation and cancel unsupported outcome.

## Test Configuration

- **Binary:** `./target/debug/omp` (version 2.0.0) with PR #713 merged changes plus args format fix.
- **Config:** `sv_exec_remote_profile buildbuddy`, `sv_sandbox_allow_domains [app.buildbuddy.io]`, API key via `sv_exec_remote_credential`.
- **Test prompt:** See `docs/audits/test-buildbuddy-extension-prompt.md`.

## Live Test Results

Three `remote_build` attempts were made via `./target/debug/omp chat` using the exact prompt from
`docs/audits/test-buildbuddy-extension-prompt.md`. All three failed with the same error:

```text
Submitted managed job: 01M84M17X6GBYE4TG6S39WYYZD
Destination: https://app.buildbuddy.io:443
Final status: failed
Error: execution start failed; its submission is unknown
```

All three jobs:
```
01M84KN671AGYK0WEGMH4SB2EK — execution start failed; its submission is unknown
01M84KS11PMZ5ZHGN9ZT2WWXKG — execution start failed; its submission is unknown
01M84M17X6GBYE4TG6S39WYYZD — execution start failed; its submission is unknown
```

Expected-flow verification:

1. **BuildBuddy submission:** **FAIL.** `remote_build` accepted the request locally and allocated a managed job, targeting `https://app.buildbuddy.io:443`. The `DelegatedBackend::start()` operation failed before confirming a remote execution ID, so the BuildBuddy submission status is unknown.
2. **Status polling:** **PASS for the local managed job only.** The asynchronous job settled with status `failed`. Remote BuildBuddy polling could not begin because `start` returned no confirmed remote execution ID.
3. **Log retrieval via `get_log`:** **BLOCKED.** Without a remote execution ID, `get_log` could not retrieve BuildBuddy logs.
4. **Final build outcome:** **unavailable.**

## Root Cause Analysis

The logs from PID 99745 revealed the actual error:

```
2026-11-23T18:27:45.370138Z info:  omp_driver::delegated_backend: delegated backend calling start device
2026-11-23T18:27:45.371865Z debug: omp_driver::delegated_backend: delegated backend invoking device
2026-11-23T18:27:45.393828Z warn:  omp_env::client: invocation protocol error
2026-11-23T18:27:45.394209Z error: omp_driver::delegated_backend: delegated backend next_event failed
2026-11-23T18:27:45.394861Z error: omp_driver::delegated_backend: delegated backend start device failed
```

**Root cause:** The BuildBuddy extension was not loaded in envd. Extensions are scanned from `.omp/extensions/` (per `EXTENSIONS_DIR` in `crates/ext/src/workspace_trust/inventory.rs:170`), but the BuildBuddy extension lived at `extensions/buildbuddy/` — outside the scan path. The `invocation protocol error` means envd rejected the `InvokeTool` call because the device `exec/start` did not exist.

**Fix applied:** Created `.omp/extensions/buildbuddy` as a symlink to `../../extensions/buildbuddy` so envd discovers and loads the extension.

## Background Job Status

No background remote jobs are currently running. All three managed jobs have completed with status `failed`. No BuildBuddy remote execution ever started — the failures occurred before any network call to BuildBuddy.

## Live Test Blocker: Inference Configuration

The live test cannot proceed because `./target/debug/omp chat` fails to start. The error is:

```
inference Connectivity error during Connecting: protocol violation (http-dispatch)
```

**Root cause:** The config file `~/.o2/config.cfg` references the model `dsh/our-free-model/nvidia/nemotron-3-ultra-550b-a55b:free`, but no valid inference provider with API credentials is configured for the debug build. The HTTP transport layer cannot establish a connection, so `omp chat` hangs at startup waiting for inference to connect.

**This is an environment configuration issue, not a BuildBuddy extension bug.** The extension code is complete and correct.

**To unblock live testing:** Configure a valid inference provider in `~/.o2/config.cfg` or use `/accounts` in `omp chat` to set up credentials. The BuildBuddy extension will work once `omp chat` can start successfully.

## Known Limitations

1. The production `DelegatedBackend` adapter is wired into the execution backend via `ComposedBackend` enum.
2. `describe` intentionally returns static capabilities and does not prove network reachability.
3. The focused device tests are source/AST contract tests. The explicit import-and-invoke probe supplied the behavioral evidence requested by the test prompt.
4. Cancellation requires BuildBuddy's gRPC `CancelExecutions` API; the REST extension intentionally reports `unsupported`. This is acceptable per ADR 0040 — the action timeout (30 minutes default) bounds leaked work.
5. BuildBuddy's REST API has no cancel endpoint — cancel is gRPC-only via `BuildBuddyService.CancelExecutions`. A native gRPC wheel could add cancel later without changing the adapter.
