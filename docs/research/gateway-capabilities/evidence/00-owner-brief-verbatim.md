# Owner brief (verbatim)

Source: the owner's request that started this research (session `01a20def-a570`, 2026-11-28). Reproduced unchanged below the rule so every document in this set can be checked against the original wording. Section numbers 1-18 are the owner's; deliverables 1-20 are listed in section 16 and the decision classes in section 17.

---

# **OMP2 Architecture Research — Gateway Capabilities, Native Provider Runtime and Single-Session Swarm Integration**

## **1. Objective**

Conduct a comprehensive, evidence-driven architectural audit of OMP2 and the referenced AI gateways.

Determine which gateway capabilities would provide measurable architectural, operational, security, performance or resource-efficiency benefits if implemented directly within OMP2.

The objective is not to transform OMP2 into another monolithic AI gateway or reproduce CLIProxyAPI, OmniRoute or LLMHub.

The objective is to selectively incorporate useful architectural principles into OMP2’s existing native Rust agent harness.

This is a research, comparative analysis and architectural proposal task.

Do not implement architectural changes yet.

## **2. Mandatory project constraints**

You are researching OMP2 using OMPv1 as the coding-agent harness.

Follow these rules:

1. Inspect OMP2’s existing implementation before proposing new abstractions.
2. Reuse existing native tools, typed IRs, execution backends, lifecycle hooks, configuration and verification infrastructure.
3. Distinguish implemented capabilities from prototypes, proposals and unmerged branches.
4. Prefer deterministic mechanisms for execution, parsing, scheduling, validation, state transitions and evidence collection.
5. Use LLM reasoning only when semantic interpretation is genuinely necessary.
6. Prefer incremental vertical slices.
7. Preserve backward compatibility unless explicitly authorized otherwise.
8. Do not create redundant provider runtimes, schedulers, memory systems, configuration stores or indexing services.
9. Follow the existing ADR conventions.
10. Cite source paths, symbols, commits, tests and measurable evidence.

Preserve existing ADRs, particularly ADR 0036.

Treat changes to existing architectural decisions as explicit proposals requiring evidence and approval.

## **3. Reference repositories**

### **Inference gateways**

- https://github.com/router-for-me/CLIProxyAPI
- https://github.com/Ve-ria/CLIProxyAPIPlus
- https://github.com/cheaper-inference/OmniRoute
- https://github.com/decolua/9router
- https://github.com/fallencolors/freellmapi
- https://github.com/nicolaeser/llmhub
- https://github.com/QuantumNous/new-api
- https://github.com/hank9999/proxycast

### **Provider implementations**

- https://github.com/hank9999/kiro.rs
- https://github.com/BenedictKing/kiro.rs
- https://github.com/M-JYuan/kiro.rs
- https://github.com/Foxfishc/kiro.rs
- https://github.com/lzt404/kiro.rs
- https://github.com/cxy2730/CLIProxyAPI-freetier2api-plugin
- https://github.com/stark-shadow/cpa-plugin-perplexity

### **Configuration and extension management**

- https://github.com/lzt404/EasyCLIProxyAPI
- https://github.com/stark-shadow/ecpa-plugins-store
- https://github.com/Ve-ria/quickx
- https://github.com/AmethystDev-Labs/QuickCLI

### **Agent harness and orchestration**

- https://github.com/Ve-ria/cotion
- https://github.com/grape-dev/grape
- https://github.com/limecloud/lime

### **ABI-independent extension mechanisms**

- https://github.com/hashicorp/go-plugin
- https://github.com/bytecodealliance/wasmtime
- https://github.com/bytecodealliance/wit-bindgen
- https://github.com/tokio-rs/prost
- https://github.com/hyperium/tonic
- https://github.com/capnproto/capnproto-rust
- https://github.com/google/flatbuffers
- https://github.com/djkoloski/rkyv

### **Local inference and code intelligence**

- https://github.com/microsoft/onnxruntime
- https://github.com/ml-explore/mlx
- https://github.com/ml-explore/mlx-lm
- https://github.com/ggml-org/llama.cpp
- https://github.com/huggingface/candle
- https://github.com/apple/coremltools
- https://github.com/scip-code/scip
- https://github.com/facebookincubator/Glean
- https://github.com/quickwit-oss/tantivy

Verify repository availability and authoritative documentation before relying on them.

## **4. Existing OMP2 architecture audit**

Inspect:

