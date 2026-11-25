# Executed baseline, allocation probes and benchmark specification

Pin: `fda1315052b8b4074f9ea5acc842b2cc0b08b2b2`. **RUN** means executed here;
all other workload procedures below are proposed, not results.

## Reproduce and inspect raw evidence

From the pinned repository with its compatible toolchain and dependency cache:

```sh
source /workspace/omp-toolchain/environment.sh
python3 docs/research/native-resource-architecture/experiments/collect_baseline.py
```

The first line is this environment's task-local toolchain setup, not a new OMP2
requirement. Elsewhere use the repo-pinned Rust toolchain and existing `just`
recipes. The collector invokes offline locked Cargo metadata/tree, focused
nextest-plus-doctest recipes, and bounded research-only Rust probes. It creates
temporary binaries/build output outside production source and writes raw JSON
only under this research directory. It does not purge OS caches or call providers.

- [Final successful collection](experiments/baseline-results-final.json): commands,
  status, environment, original sample rows and summaries.
- [Initial collection](experiments/baseline-results.json): preserves two failed
  standalone native-probe links; core/journal tests and synthetic graph ran.
- [Intermediate attempt](experiments/baseline-results-complete.json): preserves
  the failure when only `.rlib` was supplied. The workspace uses split metadata;
  explicitly supplying both matching `.rlib` and `.rmeta` repaired the links.
- [Collector](experiments/collect_baseline.py),
  [allocation probe](experiments/ownership_probe.rs),
  [actual Str probe](experiments/core_str_probe.rs),
  [actual blob API probe](experiments/blob_range_probe.rs).

Raw stderr is limited to its last 8 KiB by the collector. Dependency-tree stdout
is an explicitly labeled excerpt with full-output SHA256 and line count; lock
duplicate groups and workspace inventory are complete. No omitted text is
represented as independently checked. The final collection has no failed commands.

## Actual environment and executed checks

| Property | Recorded value/scope |
| --- | --- |
| OS/architecture | Linux 6.18.57, x86_64, glibc 2.41 |
| CPU | 3 visible CPUs; cgroup quota `200000 100000` (2 CPU-equivalents) |
| Memory | cgroup ceiling 8,589,934,592 bytes; host `/proc/meminfo` is not the cgroup budget |
| Swap | `/proc/meminfo` reported zero swap in this environment |
| Rust | 1.99.0-nightly, commit `1a98b1e135b254f209c67d447b6d8bcd56a859e0`, LLVM 23.1.0 |
| Cargo | 1.99.0-nightly `c79e8f894`, locked/offline |
| Workspace | 62 members; 1,397 lockfile package records; 155 duplicate-name/version groups |
| Available | Cargo/rustc, just, nextest; existing repository tests/tracing |
| Not found by collector | perf, heaptrack, Valgrind, cargo-bloat, cargo-llvm-lines, macOS vmmap/footprint/sample |

`just test-pkg omp-core` executed **655 native tests, all passed**, plus doctest
groups of 145 and 2, all passed. `just test-pkg omp-journal` executed **74 passing
tests and 5 skipped**, and selected zero doctests. The skipped tests are not PASS;
the zero-doctest phase establishes no additional coverage. This is not a full
workspace, provider, process-sandbox or macOS verification claim. The compatible
toolchain emitted existing deprecation warnings. Raw commands and summaries are
preserved rather than describing compilation alone as verification.

Elapsed command times are observations from this warmed environment, not clean
build benchmarks. Earlier agent-observed cold compilation was not captured as a
controlled paired build experiment and is not used to claim a build speedup.

## Actual native blob API experiment

`blob_range_probe.rs` links the real pinned `omp-journal::blob::BlobStore`, writes
one immutable blob through `put_reader`, then reads a 4,096-byte middle range.
It compares `get(...).slice(...)` with `open_range` and a bounded read, asserting
equal bytes and source/offset/range identity. Each cell has **30 samples**.

| Blob size | Pattern | Allocation calls/sample | Requested bytes/sample | Requested live bytes while selected view is held |
| ---: | --- | ---: | ---: | ---: |
| 1 MiB | get then slice | 11 | 1,049,177 | 1,048,600 |
| 1 MiB | bounded range | 10 | 4,673 | 4,096 |
| 10 MiB | get then slice | 11 | 10,486,361 | 10,485,784 |
| 10 MiB | bounded range | 10 | 4,673 | 4,096 |
| 50 MiB | get then slice | 11 | 52,429,401 | 52,428,824 |
| 50 MiB | bounded range | 10 | 4,673 | 4,096 |

