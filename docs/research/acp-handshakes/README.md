# ACP handshake transcripts

Evidence for the ADR 0043 capability gate: what a real binary does when an ACP client talks to it.
Recorded by `scripts/acp-probe.py`, opt-in, never in CI. Raw JSON-RPC, not OMP's client. The first three
runs below use a fresh temporary `HOME` and a cleared environment; the signed-in runs further down use
the vendor's own existing login (see `--inherit-login`). Every permission request is answered with a rejection.

| Binary | Version | Transcript | initialize | session/new | prompt | cancel | permission |
|---|---|---|---|---|---|---|---|
| DeepSeek Harness `dsh --profile acp` | 0.2.0-rc.2 | [dsh](dsh-no-credential.md) | ok | ok | auth error | not reached | not reached |
| Devin `devin acp` | 3000.11.3 | [devin](devin-no-credential.md) | ok | ok (no login needed) | auth error | not reached | not reached |
| Grok `grok agent --no-leader stdio` | 1.0.46 | [grok](grok-no-credential.md) | ok | **auth error** | not reached | not reached | not reached |

None of the three was signed in inside the probe, so no run got as far as a model turn. The three steps
that matter most for the gate (a completed prompt, `session/cancel` mid-turn, a denied permission) are
**not recorded for any binary** in these three. Claude (bridge) and Antigravity are not probed.

## Signed-in runs, 2026-10-21

Each run: `initialize`, `session/new`, optional `session/set_mode` or `session/set_config_option`, a prompt
that asks for a shell write (`echo hi > probe.txt`, in a throwaway working directory), then a 400-line
prompt cancelled after 2.5 s. UUIDs are replaced by stable placeholders (`<uuid-N>`).

| Run | Version | Transcript | Permission requests | `probe.txt` created | Cancel mid-turn |
|---|---|---|---|---|---|
| dsh, default permission mode | 0.2.0-rc.2 | [dsh-account-route](dsh-account-route.md) | 0 | **yes** | `cancelled` |
| **dsh, `DSH_PERMISSION_MODE=read-only`** | 0.2.0-rc.2 | [dsh-read-only](dsh-read-only.md) | **1 (denied)** | **no** | `cancelled` |
| Devin, `ask` mode | 3000.11.92 | [devin-ask-mode](devin-ask-mode.md) | 0 | no (declined by itself) | `cancelled` |
| Devin, `accept-edits` mode | 3000.11.92 | [devin-accept-edits-mode](devin-accept-edits-mode.md) | 0 | **yes** | `cancelled` |
| Grok, operator's own config | 1.0.63 | [grok-signed-in](grok-signed-in.md) | 0 | **yes** | `cancelled` |

What this establishes for the ADR 0043 gate:

- **Cancel works on all three binaries.**
- **No agent asked before running a shell write in its default configuration.** The ACP permission request
  never fired for dsh (default), Devin (`accept-edits`) or Grok. A policy that forbids a tool therefore cannot
  rely on the agent asking.
- **dsh can enforce the strictest policy itself.** The `acp` profile includes dsh's sandbox stack, selected by
  the `DSH_PERMISSION_MODE` environment variable (`read-only`, `workspace-write`, `danger-full-access`). With
  `read-only` dsh asked once over ACP, the probe rejected it, and the file was not created. This is the
  denied-permission proof for dsh. It needs `DSH_PERMISSION_MODE=read-only` in the child's environment.
- **Devin enforces read-only only through its `ask` mode**, which it applies by declining on its own, with no
  permission request. A profile has to send `session/set_mode ask` and check that the mode took effect.
- **Grok cannot be shown to be constrained from the client.** Its ACP `session/new` advertises only a model
  and a reasoning-effort option, no permission mode. The operator's `ui.permission_mode` was `always-approve`,
  which explains why nothing was asked. The documented `--deny` rules did not prevent the write in three
  forms tried (`--deny Bash --deny Edit`, with `(*)` patterns, lower case); a fourth, placed after the
  `agent` subcommand, was rejected by the CLI (exit code 2). Grok has no proven way to meet a read-only
  policy, so a Grok profile cannot be enabled under it.

Other signed-in findings:

- **`authMethods` is non-empty for Devin (`[devin-browser]`) and Grok (`[xai.api_key, cached_token, grok.com]`)
  while both are signed in and working.** The `AcpExecutor` check that treats a non-empty `authMethods` as
  "unauthenticated" would refuse both.
