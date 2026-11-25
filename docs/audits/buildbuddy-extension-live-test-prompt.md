# BuildBuddy Extension Live Test Prompt

Use this prompt in `./target/debug/omp chat` to test the BuildBuddy extension via the `DelegatedBackend` adapter.

## Prerequisites

- Binary: `./target/debug/omp` (version 2.0.0) with PR #713 merged.
- Config (`~/.o2/config.cfg`):
  - `sv_exec_remote_profile buildbuddy` — selects `DelegatedBackend`.
  - `sv_sandbox_allow_domains [app.buildbuddy.io]` — allows network through sandbox.
  - `sv_exec_remote_credential` — BuildBuddy API key configured.

## Prompt

```
Use the remote_build tool to run "just check" on the current repository.
This should use the BuildBuddy extension via the DelegatedBackend adapter
to submit the run to app.buildbuddy.io, poll for completion, and return
the result. Verify that:

1. The run is submitted successfully to BuildBuddy
2. The run status is polled correctly
3. The logs are retrieved via get_log
4. The final result shows the build outcome

If the run fails with a network error, check that app.buildbuddy.io is
in the sandbox allowlist (sv_sandbox_allow_domains).
```

## Expected Flow

1. `remote_build` tool receives the request.
2. `ComposedBackend::Delegated` is selected because profile is `"buildbuddy"`.
3. `DelegatedBackend::start()` calls the extension's `start` device → `POST /api/v1/Run`.
4. `DelegatedRun::next_event()` polls the extension's `poll` device → `POST /api/v1/GetInvocation`.
5. Logs are fetched via the extension's `get_log` device → `POST /api/v1/GetLog`.
6. Final result is returned with the build outcome.

## What to Watch For

- **Network blocked:** If you see `BuildBuddyUnavailable: network blocked by sandbox`, the allowlist isn't active — verify `~/.o2/config.cfg` has `sv_sandbox_allow_domains [app.buildbuddy.io]`.
- **Authentication errors:** Verify the BuildBuddy API key is configured via `sv_exec_remote_credential`.
- **Cancel unsupported:** Cancel will return `unsupported` — this is expected and acceptable per ADR 0040 (REST API has no cancel endpoint).

## Capture Results

After the test completes, capture the output (success or error) and update `docs/audits/buildbuddy-extension-verification-output.md` with the actual results in the "Live Test Results" section.
