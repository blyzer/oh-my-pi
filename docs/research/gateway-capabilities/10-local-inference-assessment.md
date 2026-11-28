# Local inference assessment

> Part of the [OMP2 gateway-capabilities research](README.md). Deliverable 10. Audited commit `85d219ae7f` (`origin/omp2`). Research proposal: nothing here is implemented.

Area `INF`, dimension IDs L01 to L16. Evidence: [`evidence/omp2-local-inference.md`](evidence/omp2-local-inference.md), [`evidence/omp2-local-inference.tsv`](evidence/omp2-local-inference.tsv), [`evidence/ref-local-inference.md`](evidence/ref-local-inference.md) and the five `matrix-*.tsv` files for llama.cpp, MLX, mlx-lm, candle and coremltools. Diagram: [diagram 15](diagrams/15-shared-inference-workers.md) (PROPOSED). Current-state context: [deliverable 2](02-verified-current-state.md).

## 1. Summary

1. OMP2 has more local-inference scaffolding than it uses. A lazy embedding worker, a serialized `LocalRuntime`, verified artifact stores and an Apple FM bridge exist; the worker has no caller, recall passes no vector, and the tiny GGUF catalog has no runner.
2. llama.cpp, Ollama and LM Studio already work as local servers through loopback discovery. That covers the "local server" class without any new binding graph.
3. The locked rule (candle only, no C or C++ binding graphs, `AGENTS.md:576`) is met by the default build. It is already stretched by opt-in `fastembed`, which downloads ONNX Runtime. The cleanest fix is a boundary: C++ runtimes only out of process (INF-03, INF-07).
4. The binding constraint on the target machine is memory, not compute. Pools are per consumer, so nothing bounds the sum (INF-02).
5. Nothing here is measured. Section 8 is a plan.

## 2. Host

Captured output (full file in the evidence document):

```
$ sysctl -n machdep.cpu.brand_string hw.memsize hw.ncpu
Apple M1 Pro
17179869184
10
$ uname -m
arm64
$ uptime
22:57  up 3 days, 11:21, 1 user, load averages: 24.15 21.84 18.42
$ sw_vers
ProductName:		macOS
ProductVersion:		27.2.1
BuildVersion:		26B131
$ sysctl hw.perflevel0.physicalcpu hw.perflevel1.physicalcpu
hw.perflevel0.physicalcpu: 8
hw.perflevel1.physicalcpu: 2
Sat Nov 28 02:57:48 UTC 2026
```

The load average of about 24 on 10 cores means any timing taken now is contaminated. 16 GiB is unified memory shared with the OS and every other process.

## 3. Vocabulary: five execution classes, and weights versus state

| Class | Meaning | OMP2 today | Example |
|---|---|---|---|
| In-process inference | Model runs in the OMP process | Candle speech; Apple FM through the ABI seam; `fastembed` in `EmbeddingAdapter` (no caller) | `crates/ai/src/local/runtime.rs:246` |
| Local subprocess inference | OMP starts and owns a child | Memory embedding worker, NDJSON over stdio | `crates/memory/src/embedding/supervisor.rs:50` |
| Local server inference | A long-lived server on loopback that OMP talks to over HTTP | llama.cpp, Ollama, LM Studio discovery | `crates/ai/src/discovery/endpoints.rs:113` |
| Remote inference | A hosted model answers | All frontier providers; Apple PCC (blocked) | `docs/adr/0042-apple-pcc-integration.md:7` |
| Remote agent execution | A remote host runs the agent's tools or loop | ADW remote driver files exist (`crates/driver/src/adw/remote.rs`); not part of this area | deliverable 2, section 8 |

**Weights versus state.** Shared weights are read-only and can be held once per machine. KV caches and generation state belong to one sequence. llama.cpp keeps one model and a `server_slot` per concurrent sequence (`ggml-org/llama.cpp@8e23278:tools/server/server-context.cpp`); mlx-lm keeps an `LRUPromptCache` of per-sequence caches (`ml-explore/mlx-lm@6fe0ce4:mlx_lm/server.py`). The design below shares weights across agents and never shares KV or generation state between agents. `LocalRuntime` already enforces one request at a time per engine (`crates/ai/src/local/runtime.rs:258`).

