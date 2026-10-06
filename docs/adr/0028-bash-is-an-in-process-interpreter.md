# 0028. `Bash` is a policy-aware in-process interpreter

Status: accepted
Date: 2026-09-02
Area: tools

## Context

A `Bash` tool that spawns `/bin/bash -c "$cmd"` gives the harness exactly one decision point: the
whole string, before anything runs. The post's evidence for why that point is useless is a real
Claude-generated command:

```sh
INC="…/10.0.22621.0"; declare -A R
for d in um shared ucrt; do while IFS= read -r f; do b="${f##*/}"; R["${b,,}"]="$f"; done \
  < <(find "$INC/$d" -maxdepth 1 -type f -name "*.[hH]"); done
n=0
while IFS= read -r ref; do case "$ref" in */*) continue;; esac; r="${R[${ref,,}]:-}"; \
  [ -n "$r" ] || continue; rd="${r%/*}"; rn="${r##*/}"; \
  if [ "$ref" != "$rn" ] && [ ! -e "$rd/$ref" ]; then ln -s "$rn" "$rd/$ref"; n=$((n+1)); fi; \
done < <(grep -rhoiE "#[[:space:]]*include[[:space:]]*<[^>]+>" "$INC/um" "$INC/shared" "$INC/ucrt" \
  | sed -E "s/.*<([^>]+)>.*/\1/" | sort -u)
```

Nobody reads this before approving it. Everything in it is read-only except one `ln -s`.
Anthropic's own research points the same way: an "auto mode" where another model reads the
command outperforms human approval by a wide margin. Approval at the string boundary is theatre.

Three further costs of shelling out: models reach for `grep` and every `AGENTS.md` spends context
begging for `rg`; Windows needs WSL or Git Bash; and every call starts a fresh process, so
variables, exit codes, and `$!` do not survive between calls.

## Decision

1. `Bash` MUST be a complete bash parser and interpreter with a full coreutils set, running
   in-process. It NEVER execs `/bin/bash` and NEVER resolves commands through `$PATH` as a first
   resort. External binaries run only when no builtin owns the name.
2. Muscle memory is preserved, not corrected: `grep` is intercepted and routed to the ripgrep
   engine; `find`, `cat`, `sed`, `sort`, `ln`, … are builtins. The prompt NEVER carries
   "use rg instead of grep" guidance.
3. The console is stateful across calls: cwd, exports, variables, exit codes, `$!`, background
   jobs (0010).
4. Approval follows enforcement; it is not predicted before execution. When the exec sandbox is
   enabled (`sv_sandbox_mode` is `read-only` or `workspace-write`; the default is `workspace-write`, see the 2026-10-06 amendment), policy
   is enforced as the command runs: the interpreter's in-process path policy for redirections and
   builtins, the OS sandbox for spawned processes, and, in `scoped` network mode, an egress broker
   for network. A denial
   ends the attempt and is classified as one typed fact: a read path, a write path, or a network
   host and port. The host then asks a human once, for exactly that fact, with scope `once`
   (`ApprovedSandboxAmendment::{Path, Network}`; fail-closed on timeout or when no approval route
   is bound). If approved, the same command reruns once from its start with the sandbox amended by
   that single scope; a second denial is final. Commands whose accesses stay within policy run
   without a prompt. Policy comes from the `sv_sandbox_*` convars (0006, 0012), so a path already
   writable prompts for nothing.
5. The unit of approval is the denied fact: a path with its access kind, or a network host and
   port. It is NEVER the shell string. Capability-level units ("May I use Git to push?", `ln`) are
   not part of the decision in force; the `bash` tool declares no effects of its own, so the
   tool-level approval tier does not prompt for it, though a per-tool override can. The harness is
   a fact approver at the sandbox boundary, not the TSA for `Bash`.

## Consequences

- Runtime policy from 0006 becomes enforceable inside shell commands: a denial surfaces as one
  typed path or network fact, not as an opaque string.
- Platform neutrality nearly free: most invocations execute in-process on Windows identically.
- Interception is a general mechanism — `dyn` (0025), `grep` → ripgrep, and tool-recommendation
  guidance all sit on the same parsed command stream.
- Prohibited: `/bin/bash -c`, `$PATH`-first resolution, string-level allow/deny regexes as the
  security boundary, prompt text steering the model away from standard command names.
- Cost accepted: a bash-compatible interpreter and coreutils port is a large, permanent
  engineering surface; incompatibilities with GNU/BSD edge behaviour are the harness's bugs.
- Cost accepted: approval comes after a denial and the command reruns from its start, so side
  effects that completed before the denial can repeat. Whether the rerun is safe for a given
  command is not checked. With the sandbox off, no denial-and-rerun prompt exists, and under the 2026-10-06 amendment the shell itself then prompts under `write`.

## Amendment (2026-10-04)

The owner decided to bring this record in line with the code. The decision originally said approval
happens just in time at the capability boundary as interpretation reaches it, with the capability
(`git push`, `ln`, a network request) as the unit. The implemented model is different: the exec
sandbox enforces policy while the command runs, a denial is classified as a path or network fact,
the human is asked once about exactly that fact, and the command reruns once with the amended
sandbox. The owner kept the implemented model and amended the decision, because the denied fact is
what the sandbox can state precisely. Predicting capabilities before execution is no longer
claimed. The earlier gap note 'no distinct network-request approval' was stale and is removed:
network amendments exist and are part of this model.

## Amendment (2026-10-06)

