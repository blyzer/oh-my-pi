# Single-session swarm assessment

> Part of the [OMP2 gateway-capabilities research](README.md). Deliverable 9. Audited commit `85d219ae7f` (`origin/omp2`). Research proposal: nothing here is implemented.

Status: first pass. Written under a request budget, so several areas are labelled `not examined` instead of guessed. Evidence: [omp2-swarm-adw.md](evidence/omp2-swarm-adw.md) and [omp2-swarm-adw.tsv](evidence/omp2-swarm-adw.tsv) (rows W01 to W16), [ref-agent-harness.md](evidence/ref-agent-harness.md). Diagrams: [13](diagrams/13-single-session-logical-swarm.md), [14](diagrams/14-hybrid-swarm-with-lightweight-omp-subprocesses.md), [18](diagrams/18-resource-aware-scheduling.md). Code was read in the worktree `/private/tmp/omp-gateway-research` (same tree as `85d219ae7f`).

## 1. Answer in one paragraph

OMP2 already runs a single root session coordinating logical agents: every `task` child is a child kernel composed in the same process, journaled, cancellable, bounded by a count and a recursion depth, and given its own copy-on-write workspace that returns a content-addressed patch. The orchestrator, foreman and supervisor responsibilities mostly map onto existing code (ADW, the job board, the workpool, envd worker supervision, the cancellation tree). The measurable gaps are narrow: the pool limit is a bare count with no CPU, memory or provider-pressure input; the default per-child wall-clock limit is `Never`; nothing is measured about per-child composition cost; and the out-of-process "headless worker" form is only proposed (ADR 0039 Part B). No new orchestration layer is justified by the evidence read. The default recommendation is to extend `WorkpoolPolicy`, not to add a swarm crate.

## 2. Verified base (what exists)

| Fact | Evidence | Maturity |
|---|---|---|
| No swarm crate or tool; `vibe` mode prompt tells the model to coordinate through the session orchestration surface | `crates/agent/prompts/modes/vibe.md:1` (parent section 8) | Implemented (prompt only) |
| Child agent = child kernel in the same process, spawned by `DriverSubagentSpawner` | `crates/driver/src/subagent/spawn.rs:89 DriverSubagentSpawner`, `crates/driver/src/headless/kernel.rs:2457 compose_kernel` | Implemented |
| Child runs as a detached tokio task that settles a job | `crates/driver/src/subagent/spawn.rs:903 tokio::spawn` | Implemented |
| Every composed child gets isolation unconditionally (ADR 0007) | `crates/driver/src/subagent/spawn.rs:927 create_isolation` | Implemented |
| Isolation is per-file copy-on-write clone (APFS `clonefile`, Linux reflink) with copy fallback, built from a content-addressed manifest of the live generation | `crates/envd/src/workspace/operations.rs:673 create_worktree`, `crates/envd/src/workspace/operations.rs:2059 clonefile`, `docs/adr/0007-subagent-filesystem-isolation.md:31` | Implemented |
| Merge is a three-way manifest merge with structured conflicts and a content-addressed recovery artifact; merges are serialised by one mutex | `crates/envd/src/workspace/operations.rs:789 merge_worktree` | Implemented |
| Worktree records are durable and reloaded when envd opens | `crates/envd/src/workspace/operations.rs:1540 load_worktree_records` | Implemented |
| Job board is a runtime supervision index; durable truth is the `<meta><jobs>` tree in the session DOM | `crates/agent/src/jobs.rs:1`, `crates/agent/src/jobs.rs:699 JobBoard`, `crates/agent/src/jobs.rs:1095 rebuild` | Implemented |
| Fenced factory lease (worker id, `lease_gen`, deadline) on `JobKind::Subagent` jobs, with accept, renew and release functions | `crates/agent/src/jobs.rs:473 FactoryLease`, `crates/agent/src/jobs.rs:515 accept_lease`, `crates/agent/src/jobs.rs:522 renew_lease`, `crates/agent/src/jobs.rs:546 release_lease` | Partial (lease primitives exist; the lease TTL convar is `SV_FACTORY_LEASE_TTL` at `crates/driver/src/subagent/settings.rs:356`; no fleet transport found) |
| Workpool scheduler: queue and worker selection over the job primitive; limit read live from a convar | `crates/driver/src/subagent/workpool_scheduler.rs:101 WorkpoolPolicy`, `crates/driver/src/subagent/workpool_scheduler.rs:111 ConWorkpoolPolicy` | Implemented, count-only |
| Limits per child: concurrency 32, recursion depth 2, runtime `Never`, soft request budget 200, idle park 420 s, LSP in children off | `crates/driver/src/subagent/settings.rs:123 SV_TASK_MAX_CONCURRENCY`, `crates/driver/src/subagent/settings.rs:106 SV_TASK_MAX_RECURSION_DEPTH`, `crates/driver/src/subagent/settings.rs:144 SV_TASK_MAX_RUNTIME`, `crates/driver/src/subagent/settings.rs:166 SV_TASK_SOFT_REQUEST_BUDGET`, `crates/driver/src/subagent/settings.rs:248 SV_TASK_AGENT_IDLE_TTL`, `crates/driver/src/subagent/settings.rs:237 SV_TASK_ENABLE_LSP` | Implemented |
| Cancellation is hierarchical (commit token versus interrupt token), answered by cooperative settlement, grace, then forced termination journaled as uncertainty | `crates/agent/src/cancel.rs:1`, `crates/driver/src/subagent/spawn.rs:957 RunControl` | Implemented |
| Process supervision for envd-owned children: own process group, TERM, grace, KILL, reap, shared by MCP stdio and delegated agent adapters | `crates/envd/src/child_tree.rs:1`, `crates/envd/src/worker_pool.rs:1` | Implemented |
| ADW is a pure state machine; replay rebuilds a `Run` from recorded facts; the driver resumes a run from its journal | `crates/adw/src/run.rs:173 Run`, `crates/adw/src/replay.rs:1`, `crates/driver/src/adw/mod.rs:220` | Implemented (resume path exists) |
| Executor registry routes native OMP2, delegated agents and harnesses through one contract | `crates/driver/src/delegated_host.rs:19 ExecutorRegistry`, `crates/contract/src/delegated/executor.rs:217 Executor` | Partial (ADR 0052 row) |

