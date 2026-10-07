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

### Amendment: nested native-tool calls (2026-10-04)

The owner closed the open gap with a middle path:

- **Eval-level approval is the unit of prompts.** The enclosing `eval` invocation is admitted once,
  under its own effect envelope and approval policy. A nested `tool.<name>()` call never opens an
  approval prompt of its own.
- **Nested native-tool calls obey explicit deny and roster restrictions, without prompting.** A
  nested call to a tool whose resolved per-tool policy is `deny` (`sv_tools_approval`) is refused,
  and so is a call the invoking request's roster forbids: the `sv_tools` allowlist, a mode
  Director's bind (Plan, Vibe), a `turn_start` hook's `enabled_tools`, and Plan's confinement of
  `write` to the plan file. The refusal is a typed error in the cell that names the tool and the
  reason; it is never silent.
- **Per-call admission is a future option only**, once route- or session-scoped approvals exist,
  so a nested prompt could be answered without interrupting the cell for every call.

The roster restrictions are the agent's, not the environment's, so they travel with the call: the
agent snapshots one `omp_tool::ToolRestrictions` per model request (the live `sv_tools`
allowlist and Director binds, the hook filter, the hidden mounts the session never mounted; the
advertised roster itself is latched per session, ADR 0024), checks every call against it before any preview or execution,
and forwards it on `InvokeTool.restrictions` (`omp.env.v1.ToolRestrictions`). The environment hands
it to the eval cell through `IncomingParams::restrictions` and `RuntimeSnapshot::restrictions`
(host-only, never serialized to the child), and the parent side of the bridge applies it to each
nested call. Explicit `deny` overrides are bound into the bridge at composition, as for the `dyn`
builtin's nested admission.

### Amendment: plan-mode write confinement (2026-10-05)

The owner decided four follow-ups to the nested-call amendment:

- **Plan writes its plan file.** `write` creates `local://PLAN.md` (and the `local:/` shorthand)
  inside the invoking session's scratch root, the directory beside its journal. Before, the tool
  refused every `local://` target, so Plan could not write its own default plan file.
- **Subagents of a plan-mode parent are read-only.** A child spawned while the parent runs under
  plan mode, or by a read-only parent, carries a read-only ceiling (`sv_tools_read_only`) that its
  own cfg cannot lift: inheritance is a ceiling, like the `task` recursion limit. The ceiling is
  part of the same per-request `ToolRestrictions` snapshot the dispatch check uses. The ceiling is
  fixed for the child's life, so the child's latched wire roster (ADR 0024) is also capped at the
  read-only tools from its first request; the parent's wire roster is never narrowed by Plan.
- **The environment confines writes as well.** An invocation whose restrictions carry a plan file
  or a read-only ceiling runs inside a write scope, and the environment's writers refuse every
  change the scope does not admit, with a typed error. The dispatch check stays: it gives the
  model-visible message, and the environment is the safety net.
- **Hooks on nested calls are not implemented.** Pre- and post-tool hooks fire only for the outer
  `eval` call, not for nested `tool.<name>()` calls inside a cell, because `omp-envd` dispatches
  no tool hooks. They will be addressed together with per-call admission (the proposed
  `sv_eval_nested_approval`) once route- or session-scoped approvals exist.

## Status in omp

**Status: Partially implemented.** Host-owned policy, approval and bounded transport are in place, and the eval parent bridge exists as the bounded exception. Nested native tool calls through the bridge obey explicit `deny` and the invoking request's roster restrictions without prompting, plan mode confines writes to the plan file both at dispatch and in the environment, and subagents of a plan-mode parent are read-only (amendments above); the remaining gaps are listed below. The minimized remote stub is deferred to 0039. (Verified 2026-10-04 against `omp2` at `9eb26730`; plan-mode items re-verified 2026-10-05 on `claude/plan-mode-hardening` from `omp2` at `6bfa10e3`, by reading and by the tests named below; the sandbox-amendment route of item 1 under Verified re-verified 2026-10-07 on `feat/envd-approval-relay-daemon` from `omp2` at `1b648d0f74`, by reading.)