- **Signed-out Devin carries a typed signal.** The prompt fails with `-32000` and the agent sends
  `_cognition.ai/agent_stopped` with `"cause": "auth_required"` (see [devin-no-credential](devin-no-credential.md)),
  so it can be detected without decoding the error message.
- **dsh's free-model provider is not selectable in the `acp` profile.** `session/set_config_option` accepted
  only the five listed routes and rejected `["our-free-model","deepseek-v4.1-flash-free"]` with
  `-32602 unknown model option`. The `deepseek-account` route works with dsh's own stored login.
- **Grok updates itself.** With `auto_update = true` in the operator's config, launching it moved the installed
  binary from 1.0.46 to 1.0.62 and then 1.0.63 within about an hour, and `config.toml` was rewritten (three
  keys added, none removed). Pass `--no-auto-update` before the subcommand in scripted runs.

### Probe options added

`--inherit-login` (run with the operator's real `HOME`; nothing is copied), `--connect-only` (no prompt, no
quota), `--set-mode ID`, `--config ID=VALUE`, `--permission-prompt TEXT` with `--check-file NAME` (records the
permission requests and whether the file appeared), and UUID redaction.

## What the three binaries disagree on

| | dsh | Devin | Grok |
|---|---|---|---|
| `authMethods` in `initialize` | empty | `[devin-browser]` | `[grok.com]` |
| `session/new` while signed out | succeeds | succeeds | **fails** `-32000 Authentication required` (`data: "no auth method id provided"`) |
| Where the missing login shows up | the prompt, `-32603` | the prompt, `-32000`, plus an `_cognition.ai/agent_stopped` notification | `session/new` |
| `session/new` with a relative `cwd` | `-32602` | **accepted** | `-32602` |
| `mcpServers` omitted | `-32602` required | not tested | not tested |
| Resume | `session/resume` | `loadSession` | `loadSession` and `session/resume` |
| Session id | UUID | words (`road-spoon`) | not observed |

Grok's `--no-leader` is an option of `grok agent`, not of `grok agent stdio`; placed after `stdio` it exits
with code 2. Whether `grok agent stdio` without it attaches to a shared leader process was not tested.

## Findings that change OMP's client

These compare the transcripts with what `AcpExecutor` (`crates/envd/src/acp_client`) does.

1. **`mcpServers` is required in `session/new`** (dsh rejects its absence). Fixed in a separate PR.
2. **"Non-empty `authMethods` means unauthenticated" is wrong.** `AcpExecutor` returns `AuthRequired`
   from `initialize` whenever `authMethods` is non-empty. Devin and Grok both advertise a method while
   the agent is otherwise usable, so this check cannot tell a signed-in agent from a signed-out one. With
   Devin it would refuse the binary before `session/new`, which Devin accepts. ACP's `authMethods` lists
   what *can* be used, not what is *needed*.
3. **Authentication is signalled in three different places** (initialize for none, `session/new` for Grok,
   the prompt for dsh and Devin), with three different error codes. Detecting "needs login" must read
   the error, not assume one stage. Grok additionally expects an authentication step that `AcpExecutor`
   never sends (`data: "no auth method id provided"`); how that call works was not probed.
4. **Devin starts in a file-writing mode.** Its session reports `currentModeId: "accept-edits"` with modes
   `accept-edits`, `ask`, `plan` and `bypass`. A client that wants OMP's strictest policy has to change
   the mode before prompting; ACP's `session/new` has no sandbox parameter. The exact behavior of `ask`
   and `plan` was not verified. This matters for ADR 0043's per-profile policy work.
5. **Devin and Grok send many notifications `AcpExecutor` does not model** (`_cognition.ai/*`,
   `x.ai/*` capability blocks, `config_option_update`, `available_commands_update`, `turn_stats`).
   `AcpExecutor` ignores unknown notification methods, which is the safe default; the point is that a
   real turn carries much more than the mock peer ever sends.
6. Devin emitted updates for **two** session ids because the probe created two sessions (the second with
   a relative `cwd`, which Devin accepted). A client that correlates updates by session must filter.

## Not verified

- A signed-in agent's behavior. The probes used no credential. One extra check ran Devin once with a
  copy of an existing credential file in an isolated home: `authMethods` was unchanged and `session/new`
  succeeded. That does **not** show the agent treated itself as signed in (no prompt was sent, and
  `session/new` succeeds signed out), so it is not evidence for finding 2. It also should not be
  repeated: copying another application's credential file is outside ADR 0043's rule and is not part
  of this harness.
- Whether Devin's `ask` and `plan` modes actually prevent writes, and whether Grok needs `authenticate`.
- Cancel and permission behavior for every binary.
