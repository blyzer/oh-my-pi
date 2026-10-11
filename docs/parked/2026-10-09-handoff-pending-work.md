# Handoff: pending work after the 2026-10-07..09 sessions

Continues [`2026-10-06-handoff-pending-work.md`](2026-10-06-handoff-pending-work.md). Read that one
for the items this note marks "carried over". Remote build and test execution has its own note:
[`2026-10-09-remote-execution-next-steps.md`](2026-10-09-remote-execution-next-steps.md).

Updated 2026-10-11. "Open on 2026-10-11" is the current list. The sections after it keep their
2026-10-09 text, with each item's later status added.

## Open on 2026-10-11

Everything that was in flight on 2026-10-09 has landed (see the next two sections). Still open:

1. **#251 `fix/cli-login-callback`**, in CI: the CLI half of the login-flow item.
   - `omp auth login` now finishes when the browser callback wins and keeps waiting on an empty
     Enter.
   - It cancels on Ctrl-C.
   - It opens the browser at the `/launch` URL and prints OSC 8 links.
   - In `omp-ai`, the PKCE flow keeps waiting when a client confirms the browser prompt; before, it
     failed with `UnexpectedInput`.
   - Proved on a real PTY against the 2026-10-08 binary (see the PR body).
2. **Chat browser prompt.** `auth_input` in `crates/app/src/chat_services/accounts.rs` turns every
   confirmation answer other than empty, `y` or `yes` into `Cancel`.
   - So a callback URL pasted at the chat's "Complete authorization in the opened browser" prompt
     cancels the login.
   - The CLI takes it since #251. Not fixed in the chat.
