# Performance and benchmark plan

> Part of the [OMP2 gateway-capabilities research](README.md). Deliverable 13. Audited commit `85d219ae7f`. Research proposal: nothing here is implemented.

## 1. Rules

1. Nothing is reported as measured unless the command, the environment and the raw output are stored under `evidence/measurements/`. Everything else in this set is `not measured` and appears below as a plan.
2. Every measurement records `uname -m`, memory, core count and `uptime`, because this machine runs other work in parallel. Load averages were 7 to 24 on 10 cores during this research, so **no absolute timing in this set is a clean baseline**.
3. At least 30 runs after warm-up, median and spread, not a mean of three runs. A cold-start number needs a purged page cache; `purge` needs root, so none of the numbers below is a true cold start.
4. Do not adopt a format, runtime or architecture from a micro-benchmark alone. Zero-copy serialization in particular must win on a payload shaped like OMP2's own, end to end, including validation.
5. This plan sets **no pass/fail thresholds**. A threshold is an owner decision (see [open questions](20-open-questions.md)); inventing one before the first baseline would be fabrication.

## 2. Measurements actually made

| File | What | Result as recorded | Limits |
|---|---|---|---|
| [`11-catalog-census.md`](evidence/measurements/11-catalog-census.md) | Count of the embedded catalog snapshot (`omp-catalog`, `Catalog::embedded()`) | 125 providers, 171 routes, 5,289 models, 230 aliases, 33 distinct codec ids; routes by transport: Http 165, AwsEventStream 2, Connect 2, Local 1, Websocket 1 | a count, deterministic, one run; not a performance figure |
| [`60-python-footprint.md`](evidence/measurements/60-python-footprint.md) | On-disk size of the vendored interpreter install trees and search for a built `omp` binary | `vendor/python-release/install` 182 MB, `vendor/python/install` 166 MB; no built `omp` baseline exists (only a debug e2e host) | install trees are build inputs, not the shipped size; no binary-size delta measured |
| [`61-python-subprocess-startup.md`](evidence/measurements/61-python-subprocess-startup.md) | `python3 -c pass` startup, 30 runs per case | homebrew 3.15.0: median 49.4 ms (min 40.6, max 94.1); vendored 3.14t release build: median 27.3 ms of runs 2 to 30 (first run 627 ms, a one-off first-exec cost) | host under load; "first run" is not a purged-cache cold start; says nothing about the embedded child |
| [`62-jsonl-roundtrip.md`](evidence/measurements/62-jsonl-roundtrip.md) | JSON-lines round trip over pipes to a persistent child Python, 1,000 calls | homebrew: median 14 us, p95 19 us; vendored 3.14t: median 17 us, p95 22 us | an echo child, so a floor for the subprocess boundary only; excludes dispatch, bridge, serde and PyO3 callbacks |

## 3. What the owner asked to measure, and where it stands

| Owner item (section 15) | Status | Plan |
|---|---|---|
| Cold startup | not measured (no purged-cache run; only first-run of a standalone interpreter) | PY-02, SWM-08 |
| Warm startup | partly measured for standalone CPython only (table above) | PY-02 for the embedded child |
| Executable size | not measured (no built baseline binary) | PY-02: `just build-release` with and without the interpreter link, `stat` both |
| RSS and proportional memory | not measured | SWM-08, INF B1/B2, PY-02 |
| CPU utilization | not measured | per-card strategies below |
| Worker initialization | not measured | INF B1, SWM B1 |
| IPC latency | floor measured for a pipe and JSON (62); Protobuf, NDJSON and the real bridge not measured | IPC plan (section 4) |
| Serialization throughput | not measured (the IPC agent did not run its benchmark) | IPC E1 to E5, T1, L1 to L4 |
| Concurrent agent execution | not measured | SWM B1 to B6 |
| Token streaming overhead | not measured | IPC T1; PRV observer strategies |
| Model residency | not measured | INF B1, B2 |
| Embedding throughput | not measured | INF B1, IPC E1 to E4 |
| Cache effectiveness | not measured | PRV-06 strategy |
| Index query latency | not measured | CIX plan (deliverable 11) |

## 4. Consolidated plan, from the capability cards

Each row is the `Performance measurement strategy` line of a capability card, shortened. The card holds the full text and acceptance criteria. Cards whose strategy is "not applicable" are omitted.