Drift found: `docs/adr/STATUS.md:69` says ADR 0051 is "Proposed 2026-11-12; not started" and that `Run` is in memory only. Code has `crates/adw/src/replay.rs:1` ("ADR 0051 R4") and a journal resume entry at `crates/driver/src/adw/mod.rs:220`. The STATUS row is stale for the replay part; what is not verified here is whether production `omp adw run` journals every transition by default. ADR 0007's amendment at `docs/adr/0007-subagent-filesystem-isolation.md:81` also says a concurrent ADW driver is not implemented; that was not re-checked.

Not examined (so not claimed): bodies of ADR 0045, 0053 and 0056 beyond their STATUS rows (`docs/adr/STATUS.md:63`, `docs/adr/STATUS.md:71`, `docs/adr/STATUS.md:74`); `crates/driver/src/subagent/revive.rs`; `crates/driver/src/subagent/hub.rs`; `crates/tools/src/hub.rs`; `crates/vcs` and `crates/sandbox` internals; the body of `compose_kernel` past line 2510.

## 3. Responsibility map: orchestrator, foreman, supervisor

The owner brief (section 8) asks for responsibilities, not components, and not mandatory LLM agents. ADR 0039 already separates the roles (`docs/adr/0039-remote-control-and-factory-modes.md:618`, D3): the Foreman is an LLM role that proposes, ADW is the orchestrator, the coordinator is deterministic Rust, the scheduler is the workpool and placement, and "no model output can change a lease, a permission or a recorded verdict".

