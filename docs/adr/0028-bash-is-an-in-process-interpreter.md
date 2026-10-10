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
   `lsp` spawns language servers, RPC host tools and effect-less Python workers resolve to `read`,
   and MCP servers at the `read` tier declare nothing. (Memory `reflect` was on this list until it
   declared its one inference request, 2026-10-07: see the amendment of that date. RPC host tools
   and effect-less Python workers were on it until the 2026-10-08 undeclared effects amendment.
   `read@3`'s fetches, `ssh://` reads included, were on it until the 2026-10-08 read fetch
   amendment, which also records language servers and the vault CLI as environment-ambient.) Session
   tools (`task`, `hub`, `todo`, `goal`) bypass tool admission. Closing that bypass would surface
   two `hub` gaps. First, a sandbox-kept default `yolo` would auto-approve its peer steering: a
   `send` can wake a peer agent's inference, which `hub` does not declare.
   Declaring `hub` `Host` with the same empty envelope would auto-approve it as well (`read` tier),
   so the gap is the undeclared effect, not the marker. Second, envd's write boundary would refuse
   `hub` wholesale under a write scope, although `PLAN_TOOLS` admits it and plan mode only keeps it
   off processes (`crates/agent/src/directors/plan.rs`). Today the boundary never sees `hub`: its
   envd declaration is a stub that kernel session routing intercepts (`HubDeclarationBackend`,
   `crates/driver/src/subagent/hub.rs`). The extension control plane's
   `omp.devices.invoke` (`ProductionDeviceInvocationAdmission`) admits on claimant or capability
   with no tier or mode. A third variant replacing the `SCOPED_WRITERS` name list is a follow-up.

## Amendment (2026-10-07, memory reflect inference)

Memory `reflect` asked a reflection host for a synthesis but declared no effects, so it was `read`
tier and auto-approved everywhere. Since the journal-first driver landed (`bbdbf6b889`) nothing
bound that host either, so every call answered with the recalled evidence instead.

1. `reflect@2` declares one inference request with no spend ceiling of its own
   (`reflect_spec`, `crates/tools/src/memory.rs`); `recall` and `retain` still declare nothing.
   The tier is `exec` and the tool is `Host`, so the shipped default posture, `write` and
   `always-ask` prompt for it, and only an explicit `yolo` (or a `reflect` allow in `sv_tools_approval`)
   runs it unprompted. Inference is not a write, so envd's write boundary does not refuse it
   (plan mode's roster leaves `reflect` out anyway).
2. `compose_kernel` binds the environment's reflection bridge (`bind_reflection`,
   `crates/driver/src/headless/reflection.rs`) to the session's own `Inference` capability, a
   cloneable side route (`AuxiliaryInference`, `crates/driver/src/headless/kernel.rs`) that pins
   the session's current registry publication and honours its account pins, or rides its
   connected gateway. One call that recalls evidence is exactly one tool-free `chat_on` request
   on the catalog's `memory` role (`@memory`: the configured memory selectors, then `@commit`, then
   `@smol`); a call that recalls nothing makes none, a failed or textless answer is
   the `Synthesis` fault, and an unbound bridge still answers with the evidence.
3. With inference declared, the agent loop's `tool_cancellation` (`crates/agent/src/loop.rs`)
   gives a `reflect` call a foreground scope instead of a read-only one, which the dispatcher
   schedules exclusively: it never runs at the same time as another call of its batch, where it
   used to run beside them. Its stop request is the same turn interrupt as before, and the tool
   still answers it at once.
4. An attached session's `reflect` runs on the project daemon, directly or through `dyn` in its
   shell. The daemon recalls the evidence and relays the synthesis to the connection that issued
   the call (`crates/envd/src/reflection_relay.rs`): a `ReflectionQuery` on that request's id only,
   to a connection that advertised `reflection-relay`, answered by the attached session through
   the bridge this amendment binds; an interrupted call withdraws it and a closed connection makes
   it unavailable, so `reflect` falls back to the evidence and never invents an answer.
   Not covered: `MnemopiSettings.llm_mode` is not consulted; reflection always uses `@memory`.

## Amendment (2026-10-08, fetch tier)

Read-only network egress had no honest declaration: a tool that only reads a remote resource
(a URL, an `ssh://` file, a forge issue) could declare nothing, which is `read` and hides the
egress, or declare `exec.network`, which is `exec` and prompts for every URL under the default
posture once a confinement marker applies. The owner chose a distinct class.

1. `omp_tool::FetchEffects { credentials }` is a new effect domain (`Effects::fetch`); present
   means a fetch is permitted, so it is never empty, and it never mutates the environment. A
   requested fetch narrows a declared one only when it asks for no credentials the maximum
   withholds. On the wire it is `omp.policy.v1.FetchEffects`, `EffectEnvelope.fetch = 6`
   (`SCHEMA_REV` 22).
2. `ApprovalTier::Fetch` sits between `Read` and `Write`, so the mode table needs no new rule:
   `always-ask` prompts for a fetch, while `write`, the default posture and `yolo` allow it. A
   fetch beside a write or an exec effect takes the higher tier. It is not a write, so plan mode's
   write boundary does not refuse it.
3. A worker whose envelope fetches gets the `env.net` data grant and nothing that executes or
   writes. MCP control declarations accept the `fetch` tier (read-only egress with the server's
   credentials), and the Python API mirrors it (`omp.FetchEffects`, `Effects.fetch`,
   `Tier.FETCH`).
4. Not yet: no tool declares a fetch. `read@3`, `grep`, `glob` and `ast_grep` adopt it with
   argument-scoped invocation effects in later changes, so a local read stays `read`. (`read@3`
   declares its fetches since the 2026-10-08 read fetch amendment, `grep`, `glob` and `ast_grep`
   since the 2026-10-09 search fetch amendment.)

## Amendment (2026-10-08, undeclared effects)

The owner decided that a tool which declares no effects can no longer resolve to the auto-approved
`read` tier. An empty envelope was the default for every host that registers tools it did not
write: RPC host tools (`set_host_tools`), Python worker and frozen extension tools, and eval-defined
tools forwarded into workpool children. Each of them could run anything its host process could and
was admitted as if it only read.

1. `omp_tool::Effects::unknown()` is the ceiling of what is not declared: any command (`*`) with
   the network, the `exec` tier. A declared envelope, even an empty one, always replaces it, so an
   effect-free tool must say so to stay `read`.
2. RPC host tools. `HostToolDefinition` (`crates/rpc/src/protocol.rs`) carries an optional typed
   `effects` (`HostToolEffects`, mirroring `omp_tool::Effects` in camelCase, unknown fields
   refused); `crates/app/src/rpc_mode.rs` lowers it into `HostToolSpec::effects`, and
   `Registry::replace_host_tools` (`crates/tool/src/registry.rs`) registers an absent one as
   `Effects::unknown()`. `host_tool_specs` round-trips the envelope. The kernel dispatcher, which
   used to admit every name with no native live spec with an empty envelope, now admits a host tool
   with its registered envelope, and a name that resolves nowhere with `Effects::unknown()`
   (`crates/agent/src/dispatch.rs`).
3. Python workers and frozen extension tools. With no envelope, a tool gets its host's ceiling,
   `HostKey::undeclared_effects` (`crates/envd/src/worker.rs`): a `sandboxed` extension host,
   whose process can only read and write the workspace, gives document reads and `**` writes (the
   `write` tier); a `trusted` host, and any tier the build does not recognize, gives
   `Effects::unknown()`. The same ceiling is the registry spec (`worker_spec`,
   `crates/envd/src/tools.rs`), the extension host route's maximum, against which `ArgsCommitted`
   narrowing and the DATA grants are checked, and the tier the CONTROL snapshot reports to
   `omp.tier_of` (`device_snapshot_tier`), so the three cannot disagree. `FrozenTool.effects`
   (`crates/envd/src/exthost/extensions.rs`) is typed as `Option<omp_tool::Effects>`. For a
   trusted host the widened route maximum grants nothing the host process could not already do;
   for a sandboxed one it is exactly what its sandbox permits.
4. Eval-defined tools forwarded into workpool children (`crates/driver/src/subagent/workpool_runtime.rs`)
   declare no envelope and register `Effects::unknown()`.
5. `debug@2` declares `network: true`: a configured remote adapter is reached over TCP.
6. Consequences. Under the shipped default posture, `write` and `always-ask`, an undeclared RPC host
   tool, eval-defined tool or trusted extension tool now prompts before each call, and envd's write
   boundary refuses an undeclared worker tool under plan mode and in a read-only subagent. An
   undeclared sandboxed extension tool is `write` tier: allowed under `write` and the default
   posture, prompted under `always-ask`. Authors of extensions and RPC hosts should declare their
   envelopes; an explicit `yolo`, or `tools.approval.<name> allow`, still admits without prompting.
7. Not covered. Prelude helpers (`crates/envd/src/tools.rs`) keep their own handling. Native Rust
   extensions (`ExtensionRegistrar::tool_spec`) register a full `ToolSpec` and are unchanged, as
   are MCP servers at the `read` tier and the other under-declared host tools item 6 of the typed
   confinement marker amendment lists.

## Amendment (2026-10-08, argument-scoped invocation effects)

A revision's declared effects are a ceiling over every call, so a tool that can do more than most
of its calls do was judged by its worst call: `computer` was `exec` for a screenshot, and a
`read@3` that declared its fetches honestly would have prompted for a local file. The owner chose
effects judged per call, from the call's arguments.

1. `Tool::invocation_effects(&Params)` returns the effects one call can have, declared together
   with `Tool::ARGUMENT_SCOPED_EFFECTS`. The registry decodes the arguments only for a tool that
   declares it (`Registry::invocation_effects`, `Registry::scopes_invocation_effects`), with
   `omp_tool::decode_params`, the decoder `IncomingParams::whole` applies to the executor's
   finalized arguments; `decode_params` refuses any document that is not one JSON object, which
   serde would otherwise decode into a derived struct in sequence form. The executor's
   finalization is not repeated, and the two differ only where it is safe: arguments that do not
   decode strictly (malformed JSON the executor would repair, an argument-spec alias or coercion
   it would canonicalize) keep the declared maximum, and a key given twice decodes last-wins while
   the executor's finalization refuses it. No override, or arguments that do not decode, keep the
   declared maximum, as every tool that does not declare it keeps today's behaviour. An envelope
   that is not a subset of the declared maximum is refused
   (`RegistryError::InvocationEffectsExceedMaximum`), never replaced by the maximum.
2. The environment judges the call at `ArgsCommitted`. A declaring tool's gate
   (`AdmissionGate::argument_scoped`) waits for the committed arguments: when its maximum would be
   allowed or denied every narrower call resolves the same way, so the policy is fixed at
   `InvokeTool`; when it would prompt, the policy stays pending and nothing the call streams
   reaches the executor. The gate first stages the committed arguments (`AdmissionGate::stage`,
   one JSON object); only staged arguments are judged, and a pending policy resolves only from
   them (`AdmissionGate::resolve_pending`), so a commit the gate refuses leaves the call unjudged,
   still pending and still withheld, and the next commit it accepts is judged on its own
   arguments. The staged call's envelope resolves the policy, is the one the plan-mode and
   read-only write boundary judges (`InvocationExecutionPolicy::denial`) and the one the client's
   narrowing starts from; only then is the query emitted (`AdmissionGate::emit`). An admission
   answer that rewrites the arguments (`rewritten` on `AdmissionDecision::Allowed`) is judged
   again, and a rewritten call that needs more than was admitted is denied
   (`admission_effects_widened`); an answer without a patch keeps the envelope the call was judged
   under, so its arguments are decoded once.
3. The kernel's native admission (`ToolAdmission::admit`, `crates/agent/src/dispatch.rs`) receives
   the call's envelope; a call judged beyond its maximum is skipped before admission is asked.
4. First adopter: a `read_only` `computer` program keeps capture, accessibility and clipboard reads
   and loses desktop input, which its session host refuses before each such operation
   (`refuse_input_in_read_only`, `crates/envd/src/computer.rs`). A clipboard read is declared
   desktop authority (`DesktopEffects::clipboard`, `omp.policy.v1.DesktopEffects.clipboard = 4`,
   `SCHEMA_REV` 23); a clipboard write is `input`. The call's tier does not fall: how desktop reads
   should be approved is an open owner question, and until it is answered
   `ApprovalTier::from_effects` ranks every desktop authority, reads included, as `exec`, the tier
   every `computer` call had before. So the narrowing changes no prompt in any mode today; it
   makes the envelope honest and is the one place the owner's answer would change.