| Card | Title | Decision class | Strategy (shortened) |
|---|---|---|---|
| [PRV-01](05-provider-integration-recommendations.md) | Route runtime evidence from observed attempts | Partially implemented: extend | Replay a recorded failure-heavy session against fake providers (`crates/ai/src/provider/fake.rs`, cassette transport `crates/ai/src/transport/cassette.rs`) and compare total first-attempt failures and end-to-end latency with and without the observer. Not me... |
| [PRV-02](05-provider-integration-recommendations.md) | Header-driven rate and quota observation into the account pool | Partially implemented: extend | Cassette replay of recorded 429 sequences; count wasted attempts before and after. Not measured. |
| [PRV-03](05-provider-integration-recommendations.md) | Circuit breaker as a health state | Requires benchmarking | Count, from existing session journals or a replay, how often consecutive calls hit the same failing route; plan: instrument via `provider_request` spans (`registry.rs:785`). |
| [PRV-06](05-provider-integration-recommendations.md) | Route-level affinity for prompt-cache reuse | Partially implemented: extend | Cache read share (`cache_read_tokens` over input tokens) across multi-turn replays with and without route affinity, using the receipt counters. Not measured. |
| [PRV-07](05-provider-integration-recommendations.md) | Unit-aware cost ranking | Partially implemented: extend | Count model keys served by more than one route in the snapshot to size the benefit; not measured here. |
| [AUTH-01](05b-authentication-and-credential-lifecycle.md) | Per-process refresh-owner token for the persistent lease | Partially implemented: extend | two to eight processes against one temp `credentials.db` and a stub token endpoint that rotates refresh tokens and counts calls; compare provider refresh calls per expiry for constant versus per-process owner. Run on the remote build host (AGENTS.md remote-... |
| [AUTH-03](05b-authentication-and-credential-lifecycle.md) | Share one auth stack across in-process subagents | Partially implemented: extend | spawn 1, 4, 16 children with a no-op prompt on a stub provider; record wall time to first request, RSS and number of SQLite opens and discovery fetches for `gateway: None` versus a shared handle; record `uptime`. Not measured. |
| [AUTH-04](05b-authentication-and-credential-lifecycle.md) | OS credential facility for the master key on Linux and Windows | Missing and justified: propose | cold `cargo run --bin omp` build time and binary size delta with the feature on and off; first-lease latency. Not measured. |
| [AUTH-05](05b-authentication-and-credential-lifecycle.md) | Refresh keeper behaviour inside the existing broker daemon | Requires benchmarking | measure the p50 and p95 extra latency of a refresh on the request path against a stub endpoint, and the provider-side idle expiry of each bundled OAuth provider from its documentation. Not measured. |
| [AUTH-08](05b-authentication-and-credential-lifecycle.md) | Entitlement and region facts as catalog data feeding existing eligi... | Partially implemented: extend | count refused-by-entitlement requests per account-model pair in a recorded session set; not measured. |
| [AUTH-11](05b-authentication-and-credential-lifecycle.md) | At-most-once refresh send for single-use refresh tokens | Partially implemented: extend | crash-injection tests at three points with a stub endpoint that rotates and enforces reuse detection; count revoked families with and without the marker. Not measured. |
| [PRV-11](06-transport-and-protocol-recommendations.md) | WebRTC feature weight in the default app build | Requires benchmarking | `cargo build --timings` cold for `omp` with and without `omp-ai/realtime`, crate count from `cargo tree`, stripped binary size. Not measured here. |
| [PRV-13](06-transport-and-protocol-recommendations.md) | OpenAI-compatible HTTP facade over `omp-serve` | Suitable as an optional extension | Added latency of the facade over the gRPC path, per-token overhead; not measured. |
| [PRV-14](06-transport-and-protocol-recommendations.md) | Keep canonical IR; no pairwise translators | Already implemented: reuse | Token streaming overhead per codec is not measured; plan: replay a cassette through each decoder and time frames per second. |
| [PRV-16](06-transport-and-protocol-recommendations.md) | Queue-latency and per-route metrics | Missing and justified: propose | Overhead of one counter increment per attempt, not measured. |
| [PRV-17](06-transport-and-protocol-recommendations.md) | End-to-end streaming backpressure on HTTP | Requires benchmarking | Fake streaming server with a stalled reader; record RSS growth over N MB streamed. Not measured. |
| [IPC-02](06b-ipc-and-serialization.md) | Embedding batches: compact vector framing for the memory embedding... | Requires benchmarking | Benchmark plan section 6, cases E1 to E5, 1000x384 f32 batch; compare serde_json, prost and rmp-serde, then the NDJSON `OutboundFrame` path. `not measured`. |
| [IPC-03](06b-ipc-and-serialization.md) | Blob upload that hashes while streaming instead of buffering the wh... | Partially implemented: extend | Peak RSS during N concurrent 64 MiB uploads, before and after. `not measured`. |
| [IPC-04](06b-ipc-and-serialization.md) | Large IR payloads (CodeIR, ChangeIR, VerificationIR): Protobuf mess... | Requires benchmarking | Benchmark plan section 6, cases L1 to L4 on a synthetic IR shaped from a real SCIP index of this repository (size distribution first). `not measured`. |
| [IPC-05](06b-ipc-and-serialization.md) | Zero-copy formats (Cap'n Proto, FlatBuffers, rkyv): reconsider the... | Requires ADR reconsideration | Inputs come from IPC-02 and IPC-04 benchmarks. |
| [IPC-07](06b-ipc-and-serialization.md) | MessagePack for internal JSON channels | Unnecessary or redundant | MessagePack arm of the IPC-02 benchmark; `not measured`. |
| [IPC-08](06b-ipc-and-serialization.md) | Shared memory and memory-mapped files for local bulk transfer | Unnecessary or redundant | Include a socket-copy versus mmap arm for CAS blob reads in case L4 only if IPC-04 shows a large-read bottleneck; `not measured`. |
| [IPC-09](06b-ipc-and-serialization.md) | Decode-time allocation and optional compression on Protobuf boundaries | Suitable as an optional extension | Peak RSS with a declared-length-only frame; TCP bytes on the wire for a large `Turn` with and without compression. `not measured`. |
| [EXT-04](07-abi-independent-extension-assessment.md) | Admit `omp_api` and per-row `api` in Rust before spawning a child | Partially implemented: extend | not measured. Plan: time admission to first error for an unsupported level, before and after. |
| [EXT-08](07-abi-independent-extension-assessment.md) | Non-Python provider code as an external process behind a configured... | Suitable as an external integration | not measured; extra local hop latency is a plan item for the transport deliverable. |
| [EXT-10](07-abi-independent-extension-assessment.md) | WebAssembly component extensions | Requires ADR reconsideration | Plan: build a throwaway host outside the OMP2 tree and record binary-size delta, cold and warm instantiation, and per-call overhead against a Python host, per the owner's section 6 list. Not run. |
| [EXT-11](07-abi-independent-extension-assessment.md) | Native executable extension class speaking the CONTROL protocol | Requires ADR reconsideration | Plan with EXT-10's harness. |
| [EXT-12](07-abi-independent-extension-assessment.md) | Alternative encodings and shared memory for extension traffic | Requires benchmarking | Use the serialization owner's harness; record `uptime`, runs, frame sizes; compare CONTROL JSON with protobuf on a dispatch round trip. |
| [PY-02](08-python-runtime-comparative-assessment.md) | Measure embedded child startup, resident memory and the binary size... | Requires benchmarking | On a quiet machine (record `uptime`): `just build-release` twice (with and without omp-py in the link, or `size` of the `omp` binary versus a build of the same revision where `crates/py` is stubbed); `stat` both. For startup: 30 runs of spawning the hidden... |
| [PY-03](08-python-runtime-comparative-assessment.md) | Measure PyO3 callback overhead on the _omp boundary | Requires benchmarking | Rust `criterion` benchmark for attach plus a no-op `_omp` function, and a Python timing loop for typed value constructors; 5 runs of 100000 calls each, report median and p95, record `uptime`. Pair with the 62 harness replaced by the real `external_runner.py... |
| [PY-04](08-python-runtime-comparative-assessment.md) | Standalone CPython subprocess for Eval (existing opt-in) | Already implemented: reuse | Run the same Eval contract tests (`crates/envd/src/eval/tests.rs`) against both interpreters and report pass counts; time cell round trips with 62 style harness through the real runner. |
| [PY-05](08-python-runtime-comparative-assessment.md) | Standalone CPython as the runtime for extension hosts and workers | Requires ADR reconsideration | Run PY-02 and PY-03; add a standalone host spike behind a flag in a scratch branch only after (1) to (5) exist. |
| [PY-06](08-python-runtime-comparative-assessment.md) | RustPython as an embedded alternative | Requires ADR reconsideration | Compatibility run of the repo's frozen modules and `crates/py/tests/*` equivalents; size and startup via PY-02. |
| [PY-08](08-python-runtime-comparative-assessment.md) | WebAssembly components as an extension runtime | Requires ADR reconsideration | Spike measuring wasmtime binary size delta and cold instantiate time of a hello component; only after evidence items 1 to 3. |
| [SWM-01](09-single-session-swarm-assessment.md) | Keep the logical child kernel as the default agent form | Already implemented: reuse | not measured; see section 8, B1. |
| [SWM-02](09-single-session-swarm-assessment.md) | No new orchestrator; extend ADW only where it cannot drive a phase... | Partially implemented: extend | not measured; see B3. |
| [SWM-06](09-single-session-swarm-assessment.md) | Resource-aware admission in the workpool | Requires benchmarking | plan B2 and B4 in section 8. not measured. |
| [SWM-07](09-single-session-swarm-assessment.md) | Headless OMP child process (same binary) behind a trigger list | Requires benchmarking | B5 in section 8. not measured. |
| [SWM-08](09-single-session-swarm-assessment.md) | Measure what a child duplicates (provider registry, credentials, me... | Requires benchmarking | B1 in section 8. not measured. |
| [SWM-10](09-single-session-swarm-assessment.md) | Git worktree as an isolation backend | Unnecessary or redundant | B6 in section 8, for the copy-fallback case only. not measured. |
| [INF-01](10-local-inference-assessment.md) | Wire or retire the memory embedding worker | Partially implemented: extend | B1 and B2 in section 8 (`not measured`). |
| [INF-02](10-local-inference-assessment.md) | Process-wide shared model residency and one memory budget | Missing and justified: propose | B2 in section 8 (`not measured`). |
| [INF-03](10-local-inference-assessment.md) | llama.cpp as an external local server | Suitable as an external integration | B3 in section 8 (`not measured`). |
| [INF-04](10-local-inference-assessment.md) | llama.cpp linked in process | Requires ADR reconsideration | `not applicable` until the owner reopens the rule. |
| [INF-05](10-local-inference-assessment.md) | MLX and mlx-lm as an optional managed server | Suitable as an optional extension | B3 with MLX added (`not measured`). |
| [INF-06](10-local-inference-assessment.md) | Core ML and the Neural Engine for embeddings and classifiers | Requires benchmarking | B5 in section 8 (`not measured`). |
| [INF-07](10-local-inference-assessment.md) | Confine ONNX Runtime to the out-of-process worker | Requires ADR reconsideration | `not applicable` |
| [INF-08](10-local-inference-assessment.md) | Candle encoders for embeddings and rerank | Requires benchmarking | B1 (`not measured`). |
| [INF-09](10-local-inference-assessment.md) | In-process tiny generator for title, memory and classifier chores | Requires benchmarking | B3 and B4 (`not measured`). |
| [INF-10](10-local-inference-assessment.md) | Typed hardware placement policy replacing the dead TinyDevice | Missing and justified: propose | Reuse B1 to B5 results (`not measured`). |
| [INF-11](10-local-inference-assessment.md) | Local reranker lane | Partially implemented: extend | B4 (`not measured`). |
| [INF-12](10-local-inference-assessment.md) | Linux without Docker or Podman | Partially implemented: extend | `not measured`; run B1 on a Linux host. |
| [CIX-02](11-code-intelligence-and-scip-assessment.md) | Give agents a read-only query surface over the shared index | Partially implemented: extend | B2 and B3 in section 8. |
| [CIX-03](11-code-intelligence-and-scip-assessment.md) | Share one index across swarm children by workspace identity and rev... | Missing and justified: propose | B4 in section 8. |
| [CIX-06](11-code-intelligence-and-scip-assessment.md) | Document-level incremental invalidation instead of a full rebuild | Requires benchmarking | B1 and B5 in section 8. |
| [CIX-09](11-code-intelligence-and-scip-assessment.md) | Do not add Tantivy now; decide with a measured grep baseline | Unnecessary or redundant | B3 in section 8 compares grep, the trigram extension, FTS5 and, only if grep fails the budget, Tantivy. |
| [CIX-10](11-code-intelligence-and-scip-assessment.md) | Keep semantic and associative results advisory and separate from au... | Requires benchmarking | B6 in section 8, following ADR 0060 D1's spike. |

## 5. Area benchmark tables

The detailed case tables live with the area they belong to: [6b section 6](06b-ipc-and-serialization.md) (E1 to E5, T1, L1 to L4, C1), [8 section 4](08-python-runtime-comparative-assessment.md), [9 section 8](09-single-session-swarm-assessment.md) (B1 to B6), [10 section 8](10-local-inference-assessment.md) (B1 to B5). Deliverables 5, 7 and 11 carry theirs in their cards.

## 6. Order of work

1. One idle-machine baseline of the existing build (binary size, cold and warm `omp` start, RSS at idle), because every later comparison needs it and none exists.
2. The measurements that decide an owner question: per-child duplication (SWM-08) and embedding-worker duplication (INF B2), since they decide whether any shared-infrastructure work is justified.
3. Serialization cases only after (1), and only for payload classes that exist today (embedding batches, token events). The owner's IR payloads (`CodeIR`, `ChangeIR`, `VerificationIR`) do not exist as types, so there is nothing to benchmark for them yet.
4. Provider-path measurements (TTFT overhead of the observer, cache-affinity effect) use the cassette transport and fake providers (`crates/ai/src/transport/cassette.rs`, `crates/ai/src/provider/fake.rs`) so that no provider quota is consumed.

Back to the [index](README.md).
