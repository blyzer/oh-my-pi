# OMP² Native CLI Authentication and Provider Integration

Status: discovery/research; implementation not started for native CLI integration.
Date: 2026-10-10

## Current OMP² boundary

`omp-ai` exposes typed provider operations, catalog identities, credentials, streams and receipts. `omp-driver` composes the production stack. `envd` owns host process/document authorities; it is not a model-provider registry. The existing Apple integration (`crates/ai/src/local/applefm.rs`) is a native transport with typed chat events, cancellation and availability evidence.

A native provider CLI is not automatically a raw model transport. Codex, Claude Code and Gemini CLI can own their own agent loop, filesystem tools, approvals, sessions and MCP. Therefore the first contract should be a **delegated agent executor**, separate from direct API provider execution. Only a CLI that exposes a stable, machine-readable inference protocol with equivalent semantics may become an `omp-ai` transport.

## Official CLI evidence

### OpenAI Codex

Official docs:

- [`codex exec`](https://learn.chatgpt.com/docs/non-interactive-mode.md)
- [`codex app-server`](https://learn.chatgpt.com/docs/app-server.md)

Verified capabilities:

- `codex exec` is noninteractive and intended for scripts/CI.
- `--json` emits JSONL events including thread/turn/item/error/usage events.
- `resume` can continue a session.
- Sandbox and approval policy are explicit CLI flags.
- App-server provides bidirectional JSON-RPC over stdio, Unix socket or experimental WebSocket.
- App-server has thread/turn lifecycle, streamed notifications, approvals, cancellation and session resume.
- Saved CLI auth is reused by default, but the docs warn that auth files are credentials and must not be copied or exposed.

Recommendation: Codex app-server is the strongest first candidate. Treat it as a delegated agent backend, not a direct model provider. Use stdio/Unix socket locally; do not use unauthenticated non-loopback WebSockets.

### Anthropic Claude Code

Official Agent SDK documentation states that the SDK runs the Claude Code binary and provides built-in tools, permissions and sessions. It also explicitly says third-party products must not offer claude.ai login or rate limits without prior approval; API-key authentication is the supported default.

CLI subprocess mode can be considered only as execution delegation with the installed CLI's own permission/session behavior. OMP must not extract Claude tokens or claim delegated authentication. A native adapter must report `unknown` when the CLI exposes no safe non-destructive auth-status interface.

### Gemini CLI

Official headless documentation verifies:

- non-TTY or `-p` headless execution;
- JSON output with response/stats/error;
- JSONL output with init/session, message, tool_use, tool_result, error and result events;
- standard exit codes.

The current official page also states that Gemini CLI was replaced for unpaid/Google One users by Antigravity CLI on June 18, 2026. This is a distribution/product constraint, not evidence that Antigravity exposes the same protocol. Keep Gemini support capability-gated and investigate Antigravity separately.

### Google Antigravity

The available public documentation identifies Antigravity CLI as a TUI surface, but no verified machine-readable execution/session protocol was established in this pass. Leave it disabled until an official noninteractive interface and permission model are documented.

### xAI Grok and Devin

No verified official CLI contract was established in this pass. Do not create wrappers that pretend their HTTP APIs are CLIs. Existing direct provider/API integrations remain separate. Devin may be an asynchronous agent service rather than a token-streaming model provider and needs a distinct executor contract if official interfaces are found.

## Authentication versus execution

Supported modes must remain distinct:

```text
CliManagedExecution:
  OMP starts official CLI; CLI owns auth and provider calls.

DirectApiWithDelegatedAuth:
  OMP calls the provider API using an officially documented delegated credential.
```

The second mode must not be implemented by reading another application's Keychain, auth file or token cache. No OAuth extraction, decryption or reinterpretation is allowed.

## Architecture recommendation

Use a narrow `NativeCliAdapter`/delegated-executor seam in the existing driver/AI composition, reusing the existing process, cancellation, receipt and event infrastructure where possible. Do not add a new credential store or provider registry.

Initial Codex shape:

```text
omp-driver
  -> native CLI delegated executor
     -> codex app-server over stdio or Unix socket
        -> thread/start or thread/resume
        -> turn/start
        -> JSON-RPC notifications
        -> normalized delegated-agent events
```

The normalized event contract must preserve whether an event is:

- agent text;
- command/tool execution;
- approval request;
- usage;
- session lifecycle;
- cancellation/failure.

Do not flatten a full CLI agent into a fake raw model stream. If a future CLI exposes only final text, advertise no streaming/tool event capabilities rather than inventing them.

## Alternative assessment

| Alternative | Result |
|---|---|
| Native CLI as provider backend | Good for Codex app-server; only for CLIs with structured protocol and compatible lifecycle |
| CLI as authentication broker | Reject by default; only allowed for an official delegated-auth API, never token extraction |
| Hybrid direct API + native CLI | Recommended long term; explicit mode selection, no silent switching |
| One generic subprocess parser | Reject; human-readable output and agent semantics differ by CLI |
| HTTP gateway around each CLI | Reject unless an official CLI protocol forces it; adds process/security/serialization cost |

## First implementation candidate

Codex app-server, because its official protocol provides:

- initialize/initialized handshake;
- thread start/resume/fork;
- turn start/steer/interrupt;
- JSON-RPC JSONL transport;
- streamed item and agent-message events;
- approval and tool lifecycle;
- explicit model/cwd/sandbox options.

The first slice should be read-only or approval-constrained and must not duplicate OMP's own tools without an explicit policy decision.

## Blockers and open questions

- Whether OMP's product policy permits delegating an entire agent loop to Codex/Claude/Gemini.
- How OMP maps nested CLI tools and approvals without granting broader filesystem/network access.
- Whether a delegated executor belongs in `omp-driver` or a new `omp-agent-exec` library; decide after tracing current job/executor abstractions.
- Safe auth-status detection for each CLI.
- Exact Antigravity machine interface and licensing.
- Whether Claude's CLI/SDK terms permit this product integration; no claude.ai auth reuse is assumed.

Native CLI implementation started: **NO**.