| Responsibility | Deterministic or LLM | Existing code | Status | New layer needed? |
|---|---|---|---|---|
| Objective decomposition | LLM (semantic) | The model calls `task` with a batch (`crates/tools/src/task.rs`, `DriverSubagentSpawner::spawn` batch admission at `crates/driver/src/subagent/spawn.rs:125`) | existing | No |
| Workflow planning, dependency coordination, sequencing, retries | Deterministic | ADW `Workflow` and `Run` (`crates/adw/src/workflow.rs:167`, `crates/adw/src/run.rs:173`) | existing, concurrent phase driver not verified | No |
| Agent assignment (class, model, effort ceiling) | Deterministic | `admit_child` and class cfg (`crates/driver/src/subagent/spawn.rs:459 admit_child`, `SV_TASK_MAX_EFFORT`) | existing | No |
| Cross-workstream coordination | Deterministic plus LLM | ADR 0039 D1 workstream record (Proposed text; `docs/adr/0039-remote-control-and-factory-modes.md:606`) | absent in code | No; it is an ADR 0039 item |
| Workstream ownership | Deterministic | Job `owner` and `label` on `JobRecord` (`crates/agent/src/jobs.rs:421 JobRecord`) | partial | No |
| Conflict prevention | Deterministic | One CoW view per writer plus three-way merge with conflicts (`crates/envd/src/workspace/operations.rs:789 merge_worktree`) | existing | No |
| Deliverable integration | Deterministic, with parent policy | `finish_isolation` (`crates/driver/src/subagent/spawn.rs:1042 finish_isolation`): patch artifact, optional apply, conflicts list | existing | No |
| Progress and evidence aggregation | Deterministic | Settled children return `ChildResult` with tokens, output and workspace outcome (`crates/driver/src/subagent/spawn.rs:994`); ADW evidence is ADR 0053 | partial | No |
| Lifecycle, cancellation, timeouts | Deterministic | `crates/agent/src/cancel.rs:1`, `RunControl` deadline from `max_runtime_ms` (`crates/driver/src/subagent/spawn.rs:945`) | existing; default runtime `Never` | No |
| Process supervision | Deterministic | `crates/envd/src/child_tree.rs:1` | existing | No |
| Resource enforcement | Deterministic | Count (`WorkpoolPolicy`), recursion depth, soft request budget; no CPU or memory input | partial | Extend `WorkpoolPolicy`, no new layer |
| Health monitoring | Deterministic | Lease deadline and renew (`crates/agent/src/jobs.rs:522 renew_lease`); worker pool supervision in envd | partial (no child-kernel health probe found) | No |
| Restart and recovery | Deterministic | `JobBoard::rebuild` adopts or orphan-settles after restart (`crates/agent/src/jobs.rs:1095 rebuild`); ADW replay | partial; auto-restart of a failed child not found | No |

Conclusion: a separate Orchestrator or Foreman process or crate is not supported by the evidence. A Foreman may exist as a prompt-level role of the root session (ADR 0039 D3), which costs no code.

## 4. Shared versus duplicated per child

This answers the "do not duplicate" list in owner brief section 7. A cell says `shared`, `duplicated`, `inherited by seeding` or `not examined`. Only what the code showed is claimed.

| Service | Per-child behaviour | Evidence | Note |
|---|---|---|---|
| Environment authority (envd: workspace, exec, blobs) | shared: one `EnvClient` in the spawner | `crates/driver/src/subagent/spawn.rs:104 env` | Children do not start their own envd |
| Configuration store | inherited by seeding, not shared state: child gets a seeded context | `crates/driver/src/subagent/settings.rs` `seeded_child_ctx`; `crates/driver/src/subagent/spawn.rs:107 cfg` | ADR 0013 |
| Session registry | shared (passed as `sessions`) | `crates/driver/src/subagent/spawn.rs:99 sessions` | live-session routing |
| Session journal, DOM, `.oms` file | duplicated by design: each child has its own journal | `crates/driver/src/headless/kernel.rs:2457 compose_kernel` creates a session | independent context, required |
| Workspace | duplicated as CoW view (cheap per file, cost grows with file count) | `crates/envd/src/workspace/operations.rs:673 create_worktree` | gitignored files are not copied (ADR 0007 status) |
| Artifacts / blobs | shared content-addressed store | `crates/envd/src/workspace/operations.rs:820 write_manifest_diff` via `BlobHost` | patch is `artifact://sha256/...` |
| Workspace trust decision | inherited | `crates/driver/src/subagent/spawn.rs:113 workspace_trust` | |
| Native extension admission, tool registry, prompt | duplicated per composition (code runs `admit_native_extensions_contained_with_control` inside `compose_kernel`) | `crates/driver/src/headless/kernel.rs:2506 admit_native_extensions_contained_with_control` | cost not measured |
| Provider registry and credentials | not examined whether a child builds its own `ProductionInference` over a shared registry or reloads it | `crates/driver/src/headless/kernel.rs:266 ProductionInference` | read `compose_kernel` past 2510 before any claim |
| LSP | off in children by default; when on, whether servers are shared is not examined | `crates/driver/src/subagent/settings.rs:237 SV_TASK_ENABLE_LSP` | |
| Memory database | not examined | none | owner area for local inference and code intelligence |
| Embedding model, tiny models, code indexes | not examined | none | deliverables 10 and 11 |
| Job state | shared in parent's DOM | `crates/agent/src/jobs.rs:1` | children are job elements of the parent |

