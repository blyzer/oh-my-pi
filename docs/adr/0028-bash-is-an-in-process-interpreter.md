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
4. Approval happens at the capability boundary, just in time, as interpretation reaches it:
   the first write outside the workspace, `ln`, `git push`, a network request. Read-only
   segments MUST run without a prompt. The check consults the user's existing read/write policy
   (0006, 0012), so a directory already allowed for writes prompts for nothing.
5. The unit of approval is the capability ("May I use Git to push?"), NEVER the shell string. The
   harness is a capability approver, not the TSA for `Bash`.

## Consequences

- Runtime policy from 0006 becomes enforceable inside shell commands: the sandbox stub sees
  individual capability requests, not opaque strings.
- Platform neutrality nearly free: most invocations execute in-process on Windows identically.
- Interception is a general mechanism — `dyn` (0025), `grep` → ripgrep, and tool-recommendation
  guidance all sit on the same parsed command stream.
- Prohibited: `/bin/bash -c`, `$PATH`-first resolution, string-level allow/deny regexes as the
  security boundary, prompt text steering the model away from standard command names.
- Cost accepted: a bash-compatible interpreter and coreutils port is a large, permanent
  engineering surface; incompatibilities with GNU/BSD edge behaviour are the harness's bugs.

## Status in omp

**Status: Partially implemented.** Parser, interpreter and coreutils are in process with persistent state; approval is sandbox-denial-and-rerun, not interpreter-boundary capability approval. (Verified 2026-10-04 against `omp2` at `083b38fe7d`.)

- In-process shell: `crates/shell` (parser/runtime) and `crates/shell-builtins` (about 80 builtins including `grep` and `rg` on the ripgrep libraries, `find`, `sed`, `sort`, `ln`, `jq`); persistent cwd and exports through `crates/envd/src/exec.rs`.
- Divergence: approval in `crates/envd/src/exec.rs` (around line 2430), `crates/envd/src/exec_sandbox.rs` and `crates/envd/src/tool_shell.rs` classifies an OS sandbox denial, asks once for a path or network amendment (`ApprovedSandboxAmendment::{Path, Network}`) and reruns the command. The ADR describes just-in-time capability approval as interpretation reaches the boundary ('May I use Git to push?'). I found no git-push or `ln` capability unit (grep of `crates/shell`, `crates/envd/src/exec*`).
- The prior gap 'no distinct network-request approval' looks stale: network amendments exist. Whether a rerun can repeat side effects of the first attempt is not verified.
- `crates/tools/src/shell_intercept.rs` offers rule-configured guidance toward dedicated tools; the ADR's grep-to-ripgrep routing is satisfied by the `grep` builtin itself. Owner decision: keep the denial-and-rerun model and amend the ADR, or implement boundary-time capability approval.

## References

- The Harness Playbook, "The tool surface" — "Deep builtins: Bash"
- 0006 (host policy / sandbox stub), 0010 (jobs), 0012 (convar policy), 0025 (`dyn` builtin)
- `crates/shell` (formerly `crates/shell-engine`), `crates/shell-builtins`, `crates/tools/src/shell.rs`