This measures the native storage APIs and reproduces the memory mechanism found
in `BlobStoreAuthority::read_range`; it **does not execute an optimized envd
adapter**, IPC, authorization, artifact lookup or the end-to-end tool request.
The allocator counter measures requested bytes/live requests, not physical RSS,
actual allocator overhead or retained file-cache pages. The range branch uses a
mutable Vec as its result buffer; the full branch uses Bytes; correctness here is
equal selected content, not every adapter ownership/error behavior.

The final raw file contains instrumented medians (roughly 0.046/0.422/21.764 ms
for the three full reads and 0.008/0.005/0.008 ms for their bounded reads). Do not
turn those ordered, warm-filesystem microbenchmarks into an OMP2 latency forecast.
All full-read samples precede all bounded samples; OS cache order is a confounder.
The allocation scaling is the primary evidence. A candidate adapter must preserve
strict out-of-range rejection and empty ranges: `open_range` clamps a requested
length and interprets zero as remainder, unlike a strict empty slice.

## Synthetic graph and actual Str ownership experiments

Fixtures have symbol IDs, names, paths, documentation and caller vectors; they
are **not an existing CodeIR or semantic index**. The collector caps nodes at
100,000. Thirty instrumented samples were executed per fixture/method:

| Nodes | Representation/operation | Allocation calls/sample | Requested bytes/sample |
| ---: | --- | ---: | ---: |
| 10,000 | deep-clone owned String/Vec graph | 40,001 | 3,020,000 |
| 100,000 | deep-clone owned String/Vec graph | 400,001 | 30,200,000 |
| 10,000 | clone Arc alias to existing immutable graph | 0 | 0 |
| 100,000 | clone Arc alias to existing immutable graph | 0 | 0 |
| 10,000 | clone graph using actual `omp_core::Str` | 10,001 | 1,160,000 |
| 100,000 | clone graph using actual `omp_core::Str` | 100,001 | 11,600,000 |

Arc aliasing does not provide an independently mutable graph. The `Str` graph
still clones owned caller vectors and the containing vector; immutable string
sharing does not make the entire graph zero-allocation. The outer Rust probe is
optimized with `rustc -O`; linked native crates use the repository's test-build
artifacts. Do not call this an equivalent production release-profile comparison.
Atomic reference-counting and allocator instrumentation have costs; sub-microsecond
Arc timings approach timer resolution and do not support a meaningful speedup ratio.

Existing `Str` tests/source and existing streaming allocation tests are therefore
better evidence than counting `.clone()` text. `crates/core/src/str.rs:1297–1304`
shares the heap-backed representation. Existing warm streaming allocation
regression machinery is in `crates/agent/src/directors/stream_rules.rs:1529–1598`; extend it
where applicable instead of selecting a production global allocator for research.

## Corpus and platform matrix still required

| Workload family | Executed here | Required future coverage |
| --- | --- | --- |
| Structured data sizes | Native blob 1/10/50 MiB | 100/200/500 MiB and 1 GiB only on provisioned memory/disk budgets |
| Symbol graphs | Synthetic 10k/100k | Representative real SCIP/query graph 500k/1m/5m, measuring actual strings/edges/indexes |
| Platforms | Linux AMD64 | Linux ARM64 and macOS ARM64 first-class repeats |
| Startup | No complete harness run | Idle, first usable CLI/TUI/ACP interaction, first inference request, eager capability census |
| Remote providers | No paid calls | Deterministic recorded/mock streams first; explicitly authorized Bedrock Claude canary and OpenAI-compatible routes |
| Extension lifecycle | Source audit only | Empty config, first extension/Eval, idle/warm reuse, crash/reload/cancellation and active lease protection |
| Inference models | None loaded | Small embedding/reranking/classification candidates only after residual 16 GiB budget is known |

For each size, include zero/small/full/selective queries, malformed/truncated
inputs, valid empty outputs, repeated updates and concurrent readers. Synthetic
bytes alone are insufficient for indexing/IR format selection: preserve symbol
relationships, path/string distributions, repeated metadata and lookup skew.
If one of the requested named IRs remains absent, use actual graph/query/
verification types or label the fixture hypothetical; do not invent an existing IR.

Never allocate a 1 GiB fixture just because it is listed. Preflight cgroup/host
budget, concurrent tools, free disk and pressure; set per-process limits where
supported and stop before the environment becomes unstable. Record the blocker
and leave that scale unexecuted rather than extrapolating results.