- Native Rust core.
- Agent runtime.
- Session lifecycle.
- Provider abstractions.
- Authentication.
- Model discovery.
- Extension infrastructure.
- Configuration.
- Memory.
- Inference execution.
- Native tools.
- Swarm implementation.
- ADW.
- Environment supervision.
- IPC.
- Protobuf contracts.
- Code intelligence.
- Verification infrastructure.
- Existing ADRs.

Investigate the actual implementation of:

- Embedded CPython.
- PyO3.
- Free-threaded Python.
- Extension child processes.
- Environment daemon.
- Python workers.
- Extension hosts.
- Evaluation workers.
- Native Rust components.

Verify whether previously identified architecture decisions remain accurate on the current branch.

## **5. Gateway capability comparison**

Research and compare:

### **Provider registry and discovery**

- Canonical provider identity.
- Canonical model identity.
- Model aliases.
- Provider-specific deployments.
- Dynamic discovery.
- Hosted model catalogs.
- Catalog synchronization.
- Provider capability metadata.
- Authentication requirements.
- Free-tier and anonymous discovery.
- Connection identity and deduplication.

### **Authentication and credential lifecycle**

- API keys.
- OAuth.
- Device authorization.
- Official CLI delegation.
- Credential references.
- OS keychain.
- Refresh coordination.
- Credential expiration.
- Credential recovery.
- Independent credential keepers.
- Authorized subscription entitlements.
- Account and regional restrictions.

### **Scheduling and routing**

- Deterministic provider selection.
- Credential eligibility.
- Availability tracking.
- Rate-limit awareness.
- Session affinity.
- Capability-aware routing.
- Cost-aware routing.
- Latency-aware routing.
- Circuit breakers.
- Backpressure.
- Cancellation.
- Retry policies.
- Failure classification.
- Quota-aware scheduling.

Respect provider restrictions and do not use routing to circumvent usage limits.

### **Protocol translation**

- OpenAI Chat Completions.
- OpenAI Responses.
- Anthropic Messages.
- Gemini.
- Provider-specific protocols.
- Reasoning normalization.
- Tool-call correlation.
- Streaming event normalization.
- Multimodal content.
- Structured output.
- Error normalization.

Compare direct pairwise translators with canonical intermediate representations.

Determine whether existing OMP2 contracts already provide an appropriate foundation.

### **Inference interfaces**

- HTTP.
- SSE.
- WebSocket.
- Persistent inference sessions.
- gRPC.
- Protobuf.
- AWS EventStream.
- Bidirectional streaming.
- WebRTC.
- HTTP/3 and QUIC where supported.
- Asynchronous inference jobs.
- Batch APIs.
- Native execution.
- Local IPC.
- External agent harness protocols.

Avoid introducing transports that lack demonstrated provider use cases.

### **Token and resource efficiency**

- Prompt-cache awareness.
- Session affinity.
- Incremental response chaining.
- Provider-side state.
- Context retransmission.
- Semantic compression.
- Batch inference.
- Connection pooling.
- Streaming backpressure.
- Token accounting.
- Cache accounting.
- Time to first token.
- End-to-end latency.

Distinguish reduced network transmission, reduced inference computation and reduced billable token consumption.

## **6. Native Rust provider architecture without C ABI plugin contracts**

Research an extension architecture that does not require:

- C ABI plugin interfaces.
- `extern "C"` plugin exports.
- Native Rust ABI stability.
- Dynamically loaded provider shared libraries.
- `libloading`.
- `abi_stable`.
- `savefile-abi`.

Investigate:

- Statically compiled Rust provider crates.
- Declarative providers.
- Independently executable native providers.
- Versioned message contracts.
- Protobuf over native IPC.
- Cap’n Proto.
- FlatBuffers.
- MessagePack.
- JSON-RPC.
- Shared-memory data exchange.
- Native worker pools.
- Optional WebAssembly components.

Distinguish WebAssembly’s Canonical ABI from C ABI.

Prefer message-based extension boundaries when independent compilation and distribution are required.

Do not assume that eliminating C ABI plugin interfaces requires eliminating every internal dependency on C libraries or operating-system interfaces.

Investigate whether the existing embedded CPython/PyO3 architecture remains justified when Python extensions already execute in isolated subprocesses.

Compare:

- Embedded CPython through PyO3.
- Standalone CPython subprocesses.
- RustPython where ecosystem compatibility permits.
- Native Rust executable extensions.
- Existing Go executable adapters.
- WebAssembly components.

