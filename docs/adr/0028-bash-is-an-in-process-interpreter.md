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
   for network. The in-process path check walks each path as the kernel does (a symlink is
   followed where it stands and a later `..` leaves the link target's directory), and a
   redirection opens exactly the path that walk judged. Under the default `host` read mode a read
   follows symlinks and is refused when its walk enters or ends in an `sv_sandbox_read_deny` root,
   or when its `..`-collapsed spelling lies in one; the `minimal` and `scoped` read modes also
   refuse a walk that follows a symlink outside the runtime roots. A redirection that writes
   refuses any walk that follows a symlink, while builtin writes gated by `check_write` (`mkdir`,
   `touch`, `rm`, …) resolve links and judge the target against the writable and
   `sv_sandbox_write_deny` roots. Utility builtins that open files themselves (`cat`, `head`,
   `grep`) do not consult the read check yet. A denial
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
   cannot be constructed is a hard error, as before, and so is a refused policy. (2026-10-07: under an
   explicit `off`, a defaulted network mode does not count as asking for a network-only sandbox, and an
   egress broker that cannot start under the shipped default disables the network instead of failing;
   see that amendment.)
4. The shell is process authority when nothing confines it: with no active sandbox, `bash` (which
   declares no effects) resolves to the `exec` tier, so `write` and `always-ask` prompt for it. With an
   active sandbox it stays `read` and the denial-and-rerun flow above is unchanged. A per-tool
   `sv_tools_approval` override stays authoritative in every case.
5. A `yolo` that no sandbox confines is reported once per session as a typed notice, `<notice kind=warn
   name=approval-posture>`, whose data carries the configured mode, who configured it, the effective
   mode, and the sandbox state with its cause: downgraded to `write` when it was the default, respected
   but unconfined when the user asked for it. `/security` shows the effective mode beside the configured
   one.

## Amendment (2026-10-07)

The owner decided that the shipped network posture for agent commands is `scoped`, not `disabled`.

1. `sv_sandbox_network_mode` defaults to `scoped`. Inside the sandbox, HTTP(S) and SOCKS clients that
   honour `HTTP_PROXY`, `HTTPS_PROXY` or `ALL_PROXY` reach the session's egress broker
   (`crates/envd/src/sandbox_proxy.rs`). It refuses every host that is not allowlisted
   (`sv_sandbox_allow_domains` is empty by default) and records the typed network fact that the
   denial-and-rerun flow above asks about, once per command. Clients that ignore the proxy environment
   (ssh, nc, raw sockets) cannot connect, and commands have no DNS of their own. A request whose
   approval could not help records no fact and so offers no prompt: `localhost` and its subdomains
   (unless `sv_sandbox_allow_localhost` is set) and any IP literal outside the routable space, which the
   approved rerun would refuse again after resolution. Known limit: a name that resolves only to
   private addresses (a host on a corporate network) still records a fact, and its approved rerun is
   refused after resolution, because the broker never resolves a name it refuses; resolving first would
   turn every refused name into a DNS lookup.
2. A defaulted network mode never sandboxes an explicit `sv_sandbox_mode off`: only a `scoped` mode the
   user set compiles a network-only sandbox there. `off` therefore keeps meaning unsandboxed on hosts
   with no native backend (Linux without bubblewrap, Windows). One typed answer, `NetworkConfinement`
   (`unconfined`, `disabled`, `scoped`; `crates/envd/src/exec_settings/sandbox.rs`), feeds the sandbox
   compiler, the workflow posture and `/security`. A requested sandbox that was not constructed confines
   no network. It is the confinement shell sessions are configured for, not a measurement: a construction
   probe starts no broker, so a session whose broker could not start (point 3) runs `disabled` while it
   reports `scoped`, and eval cells and detached processes (point 4) always run `disabled`. A spawned
   child keeps its parent's choice: `Ctx::seed_child` carries every value the user set, not only those
   that differ from the default, so an explicit `scoped` under an explicit `off` still confines the
   child, as it already did for a resumed child, whose seed copies the session and archive layers.
3. When the egress broker cannot start under the shipped default (loopback bind, temporary directory,
   thread spawn), the session keeps its filesystem confinement and runs with the network `disabled`. A
   warning is logged and the session note says `network=disabled (scoped broker unavailable)`. A sandbox
   the user configured (any user-set `sv_sandbox_*` convar, as in point 3 of the 2026-10-06 amendment)
   still fails hard, and so does an approved network amendment whose one-shot broker cannot start. A path
   amendment reruns with the session's resolved network and broker, so a session that fell back stays
   disabled.
4. Eval cells and detached processes compile with the network `disabled`. They never hold a broker
   attempt token, so the broker would refuse every request they made. Construction probes compile the
   scoped profile without starting a broker.
5. Workflow phases: `ProductionAdwHost::posture` reports that confinement (point 2). A user
   phase that declares `requires.network = "disabled"` (`.omp/workflows/<name>.toml`, loaded by
   `crates/driver/src/adw/definition.rs`) is now refused under the default posture, which is `scoped`, or
   `unrestricted` where the sandbox is not constructed. It is also refused under mode `off` even with an
   explicit `disabled` network, which now reports `unrestricted` because nothing enforces it.
6. Accepted costs. On macOS the scoped Seatbelt profile re-allows the Apple network mach services
   (`com.apple.trustd`, `com.apple.SecurityServer`, `com.apple.ocspd`, the `SystemConfiguration`
   services and others) for every default command, where the closed network denied them; a follow-up
   should measure which of them proxy-aware TLS clients need. Every default shell session owns a broker
   listener thread, which blocks in `accept` and is woken by a self-connect on drop rather than polling.
   The first contact with each host prompts in interactive sessions, and headless print, which binds no
   approval route, ends every refused network attempt as `Denied`; a multi-host install still fails
   after its single rerun.

## Status in omp

**Status: Implemented.** Parser, interpreter and coreutils run in process with persistent state, and approval is the sandbox-denial-and-rerun model in the amended decision. Limits: the denial-and-rerun prompt exists only while a sandbox is constructed, and a rerun can repeat side effects. The default `yolo` holds only inside an active sandbox, an explicit one is respected (2026-10-06 amendment). The network is `scoped` by default, a defaulted network mode never sandboxes an explicit `off`, and a broker that cannot start under the default disables the network (2026-10-07 amendment). (Verified 2026-10-06 against `omp2` at `f2ca37d533`, plus the changes of the 2026-10-06 and 2026-10-07 amendments.)

- In-process shell: `crates/shell` (parser/runtime) and `crates/shell-builtins` (about 80 builtins including `grep` and `rg` on the ripgrep libraries, `find`, `sed`, `sort`, `ln`, `jq`); persistent cwd and exports through `crates/envd/src/exec.rs`.
- Enforcement: `ExecSandbox` and its per-attempt wrapper in `crates/envd/src/exec_sandbox.rs` implement the shell's `PathPolicy` and `SpawnWrapper`; `exec.rs` installs both on each run (`set_path_policy`, `set_spawn_wrapper`). The sandbox is `workspace-write` by default (`SV_SANDBOX_MODE`, `crates/envd/src/exec_settings/sandbox.rs`), and `SandboxState::probe` reports whether it was constructed; network is `scoped` by default (`SV_SANDBOX_NETWORK_MODE`, 2026-10-07), and the scoped egress broker (`crates/envd/src/sandbox_proxy.rs`) is what produces a typed network fact. `SandboxSettings::network_confinement` and `exec_settings::network_confinement` apply the provenance rule; `ExecSandbox` records the confinement it really applies (`resolve_network` in `exec_sandbox.rs`: a session starts its broker, a `SandboxConsumer::Child` such as an eval cell or a detached process gets `disabled`, a probe starts none) and `amended_scope` reuses it. Proofs: `network_confinement_*` and `an_explicit_off_survives_the_default_flip` in `exec_settings/sandbox.rs`; `explicit_off_*`, `scoped_network_resolves_by_consumer`, `broker_start_failure_degrades_only_the_shipped_default`, `tokenless_children_and_probes_compile_without_a_broker` and `a_path_amendment_after_the_broker_fallback_stays_network_disabled` in `exec_sandbox.rs`; `explicit_off_with_the_default_network_runs_commands_unsandboxed` in `exec.rs`; `unreachable_literals_are_refused_without_an_amendable_fact` in `sandbox_proxy.rs`; the posture tests in `crates/driver/src/adw/production.rs`; `children_keep_an_explicit_scoped_network_under_sandbox_mode_off` in `crates/driver/tests/subagent_cfg.rs`. Under the broker profile the Seatbelt caveats copied into the session note no longer claim unfiltered outbound egress and say commands have no DNS of their own (`crates/sandbox/src/backends/seatbelt.rs`). Checked live on macOS with Seatbelt and the default settings (a throwaway test, not kept): `/usr/bin/curl https://example.com` ended `Denied` with the fact `network example.com:443` (curl exit 56, `CONNECT tunnel failed, response 403`), and `/usr/bin/nc -z` to a raw IP on port 22 could not connect, while both succeeded outside the sandbox.
- Read lane (2026-10-07): `FilePolicy::admit_read` walks the requested path physically (`resolve_physical_path`: links followed in place, `..` applied after them, as the kernel, `read_dir` and a spawned child resolve it), and `check_read` and `FilePolicy::open` both use its result; `open` opens exactly that path from its root with `O_NOFOLLOW` on every component, so `link/../key` is judged and read as the link target's sibling, not the link's. In `host` read mode it follows symlinks and denies when the walk enters or ends in a `read_deny` root or the `..`-collapsed spelling lies in one (a link entry inside a denied root stays denied, as under bubblewrap's mask); loops and other resolution errors deny. Known limits of the in-process lane: a missing directory followed by `..` resolves instead of failing with `ENOENT` as it does for the kernel, and `read_deny` roots are matched byte-wise, so a case variant of a denied root on a case-insensitive volume is not refused in process. Utility builtins that open files directly (`cat.rs`, `head.rs`, `grep.rs` in `crates/shell-builtins`) do not call `check_read`; that gap predates this lane and is tracked in `docs/parked/2026-10-06-handoff-pending-work.md`. A program reached through a link, such as Homebrew's `/opt/homebrew/bin/git` into `../Cellar`, a glob through a linked directory, or a cwd that crosses a link, now runs in the foreground as it already did in detached scripts, which re-enter the shell child under the kernel wrapper only (`detached_command` in `exec.rs`; `shell_child.rs` installs no path policy). The `minimal` and `scoped` read modes keep refusing symlinks until a follow-up grants the traversed link entries in both lanes: Seatbelt needs a read on each link it resolves, and bubblewrap's restricted view binds only canonical paths. Proofs: the `host_read_*`, `restricted_read_mode_*`, `approved_read_scope_*` and `protected_open_*` tests in `exec_sandbox.rs`; `environment_only_sandbox_*` (including `link/../key` through the redirect and glob lanes) and the live Seatbelt `sandboxed_session_runs_programs_reached_through_symlinks` in `exec.rs`.
- Approval: `classify_sandbox_denial` in `crates/envd/src/exec.rs` types the denial (`SandboxDenialFact::{ReadPath, WritePath, Network, Unknown}`); `approve_sandbox_amendment` asks one `sandbox_amendment` ticket (scope `once`, human required, 120 s timeout, fail-closed, and false when no route is bound); on approval `run_session_command` reruns the command once with `amended_scope` or `amended_network` (guarded by `command.rerun`). `Unknown` denials are never amended. Path scopes are captured with identity checks before the rerun (`ApprovedPathScope` in `exec_sandbox.rs`).
- Tool-level admission: `bash@2` declares `Effects::empty()` (`crates/tools/src/shell.rs`), so the tier derived in `crates/envd/src/admission.rs` is `read` and no mode prompts for it; only a per-tool `sv_tools_approval` override or a hook changes that. Nested `dyn` targets are admitted separately on their own effects (`DynamicAdmission`, `crates/envd/src/devices_host.rs`).
- Not implemented, and no longer claimed: a git-push, `ln` or other capability unit; prediction of capabilities before execution (grep of `crates/shell`, `crates/envd/src/exec*`). The comment on `effects` in `crates/tools/src/shell.rs` once said the environment host admits exact filesystem, spawn and network effects as interpretation reaches those boundaries; it overstated the code and now describes the static empty declaration and the denial, single prompt and single rerun model.
- `crates/tools/src/shell_intercept.rs` offers rule-configured guidance toward dedicated tools; the grep-to-ripgrep routing in rule 2 is satisfied by the `grep` builtin itself.
- Unverified: whether a rerun can repeat side effects of the first attempt in practice. The design reruns from the start, so it can.

## References

- The Harness Playbook, "The tool surface" — "Deep builtins: Bash"
- 0006 (host policy / sandbox stub), 0010 (jobs), 0012 (convar policy), 0025 (`dyn` builtin)
- `crates/shell` (formerly `crates/shell-engine`), `crates/shell-builtins`, `crates/tools/src/shell.rs`
