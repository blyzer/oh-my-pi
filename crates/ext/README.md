# omp-ext

Extension configuration, dependency resolution, lockfiles, index metadata, and
local trust state for OMP.

## Structural philosophy

The crate is a pure domain: deterministic data transformations over declared
configuration and durable on-disk state. It owns no process, no connection, and
no CLI. Everything that needs a running Environment — Git materialization,
site publication — lives in the host or the CLI driver above it, so this crate
sits below both and can be reasoned about as data in, data out.

- `config`: declared extension configuration, overlays, scopes, and CLI
  contributions.
- `lock`: reproducible lockfiles and local installed/enabled records.
- `resolver`: the `uv` resolution driver and R1-R12 policy checks.
- `trust`: signature verification, trust tiers, and the local grant file,
  including operator approvals of plugin-launched commands.
- `plugin_command`: the approval key (`Hash32` digest of plugin version,
  command, arguments, environment, working directory, a hook's event and
  matcher, and the contents of every plugin-root file the launch names, so a
  script edited in place asks again; a named file that cannot be read is
  never approvable) and typed refusal for every command a plugin would run: an
  installed plugin's MCP, LSP, and DAP servers and its hooks (approved under
  `name@marketplace`), and an Agent Plugins 1.0 package's stdio MCP servers
  (approved under its manifest name). Plugin resolution reads the approvals
  (`CommandApprovals`) and each launching seam gates through them. The
  digest domain is `omp.plugin-command.v2`: approvals recorded before files
  were bound match nothing, so plugin commands are approved once more.
- `index`, `upgrade`, `doctor`: index metadata, generation commits, and
  integrity diagnostics.
- `marketplace`, `claude_plugin`: Claude-compatible marketplace catalogs, the
  `installed_plugins.json` registry `omp ext install` writes, and the
  resolution of enabled installs into contained plugin roots whose skills,
  commands, rules, MCP, LSP/DAP servers, and hooks the driver and Environment
  discovery load. Claude Code's own registry merges in read-only
  (`ClaudeCodeHome`); omp's registries win for the same id.
- `claude_hooks`: Claude Code hook declarations (`hooks/hooks.json`, manifest
  `hooks`) parsed into typed hooks, plus the data tables mapping Claude events
  onto omp hook seams and Claude tool names onto omp tool families.

## Claude Code hook events

Every event of the Claude Code hooks reference either runs at the omp
lifecycle point with its semantics (`ClaudeHookEvent::seam`; the driver's
plugin hook host runs it) or is unsupported. A hook on an unsupported event
never runs, and the launching host names it once per plugin and event (chat
notice, print-mode `warning:` on stderr, and the RPC/ACP log): "plugin `id`
declares a `Event` hook: `Event` is not supported by omp; this hook will not
run". Every mapped hook still needs the operator's approval of its command and
trigger (`omp ext trust`).

| Event | omp lifecycle point | Notes |
| --- | --- | --- |
| `SessionStart` | `session_start` | Matcher `startup`/`resume`; context reaches the next prompt. |
| `SessionEnd` | `session_shutdown` | Quit, a finished print/RPC/ACP run, `/new` (`clear`), `/resume` (`resume`), fork/hand-off/signal (`other`). Runs within the 1.5 s shutdown budget, whatever its `timeout`; output discarded. |
| `UserPromptSubmit` | `before_agent_start` | Main session only. |
| `PreToolUse` | `tool_call` | |
| `PostToolUse` | `tool_result` (`ok`) | |
| `PostToolUseFailure` | `tool_result` (`faulted`) | |
| `Stop` | `agent_settled` | Main session only. |
| `SubagentStop` | `agent_settled` | Subagent kernels only. |
| `SubagentStart` | `before_agent_start`, a subagent's first prompt | Matcher on the agent class; `additionalContext` opens the subagent's context. |
| `StopFailure` | `agent_end` of a turn that failed on a provider error | `error` from the provider failure category; output discarded. |
| `PreCompact` | `compaction` | Exit 2 / `block` cancels the compaction. |
| `PostCompact` | `compaction_done` | `trigger` and `compact_summary`; output discarded. |
| `Notification` | `tool_approval_requested` | `permission_prompt` only, when the approval prompt is filed (no idle delay); omp raises no other notification type. |
| `Setup` | unsupported | omp has no `--init`/`--maintenance` run. |
| `UserPromptExpansion` | unsupported | Prompt templates expand without a lifecycle point (`command_invoke` is not emitted). |
| `PostToolBatch` | unsupported | No awaited point between a resolved tool batch and the next request: `turn_end` is a non-blocking observation and `turn_start` sees an already built request, so neither context nor a block could land before the next model call. |
| `PermissionRequest` | unsupported | `tool_approval_requested` is observe-only: a hook cannot answer omp's approval prompt. |
| `PermissionDenied` | unsupported | omp has no auto-mode classifier whose denial a hook could let the model retry. |
| `TaskCreated`, `TaskCompleted` | unsupported | omp's `todo` list has no create/complete lifecycle point a hook could roll back. |
| `TeammateIdle` | unsupported | omp has no agent teams. |
| `FileChanged` | unsupported | omp has no watched-file hook point. |
| `ConfigChange` | unsupported | Configuration changes have no blockable lifecycle point. |
| `CwdChanged` | unsupported | The session shell's working directory changes without a lifecycle point. |
| `DirectoryAdded` | unsupported | Directories are added only at launch (`--add-dir`). |
| `InstructionsLoaded` | unsupported | Context files and rules are discovered once at composition, without a lifecycle point. |
| `WorktreeCreate`, `WorktreeRemove` | unsupported | Subagent worktree isolation has no hook point that could replace creation or removal. |
| `PreModelSwitch` | unsupported | Model changes have no blockable point. |
| `PostModelSwitch` | unsupported | `model_changed` fires only when the model changes between requests of one agent run, not for `/model` between prompts; most switches would never reach the hook. |
| `Elicitation`, `ElicitationResult` | unsupported | omp's MCP client does not offer elicitation, so no server ever asks. |
| `MessageDisplay` | unsupported | omp has no display-transform point: the TUI renders the journaled text, and `message_update` is an observation that cannot replace it. |