The owner decided that the shipped approval posture `yolo` (never ask) holds only while a native sandbox
confines the commands the agent runs. This is a deliberate change from omp 1.x, where `yolo` was
unconditional.

1. `sv_sandbox_mode` defaults to `workspace-write`. The default approval mode stays `yolo`, so a fresh
   install runs unprompted inside the sandbox.
2. The effective approval mode is one pure function of the configured mode, who configured it, and the
   sandbox state (`effective_approval_mode`, `crates/envd/src/admission.rs`). The state is `active` (a
   sandbox was actually constructed), `off` (mode `off`), or `unavailable` (requested but not
   constructible on this host); the convar alone never counts. A defaulted `yolo` without an active
   sandbox becomes `write`. An explicit `yolo` (`--approval-mode yolo`, `--yolo`, `--auto-approve`, or a
   user layer of `sv_tools_approval_mode`, read from the `omp-con` layers by `Ctx::is_user_set`) is
   respected and runs unconfined; headless mode's denial message still points at it.
3. A shipped-default sandbox on a platform that cannot construct it (no native backend, or the backend
   fails its live probe) does not stop commands from running; it removes the confinement. Commands then
   run unsandboxed under `write`. A sandbox the user asked for (any user-set `sv_sandbox_*` convar) that
   cannot be constructed is a hard error, as before, and so is a refused policy.
4. The shell is process authority when nothing confines it: with no active sandbox, `bash` (which
   declares no effects) resolves to the `exec` tier, so `write` and `always-ask` prompt for it. With an
   active sandbox it stays `read` and the denial-and-rerun flow above is unchanged. A per-tool
   `sv_tools_approval` override stays authoritative in every case.
5. A `yolo` that no sandbox confines is reported once per session as a typed notice, `<notice kind=warn
   name=approval-posture>`, whose data carries the configured mode, who configured it, the effective
   mode, and the sandbox state with its cause: downgraded to `write` when it was the default, respected
   but unconfined when the user asked for it. `/security` shows the effective mode beside the configured
   one.

## Status in omp

**Status: Implemented.** Parser, interpreter and coreutils run in process with persistent state, and approval is the sandbox-denial-and-rerun model in the amended decision. Limits: the denial-and-rerun prompt exists only while a sandbox is constructed, and a rerun can repeat side effects. The default `yolo` holds only inside an active sandbox, an explicit one is respected (2026-10-06 amendment). (Verified 2026-10-06 against `omp2` at `f2ca37d533`, plus the changes of that amendment.)

- In-process shell: `crates/shell` (parser/runtime) and `crates/shell-builtins` (about 80 builtins including `grep` and `rg` on the ripgrep libraries, `find`, `sed`, `sort`, `ln`, `jq`); persistent cwd and exports through `crates/envd/src/exec.rs`.
- Enforcement: `ExecSandbox` and its per-attempt wrapper in `crates/envd/src/exec_sandbox.rs` implement the shell's `PathPolicy` and `SpawnWrapper`; `exec.rs` installs both on each run (`set_path_policy`, `set_spawn_wrapper`). The sandbox is `workspace-write` by default (`SV_SANDBOX_MODE`, `crates/envd/src/exec_settings/sandbox.rs`), and `SandboxState::probe` reports whether it was constructed; network is `disabled` by default (`SV_SANDBOX_NETWORK_MODE`), and the scoped egress broker (`crates/envd/src/sandbox_proxy.rs`) is what produces a typed network fact.
- Approval: `classify_sandbox_denial` in `crates/envd/src/exec.rs` types the denial (`SandboxDenialFact::{ReadPath, WritePath, Network, Unknown}`); `approve_sandbox_amendment` asks one `sandbox_amendment` ticket (scope `once`, human required, 120 s timeout, fail-closed, and false when no route is bound); on approval `run_session_command` reruns the command once with `amended_scope` or `amended_network` (guarded by `command.rerun`). `Unknown` denials are never amended. Path scopes are captured with identity checks before the rerun (`ApprovedPathScope` in `exec_sandbox.rs`).
- Tool-level admission: `bash@2` declares `Effects::empty()` (`crates/tools/src/shell.rs`), so the tier derived in `crates/envd/src/admission.rs` is `read` and no mode prompts for it; only a per-tool `sv_tools_approval` override or a hook changes that. Nested `dyn` targets are admitted separately on their own effects (`DynamicAdmission`, `crates/envd/src/devices_host.rs`).
- Not implemented, and no longer claimed: a git-push, `ln` or other capability unit; prediction of capabilities before execution (grep of `crates/shell`, `crates/envd/src/exec*`). The comment on `effects` in `crates/tools/src/shell.rs` once said the environment host admits exact filesystem, spawn and network effects as interpretation reaches those boundaries; it overstated the code and now describes the static empty declaration and the denial, single prompt and single rerun model.
- `crates/tools/src/shell_intercept.rs` offers rule-configured guidance toward dedicated tools; the grep-to-ripgrep routing in rule 2 is satisfied by the `grep` builtin itself.
- Unverified: whether a rerun can repeat side effects of the first attempt in practice. The design reruns from the start, so it can.

## References

- The Harness Playbook, "The tool surface" — "Deep builtins: Bash"
- 0006 (host policy / sandbox stub), 0010 (jobs), 0012 (convar policy), 0025 (`dyn` builtin)
- `crates/shell` (formerly `crates/shell-engine`), `crates/shell-builtins`, `crates/tools/src/shell.rs`