## 4. Runtime comparison against the locked rule

The rule: local audio and ML via candle, never C or C++ binding graphs (`AGENTS.md:576`; ADR 0023 rule 5, `docs/adr/0023-tiny-local-model.md:20`).

| Runtime | Written in | How OMP2 would reach it | Rule | Maturity or evidence | Card |
|---|---|---|---|---|---|
| Candle | Rust | In process (speech today) or in the worker | Conforms | `Prototype` for speech, Metal supported | INF-08, INF-09 |
| ONNX Runtime CPU (via fastembed) | C++ | In process (adapter, unused) or in the worker | Out of letter in process; defensible in the worker | `Prototype`; docs only, code not read | INF-07 |
| ONNX Runtime with CoreML provider | C++ plus OS framework | Same | Same | `Absent`; documentation only, code not read | INF-06 |
| MLX / mlx-lm | C++ core, Python | Local server only | Out of process, no binding graph | `Absent` | INF-05 |
| llama.cpp | C/C++ | Local server (works today); in-process binding barred | Server: no binding graph; in-process: violates | Server `Implemented`, in-process `Absent` | INF-03, INF-04 |
| Direct Core ML | OS framework (coremltools is Python tooling) | Dynamic ABI seam like Apple FM | No C binding crate; Swift or Objective-C ABI | `Absent` | INF-06 |
| Apple FM | OS framework | Dynamic ABI seam (`crates/ai/src/local/applefm/aarch64.s`) | Conforms (no binding crate) | `Prototype` (opt-in feature) | INF-11 |

## 5. Allocation proposal (PROPOSED, not implemented, not measured)

