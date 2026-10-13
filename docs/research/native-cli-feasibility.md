# OMP² Native CLI Feasibility: Codex, Claude Code, Gemini, Antigravity, Grok, Devin

Status: feasibility research; no OMP implementation.
Date: 2026-10-13
Closes: CLI-4, CLI-5, CLI-6 (`docs/parked/2026-10-10-native-cli-integration-next-steps.md`); refines CLI-2.
Context: `docs/adr/0043-native-cli-integration.md` (proposed, not edited by this note),
`docs/research/native-cli-integration.md`.

## Summary

Installed and probed on this Mac: `codex` 0.160.0, `gemini` 0.59.0, `agy` 1.1.10 (Antigravity CLI),
`grok` 1.0.46, `claude` 2.1.296. `devin` and `antigravity` are NOT installed.

| CLI | Version | Verdict | Reason (one sentence) | ADR 0043 non-goal |
|---|---|---|---|---|
| Codex | 0.160.0 | `adapter-ready` | `codex app-server` speaks documented JSON-RPC over stdio JSONL, and its whole request/notification/approval/interrupt surface was emitted by the installed binary (`generate-json-schema`: 104 client requests, 10 server requests, 83 notifications). | Satisfies "No credential migration or extraction." when OMP spawns `codex app-server --listen stdio://` itself and never reads `auth.json` or uses `chatgptAuthTokens`. |
| Claude Code | 2.1.296 | `capability-gated` | `-p` with `--output-format stream-json`, `--input-format stream-json`, session and permission flags exist, but the control/interrupt wire contract was read only as Agent SDK types and delegation under subscription auth is unresolved (U9, U10). | Implementing now would contradict "No Claude/Gemini/Antigravity/Grok/Devin implementation without verified official contracts." |
| Gemini CLI | 0.59.0 | `capability-gated` | `--acp` (JSON-RPC) and `-o stream-json` exist with a documented exit-code table, but nothing beyond `--help` was verified on the binary, headless interrupt is undocumented, and there is no auth-status command (U13-U16). | Implementing now would contradict "No Claude/Gemini/Antigravity/Grok/Devin implementation without verified official contracts." |
| Antigravity | `agy` 1.1.10 | `capability-gated` | A documented `json`/`stream-json` print mode exists, but there is no in-protocol cancel or approval channel (`control_request` is rejected), `--input-format` is absent from `--help`, and soft-denied tools are reported on stderr, not in the stream (U17-U21). | Implementing now would contradict "No Claude/Gemini/Antigravity/Grok/Devin implementation without verified official contracts."; the Boundary "No generic human-output parser when a structured interface is unavailable." holds only if `--output-format` is never `text`. |
| Grok | 1.0.46 | `capability-gated` | `-p --output-format streaming-json` (ACP updates as NDJSON) and `grok agent stdio` (ACP) exist with `--cwd` and `dontAsk`, but exit codes, cancel semantics and auth status are undocumented (U22-U27). | Implementing now would contradict "No Claude/Gemini/Antigravity/Grok/Devin implementation without verified official contracts." |
| Devin | not installed | `blocked` | No binary to observe, so no machine interface is verified locally; the docs claim `devin acp`, `-p` and `devin auth status` (U28). | Implementing now would contradict "No Claude/Gemini/Antigravity/Grok/Devin implementation without verified official contracts." |

Adapter-ready: **Codex only.** The other four installed CLIs have real headless/structured modes, but
none passed the bar below, so they stay disabled by the gate, not by absence of an interface.

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
- Three installed or documented CLIs speak ACP (Gemini `--acp`, Grok `agent stdio`, Devin `devin acp` per docs).
  One ACP client could serve all of them; that needs a handshake probe in an isolated `HOME` first (not done here).

## Method and evidence rules

- OBS: observed in this session from a local command (command shown, output quoted; wrapped lines joined).
- SCHEMA: observed in files generated by the installed binary into `/tmp` (Codex only).
- DOC: vendor page text read on 2026-10-13; not exercised.
- UNVERIFIED (Un): neither observed nor documented in the text read, or doc and binary disagree.
- INFERENCE: my reading, not observed.

Commands run: `--help`, `--version` and subcommand `--help` for all five CLIs, plus
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

## 6. Devin (no CLI installed)

`command -v devin antigravity` printed nothing and exited 1. No Devin interface was observed. Everything below is
vendor documentation read 2026-10-13 and is UNVERIFIED locally (U28).

DOC (`docs.devin.ai/cli/reference/commands.md`, partial read):