The honest finding: the large shared services (envd, blobs, session registry) are shared; whether the provider registry, credential keepers and memory database are duplicated per child is the open measurement (card SWM-08).

## 5. Criteria: logical agent, headless OMP child, sandboxed subprocess, external harness

Default is the logical agent (child kernel in the root process). Move right only when a listed trigger is true and logged.

| Form | Use when | Do not use when | Existing path |
|---|---|---|---|
| Logical agent (child kernel, in-process) | Default. Needs shared provider routing, shared envd, cheap spawn, journal-first resume. | Child must be killable by OS signal, memory-capped, or must survive root crash. | `crates/driver/src/subagent/spawn.rs:923 run_child` |
| Headless OMP child (same binary, separate process) | A crash or leak in one child must not take the root down; per-process memory or CPU limits are needed; independent lifecycle (long-lived, detachable); version-pinned worker on another host. | The only reason is parallelism (tokio already parallelises) or isolation of files (CoW already does that). | Proposed: ADR 0039 B1 `omp serve --worker`; no code found for `--worker` or `Fleet` (searched `crates/**/*.rs` and `*.proto`) |
| Sandboxed subprocess | Running untrusted code or hostile repository content; the unit is a command, not an agent. | The unit needs an LLM loop and provider access. | `crates/sandbox` and `crates/envd/src/exec_sandbox.rs`; not re-read here |
| External harness (delegated executor) | A different agent product must do the work; its own auth and loop are accepted; acceptance still comes from OMP2 evidence. | The task needs OMP2 tools or the shared journal inside the loop. | `crates/driver/src/delegated_host.rs:19 ExecutorRegistry`; process teardown shared with `crates/envd/src/child_tree.rs:1` |

Each trigger must be recorded as a reason string on the spawn so that it can be audited and counted; the existing spawn already journals class and depth in `<meta>` (`crates/driver/src/subagent/mod.rs:29`).

## 6. Reference harnesses (summary)

Full notes in [ref-agent-harness.md](evidence/ref-agent-harness.md); matrices: [Ve-ria_cotion](evidence/matrix-Ve-ria_cotion.tsv), [grape-dev_grape](evidence/matrix-grape-dev_grape.tsv), [limecloud_lime](evidence/matrix-limecloud_lime.tsv). Lime is GPL-3.0 and was studied only; nothing was copied. Cotion is a 342-line TypeScript CLI that runs workflows by launching an external agent binary per workflow with `execa` (`Ve-ria/cotion@b7a4307:src/run.ts`), concurrency by `Promise.allSettled`, a JSONL run log, no isolation. Grape is a multi-tenant Python and MySQL DevOps platform whose dispatch is a conditional-update SQL state machine with an aggregate sweeper (`grape-dev/grape@b31f8d8:src/domains/mgmt/org_dispatch.py`) and a crash-restart worker script (`grape-dev/grape@b31f8d8:start-worker.sh`); it is a hosted-service model, not a local agent harness. Lime is a Rust, Codex-shaped app server with a durable parent and child thread graph (`limecloud/lime@015b878:lime-rs/crates/agent-graph-store/src/types.rs`), a `spawn_agent` tool contract whose owner is supplied by the app server (`limecloud/lime@015b878:lime-rs/crates/tool-runtime/src/agent_control.rs`), a managed worktree crate (`limecloud/lime@015b878:lime-rs/crates/worktree/src/lib.rs`) and several sandbox crates (bodies not read). What transfers: deterministic state transitions with conditional updates and a sweeper (grape), a persisted spawn edge graph with open and closed status (lime), keeping the tool contract separate from the owner of agent state (lime). What does not transfer: SQL as the dispatch store (OMP2 already has the journal), a hosted multi-tenant service, and any copying of GPL code.

## 7. Capability cards

