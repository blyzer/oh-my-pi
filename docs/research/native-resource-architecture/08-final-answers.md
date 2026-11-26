# Explicit answers to the fifty research questions

Pin: `fda1315052b8b4074f9ea5acc842b2cc0b08b2b2`. Source facts and open
measurements are distinguished below. Detailed source ranges and external pins
are in chapters [01](01-native-baseline-and-lifecycle.md),
[02](02-saetta-and-native-inference.md),
[03](03-abi-python-and-data-boundaries.md),
[04](04-providers-context-and-allocation.md),
[05](05-technology-and-meta-harness.md) and
[06](06-benchmarks-and-memory.md). Decisions R01–R14 are expanded in the
[roadmap](07-roadmap-and-decisions.md) and [records](adoption-records.json).

## Native architecture and ABI

**1. Does OMP2 actually need a stable native ABI?**
No demonstrated requirement was found for a general independently compiled Rust
plugin ABI. Native workspace crates are co-built and exchange typed Rust values.
Existing trusted system/engine C-ABI boundaries have concrete uses; they do not
justify adding another plugin architecture (chapter 03, R13).

**2. How does it currently address Rust's unstable ABI?**
By using one compatible toolchain/build for native crates, ordinary calls,
generics/traits and owned/shared Rust types; independent processes use explicit
protocols. This is an architectural choice, not emulation or a missing ability
to generate native machine code. FFI is used where an actual foreign contract exists.

**3. Which components genuinely benefit from independently loaded libraries?**
Existing Linux audio/system C libraries are a source-proven example. A supported
optional inference C library may be appropriate behind its existing native owner.
No further trusted Rust DLL extraction has a measured benefit in this audit.
Loading/unloading alone does not establish reclamation or safety.