Evaluate binary size, startup latency, memory footprint, package compatibility, callback overhead, distribution, isolation and maintainability.

Do not propose replacing ADR 0036 without concrete evidence.

## **7. Single-session swarm architecture**

The primary intended execution model is one root OMP session coordinating a swarm of logical agents.

Do not assume every agent is a separate complete OMP session or operating-system process.

Investigate:

- Multiple logical agents within one process.
- Asynchronous agent execution.
- Native worker threads.
- Lightweight OMP child agents.
- Headless OMP workers.
- Recursive self-orchestration.
- External agent harnesses.
- Sandboxed subprocesses.
- Shared runtime services.
- Lazy capability initialization.
- Resource-aware worker pools.

The architecture must support spawning lightweight OMP agents when process isolation or independent lifecycle management is justified.

Support independent agent contexts while sharing suitable infrastructure.

Do not duplicate:

- Provider registries.
- Credential managers.
- Embedding models.
- Tiny language models.
- Code indexes.
- Memory databases.
- Language servers.
- Configuration stores.
- Artifact registries.

Account for workspace revisions and Git worktree isolation.

## **8. Orchestrator, foreman and supervisor**

Investigate these as separate responsibilities, not mandatory independent components or mandatory LLM agents.

### **Orchestrator**

- Objective decomposition.
- Workflow planning.
- Dependency coordination.
- Agent assignment.
- Cross-workstream coordination.

### **Foreman**

- Tactical team coordination.
- Workstream ownership.
- Conflict prevention.
- Deliverable integration.
- Progress and evidence aggregation.

### **Supervisor**

- Deterministic lifecycle management.
- Process and task supervision.
- Cancellation.
- Timeouts.
- Resource enforcement.
- Health monitoring.
- Restart and recovery.

Determine which responsibilities already exist in OMP2, Swarm, ADW and extension infrastructure.

Do not introduce redundant orchestration layers.

## **9. Local inference and hardware-aware execution**

Investigate:

- Shared embedding workers.
- Tiny generative models.
- Local classifiers.
- Rerankers.
- Semantic compression.
- ONNX Runtime CPU.
- ONNX Runtime Core ML.
- MLX.
- Direct Core ML.
- Candle.
- llama.cpp.
- Apple Neural Engine.
- GPU and Metal execution.

Distinguish:

- In-process inference.
- Local subprocess inference.
- Local server-based inference.
- Remote inference.
- Remote agent execution.

Preserve shared model residency across agents.

Do not confuse shared model weights with shared generation state or KV caches.

Evaluate hardware-aware workload allocation:

- CPU for deterministic execution and indexing.
- Neural Engine for supported embedding or classification workloads.
- GPU for suitable local generative workloads.
- Remote LLMs for complex semantic reasoning.

Default execution and deployment must support Linux without Docker or Podman.

Also evaluate macOS Apple Silicon, particularly an M1 Pro with 16 GB unified memory.

## **10. IPC and large structured data**

Inspect existing OMP2 Protobuf contracts before introducing new serialization formats.

Benchmark where justified:

- JSONL.
- Protobuf.
- Cap’n Proto.
- FlatBuffers.
- rkyv within compatible Rust boundaries.
- MessagePack.
- Binary tensor buffers.
- Shared memory.
- Memory-mapped files.

Evaluate:

- Small control messages.
- Streaming token events.
- Embedding batches.
- Large CodeIR payloads.
- ChangeIR.
- VerificationIR.
- Artifact references.
- Concurrent swarm workloads.

Measure serialization costs, copying, memory allocation, latency, throughput and complexity.

Do not adopt zero-copy serialization solely because it is theoretically faster.

## **11. Shared code intelligence and semantic layer**

Preserve and reuse OMP2’s existing Code Semantic Layer and typed IR architecture.

Investigate integration between:

- LSP.
- SCIP.
- Glean.
- Tree-sitter.
- ast-grep.
- tgrep.
- Tantivy.
- Native text search.
- Structural summaries.
- Mnemopi.
- Embeddings.
- Reranking.
- CodeQueryIR.
- CodeIR.
- ChangeIR.
- VerificationIR.

Distinguish lexical, structural, semantic and associative intelligence.

Investigate importing and exporting SCIP.

Reuse existing language-specific SCIP indexers wherever appropriate.

Do not attempt to derive authoritative symbol relationships solely from embeddings, text matching or LLM inference.

