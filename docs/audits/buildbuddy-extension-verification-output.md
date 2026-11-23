# BuildBuddy Remote Build Verification Output

**Date:** 2026-11-23  
**Recipe:** `just check`  
**Destination:** `https://app.buildbuddy.io:443`  
**Final result:** **FAIL — credential not passed to extension (FIXED)**

## Remote Build Output

### Attempts 1-2: Credential Missing (Before Fix)

`remote_build` accepted the request but failed because the `DelegatedBackend` adapter did not pass the BuildBuddy API key to the extension. The extension's `client.py` tried to read the key via `subprocess.run(["security", "find-generic-password", ...])`, which is **blocked by the sandbox**.

```text
Attempt 1: job 01M84JDT6CHYCWM7HNR4RVMT9G — execution start failed
Attempt 2: job 01M84JP58MBGTT28CE6CW2TCG3 — execution start failed (transport)
```

## Root Cause

The `DelegatedBackend` adapter was created without the BuildBuddy API key credential. The extension's `client.py` tried to read the key via `subprocess.run(["security", "find-generic-password", ...])`, which is **blocked by the sandbox**. The `start` device failed before it could make the HTTP request to BuildBuddy.

## Fix Applied

1. **`crates/driver/src/delegated_backend.rs`** — `DelegatedBackend::new()` now accepts `credential: Option<Credential>`. The credential is passed to all device calls (`start`, `poll`, `get_log`) via the `api_key` argument.
2. **`crates/driver/src/exec_composer.rs`** — `ExecutionSettings::connect()` passes the resolved `Credential` to `DelegatedBackend::new()` when the profile is `"buildbuddy"`.
3. **`extensions/buildbuddy/src/omp_buildbuddy/client.py`** — `_api_key()` now accepts the API key from request args instead of calling `subprocess.run`. Falls back to `BUILDBUDDY_API_KEY` env var for testing.
4. **`extensions/buildbuddy/src/omp_buildbuddy/devices.py`** — Device handlers pass `api_key` from args to client functions.

## Verification

- `just check-pkg omp-driver` — compiles cleanly
- `python3 -m pytest tests/ -v` — 31/31 tests pass
- `just build` — OMP binary rebuilt successfully (commit 6ccd007990)

## Sandbox Allowlist Check

The active `~/.o2/config.cfg` contains:

```text
sv_exec_remote_endpoint "grpcs://app.buildbuddy.io"
sv_exec_remote_profile buildbuddy
sv_sandbox_allow_domains [app.buildbuddy.io]
```

Therefore, `app.buildbuddy.io` is already present in `sv_sandbox_allow_domains`.

## Next Steps

Run `./target/debug/omp chat` and trigger a remote build via the `remote_build` tool to verify the credential fix works end-to-end. The binary now includes the credential passing code (commit 6ccd007990).

## Known Limitations

1. The production `DelegatedBackend` adapter is wired into the execution backend via `ComposedBackend` enum.
2. A live BuildBuddy API request has not yet succeeded — the credential fix was applied after the last failed attempt. The next live test will verify whether the fix resolves the transport failure.
3. `describe` intentionally returns static capabilities and does not prove network reachability.
4. The focused device tests are source/AST contract tests. The explicit import-and-invoke probe supplied the behavioral evidence requested by the test prompt.
5. Cancellation requires BuildBuddy's gRPC `CancelExecutions` API; the REST extension intentionally reports `unsupported`. This is acceptable per ADR 0040 — the action timeout (30 minutes default) bounds leaked work.
6. **Live network test ready.** `~/.o2/config.cfg` sets `sv_exec_remote_profile buildbuddy` and `sv_sandbox_allow_domains [app.buildbuddy.io]`. The binary includes the credential fix. A live test can now proceed by running `./target/debug/omp chat` and triggering a remote build.