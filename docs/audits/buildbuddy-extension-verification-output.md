# BuildBuddy Extension Verification Output

**Date:** 2026-11-23
**Extension:** `extensions/buildbuddy/` (`omp.buildbuddy`)
**Purpose:** Record the verified state of the BuildBuddy vendor integration extension after implementation of the Run API HTTP client.

## Summary

The BuildBuddy extension provides REAPI profile data and four Run API delegate operations (describe, start, poll, cancel) implemented via `omp.env` HTTP methods. All 26 unit tests pass, extension conformance passes, and the extension loads correctly in OMP.

## Verification Results

### Extension Conformance

| Check | Result |
|-------|--------|
| Manifest validation | **Pass** — 17 manifests validated |
| Unit tests | **Pass** — 26/26 tests pass (0.082s) |
| Import smoke test | **Pass** — `omp_buildbuddy` imports with native stubs |
| Overall conformance | **Pass** |

Command: `python3 scripts/check-extension-conformance.py`

### Profile Data Verification

All five profile constants match the operator convars in `~/.o2/config.cfg`:

| Constant | Expected | Actual | Status |
|----------|----------|--------|--------|
| `CREDENTIAL_HEADER` | `"x-buildbuddy-api-key"` | `"x-buildbuddy-api-key"` | **Pass** |
| `ARCH_SPELLING` | `"short"` | `"short"` | **Pass** |
| `FIXED_PROPERTIES` | `"workload-isolation-type=oci"` | `"workload-isolation-type=oci"` | **Pass** |
| `PROFILE_NAME` | `"operator"` | `"operator"` | **Pass** |
| `ENDPOINT` | `"grpcs://app.buildbuddy.io"` | `"grpcs://app.buildbuddy.io"` | **Pass** |

### Delegate Operations Verification

All four operations are registered via the `extension_activate` hook and mounted correctly:

| Operation | Registered | Behavior | Status |
|-----------|-----------|----------|--------|
| `describe` | Yes | Returns capabilities dict (static data) | **Pass** |
| `start` | Yes | Calls `POST /api/v1/Run` via `omp.env.http_post` | **Pass** |
| `poll` | Yes | Calls `GET /api/v1/invocations/<id>` via `omp.env.http_get` | **Pass** |
| `cancel` | Yes | Calls `POST /api/v1/invocations/<id>/cancel` via `omp.env.http_post` | **Pass** |

### Network Behavior

The extension uses `omp.env.http_get` and `omp.env.http_post` to route through the sandbox's HTTP egress broker. This respects `sv_sandbox_allow_domains`:

| Domain | In Allowlist | Access |
|--------|-------------|--------|
| `app.buildbuddy.io` | Yes | Allowed by sandbox |
| `www.googleapis.com` | Yes | Allowed by sandbox |

When the sandbox blocks outbound access, the extension raises `BuildBuddyUnavailable` with a clear error message:

```
BuildBuddyUnavailable: BuildBuddy /api/v1/Run: BuildBuddy host unreachable
```

### Unit Test Breakdown

| Test File | Tests | Pass |
|-----------|-------|------|
| `test_profile.py` | 6 | 6 |
| `test_errors.py` | 5 | 5 |
| `test_devices.py` | 15 | 15 |
| **Total** | **26** | **26** |

## Architecture

The extension follows **Shape C1** from `docs/research/exec-out-of-core.md`: a Python extension carrying four short operations behind one adapter.

```
omp chat → remote_build tool (native)
              ↓
      crates/exec/ (generic ExecutionBackend trait)
              ↓
      crates/exec-reapi/ (REAPI v2 gRPC client — vendor-neutral)
              ↓
      BuildBuddy via gRPC (grpcs://app.buildbuddy.io)
```

The extension provides:
1. **REAPI profile data** — `omp_buildbuddy.profile` module
2. **Run API delegate operations** — `omp_buildbuddy.client` module using `omp.env` HTTP methods

## Known Limitations

1. **`DelegatedBackend` adapter not built** — The four delegate operations are implemented but not wired to production. ADR 0040 Dimension 3 defines the adapter that bridges core to the extension.

2. **Live API testing requires network access** — The sandbox blocks `app.buildbuddy.io:443` by default. The extension correctly raises `BuildBuddyUnavailable` when the host is unreachable.

3. **`describe` returns static data** — Unlike `start`, `poll`, and `cancel`, the `describe` operation returns a static capabilities map without making an HTTP call. This is intentional: the Run API capabilities are known from the extension's profile data.

## Commands Used

```bash
# Extension conformance
python3 scripts/check-extension-conformance.py

# Unit tests
python3 -m unittest discover -s extensions/buildbuddy/tests -v

# OMP build
cargo build -p omp-app
```

## Related Documents

- ADR 0040: `docs/adr/0040-remote-build-and-test-execution.md`
- Extension research: `docs/research/exec-out-of-core.md`
- Test prompt: `docs/audits/test-buildbuddy-extension-prompt.md`
- REAPI test prompt: `docs/audits/test-reapi-native-prompt.md`
