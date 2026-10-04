# 0006. Policy on the trusted host; an obedient bounded stub in the sandbox

Status: accepted
Date: 2026-09-02
Area: runtime

## Context

The Factorio row of the envelope (0001) forces the placement question: when the repository and
tool input are hostile and no human is present, which process runs the tools? Three placements were
tried on paper; two fail.

**Executor in the VM.** The harness on the host asks a VM-resident executor to run tools. This
breaks on two facts:

- Programmatic tool use (Code mode, `Eval`) needs every tool in one namespace. Tools that touch
  harness state (session tree, settings, subagent spawn) and tools that touch environment state
  (files, processes) cannot be split by side, so the VM would need to reach back to the host.
- That reach-back is a duplex gateway from untrusted code into the trusted process. Either the
  gateway is open — and the VM can DoS the host — or it is rate-limited, and the harness is now
  rate-limiting its own VM on certain actions. Both outcomes add a second protocol for no gain.

**Driver in the VM.** Move the whole harness inside. Now the app's prompts and internal source are
inside the untrusted boundary. Fixing that means moving the app out and connecting over network RPC,
and moving session storage out — which requires granting the VM write access to session storage.
That is the duplex gateway again, with the same DoS and rate-limit problems, plus the leak.

**Stub in the VM.** A single obedient stub executes environment requests and streams results back.
The remaining hazard is bandwidth: a misused `Read` can return 2 GB, and the host must not let an
untrusted stream exhaust its memory or the model's context.

## Decision

The trust boundary is fixed as follows.

- The **host** MUST own session state, inference, policy, tool routing, approval, limits, and
  journaling. None of these move into the sandbox under any deployment.
- The **sandbox** MUST own only environment execution, reached through a small, obedient protocol.
  The stub NEVER initiates calls into host authority; it answers requests. The one bounded
  exception is the eval parent bridge, defined below.
- Every stream that crosses back from the sandbox MUST be bounded on the host side before the
  untrusted side can exhaust host memory or context (0009). Bounding is a property of the transport
  and the call-outcome path, not of the tool that happened to be invoked.
- The same host MUST be able to point the stub at a local process, a container, a VM, or a remote
  machine. Local use is the stub running in-process; it is not a different architecture. Today the
  environment side of every target is the full `omp-envd` binary. A separately minimized stub for
  remote, container, and VM targets is deferred to 0039 (remote control and factory workers, which
  currently place a full envd on each worker) and is not part of the decision in force.
- Extensions and custom tools are environment execution: they run on the stub side of the boundary
  or in a host-supervised unit the host can kill (0011), never inside the host's authority.
- **Exception: the eval parent bridge.** Code running in an `Eval` cell on the stub side MAY call
  back into the live parent session through `omp_envd::eval::ParentSessionHost`
  (`crates/envd/src/eval/bridge.rs`), which the host binds per session owner. The exception is
  bounded as follows.
  1. It is a host-granted capability, not an open gateway. The host binds the parent; each eval
     run holds an authenticated grant; a call is dispatched only if its name is in the grant's
     capability set, and the host withholds the name for any operation its parent does not
     implement.
  2. The callee is host code. Every bridged operation MUST be authorized by host-side policy
     (convars, limits, approval), never by anything the stub supplies. The stub receives only the
     typed result.
  3. The binding is a revocable lease. Retiring the parent binding revokes later
     calls.
  4. The set of bridged operations is closed: `__completion__`, `__agent__`, `__workpool__`,
     `__concurrency__`, `__budget__`, plus extension prelude helpers. Adding one is an amendment
     to this record, not a driver detail.
  5. It is not a general duplex channel. Its behavior on remote targets is unspecified until 0039
     designs the remote worker protocol.

## Consequences

- Factorio is satisfied without making local use worse: the local path is the remote path with a
  cheaper transport.
- Duplex gateways from sandbox to host are prohibited, except the bounded eval parent bridge above.
  A tool that needs harness state is routed by the host; the sandbox never sees session storage.
- Code mode and `Eval` keep one tool namespace because routing is a host concern (0025, 0036).
- Cost accepted: extension authors see two filesystems — the host's and the sandbox's. Making that
  pleasant is the job of `@remote` (0036), not a reason to blur the boundary.
- Cost accepted: every effect crosses a framed protocol even locally. The frame is bounded and
  typed, which is the point.

## Amendment (2026-10-04)

The owner decided to keep the trust-boundary decision and to record the one place where the code
deliberately lets stub-side code call back into the host. `Eval` needs parent-session operations
(child workpools and, in the interface, completions, agents, concurrency and budget controls) from
inside a cell, and the code provides them through the authenticated `ParentSessionHost` bridge.
Rather than leave a standing contradiction with 'the stub NEVER initiates calls into host
authority', the decision now carves out that bridge as a bounded exception with explicit
conditions, the main one being that host-side policy, not the stub, authorizes each operation.
The owner also deferred the 'minimized stub for remote targets' part: today every target runs the
full `omp-envd` binary, and a smaller obedient stub is left to the remote-control and factory-worker
work in 0039, so it is no longer asserted here as current behavior. Whether the bridge as built meets
condition 2 is audited in the status section below, which records one open gap.

## Status in omp

**Status: Partially implemented.** Host-owned policy, approval and bounded transport are in place, and the eval parent bridge exists as the bounded exception. One open gap: nested native tool calls made through the bridge are not individually admitted by host policy. The minimized remote stub is deferred to 0039. (Verified 2026-10-04 against `omp2` at `9b2d91fe9d`.)

