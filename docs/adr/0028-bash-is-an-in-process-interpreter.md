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
   that single scope; a second denial is final. A network fact may also be approved for the rest
   of the session, which lets one command chain a bounded number of reruns, each for a new
   endpoint (amendment 2026-10-07, session network grants). Commands whose accesses stay within
   policy run without a prompt. Policy comes from the `sv_sandbox_*` convars (0006, 0012), so a
   path already writable prompts for nothing.
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
  effects that completed before the denial can repeat, up to once per rerun (at most
  `sv_sandbox_network_session_reruns` times for one command chained by session network grants).
  Whether the rerun is safe for a given command is not checked. With the sandbox off, no denial-and-rerun prompt exists, and under the 2026-10-06 amendment the shell itself then prompts under `write`.

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
   `sv_tools_approval` override stays authoritative in every case. (2026-10-07: the escalation keys on
   the typed `Confinement::ExecSandbox` marker, not on the name `bash`; see the typed confinement
   marker amendment.)
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
   approval could not help records no amendable fact and so offers no prompt: `localhost` and its
   subdomains (unless `sv_sandbox_allow_localhost` is set) and any IP literal outside the routable
   space, which the approved rerun would refuse again after resolution. Known limit: a name that
   resolves only to private addresses (a host on a corporate network) still records a fact, and its
   approved rerun is refused after resolution, because the broker never resolves a name it refuses;
   resolving first would turn every refused name into a DNS lookup.
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
   listener thread, which waits in `poll` on its listener and on a wake socket pair made with it, so an
   idle broker costs no wakeups and stopping it needs no new descriptor. The Linux in-namespace relay
   blocks in `accept`. Neither stops on a failure that belongs to one connection, and descriptor or
   memory exhaustion only backs them off (`omp_sandbox::AcceptFailure`).
   The first contact with each host prompts in interactive sessions, and headless print, which binds no
   approval route, ends every refused network attempt as `Denied`; a multi-host install still fails
   after its single rerun unless each host is approved for the session (amendment 2026-10-07,
   session network grants).
