# OMP² Native CLI Feasibility: Codex, Claude Code, Gemini, Antigravity, Grok, Devin

Status: feasibility research; no OMP implementation.
Date: 2026-10-13
Closes: CLI-4, CLI-5, CLI-6 (`docs/parked/2026-10-10-native-cli-integration-next-steps.md`); refines CLI-2.
Context: `docs/adr/0043-native-cli-integration.md` (proposed, not edited by this note),
`docs/research/native-cli-integration.md`.

> **Superseded in part, 2026-10-20 (owner decision, see ADR 0043 "Amendment 2026-10-20").** The verdict
> tables and "remain disabled" statements below are the 2026-10-13/14 evidence and are left as written. What
> changed: (1) section 6.1 became the plan, with one ACP client and per-CLI launch profiles; (2) the gate is now a
> recorded handshake transcript per binary, not the absence of implementation; (3) **Gemini CLI is dropped**,
> because Google stopped serving it for individual, AI Pro and AI Ultra accounts on 2026-06-18; (4) the "Codex,
> Claude and Antigravity do not expose ACP" line is true of the CLIs but not of their bridges: Claude has
> `@agentclientprotocol/claude-agent-acp`, Antigravity has Google's `agy_acp_server.par`, and DeepSeek Harness
> (`dsh --profile acp`, not covered below) exposes ACP itself. Codex keeps its native app-server adapter.

# Update after official-source review (read 2026-10-14)

The U9-U27 register was rechecked against vendor documentation, vendor repositories, and tagged releases only. **14 of 19 items moved from UNVERIFIED:** 9 are now `confirmed`, 5 are `refuted`, and 5 remain `UNVERIFIED`. The capability gate does not change for any vendor: no installed-binary transcript or equivalent end-to-end proof was added.

| Result for U9-U27 | Count | Items |
|---|---:|---|
| `confirmed` | 9 | U9, U10, U12, U13, U14, U18, U23, U24, U25 |
| `refuted` | 5 | U15, U16, U20, U21, U27 |
| still `UNVERIFIED` | 5 | U11, U17, U19, U22, U26 |

| CLI | Version | Verdict | Reason (one sentence) | ADR 0043 non-goal |
|---|---|---|---|---|
| Codex | 0.160.0 | `adapter-ready` | `codex app-server` speaks documented JSON-RPC over stdio JSONL, and its whole request/notification/approval/interrupt surface was emitted by the installed binary (`generate-json-schema`: 104 client requests, 10 server requests, 83 notifications). | Satisfies "No credential migration or extraction." when OMP spawns `codex app-server --listen stdio://` itself and never reads `auth.json` or uses `chatgptAuthTokens`. |
| Claude Code | 2.1.296 | `capability-gated` | The vendor-published Agent SDK package now specifies the `control_request`/`control_response` stream and interrupt/permission messages, and Anthropic documents third-party app use only for the user's own unmodified binary; the gate remains because no installed-binary transcript or mock transcript was recorded (U11 and the binary-conformance bar). | Implementing now would contradict "No Claude/Gemini/Antigravity/Grok/Devin implementation without verified official contracts." |
| Gemini CLI | 0.59.0 | `capability-gated` | The tagged vendor source confirms ACP initialization, load, cancel and permission handling and its headless signal/exit paths, while the official FAQ prohibits wrapping Gemini CLI OAuth in third-party software; the gate remains because no installed-binary handshake was recorded (U13-U16 are now settled). | Implementing now would contradict "No Claude/Gemini/Antigravity/Grok/Devin implementation without verified official contracts." |
| Antigravity | `agy` 1.1.10 | `capability-gated` | Official sources confirm stderr-only soft-denials, absence of an auth-status command, and the consumer-plan prohibition on wrapping `agy`, but do not settle whether 1.1.10 accepts the later `--input-format` flag or its SIGINT exit code (U17, U19). | Implementing now would contradict "No Claude/Gemini/Antigravity/Grok/Devin implementation without verified official contracts."; the Boundary "No generic human-output parser when a structured interface is unavailable." holds only if `--output-format` is never `text`. |
| Grok | 1.0.46 | `capability-gated` | Official sources settle ACP cancel, permission-mode precedence, the `grok models` auth banner and the `-s` semantics, but the numeric exit-code contract and third-party OAuth delegation remain unresolved (U22, U26). | Implementing now would contradict "No Claude/Gemini/Antigravity/Grok/Devin implementation without verified official contracts." |
| Devin | 3000.11.3 | `capability-gated` | `devin acp` ("Run as an ACP (Agent Client Protocol) server over stdio") speaks a public JSON-RPC spec with MUST-level `session/cancel`, `session/request_permission` and absolute `cwd`, so it is the strongest non-Codex candidate; but nothing beyond `--help` ran (no handshake), exit codes, auth behaviour under ACP and permission-mode handling are undocumented or unverified (U28, U30-U39). | Implementing now would contradict "No Claude/Gemini/Antigravity/Grok/Devin implementation without verified official contracts."; the ACP spec is an official contract, but its use by the installed binary is verified from `--help` only. |

Adapter-ready: **Codex only.** The other five installed CLIs have real headless/structured modes (three of them speak
ACP, see section 6.1), but none passed the bar below, so they stay disabled by the gate, not by absence of an interface.

