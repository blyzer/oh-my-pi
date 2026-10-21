# ACP handshake transcripts

Evidence for the ADR 0043 capability gate: what a real binary does when an ACP client talks to it.
Recorded by `scripts/acp-probe.py`, opt-in, never in CI. Raw JSON-RPC, not OMP's client.

| Binary | Version | Transcript | Initialize | Session/new | Prompt | Cancel | Permission |
|---|---|---|---|---|---|---|---|
| DeepSeek Harness `dsh --profile acp` | 0.2.0-rc.2 | [dsh-no-credential](dsh-no-credential.md) | ok | ok | stopped at an auth error | not exercised | not exercised |

## Findings that change OMP's client

These come from comparing the transcript with what `AcpExecutor` (`crates/envd/src/acp_client`) sends.

1. **`mcpServers` is required in `session/new`.** `dsh` rejects a request that omits it:
   `-32602 Invalid params, mcpServers: Required value is missing`. The ACP session-setup page lists the
   MCP server list as a parameter of every `session/new`, `session/load` and `session/resume`.
   `NewSessionParams` marks the field `skip_serializing_if = "slice_is_empty"`, so `AcpExecutor`
   sends `{"cwd": ...}` with no `mcpServers` and **would fail against dsh at `session/new`**. The
   fix is to always serialize the field, as `[]`.
2. **A missing credential is not signalled at `initialize`.** `dsh` returns `authMethods: []` and
   a session, then fails the *prompt* with `-32603 Internal error: turn failed: ... no API key for
   provider route "deepseek-official"`. `AcpExecutor` only reports `AuthRequired` for a non-empty
   `authMethods`, so for `dsh` it would surface a generic `Failure`, not `AuthRequired`.
3. **`dsh` offers `session/resume`** (`sessionCapabilities.resume`) and not `loadSession`.
   `AcpExecutor` only knows `session/load`, so resuming a `dsh` session is reported as unsupported.
4. `cwd` must be absolute and an unknown session id is rejected with `-32602`, both matching the
   spec and OMP's own checks.

## Not yet recorded

- A prompt that completes, `session/cancel` during a running turn, and a denied permission. All three
  need a working model credential; none exists on this machine (`DEEPSEEK_API_KEY` is unset and
  the keychain has no entry). Record them with `--pass-env DEEPSEEK_API_KEY` once one is available.
- The remaining binaries: Devin, Grok, Antigravity, Claude (bridge).
