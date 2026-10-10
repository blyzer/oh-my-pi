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
