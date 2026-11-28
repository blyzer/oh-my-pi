# Hybrid swarm with lightweight OMP subprocesses

> Part of the [OMP2 gateway-capabilities research](../README.md). Diagram for deliverables 9 and 12. Audited commit `85d219ae7f` (`origin/omp2`). Research proposal: nothing here is implemented.

Status: PROPOSED architecture view. Only the logical-agent path and the envd child-process teardown are EXISTING. The headless OMP child, the scoped gateway token and the journal upload are PROPOSED (ADR 0039 B1 to B6, no code found), and are gated by card SWM-07 and benchmark B5. All dotted edges are proposed.

```mermaid
flowchart TB
  root["Root session (EXISTING)"]
  trig{"Trigger list met? crash containment, memory cap, independent lifecycle"}
  logical["Logical agent: child kernel (EXISTING spawn.rs:923)"]
  headless["PROPOSED: headless OMP child, same binary, separate process"]
  harness["External harness via ExecutorRegistry (EXISTING delegated_host.rs:19)"]
  sandbox["Sandboxed command (EXISTING crates/sandbox, envd exec_sandbox.rs)"]
  tree["envd child tree: TERM, grace, KILL, reap (EXISTING child_tree.rs:1)"]
  gw["Root gateway with scoped token, no provider credentials in child (PROPOSED, ADR 0039 B6)"]
  cas["Blob store: patch and journal by digest (EXISTING artifact://sha256)"]
  lease["FactoryLease fencing (EXISTING crates/agent/src/jobs.rs:473)"]
  root --> trig
  trig -->|"no, default"| logical
  trig -.->|"yes, recorded reason"| headless
  root --> harness
  root --> sandbox
  headless -.-> tree
  harness --> tree
  headless -.-> gw
  headless -.-> cas
  logical --> cas
  lease -.-> headless
```

```
                          +--> logical agent (child kernel, EXISTING, default)
 root --> trigger list ---+
                          +- - > headless OMP child (PROPOSED, SWM-07, needs B5)
                                   - - scoped gateway token to root (PROPOSED)
                                   - - FactoryLease fencing (EXISTING primitive, no transport)
                                   - - patch and journal to blob store by digest
 root --> external harness (EXISTING ExecutorRegistry) --> envd child tree teardown (EXISTING)
 root --> sandboxed command (EXISTING sandbox)
 Solid = existing. Dashes = proposed.
```

Back to the [index](../README.md).
