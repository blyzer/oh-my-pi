# omp-exec-buildbuddy

BuildBuddy's REST v1 Run API backend for `omp-exec`.

## Contract

`BuildBuddyBackend` accepts a trusted `RunInputSource`. The source supplies a repository URL,
base commit, optional branch, and security-filtered Git patch bytes prepared by the environment
authority. The backend never reads the checkout and never obtains project credentials from `.git`.

A run:

1. submits `POST /api/v1/Run` with `steps`, `commitSha`, optional `branch`, base64-encoded
   `patches`, operator platform properties, and a bounded timeout;
2. emits `Accepted` and `Executing` for the invocation;
3. polls `GetInvocation` until the invocation is complete;
4. retrieves bounded, paginated `GetLog` pages; and
5. settles with the API's boolean success normalized to exit code `0` or `1`.

The public Run API does not expose an exact shell exit code. `1` means only that the remote
invocation was unsuccessful; it is not an infrastructure failure.

## Security

- The endpoint must be HTTPS. Plain HTTP is accepted only for loopback fake-server tests.
- The API key is a sensitive `x-buildbuddy-api-key` header and is never placed in argv, action
  environment, URLs, logs, or telemetry.
- Patch bytes and response logs are bounded before submission or storage.
- Commands are serialized from argv with single-quote escaping; shell strings are not accepted.
- REST cancellation is unavailable. Enabling `buildbuddy-cancel` lets the run use the separate
  invocation-scoped gRPC cancellation client from `omp-exec-reapi`.

The public Run API has no content-addressed output-file digests. `Metadata` suppresses log
retrieval; `Logs` and `All` retrieve bounded logs as output events; `Paths` and `All` cannot
materialize artifacts and therefore settle with an empty artifact list. REAPI remains the backend
for digest-addressed artifacts.

## Features

- `buildbuddy-cancel`: enables the optional `BuildBuddyService.CancelExecutions` integration.
- No action cache: the public Run API is not an REAPI action-cache interface.
- No warm-cache guarantee: runner reuse is service configuration, not a backend invariant.
