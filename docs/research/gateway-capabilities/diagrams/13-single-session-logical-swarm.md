# Single-session logical swarm

> Part of the [OMP2 gateway-capabilities research](../README.md). Diagram for deliverables 9 and 12. Audited commit `85d219ae7f` (`origin/omp2`). Research proposal: nothing here is implemented.

Status: PROPOSED architecture view. Elements marked EXISTING are verified in code and cited; no element here is new code. The proposal is to keep this shape as the default and add nothing to it.

```mermaid
flowchart TB
  root["Root session and kernel"]
  task["task tool: batch spawn (EXISTING crates/tools/src/task.rs)"]
  spawner["DriverSubagentSpawner (EXISTING crates/driver/src/subagent/spawn.rs:89)"]
  pool["Workpool scheduler, count-only policy (EXISTING crates/driver/src/subagent/workpool_scheduler.rs:101)"]
  board["JobBoard over meta jobs tree (EXISTING crates/agent/src/jobs.rs:699)"]
  adw["ADW run, pure state machine (EXISTING crates/adw/src/run.rs:173)"]
  c1["Child kernel A, own journal"]
  c2["Child kernel B, own journal"]
  env["envd: workspace, exec, blobs (EXISTING one EnvClient)"]
  v1["CoW view A (EXISTING operations.rs:673)"]
  v2["CoW view B"]
  merge["Three-way merge, patch artifact (EXISTING operations.rs:789)"]
  cancel["Cancel tree (EXISTING crates/agent/src/cancel.rs:1)"]
  root --> task --> spawner
  adw -->|"dispatch decisions"| spawner
  spawner --> pool --> c1
  pool --> c2
  spawner --> board
  c1 --> env
  c2 --> env
  env --> v1
  env --> v2
  v1 --> merge
  v2 --> merge
  merge -->|"patch or conflicts"| root
  cancel -.-> c1
  cancel -.-> c2
  shared["PROPOSED: pass shared handles for provider registry, credentials, memory (SWM-08, unverified duplication)"]
  shared -.-> c1
  shared -.-> c2
```

```
 root session --task--> DriverSubagentSpawner --> workpool (count limit) --+--> child kernel A --> CoW view A --+
   ^   ADW run (EXISTING) -----dispatch------^         |                  +--> child kernel B --> CoW view B --+
   |                                                    v                                                       v
   +<----------- patch artifact or conflicts <---- three-way merge (envd) <-----------------------------------+
 JobBoard (index of the meta jobs tree) tracks every child; cancel tree reaches every child.
 PROPOSED only: shared handles for provider registry, credentials, memory db (SWM-08).
 Everything else on this page is EXISTING; nothing new is added.
```

Back to the [index](../README.md).