- Host policy and bounded environment transport: `crates/envd/src/server.rs`, `crates/envd/src/tools.rs` (hook approval descriptions merged into one durable ticket by `crates/agent/src/{hooks,approvals,dispatch}.rs`), `crates/envd/src/{exec,process_store}.rs` (process leases, generation fencing, tree cleanup).
- Extensions and eval run in supervised child processes the host can kill (`crates/envd/src/{worker,exthost}`), consistent with the last rule.
- Minimized stub: not implemented. Remote, container and VM targets would run the full `omp-envd` binary. Deferred to 0039.
- Eval parent bridge, mechanism: `ParentSessionHost` (`crates/envd/src/eval/bridge.rs`, trait at line 831) is bound per session owner by `ProjectEnvironment::bind_eval_sdk_parent` (`crates/envd/src/lib.rs`), called from `crates/driver/src/headless/kernel.rs` (around line 2405). Each run holds a `BridgeGrant` (token and generation checked in `BridgeDispatcher::dispatch`); a name outside the grant's `BridgeCapabilities` is refused (`CapabilityDenied`); `SessionBridgeHost::capabilities` advertises `__completion__`, `__agent__`, `__concurrency__`, `__budget__` and `__workpool__` only when every bound parent reports the operation available; the binding is a `ParentBindingLease` and a revoked generation fails later calls (`eval bridge parent lease was revoked`). Behavior on remote targets is not verified.
- Eval parent bridge, production composition: the only `ParentSessionHost` implementations in the tree outside tests are `WorkpoolSessionHost` and `WorkpoolParentHost` (`crates/driver/src/subagent/workpool_scheduler.rs`). `WorkpoolSessionHost` reports completion, agent, concurrency and budget unavailable, so the production grant carries `__workpool__` and no other parent operation. The other four names are defined in the trait and exercised only by test hosts (`crates/app/tests/it/envd_contract.rs`, `crates/envd/src/eval/process.rs`).
- Is each bridged call authorized by host policy? `__workpool__`: yes, in the host's own terms. `SchedulerRegistry::bridge_call` refuses eval-defined tools unless `sv_eval_tools_enabled` (`ConWorkpoolPolicy::eval_tools_enabled`), and `KernelWorkpoolLauncher::spawn` (`crates/driver/src/subagent/workpool_runtime.rs`) refuses a worker past the `sv_task_recursion_depth` ceiling, for an agent in `sv_task_disabled_agents`, or at `sv_task_max_concurrency` active subagents. Workers are child kernels built through `compose_kernel` in isolated workspaces, so their tool calls use the child's normal loop. No interactive approval or `AdmissionGate` runs for the bridge call itself: the enclosing `eval` invocation is admitted once with the effect envelope from `eval_effects()` (`crates/tools/src/eval.rs`, line 1304: any command with network, writes to `**`, `subagents: u32::MAX`, unbounded inference), so pool creation and push are bounded by the convar limits above, not by a per-call approval. `__completion__`, `__agent__`, `__concurrency__`, `__budget__`: no production implementation, so not reachable; a future host MUST enforce rule 2 of the exception when it adds one.
- Nested native-tool calls (`tool.<name>()` in the Python prelude), formerly the open gap. `SessionBridgeHost::capabilities` still grants every native registry tool except `eval`, and `RegistryBridgeHost::call` still calls `Registry::invoke` directly, but every nested registry call now passes `SessionBridgeHost::admit_nested` first (`crates/envd/src/eval/bridge.rs`): an explicit `deny` override refuses with `BridgeHostError::ApprovalDenied`, then the cell's `ToolRestrictions` refuse with `BridgeHostError::RosterRestricted` (the same `RosterDenial` text the agent journals). Both surface in the cell as a `RuntimeError` naming the tool and the reason, exactly as other bridge errors surface. `allow` and `prompt` policies, and the approval mode's tier, are not consulted for nested calls: the `eval` admission is the prompt. The privileged parent operations (`__workpool__` and the others) and prelude helpers are not roster tool names and are not roster-checked.
- Agent-side dispatch check (the guard `crates/agent/src/directors/plan.rs` refers to): `Dispatcher::check_roster*` against the request's snapshot, at `ToolCallStarted` (before an execution unit opens, so a refused `edit` never previews), at the non-streamed `ToolCallReady`, after a `tool_call` hook rewrites the target, on `retry_tool_tail`, for provider workflow actions, and before inline sloppy-edit recovery. It covers native, session and host tools. A refused call is journaled faithfully and settles as `CallOutcome::policy_denied` with code `tool.roster.restricted` (`Dispatcher::deny_prepared`). Plan's `write` opens no unit until its committed `path` passes the plan-file check (`omp_tool::plan_target_matches`: lexical, fail-closed; `..`, backslashes, percent escapes, absolute-vs-relative mismatches and any other spelling are refused).
- Verified (by reading, unless a test is named):
  1. `bash` through the bridge: the exec sandbox applies, because `ExecHost` compiles it from host settings for every shell session it opens (`crates/envd/src/exec.rs`, `active_sandbox`), whoever calls the tool. The per-invocation command approval does not apply: it is the server's `AdmissionGate` for an `InvokeTool`, and a bridged call has none; the `eval` admission stands in for it. The sandbox-amendment prompt (a one-shot rerun after a sandbox denial) goes to the relay of the connection that issued the command when there is one, else to the route bound process-wide on `ExecHost` (`bind_sandbox_approval_route`; see 0028). A bridged call runs in a task of its own (`bridge_tasks.spawn` in `crates/envd/src/eval/process.rs`), outside the invocation's `tools::invocation_approvals` scope, so a bridged `bash` carries no relay. In an in-process composition it can still raise that prompt through the host route; where no route is bound, as on a project daemon, its amendment fails closed. This is a pre-existing exception to "eval-level approval is the unit of prompts".
  2. Pre- and post-tool hooks: none fire for nested calls. `tool_call` (gate and transform), `call_open`, `tool_execution_start`/`_end` and `tool_result` are fired by the agent loop and dispatcher for the enclosing `eval` call only, and `omp-envd` dispatches no tool hooks at all. Not implemented, by decision: they come with per-call admission (amendment above).
  3. Plan and `write`. Dispatch confines `write` to the plan file while Plan is active (agent test `plan_mode_refuses_hidden_tools_and_off_plan_writes_at_dispatch`, envd tests `nested_calls_obey_the_invocation_roster_and_plan_file_scope` and `cell_nested_calls_obey_the_invocation_roster`); `edit`, `ast_edit`, `bash` and `eval` are not in `PLAN_TOOLS` and are refused by name. `hub`, which is in `PLAN_TOOLS`, could start, restart and feed input to processes; while plan mode or the read-only ceiling applies it keeps its peer and observation operations only (rule `director:plan/processes`, agent test `plan_mode_keeps_hub_off_processes`). `write` to `local://` was refused by the tool itself before this change (`reject_uri_like_target`: "local:// targets are not supported yet", reproduced by running `local_targets_reach_the_documents_as_plain_writes` against the old tool), so Plan could not write its default plan file; it now resolves inside the invoking session's scratch root (`crates/envd/src/tool_url/local.rs`, envd tests `write_creates_the_local_plan_file_in_the_session_scratch_root`, `local_writes_refuse_symlinks_that_leave_the_scratch_root`). That root is now the directory beside the session's journal (`<sessions>/<stem>/local`), which the app's plan review, session deletion and `omp gc` use; before, `local://` was keyed by the dispatcher's principal (the SHA-256 of the journal path), so the plan review could not see a plan the agent wrote and `omp gc` treated the directory as orphaned (envd test `a_journaled_session_writes_its_plan_beside_its_journal`).
  3a. Environment write scope (`crates/envd/src/write_scope.rs`). The two guards found dead are resolved. The `InvocationExecutionPolicy` plan denial (`crates/envd/src/server.rs`) no longer reads the untyped `omp/execution-mode` and `omp/plan-yolo` props, which no client set: it derives a `WriteScope` from the typed `InvokeTool.restrictions` the agent already sends (`plan_file`, and the new `read_only_ceiling`), runs the native executor inside it, and at `ArgsCommitted` refuses a tool that could write around the scoped writers (an envelope that runs commands, spawns subagents, or writes documents through anything but `write`, `edit` and `lsp`). Its old exemption of every `local://`, `vault://` and `sandbox://` target and the yolo prop are removed: the scope admits the plan file only, and the yolo hand-off exits Plan, so the next request carries no plan file. `omp_edit::PathPolicy::plan_active` and `enforce_write` are removed: they belonged to the `omp_edit::Session` pipeline, which no production writer uses, and their rule (any `local://` file writable) was looser than the plan-file rule. Inside the scope, every write the environment owns is admitted only for the plan file: the document authority client (`crates/envd/src/docs.rs`: text, create, delete and move transactions, including lease-addressed ones, which fail closed; directory, removal, rename, copy, link and permission requests) and the tool document host's direct writers (`crates/envd/src/tool_document.rs`: plain writes outside the workspace, `local://` included, before any parent directory is created; archive members; SQLite rows; SSH, vault and RPC-host resources). The refusal is the typed `omp_tool::WriteScopeDenied`: `DocumentError::WriteScope` at the authority client, and the write tool journals it as its durable `Fault::WriteScope` (special writes through `backends::Fault::WriteScope`), not as `Fault::Document` text (omp-tools test `write_scope_denials_are_durable_typed_faults`). The scope is per invocation, so it clears when Plan ends. Nested `tool.<name>()` calls run on bridge tasks in the scope of the `eval` invocation that started the cell (envd test `cell_nested_calls_run_in_the_invocations_write_scope`), although `eval` itself is refused at the boundary while a scope applies. Joined proofs over the environment wire: `plan_mode_invocations_change_only_the_plan_file` (off-plan writes refused, the plan file written, `ast_edit` refused at the boundary, an unscoped write after Plan succeeds; `bash` is covered by the server unit test `write_scopes_refuse_tools_that_write_around_the_scoped_writers`) and `read_only_subagent_invocations_change_nothing`. Writers that bypass the scope: processes (`bash`, `eval`, `hub start`, `dyn` devices; refused by name at dispatch and, for envd invocations, at the boundary); `ast_edit`, which stages and applies with `std::fs` (refused by name and at the boundary); the eval prelude's own `write()` helper, which writes from the Python worker (eval is refused); the privileged mutation intent (`PrivilegedMutationIntent`, a human-approved, ticketed connection request outside any invocation); language-server-initiated edits (`workspace/applyEdit`) and DAP sessions inside the document server; extension-host (worker-route) tools, which the boundary refuses only when their declared envelope writes; and user or editor writes, which are deliberately outside any invocation.
  4. Other paths that reach a tool without the request's roster: RPC host tools (`set_host_tools`) and session tools are dispatched by the agent loop and are covered. Provider workflow actions, `retry_tool_tail` and inline sloppy-edit recovery were in the same class and are fixed. Subagent compositions (`task`, `hub`, workpool workers) run child kernels with their own composed roster. Verified: `Ctx::seed_child` seeded the parent's effective `sv_tools` (Plan's bind included, `write` with it) as a plain inherited allowlist, and the child's class cfg replaces it (driver test `children_of_a_plan_mode_parent_inherit_a_read_only_ceiling`); Plan's plan-file rule never reached the child. Now every child of a plan-mode parent, and every descendant of a read-only child, carries `sv_tools_read_only` (`configure_child`, `crates/driver/src/subagent/spawn.rs`), set after its cfgs and script read-only, and its kernel caps the advertised roster and dispatch at `PLAN_READ_ONLY_TOOLS` (rule `parent:plan/read_only`, agent test `a_read_only_subagent_cannot_write_whatever_its_allowlist_says`); its `InvokeTool.restrictions` carry the ceiling, so the environment refuses its writes too. A child revived after the parent left Plan is not read-only. Still open: eval-defined tool handlers forwarded from workpool workers (`EvalForwardExecutor`) run their cell with no roster snapshot and no write scope, so only explicit `deny` applies to their nested calls; the privileged bridge operations (`__workpool__` and the others) and prelude helpers are not roster tool names and are not roster-checked. `dyn` builtin devices keep their own `DynamicAdmission` (explicit deny plus prompt) and are reachable whenever `bash` is. Extension-host connections call tools through `InvokeTool` with an `AdmissionGate` but carry no agent roster, because they are not model calls. The standalone CLI tool command (`crates/app/src/standalone_tool_cmd.rs`) is user-invoked, not a model call. Also found: the `ResourceMaterializer` and MCP manager key their `local://` root by the composition's runtime id, a third root unrelated to the session's.

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
