# Proposed architecture diagrams

> Part of the [OMP2 gateway-capabilities research](README.md). Deliverable 12. Audited commit `85d219ae7f`. Research proposal: nothing here is implemented.

The owner's section 14 lists 19 diagrams. Each is a file under `diagrams/` with a title, a status line, one Mermaid block, one ASCII block, an evidence list and a legend. **Status distinguishes what is verified from what is proposed**: `CURRENT` diagrams cite code read at `85d219ae7f`; `REFERENCE REPOSITORY` diagrams describe a reference repository at its pinned SHA (not OMP2); `PROPOSED` diagrams show proposals, and any element that exists today is labelled with its citation. Mermaid syntax was written conservatively and not rendered in this environment: no Mermaid renderer was run, so a rendering error is possible; the ASCII block is the fallback.

## Index

| # | Diagram | Status | Used by deliverable |
|---|---|---|---|
| 01 | [current verified OMP2 architecture](diagrams/01-current-omp2-architecture.md) | CURRENT, verified at 85d219ae7f | 2 |
| 02 | [CLIProxyAPI architecture](diagrams/02-cliproxyapi-architecture.md) | CURRENT, verified at `router-for-me/CLIProxyAPI@dbe641f` (reference re | 3, 5 |
| 03 | [OmniRoute architecture](diagrams/03-omniroute-architecture.md) | CURRENT, verified at `cheaper-inference/OmniRoute@3e23a8a` (reference  | 3, 5 |
| 04 | [LLMHub architecture](diagrams/04-llmhub-architecture.md) | REFERENCE REPOSITORY, read at `8bdc1de` (`nicolaeser/llmhub`, MIT, com | 3 |
| 05 | [ProxyCast architecture](diagrams/05-proxycast-architecture.md) | REFERENCE REPOSITORY, read at `3ad8ff6` (`hank9999/proxycast`, no lice | 3 |
| 06 | [Current provider execution path](diagrams/06-current-provider-execution-path.md) | CURRENT, verified at 85d219ae7f | 5 |
| 07 | [Proposed provider runtime](diagrams/07-proposed-provider-runtime.md) | PROPOSED, not implemented | 5 |
| 08 | [Authentication and credential lifecycle (current sequence and proposed delta)](diagrams/08-authentication-and-credential-lifecycle.md) | CURRENT, verified at 85d219ae7f | 5b |
| 09 | [Protocol translation](diagrams/09-protocol-translation.md) | CURRENT, verified at 85d219ae7f | 6 |
| 10 | [Streaming execution](diagrams/10-streaming-execution.md) | CURRENT, verified at 85d219ae7f | 6 |
| 11 | [ABI-independent extension boundaries](diagrams/11-abi-independent-extension-boundaries.md) | CURRENT, verified at 85d219ae7f | 7 |
| 12 | [embedded versus standalone Python workers](diagrams/12-embedded-vs-standalone-python-workers.md) | CURRENT** (verified at the audited commit, see [Python runtime audit]( | 8 |
| 13 | [Single-session logical swarm](diagrams/13-single-session-logical-swarm.md) | PROPOSED architecture view | 9 |
| 14 | [Hybrid swarm with lightweight OMP subprocesses](diagrams/14-hybrid-swarm-with-lightweight-omp-subprocesses.md) | PROPOSED architecture view | 9 |
| 15 | [shared inference workers](diagrams/15-shared-inference-workers.md) | PROPOSED | 10 |
| 16 | [Shared code intelligence layer](diagrams/16-shared-code-intelligence-layer.md) | PROPOSED architecture view | 11 |
| 17 | [SCIP integration](diagrams/17-scip-integration.md) | PROPOSED architecture view for the export and sharing parts | 11 |
| 18 | [Resource-aware scheduling](diagrams/18-resource-aware-scheduling.md) | PROPOSED architecture view | 9 |
| 19 | [complete proposed architecture](diagrams/19-complete-proposed-architecture.md) | PROPOSED, not implemented | 1 |

## The complete proposed architecture

[Diagram 19](diagrams/19-complete-proposed-architecture.md) is the single picture. In short: one root session with logical child kernels (verified); the provider runtime keeps its fixed stack and gains producers for route evidence, header-driven rate and quota observation and a breaker state (PRV-01 to PRV-03); credentials gain a per-process refresh owner and a shared stack for children (AUTH-01, AUTH-03); extensions keep the message boundary and gain enforcement of declared manifest codes (EXT-03 to EXT-05); local inference gets one memory budget per process and optional external servers (INF-02, INF-03, INF-05); code intelligence gets one index keyed by workspace identity and revision and one typed result (CIX-03, CIX-11). No new scheduler, credential manager, index service or control plane is drawn.

## Not rendered

The Mermaid blocks were checked by `evidence/check_docs.py` only for balanced fences and presence, not for syntax. Open each diagram in a Mermaid-capable viewer before relying on its layout.

Back to the [index](README.md).
