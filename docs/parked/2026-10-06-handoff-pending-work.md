# Handoff: pending work after the 2026-10-06 session

Planning material, not wired into code. It outranks nothing in `AGENTS.md`, the ADRs or the code.
Written for the next Claude Code session (a local one on an Apple-silicon Mac is the intended
runner) so it can pick up without this session's context. Every item says whether it was
verified in code or is only a recorded decision or intention.

## What this session landed on `omp2` (for orientation)

- #166 Anthropic cache breakpoints (`budget_cache_breakpoints`, at most 4 markers).
- #167 project cfg no longer carries user authority (`PROJECT` flag on convars).
- #168 resume distinguishes started from not-started tool calls; #174 retry asks for confirmation
  when a started tail may have had effects (`RetryConfirmation::EffectsUnknown`).
- #169, #170, #175 test isolation and deflakes (git config isolation, walker temp dir, idle TTL,
  envd relay enable).
- #172, #173, #176 session account pins, the typed `account-pin` notice, account names.
- #177, #179 parked audit notes; status notes on `docs/py/05-hooks.md` and `07-ui.md`.
- #178 project MCP config is off by default (`sv_mcp_enable_project_config`, user-level only);
  project-authored MCP values are literal (no `!command`, no environment lookup).
- #180 one contained reader for project files (`omp_core::project_file`).
- #181 `Yolo` is honoured only inside an active sandbox unless the user chose it explicitly; the
  sandbox is on by default with `workspace-write`.

## 1. Security: project-sourced inputs

Source of truth: `docs/parked/2026-10-06-project-input-trust-audit.md` (findings F1-F14).

Open, in the order the owner approved the sequence:

1. **Trust model (PR2 to PR8 of the audit).** Decision recorded: trust is per workspace, bound to a
   digest, which matches `omp-ext` (`crates/ext/src/trust.rs`: `Grant` keyed by workspace with
   `exact` or `subtree` scope, capability digest, publisher, tier, duration). Plan:
   `WorkspaceTrust::{Untrusted, Trusted}`, `grants.toml` `[[workspace_trust]]`, a re-ask when the
   `inputs_digest` changes, driver plumbing, envd gating, `omp trust` and `--trust-workspace`, a
   TUI overlay (needs real-PTY proof through `.omp/tools/tui.ts`) and an approval floor.
   Subtree trust, if added, needs three rules: an explicit `omp trust --subtree`, refusing `/` and
   `$HOME`, and a per-repo digest re-ask. Not started; design only.
2. **Explicit MCP approval per workspace.** #178 is a single user-level switch. The owner asked
   that project MCP stay blocked until the user approves it explicitly, which the trust model above
   should provide. Also: `omp config mcp add --scope project` and `/mcp add` write servers that do
   not load until the user opts in and print no hint; the literal-value notice goes only to
   `tracing`, not to the TUI.
3. **F2** native `.omp/extensions` with self-declared grants. **F3** LSP and DAP project config
   reading is now contained (#180) but launching repo-local binaries (`node_modules/.bin`,
   `.venv/bin`, `bin`) is not gated.
4. **Not covered by #180:** `HostStore::upsert` and `persist_hosts` (hosts.toml writes, `ssh add`)
   still follow symlinks; the `.cfg.lock` sibling is opened with `create(true)` and follows links;
   user-level `SYSTEM.md` and user cfg were not hardened; the Windows path is untested
   (`O_NOFOLLOW` is unix-only). A refused project `secrets.yml` is a hard error by design (skipping
   would drop masking rules); flip only if the owner asks.
5. `cl_disabled_extensions` as a `PROJECT`-flagged convar (proposal, not decided in code).

## 2. Yolo and sandbox follow-ups (after #181)

Verified in the PR; list is what a reviewer should still check on a Mac:

- Seatbelt behaviour on real macOS: the sandbox probe, the denial-and-rerun flow of ADR 0028, and
  the posture notice (`approval-posture`). Linux CI has no `bwrap`, so Linux only exercises the
  "unavailable" path.
- `bash@2` declares no effects, so its tier was `read`; without a sandbox it now resolves to the
  `exec` tier, which means `AlwaysAsk` also prompts for bash when no sandbox confines it.