### SWM-01 Keep the logical child kernel as the default agent form
**Decision class:** Already implemented: reuse
**Existing implementation evidence:** `crates/driver/src/subagent/spawn.rs:89 DriverSubagentSpawner`, `crates/driver/src/headless/kernel.rs:2457 compose_kernel`, `crates/driver/src/subagent/workpool_scheduler.rs:101 WorkpoolPolicy`. Implemented, reachable from the `task` tool.
**Actual missing capability:** None for the default form.
**Alternatives considered:** One OS process per agent (heavier, loses shared envd and routing); native worker threads per agent (agents are async tasks, threads add nothing).
**Expected benefit:** memory and latency (no process start per agent); reliability from journal-first resume.
**Implementation complexity:** low, no change.
**Security implications:** Same trust boundary as the root; mitigated by per-child CoW view and inherited workspace trust (`crates/driver/src/subagent/spawn.rs:113 workspace_trust`).
**Maintenance cost:** none new.
**Performance measurement strategy:** not measured; see section 8, B1.
**Acceptance criteria:** documentation states the default; spawn journals class and depth.

### SWM-02 No new orchestrator; extend ADW only where it cannot drive a phase graph
**Decision class:** Partially implemented: extend
**Existing implementation evidence:** `crates/adw/src/run.rs:173 Run`, `crates/adw/src/workflow.rs:167` `Workflow`, `crates/adw/src/replay.rs:1`, `crates/driver/src/adw/mod.rs:220`. Implemented as a pure state machine with replay.
**Actual missing capability:** Concurrent independent phases in one run are described as unimplemented in `docs/adr/0007-subagent-filesystem-isolation.md:81`; not re-verified here.
**Alternatives considered:** A separate orchestrator service or crate (duplicates ADW dispatch decisions); LLM-driven orchestration only (non-deterministic state transitions, against owner constraint 4).
**Expected benefit:** reliability and latency of multi-phase work.
**Implementation complexity:** medium, owned by the ADW roadmap.
**Security implications:** none new if phases keep per-phase isolation.
**Maintenance cost:** low, same crate.
**Performance measurement strategy:** not measured; see B3.
**Acceptance criteria:** two independent phases dispatch concurrently, each with its own CoW view, and replay reproduces the same run.
**Follow-up:** confirm the concurrent driver status first; if present, reclassify as reuse.

### SWM-03 Do not add a dedicated Foreman component
**Decision class:** Unnecessary or redundant
**Existing implementation evidence:** `docs/adr/0039-remote-control-and-factory-modes.md:618` D3 (role split), `crates/driver/src/subagent/spawn.rs:1042 finish_isolation` (integration), `crates/envd/src/workspace/operations.rs:789 merge_worktree` (conflict detection). Foreman duties are partly existing code and partly a prompt-level role.
**Actual missing capability:** Workstream record above ADW runs is Proposed text only.
**Alternatives considered:** A Foreman LLM agent with authority over leases (rejected by D3: model output cannot change a lease, permission or verdict); a Foreman crate.
**Expected benefit:** none beyond what the root session and ADW give; avoids a duplicate scheduler.
**Implementation complexity:** not applicable.
**Security implications:** a Foreman with write authority over scheduling would widen the prompt-injection surface.
**Maintenance cost:** avoided.
**Performance measurement strategy:** not applicable
**Acceptance criteria:** foreman duties are listed in section 3 with owning code; no new crate named foreman.

### SWM-04 Supervisor duties are already covered; keep them where they are
**Decision class:** Already implemented: reuse
**Existing implementation evidence:** `crates/agent/src/cancel.rs:1`, `crates/envd/src/child_tree.rs:1`, `crates/driver/src/subagent/spawn.rs:945` (deadline into `RunControl`), `crates/agent/src/jobs.rs:1095 rebuild`. Implemented.
**Actual missing capability:** None for cancellation, TERM, grace, KILL and orphan settlement.
**Alternatives considered:** A supervisor crate or actor (would duplicate the cancellation tree and envd child tree).
**Expected benefit:** reliability.
**Implementation complexity:** low (documentation and tests only).
**Security implications:** forced termination is journaled as uncertainty, not success (`crates/agent/src/cancel.rs:1`).
**Maintenance cost:** none new.
**Performance measurement strategy:** not applicable
**Acceptance criteria:** a cancel test shows a child tree torn down with the TERM, grace, KILL ladder.

