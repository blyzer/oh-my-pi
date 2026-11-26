# OMP Inference WebSocket Timeout — Diagnostic Prompt for Another LLM

## Problem Summary

The `omp` debug binary (`./target/debug/omp`) fails to start `omp chat` with this error:

```
inference DeadlineExceeded error during Streaming [http 101]: protocol violation (websocket-timeout)
```

The error persists even after:
- Logging in with OpenAI API key via `/login openai-codex`
- Selecting different models (e.g., `openai-codex/gpt-5.6-luna`)
- Rebuilding the binary multiple times
- Killing and restarting `omp-bridge` (production OMP v1 process)

## Environment

- **OS:** macOS (Apple Silicon)
- **Binary:** `./target/debug/omp` (version 2.0.0, ~802MB)
- **Config file:** `~/.o2/config.cfg`
- **Model configured:** `openai-codex/gpt-5.6-luna`
- **Provider:** `openai-codex` (OpenAI Codex subscription)
- **Credentials:** API key set via `/login openai-codex` in `omp chat`
- **Background process:** `omp-bridge` (production OMP v1 at `~/.local/bin/omp-bridge`)

## Config File (`~/.o2/config.cfg`)

```
ai_compaction_threshold_tokens 300
ai_extended_context true
ai_model_roles {default openai-codex/gpt-5.6-luna}
ai_provider_timeout_seconds 300
ai_search_order []
ai_thinking off
sv_credential_key_source local-file
sv_vault_enabled true
```

Plus BuildBuddy execution settings (unrelated to inference):
```
sv_exec_remote_profile buildbuddy
sv_sandbox_allow_domains [app.buildbuddy.io]
sv_exec_remote_credential "!security find-generic-password -s buildbuddy-omp2-trial -w"
```

## Error Flow

1. User runs `./target/debug/omp chat`
2. The TUI displays, showing `gpt-5.6-sol` or `easycliproxy` as the model (not the configured `openai-codex/gpt-5.6-luna`)
3. User types any prompt (e.g., `ping`, `!ls`)
4. The model attempts to connect via inference
5. Error: `inference DeadlineExceeded error during Streaming [http 101]: protocol violation (websocket-timeout)`
6. The chat becomes unresponsive — no further input works
7. If user exits and relaunches, the same error repeats

## Key Code Locations

### Error Origin
The error is generated in `crates/ai/src/transport/websocket_transport.rs` at line 519:

```rust
let error = pump_error(
    simple_error(ErrorKind::DeadlineExceeded, ErrorPhase::Streaming, emitted, "websocket-timeout"),
    &pump_attempt,
    &pump_evidence,
    status,
    pump_provider_request_id.as_ref(),
    started,
    emitted,
);
```

This means the transport layer is using **WebSocket mode** (`FramingProtocol::WebSocket`) and timing out while waiting for the first committed frame from the provider.

### Transport Selection
The transport layer has two modes:
- **HTTP transport** (`crates/ai/src/transport/http.rs`) — standard HTTP POST with SSE streaming
- **WebSocket transport** (`crates/ai/src/transport/websocket_transport.rs`) — bidirectional WebSocket for realtime operations

The error `[http 101]` indicates an HTTP 101 Switching Protocols response — the server upgraded to WebSocket. But then the WebSocket stream times out.

### Why WebSocket Is Used
The `FramingProtocol::WebSocket` framing is selected based on the provider catalog. Check:
- `crates/catalog/compat/providers/openai-codex.kdl` — provider definition
- The catalog determines whether a provider uses WebSocket or HTTP+SSE

### The `omp-bridge` Conflict
A production OMP v1 process (`omp-bridge`, PID ~7942) runs in the background:
- Located at `~/.local/bin/omp-bridge`
- Running since Oct 10, 2026 (194+ CPU hours)
- Manages inference for production OMP v1
- May intercept or conflict with debug build's inference requests

When killed, it auto-restarts (managed by launchd).

### Model Name Mismatch
The TUI shows `gpt-5.6-sol` or `easycliproxy` instead of the configured `openai-codex/gpt-5.6-luna`. This suggests:
- The catalog is not resolving `openai-codex/gpt-5.6-luna` correctly
- OR the `omp-bridge` process is overriding the model selection
- OR the credentials are routing to a different provider

