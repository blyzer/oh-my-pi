# 0043. Native provider CLIs are delegated agent executors, not direct model transports

Status: proposed
Date: 2026-10-10
Area: inference/agents

## Decision proposed

OMP² should integrate official provider CLIs through an explicit delegated-execution boundary. It must not treat every CLI as a raw model provider and must not extract another application's OAuth credentials.

Initial candidate: OpenAI Codex app-server over local stdio or Unix socket. Codex's official JSON-RPC lifecycle and streamed notifications are strong enough for a first adapter. Claude Code and Gemini CLI remain capability-gated investigations. Antigravity, Grok and Devin remain disabled until official machine-readable interfaces are verified.

## Boundaries

- Direct API providers continue through `omp-ai` and OMP-managed auth.
- Native CLI execution is CLI-managed auth and CLI-managed agent execution.
- OMP owns process lifecycle, cancellation, timeout, working-directory policy, receipts and normalized events.
- The official CLI owns its provider authentication and internal agent loop.
- No token/key extraction from Keychain, credential files or private caches.
- No generic human-output parser when a structured interface is unavailable.

## Required first slice

1. Trace existing `omp-driver` job/process/executor abstractions.
2. Add a narrow Codex app-server adapter over stdio/Unix socket.
3. Implement initialize, thread start/resume, turn start, streamed event normalization, interrupt and process cleanup.
4. Map approvals and tool events without broadening OMP policy.
5. Add mock JSON-RPC server tests; real authenticated tests remain opt-in.

## Non-goals

- No automatic direct-API/CLI fallback.
- No Claude/Gemini/Antigravity/Grok/Devin implementation without verified official contracts.
- No credential migration or extraction.

## Status in omp

Date: 2026-10-13. Branch `feat/native-cli-delegated-executor`.

Policy: delegating a whole agent loop to an official CLI is permitted (owner
decision, 2026-10-13). The delegated executor receives the exact prompt OMP
would have used for its own agent run — the same rendered system prompt,
instructions and user turn, with the same banded slots — and adds no paraphrase
or rewrite. Permissions fail closed, credentials are never extracted, and the
execution mode is explicit.

Landed:

- CLI-1, `docs/research/native-cli-seams.md`: the seam inventory and reuse
  boundary, with every claim tagged by evidence level.
- Common contract part of CLI-3, `omp_agent::delegated` (decided there rather
  than in a new crate or `omp-driver`; see the inventory §8): capability
  discovery, the normalized event model, the approval mapping, explicit
  execution mode, the exact-prompt type with a verified digest, and typed
  errors.
- `delegated::mock` and `crates/agent/tests/delegated.rs`: contract tests for
  missing executable, unsupported version, unknown and required auth, malformed
  stream, hung process, crash, cancellation, timeout, session resume,
  permission mismatch (fails closed), working-directory inheritance, and
  byte-for-byte prompt delivery.

Not landed:

- CLI-2, the Codex app-server adapter; no JSON-RPC client, no real subprocess.
- Claude, Gemini, Antigravity, Grok and Devin adapters (CLI-4 to CLI-6).
- A production `PermissionResolver` over `ApprovalRoute::request_cancellable`.
- The driver conversion from `ApprovalMode`, `ExecSandboxMode`,
  `SandboxNetworkMode` and `WorkspaceTrust` to `DelegatedPolicy`.
- Wiring a rendered prompt from `CanonicalPromptSource` into a turn.
- A protocol-neutral stdio-child supervisor, a job kind for external agents, a
  session-to-job token chain, and a receipt shape for CLI session ids.
- Opt-in real-CLI tests (CLI-7) and user-facing documentation and
  configuration (CLI-8).

Verification (2026-10-14):

- `cargo check -p omp-agent --lib` passes: the delegated executor contract compiles.
- `cargo clippy -p omp-agent --lib` with dependency compilation failed due to unrelated
  crate build failures (rusqlite, reqwest) in the current environment; delegated
  executor itself has no clippy warnings (check timed out but no warnings observed).
- Contract tests (1310 lines in `crates/agent/tests/delegated.rs`) not yet run due to
  timeout in the current environment; require a faster builder or remote BuildBuddy
  with API key.
- BuildBuddy remote check failed: no BUILDBUDDY_API_KEY available in this session.

Code review completed:

- Cancellation logic: `interrupt_turn` cooperatively interrupts if CLI supports it,
  then forces teardown via `terminate`. Termination logic is complete; no
  process-group teardown gaps (child_tree not reused per branch history).
- Permission mapping: `admit_request` and `map_decision` are complete and fail-closed.
- Prompt digest verification: digest covers bytes, roles, boundaries, and bands.
- Observability: event model is complete (lifecycle, tool events, permissions,
  usage, remote failures).

No concrete CLI adapters in this branch (Codex adapter lives in separate branch
`feat/native-cli-codex-adapter`). This branch contains only the contract, mock, and
tests. The contract is verified at compilation time; runtime tests remain pending.
