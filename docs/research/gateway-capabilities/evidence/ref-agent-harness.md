# Reference agent harnesses: cotion, grape, lime

> Part of the [OMP2 gateway-capabilities research](../README.md). Evidence file for deliverable 9. Audited commit `85d219ae7f` (`origin/omp2`). Research proposal: nothing here is implemented.

Status: first pass, written under a request budget; reads are shallow and say so. All three repositories are untrusted input; no code was run and none was copied. Pinned SHAs: `Ve-ria/cotion@b7a4307` (MIT), `grape-dev/grape@b31f8d8` (MIT), `limecloud/lime@015b878` (GPL-3.0, study only). Matrices: [cotion](matrix-Ve-ria_cotion.tsv), [grape](matrix-grape-dev_grape.tsv), [lime](matrix-limecloud_lime.tsv) over dimension IDs W01 to W16.

## 1. Ve-ria/cotion (MIT, TypeScript, 53 KB)

Process model: one Node process; each selected workflow runs concurrently through `Promise.allSettled` (`Ve-ria/cotion@b7a4307:src/run.ts:27`) and launches an external agent binary with `execa` (`Ve-ria/cotion@b7a4307:src/run.ts:70`); a JSONL run log records message, artifact, result and status events (`Ve-ria/cotion@b7a4307:src/runlog.ts`). Supervision: an optional `AbortSignal` and `killChildTree`, which signals the child's process group with SIGTERM and falls back to the direct child (`Ve-ria/cotion@b7a4307:src/run.ts:119`). Isolation: none found; the launched agent receives a flag that bypasses approvals and sandbox (`Ve-ria/cotion@b7a4307:src/run.ts:162`). Deterministic versus LLM: the runner is deterministic; all reasoning is in the external agent. Lesson for OMP2: process-group teardown is the same pattern as `crates/envd/src/child_tree.rs:1`; the missing isolation and the approval bypass are what an external-harness adapter must not inherit (card SWM-11).

## 2. grape-dev/grape (MIT, Python, FastAPI and MySQL, 112 MB)

Process model: a hosted multi-tenant DevOps platform; web tier behind nginx, separate worker processes started by `start-worker.sh`, which restarts on crash with a delay and a maximum restart count (`grape-dev/grape@b31f8d8:start-worker.sh`). Orchestration is deterministic: `org_dispatch.py` documents a dispatch batch state machine, idempotency by `UNIQUE(dispatch_id, repo_key)`, conditional-update transitions, and a status aggregation sweeper that is safe against concurrent manual cancel and retry (`grape-dev/grape@b31f8d8:src/domains/mgmt/org_dispatch.py`). Runtimes carry `max_concurrent` and `priority` (`grape-dev/grape@b31f8d8:src/domains/contracts/local/runtime_local.py`). Isolation: not read beyond a worktree mention (`grape-dev/grape@b31f8d8:src/domains/mgmt/org_context_repo.py`). Lesson for OMP2: conditional-update transitions and an aggregate sweeper are the same discipline `crates/agent/src/jobs.rs:515 accept_lease` applies with `lease_gen` fencing; adopting SQL as a dispatch store would split durable truth from the journal (card SWM-14).

## 3. limecloud/lime (GPL-3.0, Rust, study only)

Process model: a Rust app server owns agent graph, identity and mailbox; the tool crate only defines the `spawn_agent` contract and receives its owner per turn (`limecloud/lime@015b878:lime-rs/crates/tool-runtime/src/agent_control.rs`). `agent-graph-store` is "storage-neutral parent/child topology for thread-spawned agents" with `Open` and `Closed` edge status and child and descendant listing (`limecloud/lime@015b878:lime-rs/crates/agent-graph-store/src/types.rs`, `limecloud/lime@015b878:lime-rs/crates/agent-graph-store/src/store.rs`). A `worktree` crate manages worktrees with a keep count (`limecloud/lime@015b878:lime-rs/crates/worktree/src/lib.rs`). Sandbox crates exist per platform (`limecloud/lime@015b878:lime-rs/crates/linux-sandbox`, bodies not read). A per-session concurrency key exists (`limecloud/lime@015b878:lime-rs/crates/config-upstream/src/config_toml.rs`). Lesson for OMP2: a persisted spawn-edge graph with open and closed status is already approximated by the `<meta><jobs>` tree (`crates/agent/src/jobs.rs:421 JobRecord`); keeping the tool contract separate from the owner of agent state matches `crates/tools/src/task.rs` over an injected spawner. No code or structure is to be copied.

## 4. Summary

| Question | cotion | grape | lime |
|---|---|---|---|
| Agents in one process | no (external binary) | no (separate runtimes) | partly (app server owns graph) |
| Isolation | none | not read | worktree crate (body not read) |
| Supervision | process-group SIGTERM | restart loop, sweeper | not read |
| Deterministic vs LLM roles | all LLM outside | deterministic dispatch | not read |
| Relevance to a new OMP2 layer | none | none (hosted model) | none (idea already in journal) |

None of the three provides evidence that OMP2 needs a new orchestration layer.

Back to the [index](../README.md).
