# ACP handshake transcript: DeepSeek Harness dsh --profile acp, account route, default permission mode

Recorded 2026-10-21 23:04 UTC by `scripts/acp-probe.py`. Raw JSON-RPC, not OMP's client.

- Command: `/Applications/DeepSeek Harness.app/Contents/Resources/runtime/cli/bin/dsh --profile acp`
- Version: 0.2.0-rc.2
- Isolation: fresh temporary HOME, cleared environment; passed through: none; set: DSH_HOME
- Permission requests from the agent are answered with a rejection.

## Steps

| Step | Expected | Observed |
|---|---|---|
| initialize | result with protocolVersion | ok |
| session/new (absolute cwd, mcpServers []) | result with sessionId | ok |
| session/set_config_option model | result | ok |
| session/prompt that needs a tool | permission requests, each answered with a rejection | ok — permission requests from the agent: 0, stopReason=end_turn, probe.txt created: yes |
| session/new (relative cwd) | error: cwd must be absolute | error -32602: Invalid params: cwd must be an absolute path: relative/path |
| session/prompt (unknown session) | error | error -32602: Invalid params: unknown session: no-such-session |
| unknown method | error -32601 | error -32601: "Method not found": session/does-not-exist |
| session/prompt then session/cancel after 2.5s | result with stopReason cancelled, or an error | ok — stopReason=cancelled |
| close stdin | agent exits | exit code 0 |

## Frames