## End-to-end benchmark recipe

1. **Freeze baseline:** exact source/toolchain/lockfile, workload seeds, recorded
   provider streams, source snapshots, policies and evaluator version. Preserve
   source truth, journal history and mandatory gates unchanged.
2. **Inventory activity:** process tree, open resources/threads, queue depths,
   lazy resource activations, socket/database/model counts. Attribute constructor
   stages through existing tracing and P8 environment workload/census seams.
3. **Separate stages:** context projection, codec serialization, body signing,
   send, first headers/event/token, stream decode, tool dispatch, persistence,
   terminal draw and final task correctness. A provider TTFT is not harness time.
4. **Record memory:** parent/children RSS/PSS/private/shared/mappings/faults,
   live/peak requested heap and allocation rate; cgroup totals/limits/pressure on
   Linux; physical footprint/compression/swap/Metal contention on macOS. Avoid
   double-counting shared pages or treating RSS as physical footprint.
5. **Run paired candidates:** randomized/interleaved baseline/candidate ordering,
   declared warmups, same snapshot and workload, multiple fresh processes. Use
   preserved independent held-out repositories/prompts, not training fixtures.
6. **Report uncertainty:** sample count, median, range and bootstrap confidence
   intervals where justified. Use P95/P99 only with an adequate tail population
   (hundreds/thousands of independent observations for tail decisions, with raw
   data and autocorrelation checks), not thirty ordered microbench samples.
7. **Promote only legally:** identical required outputs/invariants, no security/
   cancellation/replay regression, improvement beyond repeat noise on held-out
   cases, acceptable cold/warm trade-off and bounded memory. Revert otherwise.

Serialization candidates must include construction, encoding, validation,
framing, transport, page faults, materialization and cleanup. Compare current
Prost/selective CAS first. A FlatBuffers/rkyv field-access timing without the
construction/validation pipeline is not an adoption result. Arrow requires a
genuinely columnar workload. Compare fixed schema/source versions and corruption
handling, not only encoded length.

## Concrete future commands and profiling boundaries

Commands below are procedures, not claimed executions in this report:

```sh
cargo tree --locked -e features -p omp-app
cargo tree --locked -d
just test-pkg omp-ai
just test-pkg omp-agent
just test-pkg omp-envd
just test-pkg omp-tui
```

Use existing P8 scripts and E2E contracts identified in chapter 01; do not invent
a second benchmark service. For an already running Linux PID, read
`/proc/<pid>/smaps_rollup`, `status`, `stat` and cgroup-v2 `memory.current`,
`memory.peak` where supported and `memory.pressure`; retain permission/kernel
failures. On macOS capture `vmmap -summary <pid>`, `footprint <pid>` and Instruments
Allocations/VM Tracker/Time Profiler under actual concurrent development load.
Do not run privileged cache purges on a shared development machine and call them
ordinary cold startup. Distinguish a fresh process from a cold filesystem cache.

Optional allocation instrumentation can be feature/test-binary scoped, use the
existing counters or a source-reviewed profiler, and remain off by default.
Record its overhead and compare uninstrumented timings separately. Instrumented
allocators in the research probes forward to System and use unsafe only to
implement that allocator instrumentation contract; production allocator code is
unchanged. Tool availability/licensing/limitations are in chapter 05.

## Regression acceptance and blockers

Gate semantics are OMP2's ADRs/contracts, golden traces, deterministic replay,
property/model/fault tests and source-revision evidence—not Pi output parity.
Normalize only explicit nondeterministic IDs/timestamps; do not erase divergent
tool order, permissions, source identities, outcomes or usage accounting.

Use isolated test jobs for periodic allocation/long-session regression checks,
keeping normal startup uninstrumented. Extend existing CI path selection so
documentation-only changes do not require a performance run. Do not fail PRs on
single noisy wall-clock samples; establish platform-specific control distributions,
review thresholds and multi-run confirmation. Mandatory correctness gates remain
independent of optimization scores and memory budgets.

Still unmeasured: full OMP2 active/idle physical footprint, actual HTTP-capture
growth, prompt/signing end-to-end allocation impact, all non-AMD64 platforms,
Bedrock account/region/model availability, meaningful local-model quality and
16 GiB coexistence. Saetta's attempted native test was blocked by missing clang
in this environment (chapter 02). No claim of a full end-to-end benchmark or
Saetta-vs-OMP2 runtime win follows from the executed probes.
