# ACP handshake transcript: DeepSeek Harness (dsh --profile acp), no credential

Recorded 2026-10-21 00:00 UTC by `scripts/acp-probe.py`. Raw JSON-RPC, not OMP's client.

- Command: `/Applications/DeepSeek Harness.app/Contents/Resources/runtime/cli/bin/dsh --profile acp`
- Version: 0.2.0-rc.2
- Isolation: fresh temporary HOME, cleared environment; passed through: none; set: DSH_HOME
- Permission requests from the agent are answered with a rejection.

## Steps

| Step | Expected | Observed |
|---|---|---|
| initialize | result with protocolVersion | ok |
| session/new (absolute cwd, mcpServers []) | result with sessionId | ok |
| session/new (relative cwd) | error: cwd must be absolute | error -32602: Invalid params: cwd must be an absolute path: relative/path |
| session/prompt (unknown session) | error | error -32602: Invalid params: unknown session: no-such-session |
| unknown method | error -32601 | error -32601: "Method not found": session/does-not-exist |
| session/prompt then session/cancel after 2.0s | result with stopReason cancelled, or an error | error -32603: Internal error: turn failed: llm-deepseek: no API key for provider route "deepseek-official"; store DEEPSEEK_API_KEY through the credentials |
| close stdin | agent exits | exit code 0 |

## Frames

```text
-> {"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": 1, "clientCapabilities": {}, "clientInfo": {"name": "omp-acp-probe", "version": "0"}}}
<- {"jsonrpc":"2.0","id":1,"result":{"protocolVersion":1,"agentInfo":{"name":"deepseek-harness-acp","version":"0.0.1"},"agentCapabilities":{"mcpCapabilities":{"http":true},"promptCapabilities":{"image":false,"audio":false,"embeddedContext":false},"sessionCapabilities":{"close":{},"list":{},"resume":{}}},"authMethods":[]}}
-> {"jsonrpc": "2.0", "id": 2, "method": "session/new", "params": {"cwd": "<home>/work", "mcpServers": []}}
<- {"jsonrpc":"2.0","id":2,"result":{"sessionId":"2df07a75-7903-4962-bf7c-7fb691e0cc8f","configOptions":[{"id":"model","name":"Model","category":"model","type":"select","currentValue":"[\"deepseek-official\",\"deepseek-v4-flash\"]","options":[{"group":"deepseek-official","name":"DeepSeek","options":[{"value":"[\"deepseek-official\",\"deepseek-v4-flash\"]","name":"deepseek-v4-flash"},{"value":"[\"deepseek-official\",\"deepseek-flash\"]","name":"DeepSeek-V41-Flash"},{"value":"[\"deepseek-official\",\"deepseek-v4-pro\"]","name":"DeepSeek-V4-Pro","description":"Stronger agentic coding, knowledge, and difficult reasoning; suited to complex or quality-critical tasks at higher cost."}]}]},{"id":"reasoning_effort","name":"Reasoning effort","category":"thought_level","type":"select","currentValue":"high","options":[{"value":"off","name":"Off","description":"Use for simple tasks that do not need reasoning."},{"value":"low","name":"Low","description":"Prefer for routine or latency-sensitive tasks."},{"value":"high","name":"High","description":"The default balance for most tasks."},{"value":"max","name":"Max","description":"Reserve for the hardest quality-first tasks."}]}]}}
-> {"jsonrpc": "2.0", "id": 3, "method": "session/new", "params": {"cwd": "relative/path", "mcpServers": []}}
<- {"jsonrpc":"2.0","id":3,"error":{"code":-32602,"message":"Invalid params: cwd must be an absolute path: relative/path"}}
-> {"jsonrpc": "2.0", "id": 4, "method": "session/prompt", "params": {"sessionId": "no-such-session", "prompt": [{"type": "text", "text": "x"}]}}
<- {"jsonrpc":"2.0","id":4,"error":{"code":-32602,"message":"Invalid params: unknown session: no-such-session"}}
-> {"jsonrpc": "2.0", "id": 5, "method": "session/does-not-exist", "params": {}}
<- {"jsonrpc":"2.0","id":5,"error":{"code":-32601,"message":"\"Method not found\": session/does-not-exist","data":{"method":"session/does-not-exist"}}}
-> {"jsonrpc": "2.0", "id": 6, "method": "session/prompt", "params": {"sessionId": "2df07a75-7903-4962-bf7c-7fb691e0cc8f", "prompt": [{"type": "text", "text": "Reply with the single word: ok"}]}}
-> {"jsonrpc": "2.0", "method": "session/cancel", "params": {"sessionId": "2df07a75-7903-4962-bf7c-7fb691e0cc8f"}}
<- {"jsonrpc":"2.0","id":6,"error":{"code":-32603,"message":"Internal error: turn failed: llm-deepseek: no API key for provider route \"deepseek-official\"; store DEEPSEEK_API_KEY through the credentials service (the web Models page writes it), or export DEEPSEEK_API_KEY in the launching environment"}}
```

## Agent stderr

```text
(empty)
```