```text
-> {"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": 1, "clientCapabilities": {}, "clientInfo": {"name": "omp-acp-probe", "version": "0"}}}
<- {"jsonrpc":"2.0","id":1,"result":{"protocolVersion":1,"agentInfo":{"name":"deepseek-harness-acp","version":"0.0.1"},"agentCapabilities":{"mcpCapabilities":{"http":true},"promptCapabilities":{"image":false,"audio":false,"embeddedContext":false},"sessionCapabilities":{"close":{},"list":{},"resume":{}}},"authMethods":[]}}
-> {"jsonrpc": "2.0", "id": 2, "method": "session/new", "params": {"cwd": "<home>/work", "mcpServers": []}}
<- {"jsonrpc":"2.0","id":2,"result":{"sessionId":"<uuid-1>","configOptions":[{"id":"model","name":"Model","category":"model","type":"select","currentValue":"[\"deepseek-official\",\"deepseek-v4-flash\"]","options":[{"group":"deepseek-official","name":"DeepSeek","options":[{"value":"[\"deepseek-official\",\"deepseek-v4-flash\"]","name":"deepseek-v4-flash"},{"value":"[\"deepseek-official\",\"deepseek-flash\"]","name":"DeepSeek-V41-Flash"},{"value":"[\"deepseek-official\",\"deepseek-v4-pro\"]","name":"DeepSeek-V4-Pro","description":"Stronger agentic coding, knowledge, and difficult reasoning; suited to complex or quality-critical tasks at higher cost."}]},{"group":"deepseek-account","name":"DeepSeek Account","options":[{"value":"[\"deepseek-account\",\"deepseek-flash\"]","name":"DeepSeek-V41-Flash"},{"value":"[\"deepseek-account\",\"deepseek-v4-pro\"]","name":"DeepSeek-V4-Pro","description":"Stronger agentic coding, knowledge, and difficult reasoning; suited to complex or quality-critical tasks at higher cost."}]}]},{"id":"reasoning_effort","name":"Reasoning effort","category":"thought_level","type":"select","currentValue":"high","options":[{"value":"off","name":"Off","description":"Use for simple tasks that do not need reasoning."},{"value":"low","name":"Low","description":"Prefer for routine or latency-sensitive tasks."},{"value":"high","name":"High","description":"The default balance for most tasks."},{"value":"max","name":"Max","description":"Reserve for the hardest quality-first tasks."}]}]}}
-> {"jsonrpc": "2.0", "id": 3, "method": "session/set_config_option", "params": {"sessionId": "<uuid-1>", "configId": "model", "value": "[\"deepseek-account\",\"deepseek-flash\"]"}}
<- {"jsonrpc":"2.0","id":3,"result":{"configOptions":[{"id":"model","name":"Model","category":"model","type":"select","currentValue":"[\"deepseek-account\",\"deepseek-flash\"]","options":[{"group":"deepseek-official","name":"DeepSeek","options":[{"value":"[\"deepseek-official\",\"deepseek-flash\"]","name":"DeepSeek-V41-Flash"},{"value":"[\"deepseek-official\",\"deepseek-v4-pro\"]","name":"DeepSeek-V4-Pro","description":"Stronger agentic coding, knowledge, and difficult reasoning; suited to complex or quality-critical tasks at higher cost."}]},{"group":"deepseek-account","name":"DeepSeek Account","options":[{"value":"[\"deepseek-account\",\"deepseek-flash\"]","name":"DeepSeek-V41-Flash"},{"value":"[\"deepseek-account\",\"deepseek-v4-pro\"]","name":"DeepSeek-V4-Pro","description":"Stronger agentic coding, knowledge, and difficult reasoning; suited to complex or quality-critical tasks at higher cost."}]}]},{"id":"reasoning_effort","name":"Reasoning effort","category":"thought_level","type":"select","currentValue":"high","options":[{"value":"off","name":"Off","description":"Use for simple tasks that do not need reasoning."},{"value":"low","name":"Low","description":"Prefer for routine or latency-sensitive tasks."},{"value":"high","name":"High","description":"The default balance for most tasks."},{"value":"max","name":"Max","description":"Reserve for the hardest quality-first tasks."}]}]}}
-> {"jsonrpc": "2.0", "id": 4, "method": "session/prompt", "params": {"sessionId": "<uuid-1>", "prompt": [{"type": "text", "text": "Use your shell tool to run exactly this command and nothing else: echo hi > probe.txt"}]}}
<- {"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"<uuid-1>","update":{"sessionUpdate":"agent_thought_chunk","messageId":"<uuid-2>","content":{"type":"text","text":"The user asks to run exactly `echo hi > probe.txt` and nothing else. This is a benign command. I should just run it."}}}}
<- {"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"<uuid-1>","update":{"sessionUpdate":"usage_update","used":5628,"size":1000000}}}
<- {"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"<uuid-1>","update":{"sessionUpdate":"tool_call","toolCallId":"call_00_pOUNADtYsek3N7hj9plk9553","title":"bash","kind":"other","status":"in_progress","rawInput":{"command":"echo hi > probe.txt","description":"Write hi to probe.txt"}}}}
<- {"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"<uuid-1>","update":{"sessionUpdate":"tool_call_update","toolCallId":"call_00_pOUNADtYsek3N7hj9plk9553","status":"completed","content":[{"type":"content","content":{"type":"text","text":"(no output)"}}]}}}
<- {"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"<uuid-1>","update":{"sessionUpdate":"agent_message_chunk","messageId":"<uuid-3>","content":{"type":"text","text":"Done. The command ran with no output, as expected — `probe.txt` now contains `hi`."}}}}
<- {"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"<uuid-1>","update":{"sessionUpdate":"usage_update","used":5665,"size":1000000}}}
<- {"jsonrpc":"2.0","id":4,"result":{"stopReason":"end_turn"}}
-> {"jsonrpc": "2.0", "id": 5, "method": "session/new", "params": {"cwd": "relative/path", "mcpServers": []}}
<- {"jsonrpc":"2.0","id":5,"error":{"code":-32602,"message":"Invalid params: cwd must be an absolute path: relative/path"}}
-> {"jsonrpc": "2.0", "id": 6, "method": "session/prompt", "params": {"sessionId": "no-such-session", "prompt": [{"type": "text", "text": "x"}]}}
<- {"jsonrpc":"2.0","id":6,"error":{"code":-32602,"message":"Invalid params: unknown session: no-such-session"}}
-> {"jsonrpc": "2.0", "id": 7, "method": "session/does-not-exist", "params": {}}
<- {"jsonrpc":"2.0","id":7,"error":{"code":-32601,"message":"\"Method not found\": session/does-not-exist","data":{"method":"session/does-not-exist"}}}
-> {"jsonrpc": "2.0", "id": 8, "method": "session/prompt", "params": {"sessionId": "<uuid-1>", "prompt": [{"type": "text", "text": "Count from 1 to 400, one number per line, and after each number write one sentence about it."}]}}
-> {"jsonrpc": "2.0", "method": "session/cancel", "params": {"sessionId": "<uuid-1>"}}
<- {"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"<uuid-1>","update":{"sessionUpdate":"agent_thought_chunk","messageId":"<uuid-4>","content":{"type":"text","text":"The user wants a list from 1 to 400, one number per line, and after each number one sentence about it. That's 800 lines. That's a lot of output. I should just write it. But token budget... 400 numbers × (number + sentence) is maybe 8000-12000 tokens. That's feasible but long. Let me do it directly in the response.\n\nHmm, but should I use the shell to generate? No — the content needs to be meaningful sentences about each number. I can generate them. Let me just produce the list carefully.\n\nActually, generating 400 unique sentences is a big task but doable. Let me aim for concise factual sentences about each number. To keep it manageable, I'll vary content: number properties (prime, even/odd, square, factors), historical/cultural references, etc. Each sentence should be true-ish.\n\nLet me be careful about accuracy. I'll write factual statements I'm confident about.\n\nI'll write it out. Let me go.\n\nNumbers 1-400. I'll keep sentences short.\n\nLet me draft systematically:\n1: 1 is the multiplicative identity, since any number multiplied by it stays unchanged.\n2: 2 is the only even prime number.\n3: 3 is the smallest odd prime.\n4: 4 is the first perfect square greater than 1.\n5: 5 is the number of fingers on a typical human hand.\n6: 6 is the first perfect number, equal to the sum of its divisors 1, 2, and 3.\n7: 7 is often considered lucky and is the number of days in a week.\n8: 8 is the second cube, 2³.\n9: 9 is"}}}}
<- {"jsonrpc":"2.0","id":8,"result":{"stopReason":"cancelled"}}
```

## Agent stderr

```text
(empty)
```
