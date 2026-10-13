# omp-agent

Agent kernel: the turn loop, central tool dispatch, cancellation tree, and Directors, all folding into `omp-session`.

Environment `DoneProjected` terminals carry source-line `ProjectionSpan`
candidates alongside their bounded model parts. Central byte and line bounds
produce the final sorted, deduplicated `VisibilityReceipt`; only completely
retained rows are returned to the exact registered tool revision through
`Registry::authorize_visibility`. A receipt with no retained rows is still
returned when candidates were supplied. Terminals from older producers with
no candidates keep the existing no-authorization behavior. Typed verdict
decode failures and authority rejection propagate as dispatch errors.
For externally routed calls the executor also acknowledges that receipt with
the owning environment host. The host checks the exact revision and typed
verdict, refuses receipt lines absent from the tool's projection, and invokes
the registered authority. This application-only DATA operation requires
document-read authority and is unavailable to extension-host connections.

## Delegated executors (`delegated`)

`omp_agent::delegated` is the backend-neutral contract for running an
official third-party agent CLI (ADR 0043) as a *delegated agent executor*.
It is not a model transport: the CLI owns its agent loop, tools, sessions and
login; OMP owns process lifecycle, cancellation, timeouts, working-directory
policy and the normalized event stream. It never implements `Inference`, is
never a catalog provider, and is never flattened into a token stream.

Structure: vocabulary (`DelegatedEvent`, `Capabilities`, `DelegatedPolicy`,
`PermissionRequest`, `ExecutorError`), two backend traits
(`DelegatedExecutor`, `DelegatedSession`), and a host driver
(`start_session`, `ActiveSession::run_turn`) written once over the traits so
every backend gets identical negotiation and failure handling. `delegated::mock`
is a deterministic in-memory backend; `tests/delegated.rs` holds the contract
tests. No adapter for a real CLI lives here, and nothing here spawns a process.

Rules the types enforce rather than document:

- **Explicit mode.** `ExecutionMode::{DirectApi, CliManaged}` is chosen by the
  caller; a delegated executor only offers `CliManaged`, and a `DirectApi`
  request is an error, never a reroute.
- **Exact prompt.** The executor receives the prompt OMP would have used for
  its own run (rendered system items, volatile band, user turn, band digests) as
  a `DelegatedPrompt`. It forwards those bytes verbatim and echoes the digest of
  what it forwarded; any difference ends the session (`PromptRewritten`).
- **No credentials.** No contract type carries a credential, auth path or
  environment map. The CLI owns its login; OMP only observes `AuthStatus`, and
  `Unknown` is refused unless the request opts in.
- **Fail closed.** A policy the executor does not list verbatim is refused
  (`PolicyUnmappable`); a backend that applies a different policy than
  requested is torn down (`PolicyDrift`); a permission request the session
  policy forbids or the CLI did not classify is denied without asking anyone; an
  approval is only ever narrowed (`map_decision`), never widened; an event
  needing an unadvertised feature is a contract violation.
- **Observation-only tool events.** Nothing in the event vocabulary authorizes
  OMP to run anything (contrast `ChatEvent::ToolCallReady`).

The policy axes mirror `ExecSandboxMode`, `SandboxNetworkMode` and
`ApprovalMode` from `omp-envd`, which depends on this crate; the driver
converts after applying `effective_approval_mode`. Seams the contract reuses and
those a real adapter must still close are in
`docs/research/native-cli-seams.md`.

Philosophy: the contract is small and typed so that the dangerous parts
(policy, approvals, prompt fidelity, teardown) are decided once, in the host
driver, and not re-decided by each CLI adapter.
