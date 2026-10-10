# OMP² native CLI integration next steps

Status: research/proposed architecture; native CLI implementation not started.
Reports: `docs/research/native-cli-integration.md`, `docs/adr/0043-native-cli-integration.md`.

GitHub Issues are disabled; this parked document is the repository task convention.

## Dependencies

```text
CLI/provider discovery
  -> delegated-executor contract
  -> Codex app-server adapter
  -> event/approval/security validation
  -> optional Claude/Gemini adapters
```

## Tasks

### CLI-1 — Inventory current execution seams

Trace `omp-driver`, `omp-agent`, job lifecycle, cancellation, subprocess/envd authorities,
receipts, observability and existing provider transport contracts. Acceptance: exact reuse
boundary and no duplicate process/credential manager.

### CLI-2 — Codex app-server adapter

Implement only after CLI-1. Use stdio or Unix socket JSON-RPC; support initialize, thread
start/resume, turn start, streamed notifications, interrupt and process cleanup. Never copy
Codex auth files. Acceptance: deterministic mock server tests and explicit capability report.

### CLI-3 — Delegated event and approval policy

Normalize agent text, command/tool events, approvals, usage, failure and session lifecycle
without flattening an agent into a fake raw model stream. Map CLI permissions to OMP policy or
fail closed when semantics cannot be mapped.

### CLI-4 — Claude Code feasibility

Verify official headless JSON/stream/session interface and commercial/auth terms. Do not offer
claude.ai login or rate limits; do not extract credentials. Implement only if the official
execution contract and policy permit it.

### CLI-5 — Gemini/Antigravity feasibility

Verify Gemini JSON/JSONL headless behavior and the current Antigravity replacement/interface.
Keep unsupported modes disabled and documented.

### CLI-6 — Optional provider adapters

Investigate xAI and Devin official CLI/agent interfaces. Represent Devin as an asynchronous
agent executor if that is its actual semantic model; do not force token-streaming semantics.

### CLI-7 — Security and lifecycle tests

Mock missing executable, unsupported version, unknown auth status, auth-required, malformed
JSONL, hung process, crash, cancellation, timeout, session resume, permission mismatch and
working-directory inheritance. Add opt-in real CLI tests only with explicit credentials.

### CLI-8 — Documentation and configuration

Document direct API versus CLI-managed execution, supported versions, installation, auth
ownership, permissions, sessions, platforms, limitations and troubleshooting. Configuration
must make execution mode explicit; no silent switching.