5. Not yet: the batch scheduler still classifies a call's cancellation from the declared maximum
   (`tool_cancellation`, `crates/agent/src/loop.rs`), so a `read_only` `computer` call stays
   foreground and batch-exclusive, and a nested `dyn` device call is still admitted on its target's
   declared maximum (`DynamicAdmission`, `crates/envd/src/devices_host.rs`).

## Amendment (2026-10-08, host-keyed fetch subjects)

A prompt's subject was the tool name, and a `session` grant matches kind and subject, so one
`session` answer to a fetch would have silenced every later call of that tool, whatever it
reached. The owner chose: under `always-ask` a fetch is asked once per host per session.

1. The environment names the hosts. `Tool::fetch_locators(&Params)`, consulted with
   `Tool::invocation_effects` for a tool that declares `ARGUMENT_SCOPED_EFFECTS`, returns the
   URLs a call fetches (`Registry::fetch_locators`). When the committed call's envelope fetches,
   `ConnectionState::scope_committed` names the host each locator reaches with the resolver that
   performs it (`crates/envd/src/fetch_host.rs`): an http(s) URL by its authored host and port,
   `issue://` and `pr://` by the GitHub host the URL or the workspace's git remote names
   (`GithubResolver::fetch_host`), `ssh://` by its alias, `mcp://` by the mounted server
   advertising the resource. Redirects the client follows and site readers that re-target a fetch
   reach hosts known only once it runs; the authored host's grant covers them. A locator the
   environment cannot name (`resolve_locators`) marks the call partly unnamed and never drops the
   hosts the others name, so a locator that cannot be named never turns a named host into the
   tool's own fetch.
