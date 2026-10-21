# ACP handshake transcript: Grok CLI (grok agent --no-leader stdio), not signed in

Recorded 2026-10-21 00:43 UTC by `scripts/acp-probe.py`. Raw JSON-RPC, not OMP's client.

- Command: `<real-home>/.grok/bin/grok agent --no-leader stdio`
- Version: grok 1.0.46 (2765805b9442)
- Isolation: fresh temporary HOME, cleared environment; passed through: none; set: none
- Permission requests from the agent are answered with a rejection.

## Steps

| Step | Expected | Observed |
|---|---|---|
| initialize | result with protocolVersion | ok |
| session/new (absolute cwd, mcpServers []) | result with sessionId | error -32000: Authentication required |
| session/new (relative cwd) | error: cwd must be absolute | error -32602: Invalid params |
| session/prompt (unknown session) | error | error -32602: Invalid params |
| unknown method | error -32601 | error -32601: Method not found |
| close stdin | agent exits | exit code 0 |

## Frames

```text
-> {"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": 1, "clientCapabilities": {}, "clientInfo": {"name": "omp-acp-probe", "version": "0"}}}
<- {"jsonrpc":"2.0","id":1,"result":{"protocolVersion":1,"agentCapabilities":{"loadSession":true,"promptCapabilities":{"image":false,"audio":false,"embeddedContext":true},"mcpCapabilities":{"http":true,"sse":true},"sessionCapabilities":{"list":{},"resume":{},"close":{}},"auth":{},"_meta":{"x.ai/fs_notify":true,"x.ai/hooks":{"blockingEvents":["pre_tool_use","stop","subagent_stop"],"decisions":["deny","block"],"stopSignals":["continue","stopReason","additionalContext"]},"x.ai/capabilities":{"toolOverrides":{"x_keyword_search":true,"x_semantic_search":true,"x_user_search":false,"x_thread_fetch":false}}}},"authMethods":[{"id":"grok.com","name":"Grok","description":"Sign in with Grok"}],"_meta":{"grokShell":true,"defaultAuthMethodId":null,"x.ai/mcp/sdk":true,"x.ai/pluginDirs":true,"currentWorkingDirectory":"/private<home>/work","agentVersion":"1.0.46","agentId":"1fd70e3c-9bc2-5773-8592-d53e5b6e1eb3","agentInstanceId":"69764835-9eec-4df1-b3dc-87bd940c232e","hostname":"Envers-MacBook-Pro.local","modelState":{"currentModelId":"grok-4.6","availableModels":[{"modelId":"grok-4.6","name":"Grok 4.6","description":"SpaceXAI's latest frontier model","_meta":{"totalContextTokens":500000,"agentType":"grok-build-plan","supportsReasoningEffort":true,"reasoningEffort":"high","reasoningEfforts":[{"id":"xhigh","value":"xhigh","label":"Extra High Effort","description":"Highest effort and reasoning level","default":false},{"id":"high","value":"high","label":"High Effort","description":"Higher implementation quality with extensive reasoning","default":true},{"id":"medium","value":"medium","label":"Medium Effort","description":"Balanced effort with standard implementation and testing","default":false},{"id":"low","value":"low","label":"Low Effort","description":"Quick, fast implementations","default":false}]}},{"modelId":"grok-4.5","name":"Grok 4.5","_meta":{"totalContextTokens":500000,"agentType":"grok-build-plan","supportsReasoningEffort":true,"reasoningEffort":"high","reasoningEfforts":[{"id":"high","value":"high","label":"High Effort","description":"Highest implementation quality with extensive reasoning","default":true},{"id":"medium","value":"medium","label":"Medium Effort","description":"Balanced effort with standard implementation and testing","default":false},{"id":"low","value":"low","label":"Low Effort","description":"Quick, fast implementations","default":false}]}}]},"mcpServers":[],"mcpApps":false,"metadata":null,"availableCommands":[{"name":"compact","description":"Compress conversation history to save context window","input":null},{"name":"always-approve","description":"Toggle always-approve mode (skip all permission prompts)","input":{"hint":"on|off"}},{"name":"context","description":"Show context window usage and session stats","input":null},{"name":"session-info","description":"Show session details (model, turns, context usage)","input":null},{"name":"deep-research","description":"Research with bounded parallel agents, cross-check evidence, and write a cited report","input":{"hint":"<query>"}},{"name":"workflow","description":"Launch a saved workflow, list runs, or manage a run (pause, resume, stop, save)","input":{"hint":"<name> [--agent-budget N] [--effort LEVEL] [args] | runs | pause|resume|stop|save [name]"}},{"name":"goal","description":"Set, manage, or check an autonomous goal","input":{"hint":"<objective> [--budget <tokens>] | status | pause | resume | clear"}}],"cancelRewind":true,"sessionRecap":true,"feedbackTraceOffer":false,"voiceMode":true}}}
-> {"jsonrpc": "2.0", "id": 2, "method": "session/new", "params": {"cwd": "<home>/work", "mcpServers": []}}
<- {"jsonrpc":"2.0","method":"_x.ai/session/setup","params":{"method":"session/new","phase":"auth","sessionId":null}}
<- {"jsonrpc":"2.0","method":"_x.ai/session/setup","params":{"method":"session/new","phase":"resolve_workspace","sessionId":null}}
<- {"jsonrpc":"2.0","method":"_x.ai/session/setup","params":{"method":"session/new","phase":"folder_trust","sessionId":null}}
<- {"jsonrpc":"2.0","method":"_x.ai/session/setup","params":{"method":"session/new","phase":"plugin_registry","sessionId":null}}
<- {"jsonrpc":"2.0","method":"_x.ai/session/setup","params":{"method":"session/new","phase":"mcp_merge","sessionId":null}}
<- {"jsonrpc":"2.0","method":"_x.ai/session/setup","params":{"method":"session/new","phase":"persistence_init","sessionId":"01a15be9-7f38-7451-ad41-1c90e96518fe"}}
<- {"jsonrpc":"2.0","method":"_x.ai/mcp/servers_updated","params":{"mcpServers":[]}}
<- {"jsonrpc":"2.0","id":2,"error":{"code":-32000,"message":"Authentication required","data":"no auth method id provided"}}
-> {"jsonrpc": "2.0", "id": 3, "method": "session/new", "params": {"cwd": "relative/path", "mcpServers": []}}
<- {"jsonrpc":"2.0","method":"_x.ai/session/setup","params":{"method":"session/new","phase":"spawn_session_actor","sessionId":"01a15be9-7f38-7451-ad41-1c90e96518fe"}}
<- {"jsonrpc":"2.0","id":3,"error":{"code":-32602,"message":"Invalid params","data":"Path is not absolute: relative/path"}}
-> {"jsonrpc": "2.0", "id": 4, "method": "session/prompt", "params": {"sessionId": "no-such-session", "prompt": [{"type": "text", "text": "x"}]}}
<- {"jsonrpc":"2.0","method":"_x.ai/session/setup","params":{"method":"session/new","phase":"auth","sessionId":null}}
<- {"jsonrpc":"2.0","method":"_x.ai/session/setup","params":{"method":"session/new","phase":"resolve_workspace","sessionId":null}}
<- {"jsonrpc":"2.0","id":4,"error":{"code":-32602,"message":"Invalid params","data":"unknown session id"}}
-> {"jsonrpc": "2.0", "id": 5, "method": "session/does-not-exist", "params": {}}
<- {"jsonrpc":"2.0","id":5,"error":{"code":-32601,"message":"Method not found"}}
```

## Agent stderr

```text
(empty)
```
