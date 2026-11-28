# OMP2 swarm, workpool, ADW and supervision audit

> Part of the [OMP2 gateway-capabilities research](../README.md). Evidence file for deliverable 9. Audited commit `85d219ae7f` (`origin/omp2`). Research proposal: nothing here is implemented.

Status: first pass, written under a request budget. The TSV twin is `omp2-swarm-adw.tsv` (rows W01 to W16). Worktree read: `/private/tmp/omp-gateway-research` (same tree as `85d219ae7f`).

## 1. Method

Read the named files and followed symbols with grep. A claim about behaviour cites the code that enforces it. Anything not opened is listed in section 6.

## 2. Spawn, composition and limits

- `crates/driver/src/subagent/spawn.rs:89 DriverSubagentSpawner` holds the parent session, shared `SessionRegistry`, `JobBoard`, one `EnvClient`, a `CfgLoader`, lifecycle hooks and the parent's workspace trust. A spawn admits the batch, admits each child, then creates a per-child `CancellationToken` (`crates/driver/src/subagent/spawn.rs:139`).
- `run_child` (`crates/driver/src/subagent/spawn.rs:923 run_child`) creates isolation unconditionally, composes a kernel on the isolated root through `compose_kernel` (`crates/driver/src/subagent/spawn.rs:933`), computes a deadline from `max_runtime_ms` (`crates/driver/src/subagent/spawn.rs:945`), runs one turn with `RunControl::new(token, deadline)` plus a soft request budget, then parks the idle child after a TTL (`crates/driver/src/subagent/spawn.rs:965`). On error the view is destroyed; on a cancelled run or a schema error it is discarded; otherwise `finish_isolation` merges.
- Schema-bound children get yield reminders: two soft, then one natively forced choice (`crates/driver/src/subagent/spawn.rs:60 MAX_YIELD_RETRIES`).
- Child class and recursion depth are journaled on `<meta>` (`crates/driver/src/subagent/mod.rs:29`).
- Convars: concurrency 32 (`crates/driver/src/subagent/settings.rs:123 SV_TASK_MAX_CONCURRENCY`), max recursion 2 (`crates/driver/src/subagent/settings.rs:106 SV_TASK_MAX_RECURSION_DEPTH`), runtime `Never` (`crates/driver/src/subagent/settings.rs:144 SV_TASK_MAX_RUNTIME`), soft request budget 200 (`crates/driver/src/subagent/settings.rs:166 SV_TASK_SOFT_REQUEST_BUDGET`), LSP in children false (`crates/driver/src/subagent/settings.rs:237 SV_TASK_ENABLE_LSP`), idle park 420 s (`crates/driver/src/subagent/settings.rs:248 SV_TASK_AGENT_IDLE_TTL`), factory lease TTL (`crates/driver/src/subagent/settings.rs:356 SV_FACTORY_LEASE_TTL`).

## 3. Isolation and workspace revision identity

- ADR 0007 decision: every writing child gets its own view of tracked and untracked non-ignored files; per-file clone (APFS `clonefile`, Linux reflink) with copy fallback; the child returns changes, the parent integrates (`docs/adr/0007-subagent-filesystem-isolation.md:26`). The amendment of 2026-10-04 removed the unimplemented backends; "worktree-only isolation presented as full isolation" is prohibited (`docs/adr/0007-subagent-filesystem-isolation.md:53`).
- `create_worktree` (`crates/envd/src/workspace/operations.rs:673 create_worktree`) snapshots the live workspace first (`snapshot_at`), refuses a non-live base generation, creates a ULID-named root, writes `.omp-isolation-owner` with the owner pid, clones every manifest entry and records a `WorktreeRecord` with `base`, `generation` and `owner_pid`. It honours a cancellation token. An untracked tree over `ISOLATION_BASELINE_MAX_UNTRACKED_BYTES` (1 GiB) is refused (`crates/envd/src/workspace/operations.rs:57`).
- `merge_worktree` (`crates/envd/src/workspace/operations.rs:789 merge_worktree`) takes a global transition mutex, snapshots the view and the parent, writes a manifest diff artifact, runs a three-way `merge_target` and returns conflicts. Branch mode records `omp/agent/<id>` metadata internally without invoking a VCS subprocess.
- `destroy_worktree` without `force` refuses a view with unmerged changes (`crates/envd/src/workspace/operations.rs:749 destroy_worktree`); the spawner always passes `force: true` (`crates/driver/src/subagent/spawn.rs:1097`).
- Records are durable (`DurableWorktreeRecord`, `DurableSnapshotRecord`) and reloaded by `WorkspaceOperations::open` (`crates/envd/src/workspace/operations.rs:472`).
- Revision identity: a `u64` generation counter, snapshot ids, a manifest hash and a tree hash per snapshot (`crates/envd/src/workspace/operations.rs:275 DurableSnapshotRecord`). The job record has no field for it (`crates/agent/src/jobs.rs:421 JobRecord`).
- Searched for an owner-liveness reaper (`owner_pid`, orphan, stale) in `crates/envd/src/workspace/*.rs`, `crates/envd/src/recovery.rs`, `crates/envd/src/process_identity.rs`: only the recording was found.