| Workload | Proposed placement | Class | Reason | Fallback |
|---|---|---|---|---|
| Indexing, parsing, scheduling, validation, state transitions, evidence | CPU, inside the OMP process | In process | Deterministic work belongs on CPU (owner rule 4) | None needed |
| Embeddings (bge-base 768 d, e5-large 1024 d) | Shared worker; CPU first; ANE path only if B5 shows a gain | Local subprocess | Isolation, one resident copy | Remote `Embed` route; recall without vectors |
| Rerank | Same worker (cross-encoder) or Apple FM on eligible Macs | Local subprocess or in process | Small candidate sets | Fusion order |
| Closed-ladder classification | Deterministic rules first; encoder in the worker if rules fail | Local subprocess | Owner rule 4 and 5 | Online `tiny` role |
| Tiny generation (titles, extraction) | Apple FM where eligible; llama-server on GPU where the user runs one; else the online `tiny` role (today's default) | In process or local server | No generator exists in OMP2 | Online `tiny` role |
| Speech | Candle with Metal (existing, opt-in) | In process | Already built | CPU |
| Complex semantic reasoning, planning | Remote frontier models | Remote inference | Quality | Other provider by routing |

On an M1 Pro, the Neural Engine is reachable only through Core ML, either directly or through an ONNX Runtime provider that takes only the operators it supports (ORT docs claim). Whether the ANE helps these encoders is the question of B5. The GPU is reached through candle Metal, Apple FM, llama-server or MLX.

Proposed memory rule for a 16 GiB host (proposal, not measured): one process-wide `MemoryPool` whose default limit is a fraction of physical memory chosen after B1 to B3, loading at most one embedding model and one generator at a time.

## 6. Shared-residency design (PROPOSED)

1. **Per process.** Replace per-consumer pools by one `MemoryPool` that every `LocalRuntime` and the Apple FM admission draw from. Reuses `MemoryPool`, `AdmissionControl`, `RuntimeLease` (`crates/ai/src/local/runtime.rs:98`, `:166`, `:363`). Subagents already live in the process and share `MemoryRuntime` (`crates/envd/src/memory.rs:101`), so they share the pool for free.
2. **Per machine.** The embedding worker is the only component that needs cross-process sharing, because it is a child process and two OMP processes would each start one. Proposal: the first supervisor takes a per-user lock and a loopback or Unix-socket endpoint; later supervisors attach as clients of the same worker. Frames, size caps and generation fencing stay as in `crates/memory/src/embedding/protocol.rs:12`. No new scheduler or registry.
3. **Servers.** For llama.cpp and mlx-lm the server owns weights; OMP sends one slot or one sequence per agent and never assumes KV is shared. Router-style eviction belongs to the server (`ggml-org/llama.cpp@8e23278:tools/server/server-models-sched.cpp`), not to OMP.
4. **Failure.** A reservation failure returns `LocalErrorKind::Overloaded` and the caller applies its explicit policy; a pinned local model is never silently promoted to a hosted one (ADR 0023 rule 4).
5. **Vector transport.** Vectors are JSON floats today. If B1 shows the cost matters, a binary frame is an option; it is not proposed now.

## 7. Cards

### INF-01 Wire or retire the memory embedding worker
**Decision class:** Partially implemented: extend
**Existing implementation evidence:** `EmbeddingSupervisor` and the stdio worker exist (`crates/memory/src/embedding/supervisor.rs:50`, `crates/memory/src/embedding/worker.rs:8`), maturity `Prototype`. `rebuild_local_embeddings` (`crates/memory/src/runtime.rs:714`) has no caller and recall passes no vector (`crates/tools/src/memory.rs:261`).
**Actual missing capability:** A production owner for the supervisor, a caller of `rebuild_local_embeddings`, a query-embedding step before `search`, and a decision whether the unused `EmbeddingAdapter` (`crates/ai/src/local/embedding.rs:195`) is deleted.
**Alternatives considered:** Leave it dormant; use the typed `Embed` operation against a remote or local server (`crates/ai/src/client.rs:70`); delete the worker.
**Expected benefit:** Reduced billable tokens and network transmission for memory recall if dense vectors are on; latency; offline recall. Whether dense recall improves quality is not measured.
**Implementation complexity:** medium: one construction site and one async step in recall, no new protocol.
**Security implications:** The worker runs a native ONNX Runtime in a child process; frames are size-capped and JSON-validated (`crates/memory/src/embedding/protocol.rs:12`). Model ids are charset-validated. Model download comes from Hugging Face at first use.
**Maintenance cost:** Low: code already exists and is isolated; cost is keeping fastembed upgrades compatible.
**Performance measurement strategy:** B1 and B2 in section 8 (`not measured`).
**Acceptance criteria:** With vectors enabled, a recall query produces a query vector via the worker; with the feature off, behaviour and binary size are unchanged; a killed worker is restarted with a new generation and no stale vector is accepted; a test covers each.
**Follow-up:** Do not start before INF-02 decides where the supervisor lives.

### INF-02 Process-wide shared model residency and one memory budget
**Decision class:** Missing and justified: propose
**Existing implementation evidence:** `LocalRuntime` shares one lazily loaded engine across clones and unloads on idle (`crates/ai/src/local/runtime.rs:246`, `:336`). Pools are made per consumer (`crates/app/src/chat_voice.rs:834`, `crates/app/src/say_cmd.rs:60`, `crates/envd/src/media_tts.rs:482`); Apple FM has a separate admission (`crates/ai/src/local/applefm.rs:100`). `MemoryRuntime` is already shared with subagents (`crates/envd/src/memory.rs:101`).
**Actual missing capability:** One `MemoryPool` per process (and a per-machine lock or socket for the embedding worker so two OMP processes do not each load the weights), reported through the existing status surfaces. Weights are shared; KV and generation state remain per request or per slot.
**Alternatives considered:** Keep per-consumer pools; a separate inference daemon with its own scheduler (rejected by owner rule 8 as a redundant scheduler); rely on the OS to page.
**Expected benefit:** Memory (the binding constraint on a 16 GB M1 Pro), avoided duplicate weight loads, predictable admission failures.
**Implementation complexity:** medium: a shared handle plus a machine-scope lease; no new scheduler.
**Security implications:** A per-machine endpoint must be per-user, mode 0600 and verify the peer; a shared worker must not see another user's text.
**Maintenance cost:** Medium: cross-process lifecycle and stale-lock handling.
**Performance measurement strategy:** B2 in section 8 (`not measured`).
**Acceptance criteria:** Two concurrent OMP processes with the feature on load one embedding model between them; a failed reservation returns `LocalErrorKind::Overloaded`; subagents in one process add no extra resident copy; tests cover crash of the owner.
**Follow-up:** Needs a Draft ADR (section 9).

### INF-03 llama.cpp as an external local server
**Decision class:** Suitable as an external integration
**Existing implementation evidence:** Loopback discovery of `llama-server` on 8080 with `/props` context probing (`crates/ai/src/discovery/endpoints.rs:113`, `crates/ai/src/discovery/probe.rs:896`); provider entry `crates/catalog/compat/providers/llama.cpp.kdl:3`. Maturity `Implemented` for chat discovery.
**Actual missing capability:** Documented, tested use of the server's embeddings and rerank routes through the typed `Embed` operation, and slot-aware behaviour (one slot per concurrent agent, no assumption that KV is shared). Not verified: that `Embed` routes to a local server end to end.
**Alternatives considered:** Bind llama.cpp in process (INF-04); write a native generator (INF-09); use Ollama only.
**Expected benefit:** Reduced billable tokens for chores and embeddings, offline use, GPU through Metal on macOS and CUDA or CPU on Linux, with no new Rust dependency.
**Implementation complexity:** low: an existing route plus documentation and a contract test against a fake server.
**Security implications:** Server is an unauthenticated loopback HTTP listener by default; OMP must keep it on loopback, treat output as untrusted and never send secrets to it. The user, not OMP, installs the binary.
**Maintenance cost:** Low for OMP; the server's flags and API drift with upstream.
**Performance measurement strategy:** B3 in section 8 (`not measured`).
**Acceptance criteria:** A fake `llama-server` test proves embeddings and rerank requests and error mapping; discovery never probes beyond the known loopback endpoints; docs say weights are shared and KV is per slot.
**Follow-up:** Extend with INF-05 if the benchmark favours MLX.

### INF-04 llama.cpp linked in process
**Decision class:** Requires ADR reconsideration
**Existing implementation evidence:** None. `Cargo.lock` has no `llama` crate (parent grep). `AGENTS.md:576` and ADR 0023 rule 5 forbid `llama-cpp` bindings; the stale `TinyDevice::gpu_layers` mapping (`crates/ai/src/local/device.rs:83`) is the only trace.
**Actual missing capability:** Nothing is missing that INF-03 does not cover with a process boundary. An in-process binding would need the owner to reverse a locked decision.
**Alternatives considered:** INF-03 (server), INF-09 (candle generator), Apple FM (already native).
**Expected benefit:** In-process saves one IPC hop; no measurement shows the hop matters.
**Implementation complexity:** high: C++ build graph, cold-build gate (AGENTS.md), crash isolation lost.
**Security implications:** A C++ parser of untrusted GGUF files would run in the agent process, with no isolation.
**Maintenance cost:** High: upstream churn and build matrix.
**Performance measurement strategy:** `not applicable` until the owner reopens the rule.
**Acceptance criteria:** Owner approval and a Draft ADR; otherwise the card is closed.
**Follow-up:** Recommendation: do not reopen.

### INF-05 MLX and mlx-lm as an optional managed server
**Decision class:** Suitable as an optional extension
**Existing implementation evidence:** Absent: no `mlx` in `Cargo.lock`. Local-server routing exists (`crates/ai/src/discovery/endpoints.rs:113`) but has no MLX endpoint kind. Reference: `ml-explore/mlx-lm@6fe0ce4:mlx_lm/server.py`.
**Actual missing capability:** An extension that starts `mlx_lm.server` on loopback on Apple Silicon only, with a per-sequence prompt cache and OMP-owned lifecycle (start lazily, stop on idle).
**Alternatives considered:** Run it by hand and add a catalog provider (that is the external-integration path, already possible); candle (INF-08); llama-server with Metal (INF-03).
**Expected benefit:** Possibly better Metal throughput on Apple Silicon; unified memory. Not measured.
**Implementation complexity:** medium: extension plus Python dependency management, which ADR 0036 constrains.
**Security implications:** Python process with network access to model hubs; run it under the extension permission model.
**Maintenance cost:** Medium: tracks fast-moving upstream.
**Performance measurement strategy:** B3 with MLX added (`not measured`).
**Acceptance criteria:** Extension is off by default, refuses on non-Apple-Silicon hosts, reports memory use, and stops after idle timeout.
**Follow-up:** Only if B3 shows a material win over llama-server.

### INF-06 Core ML and the Neural Engine for embeddings and classifiers
**Decision class:** Requires benchmarking
**Existing implementation evidence:** None for Core ML (`TinyDevice::Coreml` at `crates/ai/src/local/device.rs:12` is unused). Apple FM is native but placement is OS-chosen (`crates/ai/src/local/applefm.rs:100`). Reference: `apple/coremltools@a207b1a:coremltools/models/model.py` (`ComputeUnit.CPU_AND_NE`).
**Actual missing capability:** Evidence that ANE placement of a small encoder beats the CPU on this machine for energy or latency, and a way to reach it from Rust without a binding graph.
**Alternatives considered:** ONNX Runtime CoreML provider (INF-07); direct Core ML through the dynamic ABI seam used for Apple FM; leave embeddings on CPU.
**Expected benefit:** Lower CPU load and power; frees cores for tool execution. Not measured; partial operator coverage may leave most of the graph on CPU (ORT docs claim).
**Implementation complexity:** high: model conversion (Python tooling, offline), static shapes, Swift or Objective-C ABI.
**Security implications:** Compiled model artifacts need the same SHA-256 verification as `ArtifactStore` (`crates/ai/src/local/artifact.rs`).
**Maintenance cost:** High: per-OS-release behaviour.
**Performance measurement strategy:** B5 in section 8 (`not measured`).
**Acceptance criteria:** A measurement file shows latency, RSS and compute-unit placement for CPU only, CPU and NE, and all units on this host, with load average recorded.
**Follow-up:** Build nothing before B5.

### INF-07 Confine ONNX Runtime to the out-of-process worker
**Decision class:** Requires ADR reconsideration
**Existing implementation evidence:** `fastembed` with `ort-download-binaries-rustls-tls` (`Cargo.toml:60`). In process: `EmbeddingAdapter` (`crates/ai/src/local/embedding.rs:195`, no caller). Out of process: the memory worker (`crates/memory/src/embedding/worker.rs:8`). ADR 0023 rule 5 prefers candle only.
**Actual missing capability:** An explicit ADR statement that ONNX Runtime is allowed inside the worker process only, plus removal or gating of the in-process adapter so the graph rule is testable (for example a `cargo tree` check).
**Alternatives considered:** Replace with candle BERT (INF-08); allow ORT everywhere; ban fastembed.
**Expected benefit:** Keeps the C++ runtime off the agent process, which keeps crash and memory isolation; no new code.
**Implementation complexity:** low: policy plus one deletion.
**Security implications:** ORT binaries are downloaded at build time over TLS; supply-chain exposure sits in the build, not at runtime. Pin and verify.
**Maintenance cost:** Low.
**Performance measurement strategy:** `not applicable`
**Acceptance criteria:** The ADR states the boundary; `cargo tree -i ort` shows it only under the worker's feature; the in-process adapter is gone or unreachable from default and `local-all` paths.
**Follow-up:** Documentation only for ONNX Runtime: docs read, code not read.

### INF-08 Candle encoders for embeddings and rerank
**Decision class:** Requires benchmarking
**Existing implementation evidence:** Candle is already in the graph for speech (`Cargo.toml:42`, `crates/ai/src/local/stt.rs:433`). Reference: `huggingface/candle@9c1fcff:candle-transformers/src/models/bert.rs`.
**Actual missing capability:** A candle BERT or ModernBERT embedder behind the same worker protocol, to meet the locked rule without ORT, and a measurement against ORT CPU.
**Alternatives considered:** Keep fastembed (INF-07); llama-server embeddings (INF-03).
**Expected benefit:** Rule conformance, smaller dependency graph, Metal path through the existing `metal` feature. Speed and accuracy not measured.
**Implementation complexity:** medium: model loading, tokenizer and pooling code in the worker.
**Security implications:** Pure Rust; safetensors loading avoids pickle risk.
**Maintenance cost:** Medium: OMP owns model code paths.
**Performance measurement strategy:** B1 (`not measured`).
**Acceptance criteria:** Same vectors within a stated tolerance of the ORT result on a fixed corpus, same frame protocol, lower or equal RSS.
**Follow-up:** Extend if B1 passes.

### INF-09 In-process tiny generator for title, memory and classifier chores
**Decision class:** Requires benchmarking
**Existing implementation evidence:** Catalog of verified GGUF artifacts but no runner (`crates/ai/src/local/tiny_catalog.rs:101`, `:180`); the `tiny` role resolves to an online model (`docs/adr/0023-tiny-local-model.md:62`); chores other than speech rewriting have no caller. Reference: `huggingface/candle@9c1fcff:candle-transformers/src/models/quantized_lfm2.rs`.
**Actual missing capability:** First, callers: nothing calls the tiny role for titles or classification. Only then a generator. Apple FM already gives an on-device option on eligible Macs.
**Alternatives considered:** Keep the online `tiny` role; Apple FM; llama-server (INF-03); deterministic rules as in ADR 0062.
**Expected benefit:** Reduced billable tokens and one fewer hosted round trip per chore, offline operation. Not measured.
**Implementation complexity:** high: model runner, template handling, constrained output, plus callers.
**Security implications:** Pinned local models must never silently fall back to hosted ones (ADR 0023 rule 4).
**Maintenance cost:** High.
**Performance measurement strategy:** B3 and B4 (`not measured`).
**Acceptance criteria:** A caller exists for at least one chore, the local path beats the online `tiny` role on latency on this host, and failure does not promote to a hosted model unless the caller's policy says so.
**Follow-up:** Prefer deterministic rules where they suffice.

### INF-10 Typed hardware placement policy replacing the dead TinyDevice
**Decision class:** Missing and justified: propose
**Existing implementation evidence:** `TinyDevice` and `gpu_layers` are unreferenced (`crates/ai/src/local/device.rs:12`, `:83`); each engine hard-codes its device (`crates/ai/src/local/tts.rs:397`, `crates/ai/src/local/parakeet.rs:240`).
**Actual missing capability:** One typed policy that maps a workload (embed, rerank, classify, generate, speech) to CPU, GPU, ANE-capable path or remote, from host facts and the memory pool, with the allocation in section 5 as the default.
**Alternatives considered:** Per-engine flags as today; an environment variable only.
**Expected benefit:** Predictable placement on 16 GB hardware, fewer surprise Metal loads.
**Implementation complexity:** medium: one enum, one resolver, tests.
**Security implications:** None beyond not widening the surface; placement never routes local-pinned data to a remote.
**Maintenance cost:** Low.
**Performance measurement strategy:** Reuse B1 to B5 results (`not measured`).
**Acceptance criteria:** Resolver is pure and unit-tested; remote is chosen only by explicit policy; no caller reads `TinyDevice` after the change.
**Follow-up:** Depends on INF-02.

### INF-11 Local reranker lane
**Decision class:** Partially implemented: extend
**Existing implementation evidence:** `aux_lane::rank` (Apple FM, 0 to 10 score) behind `apple-aux-memory`, test-only caller (`crates/memory/src/aux_lane.rs:17`); `search_async` was added to host it (`crates/memory/src/runtime.rs:439`). Reference: `ggml-org/llama.cpp@8e23278:tools/server/server-context.cpp:534` (rank pooling).
**Actual missing capability:** A real call site in recall, a score contract that is not a generated integer, and a non-Apple option (cross-encoder in the worker or llama-server `/reranking`).
**Alternatives considered:** No reranker (reciprocal-rank fusion only); remote rerank API.
**Expected benefit:** Better precision for a few candidates at local cost. Not measured.
**Implementation complexity:** medium.
**Security implications:** Candidate memory text goes to the model; local only.
**Maintenance cost:** Medium.
**Performance measurement strategy:** B4 (`not measured`).
**Acceptance criteria:** Recall with the lane on never returns fewer results than with it off, falls back to input order on any fault, and has a test for each.
**Follow-up:** Follow INF-01.

### INF-12 Linux without Docker or Podman
**Decision class:** Partially implemented: extend
**Existing implementation evidence:** Default `omp-ai` features are empty (`crates/ai/Cargo.toml:16`); Apple FM is macOS only; ORT binaries are downloaded (`Cargo.toml:60`). No Linux build of the local features was run in this research.
**Actual missing capability:** A documented and CI-checked Linux path: CPU-only embedding worker, optional llama-server binary the user installs, no container.
**Alternatives considered:** Ship a container image (rejected by the owner rule).
**Expected benefit:** Meets the owner requirement for default deployment.
**Implementation complexity:** low.
**Security implications:** Downloaded binaries need pinning.
**Maintenance cost:** Low.
**Performance measurement strategy:** `not measured`; run B1 on a Linux host.
**Acceptance criteria:** CI builds and runs the worker tests on Linux with no container runtime present.
**Follow-up:** Pair with INF-07.

## 8. Benchmark plan (`not measured`)

Nothing below has been run. Each run follows the method: record `uname -m`, memory, core count and `uptime` before and after, because other work runs on the machine (load average was about 24 at capture), and keep raw output under `evidence/measurements/`.

| ID | Question | Planned command or action | Metrics | Runs |
|---|---|---|---|---|
| B1 | Embedding worker cold start, throughput, RSS: ORT CPU versus candle BERT | Send a fixed 1,000-text corpus through the worker protocol in batches of 1, 8 and 32; `ps -o rss= -p <pid>` before, during, after | Load time, p50 and p95 per batch, RSS, vector agreement | 5 per setting |
| B2 | Duplication cost | Start 1, 2 and 4 OMP processes with the worker on, then one shared worker once INF-02 exists | Sum of RSS, time to first vector | 3 |
| B3 | llama-server (and later mlx-lm) for a tiny model such as `lfm2-700m` | `llama-server -m <gguf> --parallel N` with N of 1, 2, 4, driven by a fixed prompt set; slot save and restore | Time to first token, tokens per second, RSS, slot isolation | 5 |
| B4 | Apple FM `rank` latency and quality | Fixed 20-candidate recall set, on and off | Latency per candidate, order change, fault fallback | 5 |
| B5 | ANE versus CPU for a small encoder | Core ML model with `CPU_ONLY`, `CPU_AND_NE`, `ALL` (coremltools, offline); ORT CoreML provider with the same options | Latency, power sample if the owner allows `powermetrics` (needs sudo, owner decision), placement report | 5 |

Acceptance thresholds are proposals to be fixed before running; no number is claimed here.

## 9. Draft ADR needed?

Yes. One draft ADR, "Local inference placement and shared residency", to be proposed to the owner without editing ADR 0023 or 0036. It would: (a) state that C or C++ runtimes are allowed out of process only (INF-03, INF-05, INF-07) and in-process only through a native OS framework seam; (b) fix the per-process pool and the per-machine embedding worker (INF-02); (c) replace `TinyDevice` (INF-10). INF-04 (in-process llama.cpp) stays rejected unless the owner reopens the locked rule. The draft is not written in this pass.

## 10. What this assessment did not do

- No build, test, model download or timing.
- ONNX Runtime facts come from official documentation only; code not read.
- mlx, mlx-lm, candle and coremltools were read at architecture level (listings, a few symbols); only llama.cpp's server was read in more depth.
- `parakeet.rs`, `stt.rs`, `tts.rs` and `artifact.rs` bodies were not read.
- Linux behaviour of the local features was not built or run.

Back to the [index](README.md).
