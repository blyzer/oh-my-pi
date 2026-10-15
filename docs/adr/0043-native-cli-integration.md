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

Date: 2026-10-13. Branch `feat/native-cli-codex-adapter`.

Policy: delegating a whole agent loop to an official CLI is permitted (owner
decision, 2026-10-13). The delegated executor receives the exact prompt OMP
would have used for its own agent run — the same rendered system prompt,
instructions and user turn, with the same banded slots — and adds no paraphrase
or rewrite. Permissions fail closed, credentials are never extracted, and the
execution mode is explicit.

Landed:

- CLI-1, `docs/research/native-cli-seams.md`: the seam inventory and reuse
  boundary, with every claim tagged by evidence level.
- Common contract part of CLI-3, `omp_agent::delegated`: capability
  discovery, the normalized event model, the approval mapping, explicit
  execution mode, the exact-prompt type with a verified digest, typed errors,
  and `delegated::mock` with `crates/agent/tests/delegated.rs`.
- Shared process-group teardown, `omp-envd` `child_tree`, used by the MCP stdio
  transport and the Codex adapter.
- CLI-2, the Codex app-server adapter, `omp_envd::codex_app_server`
  (`CodexExecutor`, `CodexSession`; see the crate README): typed JSON-RPC wire
  structs for `initialize`, `thread/start|resume`, `thread/inject_items`,
  `turn/start|interrupt`, lifecycle/delta/item/usage/error notifications and the
  command, file-change and permission approvals; a capped, cancel-safe line
  reader; a non-blocking writer; request/response correlation; exact policy
  mapping with the applied policy reported back; prompt delivery with a digest
  of the bytes written; approvals narrowed to single-use; crash, hang,
  interrupt, kill-the-tree and drop handling on `child_tree`. Tests use an
  in-process mock JSON-RPC peer over in-memory pipes.

Not landed:

- Claude, Gemini, Antigravity, Grok and Devin adapters (CLI-4 to CLI-6).
- A production `PermissionResolver` over `ApprovalRoute::request_cancellable`.
- The driver conversion from `ApprovalMode`, `ExecSandboxMode`,
  `SandboxNetworkMode` and `WorkspaceTrust` to `DelegatedPolicy`, and the
  executor's registration in the driver or app (nothing constructs
  `CodexExecutor` yet).
- Wiring a rendered prompt from `CanonicalPromptSource` into a turn.
- A job kind for external agents, a session-to-job token chain, and a receipt
  shape for CLI session ids.
- Opt-in real-CLI tests (CLI-7) and user-facing documentation and
  configuration (CLI-8). No test in this change starts a real `codex`.

Known limits and open questions (feasibility items U2-U5 are still not
answered by a live server; the mock peer encodes the documented/generated
shapes, not observed behavior):

- Login is reported `Unknown`: no probe that avoids credential state exists.
- Codex has no system-prompt field on `turn/start`; system sections are
  injected with `thread/inject_items` as developer messages. A prompt that
  is not system-sections-then-user-sections is rejected as `PromptRewritten`.
- Approval policy `write` and network `scoped` have no exact Codex equivalent
  and are `PolicyUnmappable`.
- Whether the policy in `ThreadStartResponse` is the post-merge effective one
  (U4) is unverified; the adapter trusts it only as far as comparing it with
  the request.

Verification of the delegated-executor contract branch (2026-10-14):

- `cargo check -p omp-agent --lib` passed on that branch.
- Its `cargo clippy -p omp-agent --lib` and contract tests were not completed
  in that environment; the tests still require an observed run before the
  contract can be considered verified.
- Review covered cancellation, fail-closed permission mapping, prompt digest,
  and normalized events. Review is not a substitute for the contract tests.
- The contract branch adds no CLI adapter; the Codex adapter listed above was
  merged separately from `feat/native-cli-codex-adapter`.