## What to Investigate

### 1. Why Is WebSocket Being Used?
OpenAI's standard API uses HTTP POST with SSE streaming, not WebSocket. The `openai-codex` provider in the catalog may be configured for WebSocket, but the OpenAI API key should use HTTP+SSE.

Check `crates/catalog/compat/providers/openai-codex.kdl` — does it specify WebSocket framing?

### 2. Why Does the Model Name Not Match?
Config says `openai-codex/gpt-5.6-luna`, but TUI shows `gpt-5.6-sol` or `easycliproxy`. This means either:
- The catalog resolution is falling back to a default
- The `omp-bridge` process is intercepting and routing to a different model
- The credentials are not being applied correctly

### 3. Is `omp-bridge` Interfering?
The production `omp-bridge` process may be:
- Listening on the same ports or sockets
- Intercepting inference requests
- Routing to its own configured model (easycliproxy)

### 4. Credential Routing
After `/login openai-codex`, the API key is stored in `credentials.db`. But:
- Is the key being used for the correct provider?
- Does the debug build's auth manager correctly route to `openai-codex`?
- Or does it fall through to `omp-bridge`'s credentials?

### 5. The HTTP 101 Response
HTTP 101 means the server accepted a WebSocket upgrade. If OpenAI's API doesn't support WebSocket for this model, then:
- The request is going to the wrong endpoint
- OR the `omp-bridge` gateway is upgrading to WebSocket for its own processing

## Files to Examine

1. `crates/catalog/compat/providers/openai-codex.kdl` — Provider definition, framing protocol
2. `crates/ai/src/transport/websocket_transport.rs` — WebSocket transport, timeout logic
3. `crates/ai/src/transport/http.rs` — HTTP transport, framing selection
4. `crates/ai/src/call.rs` — Transport selection based on provider
5. `crates/ai/src/auth/manager.rs` — Credential routing
6. `crates/app/src/chat_services/accounts.rs` — Login flow
7. `~/.o2/credentials.db` — Stored credentials (if accessible)
8. `crates/ai/src/error.rs` — Error types and messages

## Expected Behavior

For OpenAI API key authentication:
1. `omp chat` should use HTTP POST to OpenAI's API
2. Streaming should use SSE (Server-Sent Events)
3. No WebSocket upgrade should occur
4. The configured model `openai-codex/gpt-5.6-luna` should be used

## Questions for the LLM

1. Why is the transport layer selecting WebSocket instead of HTTP+SSE for OpenAI?
2. How does the catalog determine framing protocol for a provider?
3. Could `omp-bridge` be intercepting and upgrading the connection to WebSocket?
4. How to force HTTP+SSE mode for the debug build?
5. How to verify the API key is being used correctly?
6. Is there a configuration option to disable WebSocket framing?
7. How to stop or isolate `omp-bridge` from interfering?

## Commands to Run for Diagnosis

```bash
# Check omp-bridge status
ps aux | grep omp-bridge

# Check what ports are in use
lsof -i -P | grep omp

# Check the catalog provider definition
cat crates/catalog/compat/providers/openai-codex.kdl

# Check envd logs for inference errors
cat ~/.local/share/omp/envd/*.log | grep -i "websocket\|timeout\|inference"

# Check credentials database
sqlite3 ~/.o2/credentials.db "SELECT * FROM credentials;"

# Try with explicit model
./target/debug/omp chat --model openai-codex/gpt-5.6-luna
```

## Context: BuildBuddy Extension Work

This inference issue is **separate** from the BuildBuddy extension work. The BuildBuddy extension is complete:
- ✅ `DelegatedBackend` adapter built and wired
- ✅ Correct BuildBuddy API endpoints
- ✅ 31/31 unit tests pass
- ✅ PR #713 merged
- ✅ Extension discovered via `.omp/extensions/buildbuddy` symlink

The live test for BuildBuddy requires `omp chat` to work, which is blocked by this inference issue. Fixing inference will unblock the BuildBuddy extension live test.
