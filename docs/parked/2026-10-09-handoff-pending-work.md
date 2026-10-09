# Handoff: pending work after the 2026-10-07..09 sessions

Continues [`2026-10-06-handoff-pending-work.md`](2026-10-06-handoff-pending-work.md). Read that one
for the items this note marks "carried over". Remote build and test execution has its own note:
[`2026-10-09-remote-execution-next-steps.md`](2026-10-09-remote-execution-next-steps.md).

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

## In flight when this note was written

Each fix lives on its own branch, with a worktree `~/repositories/omp2-wt-<name>` and its own
`target/`.

| Branch / PR | What | State |
|---|---|---|
| #224 `fix/editor-base-disk-change-flake` | The editor-base merge test waits for the docserver to adopt the disk change. Root cause: an external `fs::write` reaches the authority only through the async file watcher; failed 3-6% under load, 0 of 3500 after the fix | merged |
| #225 `fix/grep-glob-remote-reads` | Effects slice 8: `grep`, `glob` and `ast_grep` declare remote reads per root, reusing `read::classify_target` | base is the merged `fix/read-declares-fetch` and it conflicts: retarget to `omp2`, rebase, re-run CI |
| #226 `feat/remote-verify-recipe` | `just remote*` recipes, `docs/audits/remote-verification-trial.md`, `.gitignore` `/vendor` | CI pending, then merge |
| `fix/attached-posture-notice` | The `approval-posture` notice in attached compositions: the env tool executor and the kernel admission share one once-only report through `install_tool_authority` | implemented (2f679bbb06), reviewed (approve, 5 minors below), publishing |
| `fix/disabled-network-diag-review` | Follow-up to #220 from its third review: judge the program a URL was handed to in the sandbox's view; docs say exactly which commands stay silent | implemented (ad72a009cc), publishing |
| `fix/credential-kind-direction` | Follow-up to #222: the startup repair re-stores static secrets in both directions from the current catalog/models.toml, so a config change rewrites stored credentials (major); plus minors (repair stops at the first bad row; `StorageLocked` remedy assumes `auto`; importer kind from a private label enum; KindMismatch advice; untested "composition never fails"; BEARER→API_KEY normalisation of explicit writes; STATUS notes) | implementing |
| `fix/logout-durable-purge` | `auth logout` must remove the account state durably: `AccountStateStore::purge_account` is only called from a test, so a logged-out account reloads as Active. The v1 importer names the principal `email:<addr>` where native OAuth uses the raw email, so they never deduplicate. `omp token` picks `accounts[0]` even without a credential row. `dry-balance --json` keys collide | not started |
| `fix/login-flow-completion` | Chat and CLI login wait only for pasted input, so when the loopback callback wins the flow reports "ended without a result" although the account was saved. Fix: select on the session events, withdraw the paste prompt, keep waiting on an empty Enter, print the short `/launch` URL with an OSC 8 link and open the browser from the CLI. Needs real-PTY proof | not started |

The briefs for the last two are above; the evidence (file and line at `8869704535`) is in ADR 0018's
amendment and in the review of #214.

## New follow-ups found while verifying (not yet scheduled)

1. **External Python version.** `discover_external_python` (`crates/envd/src/eval/process.rs`)
   accepts any `python3`, but `external_runner.py` uses 3.9/3.10 syntax, so a host with Python 3.8
   fails `eval` opaquely (seen on the Ubuntu 20.04 runner). Require 3.10 and say why.
2. **`InstrumentedTool` still drops** `Tool::stream_match_text` (stream rules for edit, write,
   apply_patch), `Tool::projection` and `Tool::authorize_visibility` (`crates/envd/src/tools.rs`).
   #219 fixed the effects hooks only.
3. **Visibility for environment-run tools.** The `DoneProjected` path stages an empty visibility
   list, so `authorize_visibility` never runs for them (open in ADR 0009).
4. **Detached terminals.** The driver's `EnvToolExecutor` cannot parse a `ToolTerminal::Detached`
   verdict and treats it as an invalid outcome.
5. **Large extension results.** A worker result over `CONTROL_RESULT_INLINE_BYTES` drops its parts
   in `worker.rs` `control_completion`, so it still reaches the model empty.
6. **Posture notice (review minors of the in-flight branch).**
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
10. **Chat Ctrl-C.** Not a bug: quitting `omp chat` needs a second Ctrl-C within 500 ms (pi parity).
    The tui tool sends `C-c C-c` since #209.

## Carried over from the 2026-10-06 handoff (status on 2026-10-09)

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
  - The posture notice was missing in attached mode; its fix is in flight.
  - Still open: in-process builtins (`cat`, `head`, `grep`) ignore `read_deny`.
- **§3 Accounts:** `--api-key` precedence over stored accounts is not started.
- **§4 Cache decisions:** still pending.
- **§5 Older backlog:** unchanged. The "`editor_base` test fails on Linux as root" item may be the
  watcher race fixed in #224; unverified.
- **§6 Housekeeping (owner):** unchanged. New items:
  - remove the trial branch `rbe-trial` and the worktree `omp2-wt-bb`;
  - revoke the BuildBuddy trial key when it is no longer needed.
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