Verdict bar used in this note (mine, derived from the brief's definitions): `adapter-ready` needs
(a) a vendor-documented structured protocol that the installed binary's own help confirms,
(b) a contract checkable against the installed binary (a binary-emitted schema or equivalent), not prose only,
(c) an in-protocol interrupt and permission-response channel, and
(d) a permission mapping that can be made fail-closed.
Only Codex meets (b): its binary emits the schema. Missing auth-status does not block `adapter-ready`
because `docs/research/native-cli-integration.md` already requires adapters to report `unknown` then.

Refinements to earlier notes (the earlier text is left as written):

- Codex "Unix socket" is not raw JSONL. Docs: "Unix socket (`--listen unix://` or `--listen unix://PATH`):
  WebSocket connections over Codex's default app-server control socket". Only `stdio://` is plain
  newline-delimited JSON. Docs also say the app-server command and WebSocket transport "are experimental
  and aren't supported for production workloads".
- Antigravity has a documented headless/JSON surface (the earlier note found none); Grok has a documented
  ACP-over-stdio mode. Both are documented, not locally exercised, so ADR 0043's "remain disabled" still holds.
- Three CLIs expose an ACP server mode in `--help` or documentation (Gemini `--acp`, Devin `devin acp`, Grok `agent stdio`);
  one ACP client adapter could serve all three, which changes the plan from one adapter per CLI (section 6.1). Codex,
  Claude and Antigravity do not expose ACP (confirmed with `--help` and `<cli> acp --help`).

## Method and evidence rules

- OBS: observed in this session from a local command (command shown, output quoted; wrapped lines joined).
- SCHEMA: observed in files generated by the installed binary into `/tmp` (Codex only).
- DOC: vendor page text read on 2026-10-13; not exercised.
- UNVERIFIED (Un): neither observed nor documented in the text read, or doc and binary disagree.
- INFERENCE: my reading, not observed.

Commands run: `--help`, `--version` and subcommand `--help` for all six CLIs, plus
`codex app-server generate-json-schema` / `generate-ts` into `/tmp` with `CODEX_HOME=/tmp/cliFeas-codex-home`
(the directory was empty afterwards, `find` printed nothing). Nothing logged in, refreshed a token, started an
agent, or made a model request. No credential, config, token or Keychain file was read. `codex login status`,
`claude auth status`, `codex doctor`, `account/read` were named but deliberately not executed (U8, U11).
The `/tmp` outputs were removed after this note was written; reproduce with
`CODEX_HOME=$(mktemp -d) codex app-server generate-json-schema --out "$(mktemp -d)"`.

## 1. Codex CLI 0.160.0

### Observed

```text
$ codex --version
codex-cli 0.160.0
$ codex exec --help
Run Codex non-interactively
  [PROMPT] Initial instructions for the agent. If not provided as an argument (or if `-` is used), instructions are read from stdin.
  -s, --sandbox <SANDBOX_MODE>   [possible values: read-only, workspace-write, danger-full-access]
      --approve-for-me           Route approval requests through automatic review using the workspace-write sandbox
      --dangerously-bypass-approvals-and-sandbox
  -C, --cd <DIR>                 Tell the agent to use the specified directory as its working root
      --add-dir <DIR>           Additional directories that should be writable alongside the primary workspace
      --ephemeral                Run without persisting session files to disk
      --output-schema <FILE>     Path to a JSON Schema file describing the model's final response shape
      --json                     Print events to stdout as JSONL
  resume  Resume a previous session by id or pick the most recent with --last
  fork    Fork a previous session by id into a new session
$ codex exec resume --help        (options list has no --cd and no --sandbox; has --last, --all, --json)
$ codex --help                    (top level only)
  -a, --ask-for-approval <APPROVAL_POLICY>   Possible values: on-request, never
  agents  Browse all agent sessions on the shared local app-server daemon
  app-server  [experimental] Run the app server or related tooling
      --no-daemon  Run without the shared background server, even if it is already running
$ codex exec --help | grep -c -- '--ask-for-approval'
0
$ codex app-server --help
  daemon                Manage the local app-server daemon
  proxy                 Proxy stdio bytes to the running app-server control socket
  generate-ts           [experimental] Generate TypeScript bindings for the app server protocol
  generate-json-schema  [experimental] Generate JSON Schema for the app server protocol
      --listen <URL>  Transport endpoint URL. Supported values: `stdio://` (default), `unix://`, `unix://PATH`, `ws://IP:PORT`, `off`
      --stdio         Use stdio as the transport (equivalent to `--listen stdio://`)
      --ws-auth <MODE>  [possible values: capability-token, signed-bearer-token]
      --ws-token-file <PATH> | --ws-token-sha256 <HEX> | --ws-shared-secret-file <PATH>
      --ws-issuer | --ws-audience | --ws-max-clock-skew-seconds
      --code-mode-host <URL>   Connect to a remote code-mode host instead of starting a local host
$ codex app-server generate-json-schema --help
Usage: codex app-server generate-json-schema [OPTIONS] --out <DIR>
  -o, --out <DIR>   Output directory where the schema bundle will be written
      --experimental   Include experimental methods and fields in the generated output
$ codex app-server generate-ts --help
Usage: codex app-server generate-ts [OPTIONS] --out <DIR>
  -o, --out <DIR>   Output directory where .ts files will be written
  -p, --prettier <PRETTIER_BIN>   Optional path to the Prettier executable to format generated files
      --experimental
$ codex login status --help       "Show login status"
$ codex doctor --help             "Diagnose local Codex installation, config, auth, and runtime health"; "--json  Emit a redacted machine-readable report"
```

`codex app-server daemon` subcommands (OBS): `bootstrap, start, restart, update, enable-remote-control,
disable-remote-control, stop, version`. `codex app-server proxy` takes `--sock <SOCKET_PATH>`.
`codex mcp-server --help` printed the top-level help (not a subcommand in 0.160.0; DOC agrees it was removed).

### The 8 questions

| # | Question | Finding | Status |
|---|---|---|---|
| 1 | Headless / non-TTY | `codex exec` ("Run Codex non-interactively"); prompt from arg or stdin. | OBS |
| 2 | Structured stream | `exec --json` JSONL; `app-server` bidirectional JSON-RPC 2.0 (see transport table). `exec` event names (`thread.started`, `turn.started`, `turn.completed`, `turn.failed`, `item.*`, `error`) are DOC only. | OBS + DOC |
| 3 | Session create/resume | CLI: `exec resume [SESSION_ID] [PROMPT]`, `--last`, `exec fork`, `--ephemeral`. Protocol: client methods `thread/start`, `thread/resume`, `thread/fork`, `thread/list`, `thread/read`. | OBS + SCHEMA |
| 4 | Interrupt / cancel | Protocol: `turn/interrupt` with `{threadId, turnId}`; `TurnStatus` enum is `completed, interrupted, failed, inProgress`. DOC: "success is `{}` and the turn ends with `status: "interrupted"`". An approval answered `cancel` also interrupts the turn (SCHEMA description). `exec`: no signal behaviour documented. | SCHEMA + DOC; exec = U1 |
| 5 | Permissions | See "Fail-closed mapping" below. | OBS + SCHEMA |
| 6 | Working directory | `exec -C/--cd <DIR>`, `--add-dir`; protocol: `ThreadStartParams.cwd`, `ThreadResumeParams.cwd`, `TurnStartParams.cwd`. `exec resume`/`exec fork` expose neither `--cd` nor `--sandbox`, so resume with explicit policy needs `app-server`. | OBS + SCHEMA |
| 7 | Auth status | `codex login status` (DOC: "exits with `0` when credentials are present"); `codex doctor --json` (redacted report); protocol `account/read` with `refreshToken: false`. None executed. | OBS (help) + DOC; U8 |
| 8 | Exit codes | No table in any help. DOC only for `login status`, `apply`, `cloud`. `exec` and `app-server` process exit codes undocumented. | U1, U2 |

### app-server transports (OBS from help, semantics DOC)

| `--listen` value | Wire (DOC) | Use for OMP |
|---|---|---|
| `stdio://` (default; also `--stdio`) | "newline-delimited JSON (JSONL)" | First choice: OMP owns the child process, so lifecycle, cancellation and cleanup stay with OMP. |
| `unix://` / `unix://PATH` | "WebSocket connections over Codex's default app-server control socket or a custom Unix socket path, using the standard HTTP Upgrade handshake" | Needs a WebSocket client; the default socket is the user's shared daemon, so avoid it (INFERENCE). |
| `ws://IP:PORT` | one JSON-RPC message per WebSocket text frame; "experimental and unsupported"; non-loopback "currently allow unauthenticated connections by default" | Do not use. If ever needed: loopback plus `--ws-auth` with a token file. |
| `off` | no local transport | n/a |

Also: `codex app-server proxy --sock` bridges stdio bytes to the running control socket; `codex app-server
daemon …` manages a shared local daemon; top-level `codex --remote ws://…|wss://…|unix://…` is the TUI client.
Because the 0.160 TUI defaults to a shared daemon (`--no-daemon` opts out), OMP should spawn its own private
`codex app-server --listen stdio://` and never attach to the user's daemon (INFERENCE).

### Typed bindings route (SCHEMA)

```text
$ CODEX_HOME=/tmp/cliFeas-codex-home codex app-server generate-json-schema --out /tmp/cliFeas-schema            exit 0, 314 files
$ ... generate-json-schema --experimental --out /tmp/cliFeas-schema-exp                                         440 files
$ ... generate-ts --out /tmp/cliFeas-ts                                                                         734 files
```

Shape of the stable bundle: `ClientRequest.json`, `ServerRequest.json`, `ServerNotification.json`,
`JSONRPC*.json`, per-approval `*Params/*Response.json`, `codex_app_server_protocol.schemas.json`,
`codex_app_server_protocol.v2.schemas.json` (611,560 bytes, 651 definitions), and `v1/`, `v2/` directories.
Counts by `method` const: stable 104 client requests / 10 server requests / 83 notifications; with
`--experimental` 167 / 11 / 83. The docs say each output "matches that version exactly", so generate at build
time against the pinned `codex --version` and re-check on upgrade. Whether a Rust-consumable crate is published
was not checked (U7); the observed route to typed bindings is JSON Schema or TypeScript.

Methods the first slice needs (names from `ClientRequest.json`, `ServerRequest.json`, `ServerNotification.json`):

- client: `initialize` (params `clientInfo`, `capabilities`; response `codexHome, platformFamily, platformOs, userAgent`), `thread/start`, `thread/resume`, `thread/fork`, `turn/start`, `turn/steer`, `turn/interrupt`, `account/read`.
- server requests (blocking, need an answer): `item/commandExecution/requestApproval`, `item/fileChange/requestApproval`, `item/permissions/requestApproval`, `item/tool/requestUserInput`, `item/tool/call`, `mcpServer/elicitation/request`, `account/chatgptAuthTokens/refresh`, `attestation/generate`, legacy `applyPatchApproval`, `execCommandApproval`.
- notifications: `thread/started`, `turn/started`, `turn/completed`, `item/started`, `item/completed`, `item/agentMessage/delta`, `item/commandExecution/outputDelta`, `thread/tokenUsage/updated`, `serverRequest/resolved`, `error`, `warning`.
- `ThreadItem` types: `userMessage, hookPrompt, agentMessage, functionCallOutput, plan, reasoning, commandExecution, fileChange, mcpToolCall, dynamicToolCall, collabAgentToolCall, subAgentActivity, webSearch, imageView, sleep, imageGeneration, enteredReviewMode, exitedReviewMode, contextCompaction`.

### Fail-closed permission mapping (SCHEMA, with INFERENCE marked)

Start-of-thread fields (`ThreadStartParams`: `approvalPolicy, approvalsReviewer, cwd, sandbox, config, ephemeral, …`;
`TurnStartParams`: `approvalPolicy, approvalsReviewer, cwd, sandboxPolicy, …`):

| Concept | Schema values | OMP rule |
|---|---|---|
| `approvalPolicy` | `untrusted`, `on-request`, `never`, or `{granular:{mcp_elicitations, rules, sandbox_approval, request_permissions, skill_approval}}` | Send an explicit value on every `thread/start`/`thread/resume`/`turn/start`; never rely on `config.toml` defaults. |
| `sandbox` (`SandboxMode`) | `read-only`, `workspace-write`, `danger-full-access` | Start from `read-only`; never send `danger-full-access`. `sandboxPolicy.type` may also be `externalSandbox`: never. |
| `approvalsReviewer` | schema text: "Defaults to `user`. `auto_review` uses a carefully prompted subagent…" | Pin `user`; `auto_review` (CLI `--approve-for-me`) hands decisions to a Codex subagent outside OMP policy. |
| Approval answer (command) | `accept, acceptForSession, acceptWithExecpolicyAmendment, applyNetworkPolicyAmendment, decline, cancel` | Map OMP allow-once to `accept`, deny to `decline`, abort to `cancel`. Never send the three that persist or widen future behaviour. |
| Approval answer (file change) | `accept, acceptForSession, decline, cancel` ("cancel: … The turn will also be immediately interrupted") | Same. |
| Unmapped server requests | `item/tool/requestUserInput`, `mcpServer/elicitation/request`, `item/tool/call`, `item/permissions/requestApproval` | No OMP counterpart yet: answer with the most restrictive response (empty grant, `decline`/`cancel`, or JSON-RPC error). How the server treats an error reply is undocumented (U3). |
| Effective policy check | `ThreadStartResponse` carries `approvalPolicy`, `approvalsReviewer`, `cwd`, `sandbox` | INFERENCE: compare to what was requested and abort the thread if broader; whether these are post-merge effective values is unverified (U4). |

The CLI exposes fewer values than the protocol: top-level `-a` lists only `on-request` and `never`
(OBS), `exec` has no `-a` at all (OBS), while the schema also has `untrusted`. Doc drift: the app-server
doc examples show `"approvalPolicy": "unlessTrusted"` and `"sandbox": "workspaceWrite"`, but the generated schema
enumerates `untrusted` and `workspace-write` (SCHEMA wins; not sent to a live server, U5).

### Official pages (read 2026-10-13)

See Sources S1-S7. They add: the app-server lifecycle (`initialize` once per connection, then `initialized`,
`thread/start`, `turn/start`, stream, `turn/completed`), WebSocket auth flags, JSON-RPC `-32001` overload,
`optOutNotificationMethods`, `experimentalApi` opt-in, and that "If you are automating jobs or running Codex in CI,
use the Codex SDK instead."

### Verdict

`adapter-ready`. Reason: the documented JSON-RPC surface was emitted by the installed binary, including
interrupt, thread resume and every approval request/decision enum, and a fail-closed mapping exists.
ADR 0043 non-goal: satisfies "No credential migration or extraction." provided OMP never reads Codex
credentials and never uses `account/login/start` with `chatgptAuthTokens`.
Gaps to close in CLI-2 tests with a mock server, not by this note: U2-U5.

## 2. Claude Code 2.1.296

### Observed

```text
$ claude --version
2.1.296 (Claude Code)
$ claude --help
  -p, --print   Print response and exit (useful for pipes). Note: The workspace trust dialog is skipped when Claude is run in non-interactive mode (via -p, or when stdout is not a TTY...
  --output-format <format>   Output format (only works with --print): "text" (default), "json" (single result), or "stream-json" (realtime streaming)
  --input-format <format>    Input format (only works with --print): "text" (default), or "stream-json" (realtime streaming input)
  --include-partial-messages | --replay-user-messages | --json-schema <schema> | --max-budget-usd <amount>
  -c, --continue   Continue the most recent conversation in the current directory
  -r, --resume [value]   Resume a conversation by session ID, or open interactive picker with optional search term
  --session-id <uuid>    Use a specific session ID for the conversation (must be a valid UUID)
  --fork-session | --no-session-persistence (only works with --print) | -n, --name <name>
  --permission-mode <mode>   (choices: "acceptEdits", "auto", "bypassPermissions", "manual", "dontAsk", "plan")
  --permission-prompts <target>   Who answers permission prompts with --print: "host" (the SDK host or --permission-prompt-tool) or "none" (nobody: anything that would prompt is denied automatically; the permission mode still decides everything else) (choices: "host", "none", default: "host")
  --allowedTools | --disallowedTools | --tools | --dangerously-skip-permissions | --restricted | --bare | --safe-mode
  --add-dir <directories...>   Additional directories to allow tool access to
  -w, --worktree [name]
  --bare   ...Anthropic auth is strictly ANTHROPIC_API_KEY or apiKeyHelper via --settings (OAuth and keychain are never read).
$ claude mcp serve --help      "Start the Claude Code MCP server"
$ claude auth status --help    "Show authentication status"; "--json  Output as JSON (default)"; "--text  Output as human-readable text"
$ claude agents --help         "--json  Print active sessions (interactive and background) as a JSON array and exit (for scripting; does not require a TTY)"
$ claude stop --help           "Stop a background session. Its conversation is kept; resume it later with `claude attach <id>`."
$ claude --help | grep -c -- '--max-turns'
0
```

`--max-turns` and a standalone `--permission-prompt-tool` flag are documented (S9) but not listed as options in
`--help`; the CLI reference itself says "`claude --help` does not list every flag". There is no `--cwd` flag (U12).

### The 8 questions

| # | Question | Finding | Status |
|---|---|---|---|
| 1 | Headless / non-TTY | `-p/--print`; trust dialog skipped (quoted above). | OBS |
| 2 | Structured stream | `--output-format json` or `stream-json`, `--input-format stream-json`, `--include-partial-messages`; MCP-server mode `claude mcp serve`. Event details (`system/init` with `capabilities`, `result`, `permission_denials`) are DOC. | OBS + DOC |
| 3 | Sessions | `-c`, `-r <id\|name\|path.jsonl>`, `--session-id <uuid>`, `--fork-session`, `--no-session-persistence`; `session_id` is returned in `json` output (DOC). | OBS + DOC |
| 4 | Interrupt / cancel | Not in `-p` help. DOC: "To end the turn instead, send SIGINT, or call the Agent SDK's `interrupt()`"; `interrupt()` is "Only available in streaming input mode" and returns a receipt when the CLI advertises `interrupt_receipt_v1`; SIGTERM exits 143 and "records no result". Background sessions: `claude stop <id>` (OBS). | OBS + DOC; wire spec U9 |
| 5 | Permissions | `--permission-mode`, `--permission-prompts none`, tool allow/deny lists (OBS). Fail-closed needs explicit flags (below). | OBS + DOC |
| 6 | Working directory | No `--cwd`; process cwd plus `--add-dir`; `--worktree`. DOC: SDK option `cwd` (default `process.cwd()`); a session ID resolves "in any project on this machine". | OBS + DOC |
| 7 | Auth status | `claude auth status` (JSON default, `--text`); DOC: "Exits with code 0 if logged in, 1 if not", `authMethod` is one of `none, claude.ai, oauth_token, api_key, api_key_helper, third_party`. Not executed. | OBS (help) + DOC; U11 |
| 8 | Exit codes | DOC: "exits with code 0 on success and a non-zero code when the run fails"; 143 on SIGTERM; `--max-turns` "Exits with an error when the limit is reached"; in-run failures such as missing authentication are "printed as the result on stdout". No per-failure code table. | DOC; partial |

Fail-closed mapping is possible but not by default. DOC: "A run where nothing sets a permission mode takes the
built-in starting permission mode, which can be `auto`, so pass the one you want." OMP would have to pass
`--permission-mode dontAsk` (or `plan`) and `--permission-prompts none` on every run, never `--dangerously-skip-permissions`
or `bypassPermissions`. Hermeticity is a second problem: DOC "Without `--bare`, a `-p` session runs the hooks in a
project's `.claude/settings.json` and connects the servers in its `.mcp.json`, even in a folder you've never trusted",
and user/project `permissions.allow` rules also apply. `--bare`, `--setting-sources`, `--safe-mode`, `--restricted` narrow
that, but `--bare` never reads OAuth or keychain, so it only works with an API key or `apiKeyHelper`.

### Verdict

`capability-gated`. Reason: see Summary (U9, U10). ADR 0043 non-goal: implementing now would contradict
"No Claude/Gemini/Antigravity/Grok/Devin implementation without verified official contracts."; using
`claude setup-token` / `CLAUDE_CODE_OAUTH_TOKEN` from OMP would additionally contradict "No credential migration or extraction."
To reach `adapter-ready`: resolve U10 with the vendor, locate or obtain a CLI-level wire spec for `control_request`/`control_response`
(U9), and record a mock transcript for CLI-7.

## 3. Gemini CLI 0.59.0

### Observed

```text
$ gemini --version
0.59.0
$ gemini --help
Gemini CLI - Defaults to interactive mode. Use -p/--prompt for non-interactive (headless) mode.
  -p, --prompt   Run in non-interactive (headless) mode with the given prompt. Appended to input on stdin (if any).
  -o, --output-format   The format of the CLI output.  [choices: "text", "json", "stream-json"]
      --acp   Starts the agent in ACP mode
      --experimental-acp   Starts the agent in ACP mode (deprecated, use --acp instead)
  -r, --resume   Resume a previous session. Use "latest" for most recent or index number (e.g. --resume 5).
      --session-file   Load a session from a JSON file
      --session-id   Start a new session with a manually provided UUID.
      --list-sessions   List available sessions for the current project and exit.
      --approval-mode   Set the approval mode: default (prompt for approval), auto_edit (auto-approve edit tools), yolo (auto-approve all tools), plan (read-only mode)
  -y, --yolo   Automatically accept all actions (aka YOLO mode...)
      --policy / --admin-policy   Additional policy files or directories to load
      --allowed-tools   [DEPRECATED: Use Policy Engine instead ...]
  -s, --sandbox   Run in sandbox?
      --include-directories   Additional directories to include in the workspace (comma-separated or multiple --include-directories)
      --skip-trust   Trust the current workspace for this session.
```

There is no cwd flag and no auth subcommand (`mcp, extensions, skills, hooks, gemma` only).

### The 8 questions

| # | Question | Finding | Status |
|---|---|---|---|
| 1 | Headless | `-p`; DOC: "triggered when the CLI is run in a non-TTY environment or when providing a query with the `-p`". | OBS + DOC |
| 2 | Structured stream | `-o json`, `-o stream-json`, `--acp`. DOC: `json` has `response`, `stats`, optional `error`; `stream-json` events `init, message, tool_use, tool_result, error, result`; ACP is "JSON-RPC 2.0 over stdio". | OBS + DOC |
| 3 | Sessions | `-r latest\|N`, `--session-id`, `--session-file`, `--list-sessions`; ACP `newSession`, `loadSession`. | OBS + DOC |
| 4 | Interrupt | ACP `cancel` ("Cancels an ongoing prompt") DOC. Headless SIGINT behaviour undocumented. | DOC; U14 |
| 5 | Permissions | `--approval-mode`, `--policy`; DOC policy engine: "`ask_user` … (In non-interactive mode, this is treated as `deny`.)". So default headless is fail-closed; OMP must never pass `-y/--yolo` or `--approval-mode yolo`. ACP `setSessionMode` can raise approval level. User or admin policy files can widen. | OBS + DOC |
| 6 | Working directory | No cwd flag; process cwd, `--include-directories`, `-w`. ACP `newSession` carries `cwd` per the ACP spec (S-acp). | OBS + DOC |
| 7 | Auth status | None found in help or docs read. Headless: "will use your existing authentication method, if an existing authentication credential is cached"; else env vars (`GEMINI_API_KEY`, Vertex). | OBS (absence); U15 |
| 8 | Exit codes | DOC: `0` Success, `1` General error or API failure, `42` Input error (invalid prompt or arguments), `53` Turn limit exceeded. | DOC |

Product note (DOC, blog dated May 19, 2026): Gemini CLI "will stop serving requests for Google AI Pro and Ultra, as
well as those using it free of charge" on June 18, 2026; paid Gemini API keys, Vertex and Code Assist Standard/Enterprise
remain. Doc drift: the CLI cheatsheet (updated May 1, 2026) still labels `--experimental-acp` "Experimental feature"
with the expansion "Agent Code Pilot"; 0.59.0 help deprecates it in favour of `--acp`.

### Verdict

`capability-gated`. Reason: see Summary. ADR 0043 non-goal: "No Claude/Gemini/Antigravity/Grok/Devin implementation without
verified official contracts." To reach `adapter-ready`: a handshake probe of `gemini --acp` in an isolated `HOME`
(U13), a decision on interrupt for the non-ACP path (U14), and the terms answer (U16).

## 4. Antigravity CLI (`agy`) 1.1.10

### Observed

```text
$ agy --version
1.1.10
$ agy --help
Usage of agy:
  --print   Run a single prompt non-interactively and print the response       (-p, --prompt are aliases)
  --print-timeout   Timeout for print mode wait (default 5m0s)
  --output-format   Output format for print mode (text, json, stream-json) (default text)
  --json-schema   Optional JSON schema string or path to a schema file to enforce structured output (for stream-json, only applicable to the final result)
  --continue (-c)   Continue the most recent conversation
  --conversation   Resume a previous conversation by ID
  --project / --new-project
  --add-dir   Add a directory to the workspace (repeatable)
  --dangerously-skip-permissions   Auto-approve all tool permission requests without prompting
  --mode   Set the agent execution mode for this session (accept-edits, plan)
  --sandbox   Run in a sandbox with terminal restrictions enabled
Available subcommands: agent, agents, changelog, help, install, models, plugin, plugins, update
$ agy --help | grep -ci 'input-format'
0
$ agy --totally-bogus-flag --version      →  1.1.10   (exit 0; --version short-circuits, so this cannot confirm or refute flag support)
```

### The 8 questions

| # | Question | Finding | Status |
|---|---|---|---|
| 1 | Headless | `-p/--print/--prompt`; DOC: "Headless mode uses your cached credentials"; unauthenticated CI run "exits with an `authentication required` error instead of hanging". | OBS + DOC |
| 2 | Structured stream | `--output-format json` (single envelope with `conversation_id`, `status`, `response`, `usage`) and `stream-json` (NDJSON `init`, `step_update`, `result`). DOC also: `--input-format stream-json` for multi-turn on stdin, which `agy --help` does not list. No ACP/JSON-RPC/MCP-server flag in help. | OBS + DOC; U17 |
| 3 | Sessions | `-c`, `--conversation <id>`; DOC: `conversation_id` in output. | OBS + DOC |
| 4 | Interrupt | No cancel flag. DOC: result `status` can be `CANCELED` or `INTERRUPTED` ("for example, `SIGINT`"); input event `control_request`/`control_response` → "`ERROR` result, session ends", exit `2`; closing stdin ends a stream session. | DOC; U19 |
| 5 | Permissions | DOC: no prompt in headless; "A tool that requires approval it can't obtain is soft-denied: the run continues, exits `0`, and prints a notice to `stderr`". Allow rules live in `~/.gemini/antigravity-cli/settings.json` (`permissions.allow`), not in flags; flags are `--mode`, `--sandbox`, `--dangerously-skip-permissions`. Fail-closed by default, but OMP cannot answer an approval or pass per-run allow rules, and a denial exits 0. | OBS + DOC; U18 |
| 6 | Working directory | No cwd flag (`--add-dir`, `--project`); DOC: the `init` event reports `cwd` and `permission_mode`. | OBS + DOC |
| 7 | Auth status | None. Interactive `/usage`, `/credits`, `/logout` only (DOC, reference page). | OBS (absence); U20 |
| 8 | Exit codes | DOC: `0` success, "non-zero" on failure; `1` for an unknown `--model` and for malformed or non-`text` stream input; `2` for `control_request`/`control_response` or CLI-handled slash commands; `status` table `SUCCESS, ERROR, CANCELED, INTERRUPTED, INVALID, WAITING, RUNNING`. | DOC |

### Verdict

`capability-gated`. Reason: see Summary. ADR 0043 non-goal: "No Claude/Gemini/Antigravity/Grok/Devin implementation
without verified official contracts."; the Boundary "No generic human-output parser…" is respected only if
`--output-format` is never `text`. To reach `adapter-ready`: confirm `--input-format` on 1.1.10 (U17), find a way to
learn about soft-denials in-stream (U18), and settle the terms (U21).

## 5. Grok CLI 1.0.46

### Observed

```text
$ grok --version
grok 1.0.46 (2765805b9442) [stable]
$ grok --help
  -p, --single <PROMPT>   Single-turn prompt. Prints the response to stdout and exits
      --output-format <OUTPUT_FORMAT>   Output format for headless mode
          - plain / - json
          - streaming-json:          NDJSON: one ACP session update per line, the agent's native format
          - streaming-messages-json: NDJSON in the Anthropic Messages API wire format
      --json-schema <SCHEMA>   ... Implies --output-format json.
  -c, --continue | -r, --resume [<SESSION_ID_OR_TITLE>] | --fork-session
  -s, --session-id <SESSION_ID>   Use a specific session UUID for a **new** conversation ... Does not resume existing sessions
      --cwd <CWD>   Working directory
      --max-turns <N>   Maximum number of agent turns
      --permission-mode <MODE>   [possible values: default, acceptEdits, auto, dontAsk, bypassPermissions, plan]
      --allow <RULE> / --deny <RULE> / --always-approve / --sandbox <PROFILE> / --tools / --disallowed-tools
  agent  Run Grok without the interactive UI
$ grok agent --help
  stdio     Run the agent over stdio
  headless  Run the agent headlessly over the Grok WebSocket relay
  serve     Run the agent as a WebSocket server
  leader    Run as the shared leader process for other clients
$ grok agent serve --help
  --bind <BIND>   Address for the server to listen on [default: 127.0.0.1:2419]
  --secret <SECRET>   Secret token for client authentication (auto-generated if not provided) [env: GROK_AGENT_SECRET=]
$ grok doctor --help    "Check terminal, clipboard, color, and input support without starting Grok"; "--json"
$ grok inspect --help   "Show the configuration Grok discovers for this directory"; "--json"
$ grok login --help     "--oauth", "--device-auth"        (login/logout are the only auth subcommands)
```

### The 8 questions

| # | Question | Finding | Status |
|---|---|---|---|
| 1 | Headless | `-p/--single`, `--prompt-file`, `--prompt-json`; `grok agent …`. | OBS |
| 2 | Structured stream | `--output-format streaming-json` (ACP updates, NDJSON), `streaming-messages-json`, `json`; `grok agent stdio` (DOC: "Run Grok as an ACP agent over JSON-RPC on stdin/stdout"); `grok agent serve` (WebSocket, default `127.0.0.1:2419`, bearer-style `--secret`). | OBS + DOC |
| 3 | Sessions | `-c`, `-r <id\|title>`, `-s <uuid>` (new only), `--fork-session`, `grok sessions list/search/delete`. Doc drift: the headless page calls `-s` "Create or resume a named headless session"; help and the sessions page say it never resumes (U27). | OBS + DOC |
| 4 | Interrupt | No flag or documented mechanism in the pages read; ACP spec defines `session/cancel`, but whether Grok implements it is unstated. | U23 |
| 5 | Permissions | `--permission-mode`, `--allow/--deny`, `--sandbox`, `--tools`. DOC (enterprise page): `dontAsk` "Silently deny anything without an explicit allow rule", "Headless, CI". OMP would pass `--permission-mode dontAsk` plus a narrow `--allow` set and never `--always-approve`/`bypassPermissions`. User config can set `[ui] permission_mode = "always-approve"` or legacy `yolo = true`; precedence against the flag is not stated (U24). | OBS + DOC |
| 6 | Working directory | `--cwd <CWD>` (OBS); ACP `session/new` has `cwd` (DOC example). | OBS + DOC |
| 7 | Auth status | None observed or documented. `doctor --json` is terminal diagnostics; `inspect --json` is config discovery; neither is documented as auth status. | OBS (absence); U25 |
| 8 | Exit codes | None in help or in the headless, reference, permissions, sessions or enterprise pages. | U22 |

### Verdict

`capability-gated`. Reason: see Summary. ADR 0043 non-goal: "No Claude/Gemini/Antigravity/Grok/Devin implementation
without verified official contracts." To reach `adapter-ready`: handshake probe of `grok agent stdio` in an isolated `HOME`
(not done), exit-code and cancel documentation or observation (U22, U23), and terms (U26).

## 6. Devin 3000.11.3 (installed after the first pass)

`command -v devin` → `/Users/enverfrancisco/.local/bin/devin`; `devin --version` → `devin 3000.11.3 (9c803229faa4)`.
The first pass of this note recorded Devin as "not installed" (verdict `blocked`); that no longer holds and is replaced
here. Only `--help` and subcommand `--help` ran. Not run: `devin auth login|logout|status`, `devin acp`, `devin -p`, any
interactive session, `devin doctor`, `devin models list`, `devin mcp *`, `devin cloud`, and no file under
`~/.config/devin` or `~/.local/share/devin` was read.

### Observed

```text
$ devin --help
A fast and minimal agent that lives both in your terminal and in the cloud.
Usage: devin [OPTIONS] [PATH]... [-- <PROMPT>...] [COMMAND]
Commands: auth, mcp, models, doctor, rules, skills, plugins, cloud, desktop, list [aliases: ls], rm, ssh, forward, update, version, migrate, sandbox, setup, uninstall, acp
  acp        Run as an ACP (Agent Client Protocol) server over stdio
  auth       Authentication related commands
  doctor     Diagnose the local Devin configuration
  sandbox    [Research Preview] Process sandboxing for the exec tool
  [PATH]...  Open Devin Desktop on the given path(s)
      --permission-mode <PERMISSION_MODE>   Modes: "auto" auto-approves read-only tools, "accept-edits" also auto-approves workspace edits, "smart" additionally auto-runs actions a fast model judges safe, "dangerous" auto-approves all tools.  [env: DEVIN_PERMISSION_MODE=] [default: auto]
      --sandbox   [Research Preview] Sandbox exec-tool processes (macOS seatbelt / Linux bwrap+seccomp)  [env: DEVIN_SANDBOX=]
      --cloud     Drive Devin cloud sessions instead of the local agent. The REPL talks to `devin acp --cloud`, which relays ACP to the Devin cloud ACP WebSocket. Requires a Devin account (sign in with `auth login`).
      --model <MODEL>   ... With -c/--continue or -r/--resume, switches the resumed conversation to this model  [env: DEVIN_MODEL=]
  -p, --print [<PROMPT>]   Print response and exit. Runs in non-interactive mode: processes the prompt and exits.
      --export [<PATH>]   Export conversation to a file. Exports after each turn.
  -c, --continue   Continue the most recent conversation
  -r, --resume [<SESSION_ID>]   Resume a conversation. Provide a session ID to resume a specific session, or omit to select interactively.
      --respect-workspace-trust [<RESPECT_WORKSPACE_TRUST>]   Defaults to true in every mode. Non-interactive (print) mode cannot show the trust prompt and fails in an untrusted directory; pass --respect-workspace-trust false to skip the check.
      --prompt-file <FILE> | --config <PATH> (Override the default user config file (~/.config/devin/config.json).)
 (no --cwd, no --output-format, no --input-format flag)
$ devin acp --help
Run as an ACP (Agent Client Protocol) server over stdio
Usage: devin acp [OPTIONS]
      --agent-type <AGENT_TYPE>   Possible values: summarizer (no tools; persists to ~/.local/share/devin/summaries/<session_id>.md) | review (read-only + shell tools)
      --model <MODEL>   Default model for every new ACP session  [env: DEVIN_MODEL=]
      --refusal-fallback <MODEL>   Models to switch to, in order, when the provider refuses a request under its usage policy  [env: DEVIN_REFUSAL_FALLBACK=]
      --cloud   Relay to Devin cloud instead of running the local agent. ... so the connected host drives cloud Devin sessions. Requires a Devin account (sign in with `auth login`).
  (no --permission-mode, no --sandbox, no --cwd on `devin acp`)
$ devin auth --help          login  Log in to Devin | logout  Log out and remove stored credentials | status  Check authentication status
$ devin auth status --help   Check authentication status      Usage: devin auth status      Options: -h, --help
$ devin doctor --help        Diagnose the local Devin configuration      --json  Emit machine-readable JSON instead of the text table
$ devin list --help          --format <FORMAT>  interactive (default) | json | csv
$ devin sandbox --help       [Research Preview] ...   setup  [Research Preview] Print the sandbox prerequisites for the current platform
$ devin mcp --help           add, list, get, remove, login, logout, enable, disable
$ devin models --help        list  List available models, organized by model family
```

### The 8 questions

| # | Question | Finding | Status |
|---|---|---|---|
| 1 | Headless / non-TTY | `devin -p/--print [<PROMPT>]` ("Runs in non-interactive mode: processes the prompt and exits"); prompt via `--prompt-file`. In an untrusted directory it fails unless `--respect-workspace-trust false`. | OBS |
| 2 | Structured stream | `devin acp` ("Run as an ACP (Agent Client Protocol) server over stdio"; ACP is JSON-RPC 2.0, S26). `-p` has no `--output-format` in help (U38). JSON exists only for `devin list --format json`, `devin doctor --json`, and `--export` (DOC: ATIF). | OBS + S26 |
| 3 | Session create/resume | CLI: `-c/--continue`, `-r/--resume [<SESSION_ID>]`, `devin list --format json`, `devin rm`. ACP: `session/new`, `session/load` (needs `loadSession`), `session/prompt`, `session/update`; whether `devin acp` advertises load/resume is unverified (U40). | OBS + S26 |
| 4 | Interrupt / cancel | No flag in help. ACP spec: `session/cancel` is a baseline notification every agent "MUST" support; the client "MUST" answer pending `session/request_permission` with outcome `cancelled`; the agent "MUST" return stop reason `cancelled`. Devin's implementation, and whether shell children die, is unverified (U32). | S26 (spec); U32 |
| 5 | Permissions | Flags: `--permission-mode` {`auto` (default; read-only tools auto), `accept-edits`, `smart`, `dangerous`}, `--sandbox`; these are on the top-level command, not on `devin acp --help` (U33). Under ACP the spec routes approval through `session/request_permission` with option kinds `allow_once, allow_always, reject_once, reject_always`; OMP, as the client, would answer `reject_once`/`cancelled` unless its own policy allows. Fail-closed mapping: never pass `dangerous`; do not advertise `fs`/`terminal` capabilities unless OMP wants to mediate them; answer only `allow_once`/`reject_once`, never `*_always`. Unknown option kinds → reject. This is more promising than every non-Codex CLI because the approval decision is made in-protocol by OMP, not by a settings file. Whether Devin actually asks via ACP for each tool is unverified (U33, U34). | OBS (flags) + S26 (spec); U33, U34 |
| 6 | Working directory | No `--cwd` flag (the `[PATH]...` positional means "Open Devin Desktop on the given path(s)", not a working directory). ACP `session/new` takes `cwd`: it "MUST be an absolute path" and "MUST be used for the session regardless of where the Agent subprocess was spawned" (S26). | OBS + S26 |
| 7 | Auth status | `devin auth status` ("Check authentication status", no options). Named as the candidate non-destructive probe; NOT run; side effects and exit semantics unverified (U39). ACP offers `authMethods` and `authenticate` (S26), but calling `authenticate` could start a login (U31). `devin doctor --json` is config diagnostics, not documented as auth. | OBS (help) + S26; U31, U39 |
| 8 | Exit codes | None in any help. DOC only (before install): `devin doctor` "exits with a non-zero status if any check fails". | U37 |

### Verdict

`capability-gated` (not `blocked`, and not yet `adapter-ready`). Why: the installed binary exposes `devin acp`, an ACP server
over stdio, and ACP is a public, versioned JSON-RPC contract with MUST-level `session/cancel`, `session/request_permission`
(four option kinds, `cancelled` outcome) and absolute `cwd`, so the interface and its permission/cancel channels are
specified, unlike Claude's `-p` flags or Gemini's/Antigravity's headless modes. It stays gated because nothing beyond
`--help` was exercised: no `initialize` handshake, so login need, advertised capabilities, cancel behaviour, and whether
`--permission-mode` or sandboxing reach the ACP child are unverified (U30–U34, U37–U42), and the delegation terms are unread
for this use (U28).

ADR 0043 non-goal engaged: "No Claude/Gemini/Antigravity/Grok/Devin implementation without verified official contracts."
The official contract (ACP) exists; what is not verified is Devin's conformance. The ADR also says Devin "remain[s]
disabled until official machine-readable interfaces are verified", and a `--help` line is not a handshake.
Boundary "No token/key extraction from Keychain, credential files or private caches" is respected as long as OMP never
reads `~/.config/devin`/`~/.local/share/devin`, never sets `WINDSURF_API_KEY`, and never calls `authenticate`.
Hard exclusions: `--cloud` (sends work to Devin cloud; U35), `--agent-type summarizer` (writes to `~/.local/share/devin/summaries`; U36),
`--permission-mode dangerous`, and `devin auth login|logout`.

To reach `adapter-ready` (needs owner approval, since it runs the binary): one `initialize` plus `session/new` plus a
`session/cancel` against `devin acp` in a throwaway `HOME` and an empty git repo, with no `authenticate` call and the network
observed; plus U30–U34.

Cloud API (DOC from the first pass, unchanged): Devin API v3 REST (`POST /v3/organizations/{org}/sessions`, service-user
keys prefixed `cog_`) is an asynchronous session service, a separate executor model from the local ACP agent; not part of
this verdict.

### 6.1 One ACP client could serve several CLIs

Checked with `<cli> --help` and `<cli> acp --help` only:

| CLI | ACP server mode from `--help` | Evidence |
|---|---|---|
| Gemini 0.59.0 | Yes | `--acp   Starts the agent in ACP mode` and `--experimental-acp ... (deprecated, use --acp instead)`. |
| Grok 1.0.46 | Yes | `grok agent --help` lists `stdio     Run the agent over stdio`; `grok --help` says `streaming-json: NDJSON: one ACP session update per line, the agent's native format`. `grok acp` is not a subcommand (prints the top-level help). |
| Devin 3000.11.3 | Yes | `devin acp --help`: "Run as an ACP (Agent Client Protocol) server over stdio". |
| Claude 2.1.296 | No | `claude --help` has no `acp` match; `claude acp --help` prints the top-level help (no such subcommand). `claude mcp serve` is an MCP server, not ACP. |
| Codex 0.160.0 | No | No `acp` match; `codex acp --help` prints the top-level help. `codex app-server` is a different JSON-RPC protocol. |
| Antigravity `agy` 1.1.10 | No | No `acp` match; `agy acp --help` prints flag usage. Its machine mode is `--output-format stream-json`. |

Consequence: three of the five non-Codex targets speak the same public protocol. A single ACP client adapter
(`initialize`, optional `authenticate`, `session/new|load`, `session/prompt`, `session/update`, `session/cancel`, answering
`session/request_permission`, optionally serving `fs/*` and `terminal/*`) could serve Gemini, Grok and Devin through
per-CLI launch profiles (argv, env, `authMethods` handling, permission mapping, exit-code table), instead of three
protocol-specific adapters. Codex remains its own adapter (app-server), Claude and Antigravity need CLI-specific ones or stay
disabled. This changes the plan in `docs/parked/2026-10-10-native-cli-integration-next-steps.md` from one adapter per CLI to
one ACP client plus profiles; the parked doc is annotated accordingly. It does not by itself change any verdict, because
the shared protocol is unverified against each binary (U29) and the per-vendor differences that matter for ADR 0043 (login
behaviour, permission flags honoured, cancel semantics, terms) are exactly the unverified ones.

## 7. Terms and auth ownership

Question per CLI: may OMP delegate to the official CLI without extracting credentials, according to official text read?
Only quoted official text counts; everything else is UNVERIFIED. This is evidence, not legal advice.

| CLI | Who owns auth (official text) | May OMP delegate without extracting credentials? |
|---|---|---|
| Codex | "Codex caches login details locally in a plaintext file at `~/.codex/auth.json` or in your OS-specific credential store." "treat `~/.codex/auth.json` like a password: it contains access tokens." App-server: "Codex owns the ChatGPT OAuth flow, persists tokens, and refreshes them automatically." `chatgptAuthTokens` is "intended for host apps that already own the user's ChatGPT auth lifecycle". `codex exec` "reuses saved CLI authentication by default." | **Yes, technically and by text**, if OMP only spawns `codex app-server` and never reads `auth.json` or uses `chatgptAuthTokens`. App-server is documented as the interface for "a deep integration inside your own product: authentication, conversation history, approvals, and streamed agent events", but also "experimental and aren't supported for production workloads". Contract terms for a third-party product are UNVERIFIED (U6): only OpenAI's consumer Terms of Use were read, and they list as prohibited "Automatically or programmatically extract data or Output"; applicability to a user-driven local agent was not resolved, Business Terms not read. |
| Claude Code | Credentials in Keychain or `~/.claude/.credentials.json` (authentication page); `claude auth` manages them. Legal page: "developers may not collect, store, or intermediate Claude.ai credentials or session tokens — sign-in to a Claude account must complete through Anthropic's own flow." | **Conditional.** Supportive: the page "does not … prevent an end user from signing in to the unmodified Claude Code binary with their own Claude subscription, including where a platform hosts Claude Code", provided "The Claude Code binary must not be modified" and "Customers may not pay for, resell, or intermediate Claude usage on their end users' behalf". Restrictive: "Anthropic does not permit third-party developers to offer Claude.ai login into their own applications, or to route requests through Free, Pro, or Max plan credentials on behalf of their users"; Agent SDK overview: "Use the API key authentication methods". Whether OMP spawning the user's own `claude` counts as "route requests … on behalf of" is UNVERIFIED (U10). API keys and Bedrock/Vertex/Foundry credentials are the documented route for developers. Naming: "Claude Code" must not be part of a product or feature name. |
| Gemini CLI | Cached credential, or `GEMINI_API_KEY`, or Vertex credentials (authentication page). | **UNVERIFIED (U16).** ToS page: "Directly accessing the services powering Gemini CLI … using third-party software, tools, or services (for example, using OpenClaw with Gemini CLI OAuth) is a violation of applicable terms and policies." The text targets direct access with the CLI's OAuth; it does not say whether launching the official binary from another product is allowed. Supported methods listed: Google account (Gemini Code Assist), Gemini API key, Vertex AI. |
| Antigravity | Keyring token profile, or `GEMINI_API_KEY` with `modelProvider: gemini` in settings (install page). Headless "uses your cached credentials. Authenticate once with an interactive `agy` session first." | **UNVERIFIED (U21).** Antigravity Terms item 6: "Using third party software, tools, or services to access the Service (e.g. using OpenClaw with Antigravity OAuth) is a breach of this Agreement." The vendor documents scripting and CI use of `agy -p`, but a third-party product driving it is not addressed. |
| Grok | `grok login` (browser OIDC or device code), API key `XAI_API_KEY`, external `auth_provider_command` (enterprise page). | **UNVERIFIED (U26).** Consumer ToS: "You may not share your account credentials or make your account available to anyone else." Enterprise ToS permits integrations through the API under an Order Form. Nothing read addresses a third-party product launching `grok`. |
| Devin | Credentials are held by the CLI (`devin auth login`; DOC: stored in `credentials.toml`, "treat it as sensitive"). `devin acp --cloud` help: "Requires a Devin account (sign in with `auth login`)". ACP spec: an agent advertises `authMethods` and the client calls `authenticate` only for protocol-driven methods. | **UNVERIFIED (U28, U30, U31).** Technically, OMP spawning `devin acp` without touching credentials is possible in principle. Cognition Platform ToS (June 30, 2026) bars making the Services "available to anyone other than Authorized Users"; nothing read addresses delegation. |

Common rule across all rows, from ADR 0043 and `docs/research/native-cli-integration.md`: OMP never reads another
application's Keychain item, auth file or token cache, and never sets `CLAUDE_CODE_OAUTH_TOKEN`-style long-lived
tokens on the user's behalf.

## 8. UNVERIFIED register

| ID | Item |
|---|---|
| U1 | Codex `exec`: exit-code table and SIGINT/SIGTERM behaviour (not in help; docs only for `login status`, `apply`, `cloud`). |
| U2 | Codex `app-server`: process exit codes, stdin-EOF and SIGTERM behaviour. |
| U3 | Codex `app-server`: behaviour when a server request (approval, user input, elicitation, tool call) is answered with a JSON-RPC error or never answered. |
| U4 | Codex: whether `ThreadStartResponse.approvalPolicy/sandbox` are post-merge effective values after `config.toml`/requirements layering. |
| U5 | Codex: doc examples (`unlessTrusted`, `workspaceWrite`) vs generated schema (`untrusted`, `workspace-write`); not sent to a live server. |
| U6 | Codex: contract terms for a third-party product embedding `app-server` under ChatGPT-managed auth (Business Terms not read; consumer clause quoted above unresolved). |
| U7 | Codex: whether a Rust-consumable protocol crate is published (only JSON Schema and TypeScript generators observed). |
| U8 | Codex: whether `codex login status`, `codex doctor`, `account/read` have side effects (token refresh, keyring prompt) on this machine; not executed. |
| U9 | Claude: **confirmed** (read 2026-10-14; Claude Code 2.1.296 / Agent SDK 0.3.296). Official package: `https://www.npmjs.com/package/@anthropic-ai/claude-agent-sdk/v/0.3.296`; the published `sdk.d.ts` says, "Envelope for a control-protocol request, sent by either side on the same stream as the messages", with `type: 'control_request'`, `request_id`, `type: 'control_response'`, `subtype: 'interrupt'`, and `subtype: 'can_use_tool'`. This settles the wire shape from an official vendor package; it does not prove a local binary handshake. |
| U10 | Claude: **confirmed** (read 2026-10-14; Claude Code 2.1.296). Official support: `https://support.claude.com/en/articles/15036540-use-the-claude-agent-sdk-with-your-claude-plan.md` says, "You can still use the Claude Agent SDK, `claude -p`, and third-party apps with your subscription limits." Official legal guidance: `https://code.claude.com/docs/en/legal-and-compliance.md` says an end user may sign in to "the unmodified Claude Code binary with their own Claude subscription", while developers may not route Free/Pro/Max credentials on behalf of users. Confirmed only for that own-user/unmodified-binary case; it does not authorize OMP to offer Claude.ai login or intermediate credentials. |
| U11 | Claude: **UNVERIFIED** (read 2026-10-14; Claude Code 2.1.296). Official CLI reference `https://code.claude.com/docs/en/cli-reference.md` documents `claude auth status` and its exit values, but does not document whether it is side-effect free. Searched the CLI reference, authentication page, changelog, and Agent SDK docs; no official statement about token refresh, keychain prompts, or side effects. |
| U12 | Claude: **confirmed** (read 2026-10-14; Claude Code 2.1.296 / Agent SDK 0.3.296). Official CLI reference `https://code.claude.com/docs/en/cli-reference.md` documents `--max-turns` ("Exits with an error when the limit is reached") and `--permission-prompt-tool`; it explicitly says "`claude --help` does not list every flag". The vendor package `https://www.npmjs.com/package/@anthropic-ai/claude-agent-sdk/v/0.3.296` identifies `claudeCodeVersion: 2.1.296` and its published subprocess source sends both flags. This settles the documented acceptance for the matching SDK/CLI release, although no local probe was run. |
| U13 | Gemini: **confirmed** (read 2026-10-14; Gemini CLI v0.59.0). Tagged vendor source `https://github.com/google-gemini/gemini-cli/blob/v0.59.0/packages/cli/src/acp/acpRpcDispatcher.ts` returns `protocolVersion`, `authMethods`, and `agentCapabilities.loadSession: true`, and defines `cancel`; `https://github.com/google-gemini/gemini-cli/blob/v0.59.0/packages/cli/src/acp/acpSession.ts` calls `requestPermission` and maps `outcome: 'cancelled'`. The vendor ACP page `https://github.com/google-gemini/gemini-cli/blob/v0.59.0/docs/cli/acp-mode.md` lists `initialize`, `authenticate`, `newSession`, `loadSession`, `prompt`, `cancel`, and `setSessionMode`. Confirmed from the exact tagged source; binary conformance remains unverified. |
| U14 | Gemini: **confirmed** (read 2026-10-14; Gemini CLI v0.59.0). Tagged vendor source `https://github.com/google-gemini/gemini-cli/blob/v0.59.0/packages/cli/src/utils/cleanup.ts` registers `SIGINT`/`SIGTERM` and its `gracefulShutdown` calls `process.exit(ExitCodes.SUCCESS)`; `https://github.com/google-gemini/gemini-cli/blob/v0.59.0/packages/core/src/utils/exitCodes.ts` defines `SUCCESS: 0` and `FATAL_CANCELLATION_ERROR: 130`. The non-interactive source only installs raw Ctrl-C handling when stdin is a TTY, so the documented headless signal path is the cleanup handler (0), not the interactive cancellation path (130). |
| U15 | Gemini: **refuted** (read 2026-10-14; Gemini CLI v0.59.0). The tagged vendor command registry `https://github.com/google-gemini/gemini-cli/blob/v0.59.0/packages/cli/src/config/config.ts` registers only `mcp`, `extensions`, `skills`, `hooks`, and `gemma`; the authentication command implementation `https://github.com/google-gemini/gemini-cli/blob/v0.59.0/packages/cli/src/ui/commands/authCommand.ts` has `signin`, `signout`, and `auth`, not a non-destructive status command. |
| U16 | Gemini: **refuted** (read 2026-10-14; Gemini CLI docs current for the v0.59.0-era product). Official FAQ `https://geminicli.com/docs/resources/faq/` says, "Using third-party software, tools, or services to harvest or piggyback on Gemini CLI's OAuth authentication ... is a direct violation" and recommends a Vertex AI or Google AI Studio API key for a third-party coding agent. The official ToS page `https://geminicli.com/docs/resources/tos-privacy/` repeats that direct third-party access is a violation. |
| U17 | Antigravity: **UNVERIFIED** (read 2026-10-14; target `agy` 1.1.10). Searched official CLI reference/headless pages and the tagged vendor changelog `https://github.com/google-antigravity/antigravity-cli/blob/1.1.10/CHANGELOG.md`; the 1.1.10 changelog does not document `--input-format`, while the later 1.1.15 release explicitly says it "Added `--input-format stream-json`". That later-release evidence does not establish support in 1.1.10. |
| U18 | Antigravity: **confirmed** (read 2026-10-14; `agy` 1.1.10 and later). Official headless docs `https://antigravity.google/docs/cli/headless.md` say diagnostics, authentication prompts, progress and permission notices go to `stderr`, while `stream-json` is NDJSON on `stdout`; they state, "A tool that requires approval it can't obtain is soft-denied: the run continues, exits `0`, and prints a notice to `stderr`". The vendor changelog `https://github.com/google-antigravity/antigravity-cli/blob/main/CHANGELOG.md` records the soft-deny behavior in 1.1.3, before 1.1.10. Therefore no soft-denial event is promised in the stream. |
| U19 | Antigravity: **UNVERIFIED** (read 2026-10-14; `agy` 1.1.10). Searched `https://antigravity.google/docs/cli/headless.md`, `https://antigravity.google/docs/cli/reference.md`, the 1.1.10 tagged changelog, and the current vendor changelog. They document status `INTERRUPTED` and `SIGINT` as an example, but no numeric SIGINT exit code for 1.1.10; `control_request` input is documented as an error/exit-2 path, not a SIGINT contract. |
| U20 | Antigravity: **refuted** (read 2026-10-14; `agy` 1.1.10). Official CLI reference `https://antigravity.google/docs/cli/reference.md` lists account-related commands `/usage`, `/credits`, and `/logout`; the official install/auth page `https://antigravity.google/docs/cli/install.md` documents sign-in, keyring credentials and `/logout`, but no auth-status command. |
| U21 | Antigravity: **refuted** (read 2026-10-14; consumer CLI docs current for 1.1.10 product line). Official FAQ `https://antigravity.google/docs/faq.md` says, "Wrapping `agy` inside third-party coding agents or applications ... is not supported under consumer plans"; it separately says using third-party software to access Antigravity violates the Terms and recommends a Gemini Enterprise or Google AI Studio API key. |
| U22 | Grok: **UNVERIFIED** (read 2026-10-14; target Grok 1.0.46). Searched official `https://docs.x.ai/build/cli/headless-scripting.md`, `https://docs.x.ai/build/cli/reference.md`, `https://x.ai/changelog/build`, and the vendor source repository `https://github.com/xai-org/grok-build`; the current public xAI docs do not provide a numeric exit-code table for 1.0.46. An official source snapshot's 1.0.45 headless guide has a table, but that version mismatch prevents applying it to 1.0.46. |
| U23 | Grok: **confirmed** (read 2026-10-14; Grok Build 1.0.42+ / official source snapshot 1.0.45). Official changelog `https://x.ai/changelog/build` says in v1.0.42, "`grok -p` exits cleanly when interrupted and performs session cleanup". The vendor source `https://github.com/xai-org/grok-build/blob/main/crates/codegen/xai-grok-shell/src/agent/mvp_agent/acp_agent.rs` implements `session/cancel` by sending `SessionCommand::Cancel`, and `https://github.com/xai-org/grok-build/blob/main/crates/codegen/xai-grok-shell/src/session/acp_session_impl/turn.rs` maps `TurnOutcome::Cancelled` to `acp::StopReason::Cancelled`. |
| U24 | Grok: **confirmed** (read 2026-10-14; Grok Build 1.0.45 source / 1.0.46 docs). Official permissions docs `https://docs.x.ai/build/features/permissions.md` say `dontAsk` silently denies without an explicit allow rule; the vendor guide `https://github.com/xai-org/grok-build/blob/main/crates/codegen/xai-grok-pager/docs/user-guide/22-permissions-and-safety.md` says, "CLI overrides config for that process" and "`dontAsk` ... [is] deny-by-default". Tagged-source-equivalent vendor code also applies `--permission-mode` to the main agent definition before permission resolution. |
| U25 | Grok: **confirmed** (read 2026-10-14; Grok Build 1.0.45 source / 1.0.46 docs). The official vendor source `https://github.com/xai-org/grok-build/blob/main/crates/codegen/xai-grok-pager/src/models.rs` calls `AuthStatus::resolve` and prints "You are using XAI_API_KEY", "You are logged in with ...", or "You are not authenticated." `grok models` is therefore a non-destructive command that reports auth status, even though no command is named `auth status`. |
| U26 | Grok: **UNVERIFIED** (read 2026-10-14; target Grok 1.0.46). Official docs and vendor README advertise headless/ACP integration into "custom apps" (`https://docs.x.ai/build/cli/headless-scripting.md`, `https://github.com/xai-org/grok-build/blob/main/README.md`), while the consumer ToS `https://x.ai/legal/terms-of-service` says users may not share credentials or make accounts available to anyone else. No official text found that resolves whether a third-party product may launch Grok with a user's cached OAuth account rather than an API key. |
| U27 | Grok: **refuted** (read 2026-10-14; Grok Build 1.0.45 source / 1.0.46 docs). The vendor guide `https://github.com/xai-org/grok-build/blob/main/crates/codegen/xai-grok-pager/docs/user-guide/14-headless-mode.md` says `-s/--session-id` creates a **new** UUID session and "does not resume"; the official xAI sessions page `https://docs.x.ai/build/features/sessions.md` agrees, while the xAI headless table's older "create or resume" wording is contradicted by the more specific current sessions documentation and CLI source/help. |
| U28 | Devin: whether a third-party product launching the local `devin` binary (or `devin acp`) is allowed; the Cognition Platform ToS read bars making the Services "available to anyone other than Authorized Users" and says nothing about delegation. |
| U29 | Cross-CLI: no ACP handshake probe was run for Gemini, Grok or Devin (only `--help` was allowed). Conformance of each agent to the public ACP spec is therefore unverified. |
| U30 | Devin: whether `devin acp` needs a prior login to answer `initialize`, and whether `initialize` or `session/new` reads credentials or touches the network. |
| U31 | Devin: which `authMethods` `devin acp` advertises and what `authenticate` does (it could start a browser login). OMP would never call `authenticate` for it. |
| U32 | Devin: whether `session/cancel` is implemented, returns `stopReason: "cancelled"`, and kills shell children the agent started. |
| U33 | Devin: whether `--permission-mode` is honored under `devin acp`, and which tools raise `session/request_permission` in the default mode `auto`; whether `smart` makes extra model calls. |
| U34 | Devin: whether `devin acp` runs shell commands itself or through client `terminal/*` (decides whether OMP can withhold the `terminal` capability to fail closed). |
| U35 | Devin: `--cloud` (on both `devin` and `devin acp`) relays to the Devin cloud ACP WebSocket, so work leaves the machine; out of scope and must never be passed. Whether any default or config can turn it on implicitly was not checked. |
| U36 | Devin: `devin acp --agent-type summarizer` persists to `~/.local/share/devin/summaries/<session_id>.md` (OBS in help); never used. Whether the default agent also writes outside the workspace was not checked. |
| U37 | Devin: exit codes for `-p`, `acp`, `auth status` (none in help; docs only for `devin doctor`). |
| U38 | Devin: `-p` has no `--output-format` in help, so its stdout is not a documented machine format; ACP is the only structured route found. |
| U39 | Devin: `devin auth status` is the candidate non-destructive auth probe; side effects (token refresh, network call, keychain prompt) and exit-code semantics are unverified; not run. |
| U40 | Devin: whether `devin acp` advertises `loadSession` / `sessionCapabilities.resume`, and how a session id from `devin list --format json` relates to ACP `sessionId`. |
| U41 | Devin: workspace trust under ACP (print mode "fails in an untrusted directory"); whether `--respect-workspace-trust` applies to `acp`, and whether the trust store is read or written. |
| U42 | Devin: doc vs help drift: docs list `--permission-mode normal, accept-edits, smart, dangerous, autonomous`; help lists `auto, accept-edits, smart, dangerous` (default `auto`). Docs say `devin acp` reads `WINDSURF_API_KEY`; help does not mention it, and OMP would never set it. |

Pages read only in part: Claude `authentication.md` (300 of 314 lines),
`agent-sdk/typescript.md` (searched, not read through), Gemini `policy-engine` (first 300 of 635 lines),
Antigravity `permissions.md` (first 300 of 378), Devin `commands.md` (403 of 625 plus search) and `permissions.md`
(first 300 of 468), both xAI Terms pages (all but the last few lines), Codex `non-interactive-mode` and
`app-server` (searched for the relevant sections, not read through).

## 9. Sources (initial set read 2026-10-13; additions S27-S33 read 2026-10-14)

| ID | URL | Notes |
|---|---|---|
| S1 | `https://developers.openai.com/codex/noninteractive` | Tool resolved to `https://learn.chatgpt.com/docs/non-interactive-mode`; no page date shown. |
| S2 | `https://developers.openai.com/codex/app-server` | Resolved to `https://learn.chatgpt.com/docs/app-server`. |
| S3 | `https://learn.chatgpt.com/docs/auth.md` | Login caching, credential storage, `codex login status`. |
| S4 | `https://learn.chatgpt.com/docs/auth/ci-cd-auth.md` | `auth.json` handling. |
| S5 | `https://learn.chatgpt.com/docs/developer-commands.md` | `login status` exit 0; `mcp-server` removed. |
| S6 | `https://learn.chatgpt.com/docs/open-source.md` | App-server sources in `openai/codex`. |
| S7 | `https://openai.com/policies/terms-of-use/` | Canonical `…/row-terms-of-use/`, "Effective: January 1, 2026". |
| S8 | `https://code.claude.com/docs/en/headless` | Markdown alternate; headless, SIGTERM/SIGINT, permission prompts, session resume. |
| S9 | `https://code.claude.com/docs/en/cli-reference.md` | Flags, `auth status` exit codes. |
| S10 | `https://code.claude.com/docs/en/agent-sdk/overview.md` | claude.ai login note, Commercial Terms. |
| S11 | `https://code.claude.com/docs/en/agent-sdk/typescript.md`, `…/streaming-vs-single-mode.md` | `interrupt()`, control capabilities. |
| S12 | `https://code.claude.com/docs/en/legal-and-compliance.md`, `…/authentication.md` | Authentication and credential use. |
| S13 | `https://geminicli.com/docs/cli/headless/` | "Last updated: Mar 10, 2026". |
| S14 | `https://geminicli.com/docs/cli/acp-mode/` | "Last updated: Apr 10, 2026". |
| S15 | `https://geminicli.com/docs/cli/cli-reference/` | "Last updated: May 1, 2026". |
| S16 | `https://geminicli.com/docs/get-started/authentication/` | "Last updated: Sep 18, 2026". |
| S17 | `https://geminicli.com/docs/resources/tos-privacy/` | "Last updated: Apr 10, 2026". |
| S18 | `https://geminicli.com/docs/reference/policy-engine/` | Partial. |
| S19 | `https://developers.googleblog.com/an-important-update-transitioning-gemini-cli-to-antigravity-cli` | Dated May 19, 2026. |
| S20 | `https://antigravity.google/docs/cli/headless.md`, `…/install.md`, `…/reference.md`, `…/gcli-migration` (redirects to `/docs/cli/gcli-migration`), `https://antigravity.google/docs/permissions.md`, `https://antigravity.google/terms`, `https://antigravity.google/llms.txt` | `https://antigravity.google/docs/cli/permissions.md` returned HTTP 404, so the CLI permission rules were read from `/docs/permissions.md` (CLI section). No page dates shown. |
| S21 | `https://docs.x.ai/build/overview.md`, `…/build/cli/headless-scripting.md`, `…/build/cli/reference.md`, `…/build/features/permissions.md`, `…/build/features/sessions.md`, `…/build/features/sandbox.md`, `…/build/enterprise.md`, `https://docs.x.ai/llms.txt` | No page dates shown. |
| S22 | `https://x.ai/legal/terms-of-service` ("Last Updated: October 12, 2026"), `https://x.ai/legal/terms-of-service-enterprise` ("Last Updated: August 14, 2026") | Nearly full reads. |
| S23 | `https://docs.devin.ai/get-started/devin-intro` (resolved from `https://docs.devin.ai/`), `https://docs.devin.ai/cli`, `…/cli/reference/commands.md`, `…/cli/reference/permissions.md`, `…/cli/reference/keyboard-shortcuts.md`, `…/cli/enterprise/devin-auth.md`, `…/api-reference/overview.md`, `https://docs.devin.ai/llms.txt` | Partial reads noted above. Read before the binary was installed; where they disagree with `devin --help` 3000.11.3 the help wins (U42). |
| S24 | `https://cognition.com/legal/platform-terms-of-service` | Requested `cognition.ai/pages/terms-of-service`; "Last updated: June 30, 2026". |
| S25 | `https://agentclientprotocol.com/protocol/v1/prompt-turn.md`, `…/session-setup.md` | Public ACP spec: `session/cancel`, `session/request_permission`, `session/new` with `cwd`, `session/load`. |
| S26 | `https://agentclientprotocol.com/protocol/v1/overview.md`, `…/initialization.md`, `…/authentication.md`, `…/tool-calls.md` | Public ACP spec (read 2026-10-13 for the Devin update): method list, MUST-level cancel and permission outcomes, `authMethods`, absolute paths. Local `<cli> --help` for `devin` 3000.11.3 is OBS, not a source. |
| S27 | `https://www.npmjs.com/package/@anthropic-ai/claude-agent-sdk/v/0.3.296`, `https://code.claude.com/docs/en/cli-reference.md`, `https://code.claude.com/docs/en/legal-and-compliance.md`, `https://support.claude.com/en/articles/15036540-use-the-claude-agent-sdk-with-your-claude-plan.md` | Official Claude package/type contract, CLI flags, legal conditions, and support statement for third-party app usage. |
| S28 | `https://github.com/google-gemini/gemini-cli/blob/v0.59.0/packages/cli/src/acp/acpRpcDispatcher.ts`, `…/packages/cli/src/acp/acpSession.ts`, `…/packages/cli/src/utils/cleanup.ts`, `…/packages/core/src/utils/exitCodes.ts`, `…/packages/cli/src/config/config.ts`, `…/packages/cli/src/ui/commands/authCommand.ts`, `https://github.com/google-gemini/gemini-cli/blob/v0.59.0/docs/cli/acp-mode.md` | Official tagged v0.59.0 source for ACP, signals, exit codes and command registry. |
| S29 | `https://geminicli.com/docs/resources/faq/`, `https://geminicli.com/docs/resources/tos-privacy/` | Official Gemini FAQ/ToS: OAuth piggybacking is prohibited; API-key alternatives are recommended. |
| S30 | `https://github.com/google-antigravity/antigravity-cli/blob/1.1.10/CHANGELOG.md`, `https://github.com/google-antigravity/antigravity-cli/blob/main/CHANGELOG.md`, `https://antigravity.google/docs/cli/headless.md`, `https://antigravity.google/docs/cli/reference.md`, `https://antigravity.google/docs/cli/install.md`, `https://antigravity.google/docs/faq.md` | Official Antigravity CLI repository/releases and vendor docs for 1.1.10 input-format history, stream/soft-deny behavior, auth commands and third-party restrictions. |
| S31 | `https://github.com/xai-org/grok-build/blob/main/crates/codegen/xai-grok-pager/docs/user-guide/14-headless-mode.md`, `…/15-agent-mode.md`, `…/22-permissions-and-safety.md`, `…/crates/codegen/xai-grok-pager/src/models.rs`, `…/crates/codegen/xai-grok-shell/src/agent/mvp_agent/acp_agent.rs`, `…/crates/codegen/xai-grok-shell/src/session/acp_session_impl/turn.rs`, `https://github.com/xai-org/grok-build/blob/main/README.md` | Official Grok Build source/docs snapshot: exit/cancel/session/permission/auth-banner behavior and custom-app integration. |
| S32 | `https://docs.x.ai/build/cli/headless-scripting.md`, `https://docs.x.ai/build/features/sessions.md`, `https://docs.x.ai/build/features/permissions.md`, `https://docs.x.ai/build/cli/reference.md`, `https://x.ai/changelog/build` | Official xAI docs/changelog for Grok headless, session, permission, ACP, and release behavior. |
| S33 | `https://x.ai/legal/terms-of-service` | Official consumer terms read 2026-10-14; credential sharing/delegation remains unresolved for a third-party launcher. |