2. `env.v1 AdmitInvocation` reports the envelope the call was judged by (`effects = 4`), every
   distinct named host (`repeated omp.policy.v1.FetchTarget fetch = 5`, a typed resolver, host
   and port, not the shell's `NetRef`) and whether some fetch reaches a host left unnamed
   (`fetch_unnamed = 6`), `SCHEMA_REV` 25.
3. The session asks one `network` requirement (Python's `ApprovalKind.NETWORK`) per distinct
   named host, subject `http:<host>:<port>`, `github:<host>`, `ssh:<alias>` or `mcp:<server>`,
   and `tool:<name>` for the unnamed remainder (a locator left unnamed, a malformed target, or no
   named host at all) beside them; the tool itself is asked as well only when its tier is above
   the fetch (`admission_specs` in `crates/driver/src/headless/kernel.rs`). Every requirement
   offers `once` and `session`. A grant covers a later prompt only when each of its requirements
   is granted, each by any session (or persisted) grant of the same kind and subject
   (`session_grant` in `crates/agent/src/approvals.rs`): hosts granted one prompt at a time
   answer a fetch reaching all of them, while a grant for one host never covers another, nor the
   tool, and a grant for the tool's own unnamed fetch never covers a host named beside it. The
   native tools the kernel runs itself (every environment tool of an embedded fallback or an
   isolated composition, an attached session's session tools) are admitted by
   `SettingsAdmission` the same way: the dispatcher hands it the call's fetch locators
   (`ToolAdmission::admit`) and it names their hosts with the in-process environment's resolvers
   (`ProjectEnvironment::fetch_hosts`, `FetchHostNamer`). ACP presents a `network` requirement as
   a `fetch` tool call, and a permission request with several requirements names every subject
   in its title and content and carries them all in `rawInput.requirements`, since
   `allow_always` grants them all for the session; the RPC `tool_approval_request` carries every
   requirement in `requirements`.
4. A nested `dyn` prompt offers `once` and `session` and is keyed the same way: an MCP target's
   fetch is asked per server (`mcp:<server>`), any other target's as its own. (Since the
   2026-10-09 search fetch amendment a device target is judged by its arguments and names the
   hosts its locators reach, as a slot's call is.)
5. Not yet: no built-in tool names fetch locators; `read@3`, `grep`, `glob` and `ast_grep`
   adopt them with their per-target classification. (`read@3` names them since the 2026-10-08
   read fetch amendment, `grep`, `glob` and `ast_grep` since the 2026-10-09 search fetch
   amendment.)

## Amendment (2026-10-08, policy-keyed project daemons)

A project daemon compiles its command sandbox, its egress broker and its approval posture from
the control context it starts under and keeps them. Its environment socket was keyed only by the
project and the executable build, so a later session whose own configuration differed attached
without a word and its commands ran under the posture of whoever started the daemon. Observed on
Seatbelt: a daemon spawned under the shipped `workspace-write` kept serving a session whose
configuration set `sv_sandbox_mode off`; `bash` ran confined and was auto-approved as `yolo`
while the session itself reported the sandbox off. The reverse runs a session that expects the
sandbox unconfined. The owner decided that a session never runs its tools on a daemon whose
sandbox and approval policy differs from its own.

1. `daemon_policy::from_con` (`crates/envd/src/daemon_policy.rs`) digests what a daemon fixes at
   start and what decides how a command is confined, wrapped or admitted: every `sv_sandbox_*`
   setting (`SandboxSettings`, with the network mode's provenance and whether the user set any of
   them), `sv_tools_approval_mode` with its provenance (`ConfiguredApproval`), the per-tool
   `sv_tools_approval` overrides, the `sv_shell_command_prefix` wrapper the daemon's shell puts
   before every command (a user may make it a confinement layer of their own), and
   `sv_fetch_enabled`, which decides whether the daemon's `read` may fetch URLs. The result is a
   domain-separated SHA-256 `DaemonPolicy` (`omp_env::project_state`). The per-connection
   approval-mode override is not part of it, since each connection sends its own in
   `ClientHello.approval_mode`. The rest of what a daemon fixes at start is left out on purpose,
   so it never splits sessions across daemons: edit, read and grep limits, timeouts, direnv, the
   browser it drives and the memory backend change what a tool does, not how a command is
   confined or admitted or whether a tool reaches the network by itself. The tool roster is left
   out too, because an attached session composes its registry from its own context
   (`EnvServer::open_session_host`).
2. The environment socket is keyed by that policy as well as the build
   (`environment_socket(state_dir, policy)`: `/tmp/omp-<uid>-<state>-<build>-<key>-env.sock`, or a
   per-policy named pipe on Windows). Sessions whose configuration differs therefore reach daemons
   of their own. Each idle-exits on its own, and all of them share the project's one document
   authority, whose socket stays build- and policy-stable. `omp envd` without `--socket` binds the
   socket its own policy keys. The `<key>` is not the policy digest: the digest covers values such
   as injected environment variables, hosts and paths, and other local users can list `/tmp` and
   confirm a guess against it. The name carries a SHA-256 of the digest under a random 32-byte key
   private to the project (`env-socket.key` in the state directory, mode 0600, published with a
   no-replace hard link so concurrent first uses agree on one key; a damaged key is replaced).
3. Every `ServerHello` reports the policy its server enforces (`bytes policy_digest = 9`,
   `SCHEMA_REV` 26). A client checks it on the owner connection, on the attached session's
   connection (the partitioned transport exposes the daemon's hello) and when it registers
   presence. It refuses a daemon that reports another policy with
   `EnvdError::DaemonPolicyMismatch`, which names the socket and the short digests of both
   policies, and one that reports none with `EnvdError::DaemonPolicyUnreported`. A refusal on the
   owner connection makes `ProjectEnvironment::attach` run an embedded environment under the
   session's own policy, with the refusal in `fallback_notice` and a WARN log line. A refusal on
   the session connection fails the attach instead, so no tool runs: by then the session host has
   bound the composition's tool factories, which bind once. It needs another daemon to take the
   socket between the two connections, while the open owner connection keeps the first one
   serving it.
4. A session does not spawn a daemon it could never join. `omp envd` resolves its control context
   from the configuration files alone (`config.cfg` and the project overlay, under the session's
   profile, which a spawn forwards as `OMP_PROFILE`), so a session whose policy comes from settings
   made in its own process (`--add-dir` roots, a cfg script run by `exec`, an agent class, the
   compress host's unconfigured context) would only start a daemon of another policy. The driver
   resolves the policy such a daemon would enforce (`CfgFiles::daemon_policy`) and passes it as
   `AttachOptions::spawn_policy`; when it differs, `attach_owner` spawns nothing and the session
   runs embedded at once (`EnvdError::DaemonPolicyNotSpawnable`, naming both policies). A daemon
   that was spawned and still reports another policy is stopped, with a WARN line
   (`spawn_project_daemon_with`). Either way the session runs embedded, never on a daemon that
   enforces someone else's policy.
5. A resumed child presents its agent class before its environment starts. `compose_kernel`
   attaches the environment before it opens the session journal, so before this amendment a child
   resumed with `--resume` composed its environment from the main configuration while the console
   presented the class (found in review, by reading): under a class that sets
   `sv_sandbox_mode read-only`, the session joined the main configuration's daemon, where `bash`
   may write the workspace. `con_journal::present_journaled_class` now reads the journaled class
   first and adopts it into the context the environment is composed from, and the composition
   refuses to continue (`HeadlessError::EnvironmentPolicyDrift`) if the session it then opens
   presents another policy.
6. Not yet: a session does not hand its in-process settings to a daemon it spawns, so such a
   session always runs embedded. A policy is fixed when an environment starts: a setting changed
   while a session runs does not move the session to another daemon, and a chat that switches to
   a child session from inside (`/resume` from the main chat) presents the child's class on the
   console while its environment keeps the policy it was composed under, embedded or attached
   alike. When such a switch moves the console's policy away from the environment's,
   `ConJournal::resync` says so once on the console, naming both policies and `omp --resume` as
   the way to run the session under its own.

## Amendment (2026-10-08, disabled-network diag)

Under `sv_sandbox_network_mode disabled`, `curl -sI --max-time 20 https://example.com` failed
silently (exit 6, no output) and the session got no `sandbox` diag beyond the session note: the
model could not tell the sandbox from an outage. Point 7 of the 2026-10-07 amendment reports a
broker refusal, and a resolver or connection-failure marker in stderr, but a disabled network
has no broker and a quiet client prints no marker.

1. No backend gives a real signal. Seatbelt's `(deny network*)` and its denied DNS mach services
   make `connect` and `getaddrinfo` fail inside the child, and the violation goes only to the
   unified log, which no command can afford to read. Bubblewrap's empty network namespace
   records nothing either. Only the `scoped` broker sees what a client asked for. Running a
   refuse-all broker under `disabled` to get that record would change what `disabled` means (a
   loopback listener the profile admits, proxy variables in every child) and is left to the
   owner.
2. The bounded rule is to observe what the command hands the programs it launches. The
   in-process shell gives the spawn wrapper each external launch it composes
   (`SpawnWrapper::observe_launch`, called by `compose_std_command` in `crates/shell`, before
   the launcher prefix): its expanded arguments, and a resolver for its program (a path made
   absolute against the shell's working directory, and a bare name such as `exec curl` as
   found on the shell's `PATH`). The program is resolved at most once per launch, and only
   when something asks: the path policy's read check, which every sandboxed launch meets, and
   the wrapper, which shares that resolution. A session attempt whose network is disabled
   records whether one of the arguments is a network URL handed to a program that can run
   (`AttemptFacts::network_locator`). An argument counts only when the whole argument is
   `scheme://host…` with any scheme but `file`, a loopback host included because a disabled
   network cuts off the host's loopback too. Only once a URL is found is the program asked for
   and judged, in the sandbox's own view (`FilePolicy::launchable`). The session's file policy
   must admit its read, judged directly so that a refusal records no denial. Then, as the
   shell's path search judges it, the program must be found, not a directory, and executable
   by the user. A missing program, or one without its exec bit, never ran, whether the shell
   fails to spawn it (127, 126) or a launcher (`sandbox-exec`, `bwrap`) fails to exec it after
   its own spawn succeeded, so a check after the spawn could not tell. Nor did a program the
   session's file policy hides. The shell reads every program it launches through that policy
   before composing the launch, a simple command and `exec` alike, so such a launch ends in a
   read denial and is never observed; `launchable` repeats the read so the fact holds without
   relying on that order. Until the review follow-up (2026-10-09) `exec` skipped that check,
   so on Seatbelt it ran a native binary behind a `read_deny` root, and the diag would have
   counted it. The backend is no substitute for the check. Bubblewrap masks a `read_deny`
   root, but Seatbelt's `read_deny` refuses only reads and its profile allows
   `process-exec*`: an interpreter cannot read a hidden script there, yet a hidden native
   binary runs when something other than the shell launches it, a utility builtin that builds
   its own command (`xargs`, `find -exec`, `sort --compress-program`, `ifne`) or another
   program (`env /hidden/tool`). That gap stays open. Denying `process-exec*` under each
   `read_deny` root in the Seatbelt profile would close it (checked by hand with
   `sandbox-exec` and a compiled binary, not yet in the profile). A failed command with the
   locator fact and no refusal gets the mode's locator text (`disabled`, or the broker that
   could not start), as a `warn`. That text says the command failed after running a program
   given a network URL, which the sandbox may cause, and names the remedy; it does not quote
   the URL. It shares the once-per-session slot of the marker's generic text, which outranks
   it, so a session hears one of the two once, whichever sign comes first. A success, a
   cancellation and a failure without such an argument report nothing.
3. `scoped` is unchanged. Its attempts do not scan arguments, and a locator has no text there,
   because the broker records what a proxy-aware client asked for, and a URL says nothing about
   why a command whose client could reach the broker failed. A quiet client that bypasses the
   proxy (`curl -s --noproxy '*' URL`, or any program that ignores `HTTP_PROXY` and reports
   nothing when it cannot connect) never reaches the broker, so under `scoped` it still fails
   with no refusal and no marker, and gets no diag; that silent-failure class stays open there.
4. Accepted costs. A program given a URL that fails for another reason (an HTTP error status
   with `-f`, a usage error) spends the slot on a hedged diag, at most once per session. Still
   uncovered: builtins never launch a program, so `rg https://…` does not count (correctly);
   a URL inside a longer argument (`--url=…`, a message, a `python -c` script), read from a
   file, stdin or configuration (`curl -K`, a script that launches its own client), a
   scheme-less host (`curl -s example.com`, `wget -q host/path`) or a raw address
   (`nc -z 1.1.1.1 443`) leaves no locator. The rule keys on the command's final status, so
   any command whose final status is 0 after a quiet client failed reports nothing. A
   pipeline whose final stage succeeds (`curl -s URL | head`, `curl -s URL | jq .`) is one
   example; others are `body=$(curl -s URL); echo "$body"`, `curl -s URL -o f; cat f`,
   `curl -s URL || true` and `if curl -s URL; then …; else echo down; fi`. Per-stage status
   (`PIPESTATUS`), or the exit status of each launch keyed to the program that got the URL,
   would be the signal for closing that gap later. The utility builtins that build their own
   commands (`xargs`, `find -exec`, `sort --compress-program`, `ifne`) bypass
   `compose_std_command`, so their children are not observed. Eval cells and detached
   processes still get no network diag.

## Amendment (2026-10-08, read fetch)

`read@3` declared document reads only, while a URL read fetches through the environment host's own
HTTP client, `ssh://` opens a session with a configured key, `issue://` and `pr://` call the GitHub
API with stored credentials, and `mcp://` asks a mounted server. With the fetch tier, argument-scoped
invocation effects and host-keyed fetch subjects in place, it now declares each fetch per call.

1. One classification. `classify_target` (`crates/tools/src/read.rs`) routes a canonical target
   (`normalize_read_target`: outer quotes, the `@` shorthand, `file://` URLs and shell escapes
   repaired) to a `TargetClass`: `Web` (an http(s) URL, a `www.` host or a bare `host:port/`),
   `File`, `Internal` (a built-in scheme's resolver), `Foreign` (the unknown-scheme fallback) or
   `Local`. `ReadTool::execute_target` dispatches on that class, and the call's effects are derived
   from it, so the two cannot diverge. Because the executor's split of a path depends on what exists
   locally, a call is judged by every spelling it may run: the whole text, a JSON path array's
   members, and the members of a `;` or `,` list.
2. Per target. A `Web` target is an anonymous fetch while `tools.fetch.enabled` allows it (the
   executor refuses it otherwise, before fetching). An `Internal` or `Foreign` target fetches as its
   resolver reports (`Resolve::read_fetch`, `ResolverTable::read_fetch` and
   `ResolverTable::read_unknown_fetch` in `crates/tools/src/read/resolver.rs`; the production union
   `UrlResolver::read_fetch` in `crates/envd/src/tool_url.rs`): `ssh://` with credentials for a
   resource naming one host alias, `issue://` and `pr://` with credentials, and `mcp://` by its
   server's mount (`McpManager::resource_read_pin`): a remote (`http`, `sse`) server always, a
   local (`stdio`) one only at a tier whose effects reach the network (`fetch`, `exec`,
   `privileged`), and a resource no mounted server advertises yet always, since a remote server may
   advertise it by the time the read runs. Which server advertises a resource is live state that
   can change between the judgment at `ArgsCommitted` and the read (under `always-ask`, across the
   user's whole prompt), so the judgment pins it: a dispatcher that judges a call and runs it (the
   environment's `ConnectionState::scope_committed` and `spawn_native_invocation`, the kernel's
   `Dispatcher` for the native tools it runs) gives the call one `InvocationPins`
   (`crates/tool/src/pins.rs`), judges it inside them and runs its executor inside them. The MCP
   resolver resolves the server and its fetch once per call there, so the envelope, the host its
   approval names and the server its read asks are one resolution; the read asks only that server
   and is refused, never sent elsewhere, when that server is no longer mounted or reading it now
   reaches the network beyond the judged fetch (`McpManager::admit_pinned_read`). A read judged
   before any server advertised the resource was judged as a credentialed fetch whose approval names
   no server, so it asks whichever server advertises it when it runs. Every other resolver reads
   local or environment-owned state, and a scheme outside the built-in vocabulary is served by the
   attached RPC host from the resources it declared, which the environment does not fetch. The
   fetching targets are the call's locators, in canonical spelling, so the subjects of the
   host-keyed fetch subjects amendment apply: under `always-ask` a read is asked once per host per
   session.
3. The maximum. `ReadPolicy` gains `credentialed_fetch`: the deployment registers resolvers that
   fetch with stored credentials. `read@3`'s declared maximum is document reads plus a fetch,
   credentialed while `credentialed_fetch` holds and anonymous while only `fetch_enabled` does,
   absent when neither does. Production registers `ssh://`, `issue://`, `pr://` and `mcp://`
   unconditionally, so `production_read_policy` (`crates/envd/src/tools.rs`), from which both the
   environment's declaration and its executor are built, always sets it; `sv_fetch_enabled` only
   removes the anonymous URL fetch. A call a resolver judges beyond the maximum is refused
   (`RegistryError::InvocationEffectsExceedMaximum`), never admitted on it.
4. Environment-ambient processes. Language servers (started at warm-up and on any document open, a
   `read` included), debug adapters (chosen by the DAP configuration and started only by `debug`,
   whose envelope declares it), the Obsidian CLI (a vault read with `?op=read` or `?op=search`, the
   active vault `_`, discovery of an unconfigured vault; run only while `sv_vault_enabled` resolves
   it) and a local MCP server at the `read` or `write` tier are processes the environment runs
   under configuration it trusts, not per call. No `read` declares them: an `exec` in its maximum
   would refuse every `read` under plan mode and make it batch-exclusive (`tool_cancellation`), and
   a lexical classifier cannot see when the vault discovery runs.
5. The instrumenting wrapper the environment registers its tools through (`InstrumentedTool`,
   `crates/envd/src/tools.rs`) forwarded neither `Tool::ARGUMENT_SCOPED_EFFECTS` nor
   `Tool::invocation_effects` and `Tool::fetch_locators`, so no production registration was ever
   judged by its arguments: `computer`'s `read_only` narrowing of the argument-scoped amendment
   never reached it. It forwards all three now.
6. Consequences. A local read is `read` tier in every mode and sandbox state and never asks. Under
   `always-ask` a URL, `ssh://`, `issue://`, `pr://` or remote `mcp://` read asks once per host per
   session, and its gate withholds the streamed arguments until the call is judged; under `write`,
   the default posture and `yolo` it runs unasked, as before. A fetch is not a write, so plan mode
   and read-only subagents still admit it, and it is not mutating, so a `read` still runs beside the
   other calls of its batch.
7. Not covered. `grep`, `glob` and `ast_grep` still declare document reads only while they reach
   the same remote roots; they adopt the classification next (done: the 2026-10-09 search fetch
   amendment). Whether a read the attached RPC host
   serves from its own declared resources should count as that host's authority (it is a document
   read here) is an open owner question. The wrapper still drops `Tool::stream_match_text`,
   `Tool::projection` and `Tool::authorize_visibility` of the tools it instruments.

## Amendment (2026-10-09, search fetch)

`grep@1`, `glob@1` and `ast_grep@3` declared document reads only, while they reach the remote roots
`read@3` declares since the read fetch amendment: `grep` reads an http(s) root through the
environment's web reader and every other URI root through the resolver `read` uses
(`materialize_internal_root`, `crates/envd/src/tool_search.rs`), `glob` walks `ssh://` hosts by
listing them (`resource_glob`), and `ast_grep` fetches an http(s) root. `grep` and `ast_grep` also
fetched URL roots whatever `tools.fetch.enabled` said, which only `read` honoured.

1. One classification per tool, the one its executor routes by. A `grep` root keeps its
   `SearchRootKind` (`parse_root`, `crates/tools/src/grep.rs`): a `Url` root is an anonymous fetch,
   an `Internal` root fetches as the resolver the workspace reads it through reports
   (`WorkspaceSearch::internal_root_fetch`, which the environment answers with
   `ResolverTable::read_fetch`, the judgment `read@3` uses), and `Filesystem` and `Archive` roots
   are document reads. An `ast_grep` root is classified by `classify_root`
   (`crates/tools/src/ast_grep.rs`), which `AstSearchAuthority` (`crates/envd/src/tool_ast_grep.rs`)
   now dispatches on: only a `Web` root fetches, since an internal root is searched at the local
   path its resolver materializes and `ssh://`, vault and `mcp://` resources have none. A `glob`
   path fetches only when the executor routes it to the resource walk; the environment judges each
   of its targets by the routing `resource_glob` takes (`resource_walk_target`, `walk_base`) and
   `Resolve::walk_fetch`, which only `ssh://` answers, with credentials, for every walk: a walk
   from the alias list descends into every configured host. Neither `grep` nor `ast_grep` reuses
   `read@3`'s `classify_target`, because they search a `www.` host or a bare `host:port/` as a
   workspace path, which `read` would fetch: reusing it would judge a local search as a fetch.
2. The maximum. `SearchPolicy` (`crates/tools/src/grep.rs`) mirrors `ReadPolicy`'s two fetch
   fields. `grep`'s maximum holds a credentialed fetch while the deployment registers credentialed
   resolvers and an anonymous one while only `fetch_enabled` does; `glob`'s a credentialed one with
   those resolvers and never an anonymous one, since it fetches no URL; `ast_grep`'s an anonymous
   one while `fetch_enabled` holds. `production_search_policy` (`crates/envd/src/tools.rs`), from
   which both the declarations and the executors are built, always sets `credentialed_fetch`. With
   `sv_fetch_enabled false`, `grep` and `ast_grep` refuse a URL root before anything is fetched,
   each with the same typed case naming the root (`grep::Fault::UrlRootDisabled`,
   `ast_grep::Fault::UrlRootDisabled`), and no URL root is judged to fetch. `ast_grep::Fault` is
   journaled untagged, so its other faults keep their recorded `{"message": …}` shape as
   `Fault::Diagnostic`. A call judged beyond the maximum is refused, never admitted on it.
3. Locators. `grep` names each fetching root in its selector-peeled spelling, `ast_grep` each URL
   root as authored, and `glob` each remote walk by the resource it descends from
   (`ssh://prod/src` for `ssh://prod/src/**/*.rs`, `ssh://` for a walk from the alias list), so
   the host-keyed fetch subjects apply: under `always-ask` a search is asked once per host per
   session, and a walk of every configured host is asked as the tool's own fetch.
4. Devices. `ast_grep` is presented as a device under the environment's `Auto` tools policy, so
   it is reached through the shell's `dyn` builtin, whose admission (`DynHost::call_issued`,
   `crates/envd/src/devices_host.rs`) took each target's declared maximum: once that maximum
   holds a fetch, every `dyn ast_grep` would have been a fetch, and asked under `always-ask`, a
   local search included. A device call is now judged by its arguments on the claimant and revision
   its path resolves to (`Registry::device_invocation_effects`, `Registry::device_fetch_locators`),
   inside the pins its executor then runs in, and `DynamicAdmission::admit` takes the
   `NamedFetches` the environment's resolvers name for its locators, the unnamed remainder asked as
   the target's own. A device call judged beyond its maximum is refused before anyone is asked
   (`DynamicAdmissionError::Unjudged`), rendered as the registry's refusal naming the revision and
   the reason, as a slot call's refusal is. This also brings `computer`'s `read_only` narrowing to
   its `dyn` calls; its tier stays `exec`, since desktop capture and accessibility reads are
   `exec`.
5. Consequences. A search of local roots is `read` tier in every mode and sandbox state and never
   asks, as a slot call and as a `dyn` call. Under `always-ask` a `grep` of a URL, `ssh://`,
   `issue://`, `pr://` or remote `mcp://` root, a `glob` over `ssh://` and an `ast_grep` of a URL
   ask once per host per session; under `write`, the default posture and `yolo` they run unasked,
   as before. Plan mode and read-only subagents still admit them, and they stay non-mutating in a
   batch.

## Amendment (2026-10-09, attached posture notice)

The `approval-posture` notice (point 5 of the 2026-10-06 amendment) was posted only by the
kernel's own admission of the native tools its process runs. A session attached to the project
daemon runs every environment tool there, and the daemon admits those calls, so the kernel
admitted none of them and the notice never appeared. Observed live with `omp print` and
`sv_sandbox_mode off`: neither the defaulted `yolo` downgraded to `write` nor an explicit
`--approval-mode yolo` running unconfined was reported, in the journal or on any host surface,
although `bash` prompted and was refused, or ran unconfined, as the posture decided. This
amendment makes the implementation say what point 5 decided, about the sandbox the calls are
actually admitted and confined under; the only other change is the state that `ServerHello`
now reports (point 3).

1. The report is one per session, wherever a call is admitted. `install_tool_authority` resolves it
   (`session_posture`, `PostureNotice`, `crates/driver/src/headless/kernel.rs`) and installs the
   `SettingsAdmission` and the `EnvToolExecutor` on the kernel with the same report; nothing else
   wires it, so an admission or executor installed on its own reports nothing. The executor posts
   it before it commits the arguments of a call the environment admits. `compose_kernel` installs
   both through it, so the first admission of any tool reports it, whether the kernel admits the
   call (an embedded or isolated composition's native tools, an attached session's session-process
   tools) or the environment does (every environment tool of an attached session, worker tools in
   any composition), and no later admission repeats it. Session tools are trusted host code that
   nothing admits.
2. The journal decides whether the report is due: it is due while the live branch of the session
   the kernel serves holds no `approval-posture` notice of this posture. `install_tool_authority`
   decides it from the session the kernel is composed over, so a session resumed by a later
   process that already holds the notice is not told again (one holding a notice of another
   posture is). A kernel outlives the session it serves and a rewind can drop the turn holding
   the notice (a tool-tail retry rewinds to the call's authorization, before it), so the report
   is a session observer (`Kernel::with_session_observer`) and decides again from the session it
   is handed on every rewind and every switch: `SessionObserver::rewound` and `switched` now
   receive the session the kernel serves afterwards, and `Kernel::session_switched` takes the
   next session. A rewind that keeps the notice, or a switch back to a session already told,
   adds nothing; a rewind or retry that drops it, or a switch to a session never told, reports it
   at the next admission. A notice still queued on the kernel mailbox when a rewind or switch
   lands is not yet journaled and does not count.
3. The posture is the session's configured approval mode (its `--approval-mode` override
   included) against the sandbox state the environment reports in its handshake, not the one
   the session probes: `ServerHello` carries the state the server's own probe found, the one
   every admission on the connection resolves against (`SandboxState sandbox_state = 11`,
   `SandboxState::to_wire`/`from_wire` in `crates/envd/src/admission.rs`, filled by
   `accept_hello` in `crates/envd/src/server.rs`, `SCHEMA_REV` 27; tag 10 stays planned for
   the sandbox capabilities of `docs/py/06-policy.md`), and `EnvToolExecutor` reads it from the
   handshake its client completed. Equal policies digest equal settings, not equal probes: a
   project daemon probed its host once, when it started, so a Linux daemon started before
   `bwrap` or user namespaces were available keeps downgrading a defaulted `yolo` that a later
   session's own probe would keep, and a session inside an outer sandbox fails a Seatbelt probe
   that its daemon passed. The session reports the daemon's state in both cases, which is what
   its environment tools run under. Its own probe counts only when the environment reported no
   state; an embedded or isolated environment probes the same settings in the session's
   process. The kernel's own admission of the native tools it runs keeps the session's probe:
   over an attached daemon those are all `Host` tools, which no sandbox state changes.
4. Like every typed notice, it is journaled under the turn as `<notice kind=warn
   name=approval-posture>` and read from there by the host projections (the RPC and print JSON
   `notice` frames, which name it as their `source`, `transcript_json`, and the chat view). No
   `<notice>` is part of the model's projection (`omp_session::project_thread`), this one
   included, on either path, so in a headless `omp print` the model sees the refusals of a
   downgraded default `yolo` but not why. Whether the model should be told is an open owner
   decision this amendment does not make; the posture proofs pin the current projection.

## Status in omp

**Status: Implemented.** Parser, interpreter and coreutils run in process with persistent state, and approval is the sandbox-denial-and-rerun model in the amended decision. Limits: the denial-and-rerun prompt exists only while a sandbox is constructed, and a rerun can repeat side effects. The default `yolo` holds only inside an active sandbox, an explicit one is respected (2026-10-06 amendment). The network is `scoped` by default, a defaulted network mode never sandboxes an explicit `off`, a broker that cannot start under the default disables the network, and network trouble reaches the model as a `sandbox` diag (2026-10-07 amendment). A command on the project daemon asks the session that issued it for its amendment (2026-10-07 approval relay amendment). A network amendment may be approved for the rest of the session, chaining a bounded number of reruns, and an explicit deny beats every approval (2026-10-07 session network grants amendment). The default `yolo` an active sandbox keeps covers only the tools that sandbox confines, keyed on the typed `Confinement` marker (2026-10-07 typed confinement marker amendment). Memory `reflect` declares its one inference request and synthesizes through the session's inference, in an embedded environment directly and on an attached session's daemon through the reflection relay (2026-10-07 memory reflect inference amendment). A tool that declares no effects is no longer `read`: RPC host tools and eval-defined tools register `Effects::unknown()` (the `exec` tier), and Python worker tools take their extension host's ceiling, `write` under a `sandboxed` host and `exec` under a `trusted` one (2026-10-08 undeclared effects amendment). A call is judged by the effects its arguments scope, for approval and for the write boundary, by every tool that declares argument-scoped effects (2026-10-08 argument-scoped invocation effects amendment). Under `always-ask` a fetch is asked once per host per session, each host named by the environment resolver that reaches it (2026-10-08 host-keyed fetch subjects amendment). A session never runs its tools on a project daemon whose sandbox and approval policy differs from its own: the environment socket is keyed by the policy under a key private to the project, every `ServerHello` reports the policy, a session spawns no daemon it could not join, and a resumed child's environment starts under its class (2026-10-08 policy-keyed project daemons amendment). Under a disabled network, where no broker records anything, a failed command that launched a program able to run with a network URL among its arguments gets the mode's locator text once per session (2026-10-08 disabled-network diag amendment). The shell reads every program it launches through the session's file policy first, `exec` included, so it never launches one a `read_deny` root hides; Seatbelt itself still runs a hidden native binary that a utility builtin or another program launches, which stays open (same amendment, point 2). (Verified 2026-10-06 against `omp2` at `f2ca37d533`, plus the changes of the 2026-10-06 and 2026-10-07 amendments; the amendment approver re-verified 2026-10-07 on `feat/envd-approval-relay-daemon` from `omp2` at `1b648d0f74`, and the session half of the relay on `feat/envd-approval-relay-session` from `4b833b0b21`; the session network grants verified 2026-10-07 on `feat/sandbox-network-session-scope` from `omp2` at `1cbea4d0ea`, and their revocation on a session switch on the same branch after `4312bbfc5a`; the disabled-network diag on `fix/disabled-network-diag` from `omp2` at `0542ca8e8f`, and its review follow-up 2026-10-09 on `fix/disabled-network-diag-review` from `omp2` at `ef896cfb63`; by reading and by the tests named below.). `read@3` declares each fetch per target, with the classification its executor dispatches on, so a local read stays `read` and a URL, `ssh://`, `issue://`, `pr://` or remote `mcp://` read is a fetch asked once per host under `always-ask` (2026-10-08 read fetch amendment). `grep`, `glob` and `ast_grep` declare their remote roots with the classification each executor routes by, `grep` and `ast_grep` honour `tools.fetch.enabled`, and a `dyn` device call is judged by its arguments, so a local search stays `read` in every mode (2026-10-09 search fetch amendment). (Verified 2026-10-06 against `omp2` at `f2ca37d533`, plus the changes of the 2026-10-06 and 2026-10-07 amendments; the amendment approver re-verified 2026-10-07 on `feat/envd-approval-relay-daemon` from `omp2` at `1b648d0f74`, and the session half of the relay on `feat/envd-approval-relay-session` from `4b833b0b21`; the session network grants verified 2026-10-07 on `feat/sandbox-network-session-scope` from `omp2` at `1cbea4d0ea`, and their revocation on a session switch on the same branch after `4312bbfc5a`; by reading and by the tests named below.)

- In-process shell: `crates/shell` (parser/runtime) and `crates/shell-builtins` (about 80 builtins including `grep` and `rg` on the ripgrep libraries, `find`, `sed`, `sort`, `ln`, `jq`); persistent cwd and exports through `crates/envd/src/exec.rs`.
- Policy-keyed project daemons (2026-10-08): `DaemonPolicy`, `environment_socket(state_dir, policy)` and the private socket key (`SOCKET_KEY_FILE`, `socket_key`) in `crates/env/src/project_state.rs`; `daemon_policy::from_con` and `daemon_policy::of` in `crates/envd/src/daemon_policy.rs`, `ServerIdentity::policy` filled by `EnvServer::open_project` (and the other host constructors) and the `EnvdError::DaemonPolicyMismatch`, `DaemonPolicyUnreported` and `DaemonPolicyNotSpawnable` variants in `crates/envd/src/server.rs`; `ServerHello.policy_digest` in `crates/proto/proto/omp/env/v1/env.proto`; `verify_daemon_policy`, `refuse_unjoinable_spawn`, `AttachOptions::spawn_policy`, and their use in `attach_owner`, `ProjectEnvironment::connect_peer`, `register_project_presence` and `spawn_project_daemon_with` (which also forwards the profile) in `crates/envd/src/lib.rs`; `CfgFiles::daemon_policy` in `crates/driver/src/cfg.rs`; `present_journaled_class` and the drift report of `ConJournal::resync` in `crates/driver/src/headless/con_journal.rs`, and its call, the `spawn_policy` it passes and the `HeadlessError::EnvironmentPolicyDrift` check in `compose_kernel` (`crates/driver/src/headless/kernel.rs`). Proofs: `every_sandbox_and_approval_fact_changes_the_policy` and `unrelated_settings_keep_the_policy` in `daemon_policy.rs`; `the_daemon_policy_keys_only_the_environment_socket`, `the_socket_names_the_policy_only_under_the_private_project_key`, `concurrent_first_uses_publish_one_key` and `the_wire_digest_round_trips_and_only_32_bytes_are_a_policy` in `project_state.rs`; `the_hello_admits_only_the_clients_policy` and `spawn_stops_a_daemon_reporting_another_policy` in `crates/envd/src/lib.rs`; `the_spawned_daemon_policy_comes_from_the_configuration_files_alone` in `cfg.rs`; `the_journaled_class_is_presented_before_the_environment_starts` and `a_resumed_child_never_joins_the_main_configurations_daemon` (a child whose class sets `sv_sandbox_mode read-only` runs embedded under the class policy while the main configuration's daemon serves the project, and spawns no daemon) and `switching_to_a_class_of_another_policy_is_reported_once` (a `/resume` into a class of another policy warns once, naming both policies) in `con_journal.rs`; in `crates/driver/tests/approval_authority.rs`, `each_policy_joins_its_own_daemon_of_one_project` (daemons of two policies serve one project and each session joins the one that enforces its policy), `a_daemon_reporting_another_policy_is_never_joined` (a daemon reached on the session's own socket that reports another policy is refused, and the notice names both policies), `a_session_spawns_no_daemon_it_could_never_join` (no `envd.log` is written when the configured policy differs, one is when it matches), and on macOS `a_sandbox_off_session_never_runs_on_a_sandboxed_daemon` (the reported case: the `bash` call of an `sv_sandbox_mode off` session prompts and, refused, never runs, where on the `workspace-write` daemon it ran confined without a prompt) and `a_sandboxed_session_never_runs_on_an_unconfined_daemon` (the reverse: the call runs confined under the session's default `yolo`, where on the unconfined daemon it prompted). `compose_kernel` has no direct test, so the `EnvironmentPolicyDrift` check is proven only by reading.
- Attached posture notice (2026-10-09): `PostureNotice`, `PendingPosture` (its journal check `sync` and its session observer), `install_tool_authority` (which `compose_kernel` calls with the session it composes over), `session_posture` with `EnvToolExecutor::reported_sandbox` (the state the environment's handshake reported) and the post before `EnvToolExecutor` commits a call's arguments, in `crates/driver/src/headless/kernel.rs`; `ServerHello.sandbox_state` and the `SandboxState` enum in `crates/proto/proto/omp/env/v1/env.proto` (`SCHEMA_REV` 27), `SandboxState::to_wire`/`from_wire` in `crates/envd/src/admission.rs` and the state `accept_hello` reports in `crates/envd/src/server.rs`; the session handed to `SessionObserver::rewound`/`switched` by `JobBoard::apply_lifecycle` and `JobBoard::session_switched` in `crates/agent/src/jobs.rs`, and `Kernel::session_switched(&Session)` in `crates/agent/src/loop.rs`, called with the next session by `switch_to_in` in `crates/app/src/chat_control.rs`, the session transition in `crates/app/src/rpc_mode.rs` and `switch_session` in `crates/app/src/acp_mode.rs`. Proofs: `the_posture_report_posts_once_until_the_journal_lacks_it` in `kernel.rs` (the first admission at either point posts the typed notice, a later one at either adds nothing, a rewind or switch onto a branch without the notice makes it due again); `the_session_posture_follows_the_sandbox_the_environment_reports` in `kernel.rs` (a handshake reporting `active` keeps a defaulted `yolo` this process would downgrade, one reporting `backend_unavailable` downgrades it although this process constructed a sandbox, and one reporting none leaves the probe to decide; it fails when the posture ignores the handshake); `every_sandbox_state_round_trips_the_hello_wire` in `admission.rs`; `the_hello_reports_the_sandbox_state_the_host_probed` in `server.rs` (`off` for a host whose sandbox is off, the probed state for one configured for the workspace, `active` where Seatbelt runs); `every_rewind_and_switch_tells_the_session_observers` in `crates/agent/tests/jobs.rs` (each observer is handed the rewound session and the next session); in `crates/driver/tests/approval_authority.rs`, whose harness installs the tool authority with the production `install_tool_authority`, over a project daemon served in the test process: `an_attached_session_reports_its_downgraded_default_yolo_once` (two `bash` calls prompt and are refused; one `warn` notice, payload `yolo`/`default`/`write`/`off`, with the downgrade prose, journaled before the first prompt the daemon's admission filed, which fails when the executor posts it once that prompt is answered) and `an_attached_session_reports_its_unconfined_explicit_yolo_once` (both run unprompted; one notice, `yolo`/`explicit`/`yolo`/`off`, with the unconfined prose), each failing with no notice when the executor does not post it (and `the_posture_report_posts_once_until_the_journal_lacks_it` with three when the once guard is removed); `an_attached_kernel_reports_its_posture_to_every_session_it_serves` (after `Kernel::session_switched` the next session is told once too, and the session left keeps its one notice); `an_attached_kernel_tells_a_resumed_session_nothing_twice` (switched back, the first session is not told again) and `an_attached_session_resumed_by_another_kernel_is_not_told_twice` (a second kernel composed over the told session posts nothing), each failing with two notices when the switch or the composition re-arms the report without reading the journal; `an_attached_rewind_past_the_posture_notice_reports_it_again` (a rewind keeping the notice adds nothing, one dropping it reports it again; it fails with no live notice when a rewind leaves the report spent, and with three journaled when every rewind re-arms it); `a_retried_tool_tail_reports_the_posture_its_rewind_dropped` and, over the project daemon, `an_attached_retried_tool_tail_reports_the_posture_its_rewind_dropped` (the host interrupts a `bash` call at its prompt and retries the tail: the retried call reports it again, the live branch holds one and the abandoned branch the first; each fails with no live notice when a rewind leaves the report spent); on macOS `an_attached_session_inside_an_active_sandbox_posts_no_posture_notice` (the daemon's Seatbelt sandbox keeps the default `yolo`, nothing is reported). The embedded proofs (`default_yolo_without_a_sandbox_prompts_for_bash_and_says_why`, `explicit_yolo_without_a_sandbox_runs_bash_unprompted_and_says_so`, `default_yolo_inside_an_active_sandbox_runs_bash_unprompted_and_silent`) run on the same harness, and the `session_network_grants` harness installs its tool authority the same way. Each of these posture proofs also checks that no request the model was sent after the call names or quotes the notice; that pins the current projection, which leaves every `<notice>` out, not a decision that the model must never see this one. `compose_kernel` itself has no direct test; that it calls `install_tool_authority` is proven by reading.
- Enforcement: `ExecSandbox` and its per-attempt wrapper in `crates/envd/src/exec_sandbox.rs` implement the shell's `PathPolicy` and `SpawnWrapper`; `exec.rs` installs both on each run (`set_path_policy`, `set_spawn_wrapper`). The sandbox is `workspace-write` by default (`SV_SANDBOX_MODE`, `crates/envd/src/exec_settings/sandbox.rs`), and `SandboxState::probe` reports whether it was constructed; network is `scoped` by default (`SV_SANDBOX_NETWORK_MODE`, 2026-10-07), and the scoped egress broker (`crates/envd/src/sandbox_proxy.rs`) is what produces a typed network fact. `SandboxSettings::network_confinement` and `exec_settings::network_confinement` apply the provenance rule; `ExecSandbox` records the confinement it really applies (`resolve_network` in `exec_sandbox.rs`: a session starts its broker, a `SandboxConsumer::Child` such as an eval cell or a detached process gets `disabled`, a probe starts none) and `amended_scope` reuses it. Proofs: `network_confinement_*` and `an_explicit_off_survives_the_default_flip` in `exec_settings/sandbox.rs`; `explicit_off_*`, `scoped_network_resolves_by_consumer`, `broker_start_failure_degrades_only_the_shipped_default`, `tokenless_children_and_probes_compile_without_a_broker` and `a_path_amendment_after_the_broker_fallback_stays_network_disabled` in `exec_sandbox.rs`; `explicit_off_with_the_default_network_runs_commands_unsandboxed` in `exec.rs`; `unreachable_literals_are_refused_without_an_amendable_fact` in `sandbox_proxy.rs`; the posture tests in `crates/driver/src/adw/production.rs`; `children_keep_an_explicit_scoped_network_under_sandbox_mode_off` in `crates/driver/tests/subagent_cfg.rs`. Under the broker profile the Seatbelt caveats copied into the session note no longer claim unfiltered outbound egress and say commands have no DNS of their own (`crates/sandbox/src/backends/seatbelt.rs`). Checked live on macOS with Seatbelt and the default settings (a throwaway test, not kept): `/usr/bin/curl https://example.com` ended `Denied` with the fact `network example.com:443` (curl exit 56, `CONNECT tunnel failed, response 403`), and `/usr/bin/nc -z` to a raw IP on port 22 could not connect, while both succeeded outside the sandbox.
- Read lane (2026-10-07): `FilePolicy::admit_read` walks the requested path physically (`resolve_physical_path`: links followed in place, `..` applied after them, as the kernel, `read_dir` and a spawned child resolve it), and `check_read` and `FilePolicy::open` both use its result; `open` opens exactly that path from its root with `O_NOFOLLOW` on every component, so `link/../key` is judged and read as the link target's sibling, not the link's. In `host` read mode it follows symlinks and denies when the walk enters or ends in a `read_deny` root or the `..`-collapsed spelling lies in one (a link entry inside a denied root stays denied, as under bubblewrap's mask); loops and other resolution errors deny. Known limits of the in-process lane: a missing directory followed by `..` resolves instead of failing with `ENOENT` as it does for the kernel, and `read_deny` roots are matched byte-wise, so a case variant of a denied root on a case-insensitive volume is not refused in process. Utility builtins that open files directly (`cat.rs`, `head.rs`, `grep.rs` in `crates/shell-builtins`) do not call `check_read`; that gap predates this lane and is tracked in `docs/parked/2026-10-06-handoff-pending-work.md`. A program reached through a link, such as Homebrew's `/opt/homebrew/bin/git` into `../Cellar`, a glob through a linked directory, or a cwd that crosses a link, now runs in the foreground as it already did in detached scripts, which re-enter the shell child under the kernel wrapper only (`detached_command` in `exec.rs`; `shell_child.rs` installs no path policy). The `minimal` and `scoped` read modes keep refusing symlinks until a follow-up grants the traversed link entries in both lanes: Seatbelt needs a read on each link it resolves, and bubblewrap's restricted view binds only canonical paths. Proofs: the `host_read_*`, `restricted_read_mode_*`, `approved_read_scope_*` and `protected_open_*` tests in `exec_sandbox.rs`; `environment_only_sandbox_*` (including `link/../key` through the redirect and glob lanes) and the live Seatbelt `sandboxed_session_runs_programs_reached_through_symlinks` in `exec.rs`.
- Network diag (2026-10-07, point 7): the broker records `BrokerDenial { host, port, cause: BrokerRefusal }` per attempt (`ProxyPolicy::record`, first policy refusal wins; `ProxyPolicy::authorize` and `ProxyPolicy::connect` return the cause, `resolved_refusal` judges the resolved addresses, and `connect` records `upstream`); `http` answers only `policy` with `http_policy_deny` (`403`, `X-Omp-Policy-Blocked`) and a fail-closed cause with `http_broker_refused` (`502`, `X-Omp-Broker-Refused`); `ExecSandboxAttempt::take_facts` returns the amendable `denial` (policy only) beside the `refusal` of any cause; `run_session_command` pushes `exec_network_diag::network_diag` before the final `finish_session_command`, with the session's `NetworkAnnouncements` (the generic text once, each refusal's full text once per endpoint and cause), and `NetworkMarkerScan` feeds on each stderr/pty chunk in `OutputSequencer::capture_sandbox_diagnostic` while `sandbox_denial_marker` keeps its own capture (both use `find_marker`). Proofs: `fail_closed_refusals_are_recorded_per_attempt`, `resolved_addresses_are_judged_as_a_whole`, `only_policy_refusals_carry_the_policy_marker` and `unreachable_literals_are_refused_without_an_amendable_fact` in `sandbox_proxy.rs`; the marker and split-invariance tests, `refusals_are_explained_once_per_endpoint_and_cause`, `unquotable_refusals_keep_their_cause_and_leave_the_generic_text_unspent` and `generic_texts_name_the_mode_and_appear_once_per_session` in `exec_network_diag.rs`; `fail_closed_broker_refusals_stay_ordinary_failures` (the broker's whole response head as stderr), `network_markers_leave_denial_classification_unchanged` and the live Seatbelt `scoped_network_trouble_reaches_the_model_as_sandbox_diags` (offline, `.invalid` hosts; it includes `curl -v` on an allowed name that does not resolve, which ends `Failed`) in `exec.rs`. Checked live on macOS with Seatbelt and the default settings (a throwaway test, not kept): `/usr/bin/curl -sS --max-time 10 https://example.com` ended `Denied` (exit 56) with a `warn` diag naming `example.com:443`, the scoped mode and, with no approval route bound, the convar remedy only; `/usr/bin/nc -z -G 5 github.com 22` failed (exit 1, `nodename nor servname provided`) with the generic `warn` text naming `HTTP_PROXY` once, and the same command again added no diag.
- Disabled-network diag (2026-10-08): `SpawnWrapper::observe_launch` in `crates/shell/src/interp.rs`, called by `compose_std_command` in `crates/shell/src/commands.rs` for every external launch the interpreter composes (simple commands, pipeline stages, command substitutions, `exec`, and the command `timeout` runs through the shell), after the installed `PathPolicy` admits the program's read (`launch_program` resolves it at most once per launch, shared by that check and the wrapper; a refusal is a path denial, `exec` included); utility builtins that build their own commands through `Host::command` and `ChildEnv::command` in `crates/shell-builtins/src/host.rs` (`xargs`, `find -exec`, `sort --compress-program`, `ifne`) are not observed (point 4). `ExecSandboxAttempt::observe_launch` in `crates/envd/src/exec_sandbox.rs` scans the arguments only while the attempt's network is `disabled` and, on a network URL, asks for the program (which the shell has already resolved for the attempt's read check) and records `AttemptFacts::network_locator` if `FilePolicy::launchable` judges it able to run in the sandbox's own view (its read admitted by the session's file policy, then not a directory and executable; a bare name `exec` hands over is found on the shell's `PATH` first, and one found nowhere never counts); `is_network_url`, `NetworkSign` (a marker outranks a locator) and the `locator` property of `NetworkInForce` (`Disabled` and `BrokerUnavailable` only) in `crates/envd/src/exec_network_diag.rs`, whose `network_diag` takes the sign and spends the one generic slot; `run_session_command` passes `NetworkSign::of(marker, facts.network_locator)`. Proofs: `compose_std_command_prefixes_spawn_wrapper`, `compose_std_command_refuses_programs_the_path_policy_hides` (an absolute path, a relative one and a bare name found on `PATH` under a hidden root each end in a read denial of the program and are never observed), `exec_refuses_programs_the_path_policy_hides` (a simple command, `exec` at a protected root, in a subshell with a flag and without, and a bare `exec` name never run a hidden program; an admitted copy runs), `compose_std_command_resolves_programs_only_when_asked` (with no path policy and a wrapper that never asks, a relative path, a bare name and an absolute one resolve nothing; a policy and a wrapper that asks share one resolution per launch) and `spawn_wrappers_observe_expanded_external_arguments` (a variable-expanded URL reaches the wrapper with its program from a simple command, each pipeline stage, a command substitution, a relative path made absolute against the working directory, and `exec` at a protected root and in a subshell, whose bare name is found on `PATH` or, found nowhere, reaches the wrapper as no program; a builtin given one does not) in `crates/shell/src/commands.rs`; `timed_commands_reach_the_spawn_wrapper` in `crates/shell-builtins/src/timeout.rs`; `network_urls_name_a_host` and `silent_failures_without_a_network_are_explained_once_per_session` (once per session, shared with the marker; nothing on success, cancellation or under `scoped`, which leaves the slot unspent) in `exec_network_diag.rs`; `attempts_record_network_urls_only_without_a_network` in `exec_sandbox.rs` (test wrappers for `disabled` and the broker fallback, and a compiled native one where a backend exists; a URL handed to no program, a missing one, a directory, a file without its exec bit or an executable under `read_deny` records nothing and leaves later launches free to count; a `scoped` attempt records nothing) and `programs_are_resolved_only_for_a_url_without_a_network` (the program is asked for never under `scoped`, never for a launch without a URL, once for the first URL and never after); `silent_network_failures_without_a_network_get_one_sandbox_diag` in `exec.rs` (every unix host: an environment-only wrapper that reports a disabled network, `/usr/bin/false` given a variable-expanded URL fails quietly and gets one `warn`, while a success, a pipeline whose last stage succeeds, a failure without a URL, a URL inside a longer argument, the `false` builtin, a missing program (127), an `exec` of a bare name found nowhere (127), a script without its exec bit (126), a simple command and an `exec` of an executable under `read_deny` (a read denial, though the environment-only wrapper would let it run) and a `scoped` session get none) and the live Seatbelt `quiet_clients_under_a_disabled_network_reach_the_model_as_a_sandbox_diag` (a missing `/nonexistent/curl`, which `sandbox-exec` fails to exec after its own spawn, gets only the session note; `/usr/bin/curl -sI` to a `.invalid` host under `disabled`: exit 6, no output, one `warn` naming the mode, none on the repeat) and `native_programs_behind_read_deny_never_run_under_seatbelt` (a native binary under `read_deny`, which Seatbelt itself would exec, ends in a read denial as a simple command and through `exec`, with no network diag for its URL; the same binary outside the root runs).
- Approval: `classify_sandbox_denial` in `crates/envd/src/exec.rs` types the denial (`SandboxDenialFact::{ReadPath, WritePath, Network, Unknown}`); `approve_sandbox_amendment` asks one `sandbox_amendment` ticket (scope `once`, human required, 120 s timeout, fail-closed) of the approver `ExecHost::amendment_approver` picks, and is false when there is none. The relay of the connection that issued the command comes first (`crates/envd/src/approval_relay.rs`): an `Exec`, or a native invocation through the `tools::invocation_approvals` task-local, on an application connection that advertised `approval-relay`, prompts that connection on that request only. Without one, the route an in-process composition binds (`bind_sandbox_approval_route`) decides. A project daemon binds no route. An attached session advertises `approval-relay` (`attached_features` in `crates/envd/src/lib.rs`, sent only by `hello_attached_session`), and `connect_peer` starts `approval_relay::pump_approval_queries`, which files each query on the route `ProjectEnvironment::bind_approval_authority` stores (the driver's kernel route, `crates/driver/src/headless/kernel.rs`) with `request_cancellable` and answers with its decision; a withdrawal, a closed transport or shutdown cancels the filed prompt without answering, and a query that arrives while no route is bound is decided by its unreachable rules, which deny a sandbox amendment. A relay whose connection closed never falls back to the route: a command that outlives its connection (detached, auto-backgrounded) fails closed. Named processes (`start_process`: `StartProcess`, async bash, restart generations) carry no relay. Proofs: the `approval_relay` unit tests (daemon half, and the session half: `the_session_answers_through_its_bound_route_on_the_query_request`, `a_withdrawn_query_cancels_its_prompt_and_is_never_answered`, `a_closed_transport_cancels_open_prompts_without_answering`, `an_unbound_session_denies_relayed_prompts_as_unreachable`, `relayed_requirements_and_decisions_survive_the_wire`), `attached_hello_relays_approvals_and_advertises_only_supplied_edit_repair_facts` in `lib.rs`, `a_command_relay_outranks_the_host_route_and_never_falls_back` in `exec.rs`, `only_advertising_application_connections_relay_approvals`, `closing_a_connection_disconnects_its_relay`, `daemon_relays_an_amendment_only_to_the_issuing_connection`, `daemon_withdraws_cancelled_prompts_and_fails_closed_on_disconnect` and `native_bash_relays_its_amendment_on_the_invocation_request` in `server.rs`, and in `crates/driver/tests/approval_authority.rs` the `attached_daemon` tests, which attach the production composition to a daemon served in the test process (asserting no fallback notice): an approved amendment is journaled once with no invocation id and its rerun writes into the protected `.git`, a second attached session is never asked, and a refused one ends `denied` with nothing written. Against a separate `omp envd` process, P12 (`crates/e2e/tests/p12_daemon_approvals.rs`, the daemon child attached to a real docserver under an isolated home) proves that only the issuing connection is prompted, that another connection's forged answer decides nothing, that the approved rerun writes the daemon's own pid through `$$`, that the daemon never withdraws a prompt its owner answered, that a cancel withdraws the prompt before the exit, and that closing the issuing connection ends its command without writing and frees its session at once. That close also cancels the connection's commands, so P12 cannot tell it from the relay failing the prompt closed; `closing_a_connection_disconnects_its_relay` proves the relay's disconnect for a command that outlives its connection. Through the real chat on a real PTY, P7's `chat_tui_answers_a_daemon_sandbox_amendment_from_its_overlay` (`crates/e2e/tests/p7_tui.rs`, macOS) runs the shipped default posture with chat attached to the daemon it spawns: the overlay offers only approve and deny and `a` is no answer, its `y` reruns the command, which writes the pid of a live process of the same executable that is not chat, `Esc` denies a second command, which writes nothing, and the journal replays both decided `once` prompts. The overlay and grant rule are proven by `pending_approval_projects_overlay_and_hotkeys` and `a_session_offering_approval_answers_a_for_the_session` in `crates/chat/tests/host.rs` and by `a_session_grant_never_answers_a_prompt_that_offers_only_once` and `a_session_answer_the_prompt_never_offered_grants_nothing` in `crates/agent/tests/approval_desk.rs`. On a `once` approval `run_session_command` reruns the command once with `amended_scope` or `amended_network` (`AmendmentReruns::Spent` then ends the chain; session grants are the bullet below). `Unknown` denials are never amended. Path scopes are captured with identity checks before the rerun (`ApprovedPathScope` in `exec_sandbox.rs`).
- Session network grants (2026-10-07): `approve_sandbox_amendment` offers `["once", "session"]` for a network fact and `["once"]` for a path, honours only an offered scope, and on `session` inserts the endpoint into the deciding approver's `EgressGrants` (`EnvApprover::grants`: `OwnedApprovals::grants` on a relay, `RouteBinding::grants` on the in-process route, `crates/envd/src/approval_relay.rs`). `run_session_command` begins each attempt with `ExecHost::egress_grants` of the command's binding (`ExecSandbox::begin_attempt`, `ScopedProxy::begin_attempt`), and `AmendmentReruns::admits` with `sv_sandbox_network_session_reruns` (`SV_SANDBOX_NETWORK_SESSION_RERUNS`, `crates/envd/src/exec_settings/sandbox.rs`) bounds the chain; a session grant reruns under the same sandbox, a once grant compiles the amended one. `bind_sandbox_approval_route` clears the previous binding's grants, `ConnectionApprovals::disconnect` and `revoke_grants` clear a relay's, and `EnvServer::revoke_approval_grants` and the `RevokeApprovalGrants` frame (routed to the environment backend by `crates/env/src/partition.rs`, sent by `EnvClient::revoke_approval_grants`) serve `SessionGrants::revoke` (`crates/envd/src/lib.rs`), which the driver's `RevokeSessionGrants` (`crates/driver/src/headless/kernel.rs`, registered by `bind_environment_approvals` through `JobBoard::observe_sessions`) calls on every rewind and on every session switch: `Kernel::session_switched` (`crates/agent/src/loop.rs`) is called by `switch_to_in` in `crates/app/src/chat_control.rs`, the session transition in `crates/app/src/rpc_mode.rs` and `switch_session` in `crates/app/src/acp_mode.rs`. Proofs: `session_grants_admit_their_exact_endpoint_on_a_running_policy`, `deny_rules_beat_every_approval`, `deny_rules_and_ports_precede_resolution` and `only_policy_refusals_carry_the_policy_marker` in `sandbox_proxy.rs`; `jit_approval_names_only_the_detected_capability_and_exact_command`, `network_amendments_offer_the_session_and_paths_stay_once`, `amendment_reruns_admit_only_new_session_network_endpoints`, `rebinding_or_revoking_the_route_clears_its_egress_grants` and `a_command_relay_outranks_the_host_route_and_never_falls_back` in `exec.rs`; `network_session_reruns_default_to_four_and_project_a_set_bound` in `exec_settings/sandbox.rs`; `refusals_are_explained_once_per_endpoint_and_cause` in `exec_network_diag.rs`; the live Seatbelt `session_network_grants_chain_reruns_and_outlive_the_command` (the pip pattern on two loopback upstreams with `curl --noproxy ''`: one call, two prompts, three runs, then no prompt in the same or another shell), `once_network_grants_still_end_the_chain_after_one_rerun` and `deny_listed_hosts_are_never_offered_for_approval` in `tool_shell.rs`; `daemon_session_grants_belong_to_the_approving_connection` in `server.rs` (a second connection to the same daemon is asked, and `RevokeApprovalGrants` makes the owner be asked again); `grant_revocation_is_request_zero_and_requires_hello` in `crates/env/src/client.rs` and `grant_revocation_routes_to_the_environment_and_opens_no_route` in `partition.rs`; `every_rewind_and_switch_tells_the_session_observers` in `crates/agent/tests/jobs.rs`; `a_resumed_session_answers_a_journaled_network_grant_and_asks_the_rest`, `a_rewind_past_a_session_grant_asks_again_and_tells_session_observers` and `a_switch_away_from_a_session_grant_asks_again_and_tells_session_observers` in `crates/agent/tests/approval_desk.rs`; the `session_network_grants` tests in `crates/driver/tests/approval_authority.rs`, whose kernel is wired by the production `bind_environment_approvals`: on a daemon served in the test process (`an_attached_grant_holds_until_a_rewind_revokes_it`, `an_attached_grant_never_reaches_the_next_session`) and on an embedded composition's in-process route (`an_embedded_grant_holds_until_a_rewind_revokes_it`, `an_embedded_grant_never_reaches_the_next_session`), the second command fetches unprompted, and after the rewind or the switch to a new session a human is asked again; and the host switch paths, `session_switch_orders_gate_flush_transition_resync_observation_then_start` and `fork_to_an_entry_and_new_session_tell_the_kernel_of_a_switch` in `chat_control.rs`, `rpc_session_commands_publish_reset_snapshots` in `crates/app/tests/rpc_spine.rs` and `new_load_and_resume_switch_the_authoritative_durable_session` in `crates/app/tests/acp_spine.rs`, each of which observes one switch and no rewind per committed transition.
- Tool-level admission: `bash@2` declares `Effects::empty()` and `Confinement::ExecSandbox` (`crates/tools/src/shell.rs`), so with an active sandbox the tier derived in `crates/envd/src/admission.rs` is `read` and no mode prompts for it, and without one it is `exec`; only a per-tool `sv_tools_approval` override or a hook changes that. Nested `dyn` targets are admitted separately on their own effects and confinement (`DynamicAdmission`, `crates/envd/src/devices_host.rs`).
- Typed confinement marker (2026-10-07): `Confinement` and `ToolSpec::confinement` in `crates/tool/src/lib.rs`, `DeviceTarget::confinement` in `crates/tool/src/registry.rs`; `resolve_approval` takes it (`call_approval_mode` in `crates/envd/src/admission.rs`, which the `/security` feed `approval_posture` in `crates/app/src/chat_services/misc.rs` also reports per confinement), as do `ToolSettings::approval_for`, `ToolAdmission::admit` (`crates/agent/src/dispatch.rs`, from `Registry::live_spec`), `SettingsAdmission`, envd's `InvokeTool` admission and `InvocationExecutionPolicy::denial` (`crates/envd/src/server.rs`), and `DynamicAdmission::admit`. Proofs: `exec_sandboxed_tools_are_exec_tier_unless_a_sandbox_confines_them`, `a_defaulted_yolo_covers_only_sandboxed_tools`, the property test `host_tools_are_admitted_as_if_no_sandbox_existed`, `per_tool_override_is_authoritative_and_receipted` and `dynamic_admission_prompts_for_host_targets_under_a_defaulted_yolo` in `admission.rs`; `approval_for_threads_confinement` in `tool_settings.rs`; `host_dyn_devices_are_not_auto_approved_inside_a_sandbox` in `devices_host.rs`; `only_sandbox_spawning_tools_are_exec_sandboxed` in `crates/envd/src/tools.rs` (exactly `bash@2` and `hub@2`); `write_scopes_refuse_tools_that_write_around_the_scoped_writers` (the real `bash` spec) and the live Seatbelt `a_sandbox_kept_default_yolo_covers_only_sandboxed_tools` in `server.rs` (under the shipped `yolo`, `bash` runs unprompted while a `Host` network tool asks the client and, refused, never runs; under an explicit `yolo` neither asks); `settings_admission_prompts_for_host_tools_under_a_sandboxed_default_yolo` in `kernel.rs`; `native_admission_receives_the_live_spec_confinement` in `crates/agent/tests/lifecycle_hooks.rs`; `extension_tools_cannot_claim_the_exec_sandbox` in `extensions.rs`; `confinement_defaults_to_host_and_stays_off_the_projection` in `crates/tool/tests/contracts.rs` and `host_tools_are_host_confined` in `registry.rs`; `security_report_shows_the_effective_approval_mode_beside_the_configured_one` in `crates/chat/src/commands/misc.rs` (`yolo` for sandboxed tools and `write` for host tools under a sandbox-kept default `yolo`, one mode otherwise).
- Fetch tier (2026-10-08): `FetchEffects` and `Effects::fetch` in `crates/tool/src/lib.rs`; `ApprovalTier::Fetch` and `ApprovalTier::from_effects` in `crates/envd/src/admission.rs`; `Grants::from_effect_envelope` in `crates/envd/src/policy.rs`; `mcp_tier_effects` in `crates/envd/src/mcp/manager.rs`; `omp.FetchEffects` and `Tier.FETCH` in `crates/py/python/omp`. Proofs: `a_fetch_sits_between_read_and_write` and `fetch_narrowing_and_wire_round_trip` in `admission.rs`, `fetch_effects_derive_only_the_network_grant` in `policy.rs`, `declared_tiers_map_to_their_admission_tiers` in `mcp/manager.rs`.
- Argument-scoped invocation effects (2026-10-08): `Tool::ARGUMENT_SCOPED_EFFECTS`, `Tool::invocation_effects` and `decode_params` (one JSON object) in `crates/tool/src/lib.rs`; `Registry::invocation_effects`, `Registry::scopes_invocation_effects` and `RegistryError::InvocationEffectsExceedMaximum` in `crates/tool/src/registry.rs`; `AdmissionGate::argument_scoped`, `AdmissionGate::stage`, `AdmissionGate::emit`, `AdmissionGate::resolve_pending`, `rewritten` on `AdmissionDecision::Allowed`, `widened_effects_denial` and the desktop rule of `ApprovalTier::from_effects` in `crates/envd/src/admission.rs`; the `ArgsCommitted` handler (stage, then `ConnectionState::scope_committed`, then emit) and the post-answer check in `commit_invocation` in `crates/envd/src/server.rs`; native admission in `crates/agent/src/dispatch.rs`; `DesktopEffects::clipboard` in `crates/tool/src/lib.rs` and `omp.policy.v1.DesktopEffects.clipboard`; `computer`'s `invocation_effects` and `NativeParams::required_effects` in `crates/tools/src/computer.rs`; `refuse_input_in_read_only` in `crates/envd/src/computer.rs`. Proofs: `invocation_effects_narrow_and_fail_closed` (a sequence-form array keeps the maximum) and `undeclared_scoping_keeps_the_declared_maximum` in `registry.rs`; `decode_params_refuses_documents_that_are_not_objects` in `crates/tool/src/lib.rs`; `effects_are_exact_deny_safe_and_wire_stable` in `crates/tool/tests/contracts.rs` (a clipboard read is desktop authority, narrowed and carried on the wire); `an_argument_scoped_gate_waits_for_the_committed_call` (a refused commit stages and resolves nothing, a repeated commit after the query is ignored), `prompt_policy_emits_a_query_and_waits_for_an_answer` (only a patched answer is `rewritten`) and `approval_modes_resolve_from_declared_effects` (capture, accessibility and clipboard reads are each `exec`) in `admission.rs`; `argument_scoped_calls_are_approved_by_their_committed_effects` (under `always-ask` a reading call runs unasked and a writing or executing one asks; under `write` only the executing one asks), `argument_scoped_calls_meet_the_write_boundary_by_their_effects` (plan mode admits the reading call and refuses the writing and executing ones, a call judged beyond the maximum is refused, a widening admission answer is denied and a narrowing one runs) and `a_refused_commit_leaves_the_call_unjudged` (after a refused sequence-form commit, an executing commit asks and never runs and a reading commit runs unasked) in `server.rs`; `a_read_only_computer_call_keeps_the_exec_tier` in `crates/envd/src/tool_settings.rs` (the default posture, `always-ask` and `write` all prompt; only an explicit `yolo` runs it); `the_read_only_gate_refuses_exactly_the_input_operations` and `read_only_programs_refuse_every_input_operation_before_it_runs` in `crates/envd/src/computer.rs`; `native_admission_judges_each_call_by_its_arguments` in `crates/agent/tests/lifecycle_hooks.rs`; `read_only_calls_are_scoped_to_desktop_reads` and `read_only_envelope_covers_exactly_the_operations_a_read_only_program_runs` in `crates/tools/src/computer.rs`.
- Host-keyed fetch subjects (2026-10-08): `Tool::fetch_locators` in `crates/tool/src/lib.rs` and `Registry::fetch_locators` in `crates/tool/src/registry.rs`; `FetchHost`, `FetchResolver`, `NamedFetches`, `FetchHostNamer`, `fetch_subjects`, `tool_fetch_subject` and `resolve_locators` in `crates/envd/src/fetch_host.rs`, with `UrlResolver::fetch_host` (`crates/envd/src/tool_url.rs`), `GithubResolver::fetch_host` and `McpUrlResolver::fetch_server`; `EnvServer::fetch_hosts` and `ProjectEnvironment::fetch_hosts`; `AdmissionGate::name_fetch_hosts` and the `effects`/`fetch`/`fetch_unnamed` of the query in `crates/envd/src/admission.rs`, filled by `ConnectionState::scope_committed` in `crates/envd/src/server.rs`; `omp.policy.v1.FetchTarget`, `omp.policy.v1.FetchResolver` and `AdmitInvocation.effects`/`fetch`/`fetch_unnamed`; `ToolAdmission::admit` (its `fetch_locators`) and `ToolAdmissionVerdict::Prompt` (every requirement of the call) in `crates/agent/src/dispatch.rs`; `session_grant` in `crates/agent/src/approvals.rs`; `admission_specs` and `SettingsAdmission::with_fetch_hosts` in `crates/driver/src/headless/kernel.rs`, which `compose_kernel` wires with the composition's `ProjectEnvironment::fetch_hosts`; the `dyn` requirements of `DynamicAdmission::admit` and `McpManager::dynamic_effects`; `acp_tool_kind` and `permission_tool_call` in `crates/app/src/acp_mode.rs`, and the `requirements` of `tool_approval_request` in `crates/app/src/rpc_mode.rs`. Proofs: `fetch_locators_follow_the_declared_classifier` in `registry.rs`; `subjects_are_keyed_by_resolver_and_host`, `an_unnameable_locator_keeps_the_named_hosts`, `wire_targets_keep_every_named_host` and `fetch_targets_survive_the_wire_and_refuse_malformed_hosts` in `fetch_host.rs`; `the_query_reports_the_judged_envelope_and_named_hosts` and `dynamic_prompts_offer_the_session_and_key_fetches_on_their_hosts` in `admission.rs`; `fetch_queries_name_every_host_the_resolvers_reach` in `server.rs` (under `always-ask` the query names the authored http(s) hosts, the `ssh://` alias and the GitHub host of the URL or of the workspace remote, keeps the named hosts and reports the remainder unnamed when one locator cannot be named, and a local read or a fetch under `write` never asks); `grants_from_separate_prompts_cover_a_prompt_raising_them_together` in `crates/agent/tests/approval_desk.rs` (two hosts, or a tool and a host, granted on separate prompts answer a prompt raising them together; an ungranted or once-only requirement still asks; persisted only when every covering grant is); `native_admission_judges_each_call_by_its_arguments` in `crates/agent/tests/lifecycle_hooks.rs` (the dispatcher hands admission a fetching call's locators, and asks for them only when the call fetches); `admission_queries_key_each_fetch_on_its_host`, `a_session_fetch_grant_covers_its_host_and_no_other` (a session grant answers a later fetch from the same host and not another host, a fetch reaching another host as well, the tool's own fetch or the tool; hosts granted one at a time answer a fetch reaching both; a grant for the tool's own unnamed fetch never answers a host named beside it) and `settings_admission_asks_an_unnamed_kernel_fetch_as_the_tools_own` in `kernel.rs`; in `crates/driver/tests/approval_authority.rs`, `always_ask_asks_once_per_fetched_host_per_session` (an environment served in process admits an argument-scoped fetching tool under `always-ask`, the production `EnvToolExecutor` files each query on a kernel route and the approval desk answers it: the first fetch from a host asks, a `session` answer lets later fetches from it run unasked, another host asks again, a fetch reaching two hosts granted one at a time runs unasked, a local read never asks, and a fetch beside an `mcp://` resource no server advertises asks for its named host and the remainder, the remainder's grant never covering a host no one granted) and `an_embedded_kernel_asks_its_native_fetches_once_per_host` (the same sequence through kernel turns on the environment's native route, admitted by `SettingsAdmission` with the environment's `fetch_hosts`); `approval_kinds_map_to_acp_tool_kinds` and `permission_requests_show_every_requirement` in `acp_mode.rs`; `approval_requests_carry_every_requirement` in `rpc_mode.rs`.
- Read fetch (2026-10-08): `TargetClass`, `classify_target`, `normalize_read_target`, `ReadPolicy::credentialed_fetch` and `read@3`'s `invocation_effects`/`fetch_locators` in `crates/tools/src/read.rs`; `Resolve::read_fetch`, `ResolverTable::read_fetch` and `ResolverTable::read_unknown_fetch` in `crates/tools/src/read/resolver.rs`; `UrlResolver::read_fetch` in `crates/envd/src/tool_url.rs` and `McpUrlResolver`'s in `crates/envd/src/tool_url/mcp.rs`; `McpService::resource_read_pin`, `McpService::pinned_resource_server`, `McpManager::resource_read_pin`, `McpManager::admit_pinned_read` (`mount_read_fetch`, `PinnedReadError`) in `crates/envd/src/mcp`; `InvocationPins` and `ResolutionPin` in `crates/tool/src/pins.rs`, the pins `ConnectionState::scope_committed`, `effective_effects` and `spawn_native_invocation` in `crates/envd/src/server.rs` and the kernel `Dispatcher` (`Unit::Native`) in `crates/agent/src/dispatch.rs` judge and run a call in; `production_read_policy` and the forwarding `InstrumentedTool` in `crates/envd/src/tools.rs`. Proofs: `read_judges_each_call_by_what_its_targets_fetch` (local paths, `file://`, internal URIs that read local state, vault reads that ask the Obsidian CLI and unknown schemes are document reads; URLs are anonymous fetches; `ssh://`, `issue://`, `pr://` and `mcp://` credentialed ones; JSON arrays and `;`/`,` lists are judged by every member, each fetching target named once in canonical spelling), `read_maximum_holds_the_fetches_its_policy_permits` (the maximum per policy; a URL read fetches only while URL reads are enabled; a credentialed read beyond the maximum is refused) and `the_executor_takes_the_route_each_target_was_judged_by` (a target judged to fetch reaches the web reader or a fetching resolver, one judged not to reaches neither) in `crates/tools/tests/it/read.rs`; `resource_reads_fetch_by_transport_and_declared_tier` and `a_judged_resource_read_asks_only_the_server_its_judgment_pinned` (a read judged while a local `read`-tier server advertises the resource asks only that server once a remote server sorting first advertises it too, and is refused, sent nowhere, once that server is unmounted or remounted remote; a read judged as a fetch from one remote server asks that server though another would answer by then) in `crates/envd/src/mcp/manager.rs`; `the_first_resolution_of_a_judged_call_holds_for_its_execution`, `an_unjudged_call_resolves_live` and `a_pin_admits_no_more_fetch_than_was_judged` in `crates/tool/src/pins.rs`; `a_native_call_runs_inside_the_pins_its_judgment_fixed` in `crates/envd/src/server.rs` (over a connection under `always-ask`, the executor finds what the commit judgment pinned though the state moved while the user was asked) and in `crates/agent/tests/lifecycle_hooks.rs` (the kernel's admission and executor see the judgment's pin); `production_read_declares_its_fetches_and_local_reads_stay_read` (the production registry's `read`: its credentialed maximum, each target's envelope, and the policy of every mode and sandbox state, a local read allowed in all of them and a fetch asked only under `always-ask`; with `sv_fetch_enabled false` a URL read fetches nothing) and `read_queries_name_the_hosts_its_targets_fetch` (over a connection under `always-ask` a URL, `ssh://`, `issue://` and a JSON list reaching two hosts each ask once with the judged fetch and every named host; a local read never asks in any mode; under `write` and plan mode a URL read runs unasked) in `crates/envd/src/server.rs`; in `crates/driver/tests/approval_authority.rs`, `always_ask_asks_read_urls_once_per_host_per_session` (the production executor and approval desk against loopback upstreams: the first read of a host asks, a `session` answer lets later reads of it run, another port asks again, a local read never asks, and a read reaching both granted hosts runs unasked) and `an_embedded_kernel_asks_read_urls_once_per_host` (the same through kernel turns on the native route, admitted by `SettingsAdmission`).
- Search fetch (2026-10-09): `SearchPolicy`, `SearchRootKind`, `WorkspaceSearch::internal_root_fetch`, `WorkspaceSearch::walk_fetches`, `grep::Fault::UrlRootDisabled` and `grep@1`'s `invocation_effects`/`fetch_locators` in `crates/tools/src/grep.rs`; `glob@1`'s in `crates/tools/src/glob.rs`; `RootClass`, `classify_root`, `ast_grep::Fault::UrlRootDisabled` and `ast_grep@3`'s in `crates/tools/src/ast_grep.rs`; `FetchEffects::union` in `crates/tool/src/lib.rs`; `Resolve::walk_fetch` and `ResolverTable::walk_fetch` in `crates/tools/src/read/resolver.rs`, answered by `UrlResolver::walk_fetch` in `crates/envd/src/tool_url.rs`; `WorkspaceSearchAdapter`'s `internal_root_fetch` and `walk_fetches` with `resource_walk_target` and `walk_base`, which `resource_glob` also walks by, in `crates/envd/src/tool_search.rs`; `AstSearchAuthority`'s dispatch on `classify_root` in `crates/envd/src/tool_ast_grep.rs`; `production_search_policy` in `crates/envd/src/tools.rs`; `Registry::device_invocation_effects` and `Registry::device_fetch_locators` in `crates/tool/src/registry.rs`, which `DynHost::call_issued` (`crates/envd/src/devices_host.rs`) judges and names a device call with, inside `InvocationPins`, before `DynamicAdmission::admit` takes its `NamedFetches`. Proofs: `grep_judges_each_call_by_what_its_roots_fetch` (local paths, line selectors, archive members, `file://`, internal URIs that read local state, vault searches and unknown schemes are document reads; URLs anonymous fetches; `ssh://`, `issue://`, `pr://` and `mcp://` credentialed ones; every `;` root counted, each fetching root named once, a path refused before the search fetching nothing), `grep_maximum_holds_the_fetches_its_policy_permits` and `url_roots_are_refused_before_the_search_while_fetches_are_disabled` in `crates/tools/tests/it/grep.rs`; `glob_declares_the_remote_walks_its_resource_targets_take` in `crates/tools/tests/it/glob.rs`; `roots_are_classified_the_way_the_resolver_routes_them`, `url_roots_are_the_only_fetches_and_follow_the_fetch_policy` and `faults_keep_the_journaled_diagnostic_shape_beside_typed_cases` in `crates/tools/src/ast_grep.rs`; `fetches_follow_the_capability_the_scheme_serves` in `resolver.rs`; `devices_are_judged_on_the_target_their_path_resolves` in `registry.rs`; `dynamic_prompts_offer_the_session_and_key_fetches_on_their_hosts` (a partly unnamed fetch asks for its named host and the target's own) in `admission.rs`; `a_dyn_search_is_admitted_on_the_roots_it_reaches` (under `always-ask` a local `dyn ast_grep` runs unasked and a URL one asks only for `http:docs.rs:443`, and refused searches nothing) and `a_dyn_call_runs_inside_the_pins_its_judgment_fixed` (a widening call is refused unasked, with the registry's reason) in `devices_host.rs`; `production_search_tools_declare_their_remote_roots_and_local_roots_stay_read` (the production registry's maxima, each root's envelope and named hosts, a walk from the alias list unnamed, and the policy of every mode and sandbox state; with `sv_fetch_enabled false` no URL root fetches and the credentialed maxima stay) and `search_queries_name_the_hosts_their_roots_reach` (over a connection under `always-ask` each fetching search asks once with the judged fetch and its hosts and, refused, runs nothing; a local search never asks in any mode, and runs) in `crates/envd/src/server.rs`.
- Memory reflect inference (2026-10-07): `reflect_spec` in `crates/tools/src/memory.rs`; `bind_reflection` in `crates/driver/src/headless/reflection.rs`, called by `compose_kernel` with `ComposedInference::auxiliary_inference` (`AuxiliaryInference`, `crates/driver/src/headless/kernel.rs`, which shares `isolated_call_model` with `ProductionInference::chat_on`); `tool_cancellation` in `crates/agent/src/loop.rs` already makes a declared inference request a foreground, batch-exclusive call. Proofs: `reflect_declares_one_inference_request_and_its_siblings_declare_none`, `reflect_asks_its_host_exactly_once_for_recalled_evidence`, `reflect_without_evidence_never_asks_its_host`, `an_unbound_host_answers_with_the_recalled_evidence` and `a_failed_synthesis_is_a_typed_fault` in `crates/tools/src/memory.rs`; `reflect_is_exec_tier_and_only_explicit_yolo_runs_it_unprompted` in `crates/envd/src/tool_settings.rs`; `reflect_issues_exactly_one_inference_request_on_the_memory_role` (one `chat_on` on `@memory`, no tools, the instruction and the question, context and evidence), `a_refused_request_is_a_synthesis_fault` and `an_answer_without_text_is_a_synthesis_fault` in `reflection.rs`, through the envd `ReflectionBridgeHost`; and `crates/driver/tests/memory_reflect.rs`, a kernel turn on the embedded environment `ProjectEnvironment::attach` falls back to: the shipped default posture journals one `exec`-tier prompt and the approved call makes exactly one request, an explicit `yolo` prompts for nothing and makes one, and a refused call makes none. The attached daemon path: `an_attached_session_synthesizes_a_daemon_reflect_with_one_request` in `crates/driver/tests/memory_reflect.rs` (an in-process project daemon, no fallback; `retain` then `reflect` run on the daemon and the synthesis is exactly one request on the session's inference; without the `reflection-relay` capability it makes none), the `reflection_relay::tests` in envd, and `a_dyn_reflect_synthesizes_on_the_issuing_connection` in `crates/envd/src/devices_host.rs`.
- Not implemented, and no longer claimed: a git-push, `ln` or other capability unit; prediction of capabilities before execution (grep of `crates/shell`, `crates/envd/src/exec*`). The comment on `effects` in `crates/tools/src/shell.rs` once said the environment host admits exact filesystem, spawn and network effects as interpretation reaches those boundaries; it overstated the code and now says the empty declaration holds only because `ExecSandbox` confinement stands in for it.
- `crates/tools/src/shell_intercept.rs` offers rule-configured guidance toward dedicated tools; the grep-to-ripgrep routing in rule 2 is satisfied by the `grep` builtin itself.
- Unverified: whether a rerun can repeat side effects of the first attempt in practice. The design reruns from the start, so it can.

## References

- The Harness Playbook, "The tool surface" — "Deep builtins: Bash"
- 0006 (host policy / sandbox stub), 0010 (jobs), 0012 (convar policy), 0025 (`dyn` builtin)
- `crates/shell` (formerly `crates/shell-engine`), `crates/shell-builtins`, `crates/tools/src/shell.rs`