**4. Which can remain statically linked and defer initialization?**
LSP/backend/model/embedding and selected memory/index/client resources can keep
their metadata/code linked while their owners delay acquisition. Some already
do so. Driver/envd constructor profiling is needed before changing currently eager
memory banks/cache/scheduler assembly (chapter 01's lifecycle table, R07).

**5. Which modularity mechanisms balance performance and maintainability?**
Existing modules/crates, typed ownership, current traits, configuration, lazy
non-evictable singletons and owner-scoped single-flight/leases. Add a crate or
trait object only for an actual ownership/API boundary, not visual modularity.

**6. Which proposals comply with ADR 0036?**
Internal range/capture/prompt/body optimizations, existing owner lifecycle work,
typed control protocols and optional trusted engines can preserve it. Python
extensions/Eval remain embedded and killable, controlled registration remains
authoritative. R08 identifies an existing external-interpreter path in tension
with the ADR; research does not amend either silently.

**7. Which mechanisms should be rejected?**
Arbitrary third-party Rust symbol loading, stable-ABI migration without a need,
replacing Python extensions with another language/runtime, new duplicate managers/
registries/stores, and claims that ABI compatibility provides isolation. Also
reject disabling safety/observability merely to shrink the executable (R13).

## CPython and extensions

**8. Is the current embedding appropriate?**
Yes as an architectural fit: static free-threaded CPython and frozen modules in
supervised children satisfy dependable Eval and termination goals. This does not
certify every lifecycle/configuration path; configured external Python and
initialization-failure behavior need explicit conformance proofs (chapter 03).

**9. Would adopting PyO3 improve the current integration?**
PyO3 is already used, together with `pyo3::ffi` for low-level isolated PyConfig/
frozen-module initialization. The useful question is conversion/batching/lifetime
cost at specific callers, not replacing “C API” with a package already present.
OMP's 3.14 free-threaded full embedding is not interchangeable with generic abi3;
upstream 3.15 abi3t documentation does not change this pinned build.

**10. Are interpreters/processes initialized unnecessarily?**
Not established globally. Some native environment constructors are eager, while
Eval/local-model/embedding paths have demand-driven owners. Empty-extension,
first-use and configured-extension process censuses must establish actual child
creation. Carrying CPython in the binary does not prove interpreter initialization.

**11. Can idle extension resources be released?**
Reconstructable resources can be released through current supervisors after
active calls/callbacks/leases settle. A retained Eval namespace or external-effect
obligation cannot simply be evicted and replayed: its state may not be safely
reconstructable from the journal. Test explicit disposable/reset semantics first.

**12. Could another execution mechanism help a specific workload?**
Possibly existing native workers/trusted engine FFI or a separately justified
WASM workload. None is justified as a wholesale replacement for Python extensions.
Subinterpreters do not supply the crash/termination/security boundary of independent
processes; child processes alone are not a sandbox (chapters 03/05, R10/R13).

## Serialization and actual IRs

**13. Where is avoidable serialization/data movement?**
Concrete candidates include request-body rematerialization before SigV4, repeated
prompt projection, frame-vector allocation and diagnostic JSON/String work.
Not all are redundant *serialization*: some are copies or required external
encoding. No evidence supports deleting provider JSON or necessary process/
persistence framing wholesale (chapter 04, R04/R05/R14).

**14. Where is Prost appropriate already?**
The established environment/protocol and generated typed control contracts.
`omp-proto` generation already makes deliberate Bytes/map choices. Extension-host
control also uses framed JSON and Eval NDJSON; optimizing them must preserve their
real versioned contracts rather than inventing a universal-Protobuf baseline.

**15. Could rkyv improve native caches/snapshots?**
Potentially for large immutable, rebuildable, Rust-native derived snapshots after
actual graph/selectivity measurements. No adoption is justified yet. Include
validation, format version/alignment/endian settings, corruption/rebuild behavior,
encoding and current patched-version/advisory review—not unchecked archived access
or an authoritative journal migration (chapters 03/05, R09).

**16. Could FlatBuffers improve cross-language large data?**
Possibly where consumers genuinely need selective access to a large immutable
dataset. Existing CAS references/ranges/streaming are simpler first candidates.
Schema validation and materialization costs still apply; the native blob result
does not imply a FlatBuffers migration is needed.

**17. Does Cap'n Proto justify changing communication?**
Not on current evidence. Direct access and capability RPC are distinct benefits;
neither warrants replacing working typed control protocols or introducing a
second capability/authorization system. A future unmet workload needs an
end-to-end comparison and explicit boundary decision (R13).

**18. Would shared memory materially improve performance safely?**
Unproven. First remove unnecessary full reads and repeated embedding of large
artifacts using existing ranges/CAS/streaming. Read-only mappings still require
integrity, leases, crash recovery and explicit revocation/retention limits; bytes
already mapped cannot be assumed revocable (chapter 05, R02/R09).

**19. Which indexes/IRs should be memory-mapped?**
Read-mostly model weights are a natural existing backend use. A validated derived
symbol/index snapshot is conditional on profiling; a Rust heap graph cannot be
made portable merely by mapping its raw struct bytes. No blanket mmap conversion
of authoritative state or all indexes is recommended.

**20. Which benefit from partitioning/sharing/selective materialization?**
Actual KB graph/query structures and large source/artifact selections are the
strongest candidates, with source/version freshness intact. Existing Str/Arc/Bytes
already share immutable data. Named CodeIR/CodeQueryIR/ChangeIR/VerificationIR
were not established production types in the scoped audit; use actual types,
including verification snapshots/policies/evidence, rather than inventing IRs.

**21. Which allocations/clones/conversions can be eliminated?**
The full backing blob for small ranges; duplicate band rendering; eligible
contiguous-body collection; and measured secondary projection/frame/debug copies.
The actual Str probe confirms that collection clones may allocate while their
strings share. Arc aliasing cannot replace an independently mutable copy without
changing semantics (chapter 06, R02–R05/R14).

## Resource lifecycle

**22. Which subsystems should be demand-driven?**
Already optional/lazy LSP, embeddings and local inference should remain so.
Selected memory-bank/index/MCP/environment resource acquisition should be measured
for further deferral inside existing owners. Do not delay mandatory policy,
source-validation or session-authority initialization to hide its cost (R07).

**23. Which resources can be evicted?**
Rebuildable cache entries, idle optional models under their leases, and supervised
children whose live request/subscription/document/namespace obligations have ended.
An existing reaper method is not proof production wiring or physical release;
validate both before enabling a policy (chapter 01).

**24. Which should remain warm?**
Cheap registry/configuration state, current authoritative session state, active
resource dependencies and reusable HTTP pools. Expensive resources may stay warm
within measured budgets if reload cost and thrashing outweigh savings. No
universal “evict everything idle” rule is justified.

**25. Which should be mapped?**
Large immutable resources with meaningful selective access, validated layouts and
appropriate platform behavior. Measure page faults/private dirty/shared clean
pages and full-pipeline costs. A configured virtual mapping ceiling—such as the
vectX example—is not equivalent to physical memory consumption.

**26. Which should be session-only?**
Request projection/scratch, session-owned transient diagnostics, live callbacks/
tool ownership and disposable execution state belong to their existing request/
session owners. Durable journals, evidence and memory banks outlive reconstructable
resources; their lifetime cannot be shortened as a cache optimization.

**27. Which existing lifecycle mechanisms should be reused?**
LspPool initialization/Arc ownership, supervisor generations/termination, EnvServer
idle accounting, memory Off mode/embedding supervisor, LocalRuntime admission/
MemoryPool/RuntimeLease/unload, cancellation trees and existing cache limits.
Use OnceLock/LazyLock for retained resources, not as a substitute for eviction.

## Performance and hardware

**28. What offers the greatest benefit on the M1 Pro 16 GiB?**
The globally greatest benefit is not measured on that machine. The strongest
executed narrow result is eliminating full-blob retention for small reads; bounded
diagnostic retention and optional resource activation are high-value investigations.
Keep heavy generation remote and do not load Saetta's advertised large packs into
the residual memory left by IDE/browser/compiler/OMPv1 workloads (chapter 09).

**29. Which changes also benefit Linux AMD64/ARM64?**
Bounded reads, request-scoped computation, fewer body copies, bounded retention
and owner-local lifecycle policies are portable concepts. Native tests must still
run on both architectures. Metal kernels, macOS physical-footprint tools and
Seatbelt are not portable equivalents of Linux mechanisms.

**30. What reduces active memory without removing functionality?**
Retaining only selected artifact bytes, retiring settled nonauthoritative capture
ownership, deferring unused optional acquisition, and safely releasing rebuildable
resources. Capability metadata/code stays available. Required checks cannot be
silently skipped to meet a budget (R02/R03/R07).

**31. What improves startup?**
Potentially owner-scoped deferral of costly bank/index/child acquisition, after
constructor attribution and first-use tests. Existing shared lazy clients and
optional runtimes should be retained. A smaller executable or more crates alone
does not establish faster startup; full startup was not measured here.

**32. What improves long-session stability?**
Bounded diagnostic retention, explicit queue/cumulative-buffer policy, correct
child/lease cleanup and cache accounting/idle hysteresis. Validate memory plateaus
across failures, rewinds, subagents and slow consumers. No quantified production
leak or long-session fix was executed by this research (R03/R06/R07/R14).

**33. How can CI detect allocation/memory regressions?**
Extend existing P8/tracing/allocation tests with deterministic workload boundaries,
recorded streams, process census and versioned raw metrics. Run profiling only in
dedicated modes; derive platform/noise thresholds from repeated baselines, preserve
correctness gates, and confirm noisy wall-clock regressions before blocking
promotion. No always-on production global allocator change is warranted (chapter 06).

## Saetta and inference

**34. What can OMP2 learn without losing capabilities?**
Stable prompt construction, explicit buffer/ownership accounting, measured
conversation reuse, demand-paged model resources, fit guards, deterministic traces
and differential testing. Adopt methods behind OMP2 owners/contracts, not Saetta's
feature set or compatibility target (chapter 02).

**35. Which equivalents already exist?**
OMP2 has native typed dispatch, lazy/pool mechanisms, stable prompt/codec policies,
streaming, differential retained TUI rendering, deterministic tests/replay, local
runtime admission/leases and journals/CAS. Source details and limitations appear
in chapter 02's equivalence matrix; a matching concept is not proof equal cost.

**36. What is adaptable without Pi compatibility?**
Behavioral fixtures, golden traces, fragmentation/fault tests, numerical tests for
an optional backend and independent baseline/candidate comparisons. The reference
must be OMP2 ADRs/contracts, source truth and prior stable OMP2 behavior—not Pi
output parity or normalizing meaningful differences away.

**37. What helps Claude on Bedrock?**
Less projection/allocation, stable cacheable system/tool prefixes, exact SigV4 body
handling, existing connection reuse, efficient EventStream/SSE processing on their
appropriate routes, backpressure, cancellation and reported cache usage. Model/
region/interface availability must be checked for the actual account and model;
no live AWS entitlement test was performed (chapter 04).

**38. What requires local inference?**
Weight mapping, quantized tensor kernels, direct KV allocation/reuse/eviction,
speculative decoding implementation, Metal execution and backend LoRA loading.
These cannot be imposed on remote APIs that do not expose those internals.

**39. Is optional native inference valuable on the target?**
Conditional, most plausibly for specialized small embedding/reranking/classification
workloads. Require held-out quality and combined CPU/GPU/physical-footprint tests
under real concurrent development load. Local execution is not inherently cheaper
or more resource-efficient than an approved remote provider (R10).

**40. How should Saetta be treated?**
Primarily as a pinned architectural/testing reference. Its current macOS/Metal/
model-specific implementation and large-pack safeguards do not establish a
supported stable library ABI or a practical default backend for this 16 GiB target.
Small licensed code reuse/backend experiments require a concrete independent need.

## IR-driven optimization

**41. Can existing IRs express the proposed optimizations?**
Actual typed ADW workflows, verification specifications/policies, convars and
backend configuration express some execution choices. They do not magically
express new ownership primitives or all proposed lifecycle policies. Keep factual
graphs/evidence immutable and use actual type names (chapters 01/05, R12).

**42. Which need no native Rust change?**
Experiments over already supported lazy/Off/backend/context/retrieval/concurrency
settings and permitted prompt templates can start in the fast loop. Every knob
must be verified as implemented and legal; a proposed resource-budget field is
not an existing knob. Do not expose mandatory security/gate bypass as optimization.

**43. Which need native primitives?**
The bounded artifact adapter, settled-capture ownership, one-pass prompt projection,
eligible body ownership transfer, or a missing proven lifecycle/buffer primitive
need small changes at their existing Rust owners. Use them before inventing a new
optimizer IR or stable-ABI infrastructure (R02–R07/R14).

**44. Can journal/verification support Meta-Harness?**
Yes as reuse candidates: immutable source snapshots, current required-check policy,
CAS evidence and ADW phases are concrete. They do not constitute an already
implemented self-optimizing harness, nor an independent proof that arbitrary
candidate code is safe. Keep candidate execution in authorized isolation (chapter 05).

**45. How are transformations validated deterministically?**
Allowlist the actual policy/plan schema; validate types, budgets, capabilities,
ownership and preserved analyses/invariants; compare deterministic replay and
contract outcomes against immutable baselines. Reject changes to factual code,
journal history, source identity, evidence or required policies. LLVM/MLIR are
methodological references, not mandatory dependencies.

**46. How are candidates promoted safely?**
Version candidate/config/evaluator/source; separate train and held-out tasks;
enforce hard ADR/security/correctness constraints before ranking feasible Pareto
candidates; preserve raw evidence; require the existing review/promotion workflow
and rollback. A better scalar reward cannot override a failed mandatory check.

**47. Which changes avoid unnecessary complexity?**
R02–R05's owner-local data/work reductions, existing configuration first, and
measured owner-local lifecycle adjustments. R09/R10/R12 remain conditional because
their potential benefits carry more schema/resource/evaluation complexity.
Reject another global manager, provider stack, scheduler, store or capability registry.

## Final architectural decision

**48. What is the smallest high-value set?**
Preserve R01's baseline and strict tests, implement R02 behind the current artifact
authority, then independently evaluate R03 and R04. R05 follows exact-body/evidence
tests. This order has a concrete measured native allocation mechanism and avoids
requiring ABI/protocol/runtime replacement. Whole-system benefits remain to be measured.

**49. Which accepted ADRs remain unchanged?**
All of them. Specifically retain native engine/allocation principles (0035),
embedded killable Python (0036), host-owned policy (0006), termination (0011),
session authority/lifecycle, stable roster (0024), bounded output, vendor-neutral
integration (0049) and controlled schemes (0050). Upstream-derived rationale is
preserved; the external-Python tension is documented, not silently legalized.

**50. What should be first, deferred, retained or rejected?**
First: measured bounded reads, diagnostic retention investigation and request-local
work elimination. Defer: eviction policy, alternative snapshots/shared memory,
specialized local models and adaptive optimization until their experiments pass.
Retain: current owners, native interfaces, real protocols, supervisors, providers,
registries, journals and verification authority. Reject speculative Rust plugins,
blanket format/allocator replacement, duplicated infrastructure, mandatory local
inference/containers and optimization that weakens correctness or isolation.