### SWM-05 Default wall-clock limit and recovery policy for children
**Decision class:** Partially implemented: extend
**Existing implementation evidence:** `crates/driver/src/subagent/settings.rs:144 SV_TASK_MAX_RUNTIME` (default `Never`), `crates/driver/src/subagent/spawn.rs:945` deadline, `crates/driver/src/subagent/settings.rs:166 SV_TASK_SOFT_REQUEST_BUDGET` (default 200). Implemented, opt-in.
**Actual missing capability:** No default hard runtime; no restart policy for a failed child (a failed child settles `failed`; the parent decides). `crates/driver/src/subagent/revive.rs` was not read and may cover part of this.
**Alternatives considered:** Supervisor-side automatic restart (risks repeating side effects; ADR 0039 D6 says recovery is explicit, never automatically successful).
**Expected benefit:** reliability, bounded cost.
**Implementation complexity:** low (a default and a documented policy).
**Security implications:** none; bounding reduces runaway risk.
**Maintenance cost:** low.
**Performance measurement strategy:** not applicable
**Acceptance criteria:** a child over its limit settles with a `timed out` reason and its view is discarded; restart is a parent decision recorded in the journal.

### SWM-06 Resource-aware admission in the workpool
**Decision class:** Requires benchmarking
**Existing implementation evidence:** `crates/driver/src/subagent/workpool_scheduler.rs:101 WorkpoolPolicy` (count, fresh-agents flag, eval-tools flag; no CPU or memory input), `crates/driver/src/subagent/settings.rs:123 SV_TASK_MAX_CONCURRENCY`. Implemented as a count only.
**Actual missing capability:** Admission that reacts to host memory pressure, open-file or disk pressure for CoW views, and provider rate-limit state. Whether it is needed is not measured.
**Alternatives considered:** A new scheduler crate (rejected; the trait already takes a live policy); lowering the default count (cheap, blunt).
**Expected benefit:** reliability under memory pressure; latency stability.
**Implementation complexity:** medium: add inputs behind the existing trait.
**Security implications:** none.
**Maintenance cost:** medium (platform probes).
**Performance measurement strategy:** plan B2 and B4 in section 8. not measured.
**Acceptance criteria:** with 32 children on a loaded machine the pool queues rather than exhausts memory; policy stays a trait method.
**Follow-up:** extend `WorkpoolPolicy` if the benchmark shows pressure failures.

### SWM-07 Headless OMP child process (same binary) behind a trigger list
**Decision class:** Requires benchmarking
**Existing implementation evidence:** `crates/envd/src/child_tree.rs:1` (process-group teardown already shared by delegated adapters), `crates/envd/src/worker_pool.rs:1` (named-worker supervision), `docs/adr/0039-remote-control-and-factory-modes.md:346` (B1, workers run `omp serve --worker`; Proposed). No `--worker` or `Fleet` code found. Proposed.
**Actual missing capability:** OS-level memory limit and crash containment per child; independent lifecycle beyond the root. Both are only true if measured needs exist.
**Alternatives considered:** In-process child kernel plus sandbox for commands; external harness for foreign agents.
**Expected benefit:** reliability and memory containment, at a start-up and duplicated-service cost.
**Implementation complexity:** high: auth for a child to reach the root's gateway, version skew, journal upload.
**Security implications:** a child process needs a scoped token, not provider credentials (ADR 0039 B6 states no provider credentials reach workers).
**Maintenance cost:** high.
**Performance measurement strategy:** B5 in section 8. not measured.
**Acceptance criteria:** a measured case where an in-process child cannot meet a stated memory or crash-containment requirement.
**Follow-up:** propose only if B5 shows such a case.

### SWM-08 Measure what a child duplicates (provider registry, credentials, memory, indexes)
**Decision class:** Requires benchmarking
**Existing implementation evidence:** `crates/driver/src/headless/kernel.rs:2457 compose_kernel`, `crates/driver/src/headless/kernel.rs:266 ProductionInference`, `crates/driver/src/headless/kernel.rs:2506 admit_native_extensions_contained_with_control`. Composition per child is real; the duplicated fraction was not read.
**Actual missing capability:** A measured per-child composition cost (time, resident memory, files opened) and a statement of which services are shared handles.
**Alternatives considered:** Assume sharing (unverified), or build a service container now (premature).
**Expected benefit:** memory and latency, if duplication exists.
**Implementation complexity:** low for the measurement, unknown for any fix.
**Security implications:** credential keepers must stay single-owner; duplication of refresh logic would be a defect.
**Maintenance cost:** low.
**Performance measurement strategy:** B1 in section 8. not measured.
**Acceptance criteria:** table in section 4 has no `not examined` cell for provider registry, credentials and memory database.
**Follow-up:** extend by passing shared handles through `KernelOptions` if duplication is shown.