- Headless `print` denies an approval-requiring call immediately (documented in
  `crates/app/src/print_mode.rs`); with no sandbox, bash therefore needs
  `--approval-mode yolo` (explicit, honoured with a notice) or `tools.approval.bash allow`.
- Check that the `/security` panel shows the effective mode, not only the configured one.

## 3. Accounts

- Slice 2: `--api-key` precedence over stored accounts (not started).
- Slice 3 (model-pool preference): probably not applicable, omp2 has no model-pool concept
  (unverified).
- Slice 4: project-level pins are a design proposal only; do not implement without the trust model.

## 4. Inference cost and cache (decisions pending)

Source: `docs/parked/2026-10-06-omp2-token-efficiency-audit.md`.

- The default `ai_cache_retention auto` emits no cache markers, so the default configuration
  caches nothing automatically. Decision pending: should `auto` place markers?
- Extract the Volatile band (date, cwd, mounts) from the system prefix so it cannot invalidate a
  stable prefix; separate PR.
- Measure cache hits under the default configuration (needs an Anthropic key and disk).
- Optional: a dedicated TUI message for the `account-pin` notice.

## 5. Older backlog items from the task list

- Plan items 1.4, 2.1 and 2.2 (catalog, collab CLI, dead args and commands).
- Follow-ups from the ADR amendments (ADR 0006 per-call admission and nested-call hooks in eval,
  ADR 0007 gitignored files not copied, ADR 0024 prompt prefix still moved by Goal, prewalk and
  mode prompts, ADR 0034 geometry and `cl_resize_policy`).
- Tool hooks on nested `tool.<name>()` calls from eval, plus per-call approval.
- An `editor_base` test that fails on Linux when run as root (not investigated).
- Paired omp2 experiment: current tools against semantic helpers, token cost with a statistical
  test.
- `docs/adr/STATUS.md` was verified on 2026-10-04 and amended since; refresh it.

## 6. Housekeeping the owner must do or approve

- Delete stale remote branches: `claude/mbx-trial`, `claude/macos-hosted-trial`,
  `claude/hosted-macos-installable`, `claude/hosted-macos-trial2`, `claude/p7-hosted-macos-trial`,
  `claude/hosted-macos-test-fixes`. Many other merged `claude/*` branches also remain.
- Decide `vars.MACOS_RUNNER` (self-hosted Apple-silicon or the hosted `macos-15` default; hosted
  runs take over an hour for the workspace job).
- Remove the local git remote `aifam` when it is no longer needed.

## 7. Documentation known to be wrong or thin

- `docs/py/05-hooks.md` and `docs/py/07-ui.md` carry a status note (#179) because their
  `AgentEvent`, `AgentPhase`, `EventBus` and line references predate the Kernel; rewriting them
  against `KernelEvent` is still open.
- `AGENTS.md` mentions `tokio::watch` for priority signals in actor loops; `omp-agent` itself uses
  flume mailboxes and `CancellationToken` only.
- No test enforces crate layering except the `omp-agent` dependency policy in
  `scripts/check-spec.rs`; `omp-app` and `omp-chat` declare direct dependencies on host crates
  (`omp-tools`, `omp-shell`, `omp-walker`, `omp-py`, `omp-agent`) that were not checked against
  the rules in `AGENTS.md`.
- The onboarding guide (a Claude Doc, "Guía de Onboarding OMP2") was written from this code and has
  open questions at the end of its checklist: contacts, URLs, ceremonies, credentials for
  development, who publishes binaries and npm packages, the canonical repository and branch.

## How to work (rules that bit this session)

- Use `just`, never raw cargo; tests run under nextest plus `cargo test --doc`
  (`just test-pkg <crate>`). Run `just setup-python` once per checkout.
- A full workspace test target is large (about 12 GB debug). Build one crate at a time and keep
  free disk above 4 GB; the old cloud container ran out repeatedly.
- CI is the gate and is the only place the hosted macOS job runs. Merge only with a normal merge
  commit, with `expectedHeadSha`, when every `ci.yml` check is green on that exact head; rerun a
  failed job at most once, after a comment saying why it is not the PR's.
- Never skip, disable or quarantine a test to get green.
- Do not guess: mark what was not run as not run.