## 4. Jobs, leases and the workpool

- `JobKind` has four shapes: tool, subagent, process, execution (`crates/agent/src/jobs.rs:50 JobKind`). The board is an index rebuilt from the DOM (`crates/agent/src/jobs.rs:1`).
- `FactoryLease { worker_id, lease_gen, deadline_ms }` is one custom prop so a partial write cannot land (`crates/agent/src/jobs.rs:473 FactoryLease`); `accept_lease`, `renew_lease` and `release_lease` fence on `lease_gen`. The spawner calls `release_lease` (`crates/driver/src/subagent/spawn.rs:681`). Searched `crates/**/*.rs` and `*.proto` for `Fleet`, `--worker`, `LeaseGrant`, `JobKind::Work`: no match, so the fleet transport of ADR 0039 Part B is not in code.
- `WorkpoolPolicy` has three methods: `concurrency_limit`, `fresh_agents`, `eval_tools_enabled` (`crates/driver/src/subagent/workpool_scheduler.rs:101 WorkpoolPolicy`). `ConWorkpoolPolicy` reads the convars at each dispatch (`crates/driver/src/subagent/workpool_scheduler.rs:111 ConWorkpoolPolicy`). Aggregate settlement is a `JobBoard` transaction; scheduler state is a disposable index.

## 5. Supervision, ADW and executors

- `crates/agent/src/cancel.rs:1`: two tokens per tool scope (commit and interrupt), cooperative settlement, grace, forced termination journaled as uncertainty (ADR 0011).
- `crates/envd/src/child_tree.rs:1`: own process group, TERM, grace, KILL, reap, shared by MCP stdio and delegated agent adapters.
- `crates/envd/src/worker_pool.rs:1`: named-worker supervision with generation-fenced data transport (read only the module doc).
- ADW: `crates/adw/src/run.rs:173 Run` (pure state machine), `crates/adw/src/replay.rs:1` (rebuild from recorded facts, stop at first divergence), `crates/driver/src/adw/mod.rs:220` (resume from journal, `Error::Replay` on disagreement). `docs/adr/STATUS.md:69` still reads "Run is in memory only", so that row is stale for replay.
- Executors: `crates/driver/src/delegated_host.rs:19 ExecutorRegistry`; `crates/driver/src/adw/production.rs:296` builds a registry from the console context; `crates/contract/src/delegated/executor.rs:217 Executor`. `docs/adr/STATUS.md:70` (ADR 0052) says native OMP2 routes through `run_child`.
- ADR 0039 D3 role split: `docs/adr/0039-remote-control-and-factory-modes.md:618`; invariants D6 at `docs/adr/0039-remote-control-and-factory-modes.md:624`.

## 6. Not examined

Body of `compose_kernel` beyond `crates/driver/src/headless/kernel.rs:2510`; `crates/driver/src/subagent/revive.rs`; `crates/driver/src/subagent/hub.rs`; `crates/tools/src/hub.rs`; `crates/vcs`; `crates/sandbox`; ADR 0045, 0053 and 0056 bodies (STATUS rows only: `docs/adr/STATUS.md:63`, `docs/adr/STATUS.md:71`, `docs/adr/STATUS.md:74`); memory database, embedding model and index sharing.

Back to the [index](../README.md).