### SWM-09 Carry the workspace base digest on the job record and result
**Decision class:** Partially implemented: extend
**Existing implementation evidence:** `crates/envd/src/workspace/operations.rs:253 WorktreeRecord` (base snapshot id, generation), `crates/envd/src/workspace/operations.rs:275 DurableSnapshotRecord` (manifest and tree hashes), `docs/adr/0007-subagent-filesystem-isolation.md:80` (each view records its base manifest digest), `crates/agent/src/jobs.rs:421 JobRecord` (no base digest field). Partial.
**Actual missing capability:** The job record that the supervisor and ADW read does not carry the base workspace generation, so a late patch cannot be checked against the generation by the job layer alone.
**Alternatives considered:** Leave it inside envd (envd already checks at merge time).
**Expected benefit:** reliability (stale-base detection earlier), evidence.
**Implementation complexity:** low to medium.
**Security implications:** none.
**Maintenance cost:** low.
**Performance measurement strategy:** not applicable
**Acceptance criteria:** a settled child result names its base digest and the merge result names the parent digest it merged into.

### SWM-10 Git worktree as an isolation backend
**Decision class:** Unnecessary or redundant
**Existing implementation evidence:** `docs/adr/0007-subagent-filesystem-isolation.md:53` ("Prohibited: worktree-only isolation presented as full isolation"), `crates/envd/src/workspace/operations.rs:673 create_worktree`. CoW per-file clone is implemented; the git worktree backend is listed as a future target.
**Actual missing capability:** None shown. Git worktrees skip untracked non-ignored files and need a repository.
**Alternatives considered:** Git worktree backend; overlayfs or snapshots (ADR 0007 future targets).
**Expected benefit:** none over reflink on APFS or Linux reflink filesystems; possibly space on filesystems without CoW (not measured).
**Implementation complexity:** medium.
**Security implications:** shares the `.git` object store with the parent.
**Maintenance cost:** medium.
**Performance measurement strategy:** B6 in section 8, for the copy-fallback case only. not measured.
**Acceptance criteria:** none unless B6 shows the copy fallback is unacceptable.

### SWM-11 External agent harnesses as delegated executors
**Decision class:** Suitable as an external integration
**Existing implementation evidence:** `crates/driver/src/delegated_host.rs:19 ExecutorRegistry`, `crates/contract/src/delegated/executor.rs:217 Executor`, `docs/adr/STATUS.md:70` (ADR 0052 row: native executor routes through `run_child`).
**Actual missing capability:** None for the contract; breadth of adapters is an extension matter.
**Alternatives considered:** Re-implementing a foreign harness loop inside OMP2.
**Expected benefit:** reliability and coverage; acceptance stays evidence-based (ADR 0053 per STATUS row).
**Implementation complexity:** low per adapter.
**Security implications:** foreign harnesses may bypass approvals (cotion passes a flag that disables approvals and sandbox, `Ve-ria/cotion@b7a4307:src/run.ts`); OMP2 must keep its own sandbox and verdict.
**Maintenance cost:** per adapter.
**Performance measurement strategy:** not applicable
**Acceptance criteria:** a delegated run shares the same cancel and teardown path as other envd children.

### SWM-12 Do not add a swarm crate or a second scheduler
**Decision class:** Unnecessary or redundant
**Existing implementation evidence:** `crates/driver/src/subagent/workpool_scheduler.rs:101 WorkpoolPolicy`, `crates/agent/src/jobs.rs:50 JobKind`, `crates/adw/src/run.rs:173 Run`. The three together hold queueing, supervision index and sequencing.
**Actual missing capability:** None shown that these cannot do.
**Alternatives considered:** New crate for swarm state; grape-style SQL dispatch.
**Expected benefit:** avoided duplication (owner constraint 8).
**Implementation complexity:** not applicable.
**Security implications:** a second state store would split durable truth away from the journal.
**Maintenance cost:** avoided.
**Performance measurement strategy:** not applicable
**Acceptance criteria:** any later proposal for a new layer states which of the three it cannot extend, with evidence.