- Headless: `devin -p/--print [PROMPT]` "Print response and exit (non-interactive mode)"; `--respect-workspace-trust false` is needed "in an untrusted directory".
- Structured output: no JSON or NDJSON mode for `-p` found. JSON exists for `devin models list --format json`, `devin list --format json`, `devin doctor --json`, and `--export` ("ATIF format"). ACP: `devin acp` "Run Devin as an Agent Client Protocol (ACP) server over stdio … speaks JSON-RPC over stdin/stdout".
- Sessions: `-c`, `-r <SESSION_ID>`, `--cloud` ("Drive Devin Cloud sessions instead of the local agent").
- Permissions: `--permission-mode normal, accept-edits, smart, dangerous, autonomous`; `--sandbox`; rules `allow/ask/deny` with "A deny rule always wins".
- Auth status: `devin auth status` ("Check authentication status"); `devin auth login/logout`.
- Exit codes: only "`devin doctor` … exits with a non-zero status if any check fails".
- Cancel: interactive `Esc` / `Ctrl+C` only (keyboard-shortcuts page).
- Cloud: Devin API v3 REST (`POST /v3/organizations/{org}/sessions`), service-user keys prefixed `cog_`. This supports the earlier note's reading that Devin Cloud is an asynchronous session service, not a token stream (the pages read do not describe any streaming endpoint).

Verdict: `blocked` (nothing verified; this reflects the missing binary, not a product judgement).
ADR 0043 non-goal: "No Claude/Gemini/Antigravity/Grok/Devin implementation without verified official contracts."

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
| Devin | `devin auth login` stores an API token in `credentials.toml`; "treat it as sensitive". `devin acp` reads `WINDSURF_API_KEY` or the stored credentials. | **UNVERIFIED (U28).** Cognition Platform ToS (June 30, 2026) bars making the Services "available to anyone other than Authorized Users"; nothing read addresses delegation. No CLI installed. |

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
| U9 | Claude: a CLI-level wire spec for `control_request`/`control_response` (only the Agent SDK reference was read, partly). |
| U10 | Claude: whether a third-party product spawning the user's own subscription-authenticated `claude` is permitted ("route requests … on behalf of their users"). |
| U11 | Claude: whether `claude auth status` is side-effect free; not executed. |
| U12 | Claude: `--max-turns` and `--permission-prompt-tool` are documented flags but have no entry in `claude --help` 2.1.296 (the latter is only named inside the `--permission-prompts` description); acceptance by the binary was not probed. |
| U13 | Gemini: ACP behaviour on 0.59.0 (`initialize`, `cancel`, `session/request_permission`, `loadSession`); the ACP page lists `cancel` and `setSessionMode` but not permission requests. |
| U14 | Gemini: headless SIGINT behaviour and exit code (table is 0, 1, 42, 53). |
| U15 | Gemini: any non-destructive auth-status command (none found). |
| U16 | Gemini: whether wrapping the official CLI under Google-account OAuth is allowed. |
| U17 | Antigravity: `--input-format stream-json` is documented but absent from `agy --help` 1.1.10; support unconfirmed. |
| U18 | Antigravity: how a soft-denied tool appears in `stream-json` (docs say stderr notice, exit 0). |
| U19 | Antigravity: SIGINT exit code; no in-protocol cancel (`control_request` rejected). |
| U20 | Antigravity: any non-destructive auth-status command (none found). |
| U21 | Antigravity: whether a third-party product driving `agy` is allowed. |
| U22 | Grok: exit codes. |
| U23 | Grok: cancel semantics (`session/cancel` on `agent stdio`, SIGINT on `-p`). |
| U24 | Grok: headless behaviour when a prompt would be needed without `dontAsk`; precedence of `--permission-mode` over `~/.grok/config.toml`. |
| U25 | Grok: any non-destructive auth-status command (none found). |
| U26 | Grok: whether a third-party product launching `grok` is allowed. |
| U27 | Grok: `-s/--session-id` doc vs help disagreement. |
| U28 | Devin: everything (not installed): `-p` output format, exit codes, cancel, ACP details, `auth status` behaviour, delegation terms. |
| U29 | Cross-CLI: no ACP handshake probe was run for Gemini, Grok or Devin (out of the allowed command set). |

Pages read only in part: Claude `authentication.md` (300 of 314 lines),
`agent-sdk/typescript.md` (searched, not read through), Gemini `policy-engine` (first 300 of 635 lines),
Antigravity `permissions.md` (first 300 of 378), Devin `commands.md` (403 of 625 plus search) and `permissions.md`
(first 300 of 468), both xAI Terms pages (all but the last few lines), Codex `non-interactive-mode` and
`app-server` (searched for the relevant sections, not read through).

## 9. Sources (all read 2026-10-13)

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
| S23 | `https://docs.devin.ai/get-started/devin-intro` (resolved from `https://docs.devin.ai/`), `https://docs.devin.ai/cli`, `…/cli/reference/commands.md`, `…/cli/reference/permissions.md`, `…/cli/reference/keyboard-shortcuts.md`, `…/cli/enterprise/devin-auth.md`, `…/api-reference/overview.md`, `https://docs.devin.ai/llms.txt` | Partial reads noted above. |
| S24 | `https://cognition.com/legal/platform-terms-of-service` | Requested `cognition.ai/pages/terms-of-service`; "Last updated: June 30, 2026". |
| S25 | `https://agentclientprotocol.com/protocol/v1/prompt-turn.md`, `…/session-setup.md` | Public ACP spec: `session/cancel`, `session/request_permission`, `session/new` with `cwd`, `session/load`. |