- Host policy and bounded environment transport: `crates/envd/src/server.rs`, `crates/envd/src/tools.rs` (hook approval descriptions merged into one durable ticket by `crates/agent/src/{hooks,approvals,dispatch}.rs`), `crates/envd/src/{exec,process_store}.rs` (process leases, generation fencing, tree cleanup).
- Extensions and eval run in supervised child processes the host can kill (`crates/envd/src/{worker,exthost}`), consistent with the last rule.
- Minimized stub: not implemented. Remote, container and VM targets would run the full `omp-envd` binary. Deferred to 0039.
- Eval parent bridge, mechanism: `ParentSessionHost` (`crates/envd/src/eval/bridge.rs`, trait at line 831) is bound per session owner by `ProjectEnvironment::bind_eval_sdk_parent` (`crates/envd/src/lib.rs`), called from `crates/driver/src/headless/kernel.rs` (around line 2405). Each run holds a `BridgeGrant` (token and generation checked in `BridgeDispatcher::dispatch`); a name outside the grant's `BridgeCapabilities` is refused (`CapabilityDenied`); `SessionBridgeHost::capabilities` advertises `__completion__`, `__agent__`, `__concurrency__`, `__budget__` and `__workpool__` only when every bound parent reports the operation available; the binding is a `ParentBindingLease` and a revoked generation fails later calls (`eval bridge parent lease was revoked`). Behavior on remote targets is not verified.
- Eval parent bridge, production composition: the only `ParentSessionHost` implementations in the tree outside tests are `WorkpoolSessionHost` and `WorkpoolParentHost` (`crates/driver/src/subagent/workpool_scheduler.rs`). `WorkpoolSessionHost` reports completion, agent, concurrency and budget unavailable, so the production grant carries `__workpool__` and no other parent operation. The other four names are defined in the trait and exercised only by test hosts (`crates/app/tests/it/envd_contract.rs`, `crates/envd/src/eval/process.rs`).
- Is each bridged call authorized by host policy? `__workpool__`: yes, in the host's own terms. `SchedulerRegistry::bridge_call` refuses eval-defined tools unless `sv_eval_tools_enabled` (`ConWorkpoolPolicy::eval_tools_enabled`), and `KernelWorkpoolLauncher::spawn` (`crates/driver/src/subagent/workpool_runtime.rs`) refuses a worker past the `sv_task_recursion_depth` ceiling, for an agent in `sv_task_disabled_agents`, or at `sv_task_max_concurrency` active subagents. Workers are child kernels built through `compose_kernel` in isolated workspaces, so their tool calls use the child's normal loop. No interactive approval or `AdmissionGate` runs for the bridge call itself: the enclosing `eval` invocation is admitted once with the effect envelope from `eval_effects()` (`crates/tools/src/eval.rs`, line 1304: any command with network, writes to `**`, `subagents: u32::MAX`, unbounded inference), so pool creation and push are bounded by the convar limits above, not by a per-call approval. `__completion__`, `__agent__`, `__concurrency__`, `__budget__`: no production implementation, so not reachable; a future host MUST enforce rule 2 of the exception when it adds one.
- Open gap (not policy-checked): native registry tools called from eval through the same bridge (`tool.<name>()` in the Python prelude). `SessionBridgeHost::capabilities` grants every native registry tool except `eval`; `RegistryBridgeHost::call` (`crates/envd/src/eval/bridge.rs`, line 725) calls `Registry::invoke` directly (line 754). That path runs no `AdmissionGate` (`crates/envd/src/admission.rs`), resolves no `sv_tools_approval_mode` tier or per-tool `sv_tools_approval` override, and fires no admission hooks; `DynamicAdmission` is wired only for the shell `dyn` builtin. Approval for those calls is whatever approval the enclosing `eval` call received, so a cell can call `bash`, `write`, `edit` and the rest without a per-call decision. These calls go to envd-resident tools rather than into the parent session, so they fall outside the exception's list of parent operations, but they use the same channel and the same host-binds-the-grant model. Whether the target tool's own enforcement (for example the exec sandbox, or its approval route, which is bound per invocation) applies to a bridged call is not verified. Owner decision needed on whether to admit nested calls individually or to accept eval-level approval as the unit.

### Implementation notes (carried over)

Primary implementation: `crates/envd/src/server.rs`. Host policy and bounded environment
transport are implemented. The live `web_search@2` path is session-local policy: `omp-driver`
binds the one production inference facade into `omp-envd`'s search bridge, while provider HTTP
execution remains in the bounded host transport. Lifecycle-hook approval descriptions are generation-fenced in
`crates/envd/src/tools.rs`, then merged with native admission into one Core-owned durable ticket by
`crates/agent/src/{hooks,approvals,dispatch}.rs`; extension hosts never own or await the human
decision. Project daemon lifecycle stays behind the same typed boundary:
`crates/envd/src/{server,exec,process_store}.rs` owns readiness, durable generation fencing,
persistent-process idle leases, no-replace owner listeners, crash recovery, and bounded process-tree
cleanup, while `crates/app/src/{cli,ps_cmd}.rs` only dispatches and presents public operations. Gap:
remote/container/VM targets still use the full envd binary rather than a separately minimized stub.

## References

- The Harness Playbook, "The runtime" — "The sandbox should execute, not decide"
- 0001 (Factorio row), 0007 (filesystem form of this boundary), 0009 (bounding), 0011 (kill
  boundary), 0036 (`@remote` makes the boundary pleasant)
- `docs/architecture/processes.md`, `docs/architecture/crates.md`
- `crates/envd/src/{server.rs,admission.rs,exec_sandbox.rs,sandbox_proxy.rs,worker.rs}`,
  `crates/env/src/client.rs`