### SWM-13 Reclaim orphaned worktrees by owner liveness
**Decision class:** Partially implemented: extend
**Existing implementation evidence:** `crates/envd/src/workspace/operations.rs:263 DurableWorktreeRecord` (stores `owner_pid`), `crates/envd/src/workspace/operations.rs:295 ISOLATION_OWNER_FILE`, `crates/envd/src/recovery.rs:36 sweep_stale_runtime_dirs`. Durable records are loaded at open.
**Actual missing capability:** A search of `crates/envd/src/workspace/*.rs`, `crates/envd/src/recovery.rs` and `crates/envd/src/process_identity.rs` found no check that `owner_pid` is dead before reclaiming a worktree. Not examined elsewhere.
**Alternatives considered:** Leave cleanup to the user.
**Expected benefit:** disk space after a crash.
**Implementation complexity:** low.
**Security implications:** a pid check must use process identity (`crates/envd/src/process_identity.rs`) to avoid pid reuse.
**Maintenance cost:** low.
**Performance measurement strategy:** not applicable
**Acceptance criteria:** after killing a root mid-run, the next envd start reclaims or reports the orphan view.

### SWM-14 Adopt agent-control or dispatch code from the reference harnesses
**Decision class:** Rejected for security, licensing or operational reasons
**Existing implementation evidence:** reference only: `limecloud/lime@015b878:lime-rs/crates/agent-graph-store/src/types.rs` (GPL-3.0), `grape-dev/grape@b31f8d8:src/domains/mgmt/org_dispatch.py` (MIT, Python, SQL-backed).
**Actual missing capability:** None; ideas (spawn edge status, conditional-update transitions) are already expressible in the journal.
**Alternatives considered:** Re-deriving the ideas independently on OMP2 types.
**Expected benefit:** none.
**Implementation complexity:** not applicable.
**Security implications:** GPL-3.0 code cannot be copied into this tree; grape's model is a hosted multi-tenant service.
**Maintenance cost:** avoided.
**Performance measurement strategy:** not applicable
**Acceptance criteria:** no code from these repositories appears in the OMP2 tree.

## 8. Benchmark plan (all `not measured`)

No measurement was run for this deliverable. Each item needs a command, environment (`uname -m`, memory, cores, `uptime` load) and raw output saved under `evidence/measurements/` before any number is quoted.

| ID | Question | Method | Metric | Status |
|---|---|---|---|---|
| B1 | What does composing one child kernel cost, and what is duplicated? | Spawn N=1, 4, 16 trivial `task` children in one root; sample RSS of the root and open file count; trace time inside `compose_kernel` | ms per child, RSS delta per child, files opened | not measured |
| B2 | Where does the 32-child default break? | Spawn 8, 16, 32, 64 children that each read and write files on a repo of known size; watch RSS and tail latency | peak RSS, p95 spawn-to-first-turn | not measured |
| B3 | Do two independent ADW phases overlap? | Run a two-phase workflow with independent phases and record wall time versus serial | wall time ratio | not measured |
| B4 | Does a count-only limit fail before a memory-aware one would? | Replay B2 under artificial memory pressure | OOM or swap events | not measured |
| B5 | Does an in-process child fail a containment requirement a process would meet? | Induce a panic and a leak in one child; observe root | root survival, memory reclaimed | not measured |
| B6 | Cost of CoW view creation by file count, on reflink and on copy fallback | Create and merge views on repos of 1k, 10k, 100k files | ms, bytes, ISOLATION_BASELINE budget hits | not measured |

## 9. Draft ADR needed?

No new ADR. The role split is in ADR 0039 D3, isolation in ADR 0007, one executor contract in ADR 0052. Two edits are suggested to existing records, as proposals needing owner approval, not as new decisions: (a) correct the ADR 0051 row in `docs/adr/STATUS.md:69` once the production journaling default is verified; (b) if B2 or B4 shows pressure failures, an amendment to the workpool section of ADR 0039 adding resource inputs to `WorkpoolPolicy`. A new ADR becomes justified only if SWM-07 passes B5.

## 10. Open items

- Read `compose_kernel` past `crates/driver/src/headless/kernel.rs:2510` to fill the provider, credential and memory cells.
- Read `crates/driver/src/subagent/revive.rs`, `crates/driver/src/subagent/hub.rs` and `crates/tools/src/hub.rs`.
- Read `crates/vcs` and `crates/sandbox` and ADR 0045, 0053, 0056 bodies.
- Verify that production `omp adw run` journals transitions.

Back to the [index](README.md).