3. **Owner decisions.**
   - Should the model see the `approval-posture` notice? Typed notices are never projected to the
     model.
   - Unknown-scheme reads, served by the RPC host, are classified as non-fetch (ADR 0028 read fetch
     amendment). Is that right?
   - ADR 0043 (native provider CLI integration) and ADR 0044 (presentation surfaces and agent
     boundaries) are proposed and wait for review.
   - ADR 0040 records option D (#244), so Slice 1 no longer waits on runner recycling. Slice 1 has
     not started.
4. **Posture notice.** Several test harnesses built kernels without `install_tool_authority`. It is
   unverified whether that still holds after #229.
5. **Disabled-network diag gaps**, documented in ADR 0028. These stay silent:
   - a pipeline whose final stage succeeds;
   - an in-process builtin;
   - a URL inside a longer argument;
   - a raw address.
6. **BuildBuddy CI job** (`.github/workflows/buildbuddy-remote.yml`, #240-#247).
   - It failed on every `omp2` push until #250. Under `pipefail`, `scripts/remote-verify.sh` exited
     silently when `origin/HEAD` was missing, which is always the case after `actions/checkout`.
   - The first run after #250 (`506b7c143e`) was still running when this was written. Check that it
     passes.
7. **Carried-over backlog.** The owner deferred it on 2026-10-11. The items under "Carried over"
   below are unchanged.

## What landed on `omp2` 2026-10-09..11 (#223-#250)

| Theme | PRs |
|---|---|
| The 2026-10-09 in-flight list | #224 editor-base flake, #225 effects slice 8 (`grep`, `glob` and `ast_grep` remote roots), #228 disabled-network diag review, #229 attached posture notice (with `ServerHello.sandbox_state`, `SCHEMA_REV` 27, and a report derived from the journal), #231 credential-kind direction, #235 logout purges the account's durable state, #238 chat login survives the callback |
| Follow-ups found while verifying | #234 external eval requires Python 3.10, #239 `InstrumentedTool` forwards `projection`, `authorize_visibility` and `stream_match_text`, #248 visibility for projected results, detached terminals and large extension outcomes |
| Remote execution | #226 `just remote*` recipes and trial record; #240-#243 and #245-#247 BuildBuddy remote Linux verification on each `omp2` push; #244 ADR 0040 option D and the recycling probe; #250 `remote-verify.sh` without `origin/HEAD` |
| TSP (ADR 0041) | #223 phase 0, #230 phase 1, #232 phase 2, #233 phase 4 |
| Research and docs | #227 this handoff, #236 Apple PCC, #237 native provider CLI integration (ADR 0043), #249 presentation surfaces and agent boundaries (ADR 0044) |

## What landed on `omp2` (#184-#222)

| Theme | PRs |
|---|---|
| Sandbox and network on real Seatbelt | #184 active-sandbox test, #185 Seatbelt scoped egress through the loopback broker, #186 host-mode reads follow symlinks, #187 default network `scoped`, #188 network diag to the model, #200 broker survives accept errors, #207 Linux fsync and inode reuse |
| Approval relay (daemon) | #189 env/v1 relay frames, #190 relay to the issuing connection, #192 attached sessions answer through the kernel route, #193 P12, #194 P7 overlay, #195 session-scoped network amendments |
| Typed confinement | #197 `Confinement { Host, ExecSandbox }` replaces the `bash` name check |
| Workspace trust, PR2 | #191 trust rows and locked grant writer, #196 gated-input inventory and digest |
| Effects honesty, slices 1-7 and 9 | #199 no `ssh://?op=exec` in read and grep, #202 memory `reflect` declares inference, #203 fetch effect class and tier, #205 fail closed on undeclared effects, #206 argument-scoped invocation effects, #210 fetch asked once per host per session, #219 `read@3` declares its fetches per target |
| Live-verification fixes (2026-10-08) | #209 tui tool stops `omp chat` cleanly, #211 envd-native tool results reach the model (they were empty strings), #213 + #215 a project daemon is never reused under another sandbox policy, #214 + #222 credential kinds and typed credential errors, #220 legible diag for quiet network failures under `disabled` |
| Remote execution | #212 ADR 0040 (proposed); #226 recipes and trial record (open) |
| Other | #198 CI apt fail-fast, #201 `dyn` feed kept alive, #204 + #208 parked-doc updates, #216 RPC v1 parity commands, #217 + #218 + #221 ADR 0041, #223 TSP phase 0 |

## In flight on 2026-10-09 (all landed by 2026-10-11)

Each fix lives on its own branch, with a worktree `~/repositories/omp2-wt-<name>` and its own
`target/`.

| Branch / PR | What | State |
|---|---|---|
| #224 `fix/editor-base-disk-change-flake` | The editor-base merge test waits for the docserver to adopt the disk change. Root cause: an external `fs::write` reaches the authority only through the async file watcher; failed 3-6% under load, 0 of 3500 after the fix | merged |
| #225 `fix/grep-glob-remote-reads` | Effects slice 8: `grep`, `glob` and `ast_grep` declare remote reads per root, reusing `read::classify_target` | merged 2026-10-09 |
| #226 `feat/remote-verify-recipe` | `just remote*` recipes, `docs/audits/remote-verification-trial.md`, `.gitignore` `/vendor` | merged 2026-10-09 |
| `fix/attached-posture-notice` | The `approval-posture` notice in attached compositions: the env tool executor and the kernel admission share one once-only report through `install_tool_authority` | merged as #229 (2026-10-10). Its review's rewind, switch and probe findings are fixed there: the report is derived from the journal, and `ServerHello` reports the daemon's sandbox state |
| `fix/disabled-network-diag-review` | Follow-up to #220 from its third review: judge the program a URL was handed to in the sandbox's view; docs say exactly which commands stay silent | merged as #228 (2026-10-09) |
| `fix/credential-kind-direction` | Follow-up to #222: the startup repair re-stores static secrets in both directions from the current catalog/models.toml, so a config change rewrites stored credentials (major); plus minors (repair stops at the first bad row; `StorageLocked` remedy assumes `auto`; importer kind from a private label enum; KindMismatch advice; untested "composition never fails"; BEARER→API_KEY normalisation of explicit writes; STATUS notes) | merged as #231 (2026-10-10) |
| `fix/logout-durable-purge` | `auth logout` must remove the account state durably: `AccountStateStore::purge_account` is only called from a test, so a logged-out account reloads as Active. The v1 importer names the principal `email:<addr>` where native OAuth uses the raw email, so they never deduplicate. `omp token` picks `accounts[0]` even without a credential row. `dry-balance --json` keys collide | merged as #235 (2026-10-10) |
| `fix/login-flow-completion` | Chat and CLI login wait only for pasted input, so when the loopback callback wins the flow reports "ended without a result" although the account was saved. Fix: select on the session events, withdraw the paste prompt, keep waiting on an empty Enter, print the short `/launch` URL with an OSC 8 link and open the browser from the CLI. Needs real-PTY proof | chat merged as #238 (2026-10-10); CLI in #251 |

The briefs for the last two are above; the evidence (file and line at `8869704535`) is in ADR 0018's
amendment and in the review of #214.

## Follow-ups found while verifying (status on 2026-10-11)

1. **External Python version.** `discover_external_python` (`crates/envd/src/eval/process.rs`)
   accepts any `python3`, but `external_runner.py` uses 3.9/3.10 syntax, so a host with Python 3.8
   fails `eval` opaquely (seen on the Ubuntu 20.04 runner). Require 3.10 and say why. **Done in
   #234.**
2. **`InstrumentedTool` still drops** `Tool::stream_match_text` (stream rules for edit, write,
   apply_patch), `Tool::projection` and `Tool::authorize_visibility` (`crates/envd/src/tools.rs`).
   #219 fixed the effects hooks only. **Done in #239.**
3. **Visibility for environment-run tools.** The `DoneProjected` path stages an empty visibility
   list, so `authorize_visibility` never runs for them (open in ADR 0009). **Done in #248.**
4. **Detached terminals.** The driver's `EnvToolExecutor` cannot parse a `ToolTerminal::Detached`
   verdict and treats it as an invalid outcome. **Done in #248.**
5. **Large extension results.** A worker result over `CONTROL_RESULT_INLINE_BYTES` drops its parts
   in `worker.rs` `control_completion`, so it still reaches the model empty. **Done in #248.**
6. **Posture notice (review minors of the in-flight branch).** The rewind and the `ServerHello`
   field were done in #229. The harness item and the owner decision are open ("Open on 2026-10-11",
   items 3 and 4).
   - `PendingPosture::rewound` does nothing, so a tool-tail retry of the first batch loses the
     notice.
   - Several harnesses build kernels without `install_tool_authority`.
   - The notice reports the client's probed sandbox, not the daemon's (needs a `ServerHello`
     field).
   - **Owner decision:** typed notices are never projected to the model. Should the model see
     `approval-posture`?
7. **Disabled-network diag gaps.** These stay silent: a pipeline whose final stage succeeds, an
   in-process builtin, a URL inside a longer argument, a raw address. All are documented in ADR 0028.
8. **Owner question.** Unknown-scheme reads (served by the RPC host) are classified as non-fetch
   (ADR 0028 read fetch amendment).
9. **CI.** The Linux workspace test suite never runs in CI (macOS only), contrary to `AGENTS.md`.
   `just remote-test-pkg` can run it on Linux.
   - Since #240-#247, a push-only workflow runs the Linux Cargo verification on BuildBuddy after
     each `omp2` push. Pull requests still get no Linux workspace tests.
   - The workflow failed silently until #250 ("Open on 2026-10-11", item 6).
10. **Chat Ctrl-C.** Not a bug: quitting `omp chat` needs a second Ctrl-C within 500 ms (pi parity).
    The tui tool sends `C-c C-c` since #209.

## Carried over from the 2026-10-06 handoff (status on 2026-10-09; unchanged and deferred on 2026-10-11)

- **§1.1 Trust model.** PR3 to PR8 are still not started: driver plumbing, envd gating,
  `omp trust`, the overlay and the approval floor. Nothing outside `omp-ext` reads a trust decision.
- **§1.2** Explicit per-workspace MCP approval: open.
- **§1.3 F11, F2, F3.**
  - F11 (`omp adw run`: code phases unsandboxed, full environment, operator cfg ignored, Ctrl-C not
    wired) is open; ADR 0040 Slice 1 PR 1d addresses part of it.
  - F2 and F3 are open.
- **§1.4** `hosts.toml` writes and the `.cfg.lock` sibling still follow symlinks: open.
- **§2 Sandbox follow-ups.**
  - Verified live on Seatbelt on 2026-10-08: the write denial and the rerun prompt, `scoped`
    network with session-scoped amendments, `/security`, `print` denial without a sandbox, and F11.
  - The posture notice was missing in attached mode; fixed in #229.
  - Still open: in-process builtins (`cat`, `head`, `grep`) ignore `read_deny`.
- **§3 Accounts:** `--api-key` precedence over stored accounts is not started.
- **§4 Cache decisions:** still pending.
- **§5 Older backlog:** unchanged. The "`editor_base` test fails on Linux as root" item may be the
  watcher race fixed in #224; unverified.
- **§6 Housekeeping (owner):** unchanged. New items:
  - remove the trial branch `rbe-trial` and the worktree `omp2-wt-bb`;
  - revoke the BuildBuddy trial key when it is no longer needed;
  - the worktrees `omp2-wt-daemon` and `omp2-wt-parts` were removed on 2026-10-11.
- **§7 Docs:** unchanged.

## How to work on this machine (lessons from these sessions)

- **One worktree per task.**
  - Give it its own `target/` as an APFS clone of a warm one: `cp -c -R <warm>/target <wt>/target`
    (instant, no extra disk at first).
  - Pin `PYO3_CONFIG_FILE` to the warm tree's exact path string; otherwise pyo3 and everything above
    it rebuild.
  - Symlink `vendor` to the main checkout's. `.gitignore` ignores it since #226.
- **Never share one `CARGO_TARGET_DIR` between worktrees.** Cargo names workspace artifacts relative
  to each workspace root, so the trees overwrite each other's test binaries mid-run.
- **One cargo command at a time on the 16 GB Mac.** Wrap every cargo/just command in
  `lockf -k /tmp/claude-501/omp2-cargo.lock …`.
  - Two envd links at once drive the machine into swap.
  - A Bash call has a 600 s limit, and a lock wait counts against it.
- **Never `git stash`.** `refs/stash` is shared by every worktree: one agent popped another's stash.
  Commit a WIP commit instead.
- **Docs conflicts are routine.** Parallel PRs all touch the header of `docs/adr/STATUS.md`, the ADR
  0028 status line and its amendment list.
  - Keep upstream's text and add each side's sentence or amendment as its own unit.
  - Never let a line-by-line merge interleave two amendment sections.
- **A PR that conflicts with `omp2` gets no CI at all.** Check
  `git merge-tree --write-tree origin/omp2 HEAD` before waiting.
  - A stacked PR whose base merged must be retargeted to `omp2` (and closed and reopened if CI
    does not start).
- **Merge** only with a normal merge commit,
  `--match-head-commit "$(gh pr view N --json headRefOid -q .headRefOid)"`, after every `ci.yml`
  check is green on that head. Docs-only PRs show most jobs skipped by the path filter, which is
  expected.
- **Live verification:**
  - Trust the session journal, never the model's summary: `omp render <session.oms>`.
  - To see what the model receives, put a logging proxy in front of the model endpoint, using a
    scratch copy of `~/.o2` via `OMP_CONFIG_DIR`.
  - Kill the probe repo's `omp envd` daemon between config variants.
  - Drive the chat on a PTY through `OMP_TTY`.
  - Non-TTY runs need `OMP_LLM_KEY_SOURCE=local-file`.
  - The model route that worked: `easycliproxy/claude-sonnet-4-6` (local proxy at 127.0.0.1:8317).
- **Disk.** After the 2026-10-09 cleanup about 130 GB was free. Remove finished worktrees and their
  `target/` promptly. The Podman VM was stopped to free memory (`podman machine start` restores it).
- **Login probes on a PTY** (from #251):
  - Give `omp auth login` its own `HOME` and `OMP_LLM_KEY_SOURCE=local-file`. With only
    `OMP_DATA_DIR` set, the first run imports the real v1 install into the scratch data dir.
  - Put a stub `open` first on `PATH`: it records the URL the CLI opens and keeps the browser shut.
  - Simulate the browser with a GET to the printed `redirect_uri`, with the `state` from the full
    URL.
  - Drain the PTY master continuously. omp blocks on a full PTY and looks hung.
  - macOS revokes the TTY when the session leader exits. Run omp under
    `sh -c 'trap true INT; omp …; stty -a'` to read the echo state after it exits.
- **Other sessions share this repo.** omp and other agents work in their own worktrees (for
  example under `/private/tmp`) and push branches and PRs from the same account. Do not remove
  worktrees you did not create, and read a PR's head from `gh` before acting on it.

## Local state on 2026-10-11 (this machine, not in git)

The owner decides what happens to each of these:

- **Main checkout.** It is on `research/apple-pcc-integration`, merged as #236, with work that is
  not on `omp2`:
  - **Uncommitted edits.**
    - `docs/parked/2026-10-09-remote-execution-next-steps.md` records the 2026-10-10 BuildBuddy
      verification runs with their invocation links: three `remote-test-pkg omp-envd` runs, plus
      the workspace `just check` and `just test` on the runner.
    - `docs/adr/0040-remote-build-and-test-execution.md` holds a second draft of the option D
      amendment, which differs from the one #244 merged.
  - **Untracked note:** `docs/parked/2026-10-10-doneprojected-visibility-spike.md`.
- **`omp2-wt-auth`** holds uncommitted logout work that #235 superseded.
- **`stash@{0}`** is an old change to `AGENTS.md`, stashed on `f6f16df57a`.