7. Network trouble is model-visible. Every refusal from the broker's CONNECT/SOCKS authorization and
   upstream connect is recorded per attempt with a typed cause (`BrokerRefusal` in
   `crates/envd/src/sandbox_proxy.rs`): `policy` (port, deny rule or allowlist), `unresolved`,
   `non-routable` (loopback without the grant, or a private or other non-public address, before or
   after resolution) and `upstream` (every connection to an allowed, resolved host failed). The first
   policy refusal of an attempt is never replaced by a later fail-closed one. Only `policy` becomes the
   amendable network fact, and only `policy` (and, since the session network grants amendment, the
   never amendable `deny-listed`) is answered with the `403` and `X-Omp-Policy-Blocked`
   header that `sandbox_denial_marker` reads; a fail-closed cause is answered `502` with
   `X-Omp-Broker-Refused: <cause>` and never becomes a denial. So a name that does not resolve leaves
   the command `Failed` with its own exit status rather than `Denied`, also when the client prints the
   broker's response headers (`curl -v`), and RPC, subagent and Python consumers do not report it as a
   policy refusal. A shell command whose attempt recorded a refusal gets a `sandbox` diag naming the
   `host:port`, the mode in force and the remedy the first time the session meets that endpoint and
   cause, even when the command exits 0 (a plain-HTTP client that does not fail on the broker's 403):
   `info` on success, `warn` otherwise. Later commands with the same refusal get one short `warn` line
   when they fail and nothing otherwise, so a tool that keeps reaching a refused host in the background
   (update checks, telemetry) does not repeat the remedy. The remedy depends on whether an approval
   route is bound; the host is quoted only when it is an IP literal or a valid DNS name of at most 253
   bytes, and otherwise left out of the same cause-specific text. Without a refusal, a failed command
   whose stderr carried a resolver or connection-failure marker (curl, git, ssh, nc, Python, Go, Node)
   gets the generic text of the mode in force (`scoped`: only proxy-aware clients reach the broker;
   `disabled`; or the broker that could not start), once per session; since a marker is only a phrase
   in the output, the text reports it as such and does not claim the sandbox caused the failure.
   Markers are found per output chunk with a short seam to the previous one, beside the denial
   capture, which is unchanged. Not covered: eval cells and detached processes (point 4) get no
   network diag, so this covers shell commands only; the broker records nothing when its TLS gate
   closes a tunnel (after the `200`) whose ClientHello does not match the tunnelled host, nor for a
   request on an attempt token that is no longer active; and a client that connects to a raw IP
   address prints no resolver marker when the kernel sandbox refuses it (`nc -z 1.1.1.1 443` prints
   nothing and exits 1 under Seatbelt), so it gets no network diag.

## Amendment (2026-10-07, approval relay)

The owner decided that the human of point 4 is asked over the environment protocol when the
command runs on the project daemon. In the default composition a session attaches to that daemon
and `bash` runs there, but the session's approval route is bound only to its own in-process host.
The daemon had no route, so an amendment prompt never reached anyone and the command ended
`Denied`; only the embedded fallback prompted.

1. The daemon relays the prompt to the connection that issued the command, and only to it. The
   prompt is an `ApprovalQuery` on the issuing `Exec` or `InvokeTool` request. It is answered by
   an `ApprovalAnswer` on that request and withdrawn by `ApprovalWithdrawn` (env/v1, `SCHEMA_REV`
   19). It is never broadcast to other sessions attached to the same daemon.
2. A session asks for the relay with the `ClientHello` client feature `approval-relay`. Like
   `edit-repair`, it is one of `omp_env::CLIENT_FEATURES` and never a DATA grant. Only an
   attached session sends it; embedded and isolated compositions keep their in-process route. The
   daemon gives a relay only to an application connection on an environment host. Extension
   connections never get one, so extension code cannot approve its own commands.
3. The session answers through the same kernel route as its in-process prompts
   (`pump_approval_queries` in `crates/envd/src/approval_relay.rs`, over the route
   `ProjectEnvironment::bind_approval_authority` binds). The prompt is journaled and answered with
   `Up::Approve` as before. A withdrawal or a closed transport withdraws the journaled prompt
   instead of answering it. While no route is bound, a relayed prompt is decided by its unreachable
   rules, which deny a sandbox amendment.
4. The daemon decides from its own requirements and the session's decision, so an answer can
   approve or deny but cannot change the fact being approved. An answer with no decision, an
   unknown source or an empty scope denies. An answer that matches no open query on its request is
   ignored without a reply, because an error frame there would end the issuing command's stream.
5. The session's route keeps the 120 s timeout. The daemon's backstop is that timeout plus a 10 s
   grace (`RELAY_GRACE`), so the session's timeout decision lands first and the journal and the
   daemon agree at the boundary. A cancelled command withdraws its prompt. A closed connection
   denies its pending prompts and all later ones, including those of a detached or
   auto-backgrounded command whose issuing connection closed. Such prompts are never handed to
   another session.
6. The wire message mirrors `ApprovalSpec` and is generic, but only sandbox amendments are relayed.
   On the daemon, `dyn` admissions inside `bash`, privileged mutations and named processes
   (`StartProcess`, async `bash`, restart generations) still fail closed. A follow-up can route the
   first two through the same relay. A path amendment offers only `once`, and the daemon honours
   only that. The chat overlay offers `a` (approve for session) only when every requirement of a
   prompt offers `session` (`ApprovalTicket::offers`), so it answers a path amendment with approve
   or deny. A `session` answer from elsewhere (an ACP client's "allow always") is still refused by
   the once-only check, but it grants nothing: `session_grant` in `crates/agent/src/approvals.rs`
   reuses a decision only for requirements whose granting and new prompts both offer its scope, so
   each later path amendment is asked again. The same rule applies to the other once-only prompts,
   `dyn` admissions and privileged mutations, which lose the `a` answer and session reuse that
   their policies never offered. A network amendment offers `once` and `session` (amendment
   2026-10-07, session network grants).

## Amendment (2026-10-07, session network grants)

The owner decided that a network amendment may be approved for the rest of the session (decision
3, option a), so a command that needs several new hosts (`pip install`, `cargo fetch`, Homebrew)
finishes in one tool call, and that an explicit deny beats every approval.

1. Scopes. A network amendment offers `once` and `session`; a path amendment still offers only
   `once`. The environment honours a decision only in a scope its requirement offered, whoever
   made it: a human, or the session's approval desk replaying a journaled grant (source
   `config`). Any other answer is a refusal. `approve_sandbox_amendment` in
   `crates/envd/src/exec.rs` returns the granted lifetime (`AmendmentGrant::{Once, Session}`).
2. Grants follow the approval binding. An approved `session` answer adds the endpoint to the
   `EgressGrants` of the binding that decided it (`crates/envd/src/sandbox_proxy.rs`): one relay
   connection on the project daemon, or the route an in-process composition binds. Every broker
   attempt carries the grants of the binding that issued its command, and the broker consults them
   live in `ProxyPolicy::authorize`: no new broker and no recompiled Seatbelt profile. A grant
   never reaches a command another connection issued, so concurrent sessions sharing one daemon
   never share grants, and a session one composition serves after another never inherits them
   (item 3). Keyed by the normalized host, a lookup allocates nothing.
3. The journal stays the authority. The grants are a cache of journaled decisions. They are
   cleared when their connection closes, when the in-process route is rebound, when the
   conversation is rewound, and when the host switches the composition to another session (new,
   resumed, forked or branched, handed off), whose journal never approved them: the kernel and
   its environment outlive the session they serve. The kernel tells its `SessionObserver`s of
   both. Every rewind reaches `JobBoard::apply_lifecycle`, and every host that replaces the live
   session calls `Kernel::session_switched` once the switch commits (the chat controller's
   `switch_to_in`, the RPC `new_session`, `switch_session` and `branch` commands, ACP's
   `switch_session`). The driver's `bind_environment_approvals`, which binds the kernel's route as
   the environment's approval authority, registers the observer, which calls
   `ProjectEnvironment::session_grants().revoke()` on either. That clears the in-process route's
   grants and, when attached, sends the request-id-zero `RevokeApprovalGrants` client frame
   (env/v1, `SCHEMA_REV` 20), which the daemon handles in frame order for that connection only.
   After a rewind, a switch or a revocation the cache refills lazily: the first refused attempt
   for an endpoint the journal the session now serves still approves is answered by the desk
   without a human, the cache takes the grant again and the command reruns.
4. Rerun budget. The single-rerun guard became `AmendmentReruns`. A fresh command may be amended
   for any fact. After a `session` grant it may rerun again only for a network endpoint its
   binding has not approved, at most `sv_sandbox_network_session_reruns` times (default 4, at
   least 1) per command. A `once` grant ends the chain after its one rerun, as before. A refusal of
   an endpoint that a concurrent command approved after this attempt was refused reruns without a
   prompt, from a fresh command only.
5. Deny precedence. An `sv_sandbox_deny_domains` entry beats every approval, once or session. The
   broker judges the deny list before the one-shot amendment and the session grants and records a
   hit as its own `deny-listed` refusal (`BrokerRefusal::DenyListed`), which fails closed: it is
   never an amendable fact, so it never produces a `sandbox_amendment` ticket, and it is answered
   with the `403` policy marker. The network diag names `sv_sandbox_deny_domains` as the cause and
   says no prompt is offered. This tightens the old `once` behaviour, under which an approval
   bypassed the deny list.
6. Limits. The feature does nothing where no broker runs: Windows has no native command backend,
   and Linux without bubblewrap constructs no sandbox. On Linux the session reruns reuse the
   bubblewrap relay to the same broker socket; only macOS (Seatbelt) is proven here. A rewind or a
   session switch revokes all of the grants, including those the journal the session then serves
   still holds (a resumed session's, a fork's), which then cost one refused attempt each to
   refill. The broker records one refused endpoint per attempt, so parallel fetches of several new
   hosts are found one rerun at a time.

## Amendment (2026-10-07, typed confinement marker)

The owner decided that an active sandbox counts as confinement only for the tools it actually
confines. Before, a defaulted `yolo` kept alive by an active sandbox (point 2 of the 2026-10-06
amendment) auto-approved every tool, including those whose effects run on the host: in the
environment daemon, in the session process, in an MCP or extension process, or in children the
host spawns without the sandbox (browsers, language servers, debug adapters).

1. The tool contract carries a typed marker, `omp_tool::Confinement { Host, ExecSandbox }`, as
   `ToolSpec::confinement`, beside the effects. `Host` (the default) means the declared effects
   are the whole story and run with the user's authority. `ExecSandbox` means the host effects
   happen only in processes spawned under the environment's exec sandbox, or in in-process shell
   builtins checked by its path policy, so the declaration leaves out what the sandbox confines;
   nested `dyn` targets are admitted on their own spec. Coordination inside the session itself
   is not a host effect, and the marker says nothing about it. Only `bash@2` and `hub@2` are
   `ExecSandbox`. `hub` is there for the same reason as the shell: the processes it starts run
   under the exec sandbox, attached (`ExecHost::open_session`) or detached (`detached_sandbox`,
   `crates/envd/src/exec.rs`). Its peer and job operations (`send` into the mailbox of a live
   agent of this session, `cancel` of the session's own jobs) run unsandboxed in the session
   process; they are session-internal, and item 6 lists what that leaves undeclared. `eval` and
   `py_eval` are `Host`: their Python child is sandboxed, but inference, subagents, prelude
   helpers and nested `tool.<name>()` calls run host-side.
2. The registering host asserts the marker; no declaration a worker, an extension or an MCP server
   sends carries it. Python worker specs are stamped `Host` in `worker_spec`
   (`crates/envd/src/tools.rs`), `ExtensionRegistrar::tool_spec` stamps `Host`
   (`crates/agent/src/extensions.rs`), RPC host tools are `Host` (`set_host_tools`), and MCP
   targets are admitted as `Host`. The marker is off the model projection, the wire and the
   journal, so no `name@rev` changes.
3. `resolve_approval` (`crates/envd/src/admission.rs`) applies one rule per call. Tier: an
   `ExecSandbox` tool with no active sandbox is `exec`, which is point 4 above keyed on the marker
   instead of the name. Mode: `call_approval_mode` gives `effective_approval_mode` the sandbox
   only for an `ExecSandbox` tool and no sandbox (`Off`) for a `Host` one. So a defaulted `yolo`
   kept alive by an active sandbox covers sandboxed tools only, and a `Host` tool runs under
   `write` for that call: its `read` and `write` tiers proceed and its `exec` tier (process,
   network, inference, subagents, desktop input) prompts. A `Host` tool's decision is therefore
   identical under every sandbox state. An explicit `yolo` is respected for every tool, as
   before. The receipt (`ResolvedApproval`) records the confinement and the mode in force for the
   call. `/security` reports both modes through the same `call_approval_mode`: under a
   sandbox-kept default `yolo` it reads `yolo` for sandboxed tools and `write` for host tools,
   and the `sv_tools_approval_mode` help says the default `yolo` covers only the tools an active
   sandbox confines.
4. The rule applies at every admission point: the kernel's `SettingsAdmission`
   (`crates/driver/src/headless/kernel.rs`, fed the live spec's effects and marker by the
   dispatcher, with `Host` for a name that has no native live spec), envd's `InvokeTool`
   (environment and worker tools), and `DynamicAdmission` (`dyn` devices and MCP). The kernel's
   evidence names the confinement and the call's mode, and says when the default `yolo` did not
   cover the tool. envd's write boundary also reads the marker: while a write scope applies (plan
   mode, a read-only subagent), an `ExecSandbox` tool is refused before execution as process
   authority that writes around the scoped writers.
5. Consequences. On a host where the sandbox is active (macOS with Seatbelt, Linux with
   bubblewrap) the default posture now prompts for `browser`, `web_search`, `github`, `debug`,
   `computer`, `security_scan`, `image_gen`, `tts`, `eval`, exec-tier MCP targets and exec-tier
   Python devices. Headless print denies prompts, so `omp -p` flows using them need an explicit
   `yolo` or `tools.approval.<name> allow`. A `dyn` target prompt offers `once` only, so repeated
   `dyn` calls to a `Host` device re-prompt. Where no sandbox is active (Windows, Linux without
   bubblewrap, CI) nothing changes: a defaulted `yolo` was already `write`. Workflow posture
   (`ProductionAdwHost::posture`) still reports a sandbox-kept default `yolo` as
   `ApprovalScope::Yolo`, the loosest scope granted, which applies to sandboxed tools.
6. Not covered. The rule trusts declared effects, and several `Host` tools under-declare and stay
   auto-approved: `read@3` fetches URLs, the document host reaches `ssh://` and vault resources,
   `lsp` spawns language servers, memory `reflect` runs inference, RPC host tools and effect-less
   Python workers resolve to `read`, and MCP servers at the `read` tier declare nothing. Session
   tools (`task`, `hub`, `todo`, `goal`) bypass tool admission. Closing that bypass would surface
   two `hub` gaps. First, a sandbox-kept default `yolo` would auto-approve its peer steering: a
   `send` can wake a peer agent's inference, which `hub` does not declare (like memory `reflect`).
   Declaring `hub` `Host` with the same empty envelope would auto-approve it as well (`read` tier),
   so the gap is the undeclared effect, not the marker. Second, envd's write boundary would refuse
   `hub` wholesale under a write scope, although `PLAN_TOOLS` admits it and plan mode only keeps it
   off processes (`crates/agent/src/directors/plan.rs`). Today the boundary never sees `hub`: its
   envd declaration is a stub that kernel session routing intercepts (`HubDeclarationBackend`,
   `crates/driver/src/subagent/hub.rs`). The extension control plane's
   `omp.devices.invoke` (`ProductionDeviceInvocationAdmission`) admits on claimant or capability
   with no tier or mode. A third variant replacing the `SCOPED_WRITERS` name list is a follow-up.

## Status in omp

**Status: Implemented.** Parser, interpreter and coreutils run in process with persistent state, and approval is the sandbox-denial-and-rerun model in the amended decision. Limits: the denial-and-rerun prompt exists only while a sandbox is constructed, and a rerun can repeat side effects. The default `yolo` holds only inside an active sandbox, an explicit one is respected (2026-10-06 amendment). The network is `scoped` by default, a defaulted network mode never sandboxes an explicit `off`, a broker that cannot start under the default disables the network, and network trouble reaches the model as a `sandbox` diag (2026-10-07 amendment). A command on the project daemon asks the session that issued it for its amendment (2026-10-07 approval relay amendment). A network amendment may be approved for the rest of the session, chaining a bounded number of reruns, and an explicit deny beats every approval (2026-10-07 session network grants amendment). The default `yolo` an active sandbox keeps covers only the tools that sandbox confines, keyed on the typed `Confinement` marker (2026-10-07 typed confinement marker amendment). (Verified 2026-10-06 against `omp2` at `f2ca37d533`, plus the changes of the 2026-10-06 and 2026-10-07 amendments; the amendment approver re-verified 2026-10-07 on `feat/envd-approval-relay-daemon` from `omp2` at `1b648d0f74`, and the session half of the relay on `feat/envd-approval-relay-session` from `4b833b0b21`; the session network grants verified 2026-10-07 on `feat/sandbox-network-session-scope` from `omp2` at `1cbea4d0ea`, and their revocation on a session switch on the same branch after `4312bbfc5a`; by reading and by the tests named below.)

- In-process shell: `crates/shell` (parser/runtime) and `crates/shell-builtins` (about 80 builtins including `grep` and `rg` on the ripgrep libraries, `find`, `sed`, `sort`, `ln`, `jq`); persistent cwd and exports through `crates/envd/src/exec.rs`.
- Enforcement: `ExecSandbox` and its per-attempt wrapper in `crates/envd/src/exec_sandbox.rs` implement the shell's `PathPolicy` and `SpawnWrapper`; `exec.rs` installs both on each run (`set_path_policy`, `set_spawn_wrapper`). The sandbox is `workspace-write` by default (`SV_SANDBOX_MODE`, `crates/envd/src/exec_settings/sandbox.rs`), and `SandboxState::probe` reports whether it was constructed; network is `scoped` by default (`SV_SANDBOX_NETWORK_MODE`, 2026-10-07), and the scoped egress broker (`crates/envd/src/sandbox_proxy.rs`) is what produces a typed network fact. `SandboxSettings::network_confinement` and `exec_settings::network_confinement` apply the provenance rule; `ExecSandbox` records the confinement it really applies (`resolve_network` in `exec_sandbox.rs`: a session starts its broker, a `SandboxConsumer::Child` such as an eval cell or a detached process gets `disabled`, a probe starts none) and `amended_scope` reuses it. Proofs: `network_confinement_*` and `an_explicit_off_survives_the_default_flip` in `exec_settings/sandbox.rs`; `explicit_off_*`, `scoped_network_resolves_by_consumer`, `broker_start_failure_degrades_only_the_shipped_default`, `tokenless_children_and_probes_compile_without_a_broker` and `a_path_amendment_after_the_broker_fallback_stays_network_disabled` in `exec_sandbox.rs`; `explicit_off_with_the_default_network_runs_commands_unsandboxed` in `exec.rs`; `unreachable_literals_are_refused_without_an_amendable_fact` in `sandbox_proxy.rs`; the posture tests in `crates/driver/src/adw/production.rs`; `children_keep_an_explicit_scoped_network_under_sandbox_mode_off` in `crates/driver/tests/subagent_cfg.rs`. Under the broker profile the Seatbelt caveats copied into the session note no longer claim unfiltered outbound egress and say commands have no DNS of their own (`crates/sandbox/src/backends/seatbelt.rs`). Checked live on macOS with Seatbelt and the default settings (a throwaway test, not kept): `/usr/bin/curl https://example.com` ended `Denied` with the fact `network example.com:443` (curl exit 56, `CONNECT tunnel failed, response 403`), and `/usr/bin/nc -z` to a raw IP on port 22 could not connect, while both succeeded outside the sandbox.
- Read lane (2026-10-07): `FilePolicy::admit_read` walks the requested path physically (`resolve_physical_path`: links followed in place, `..` applied after them, as the kernel, `read_dir` and a spawned child resolve it), and `check_read` and `FilePolicy::open` both use its result; `open` opens exactly that path from its root with `O_NOFOLLOW` on every component, so `link/../key` is judged and read as the link target's sibling, not the link's. In `host` read mode it follows symlinks and denies when the walk enters or ends in a `read_deny` root or the `..`-collapsed spelling lies in one (a link entry inside a denied root stays denied, as under bubblewrap's mask); loops and other resolution errors deny. Known limits of the in-process lane: a missing directory followed by `..` resolves instead of failing with `ENOENT` as it does for the kernel, and `read_deny` roots are matched byte-wise, so a case variant of a denied root on a case-insensitive volume is not refused in process. Utility builtins that open files directly (`cat.rs`, `head.rs`, `grep.rs` in `crates/shell-builtins`) do not call `check_read`; that gap predates this lane and is tracked in `docs/parked/2026-10-06-handoff-pending-work.md`. A program reached through a link, such as Homebrew's `/opt/homebrew/bin/git` into `../Cellar`, a glob through a linked directory, or a cwd that crosses a link, now runs in the foreground as it already did in detached scripts, which re-enter the shell child under the kernel wrapper only (`detached_command` in `exec.rs`; `shell_child.rs` installs no path policy). The `minimal` and `scoped` read modes keep refusing symlinks until a follow-up grants the traversed link entries in both lanes: Seatbelt needs a read on each link it resolves, and bubblewrap's restricted view binds only canonical paths. Proofs: the `host_read_*`, `restricted_read_mode_*`, `approved_read_scope_*` and `protected_open_*` tests in `exec_sandbox.rs`; `environment_only_sandbox_*` (including `link/../key` through the redirect and glob lanes) and the live Seatbelt `sandboxed_session_runs_programs_reached_through_symlinks` in `exec.rs`.
- Network diag (2026-10-07, point 7): the broker records `BrokerDenial { host, port, cause: BrokerRefusal }` per attempt (`ProxyPolicy::record`, first policy refusal wins; `ProxyPolicy::authorize` and `ProxyPolicy::connect` return the cause, `resolved_refusal` judges the resolved addresses, and `connect` records `upstream`); `http` answers only `policy` with `http_policy_deny` (`403`, `X-Omp-Policy-Blocked`) and a fail-closed cause with `http_broker_refused` (`502`, `X-Omp-Broker-Refused`); `ExecSandboxAttempt::take_facts` returns the amendable `denial` (policy only) beside the `refusal` of any cause; `run_session_command` pushes `exec_network_diag::network_diag` before the final `finish_session_command`, with the session's `NetworkAnnouncements` (the generic text once, each refusal's full text once per endpoint and cause), and `NetworkMarkerScan` feeds on each stderr/pty chunk in `OutputSequencer::capture_sandbox_diagnostic` while `sandbox_denial_marker` keeps its own capture (both use `find_marker`). Proofs: `fail_closed_refusals_are_recorded_per_attempt`, `resolved_addresses_are_judged_as_a_whole`, `only_policy_refusals_carry_the_policy_marker` and `unreachable_literals_are_refused_without_an_amendable_fact` in `sandbox_proxy.rs`; the marker and split-invariance tests, `refusals_are_explained_once_per_endpoint_and_cause`, `unquotable_refusals_keep_their_cause_and_leave_the_generic_text_unspent` and `generic_texts_name_the_mode_and_appear_once_per_session` in `exec_network_diag.rs`; `fail_closed_broker_refusals_stay_ordinary_failures` (the broker's whole response head as stderr), `network_markers_leave_denial_classification_unchanged` and the live Seatbelt `scoped_network_trouble_reaches_the_model_as_sandbox_diags` (offline, `.invalid` hosts; it includes `curl -v` on an allowed name that does not resolve, which ends `Failed`) in `exec.rs`. Checked live on macOS with Seatbelt and the default settings (a throwaway test, not kept): `/usr/bin/curl -sS --max-time 10 https://example.com` ended `Denied` (exit 56) with a `warn` diag naming `example.com:443`, the scoped mode and, with no approval route bound, the convar remedy only; `/usr/bin/nc -z -G 5 github.com 22` failed (exit 1, `nodename nor servname provided`) with the generic `warn` text naming `HTTP_PROXY` once, and the same command again added no diag.
- Approval: `classify_sandbox_denial` in `crates/envd/src/exec.rs` types the denial (`SandboxDenialFact::{ReadPath, WritePath, Network, Unknown}`); `approve_sandbox_amendment` asks one `sandbox_amendment` ticket (scope `once`, human required, 120 s timeout, fail-closed) of the approver `ExecHost::amendment_approver` picks, and is false when there is none. The relay of the connection that issued the command comes first (`crates/envd/src/approval_relay.rs`): an `Exec`, or a native invocation through the `tools::invocation_approvals` task-local, on an application connection that advertised `approval-relay`, prompts that connection on that request only. Without one, the route an in-process composition binds (`bind_sandbox_approval_route`) decides. A project daemon binds no route. An attached session advertises `approval-relay` (`attached_features` in `crates/envd/src/lib.rs`, sent only by `hello_attached_session`), and `connect_peer` starts `approval_relay::pump_approval_queries`, which files each query on the route `ProjectEnvironment::bind_approval_authority` stores (the driver's kernel route, `crates/driver/src/headless/kernel.rs`) with `request_cancellable` and answers with its decision; a withdrawal, a closed transport or shutdown cancels the filed prompt without answering, and a query that arrives while no route is bound is decided by its unreachable rules, which deny a sandbox amendment. A relay whose connection closed never falls back to the route: a command that outlives its connection (detached, auto-backgrounded) fails closed. Named processes (`start_process`: `StartProcess`, async bash, restart generations) carry no relay. Proofs: the `approval_relay` unit tests (daemon half, and the session half: `the_session_answers_through_its_bound_route_on_the_query_request`, `a_withdrawn_query_cancels_its_prompt_and_is_never_answered`, `a_closed_transport_cancels_open_prompts_without_answering`, `an_unbound_session_denies_relayed_prompts_as_unreachable`, `relayed_requirements_and_decisions_survive_the_wire`), `attached_hello_relays_approvals_and_advertises_only_supplied_edit_repair_facts` in `lib.rs`, `a_command_relay_outranks_the_host_route_and_never_falls_back` in `exec.rs`, `only_advertising_application_connections_relay_approvals`, `closing_a_connection_disconnects_its_relay`, `daemon_relays_an_amendment_only_to_the_issuing_connection`, `daemon_withdraws_cancelled_prompts_and_fails_closed_on_disconnect` and `native_bash_relays_its_amendment_on_the_invocation_request` in `server.rs`, and in `crates/driver/tests/approval_authority.rs` the `attached_daemon` tests, which attach the production composition to a daemon served in the test process (asserting no fallback notice): an approved amendment is journaled once with no invocation id and its rerun writes into the protected `.git`, a second attached session is never asked, and a refused one ends `denied` with nothing written. Against a separate `omp envd` process, P12 (`crates/e2e/tests/p12_daemon_approvals.rs`, the daemon child attached to a real docserver under an isolated home) proves that only the issuing connection is prompted, that another connection's forged answer decides nothing, that the approved rerun writes the daemon's own pid through `$$`, that the daemon never withdraws a prompt its owner answered, that a cancel withdraws the prompt before the exit, and that closing the issuing connection ends its command without writing and frees its session at once. That close also cancels the connection's commands, so P12 cannot tell it from the relay failing the prompt closed; `closing_a_connection_disconnects_its_relay` proves the relay's disconnect for a command that outlives its connection. Through the real chat on a real PTY, P7's `chat_tui_answers_a_daemon_sandbox_amendment_from_its_overlay` (`crates/e2e/tests/p7_tui.rs`, macOS) runs the shipped default posture with chat attached to the daemon it spawns: the overlay offers only approve and deny and `a` is no answer, its `y` reruns the command, which writes the pid of a live process of the same executable that is not chat, `Esc` denies a second command, which writes nothing, and the journal replays both decided `once` prompts. The overlay and grant rule are proven by `pending_approval_projects_overlay_and_hotkeys` and `a_session_offering_approval_answers_a_for_the_session` in `crates/chat/tests/host.rs` and by `a_session_grant_never_answers_a_prompt_that_offers_only_once` and `a_session_answer_the_prompt_never_offered_grants_nothing` in `crates/agent/tests/approval_desk.rs`. On a `once` approval `run_session_command` reruns the command once with `amended_scope` or `amended_network` (`AmendmentReruns::Spent` then ends the chain; session grants are the bullet below). `Unknown` denials are never amended. Path scopes are captured with identity checks before the rerun (`ApprovedPathScope` in `exec_sandbox.rs`).
- Session network grants (2026-10-07): `approve_sandbox_amendment` offers `["once", "session"]` for a network fact and `["once"]` for a path, honours only an offered scope, and on `session` inserts the endpoint into the deciding approver's `EgressGrants` (`EnvApprover::grants`: `OwnedApprovals::grants` on a relay, `RouteBinding::grants` on the in-process route, `crates/envd/src/approval_relay.rs`). `run_session_command` begins each attempt with `ExecHost::egress_grants` of the command's binding (`ExecSandbox::begin_attempt`, `ScopedProxy::begin_attempt`), and `AmendmentReruns::admits` with `sv_sandbox_network_session_reruns` (`SV_SANDBOX_NETWORK_SESSION_RERUNS`, `crates/envd/src/exec_settings/sandbox.rs`) bounds the chain; a session grant reruns under the same sandbox, a once grant compiles the amended one. `bind_sandbox_approval_route` clears the previous binding's grants, `ConnectionApprovals::disconnect` and `revoke_grants` clear a relay's, and `EnvServer::revoke_approval_grants` and the `RevokeApprovalGrants` frame (routed to the environment backend by `crates/env/src/partition.rs`, sent by `EnvClient::revoke_approval_grants`) serve `SessionGrants::revoke` (`crates/envd/src/lib.rs`), which the driver's `RevokeSessionGrants` (`crates/driver/src/headless/kernel.rs`, registered by `bind_environment_approvals` through `JobBoard::observe_sessions`) calls on every rewind and on every session switch: `Kernel::session_switched` (`crates/agent/src/loop.rs`) is called by `switch_to_in` in `crates/app/src/chat_control.rs`, the session transition in `crates/app/src/rpc_mode.rs` and `switch_session` in `crates/app/src/acp_mode.rs`. Proofs: `session_grants_admit_their_exact_endpoint_on_a_running_policy`, `deny_rules_beat_every_approval`, `deny_rules_and_ports_precede_resolution` and `only_policy_refusals_carry_the_policy_marker` in `sandbox_proxy.rs`; `jit_approval_names_only_the_detected_capability_and_exact_command`, `network_amendments_offer_the_session_and_paths_stay_once`, `amendment_reruns_admit_only_new_session_network_endpoints`, `rebinding_or_revoking_the_route_clears_its_egress_grants` and `a_command_relay_outranks_the_host_route_and_never_falls_back` in `exec.rs`; `network_session_reruns_default_to_four_and_project_a_set_bound` in `exec_settings/sandbox.rs`; `refusals_are_explained_once_per_endpoint_and_cause` in `exec_network_diag.rs`; the live Seatbelt `session_network_grants_chain_reruns_and_outlive_the_command` (the pip pattern on two loopback upstreams with `curl --noproxy ''`: one call, two prompts, three runs, then no prompt in the same or another shell), `once_network_grants_still_end_the_chain_after_one_rerun` and `deny_listed_hosts_are_never_offered_for_approval` in `tool_shell.rs`; `daemon_session_grants_belong_to_the_approving_connection` in `server.rs` (a second connection to the same daemon is asked, and `RevokeApprovalGrants` makes the owner be asked again); `grant_revocation_is_request_zero_and_requires_hello` in `crates/env/src/client.rs` and `grant_revocation_routes_to_the_environment_and_opens_no_route` in `partition.rs`; `every_rewind_and_switch_tells_the_session_observers` in `crates/agent/tests/jobs.rs`; `a_resumed_session_answers_a_journaled_network_grant_and_asks_the_rest`, `a_rewind_past_a_session_grant_asks_again_and_tells_session_observers` and `a_switch_away_from_a_session_grant_asks_again_and_tells_session_observers` in `crates/agent/tests/approval_desk.rs`; the `session_network_grants` tests in `crates/driver/tests/approval_authority.rs`, whose kernel is wired by the production `bind_environment_approvals`: on a daemon served in the test process (`an_attached_grant_holds_until_a_rewind_revokes_it`, `an_attached_grant_never_reaches_the_next_session`) and on an embedded composition's in-process route (`an_embedded_grant_holds_until_a_rewind_revokes_it`, `an_embedded_grant_never_reaches_the_next_session`), the second command fetches unprompted, and after the rewind or the switch to a new session a human is asked again; and the host switch paths, `session_switch_orders_gate_flush_transition_resync_observation_then_start` and `fork_to_an_entry_and_new_session_tell_the_kernel_of_a_switch` in `chat_control.rs`, `rpc_session_commands_publish_reset_snapshots` in `crates/app/tests/rpc_spine.rs` and `new_load_and_resume_switch_the_authoritative_durable_session` in `crates/app/tests/acp_spine.rs`, each of which observes one switch and no rewind per committed transition.
- Tool-level admission: `bash@2` declares `Effects::empty()` and `Confinement::ExecSandbox` (`crates/tools/src/shell.rs`), so with an active sandbox the tier derived in `crates/envd/src/admission.rs` is `read` and no mode prompts for it, and without one it is `exec`; only a per-tool `sv_tools_approval` override or a hook changes that. Nested `dyn` targets are admitted separately on their own effects and confinement (`DynamicAdmission`, `crates/envd/src/devices_host.rs`).
- Typed confinement marker (2026-10-07): `Confinement` and `ToolSpec::confinement` in `crates/tool/src/lib.rs`, `DeviceTarget::confinement` in `crates/tool/src/registry.rs`; `resolve_approval` takes it (`call_approval_mode` in `crates/envd/src/admission.rs`, which the `/security` feed `approval_posture` in `crates/app/src/chat_services/misc.rs` also reports per confinement), as do `ToolSettings::approval_for`, `ToolAdmission::admit` (`crates/agent/src/dispatch.rs`, from `Registry::live_spec`), `SettingsAdmission`, envd's `InvokeTool` admission and `InvocationExecutionPolicy::denial` (`crates/envd/src/server.rs`), and `DynamicAdmission::admit`. Proofs: `exec_sandboxed_tools_are_exec_tier_unless_a_sandbox_confines_them`, `a_defaulted_yolo_covers_only_sandboxed_tools`, the property test `host_tools_are_admitted_as_if_no_sandbox_existed`, `per_tool_override_is_authoritative_and_receipted` and `dynamic_admission_prompts_for_host_targets_under_a_defaulted_yolo` in `admission.rs`; `approval_for_threads_confinement` in `tool_settings.rs`; `host_dyn_devices_are_not_auto_approved_inside_a_sandbox` in `devices_host.rs`; `only_sandbox_spawning_tools_are_exec_sandboxed` in `crates/envd/src/tools.rs` (exactly `bash@2` and `hub@2`); `write_scopes_refuse_tools_that_write_around_the_scoped_writers` (the real `bash` spec) and the live Seatbelt `a_sandbox_kept_default_yolo_covers_only_sandboxed_tools` in `server.rs` (under the shipped `yolo`, `bash` runs unprompted while a `Host` network tool asks the client and, refused, never runs; under an explicit `yolo` neither asks); `settings_admission_prompts_for_host_tools_under_a_sandboxed_default_yolo` in `kernel.rs`; `native_admission_receives_the_live_spec_confinement` in `crates/agent/tests/lifecycle_hooks.rs`; `extension_tools_cannot_claim_the_exec_sandbox` in `extensions.rs`; `confinement_defaults_to_host_and_stays_off_the_projection` in `crates/tool/tests/contracts.rs` and `host_tools_are_host_confined` in `registry.rs`; `security_report_shows_the_effective_approval_mode_beside_the_configured_one` in `crates/chat/src/commands/misc.rs` (`yolo` for sandboxed tools and `write` for host tools under a sandbox-kept default `yolo`, one mode otherwise).
- Not implemented, and no longer claimed: a git-push, `ln` or other capability unit; prediction of capabilities before execution (grep of `crates/shell`, `crates/envd/src/exec*`). The comment on `effects` in `crates/tools/src/shell.rs` once said the environment host admits exact filesystem, spawn and network effects as interpretation reaches those boundaries; it overstated the code and now says the empty declaration holds only because `ExecSandbox` confinement stands in for it.
- `crates/tools/src/shell_intercept.rs` offers rule-configured guidance toward dedicated tools; the grep-to-ripgrep routing in rule 2 is satisfied by the `grep` builtin itself.
- Unverified: whether a rerun can repeat side effects of the first attempt in practice. The design reruns from the start, so it can.

## References

- The Harness Playbook, "The tool surface" — "Deep builtins: Bash"
- 0006 (host policy / sandbox stub), 0010 (jobs), 0012 (convar policy), 0025 (`dyn` builtin)
- `crates/shell` (formerly `crates/shell-engine`), `crates/shell-builtins`, `crates/tools/src/shell.rs`