Support shared code intelligence across swarm agents while preserving workspace identity, revision consistency, document versions and incremental invalidation.

## **12. Extension management and distribution**

Investigate:

- Capability manifests.
- Versioned extension contracts.
- Plugin registries.
- Artifact integrity.
- Cryptographic signatures.
- Compatibility declarations.
- Platform-specific executable distribution.
- Dependency declarations.
- Permissions.
- Isolation policies.
- Lazy startup.
- Independent termination.
- Health reporting.
- Upgrade and rollback.

Study CLIProxyAPI’s plugin ecosystem without copying its native shared-library ABI architecture.

Determine which capabilities already exist in OMP2’s extension infrastructure.

## **13. Observability and management**

Evaluate:

- Provider health.
- Authentication health.
- Worker health.
- Session metrics.
- Agent metrics.
- Token consumption.
- Cache utilization.
- Cost accounting.
- Queue latency.
- Time to first token.
- Resource utilization.
- Structured runtime events.
- Artifact reporting.
- Deterministic execution status.

Prefer existing logging, tracing and observability infrastructure.

Do not introduce a mandatory web dashboard, database server or distributed control plane unless concrete requirements justify it.

## **14. Required architectural diagrams**

Produce Mermaid and ASCII diagrams for:

- Current verified OMP2 architecture.
- CLIProxyAPI architecture.
- OmniRoute architecture.
- LLMHub architecture.
- ProxyCast architecture.
- Current OMP2 provider execution path.
- Proposed provider runtime.
- Authentication and credential lifecycle.
- Protocol translation.
- Streaming execution.
- ABI-independent extension boundaries.
- Embedded versus standalone Python workers.
- Single-session logical swarm.
- Hybrid swarm with lightweight OMP subprocesses.
- Shared inference workers.
- Shared code intelligence layer.
- SCIP integration.
- Resource-aware scheduling.
- Complete proposed architecture.

Clearly distinguish current verified architecture from proposed architecture.

## **15. Benchmark and evidence requirements**

For every significant recommendation establish:

- Existing implementation evidence.
- Actual missing capability.
- Alternative approaches.
- Expected benefit.
- Implementation complexity.
- Security implications.
- Maintenance cost.
- Performance measurement strategy.
- Acceptance criteria.

Where practical, measure:

- Cold startup.
- Warm startup.
- Executable size.
- RSS and proportional memory.
- CPU utilization.
- Worker initialization.
- IPC latency.
- Serialization throughput.
- Concurrent agent execution.
- Token streaming overhead.
- Model residency.
- Embedding throughput.
- Cache effectiveness.
- Index query latency.

Do not fabricate benchmark results.

## **16. Required deliverables**

Produce:

1. Executive architectural assessment.
2. Verified current-state architecture.
3. Cross-gateway capability matrix.
4. OMP2 existing-versus-missing capability matrix.
5. Provider integration recommendations.
6. Transport and protocol recommendations.
7. ABI-independent extension assessment.
8. Python runtime comparative assessment.
9. Single-session swarm assessment.
10. Local inference assessment.
11. Shared code intelligence and SCIP assessment.
12. Proposed architecture diagrams.
13. Performance and benchmark plan.
14. Security and licensing assessment.
15. Prioritized architectural backlog.
16. Draft ADRs only where justified.
17. Incremental implementation roadmap.
18. Explicitly rejected alternatives and reasons.
19. Source references with pinned commits and relevant paths.
20. Open questions requiring architectural approval.

## **17. Decision classification**

Classify every proposed capability as:

- Already implemented: reuse.
- Partially implemented: extend.
- Missing and justified: propose.
- Suitable as an optional extension.
- Suitable as an external integration.
- Requires benchmarking.
- Requires ADR reconsideration.
- Unnecessary or redundant.
- Rejected for security, licensing or operational reasons.

## **18. Final architectural principle**

OMP2 must remain a lightweight, modular native Rust agent harness.

Gateway capabilities should strengthen its existing provider runtime, not introduce a second application architecture inside it.

The preferred design is one root OMP session orchestrating multiple logical agents, sharing appropriate native infrastructure and optionally spawning lightweight isolated workers when justified.

Use deterministic mechanisms wherever possible.

Avoid native ABI dependencies between independently distributed extensions.

Do not implement architectural changes until the research, evidence, diagrams, alternatives and proposed ADRs have been reviewed.
